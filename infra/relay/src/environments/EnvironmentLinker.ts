import {
  RelayEnvironmentLinkProofPayload,
  RelayEnvironmentLinkProofInvalidReason,
  type RelayEnvironmentLinkRequest,
} from "@coddy/contracts/relay";
import {
  decodeRelayJwt,
  normalizeRelayIssuer,
  RELAY_LINK_PROOF_TYP,
  verifyRelayJwt,
} from "@coddy/shared/relayJwt";
import * as Context from "effect/Context";
import * as DateTime from "effect/DateTime";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as Layer from "effect/Layer";
import * as Schema from "effect/Schema";

import * as DpopProofs from "../auth/DpopProofs.ts";
import * as RelayTokens from "../auth/RelayTokens.ts";
import * as EnvironmentCredentials from "./EnvironmentCredentials.ts";
import * as EnvironmentLinks from "./EnvironmentLinks.ts";
import * as ManagedEndpointAllocations from "./ManagedEndpointAllocations.ts";
import * as ManagedEndpointProvider from "./ManagedEndpointProvider.ts";
import * as RelayConfiguration from "../Config.ts";

export class EnvironmentLinkProofExpired extends Schema.TaggedError<EnvironmentLinkProofExpired>()(
  "EnvironmentLinkProofExpired",
  {
    userId: Schema.String,
    environmentId: Schema.String,
    expiresAt: Schema.String,
  },
) {
  override get message(): string {
    return `Environment '${this.environmentId}' link proof expired at ${this.expiresAt}`;
  }
}

export class EnvironmentLinkProofInvalid extends Schema.TaggedError<EnvironmentLinkProofInvalid>()(
  "EnvironmentLinkProofInvalid",
  {
    userId: Schema.String,
    environmentId: Schema.String,
    reason: RelayEnvironmentLinkProofInvalidReason,
    stage: Schema.Literals([
      "decode_token",
      "decode_payload",
      "verify_proof",
      "authorize_capabilities",
      "validate_descriptor",
      "verify_challenge",
      "validate_expiration",
      "consume_proof_nonce",
      "consume_challenge_nonce",
      "validate_origin",
      "validate_endpoint",
    ]),
    cause: Schema.optional(Schema.Defect()),
  },
) {
  override get message(): string {
    return `Environment '${this.environmentId}' link proof is invalid during ${this.stage}: ${this.reason}`;
  }
}

export type EnvironmentLinkError =
  | EnvironmentLinkProofExpired
  | EnvironmentLinkProofInvalid
  | DpopProofs.DpopProofReplayPersistenceError
  | EnvironmentLinks.EnvironmentLinkLookupPersistenceError
  | EnvironmentLinks.EnvironmentLinkUpsertPersistenceError
  | EnvironmentCredentials.EnvironmentCredentialCreatePersistenceError
  | ManagedEndpointAllocations.ManagedEndpointAllocationPersistenceError
  | ManagedEndpointProvider.ManagedEndpointProviderError;

export class EnvironmentLinker extends Context.Service<
  EnvironmentLinker,
  {
    readonly link: (input: {
      readonly userId: string;
      readonly request: RelayEnvironmentLinkRequest;
    }) => Effect.Effect<
      {
        readonly environmentId: RelayEnvironmentLinkProofPayload["environmentId"];
        readonly endpoint: RelayEnvironmentLinkProofPayload["endpoint"];
        readonly endpointRuntime:
          | ManagedEndpointProvider.ManagedEndpointProvisioningResult["runtime"]
          | null;
        readonly environmentCredential: string;
      },
      EnvironmentLinkError
    >;
  }
>()("coddy-studio-relay/environments/EnvironmentLinker") {}

const decodeProof = Schema.decodeUnknownEffect(RelayEnvironmentLinkProofPayload);

function proofAuthorizesRequestedCapabilities(
  proof: RelayEnvironmentLinkProofPayload,
  request: RelayEnvironmentLinkRequest,
): boolean {
  const scopes = new Set(proof.scopes);
  if (request.managedTunnelsEnabled && !scopes.has("managed_tunnels")) {
    return false;
  }
  return true;
}

function isSecureManagedEndpoint(endpoint: RelayEnvironmentLinkProofPayload["endpoint"]): boolean {
  try {
    const httpUrl = new URL(endpoint.httpBaseUrl);
    const wsUrl = new URL(endpoint.wsBaseUrl);
    return httpUrl.protocol === "https:" && wsUrl.protocol === "wss:";
  } catch {
    return false;
  }
}

function normalizeHostname(hostname: string): string {
  return hostname
    .trim()
    .toLowerCase()
    .replace(/^\[(.*)\]$/u, "$1");
}

