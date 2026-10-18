import {
  ClientPresentation,
  CloudSession,
  EnvironmentOwnedDataCleanup,
  PlatformConnectionSource,
  PrimaryEnvironmentAuth,
  SshEnvironmentGateway,
} from "@bibcode/client-runtime/platform";
import {
  BearerConnectionCredential,
  BearerConnectionProfile,
  BearerConnectionRegistration,
  BearerConnectionTarget,
  ConnectionBlockedError,
  ConnectionTransientError,
  Connectivity,
  EnvironmentSelection,
  mapRemoteEnvironmentError,
  type PlatformConnectionRegistration,
  sshCredentialRejectedError,
  PrimaryConnectionRegistration,
  PrimaryConnectionTarget,
  UnavailableConnectionRegistration,
  UnavailableConnectionTarget,
  Wakeups,
} from "@bibcode/client-runtime/connection";
import { bootstrapRemoteBearerSession } from "@bibcode/client-runtime/authorization";
import { fetchRemoteEnvironmentDescriptor } from "@bibcode/client-runtime/environment";
import { managedRelayAccountChanges, managedRelaySessionAtom } from "@bibcode/client-runtime/relay";
import { EnvironmentRpcRequestObserver } from "@bibcode/client-runtime/rpc";
import {
  AuthStandardClientScopes,
  type DesktopBridge,
  type DesktopEnvironmentBootstrap,
  type DesktopSshEnvironmentBootstrap,
  type DesktopSshEnvironmentTarget,
  EnvironmentId,
  PRIMARY_LOCAL_ENVIRONMENT_ID,
} from "@bibcode/contracts";
import * as Clock from "effect/Clock";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Queue from "effect/Queue";
import * as Ref from "effect/Ref";
import * as Stream from "effect/Stream";
import { FetchHttpClient } from "effect/unstable/http";

import { readDesktopPrimaryBearerToken } from "../environments/primary/desktopAuth";
import { primaryEnvironmentHttpLayer } from "../environments/primary/httpLayer";
import {
  readPrimaryEnvironmentTarget,
  resolvePrimaryEnvironmentHttpBaseUrl,
  type PrimaryEnvironmentTarget,
} from "../environments/primary/target";
import { forgetPreviewStorage } from "../browser/previewStorageCleanup";
import { clearComposerDraftsEnvironment } from "../composerDraftStore";
import {
  isHostedStaticApp,
  isServedPrimarySuppressed,
  onServedPrimarySuppressionChange,
} from "../hostedPairing";
import { clientCollector, PROFILER_CORRELATION_HEADER } from "../profiler/clientCollector";
import { appAtomRegistry } from "../rpc/atomRegistry";
import { makeEnvironmentSelection } from "./environmentSelection";
import { acknowledgeRpcRequest, trackRpcRequestSent } from "../rpc/requestLatencyState";
import {
  desktopLocalConnectionId,
  readDesktopSecondaryBootstrapsResult,
  type DesktopSecondaryBootstrapsRead,
} from "./desktopLocal";
import { connectionStorageLayer } from "./storage";

let nextObservedRpcRequestId = 0;

function currentNetworkStatus(): "unknown" | "offline" | "online" {
  if (typeof navigator === "undefined") {
    return "unknown";
  }
  return navigator.onLine ? "online" : "offline";
}

const connectivityLayer = Connectivity.layer({
  status: Effect.sync(currentNetworkStatus),
  changes: Stream.callback((queue) =>
    Effect.acquireRelease(
      Effect.sync(() => {
        const online = () => Queue.offerUnsafe(queue, "online");
        const offline = () => Queue.offerUnsafe(queue, "offline");
        window.addEventListener("online", online);
        window.addEventListener("offline", offline);
        return { online, offline };
      }),
      ({ online, offline }) =>
        Effect.sync(() => {
          window.removeEventListener("online", online);
          window.removeEventListener("offline", offline);
        }),
    ).pipe(Effect.asVoid),
  ),
});

