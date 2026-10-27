import {
  EnvironmentAuthorizationError,
  type EnvironmentId,
  type RemotePairingCodePayload,
  WS_METHODS,
} from "@coddy/contracts";
import { classifyPairingEndpoint } from "@coddy/shared/advertisedEndpoint";
import {
  PairingCodeParseError,
  PairingCodeUnsupportedVersionError,
  parsePairingCode,
} from "@coddy/shared/pairingCode";
import * as Cause from "effect/Cause";
import * as Duration from "effect/Duration";
import * as Effect from "effect/Effect";
import * as Option from "effect/Option";
import * as Result from "effect/Result";
import * as Schema from "effect/Schema";
import * as Stream from "effect/Stream";
import * as SubscriptionRef from "effect/SubscriptionRef";
import { RpcClientError } from "effect/rpc";

import { e2eeSocketUrl } from "../authorization/remote.ts";
import { fetchRemoteEnvironmentDescriptor } from "../environment/descriptor.ts";
import { deriveWsBaseUrl, normalizeHttpBaseUrl } from "../environment/endpoint.ts";
import * as Persistence from "../platform/persistence.ts";
import * as RpcSession from "../rpc/session.ts";
import {
  BearerConnectionCredential,
  BearerConnectionProfile,
  BearerConnectionRegistration,
} from "./catalog.ts";
import { computeCompatVerdict } from "./compat.ts";
import { mapRemoteEnvironmentError } from "./errors.ts";
import {
  BearerConnectionTarget,
  type ConnectionAttemptError,
  type PreparedConnection,
} from "./model.ts";
import * as EnvironmentRegistry from "./registry.ts";
import { remoteEnvironmentId } from "./remoteIdentity.ts";
import { storageIdentityTargetKey } from "./storageIdentity.ts";

export type PairingAddFailureReason =
  | "unreachable"
  | "host-identity-mismatch"
  | "pairing-rejected"
  | "incompatible"
  | "duplicate-storage-identity"
  | "local-persistence-failed";

export class PairingAddError extends Schema.TaggedError<PairingAddError>()("PairingAddError", {
  reason: Schema.Literals([
    "unreachable",
    "host-identity-mismatch",
    "pairing-rejected",
    "incompatible",
    "duplicate-storage-identity",
    "local-persistence-failed",
  ]),
  detail: Schema.String,
}) {
  override get message(): string {
    return this.detail;
  }
}

export class PairingLoopbackAcknowledgementRequiredError extends Schema.TaggedError<PairingLoopbackAcknowledgementRequiredError>()(
  "PairingLoopbackAcknowledgementRequiredError",
  { endpoint: Schema.String },
) {
  override get message(): string {
    return "This pairing code points at the host itself. Confirm you reach it through a tunnel (e.g. SSH port forwarding), then try again.";
  }
}

export interface VerifyPairingCodeInput {
  readonly code: string;
  readonly label?: string;
  readonly allowLoopbackTunnel?: boolean;
}

const POST_BOOTSTRAP_IDENTITY_MISMATCH_DETAIL =
  "The server behind this endpoint does not match the pairing code.";
const postBootstrapPersistenceError = (target: string): PairingAddError =>
  new PairingAddError({
    reason: "local-persistence-failed",
    detail: `The device credential was created, but ${target} could not be saved locally.`,
  });
const isPairingCodeParseError = Schema.is(PairingCodeParseError);
const isPairingCodeUnsupportedVersionError = Schema.is(PairingCodeUnsupportedVersionError);
const isPairingAddError = Schema.is(PairingAddError);
const isRpcClientError = Schema.is(RpcClientError.RpcClientError);
const isEnvironmentAuthorizationError = Schema.is(EnvironmentAuthorizationError);
const LEGACY_CONFIRMATION_UNSUPPORTED_DEFECT = `Unknown request tag: ${WS_METHODS.authConfirmPairing}`;

type PairingConfirmationFailureDisposition = "rollback" | "verify-authority";

const classifyPairingConfirmationFailure = (
  cause: Cause.Cause<unknown>,
  pairingConfirmationRequired: boolean,
  loopbackEndpoint: boolean,
): PairingConfirmationFailureDisposition => {
  if (cause.reasons.length !== 1) return "verify-authority";
  const reason = cause.reasons[0]!;
  if (Cause.isInterruptReason(reason)) return "verify-authority";
  if (Cause.isFailReason(reason)) {
    // On-host grants are already active and carry no pending-confirmation
    // capability. A scope denial does not invalidate that delivered credential;
    // verify it through the supervisor without broadening server permissions.
    if (
      loopbackEndpoint &&
      !pairingConfirmationRequired &&
      isEnvironmentAuthorizationError(reason.error) &&
      reason.error.requiredScope === "access:write"
    ) {
      return "verify-authority";
    }
    return isRpcClientError(reason.error) ? "verify-authority" : "rollback";
  }
  if (Cause.isDieReason(reason) && reason.defect === LEGACY_CONFIRMATION_UNSUPPORTED_DEFECT) {
    return pairingConfirmationRequired ? "rollback" : "verify-authority";
  }
  return "verify-authority";
};