function isLoopbackManagedTunnelOrigin(
  origin: RelayEnvironmentLinkProofPayload["origin"],
): boolean {
  const hostname = normalizeHostname(origin.localHttpHost);
  return (
    (hostname === "127.0.0.1" || hostname === "::1" || hostname === "localhost") &&
    Number.isInteger(origin.localHttpPort) &&
    origin.localHttpPort > 0 &&
    origin.localHttpPort <= 65_535
  );
}

const make = Effect.gen(function* () {
  const links = yield* EnvironmentLinks.EnvironmentLinks;
  const credentials = yield* EnvironmentCredentials.EnvironmentCredentials;
  const allocations = yield* ManagedEndpointAllocations.ManagedEndpointAllocations;
  const managedEndpointProvider = yield* ManagedEndpointProvider.ManagedEndpointProvider;
  const proofReplay = yield* DpopProofs.DpopProofReplay;
  const relayTokens = yield* RelayTokens.RelayTokens;
  const config = yield* RelayConfiguration.RelayConfiguration;

  return EnvironmentLinker.of({
    link: Effect.fn("relay.environment_linker.link")(function* (input) {
      const now = yield* DateTime.now;
      const nowSeconds = Math.floor(now.epochMilliseconds / 1_000);
      const unverified = yield* Effect.try({
        try: () => decodeRelayJwt(input.request.proof),
        catch: (cause) =>
          new EnvironmentLinkProofInvalid({
            userId: input.userId,
            environmentId: "unknown",
            reason: "invalid_signature_or_scope",
            stage: "decode_token",
            cause,
          }),
      });
      const candidate = yield* decodeProof(unverified).pipe(
        Effect.mapError(
          (cause) =>
            new EnvironmentLinkProofInvalid({
              userId: input.userId,
              environmentId: "unknown",
              reason: "invalid_signature_or_scope",
              stage: "decode_payload",
              cause,
            }),
        ),
      );
      yield* Effect.annotateCurrentSpan({
        "relay.environment_id": candidate.environmentId,
        "relay.link.managed_tunnels_enabled": input.request.managedTunnelsEnabled,
      });
      if (candidate.exp <= nowSeconds) {
        return yield* new EnvironmentLinkProofExpired({
          userId: input.userId,
          environmentId: candidate.environmentId,
          expiresAt: DateTime.formatIso(DateTime.makeUnsafe(candidate.exp * 1_000)),
        });
      }
      const issuer = `coddy-studio-env:${candidate.environmentId}`;
      const relayIssuer = normalizeRelayIssuer(config.relayIssuer);
      const verified = yield* verifyRelayJwt({
        publicKey: candidate.environmentPublicKey,
        token: input.request.proof,
        typ: RELAY_LINK_PROOF_TYP,
        issuer,
        audience: relayIssuer,
        nowEpochSeconds: nowSeconds,
      }).pipe(
        Effect.flatMap(decodeProof),
        Effect.mapError(
          (cause) =>
            new EnvironmentLinkProofInvalid({
              userId: input.userId,
              environmentId: candidate.environmentId,
              reason: "invalid_signature_or_scope",
              stage: "verify_proof",
              cause,
            }),
        ),
      );
      if (
        verified.sub !== verified.environmentId ||
        !proofAuthorizesRequestedCapabilities(verified, input.request)
      ) {
        return yield* new EnvironmentLinkProofInvalid({
          userId: input.userId,
          environmentId: candidate.environmentId,
          reason: "invalid_signature_or_scope",
          stage: "authorize_capabilities",
        });
      }
      if (verified.descriptor.environmentId !== verified.environmentId) {
        return yield* new EnvironmentLinkProofInvalid({
          userId: input.userId,
          environmentId: verified.environmentId,
          reason: "descriptor_mismatch",
          stage: "validate_descriptor",
        });
      }
      const challenge = yield* relayTokens.verifyLinkChallenge({
        token: verified.challenge,
        userId: input.userId,
        request: {
          managedTunnelsEnabled: input.request.managedTunnelsEnabled,
        },
        nowEpochSeconds: nowSeconds,
      });
      if (challenge === null) {
        return yield* new EnvironmentLinkProofInvalid({
          userId: input.userId,
          environmentId: verified.environmentId,
          reason: "challenge_invalid",
          stage: "verify_challenge",
        });
      }
      const expiresAt = DateTime.make(verified.exp * 1_000);
      if (expiresAt._tag === "None") {
        return yield* new EnvironmentLinkProofInvalid({
          userId: input.userId,
          environmentId: verified.environmentId,
          reason: "invalid_signature_or_scope",
          stage: "validate_expiration",
        });
      }
      const consumedNonce = yield* proofReplay.consume({
        thumbprint: verified.environmentPublicKey,
        jti: verified.jti,
        iat: verified.iat,
        expiresAt: expiresAt.value,
      });
      if (!consumedNonce) {
        return yield* new EnvironmentLinkProofInvalid({
          userId: input.userId,
          environmentId: verified.environmentId,
          reason: "replayed_nonce",
          stage: "consume_proof_nonce",
        });
      }
      const consumedChallenge = yield* proofReplay.consume({
        thumbprint: "relay-environment-link-challenge",
        jti: challenge.jti,
        iat: challenge.iat,
        expiresAt: expiresAt.value,
      });
      if (!consumedChallenge) {
        return yield* new EnvironmentLinkProofInvalid({
          userId: input.userId,
          environmentId: verified.environmentId,
          reason: "challenge_invalid",
          stage: "consume_challenge_nonce",
        });
      }
      if (input.request.managedTunnelsEnabled && !isLoopbackManagedTunnelOrigin(verified.origin)) {
        return yield* new EnvironmentLinkProofInvalid({
          userId: input.userId,
          environmentId: verified.environmentId,
          reason: "origin_not_allowed",
          stage: "validate_origin",
        });
      }
      return yield* allocations.withOperation(
        {
          userId: input.userId,
          environmentId: verified.environmentId,
          kind: "link",
        },
        (ownership) => {
          let linkPersisted = false;
          let previousLink: EnvironmentLinks.RelayLinkedEnvironmentRecord | null = null;
          let provisioned: ManagedEndpointProvider.ManagedEndpointProvisioningResult | null = null;
          let credentialRotation: EnvironmentCredentials.EnvironmentCredentialRotation | null =
            null;
          const compensate = Effect.gen(function* () {
            if (linkPersisted) {
              if (credentialRotation !== null) {
                const rolledBack = yield* credentials.rollbackRotation(credentialRotation);
                if (!rolledBack) {
                  return yield* new EnvironmentCredentials.EnvironmentCredentialCreatePersistenceError(
                    {
                      stage: "rollback-credential",
                      environmentId: credentialRotation.environmentId,
                      credentialId: credentialRotation.credentialId,
                      cause: new Error("The replacement credential is no longer active"),
                    },
                  );
                }
              }
              if (previousLink === null) {
                yield* links
                  .revokeForUser({
                    userId: input.userId,
                    environmentId: verified.environmentId,
                  })
                  .pipe(
                    Effect.catch(() =>
                      Effect.logWarning(
                        "Managed environment link compensation could not revoke link",
                      ),
                    ),
                  );
              } else {
                yield* links
                  .restoreForUser({ userId: input.userId, record: previousLink })
                  .pipe(
                    Effect.catch(() =>
                      Effect.logWarning(
                        "Managed environment link compensation could not restore prior link",
                      ),
                    ),
                  );
              }
              yield* credentials
                .revokeOrphanedForEnvironment({ environmentId: verified.environmentId })
                .pipe(
                  Effect.catch(() =>
                    Effect.logWarning(
                      "Managed environment link compensation could not revoke credentials",
                    ),
                  ),
                );
            }
            if (provisioned?.endpointDisposition === "created") {
              yield* managedEndpointProvider
                .deprovision({
                  userId: input.userId,
                  environmentId: verified.environmentId,
                  ownership,
                })
                .pipe(
                  Effect.catch(() =>
                    Effect.logWarning(
                      "Managed environment link compensation could not deprovision endpoint",
                    ),
                  ),
                );
            }
          });
          const workflow = Effect.gen(function* () {
            previousLink = yield* links.getForUser({
              userId: input.userId,
              environmentId: verified.environmentId,
            });
            provisioned = input.request.managedTunnelsEnabled
              ? yield* managedEndpointProvider.provision({
                  userId: input.userId,
                  environmentId: verified.environmentId,
                  origin: verified.origin,
                  ownership,
                })
              : null;
            const endpoint = provisioned?.endpoint ?? verified.endpoint;
            if (!isSecureManagedEndpoint(endpoint)) {
              return yield* new EnvironmentLinkProofInvalid({
                userId: input.userId,
                environmentId: verified.environmentId,
                reason: "endpoint_not_secure",
                stage: "validate_endpoint",
              });
            }
            const environmentCredential = yield* Effect.uninterruptible(
              Effect.gen(function* () {
                linkPersisted = true;
                yield* links.upsert({ ...input, proof: verified, endpoint });
                credentialRotation = yield* credentials.rotate({
                  environmentId: verified.environmentId,
                  environmentPublicKey: verified.environmentPublicKey,
                });
                return credentialRotation.token;
              }),
            );
            yield* Effect.yieldNow;
            return {
              environmentId: verified.environmentId,
              endpoint,
              endpointRuntime: provisioned?.runtime ?? null,
              environmentCredential,
            };
          });
          return workflow.pipe(
            Effect.onExit((exit) => (Exit.isFailure(exit) ? compensate : Effect.void)),
          );
        },
      );
    }),
  });
});

export const layer = Layer.effect(EnvironmentLinker, make);