/**
 * Running totals of browser wakeups. Every frame carries both totals, so a
 * frame dropped by the sliding buffer is still represented by any later one.
 */
interface BrowserWakeupCounts {
  /** Visible `visibilitychange` events: the supervisor's `application-active`. */
  readonly applicationActive: number;
  /** Returns to an inactive window by focus or visibility: view refreshes. */
  readonly focusReturn: number;
}

const wakeupsLayer = Layer.effect(
  Wakeups.ConnectionWakeups,
  Effect.gen(function* () {
    // Totals outlive listener restarts so a new consumer can start from them.
    const counts = { applicationActive: 0, focusReturn: 0 };
    const browserWakeups = Stream.callback<BrowserWakeupCounts>((queue) =>
      Effect.acquireRelease(
        Effect.sync(() => {
          let active = document.visibilityState === "visible" && document.hasFocus();
          const publish = () => Queue.offerUnsafe(queue, { ...counts });
          const leave = () => {
            active = false;
          };
          const enter = () => {
            if (active || document.visibilityState !== "visible") return false;
            active = true;
            counts.focusReturn += 1;
            return true;
          };
          const focus = () => {
            if (enter()) publish();
          };
          const visibility = () => {
            if (document.visibilityState === "visible") {
              // Keep the supervisor's original visibility-only wakeup semantics.
              enter();
              counts.applicationActive += 1;
              publish();
            } else leave();
          };
          window.addEventListener("blur", leave);
          window.addEventListener("focus", focus);
          document.addEventListener("visibilitychange", visibility);
          return { leave, focus, visibility };
        }),
        ({ leave, focus, visibility }) =>
          Effect.sync(() => {
            window.removeEventListener("blur", leave);
            window.removeEventListener("focus", focus);
            document.removeEventListener("visibilitychange", visibility);
          }),
      ).pipe(Effect.asVoid),
    );
    const browserChanges = yield* browserWakeups.pipe(
      Stream.share({ capacity: 1, strategy: "sliding", idleTimeToLive: 0 }),
    );
    // Emits once for each frame in which the selected total has passed what
    // this consumer has seen, starting from the total when it subscribes.
    const advancesOf = (total: (frame: BrowserWakeupCounts) => number) =>
      Stream.suspend(() => {
        let seen = total(counts);
        return browserChanges.pipe(
          Stream.filter((frame) => {
            const next = total(frame);
            if (next <= seen) return false;
            seen = next;
            return true;
          }),
        );
      });
    return Wakeups.make({
      focusVisibility: advancesOf((frame) => frame.focusReturn).pipe(Stream.map(() => undefined)),
      changes: Stream.merge(
        advancesOf((frame) => frame.applicationActive).pipe(
          Stream.map(() => "application-active" as const),
        ),
        managedRelayAccountChanges(appAtomRegistry).pipe(
          Stream.map(() => "credentials-changed" as const),
        ),
      ),
    });
  }),
);

function clientMetadata() {
  const desktop = window.desktopBridge !== undefined;
  const platform = navigator.platform.trim();
  return {
    label: desktop ? "BiBCode Desktop" : "BiBCode Web",
    deviceType: "desktop" as const,
    ...(platform === "" ? {} : { os: platform }),
  };
}

const SSH_HTTP_STATUS_PREFIX = /^\[ssh_http:(\d{3})\]\s*/u;
const SSH_TIMEOUT_PREFIX = /^\[ssh_timeout:[a-z-]+\]\s*/u;
const SSH_CANCELLED_PREFIX = /^\[ssh_cancelled\]\s*/u;

/**
 * Classifies a desktop SSH bridge failure. The bridge prefixes a remote API
 * refusal with `[ssh_http:<status>]` and an expired remote-script deadline
 * with `[ssh_timeout:<operation>]`, and the user's own cancellation of the
 * password prompt with `[ssh_cancelled]`. A refusal is blocked (it will not pass on
 * its own); network failures, 5xx, and deadlines are transient.
 */