const pairingConfirmationFailure = (cause: Cause.Cause<unknown>): PairingAddError => {
  const failure = Cause.squash(cause);
  return new PairingAddError({
    reason: "local-persistence-failed",
    detail: `The local connection was saved, but the server could not confirm pairing: ${failure instanceof Error ? failure.message : String(failure)}`,
  });
};

const PAIRING_BEARER_PROOF_TIMEOUT_MS = 30_000;

type PairingBearerProof = "authenticated" | "rejected" | "inconclusive";

/** Blocked reasons that conclusively refute the saved credential. */
const CONCLUSIVE_BEARER_REJECTIONS: ReadonlySet<string> = new Set([
  "authentication",
  "host-identity",
  "storage-changed",
]);

/**
 * The freshly registered supervisor connects with the saved credential, so
 * its state is the bearer proof — pairing opens no additional verification
 * socket. Bounded and interruptible.
 */
const pairingBearerProof = (
  registry: EnvironmentRegistry.EnvironmentRegistry["Service"],
  environmentId: EnvironmentId,
): Effect.Effect<PairingBearerProof> =>
  registry.stateChanges(environmentId).pipe(
    Stream.filterMap((state) => {
      if (state.phase === "connected") {
        return Result.succeed("authenticated" as const);
      }
      if (
        state.phase === "blocked" &&
        state.lastFailure !== null &&
        state.lastFailure._tag === "ConnectionBlockedError" &&
        CONCLUSIVE_BEARER_REJECTIONS.has(state.lastFailure.reason)
      ) {
        return Result.succeed("rejected" as const);
      }
      return Result.failVoid;
    }),
    Stream.runHead,
    Effect.timeoutOption(Duration.millis(PAIRING_BEARER_PROOF_TIMEOUT_MS)),
    Effect.map((outcome): PairingBearerProof =>
      Option.isSome(outcome) && Option.isSome(outcome.value) ? outcome.value.value : "inconclusive",
    ),
    Effect.orElseSucceed((): PairingBearerProof => "inconclusive"),
  );

const classifyAttemptError = (error: ConnectionAttemptError): PairingAddError => {
  if (error._tag === "ConnectionBlockedError" && error.reason === "host-identity") {
    return new PairingAddError({ reason: "host-identity-mismatch", detail: error.detail });
  }
  if (
    error._tag === "ConnectionBlockedError" &&
    (error.reason === "authentication" || error.reason === "permission")
  ) {
    return new PairingAddError({ reason: "pairing-rejected", detail: error.detail });
  }
  return new PairingAddError({ reason: "unreachable", detail: error.detail });
};

const parsePayload = (
  code: string,
): Effect.Effect<
  RemotePairingCodePayload,
  PairingCodeParseError | PairingCodeUnsupportedVersionError
> =>
  Effect.try({
    try: () => parsePairingCode(code),
    catch: (cause) =>
      isPairingCodeUnsupportedVersionError(cause)
        ? cause
        : isPairingCodeParseError(cause)
          ? cause
          : new PairingCodeParseError({ detail: String(cause) }),
  });

export const verifyAndAddPairingCode = Effect.fn(
  "clientRuntime.connection.pairingAdd.verifyAndAddPairingCode",
)(function* (input: VerifyPairingCodeInput) {
  const payload = yield* parsePayload(input.code);
  const label = input.label?.trim() || payload.name;

  switch (classifyPairingEndpoint(payload.endpoint)) {
    case "unconnectable":
      return yield* new PairingAddError({
        reason: "unreachable",
        detail: `The pairing endpoint ${payload.endpoint} is not a connectable address.`,
      });
    case "loopback":
      if (input.allowLoopbackTunnel !== true) {
        return yield* new PairingLoopbackAcknowledgementRequiredError({
          endpoint: payload.endpoint,
        });
      }
      break;
    case "private-network":
    case "public":
      break;
  }

  const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
  const identities = yield* Persistence.AcceptedStorageIdentityStore;
  const entries = yield* SubscriptionRef.get(registry.entries);
  // Keyed by the host's storage instance id, never by the environment id it
  // declares: every server calls itself "local", so that id collides with the
  // client's own Local environment and with every other saved remote.
  const environmentId = remoteEnvironmentId(payload.storageInstanceId);
  for (const entry of entries.values()) {
    const accepted = yield* identities.get(storageIdentityTargetKey(entry.target));
    if (Option.isSome(accepted) && accepted.value === payload.storageInstanceId) {
      return yield* new PairingAddError({
        reason: "duplicate-storage-identity",
        detail: `${entry.target.label} already uses this server's storage identity.`,
      });
    }
  }

  const httpBaseUrl = yield* Effect.try({
    try: () => normalizeHttpBaseUrl(payload.endpoint),
    catch: () =>
      new PairingAddError({
        reason: "unreachable",
        detail: `The pairing endpoint ${payload.endpoint} is not a valid HTTP URL.`,
      }),
  });
  const descriptor = yield* fetchRemoteEnvironmentDescriptor({ httpBaseUrl }).pipe(
    Effect.mapError((error) => {
      const mapped = mapRemoteEnvironmentError(error);
      return new PairingAddError({ reason: "unreachable", detail: mapped.message });
    }),
  );

  const saved = entries.get(environmentId);
  if (saved !== undefined) {
    return yield* new PairingAddError({
      reason: "duplicate-storage-identity",
      detail: `${saved.target.label} is already saved.`,
    });
  }

  const verdict = computeCompatVerdict(descriptor);
  if (verdict.kind === "server-too-old" || verdict.kind === "client-too-old") {
    return yield* new PairingAddError({
      reason: "incompatible",
      detail:
        verdict.kind === "server-too-old"
          ? `${descriptor.label} runs a protocol older than this app supports. Update the server.`
          : `${descriptor.label} requires a newer app. Update this app.`,
    });
  }

  const sessions = yield* RpcSession.RpcSessionFactory;
  const connectionId = `bearer:${environmentId}`;
  const target = new BearerConnectionTarget({
    environmentId,
    label,
    connectionId,
    serverEnvironmentId: descriptor.environmentId,
  });
  const prepared: PreparedConnection = {
    environmentId,
    label,
    descriptor,
    httpBaseUrl,
    socketUrl: e2eeSocketUrl(deriveWsBaseUrl(httpBaseUrl)),
    httpAuthorization: null,
    e2ee: {
      hostKey: payload.hostKey,
      auth: { kind: "pairing", token: payload.token },
    },
    target,
  };
  return yield* Effect.scoped(
    Effect.gen(function* () {
      const session = yield* sessions.connect(prepared);
      yield* session.ready;
      const authenticated = yield* session.e2eeAuthenticated;
      if (authenticated === null || authenticated.credential === undefined) {
        return yield* new PairingAddError({
          reason: "pairing-rejected",
          detail: "The host did not return a device credential.",
        });
      }
      if (
        authenticated.storageInstanceId !== payload.storageInstanceId ||
        (authenticated.environmentId !== undefined &&
          authenticated.environmentId !== descriptor.environmentId)
      ) {
        return yield* new PairingAddError({
          reason: "host-identity-mismatch",
          detail: POST_BOOTSTRAP_IDENTITY_MISMATCH_DETAIL,
        });
      }
      const config = yield* session.initialConfig;
      if (
        config.environment.environmentId !== descriptor.environmentId ||
        config.environment.storageInstanceId !== payload.storageInstanceId
      ) {
        return yield* new PairingAddError({
          reason: "host-identity-mismatch",
          detail: POST_BOOTSTRAP_IDENTITY_MISMATCH_DETAIL,
        });
      }
      const verified = {
        credential: authenticated.credential,
        environmentId,
        storageInstanceId: authenticated.storageInstanceId,
      };
      const registration = new BearerConnectionRegistration({
        target,
        profile: new BearerConnectionProfile({
          connectionId,
          environmentId: verified.environmentId,
          label,
          httpBaseUrl,
          wsBaseUrl: deriveWsBaseUrl(httpBaseUrl),
          hostKey: payload.hostKey,
        }),
        credential: new BearerConnectionCredential({ token: verified.credential }),
      });
      const identity = {
        targetKey: storageIdentityTargetKey(target),
        storageInstanceId: verified.storageInstanceId,
      };
      let registrationWritten = false;
      let identityWritten = false;
      let previousStorageInstanceId: string | null = null;
      let authorityOwned = false;
      let cleanupCompleted = false;

      const rollbackLocalWrites = () => {
        return Effect.gen(function* () {
          const cleanupFailures: string[] = [];
          if (identityWritten) {
            const failure = yield* identities
              .rollbackAcceptance(identity, previousStorageInstanceId)
              .pipe(
                Effect.match({
                  onFailure: (cause) => `identity cleanup failed: ${cause.message}`,
                  onSuccess: () => null,
                }),
              );
            if (failure !== null) cleanupFailures.push(failure);
          }
          if (registrationWritten) {
            const failure = yield* registry.rollbackRegistration(registration).pipe(
              Effect.match({
                onFailure: (cause) => `registration cleanup failed: ${cause.message}`,
                onSuccess: () => null,
              }),
            );
            if (failure !== null) cleanupFailures.push(failure);
          }
          cleanupCompleted = true;
          return cleanupFailures;
        });
      };

      const persistAndConfirm = Effect.gen(function* () {
        yield* Effect.uninterruptible(
          Effect.gen(function* () {
            yield* registry
              .register(registration)
              .pipe(Effect.mapError(() => postBootstrapPersistenceError("the server connection")));
            registrationWritten = true;
          }),
        );
        yield* Effect.uninterruptible(
          Effect.gen(function* () {
            previousStorageInstanceId = yield* identities
              .transition(identity.targetKey, (currentStorageInstanceId) => ({
                result: currentStorageInstanceId,
                mutation: {
                  _tag: "Set" as const,
                  storageInstanceId: identity.storageInstanceId,
                },
              }))
              .pipe(Effect.mapError(() => postBootstrapPersistenceError("the server identity")));
            identityWritten = true;
          }),
        );
        const confirmed = yield* Effect.uninterruptibleMask((restore) =>
          Effect.gen(function* () {
            const confirmation = yield* restore(
              session.client[WS_METHODS.authConfirmPairing]({}),
            ).pipe(Effect.exit);
            if (confirmation._tag === "Failure") {
              const disposition = classifyPairingConfirmationFailure(
                confirmation.cause,
                authenticated.pairingConfirmationRequired === true,
                classifyPairingEndpoint(payload.endpoint) === "loopback",
              );
              if (disposition === "rollback") {
                return yield* pairingConfirmationFailure(confirmation.cause);
              }
              // Ambiguous: the confirmation may have committed server-side,
              // so removing the durable local credential is no longer safe.
              return false;
            }
            return true;
          }),
        );
        authorityOwned = true;

        // Interruptible from here on. The supervisor connecting with the
        // saved credential is the bearer proof, so pairing opens no extra
        // verification socket and an unresponsive host can no longer pin an
        // uncancellable fiber through three fresh handshakes.
        yield* registry.retryNow(verified.environmentId);
        if (!confirmed) {
          const proof = yield* pairingBearerProof(registry, verified.environmentId);
          if (proof === "rejected") {
            // The server conclusively refuses the credential: the ambiguous
            // confirmation did not commit and the pending session is revoked.
            // Reporting success would save a permanently dead entry.
            // authorityOwned stays true so the outer handler passes this
            // failure through instead of re-wrapping it; cleanup runs here.
            const cleanupFailures = yield* rollbackLocalWrites();
            const cleanupDetail =
              cleanupFailures.length === 0
                ? ""
                : ` Local cleanup also failed: ${cleanupFailures.join("; ")}.`;
            return yield* new PairingAddError({
              reason: "pairing-rejected",
              detail: `The server rejected the paired credential before confirmation completed; the one-time code was consumed, so generate a new pairing code and pair again.${cleanupDetail}`,
            });
          }
          // "authenticated" proves the saved credential is active;
          // "inconclusive" keeps the saved entry and leaves recovery to the
          // supervisor, exactly like a lost confirmation reply.
        }
        return registration.target.environmentId as EnvironmentId;
      });

      return yield* persistAndConfirm.pipe(
        Effect.catch((error) =>
          authorityOwned
            ? Effect.fail(error)
            : Effect.gen(function* () {
                const cleanupFailures = yield* rollbackLocalWrites();
                const cleanupDetail =
                  cleanupFailures.length === 0
                    ? " No partial local writes from this attempt remain."
                    : ` Local cleanup also failed: ${cleanupFailures.join("; ")}.`;
                return yield* new PairingAddError({
                  reason: "local-persistence-failed",
                  detail: `${error.detail}${cleanupDetail}`,
                });
              }),
        ),
        Effect.ensuring(
          Effect.suspend(() =>
            authorityOwned || cleanupCompleted
              ? Effect.void
              : rollbackLocalWrites().pipe(Effect.ignore),
          ),
        ),
      );
    }),
  ).pipe(
    Effect.mapError((error) => (isPairingAddError(error) ? error : classifyAttemptError(error))),
  );
});