export function sshPreparationError(cause: unknown) {
  const message = cause instanceof Error ? cause.message : String(cause);
  const status = SSH_HTTP_STATUS_PREFIX.exec(message)?.[1];
  switch (status) {
    case "401":
      return new ConnectionBlockedError({
        reason: "authentication",
        detail: "The SSH environment rejected the credential.",
      });
    case "403":
      return new ConnectionBlockedError({
        reason: "permission",
        detail: "The SSH environment credential does not grant the required access.",
      });
    case "400":
      return new ConnectionBlockedError({
        reason: "configuration",
        detail: "The SSH environment rejected the authentication request.",
      });
  }
  if (SSH_TIMEOUT_PREFIX.test(message)) {
    return new ConnectionTransientError({
      reason: "timeout",
      detail: `${message.replace(SSH_TIMEOUT_PREFIX, "")} Check the connection; BiBCode keeps trying.`,
    });
  }
  if (SSH_CANCELLED_PREFIX.test(message)) {
    return new ConnectionBlockedError({
      reason: "authentication",
      detail: message.replace(SSH_CANCELLED_PREFIX, ""),
    });
  }
  return new ConnectionTransientError({
    reason: "remote-unavailable",
    detail: `Could not prepare the SSH environment: ${message}`,
  });
}

const ensureDesktopSshEnvironment = (
  bridge: DesktopBridge,
  target: DesktopSshEnvironmentTarget,
  issuePairingToken: boolean,
) =>
  Effect.tryPromise({
    try: () => bridge.ensureSshEnvironment(target, { issuePairingToken }),
    catch: sshPreparationError,
  });

function requirePairingToken(bootstrap: DesktopSshEnvironmentBootstrap) {
  return bootstrap.pairingToken === null
    ? Effect.fail(
        new ConnectionBlockedError({
          reason: "authentication",
          detail: "The SSH environment did not issue a pairing credential.",
        }),
      )
    : Effect.succeed(bootstrap.pairingToken);
}

// Standard scopes only: a leaked saved bearer must not manage access or mint
// pairings on the host. SSH access stays the authority for this desktop.
const exchangeSshPairingToken = (
  bridge: DesktopBridge,
  bootstrap: DesktopSshEnvironmentBootstrap,
  pairingToken: string,
) =>
  Effect.tryPromise({
    try: () =>
      bridge.bootstrapSshBearerSession(
        bootstrap.httpBaseUrl,
        pairingToken,
        AuthStandardClientScopes,
      ),
    catch: sshPreparationError,
  });

export const provisionDesktopSshEnvironment = Effect.fn(
  "web.connectionPlatform.ssh.provisionDesktop",
)(function* (bridge: DesktopBridge, target: DesktopSshEnvironmentTarget) {
  const bootstrap = yield* ensureDesktopSshEnvironment(bridge, target, true);
  const pairingToken = yield* requirePairingToken(bootstrap);
  // The descriptor comes first so a descriptor failure does not spend the token.
  const descriptor = yield* Effect.tryPromise({
    try: () => bridge.fetchSshEnvironmentDescriptor(bootstrap.httpBaseUrl),
    catch: sshPreparationError,
  });
  const access = yield* exchangeSshPairingToken(bridge, bootstrap, pairingToken);
  return {
    environmentId: descriptor.environmentId,
    label: descriptor.label,
    bootstrap,
    bearerToken: access.access_token,
  };
});

export const mintDesktopSshBearer = Effect.fn("web.connectionPlatform.ssh.mintDesktop")(function* (
  bridge: DesktopBridge,
  target: DesktopSshEnvironmentTarget,
) {
  const bootstrap = yield* ensureDesktopSshEnvironment(bridge, target, true);
  const pairingToken = yield* requirePairingToken(bootstrap);
  const access = yield* exchangeSshPairingToken(bridge, bootstrap, pairingToken).pipe(
    Effect.mapError((error) =>
      error._tag === "ConnectionBlockedError" && error.reason === "authentication"
        ? sshCredentialRejectedError(target.alias || target.hostname)
        : error,
    ),
  );
  return { bootstrap, bearerToken: access.access_token };
});

const desktopBridgeOrUnsupported = Effect.suspend(() => {
  const bridge = window.desktopBridge;
  return bridge === undefined
    ? Effect.fail(
        new ConnectionBlockedError({
          reason: "unsupported",
          detail: "SSH environments are only available in the desktop app.",
        }),
      )
    : Effect.succeed(bridge);
});

const capabilitiesLayer = Layer.effectContext(
  Effect.sync(() => {
    const presentation = ClientPresentation.of({
      metadata: clientMetadata(),
      scopes: AuthStandardClientScopes,
    });
    const cloudSession = CloudSession.of({
      clerkToken: Effect.gen(function* () {
        const session = appAtomRegistry.get(managedRelaySessionAtom);
        if (session === null) {
          return yield* new ConnectionBlockedError({
            reason: "authentication",
            detail: "Sign in to BiBCode Cloud to connect this environment.",
          });
        }
        const token = yield* session.readClerkToken().pipe(
          Effect.mapError(
            (error) =>
              new ConnectionTransientError({
                reason: "network",
                detail: error.message,
              }),
          ),
        );
        if (token === null) {
          return yield* new ConnectionBlockedError({
            reason: "authentication",
            detail: "The BiBCode Cloud session is unavailable.",
          });
        }
        return token;
      }),
    });
    const primaryAuth = PrimaryEnvironmentAuth.of({
      bearerToken: Effect.tryPromise({
        try: readDesktopPrimaryBearerToken,
        catch: (cause) =>
          new ConnectionTransientError({
            reason: "remote-unavailable",
            detail: `Could not load the desktop primary credential: ${String(cause)}`,
          }),
      }).pipe(Effect.map(Option.fromNullishOr)),
    });
    const ssh = SshEnvironmentGateway.of({
      provision: Effect.fn("web.connectionPlatform.ssh.provision")(function* (target) {
        const bridge = yield* desktopBridgeOrUnsupported;
        return yield* provisionDesktopSshEnvironment(bridge, target);
      }),
      ensureTunnel: Effect.fn("web.connectionPlatform.ssh.ensureTunnel")(function* (input) {
        const bridge = yield* desktopBridgeOrUnsupported;
        return yield* ensureDesktopSshEnvironment(bridge, input.target, false);
      }),
      mintBearer: Effect.fn("web.connectionPlatform.ssh.mintBearer")(function* (input) {
        const bridge = yield* desktopBridgeOrUnsupported;
        return yield* mintDesktopSshBearer(bridge, input.target);
      }),
      disconnect: Effect.fn("web.connectionPlatform.ssh.disconnect")(function* (target) {
        const bridge = window.desktopBridge;
        if (bridge === undefined) {
          return;
        }
        yield* Effect.tryPromise({
          try: () => bridge.disconnectSshEnvironment(target),
          catch: (cause) =>
            new ConnectionTransientError({
              reason: "remote-unavailable",
              detail: `Could not disconnect the SSH environment: ${String(cause)}`,
            }),
        });
      }),
    });

    return Context.make(CloudSession, cloudSession).pipe(
      Context.add(PrimaryEnvironmentAuth, primaryAuth),
      Context.add(ClientPresentation, presentation),
      Context.add(SshEnvironmentGateway, ssh),
    );
  }),
);

const loadPrimaryConnectionRegistration = Effect.fn(
  "web.connectionPlatform.loadPrimaryConnectionRegistration",
)(function* (target: PrimaryEnvironmentTarget["target"]) {
  const descriptor = yield* fetchRemoteEnvironmentDescriptor({
    httpBaseUrl: target.httpBaseUrl,
  }).pipe(Effect.provide(primaryEnvironmentHttpLayer), Effect.mapError(mapRemoteEnvironmentError));
  return new PrimaryConnectionRegistration({
    target: new PrimaryConnectionTarget({
      environmentId: descriptor.environmentId,
      label: descriptor.label,
      httpBaseUrl: target.httpBaseUrl,
      wsBaseUrl: target.wsBaseUrl,
    }),
  });
});

// A desktop-local secondary backend (e.g. a parallel WSL backend) lives on its
// own loopback origin, so — unlike the same-origin primary — it authenticates
// with a bearer token minted from the bootstrap credential the desktop issues.
const loadSecondaryConnectionRegistration = Effect.fn(
  "web.connectionPlatform.loadSecondaryConnectionRegistration",
)(function* (entry: DesktopEnvironmentBootstrap) {
  if (entry.preflightError?.kind === "wsl-secondary-unavailable") {
    return {
      registration: new UnavailableConnectionRegistration({
        target: new UnavailableConnectionTarget({
          environmentId: EnvironmentId.make(entry.id),
          label: entry.label,
          connectionId: desktopLocalConnectionId(entry.id),
          configuredDistro: entry.configuredDistro ?? null,
          detail: entry.preflightError.detail,
        }),
      }),
    };
  }
  if (
    entry.httpBaseUrl === null ||
    entry.wsBaseUrl === null ||
    entry.bootstrapToken === undefined
  ) {
    return yield* new ConnectionTransientError({
      reason: "endpoint-unavailable",
      detail: `Desktop-local backend ${entry.id} is not ready yet.`,
    });
  }
  const httpBaseUrl = entry.httpBaseUrl;
  const wsBaseUrl = entry.wsBaseUrl;
  const descriptor = yield* fetchRemoteEnvironmentDescriptor({ httpBaseUrl }).pipe(
    Effect.mapError(mapRemoteEnvironmentError),
  );
  const issuedAtEpochMs = yield* Clock.currentTimeMillis;
  const access = yield* bootstrapRemoteBearerSession({
    httpBaseUrl,
    credential: entry.bootstrapToken,
    scopes: AuthStandardClientScopes,
    clientMetadata: clientMetadata(),
  }).pipe(Effect.mapError(mapRemoteEnvironmentError));
  // Keep the desktop pool's stable backend id in the connection id. The
  // descriptor environment id still scopes projects and RPC state, while the
  // backend id lets desktop-only operations (notably the WSL folder picker)
  // route back to the instance that owns the environment.
  const connectionId = desktopLocalConnectionId(entry.id);
  // Prefer the desktop's bootstrap label (it identifies the backend and distro,
  // e.g. "WSL: Ubuntu") over the generic descriptor label, so consumers can show
  // a meaningful name without recovering it from the bootstrap list later.
  const label = entry.label || descriptor.label;
  return {
    registration: new BearerConnectionRegistration({
      target: new BearerConnectionTarget({
        environmentId: descriptor.environmentId,
        label,
        connectionId,
        // Desktop-local backends are keyed by the host topology, not by what
        // the backend declares, so the two ids stay the same value here.
        serverEnvironmentId: null,
      }),
      profile: new BearerConnectionProfile({
        connectionId,
        environmentId: descriptor.environmentId,
        label,
        httpBaseUrl,
        wsBaseUrl,
        hostKey: null,
      }),
      credential: new BearerConnectionCredential({ token: access.access_token }),
    }),
    expiresAtEpochMs: secondaryBearerExpiresAtEpochMs(issuedAtEpochMs, access.expires_in),
    refreshAtEpochMs: secondaryBearerRefreshAtEpochMs(issuedAtEpochMs, access.expires_in),
  };
});

// Poll cadence for the desktop bootstrap topology. There is no change event on
// the bridge, so the renderer polls; successful registrations are cached by a
// signature of their endpoint + token until bearer credentials approach expiry.
const PLATFORM_POLL_INTERVAL = "3 seconds";
const SECONDARY_BEARER_REFRESH_SKEW_MS = 5_000;

export function secondaryBearerExpiresAtEpochMs(
  issuedAtEpochMs: number,
  expiresInSeconds: number,
): number {
  return issuedAtEpochMs + Math.max(0, expiresInSeconds * 1_000);
}

export function secondaryBearerRefreshAtEpochMs(
  issuedAtEpochMs: number,
  expiresInSeconds: number,
): number {
  return Math.max(
    issuedAtEpochMs,
    secondaryBearerExpiresAtEpochMs(issuedAtEpochMs, expiresInSeconds) -
      SECONDARY_BEARER_REFRESH_SKEW_MS,
  );
}

interface CachedPlatformRegistration {
  readonly signature: string;
  readonly registration: PlatformConnectionRegistration;
  readonly expiresAtEpochMs?: number;
  readonly refreshAtEpochMs?: number;
}

export type PrimaryEnvironmentTargetRead =
  | {
      readonly _tag: "Success";
      readonly target: PrimaryEnvironmentTarget | null;
    }
  | {
      readonly _tag: "Failure";
      readonly cause: unknown;
    };

export function readPrimaryEnvironmentTargetResult(
  readTarget: () => PrimaryEnvironmentTarget | null = readPrimaryEnvironmentTarget,
): PrimaryEnvironmentTargetRead {
  try {
    const snapshot = readTarget();
    return {
      _tag: "Success",
      target:
        snapshot === null
          ? null
          : {
              ...snapshot,
              target: {
                httpBaseUrl: resolvePrimaryEnvironmentHttpBaseUrl(snapshot),
                wsBaseUrl: snapshot.target.wsBaseUrl,
              },
            },
    };
  } catch (cause) {
    return { _tag: "Failure", cause };
  }
}

export function primaryRegistrationToRetainAfterTopologyRead(
  previous: ReadonlyMap<string, CachedPlatformRegistration>,
  topologyRead: PrimaryEnvironmentTargetRead,
): CachedPlatformRegistration | undefined {
  return topologyRead._tag === "Failure" ? previous.get(PRIMARY_LOCAL_ENVIRONMENT_ID) : undefined;
}

export function canReuseCachedPlatformRegistration(
  cached: CachedPlatformRegistration,
  signature: string,
  nowEpochMs: number,
): boolean {
  return (
    cached.signature === signature &&
    (cached.refreshAtEpochMs === undefined || nowEpochMs < cached.refreshAtEpochMs)
  );
}

export function canRetainCachedPlatformRegistrationAfterRefreshFailure(
  cached: CachedPlatformRegistration,
  signature: string,
  nowEpochMs: number,
): boolean {
  return (
    cached.signature === signature &&
    cached.expiresAtEpochMs !== undefined &&
    nowEpochMs < cached.expiresAtEpochMs
  );
}

export function secondaryRegistrationsToRetainAfterTopologyRead(
  previous: ReadonlyMap<string, CachedPlatformRegistration>,
  topologyRead: DesktopSecondaryBootstrapsRead,
  nowEpochMs: number,
): ReadonlyMap<string, CachedPlatformRegistration> {
  if (topologyRead._tag === "Success") {
    return new Map();
  }
  return new Map(
    [...previous].filter(
      ([, cached]) => cached.expiresAtEpochMs !== undefined && nowEpochMs < cached.expiresAtEpochMs,
    ),
  );
}

const platformConnectionSourceLayer = Layer.effect(
  PlatformConnectionSource,
  Effect.gen(function* () {
    if (isHostedStaticApp()) {
      return PlatformConnectionSource.of({
        registrations: Stream.empty,
      });
    }
    const cacheRef = yield* Ref.make(new Map<string, CachedPlatformRegistration>());

    // Resolve the full set of platform-managed environments the host currently
    // reports: the primary (same-origin cookie auth) plus any desktop-local
    // backends running alongside it (bearer auth). Reused registrations come
    // from the cache; a failed entry is skipped and retried on the next poll.
    const buildPlatformRegistrations = Effect.gen(function* () {
      const previous = yield* Ref.get(cacheRef);
      const nowEpochMs = yield* Clock.currentTimeMillis;
      const next = new Map<string, CachedPlatformRegistration>();
      const registrations: Array<PlatformConnectionRegistration> = [];

      // A served page opened as a remote client withholds a new registration of its unpaired
      // server, but keeps one already made: removing it would discard that environment's local
      // data, unsent drafts included.
      const suppressedPrimary = isServedPrimarySuppressed();
      const primaryTopologyRead: PrimaryEnvironmentTargetRead = suppressedPrimary
        ? { _tag: "Success", target: null }
        : readPrimaryEnvironmentTargetResult();
      const retainedPrimary = suppressedPrimary
        ? previous.get(PRIMARY_LOCAL_ENVIRONMENT_ID)
        : primaryRegistrationToRetainAfterTopologyRead(previous, primaryTopologyRead);
      if (retainedPrimary !== undefined) {
        next.set(PRIMARY_LOCAL_ENVIRONMENT_ID, retainedPrimary);
        registrations.push(retainedPrimary.registration);
      }

      if (primaryTopologyRead._tag === "Failure") {
        yield* Effect.logWarning("Could not read the primary environment topology.", {
          cause: primaryTopologyRead.cause,
        });
      } else if (primaryTopologyRead.target !== null) {
        const target = primaryTopologyRead.target.target;
        const signature = `primary|${target.httpBaseUrl}|${target.wsBaseUrl}`;
        const cached = previous.get(PRIMARY_LOCAL_ENVIRONMENT_ID);
        if (
          cached !== undefined &&
          canReuseCachedPlatformRegistration(cached, signature, nowEpochMs)
        ) {
          next.set(PRIMARY_LOCAL_ENVIRONMENT_ID, cached);
          registrations.push(cached.registration);
        } else {
          const built = yield* loadPrimaryConnectionRegistration(target).pipe(
            Effect.tapError((error) =>
              Effect.logWarning("Could not discover the primary environment.", { error }),
            ),
            Effect.option,
          );
          if (Option.isSome(built)) {
            const cacheEntry = { signature, registration: built.value };
            next.set(PRIMARY_LOCAL_ENVIRONMENT_ID, cacheEntry);
            registrations.push(built.value);
          }
        }
      }

      const topologyRead = readDesktopSecondaryBootstrapsResult();
      for (const [id, cached] of secondaryRegistrationsToRetainAfterTopologyRead(
        previous,
        topologyRead,
        nowEpochMs,
      )) {
        next.set(id, cached);
        registrations.push(cached.registration);
      }

      if (topologyRead._tag === "Failure") {
        yield* Effect.logWarning("Could not read the desktop-local backend topology.", {
          cause: topologyRead.cause,
        });
      } else {
        for (const bootstrap of topologyRead.bootstraps) {
          const signature = [
            bootstrap.httpBaseUrl,
            bootstrap.wsBaseUrl,
            bootstrap.bootstrapToken ?? "",
            bootstrap.configuredDistro ?? "",
            bootstrap.preflightError?.kind ?? "",
            bootstrap.preflightError?.detail ?? "",
          ].join("|");
          const cached = previous.get(bootstrap.id);
          if (
            cached !== undefined &&
            canReuseCachedPlatformRegistration(cached, signature, nowEpochMs)
          ) {
            next.set(bootstrap.id, cached);
            registrations.push(cached.registration);
            continue;
          }
          const built = yield* loadSecondaryConnectionRegistration(bootstrap).pipe(
            Effect.tapError((error) =>
              Effect.logWarning("Could not connect a desktop-local backend.", {
                id: bootstrap.id,
                error,
              }),
            ),
            Effect.option,
          );
          if (Option.isSome(built)) {
            const cacheEntry = { signature, ...built.value };
            next.set(bootstrap.id, cacheEntry);
            registrations.push(built.value.registration);
          } else if (
            cached !== undefined &&
            canRetainCachedPlatformRegistrationAfterRefreshFailure(cached, signature, nowEpochMs)
          ) {
            next.set(bootstrap.id, cached);
            registrations.push(cached.registration);
          }
        }
      }

      yield* Ref.set(cacheRef, next);
      return registrations as ReadonlyArray<PlatformConnectionRegistration>;
    }).pipe(Effect.provide(FetchHttpClient.layer));

    // A change to whether this page withholds its own server rebuilds at once, so a server just
    // confirmed paired is not left out until the next poll.
    const suppressionChanges = Stream.callback<void>((queue) =>
      Effect.acquireRelease(
        Effect.sync(() =>
          onServedPrimarySuppressionChange(() => Queue.offerUnsafe(queue, undefined)),
        ),
        (stop) => Effect.sync(stop),
      ).pipe(Effect.asVoid),
    );
    return PlatformConnectionSource.of({
      registrations: Stream.merge(Stream.tick(PLATFORM_POLL_INTERVAL), suppressionChanges).pipe(
        Stream.mapEffect(() => buildPlatformRegistrations),
      ),
    });
  }),
);

const environmentOwnedDataCleanupLayer = Layer.succeed(
  EnvironmentOwnedDataCleanup,
  EnvironmentOwnedDataCleanup.of({
    clear: (environmentId) =>
      Effect.sync(() => {
        clearComposerDraftsEnvironment(environmentId);
        void forgetPreviewStorage(environmentId);
      }),
  }),
);

const rpcRequestObserverLayer = Layer.succeed(
  EnvironmentRpcRequestObserver,
  EnvironmentRpcRequestObserver.of({
    observe: ({ environmentId, method }) =>
      Effect.sync(() => {
        nextObservedRpcRequestId += 1;
        const requestId = `${environmentId}:${nextObservedRpcRequestId}`;
        trackRpcRequestSent(requestId, { method, environmentId });
        const profiled = clientCollector.observeRequest(environmentId, method);
        return {
          headers:
            profiled.correlationId === null
              ? {}
              : { [PROFILER_CORRELATION_HEADER]: profiled.correlationId },
          complete: Effect.sync(() => {
            acknowledgeRpcRequest(requestId);
            profiled.complete();
          }),
        };
      }),
  }),
);

const environmentSelectionLayer = Layer.succeed(
  EnvironmentSelection,
  makeEnvironmentSelection(appAtomRegistry),
);

type ConnectionPlatformLayerSource =
  | typeof connectionStorageLayer
  | typeof connectivityLayer
  | typeof wakeupsLayer
  | typeof capabilitiesLayer
  | typeof platformConnectionSourceLayer
  | typeof environmentOwnedDataCleanupLayer
  | typeof rpcRequestObserverLayer
  | typeof environmentSelectionLayer;

export const connectionPlatformLayer: Layer.Layer<
  Layer.Success<ConnectionPlatformLayerSource>,
  Layer.Error<ConnectionPlatformLayerSource>,
  Layer.Services<ConnectionPlatformLayerSource>
> = Layer.mergeAll(
  connectionStorageLayer,
  connectivityLayer,
  wakeupsLayer,
  capabilitiesLayer,
  platformConnectionSourceLayer,
  environmentOwnedDataCleanupLayer,
  rpcRequestObserverLayer,
  environmentSelectionLayer,
);
