import {
  type DesktopSshEnvironmentTarget,
  EnvironmentId,
  type OrchestrationShellSnapshot,
  type OrchestrationThread,
  ProjectId,
  ProviderInstanceId,
  ThreadId,
} from "@coddy/contracts";
import { describe, expect, it } from "@effect/vitest";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Equal from "effect/Equal";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Ref from "effect/Ref";
import * as Result from "effect/Result";
import * as Stream from "effect/Stream";
import * as SubscriptionRef from "effect/SubscriptionRef";
import * as TestClock from "effect/testing/TestClock";
import { currentSession } from "../rpc/client.ts";
import { makeTestExecutionEnvironmentCapabilities } from "@coddy/shared/testSupport";

import * as ClientCapabilities from "../platform/capabilities.ts";
import * as TokenStore from "../authorization/tokenStore.ts";
import {
  BearerConnectionCredential,
  BearerConnectionProfile,
  BearerConnectionRegistration,
  connectionRegistrationCatalogEntry,
  type ConnectionRegistration,
  PrimaryConnectionRegistration,
  RelayConnectionRegistration,
  SshConnectionProfile,
  type ConnectionCredential,
  type ConnectionProfile,
  UnavailableConnectionRegistration,
} from "./catalog.ts";
import * as Connectivity from "./connectivity.ts";
import * as ConnectionCredentialStore from "./credentialStore.ts";
import * as ConnectionDriver from "./driver.ts";
import {
  type ConnectionAttemptError,
  ConnectionStorageChangedError,
  ConnectionTransientError,
  BearerConnectionTarget,
  PrimaryConnectionTarget,
  RelayConnectionTarget,
  SshConnectionTarget,
  type ConnectionTarget,
  type PreparedConnection,
  type SupervisorConnectionState,
  UnavailableConnectionTarget,
  isPersistedConnectionTarget,
  withPersistedTargetLabel,
} from "./model.ts";
import * as Persistence from "../platform/persistence.ts";
import * as ConnectionProfileStore from "./profileStore.ts";
import * as EnvironmentRegistry from "./registry.ts";
import * as RpcSession from "../rpc/session.ts";
import * as EnvironmentSupervisor from "./supervisor.ts";
import * as ConnectionWakeups from "./wakeups.ts";

const TARGET = new PrimaryConnectionTarget({
  environmentId: EnvironmentId.make("environment-1"),
  label: "Test environment",
  httpBaseUrl: "https://environment.example.test",
  wsBaseUrl: "wss://environment.example.test",
});
const SECOND_TARGET = new PrimaryConnectionTarget({
  environmentId: EnvironmentId.make("environment-2"),
  label: "Second environment",
  httpBaseUrl: "https://environment-2.example.test",
  wsBaseUrl: "wss://environment-2.example.test",
});

const PREPARED: PreparedConnection = {
  environmentId: TARGET.environmentId,
  label: TARGET.label,
  descriptor: {
    environmentId: TARGET.environmentId,
    label: TARGET.label,
    platform: { os: "linux", arch: "x64" },
    serverVersion: "0.0.0-test",
    storageInstanceId: "store-test",
    bootId: null,
    remoteUpdateSupport: null,
    remoteProtocolVersion: 1,
    minCompatibleRemoteProtocol: 1,
    capabilities: makeTestExecutionEnvironmentCapabilities({ repositoryIdentity: true }),
  },
  httpBaseUrl: TARGET.httpBaseUrl,
  socketUrl: "wss://environment.example.test/ws",
  httpAuthorization: null,
  e2ee: null,
  target: TARGET,
};

const RELAY_TARGET = new RelayConnectionTarget({
  environmentId: EnvironmentId.make("environment-relay"),
  label: "Relay environment",
});
const SECOND_RELAY_TARGET = new RelayConnectionTarget({
  environmentId: EnvironmentId.make("environment-relay-2"),
  label: "Second relay environment",
});

const BEARER_TARGET = new BearerConnectionTarget({
  environmentId: EnvironmentId.make("environment-bearer"),
  label: "Bearer environment",
  connectionId: "bearer-connection",
  serverEnvironmentId: null,
});
const BEARER_PROFILE = new BearerConnectionProfile({
  connectionId: BEARER_TARGET.connectionId,
  environmentId: BEARER_TARGET.environmentId,
  label: BEARER_TARGET.label,
  httpBaseUrl: "https://bearer.example.test",
  wsBaseUrl: "wss://bearer.example.test",
  hostKey: null,
});
const BEARER_CREDENTIAL = new BearerConnectionCredential({
  token: "bearer-token",
});

const SSH_TARGET: DesktopSshEnvironmentTarget = {
  alias: "test",
  hostname: "test.example.test",
  username: "developer",
  port: 22,
};
const SSH_CONNECTION = new SshConnectionTarget({
  environmentId: EnvironmentId.make("environment-ssh"),
  label: "SSH environment",
  connectionId: "ssh-connection",
});
const SSH_PROFILE = new SshConnectionProfile({
  connectionId: SSH_CONNECTION.connectionId,
  environmentId: SSH_CONNECTION.environmentId,
  label: SSH_CONNECTION.label,
  target: SSH_TARGET,
});

const CACHED_SNAPSHOT: OrchestrationShellSnapshot = {
  snapshotSequence: 1,
  projects: [],
  threads: [],
  updatedAt: "2026-06-06T00:00:00.000Z",
};
const CACHED_THREAD: OrchestrationThread = {
  id: ThreadId.make("thread-cached"),
  projectId: ProjectId.make("project-cached"),
  title: "Cached thread",
  modelSelection: {
    instanceId: ProviderInstanceId.make("codex"),
    model: "gpt-5.4",
  },
  runtimeMode: "full-access",
  interactionMode: "default",
  branch: "main",
  worktreePath: null,
  latestTurn: null,
  createdAt: "2026-04-01T00:00:00.000Z",
  updatedAt: "2026-04-01T00:00:00.000Z",
  archivedAt: null,
  deletedAt: null,
  messages: [],
  proposedPlans: [],
  activities: [],
  checkpoints: [],
  session: null,
};

interface SessionControl {
  readonly closed: Deferred.Deferred<never, ConnectionTransientError>;
}

const makeHarness = Effect.fn("TestEnvironmentRegistry.makeHarness")(function* (
  initialTargets: ReadonlyArray<ConnectionTarget>,
  initialProfiles: ReadonlyArray<ConnectionProfile> = [],
  initialCredentials: ReadonlyArray<readonly [string, ConnectionCredential]> = [],
  options?: {
    readonly relabel?: Persistence.ConnectionRegistrationStore["Service"]["relabel"];
    /** Runs when an attempt starts, before the driver reports its first progress. */
    readonly beforePreparing?: (environmentId: EnvironmentId) => Effect.Effect<void>;
    readonly beforeSessionConnect?: (
      environmentId: EnvironmentId,
    ) => Effect.Effect<void, ConnectionAttemptError>;
    readonly sessionReady?: (
      environmentId: EnvironmentId,
    ) => Effect.Effect<void, ConnectionAttemptError>;
    readonly beforeRegistrationRegister?: (
      registration: ConnectionRegistration,
    ) => Effect.Effect<void, Persistence.ConnectionPersistenceError>;
    readonly beforeRegistrationRemove?: (
      target: ConnectionTarget,
    ) => Effect.Effect<void, Persistence.ConnectionPersistenceError>;
    readonly afterStorageIdentityAccept?: (
      identity: Persistence.AcceptedStorageIdentity,
    ) => Effect.Effect<void>;
  },
) {
  const storedTargets = yield* Ref.make(
    new Map(initialTargets.map((target) => [target.environmentId, target])),
  );
  const shellCache = yield* Ref.make(new Map([[TARGET.environmentId, CACHED_SNAPSHOT]]));
  const threadCache = yield* Ref.make(
    new Map([[TARGET.environmentId, new Map([[CACHED_THREAD.id, CACHED_THREAD]])]]),
  );
  const cacheClears = yield* Ref.make<ReadonlyArray<EnvironmentId>>([]);
  const ownedDataClears = yield* Ref.make<ReadonlyArray<EnvironmentId>>([]);
  const sessions = yield* Ref.make<ReadonlyArray<SessionControl>>([]);
  const releasedSessions = yield* Ref.make(0);
  const storedProfiles = yield* Ref.make(
    new Map(initialProfiles.map((profile) => [profile.connectionId, profile])),
  );
  const profileReadCount = yield* Ref.make(0);
  const storedCredentials = yield* Ref.make(new Map(initialCredentials));
  const storedRegistrations = yield* Ref.make<ReadonlyMap<EnvironmentId, ConnectionRegistration>>(
    new Map(),
  );
  const storedRemoteTokens = yield* Ref.make(
    new Map([
      [
        SSH_CONNECTION.environmentId,
        new TokenStore.RemoteDpopAccessToken({
          environmentId: SSH_CONNECTION.environmentId,
          label: SSH_CONNECTION.label,
          endpoint: {
            httpBaseUrl: "https://ssh.example.test",
            wsBaseUrl: "wss://ssh.example.test",
            providerKind: "cloudflare_tunnel",
          },
          accessToken: "cached-token",
          expiresAtEpochMs: Number.MAX_SAFE_INTEGER,
          dpopThumbprint: "thumbprint",
        }),
      ],
    ]),
  );
  const disconnectedSshTargets = yield* Ref.make<ReadonlyArray<DesktopSshEnvironmentTarget>>([]);
  const acceptedStorageIdentities = yield* Ref.make(new Map<string, string>());
  const acceptedStorageIdentityWrites = yield* Ref.make<
    ReadonlyArray<Persistence.AcceptedStorageIdentity>
  >([]);

  const targetStore = Persistence.ConnectionTargetStore.of({
    list: Ref.get(storedTargets).pipe(Effect.map((targets) => [...targets.values()])),
  });
  const removeStoredRegistration = Effect.fn("TestEnvironmentRegistry.removeRegistration")(
    function* (target: ConnectionTarget) {
      yield* options?.beforeRegistrationRemove?.(target) ?? Effect.void;
      yield* Ref.update(storedTargets, (current) => {
        const next = new Map(current);
        next.delete(target.environmentId);
        return next;
      });
      yield* Ref.update(storedRegistrations, (current) => {
        const next = new Map(current);
        next.delete(target.environmentId);
        return next;
      });
      if (target._tag === "BearerConnectionTarget" || target._tag === "SshConnectionTarget") {
        yield* Ref.update(storedProfiles, (current) => {
          const next = new Map(current);
          next.delete(target.connectionId);
          return next;
        });
        yield* Ref.update(storedCredentials, (current) => {
          const next = new Map(current);
          next.delete(target.connectionId);
          return next;
        });
      }
      yield* Ref.update(storedRemoteTokens, (current) => {
        const next = new Map(current);
        next.delete(target.environmentId);
        return next;
      });
    },
  );
  const registrationStore = Persistence.ConnectionRegistrationStore.of({
    register: (registration) =>
      Effect.gen(function* () {
        yield* options?.beforeRegistrationRegister?.(registration) ?? Effect.void;
        yield* Ref.update(storedTargets, (current) => {
          const next = new Map(current);
          next.set(registration.target.environmentId, registration.target);
          return next;
        });
        yield* Ref.update(storedRegistrations, (current) => {
          const next = new Map(current);
          next.set(registration.target.environmentId, registration);
          return next;
        });
        switch (registration._tag) {
          case "RelayConnectionRegistration":
            return;
          case "BearerConnectionRegistration":
            yield* Ref.update(storedProfiles, (current) => {
              const next = new Map(current);
              next.set(registration.profile.connectionId, registration.profile);
              return next;
            });
            yield* Ref.update(storedCredentials, (current) => {
              const next = new Map(current);
              next.set(registration.target.connectionId, registration.credential);
              return next;
            });
            return;
          case "SshConnectionRegistration":
            yield* Ref.update(storedProfiles, (current) => {
              const next = new Map(current);
              next.set(registration.profile.connectionId, registration.profile);
              return next;
            });
        }
      }),
    removeIfMatching: (registration) =>
      Effect.gen(function* () {
        const stored = (yield* Ref.get(storedRegistrations)).get(registration.target.environmentId);
        if (stored === undefined || !Equal.equals(stored, registration)) {
          return {
            removed: false,
            current: stored === undefined ? null : connectionRegistrationCatalogEntry(stored),
          };
        }
        yield* removeStoredRegistration(registration.target);
        return { removed: true, current: null };
      }),
    remove: removeStoredRegistration,
    relabel:
      options?.relabel ??
      ((environmentId, label) =>
        Effect.gen(function* () {
          const current = (yield* Ref.get(storedTargets)).get(environmentId);
          if (current === undefined || !isPersistedConnectionTarget(current)) {
            return Option.none();
          }
          const relabeled = withPersistedTargetLabel(current, label);
          yield* Ref.update(storedTargets, (targets) => {
            const next = new Map(targets);
            next.set(environmentId, relabeled);
            return next;
          });
          return Option.some(relabeled);
        })),
  });
  const externalRegistrationStore = Persistence.ConnectionRegistrationStore.of({
    ...registrationStore,
  });
  const acceptedStorageIdentityService = {
    get: (targetKey: string) =>
      Ref.get(acceptedStorageIdentities).pipe(
        Effect.map((current) => Option.fromUndefinedOr(current.get(targetKey))),
      ),
    accept: (identity: Persistence.AcceptedStorageIdentity) =>
      Ref.update(acceptedStorageIdentities, (current) => {
        const next = new Map(current);
        next.set(identity.targetKey, identity.storageInstanceId);
        return next;
      }).pipe(
        Effect.andThen(
          Ref.update(acceptedStorageIdentityWrites, (current) => [...current, identity]),
        ),
        Effect.andThen(options?.afterStorageIdentityAccept?.(identity) ?? Effect.void),
      ),
    rollbackAcceptance: (
      identity: Persistence.AcceptedStorageIdentity,
      previousStorageInstanceId: string | null,
    ) =>
      Ref.modify(acceptedStorageIdentities, (current) => {
        if (current.get(identity.targetKey) !== identity.storageInstanceId) {
          return [false, current] as const;
        }
        const next = new Map(current);
        if (previousStorageInstanceId === null) next.delete(identity.targetKey);
        else next.set(identity.targetKey, previousStorageInstanceId);
        return [true, next] as const;
      }),
    transition: <A>(
      targetKey: string,
      decide: (acceptedStorageInstanceId: string | null) => {
        readonly result: A;
        readonly mutation:
          | { readonly _tag: "Keep" }
          | { readonly _tag: "Set"; readonly storageInstanceId: string };
      },
    ) =>
      Ref.modify(
        acceptedStorageIdentities,
        (
          current,
        ): readonly [
          {
            readonly result: A;
            readonly identity: Persistence.AcceptedStorageIdentity | null;
          },
          Map<string, string>,
        ] => {
          const transition = decide(current.get(targetKey) ?? null);
          if (transition.mutation._tag === "Keep") {
            return [{ result: transition.result, identity: null }, current] as const;
          }
          const identity = {
            targetKey,
            storageInstanceId: transition.mutation.storageInstanceId,
          };
          const next = new Map(current);
          next.set(targetKey, identity.storageInstanceId);
          return [{ result: transition.result, identity }, next] as const;
        },
      ).pipe(
        Effect.flatMap(({ result, identity }) =>
          identity === null
            ? Effect.succeed(result)
            : Ref.update(acceptedStorageIdentityWrites, (current) => [...current, identity]).pipe(
                Effect.andThen(options?.afterStorageIdentityAccept?.(identity) ?? Effect.void),
                Effect.as(result),
              ),
        ),
      ),
  };
  const acceptedStorageIdentityStore = Persistence.AcceptedStorageIdentityStore.of(
    acceptedStorageIdentityService,
  );
  const cacheStore = Persistence.EnvironmentCacheStore.of({
    loadShell: (environmentId) =>
      Ref.get(shellCache).pipe(
        Effect.map((cache) => Option.fromUndefinedOr(cache.get(environmentId))),
      ),
    saveShell: (environmentId, snapshot) =>
      Ref.update(shellCache, (current) => {
        const next = new Map(current);
        next.set(environmentId, snapshot);
        return next;
      }),
    loadThread: (environmentId, threadId) =>
      Ref.get(threadCache).pipe(
        Effect.map((cache) => Option.fromUndefinedOr(cache.get(environmentId)?.get(threadId))),
      ),
    saveThread: (environmentId, thread) =>
      Ref.update(threadCache, (current) => {
        const next = new Map(current);
        const threads = new Map(next.get(environmentId));
        threads.set(thread.id, thread);
        next.set(environmentId, threads);
        return next;
      }),
    removeThread: (environmentId, threadId) =>
      Ref.update(threadCache, (current) => {
        const next = new Map(current);
        const threads = new Map(next.get(environmentId));
        threads.delete(threadId);
        next.set(environmentId, threads);
        return next;
      }),
    clear: (environmentId) =>
      Ref.update(shellCache, (current) => {
        const next = new Map(current);
        next.delete(environmentId);
        return next;
      }).pipe(
        Effect.andThen(
          Ref.update(threadCache, (current) => {
            const next = new Map(current);
            next.delete(environmentId);
            return next;
          }),
        ),
        Effect.andThen(
          Ref.update(cacheClears, (environmentIds) => [...environmentIds, environmentId]),
        ),
      ),
  });
  const ownedDataCleanup = Persistence.EnvironmentOwnedDataCleanup.of({
    clear: (environmentId) =>
      Ref.update(ownedDataClears, (environmentIds) => [...environmentIds, environmentId]),
  });
  const networkStatus = yield* SubscriptionRef.make<"unknown" | "offline" | "online">("online");
  const connectivity = Connectivity.Connectivity.of({
    status: SubscriptionRef.get(networkStatus),
    changes: SubscriptionRef.changes(networkStatus),
  });
  const profileStore = ConnectionProfileStore.ConnectionProfileStore.of({
    get: (connectionId) =>
      Ref.update(profileReadCount, (count) => count + 1).pipe(
        Effect.andThen(Ref.get(storedProfiles)),
        Effect.map((current) => Option.fromUndefinedOr(current.get(connectionId))),
      ),
    put: (profile) =>
      Ref.update(storedProfiles, (current) => {
        const next = new Map(current);
        next.set(profile.connectionId, profile);
        return next;
      }),
    remove: (connectionId) =>
      Ref.update(storedProfiles, (current) => {
        const next = new Map(current);
        next.delete(connectionId);
        return next;
      }),
  });
  const credentialStore = ConnectionCredentialStore.ConnectionCredentialStore.of({
    get: (connectionId) =>
      Ref.get(storedCredentials).pipe(
        Effect.map((current) => Option.fromUndefinedOr(current.get(connectionId))),
      ),
    put: (connectionId, credential) =>
      Ref.update(storedCredentials, (current) => {
        const next = new Map(current);
        next.set(connectionId, credential);
        return next;
      }),
    remove: (connectionId) =>
      Ref.update(storedCredentials, (current) => {
        const next = new Map(current);
        next.delete(connectionId);
        return next;
      }),
    putIfSaved: () => Effect.die(new Error("SSH credential refresh is not used.")),
  });
  const tokenStore = TokenStore.RemoteDpopAccessTokenStore.of({
    get: (environmentId) =>
      Ref.get(storedRemoteTokens).pipe(
        Effect.map((current) => Option.fromUndefinedOr(current.get(environmentId))),
      ),
    put: (token) =>
      Ref.update(storedRemoteTokens, (current) => {
        const next = new Map(current);
        next.set(token.environmentId, token);
        return next;
      }),
    remove: (environmentId) =>
      Ref.update(storedRemoteTokens, (current) => {
        const next = new Map(current);
        next.delete(environmentId);
        return next;
      }),
  });
  const sshGateway = ClientCapabilities.SshEnvironmentGateway.of({
    provision: () => Effect.die(new Error("SSH provisioning is not used.")),
    ensureTunnel: () => Effect.die(new Error("SSH tunnels are not used.")),
    mintBearer: () => Effect.die(new Error("SSH minting is not used.")),
    disconnect: (target) => Ref.update(disconnectedSshTargets, (current) => [...current, target]),
  });
  const driver = ConnectionDriver.ConnectionDriver.of({
    connect: (entry, reportProgress) =>
      Effect.gen(function* () {
        const target = entry.target;
        yield* options?.beforePreparing?.(target.environmentId) ?? Effect.void;
        if (target._tag === "UnavailableConnectionTarget") {
          return yield* new ConnectionTransientError({
            reason: "endpoint-unavailable",
            detail: target.detail,
          });
        }
        const prepared = {
          ...PREPARED,
          environmentId: target.environmentId,
          label: target.label,
          target,
        };
        yield* reportProgress({ stage: "preparing" });
        yield* reportProgress({ stage: "opening", prepared });
        yield* options?.beforeSessionConnect?.(target.environmentId) ?? Effect.void;
        const closed = yield* Deferred.make<never, ConnectionTransientError>();
        yield* Ref.update(sessions, (current) => [...current, { closed }]);
        const session = yield* Effect.acquireRelease(
          Effect.succeed({
            client: {} as RpcSession.RpcSession["client"],
            initialConfig: Effect.die(new Error("Config is not used by registry tests.")),
            ready: options?.sessionReady?.(target.environmentId) ?? Effect.void,
            probe: Effect.void,
            closed: Deferred.await(closed),
            e2eeAuthenticated: Effect.succeed(null),
          } satisfies RpcSession.RpcSession),
          () => Ref.update(releasedSessions, (count) => count + 1),
        );
        yield* reportProgress({ stage: "synchronizing", prepared });
        yield* session.ready;
        return { prepared, session };
      }),
  });

  const cacheLayer = Layer.succeed(Persistence.EnvironmentCacheStore, cacheStore);
  const layer = EnvironmentRegistry.layer.pipe(
    Layer.provide(
      Layer.mergeAll(
        Layer.succeed(Persistence.ConnectionTargetStore, targetStore),
        Layer.succeed(Persistence.ConnectionRegistrationStore, registrationStore),
        Layer.succeed(Persistence.AcceptedStorageIdentityStore, acceptedStorageIdentityStore),
        Layer.succeed(ConnectionProfileStore.ConnectionProfileStore, profileStore),
        Layer.succeed(ConnectionCredentialStore.ConnectionCredentialStore, credentialStore),
        Layer.succeed(TokenStore.RemoteDpopAccessTokenStore, tokenStore),
        Layer.succeed(ClientCapabilities.SshEnvironmentGateway, sshGateway),
        Layer.succeed(Connectivity.Connectivity, connectivity),
        Layer.succeed(
          ConnectionWakeups.ConnectionWakeups,
          ConnectionWakeups.ConnectionWakeups.of({
            focusVisibility: Stream.never,
            changes: Stream.never,
          }),
        ),
        Layer.succeed(ConnectionDriver.ConnectionDriver, driver),
        cacheLayer,
        Layer.succeed(Persistence.EnvironmentOwnedDataCleanup, ownedDataCleanup),
      ),
    ),
  );

  return {
    layer,
    externalRegistrationStore,
    storedTargets,
    shellCache,
    threadCache,
    cacheClears,
    ownedDataClears,
    sessions,
    releasedSessions,
    storedProfiles,
    profileReadCount,
    storedCredentials,
    storedRemoteTokens,
    disconnectedSshTargets,
    acceptedStorageIdentities,
    acceptedStorageIdentityWrites,
    networkStatus,
  };
});

/** The state a desired supervisor is built with: generation 0, before any attempt reports. */
const FIRST_DESIRED_STATE = { desired: true, phase: "connecting", generation: 0 } as const;

/**
 * Samples `registry.state` whenever an attempt starts, before the driver reports any progress.
 * The supervisor's loop has published nothing by then, so every sample is the state the registry
 * built the supervisor with. Hand the registry to `watch` before the first attempt can start.
 */
const makeFirstStateProbe = Effect.fn("TestEnvironmentRegistry.makeFirstStateProbe")(function* () {
  const registryRef = yield* Ref.make(
    Option.none<EnvironmentRegistry.EnvironmentRegistry["Service"]>(),
  );
  const samples = yield* Ref.make<
    ReadonlyArray<readonly [EnvironmentId, SupervisorConnectionState]>
  >([]);
  const beforePreparing = Effect.fn("TestEnvironmentRegistry.beforePreparing")(function* (
    environmentId: EnvironmentId,
  ) {
    const registry = Option.getOrThrow(yield* Ref.get(registryRef));
    const state = yield* registry.state(environmentId);
    yield* Ref.update(samples, (current) => [...current, [environmentId, state] as const]);
  }, Effect.orDie);
  return {
    beforePreparing,
    samples,
    watch: (registry: EnvironmentRegistry.EnvironmentRegistry["Service"]) =>
      Ref.set(registryRef, Option.some(registry)),
  };
});

function awaitConnectionState(
  registry: EnvironmentRegistry.EnvironmentRegistry["Service"],
  environmentId: EnvironmentId,
  predicate: (state: SupervisorConnectionState) => boolean,
) {
  return Effect.gen(function* () {
    const current = yield* registry.state(environmentId);
    if (predicate(current)) {
      return current;
    }
    return yield* registry
      .stateChanges(environmentId)
      .pipe(Stream.filter(predicate), Stream.runHead, Effect.map(Option.getOrThrow));
  });
}

describe("EnvironmentRegistry", () => {
  it.effect("hydrates connection profiles into catalog entries", () =>
    Effect.gen(function* () {
      const aliasedTarget = new BearerConnectionTarget({
        ...BEARER_TARGET,
        label: "Linux workstation",
      });
      const aliasedProfile = new BearerConnectionProfile({
        ...BEARER_PROFILE,
        label: "Linux workstation",
      });
      const harness = yield* makeHarness(
        [SSH_CONNECTION, aliasedTarget],
        [SSH_PROFILE, aliasedProfile],
      );

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        const entry = (yield* SubscriptionRef.get(registry.entries)).get(
          SSH_CONNECTION.environmentId,
        );

        expect(entry?.target).toEqual(SSH_CONNECTION);
        expect(Option.getOrThrow(entry?.profile ?? Option.none())).toEqual(SSH_PROFILE);
        const aliasedEntry = (yield* SubscriptionRef.get(registry.entries)).get(
          BEARER_TARGET.environmentId,
        );
        expect(aliasedEntry?.target.label).toBe("Linux workstation");
        expect(Option.getOrThrow(aliasedEntry?.profile ?? Option.none()).label).toBe(
          "Linux workstation",
        );
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("publishes network status changes independently of connection state", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness([]);

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        const offline = yield* Effect.forkChild(
          SubscriptionRef.changes(registry.networkStatus).pipe(
            Stream.filter((status) => status === "offline"),
            Stream.runHead,
            Effect.map(Option.getOrThrow),
          ),
        );

        yield* SubscriptionRef.set(harness.networkStatus, "offline");

        expect(yield* Fiber.join(offline)).toBe("offline");
        expect(yield* SubscriptionRef.get(registry.networkStatus)).toBe("offline");
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.live("starts persisted environments independently", () =>
    Effect.gen(function* () {
      const bothLoadsStarted = yield* Deferred.make<void>();
      const releaseLoads = yield* Deferred.make<void>();
      const loadCount = yield* Ref.make(0);
      const harness = yield* makeHarness([TARGET, SECOND_TARGET], [], [], {
        beforeSessionConnect: () =>
          Ref.updateAndGet(loadCount, (count) => count + 1).pipe(
            Effect.tap((count) =>
              count === 2 ? Deferred.succeed(bothLoadsStarted, undefined) : Effect.void,
            ),
            Effect.andThen(Deferred.await(releaseLoads)),
          ),
      });

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        const start = yield* Effect.forkChild(registry.start);

        yield* Deferred.await(bothLoadsStarted).pipe(Effect.timeout("1 second"));
        yield* Deferred.succeed(releaseLoads, undefined);
        yield* Fiber.join(start);

        expect(yield* Ref.get(loadCount)).toBe(2);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("exposes the current RPC generation to late query subscribers", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness([TARGET]);
      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.start;
        yield* awaitConnectionState(
          registry,
          TARGET.environmentId,
          (state) => state.phase === "connected",
        );

        const generation = yield* registry
          .runStream(
            TARGET.environmentId,
            Stream.unwrap(
              EnvironmentSupervisor.EnvironmentSupervisor.pipe(
                Effect.map((supervisor) =>
                  Stream.concat(
                    Stream.fromEffect(SubscriptionRef.get(supervisor.state)),
                    SubscriptionRef.changes(supervisor.state),
                  ).pipe(
                    Stream.filterMap((state) =>
                      state.phase === "connected"
                        ? Result.succeed(state.generation)
                        : Result.failVoid,
                    ),
                    Stream.changes,
                  ),
                ),
              ),
            ),
          )
          .pipe(Stream.runHead, Effect.map(Option.getOrThrow));

        expect(generation).toBe(1);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("preserves cached data on connection failure and clears it on explicit removal", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness([TARGET]);
      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.start;
        yield* awaitConnectionState(
          registry,
          TARGET.environmentId,
          (state) => state.phase === "connected",
        );
        const controls = yield* Ref.get(harness.sessions);
        expect(controls).toHaveLength(1);
        const active = controls[0];
        expect(active).toBeDefined();
        expect((yield* Ref.get(harness.shellCache)).get(TARGET.environmentId)).toEqual(
          CACHED_SNAPSHOT,
        );

        const retryFiber = yield* Effect.forkChild(
          awaitConnectionState(
            registry,
            TARGET.environmentId,
            (state) => state.phase === "backoff",
          ),
        );
        yield* Effect.yieldNow;
        yield* Deferred.fail(
          active!.closed,
          new ConnectionTransientError({
            reason: "transport",
            detail: "Disconnected.",
          }),
        );
        yield* Fiber.join(retryFiber);
        expect((yield* Ref.get(harness.shellCache)).get(TARGET.environmentId)).toEqual(
          CACHED_SNAPSHOT,
        );

        yield* registry.remove(TARGET.environmentId);
        expect((yield* Ref.get(harness.storedTargets)).has(TARGET.environmentId)).toBe(false);
        expect((yield* Ref.get(harness.shellCache)).has(TARGET.environmentId)).toBe(false);
        expect(yield* Ref.get(harness.cacheClears)).toEqual([TARGET.environmentId]);
        expect((yield* SubscriptionRef.get(registry.entries)).has(TARGET.environmentId)).toBe(
          false,
        );
      }).pipe(Effect.provide(harness.layer));
    }),
  );

  it.effect("persists and starts a newly registered environment", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness([]);

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.register(new RelayConnectionRegistration({ target: RELAY_TARGET }));
        yield* awaitConnectionState(
          registry,
          RELAY_TARGET.environmentId,
          (state) => state.phase === "connected",
        );

        expect((yield* Ref.get(harness.storedTargets)).get(RELAY_TARGET.environmentId)).toEqual(
          RELAY_TARGET,
        );
        expect(yield* Ref.get(harness.sessions)).toHaveLength(1);
      }).pipe(Effect.provide(harness.layer));
    }),
  );

  it.live("moves durable streams to a replacement supervisor", () =>
    Effect.gen(function* () {
      const replacement = new RelayConnectionTarget({
        environmentId: RELAY_TARGET.environmentId,
        label: "Replacement relay environment",
      });
      const harness = yield* makeHarness([RELAY_TARGET]);

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        const firstObserved = yield* Deferred.make<void>();
        const secondObserved = yield* Deferred.make<void>();
        const labels = yield* Ref.make<ReadonlyArray<string>>([]);
        yield* registry.start;
        yield* awaitConnectionState(
          registry,
          RELAY_TARGET.environmentId,
          (state) => state.phase === "connected",
        );

        const subscription = yield* Effect.forkChild(
          registry
            .followStream(
              RELAY_TARGET.environmentId,
              Stream.unwrap(
                EnvironmentSupervisor.EnvironmentSupervisor.pipe(
                  Effect.map((supervisor) =>
                    Stream.concat(Stream.succeed(supervisor.target.label), Stream.never),
                  ),
                ),
              ),
            )
            .pipe(
              Stream.tap((label) =>
                Ref.updateAndGet(labels, (current) => [...current, label]).pipe(
                  Effect.flatMap((current) =>
                    current.length === 1
                      ? Deferred.succeed(firstObserved, undefined)
                      : Deferred.succeed(secondObserved, undefined),
                  ),
                ),
              ),
              Stream.runDrain,
            ),
        );

        yield* Deferred.await(firstObserved).pipe(Effect.timeout("1 second"));
        yield* registry.register(new RelayConnectionRegistration({ target: replacement }));
        yield* Deferred.await(secondObserved).pipe(Effect.timeout("1 second"));
        yield* Fiber.interrupt(subscription);

        expect(yield* Ref.get(labels)).toEqual([RELAY_TARGET.label, replacement.label]);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.live("moves durable streams to the runtime of an identical re-registration", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness([RELAY_TARGET]);

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        const environmentId = RELAY_TARGET.environmentId;
        const firstObserved = yield* Deferred.make<void>();
        const secondObserved = yield* Deferred.make<void>();
        const observed = yield* Ref.make<
          ReadonlyArray<EnvironmentSupervisor.EnvironmentSupervisor["Service"]>
        >([]);
        yield* registry.start;
        yield* awaitConnectionState(
          registry,
          environmentId,
          (state) => state.phase === "connected",
        );

        const subscription = yield* Effect.forkChild(
          registry
            .followStream(
              environmentId,
              Stream.unwrap(
                EnvironmentSupervisor.EnvironmentSupervisor.pipe(
                  Effect.map((supervisor) =>
                    Stream.concat(Stream.succeed(supervisor), Stream.never),
                  ),
                ),
              ),
            )
            .pipe(
              Stream.tap((supervisor) =>
                Ref.updateAndGet(observed, (current) => [...current, supervisor]).pipe(
                  Effect.flatMap((current) =>
                    Deferred.succeed(
                      current.length === 1 ? firstObserved : secondObserved,
                      undefined,
                    ),
                  ),
                ),
              ),
              Stream.runDrain,
            ),
        );

        yield* Deferred.await(firstObserved).pipe(Effect.timeout("1 second"));
        // Registration always installs a fresh runtime, even for an identical entry.
        yield* registry.register(new RelayConnectionRegistration({ target: RELAY_TARGET }));
        yield* Deferred.await(secondObserved).pipe(Effect.timeout("1 second"));
        const current = yield* registry.run(
          environmentId,
          EnvironmentSupervisor.EnvironmentSupervisor.pipe(Effect.map((supervisor) => supervisor)),
        );
        yield* Fiber.interrupt(subscription);

        const [first, second] = yield* Ref.get(observed);
        expect(second).not.toBe(first);
        expect(second).toBe(current);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.live("moves followers when acquisition replaces a supervisor after catalog drift", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness([RELAY_TARGET]);
      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        const id = RELAY_TARGET.environmentId;
        const first = yield* Deferred.make<void>();
        const second = yield* Deferred.make<void>();
        const observed = yield* Ref.make<
          ReadonlyArray<EnvironmentSupervisor.EnvironmentSupervisor["Service"]>
        >([]);
        yield* registry.start;
        yield* registry
          .followStream(
            id,
            Stream.unwrap(
              EnvironmentSupervisor.EnvironmentSupervisor.pipe(
                Effect.map((supervisor) => Stream.concat(Stream.succeed(supervisor), Stream.never)),
              ),
            ),
          )
          .pipe(
            Stream.runForEach((supervisor) =>
              Ref.updateAndGet(observed, (values) => [...values, supervisor]).pipe(
                Effect.flatMap((values) =>
                  Deferred.succeed(values.length === 1 ? first : second, undefined),
                ),
              ),
            ),
            Effect.forkChild,
          );
        yield* Deferred.await(first).pipe(Effect.timeout("1 second"));
        yield* SubscriptionRef.update(registry.entries, (entries) =>
          new Map(entries).set(id, {
            target: new RelayConnectionTarget({ ...RELAY_TARGET, label: "Changed externally" }),
            profile: Option.none(),
          }),
        );
        const current = yield* registry.run(id, EnvironmentSupervisor.EnvironmentSupervisor);
        yield* Deferred.await(second).pipe(Effect.timeout("1 second"));
        const values = yield* Ref.get(observed);
        expect(values).toHaveLength(2);
        expect(values[1]).toBe(current);
        expect(values[0]).not.toBe(current);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.live(
    "releases its subscription on removal and re-binds when the same id is registered again",
    () =>
      Effect.gen(function* () {
        const harness = yield* makeHarness([]);
        yield* Effect.gen(function* () {
          const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
          const subscribed = yield* Deferred.make<void>();
          const rebound = yield* Deferred.make<void>();
          const released = yield* Deferred.make<void>();
          const observed = yield* Ref.make<
            ReadonlyArray<EnvironmentSupervisor.EnvironmentSupervisor["Service"]>
          >([]);
          const follower = yield* registry
            .followStream(
              RELAY_TARGET.environmentId,
              Stream.fromEffect(EnvironmentSupervisor.EnvironmentSupervisor).pipe(
                Stream.concat(Stream.never),
                Stream.ensuring(Deferred.succeed(released, undefined)),
              ),
            )
            .pipe(
              Stream.runForEach((supervisor) =>
                Ref.updateAndGet(observed, (values) => [...values, supervisor]).pipe(
                  Effect.flatMap((values) =>
                    Deferred.succeed(values.length === 1 ? subscribed : rebound, undefined),
                  ),
                ),
              ),
              Effect.forkChild({ startImmediately: true }),
            );
          expect(yield* Ref.get(observed)).toEqual([]);
          yield* registry.register(new RelayConnectionRegistration({ target: RELAY_TARGET }));
          yield* Deferred.await(subscribed).pipe(Effect.timeout("1 second"));
          yield* registry.remove(RELAY_TARGET.environmentId);
          yield* Deferred.await(released).pipe(Effect.timeout("1 second"));
          yield* registry.register(new RelayConnectionRegistration({ target: RELAY_TARGET }));
          yield* Deferred.await(rebound).pipe(Effect.timeout("1 second"));
          const current = yield* registry.run(
            RELAY_TARGET.environmentId,
            EnvironmentSupervisor.EnvironmentSupervisor,
          );
          const values = yield* Ref.get(observed);
          expect(values).toHaveLength(2);
          expect(values[0]).not.toBe(current);
          expect(values[1]).toBe(current);
          yield* Fiber.interrupt(follower);
        }).pipe(Effect.provide(harness.layer), Effect.scoped);
      }),
  );

  for (const outcome of ["failure", "missing"] as const) {
    it.effect(`keeps runtime and persisted metadata when relabel returns ${outcome}`, () =>
      Effect.gen(function* () {
        const removedTargets = yield* Ref.make<ReadonlyArray<ConnectionTarget>>([]);
        const harness = yield* makeHarness([RELAY_TARGET], [], [], {
          relabel: () =>
            outcome === "missing"
              ? Effect.succeedNone
              : Effect.fail(
                  new Persistence.ConnectionPersistenceError({
                    operation: "rename-connection",
                    message: "Catalog unavailable",
                  }),
                ),
          beforeRegistrationRemove: (target) =>
            Ref.update(removedTargets, (targets) => [...targets, target]),
        });
        yield* Effect.gen(function* () {
          const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
          const id = RELAY_TARGET.environmentId;
          const before = yield* registry.run(id, EnvironmentSupervisor.EnvironmentSupervisor);
          const entries = yield* SubscriptionRef.get(registry.entries);
          const saved = yield* Ref.get(harness.storedTargets);
          const error = yield* registry.rename(id, "Changed").pipe(Effect.flip);
          expect(error._tag).toBe(
            outcome === "missing" ? "EnvironmentNotRegisteredError" : "ConnectionPersistenceError",
          );
          expect(yield* SubscriptionRef.get(registry.entries)).toBe(entries);
          expect(yield* Ref.get(harness.storedTargets)).toBe(saved);
          expect(yield* registry.run(id, EnvironmentSupervisor.EnvironmentSupervisor)).toBe(before);
          expect(before.target).toBe(RELAY_TARGET);
          // Platform shadowing consumes the registry's persisted-target bookkeeping.
          yield* registry.registerPlatform(
            new PrimaryConnectionRegistration({
              target: new PrimaryConnectionTarget({
                ...TARGET,
                environmentId: id,
              }),
            }),
          );
          expect(yield* Ref.get(removedTargets)).toEqual([RELAY_TARGET]);
        }).pipe(Effect.provide(harness.layer), Effect.scoped);
      }),
    );
  }

  it.live("renames a saved environment without replacing its runtime or followers", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness(
        [BEARER_TARGET],
        [BEARER_PROFILE],
        [[BEARER_TARGET.connectionId, BEARER_CREDENTIAL]],
      );

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        const environmentId = BEARER_TARGET.environmentId;
        const subscribed = yield* Deferred.make<void>();
        const subscriptions = yield* Ref.make(0);
        yield* registry.start;
        yield* awaitConnectionState(
          registry,
          environmentId,
          (state) => state.phase === "connected",
        );

        const follower = yield* Effect.forkChild(
          registry
            .followStream(
              environmentId,
              Stream.unwrap(
                EnvironmentSupervisor.EnvironmentSupervisor.pipe(
                  Effect.map(() => Stream.concat(Stream.succeed("subscribed"), Stream.never)),
                ),
              ),
            )
            .pipe(
              Stream.tap(() =>
                Ref.update(subscriptions, (count) => count + 1).pipe(
                  Effect.andThen(Deferred.succeed(subscribed, undefined)),
                ),
              ),
              Stream.runDrain,
            ),
        );
        yield* Deferred.await(subscribed).pipe(Effect.timeout("1 second"));

        yield* registry.rename(environmentId, "  GPU box  ");

        const entry = (yield* SubscriptionRef.get(registry.entries)).get(environmentId);
        expect(entry?.target.label).toBe("GPU box");
        expect(entry?.profile).toEqual(Option.some(BEARER_PROFILE));
        expect((yield* Ref.get(harness.storedTargets)).get(environmentId)?.label).toBe("GPU box");
        expect((yield* Ref.get(harness.storedProfiles)).get(BEARER_TARGET.connectionId)).toEqual(
          BEARER_PROFILE,
        );
        expect((yield* Ref.get(harness.storedCredentials)).get(BEARER_TARGET.connectionId)).toEqual(
          BEARER_CREDENTIAL,
        );

        // Later lookups keep the live runtime: no reconnect, no resubscription.
        const state = yield* registry.state(environmentId);
        for (let iteration = 0; iteration < 50; iteration += 1) {
          yield* Effect.yieldNow;
        }
        yield* Fiber.interrupt(follower);
        expect(state.phase).toBe("connected");
        expect(state.desired).toBe(true);
        expect(yield* Ref.get(subscriptions)).toBe(1);
        expect(yield* Ref.get(harness.sessions)).toHaveLength(1);
        expect(yield* Ref.get(harness.releasedSessions)).toBe(0);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("uses the renamed label for supervisor failures and unavailable RPCs", () =>
    Effect.gen(function* () {
      const connecting = yield* Deferred.make<void>();
      const harness = yield* makeHarness([RELAY_TARGET], [], [], {
        beforeSessionConnect: () =>
          Deferred.succeed(connecting, undefined).pipe(Effect.andThen(Effect.never)),
      });
      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        const environmentId = RELAY_TARGET.environmentId;
        yield* registry.start;
        yield* Deferred.await(connecting);
        const before = yield* registry.run(
          environmentId,
          EnvironmentSupervisor.EnvironmentSupervisor,
        );
        yield* registry.rename(environmentId, "GPU box");
        const after = yield* registry.run(
          environmentId,
          EnvironmentSupervisor.EnvironmentSupervisor,
        );
        expect(after).toBe(before);
        const unavailable = yield* registry.run(environmentId, currentSession()).pipe(Effect.flip);
        expect(unavailable.message).toBe("GPU box is not connected.");
        yield* TestClock.adjust(EnvironmentSupervisor.CONNECTION_ESTABLISHMENT_TIMEOUT);
        const state = yield* awaitConnectionState(
          registry,
          environmentId,
          (value) => value.phase === "backoff",
        );
        expect(state.lastFailure?.detail).toBe("GPU box did not respond during connection setup.");
        expect(yield* Ref.get(harness.sessions)).toEqual([]);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("names a disconnect during readiness with the saved name after a rename", () =>
    Effect.gen(function* () {
      const connecting = yield* Deferred.make<void>();
      const renamed = yield* Deferred.make<void>();
      const harness = yield* makeHarness([RELAY_TARGET], [], [], {
        beforeSessionConnect: () =>
          Deferred.succeed(connecting, undefined).pipe(Effect.andThen(Deferred.await(renamed))),
        sessionReady: () =>
          Effect.fail(
            new ConnectionTransientError({
              reason: "connection-closed",
              detail: "The connection disconnected.",
            }),
          ),
      });
      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        const environmentId = RELAY_TARGET.environmentId;
        yield* registry.start;
        yield* Deferred.await(connecting);
        yield* registry.rename(environmentId, "GPU box");
        yield* Deferred.succeed(renamed, undefined);
        const state = yield* awaitConnectionState(
          registry,
          environmentId,
          (value) => value.phase === "backoff",
        );
        expect(state.lastFailure?.message).toBe("GPU box closed the connection.");
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  for (const [reason, expected] of [
    ["connection-closed", "GPU box closed the connection."],
    ["connection-lost", "The connection to GPU box was lost."],
    [
      "liveness-timeout",
      "No data from GPU box for 30 seconds. The connection is too slow or was lost.",
    ],
  ] as const) {
    it.effect(`uses the saved name after rename for ${reason}`, () =>
      Effect.gen(function* () {
        const harness = yield* makeHarness([RELAY_TARGET]);
        yield* Effect.gen(function* () {
          const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
          const environmentId = RELAY_TARGET.environmentId;
          yield* registry.start;
          yield* awaitConnectionState(
            registry,
            environmentId,
            (state) => state.phase === "connected",
          );
          const supervisor = yield* registry.run(
            environmentId,
            EnvironmentSupervisor.EnvironmentSupervisor,
          );
          const prepared = yield* SubscriptionRef.get(supervisor.prepared);
          yield* registry.rename(environmentId, "GPU box");
          expect(yield* SubscriptionRef.get(supervisor.prepared)).toBe(prepared);
          expect(Option.getOrThrow(prepared).label).not.toBe("GPU box");
          const session = (yield* Ref.get(harness.sessions)).at(-1);
          if (session === undefined) throw new Error("Missing live session");
          yield* Deferred.fail(
            session.closed,
            new ConnectionTransientError({
              reason,
              detail: "The connection disconnected.",
            }),
          );
          const state = yield* awaitConnectionState(
            registry,
            environmentId,
            (value) => value.phase === "backoff",
          );
          expect(state.lastFailure?.message).toBe(expected);
        }).pipe(Effect.provide(harness.layer), Effect.scoped);
      }),
    );
  }

  it.effect("keeps a disconnected environment disconnected across a rename", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness([RELAY_TARGET]);

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.disconnect(RELAY_TARGET.environmentId);

        yield* registry.rename(RELAY_TARGET.environmentId, "Cloud box");

        const state = yield* registry.state(RELAY_TARGET.environmentId);
        expect(state.desired).toBe(false);
        expect(yield* Ref.get(harness.sessions)).toEqual([]);
        expect(
          (yield* SubscriptionRef.get(registry.entries)).get(RELAY_TARGET.environmentId)?.target
            .label,
        ).toBe("Cloud box");
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("loads a renamed environment's saved name in a new registry", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness([RELAY_TARGET]);

      yield* EnvironmentRegistry.EnvironmentRegistry.pipe(
        Effect.flatMap((registry) => registry.rename(RELAY_TARGET.environmentId, "Cloud box")),
        Effect.provide(harness.layer),
        Effect.scoped,
      );
      const reloaded = yield* EnvironmentRegistry.EnvironmentRegistry.pipe(
        Effect.flatMap((registry) => SubscriptionRef.get(registry.entries)),
        Effect.provide(harness.layer),
        Effect.scoped,
      );

      expect(reloaded.get(RELAY_TARGET.environmentId)?.target).toEqual(
        new RelayConnectionTarget({
          environmentId: RELAY_TARGET.environmentId,
          label: "Cloud box",
        }),
      );
    }),
  );

  it.effect("refuses blank names, platform environments, and unknown environments", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness([RELAY_TARGET]);

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.registerPlatform(new PrimaryConnectionRegistration({ target: TARGET }));

        const blank = yield* registry.rename(RELAY_TARGET.environmentId, " \t ").pipe(Effect.flip);
        const platform = yield* registry.rename(TARGET.environmentId, "Renamed").pipe(Effect.flip);
        const unknown = yield* registry
          .rename(EnvironmentId.make("missing-environment"), "Renamed")
          .pipe(Effect.flip);

        expect(blank._tag).toBe("ConnectionBlockedError");
        expect(platform._tag).toBe("ConnectionBlockedError");
        expect(unknown._tag).toBe("EnvironmentNotRegisteredError");
        expect((yield* Ref.get(harness.storedTargets)).get(RELAY_TARGET.environmentId)).toEqual(
          RELAY_TARGET,
        );
        const entries = yield* SubscriptionRef.get(registry.entries);
        expect(entries.get(RELAY_TARGET.environmentId)?.target.label).toBe(RELAY_TARGET.label);
        expect(entries.get(TARGET.environmentId)?.target.label).toBe(TARGET.label);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("ignores retry signals for environments that are no longer registered", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness([]);

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.retryNow(EnvironmentId.make("removed-environment"));
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("disconnect latches the supervisor without removing the entry", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness([TARGET]);

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.start;
        yield* awaitConnectionState(
          registry,
          TARGET.environmentId,
          (state) => state.phase === "connected",
        );

        yield* registry.disconnect(TARGET.environmentId);
        const disconnected = yield* awaitConnectionState(
          registry,
          TARGET.environmentId,
          (state) => state.desired === false,
        );
        expect(disconnected.desired).toBe(false);
        expect(disconnected.phase).not.toBe("connected");
        expect((yield* SubscriptionRef.get(registry.entries)).has(TARGET.environmentId)).toBe(true);

        yield* registry.connect(TARGET.environmentId);
        const reconnected = yield* awaitConnectionState(
          registry,
          TARGET.environmentId,
          (state) => state.desired === true,
        );
        expect(reconnected.desired).toBe(true);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("disconnect on an unregistered environment is a no-op", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness([]);
      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.disconnect(EnvironmentId.make("missing-environment"));
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("disconnect on a registered cold environment does not create a supervisor", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness([TARGET]);
      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;

        yield* registry.disconnect(TARGET.environmentId);

        expect(yield* Ref.get(harness.sessions)).toEqual([]);
        expect(yield* Ref.get(harness.releasedSessions)).toBe(0);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("passive state lookup preserves explicit cold disconnect intent", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness([TARGET]);
      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;

        yield* registry.disconnect(TARGET.environmentId);
        yield* registry.state(TARGET.environmentId);
        for (let iteration = 0; iteration < 100; iteration += 1) {
          yield* Effect.yieldNow;
        }

        const state = yield* registry.state(TARGET.environmentId);
        expect(state.desired).toBe(false);
        expect(yield* Ref.get(harness.sessions)).toEqual([]);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("builds a registered environment's supervisor with the stored intent", () =>
    Effect.gen(function* () {
      const probe = yield* makeFirstStateProbe();
      const harness = yield* makeHarness([], [], [], { beforePreparing: probe.beforePreparing });

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* probe.watch(registry);
        yield* registry.register(new RelayConnectionRegistration({ target: RELAY_TARGET }));
        yield* awaitConnectionState(
          registry,
          RELAY_TARGET.environmentId,
          (state) => state.phase === "connected",
        );

        expect(yield* Ref.get(probe.samples)).toEqual([
          [RELAY_TARGET.environmentId, expect.objectContaining(FIRST_DESIRED_STATE)],
        ]);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect(
    "builds a desired supervisor with offline as its first state while the network is offline",
    () =>
      Effect.gen(function* () {
        const probe = yield* makeFirstStateProbe();
        const harness = yield* makeHarness([TARGET], [], [], {
          beforePreparing: probe.beforePreparing,
        });
        yield* SubscriptionRef.set(harness.networkStatus, "offline");

        yield* Effect.gen(function* () {
          const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
          yield* probe.watch(registry);

          // A cold lookup builds and reads the supervisor before its loop can publish a state.
          expect(yield* registry.state(TARGET.environmentId)).toMatchObject({
            desired: true,
            phase: "offline",
            network: "offline",
            generation: 0,
          });
          expect(yield* Ref.get(probe.samples)).toEqual([]);
          expect(yield* Ref.get(harness.sessions)).toEqual([]);
        }).pipe(Effect.provide(harness.layer), Effect.scoped);
      }),
  );

  it.effect("builds a replacement supervisor with the stored intent", () =>
    Effect.gen(function* () {
      const moved = new PrimaryConnectionTarget({
        ...TARGET,
        label: "Moved environment",
        httpBaseUrl: "https://moved.example.test",
        wsBaseUrl: "wss://moved.example.test",
      });
      const probe = yield* makeFirstStateProbe();
      const harness = yield* makeHarness([], [], [], { beforePreparing: probe.beforePreparing });

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* probe.watch(registry);
        yield* registry.reconcilePlatform([new PrimaryConnectionRegistration({ target: TARGET })]);
        yield* awaitConnectionState(
          registry,
          TARGET.environmentId,
          (state) => state.phase === "connected",
        );
        // A changed platform registration: the registry replaces the supervisor.
        yield* registry.reconcilePlatform([new PrimaryConnectionRegistration({ target: moved })]);
        yield* awaitConnectionState(
          registry,
          TARGET.environmentId,
          (state) => state.phase === "connected",
        );

        expect(yield* Ref.get(probe.samples)).toEqual([
          [TARGET.environmentId, expect.objectContaining(FIRST_DESIRED_STATE)],
          [TARGET.environmentId, expect.objectContaining(FIRST_DESIRED_STATE)],
        ]);
        expect(yield* Ref.get(harness.sessions)).toHaveLength(2);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("builds every started environment's supervisor with the stored intent", () =>
    Effect.gen(function* () {
      const probe = yield* makeFirstStateProbe();
      const harness = yield* makeHarness([TARGET, SECOND_TARGET], [], [], {
        beforePreparing: probe.beforePreparing,
      });

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* probe.watch(registry);
        yield* registry.start;
        for (const target of [TARGET, SECOND_TARGET]) {
          yield* awaitConnectionState(
            registry,
            target.environmentId,
            (state) => state.phase === "connected",
          );
        }

        // Startup acquires both environments concurrently, so sort before comparing.
        const samples = [...(yield* Ref.get(probe.samples))].sort(([left], [right]) =>
          left.localeCompare(right),
        );
        expect(samples).toEqual([
          [TARGET.environmentId, expect.objectContaining(FIRST_DESIRED_STATE)],
          [SECOND_TARGET.environmentId, expect.objectContaining(FIRST_DESIRED_STATE)],
        ]);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("adopts the current structured storage change and retries exactly once", () =>
    Effect.gen(function* () {
      const connectionAttempts = yield* Ref.make(0);
      const adoptionOrder = yield* Ref.make<ReadonlyArray<string>>([]);
      const firstAttemptStarted = yield* Deferred.make<void>();
      const releaseFirstAttempt = yield* Deferred.make<void>();
      const harness = yield* makeHarness([TARGET], [], [], {
        beforeSessionConnect: () =>
          Ref.updateAndGet(connectionAttempts, (count) => count + 1).pipe(
            Effect.tap((attempt) =>
              Ref.update(adoptionOrder, (current) => [...current, `attempt:${attempt}`]),
            ),
            Effect.flatMap((attempt) =>
              attempt === 1
                ? Deferred.succeed(firstAttemptStarted, undefined).pipe(
                    Effect.andThen(Deferred.await(releaseFirstAttempt)),
                    Effect.andThen(
                      Effect.fail(
                        new ConnectionStorageChangedError({
                          reason: "storage-changed",
                          detail: "The environment reported a different persistent store.",
                          targetKey: "structured:error-target",
                          acceptedStorageInstanceId: "store-a",
                          reportedStorageInstanceId: "store-b",
                        }),
                      ),
                    ),
                  )
                : Effect.void,
            ),
          ),
        afterStorageIdentityAccept: (identity) =>
          Ref.update(adoptionOrder, (current) => [
            ...current,
            `accept:${identity.targetKey}:${identity.storageInstanceId}`,
          ]),
      });
      yield* Ref.set(
        harness.acceptedStorageIdentities,
        new Map([["structured:error-target", "store-a"]]),
      );

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.start;
        yield* Deferred.await(firstAttemptStarted);
        yield* Effect.yieldNow;
        yield* Deferred.succeed(releaseFirstAttempt, undefined);
        yield* awaitConnectionState(
          registry,
          TARGET.environmentId,
          (state) => state.phase === "blocked",
        );

        yield* registry.acceptStorageIdentity(TARGET.environmentId);
        yield* awaitConnectionState(
          registry,
          TARGET.environmentId,
          (state) => state.phase === "connected",
        );

        expect(yield* Ref.get(harness.acceptedStorageIdentityWrites)).toEqual([
          {
            targetKey: "structured:error-target",
            storageInstanceId: "store-b",
          },
        ]);
        expect(yield* Ref.get(connectionAttempts)).toBe(2);
        expect(yield* Ref.get(adoptionOrder)).toEqual([
          "attempt:1",
          "accept:structured:error-target:store-b",
          "attempt:2",
        ]);
        expect(yield* Ref.get(harness.cacheClears)).toEqual([]);
        expect(yield* Ref.get(harness.ownedDataClears)).toEqual([]);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("rejects stale storage adoption without overwriting a newer accepted identity", () =>
    Effect.gen(function* () {
      const connectionAttempts = yield* Ref.make(0);
      const firstAttemptStarted = yield* Deferred.make<void>();
      const releaseFirstAttempt = yield* Deferred.make<void>();
      const storageChanged = () =>
        new ConnectionStorageChangedError({
          reason: "storage-changed",
          detail: "The environment reported a different persistent store.",
          targetKey: "structured:error-target",
          acceptedStorageInstanceId: "store-a",
          reportedStorageInstanceId: "store-b",
        });
      const harness = yield* makeHarness([TARGET], [], [], {
        beforeSessionConnect: () =>
          Ref.updateAndGet(connectionAttempts, (count) => count + 1).pipe(
            Effect.flatMap((attempt) =>
              attempt === 1
                ? Deferred.succeed(firstAttemptStarted, undefined).pipe(
                    Effect.andThen(Deferred.await(releaseFirstAttempt)),
                    Effect.andThen(Effect.fail(storageChanged())),
                  )
                : Effect.fail(storageChanged()),
            ),
          ),
      });

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.start;
        yield* Deferred.await(firstAttemptStarted);
        yield* Effect.yieldNow;
        yield* Deferred.succeed(releaseFirstAttempt, undefined);
        yield* awaitConnectionState(
          registry,
          TARGET.environmentId,
          (state) => state.phase === "blocked",
        );
        yield* Ref.update(harness.acceptedStorageIdentities, (current) => {
          const next = new Map(current);
          next.set("structured:error-target", "store-c");
          return next;
        });

        const result = yield* Effect.result(registry.acceptStorageIdentity(TARGET.environmentId));

        expect(Result.isFailure(result)).toBe(true);
        if (Result.isFailure(result)) {
          expect(result.failure).toMatchObject({
            _tag: "ConnectionPersistenceError",
            operation: "accept-storage-identity",
          });
        }
        expect(yield* Ref.get(harness.acceptedStorageIdentities)).toEqual(
          new Map([["structured:error-target", "store-c"]]),
        );
        expect(yield* Ref.get(harness.acceptedStorageIdentityWrites)).toEqual([]);
        expect(yield* Ref.get(connectionAttempts)).toBe(1);
        expect(yield* Ref.get(harness.cacheClears)).toEqual([]);
        expect(yield* Ref.get(harness.ownedDataClears)).toEqual([]);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("refuses storage adoption unless the environment is currently storage-blocked", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness([TARGET]);

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.start;
        yield* awaitConnectionState(
          registry,
          TARGET.environmentId,
          (state) => state.phase === "connected",
        );

        const error = yield* Effect.flip(registry.acceptStorageIdentity(TARGET.environmentId));

        expect(error).toMatchObject({
          _tag: "ConnectionPersistenceError",
          operation: "accept-storage-identity",
        });
        expect(yield* Ref.get(harness.acceptedStorageIdentityWrites)).toEqual([]);
        expect(yield* Ref.get(harness.sessions)).toHaveLength(1);
        expect(yield* Ref.get(harness.cacheClears)).toEqual([]);
        expect(yield* Ref.get(harness.ownedDataClears)).toEqual([]);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("does not start an unsupervised environment to evaluate storage adoption", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness([TARGET]);

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;

        const error = yield* Effect.flip(registry.acceptStorageIdentity(TARGET.environmentId));
        for (let iteration = 0; iteration < 100; iteration += 1) {
          yield* Effect.yieldNow;
        }

        expect(error).toMatchObject({
          _tag: "ConnectionPersistenceError",
          operation: "accept-storage-identity",
        });
        expect(yield* Ref.get(harness.sessions)).toEqual([]);
        expect(yield* Ref.get(harness.acceptedStorageIdentityWrites)).toEqual([]);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("removes all relay-owned data without touching non-cloud connections", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness(
        [RELAY_TARGET, SECOND_RELAY_TARGET, BEARER_TARGET],
        [BEARER_PROFILE],
        [[BEARER_TARGET.connectionId, BEARER_CREDENTIAL]],
      );

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.removeRelayEnvironments();

        const targets = yield* Ref.get(harness.storedTargets);
        expect(targets.has(RELAY_TARGET.environmentId)).toBe(false);
        expect(targets.has(SECOND_RELAY_TARGET.environmentId)).toBe(false);
        expect(targets.get(BEARER_TARGET.environmentId)).toEqual(BEARER_TARGET);
        expect(yield* Ref.get(harness.cacheClears)).toEqual(
          expect.arrayContaining([RELAY_TARGET.environmentId, SECOND_RELAY_TARGET.environmentId]),
        );
        expect(yield* Ref.get(harness.ownedDataClears)).toEqual(
          expect.arrayContaining([RELAY_TARGET.environmentId, SECOND_RELAY_TARGET.environmentId]),
        );
        expect(
          (yield* SubscriptionRef.get(registry.entries)).has(BEARER_TARGET.environmentId),
        ).toBe(true);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("keeps the runtime registered when durable removal fails", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness([RELAY_TARGET], [], [], {
        beforeRegistrationRemove: () =>
          Effect.fail(
            new Persistence.ConnectionPersistenceError({
              operation: "remove-connection",
              message: "Storage is unavailable.",
            }),
          ),
      });

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.start;
        yield* awaitConnectionState(
          registry,
          RELAY_TARGET.environmentId,
          (state) => state.phase === "connected",
        );

        const error = yield* Effect.flip(registry.removeRelayEnvironments());

        expect(error._tag).toBe("ConnectionPersistenceError");
        expect(yield* Ref.get(harness.releasedSessions)).toBe(0);
        expect((yield* SubscriptionRef.get(registry.entries)).has(RELAY_TARGET.environmentId)).toBe(
          true,
        );
        expect((yield* Ref.get(harness.storedTargets)).has(RELAY_TARGET.environmentId)).toBe(true);
        expect(yield* Ref.get(harness.cacheClears)).toEqual([]);
        expect(yield* Ref.get(harness.ownedDataClears)).toEqual([]);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("starts a newly paired bearer environment without re-reading its profile", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness([]);

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.register(
          new BearerConnectionRegistration({
            target: BEARER_TARGET,
            profile: BEARER_PROFILE,
            credential: BEARER_CREDENTIAL,
          }),
        );
        yield* awaitConnectionState(
          registry,
          BEARER_TARGET.environmentId,
          (state) => state.phase === "connected",
        );

        expect(yield* Ref.get(harness.profileReadCount)).toBe(0);
        expect(
          Option.getOrThrow(
            (yield* SubscriptionRef.get(registry.entries)).get(BEARER_TARGET.environmentId)
              ?.profile ?? Option.none(),
          ),
        ).toEqual(BEARER_PROFILE);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("does not roll back a newer registration for the same environment", () =>
    Effect.gen(function* () {
      const first = new BearerConnectionRegistration({
        target: BEARER_TARGET,
        profile: BEARER_PROFILE,
        credential: new BearerConnectionCredential({ token: "first-credential" }),
      });
      const replacement = new BearerConnectionRegistration({
        target: BEARER_TARGET,
        profile: BEARER_PROFILE,
        credential: new BearerConnectionCredential({ token: "replacement-credential" }),
      });
      const replacementRegisterStarted = yield* Deferred.make<void>();
      const releaseReplacementRegister = yield* Deferred.make<void>();
      const harness = yield* makeHarness([], [], [], {
        beforeRegistrationRegister: (registration) =>
          registration === replacement
            ? Deferred.succeed(replacementRegisterStarted, undefined).pipe(
                Effect.andThen(Deferred.await(releaseReplacementRegister)),
              )
            : Effect.void,
      });

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.register(first);
        const replacementFiber = yield* Effect.forkChild(registry.register(replacement));
        yield* Deferred.await(replacementRegisterStarted);
        const rollbackFiber = yield* Effect.forkChild(registry.rollbackRegistration(first));
        yield* Deferred.succeed(releaseReplacementRegister, undefined);
        yield* Fiber.join(replacementFiber);
        expect(yield* Fiber.join(rollbackFiber)).toBe(false);

        expect((yield* Ref.get(harness.storedCredentials)).get(BEARER_TARGET.connectionId)).toEqual(
          replacement.credential,
        );
        expect(
          (yield* SubscriptionRef.get(registry.entries)).get(BEARER_TARGET.environmentId)?.target,
        ).toEqual(BEARER_TARGET);
        expect(yield* Ref.get(harness.cacheClears)).toEqual([]);
        expect(yield* Ref.get(harness.ownedDataClears)).toEqual([]);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("reconciles the local runtime to a replacement from another store", () =>
    Effect.gen(function* () {
      const first = new BearerConnectionRegistration({
        target: BEARER_TARGET,
        profile: BEARER_PROFILE,
        credential: new BearerConnectionCredential({ token: "first-credential" }),
      });
      const replacementTarget = new BearerConnectionTarget({
        ...BEARER_TARGET,
        label: "Replacement environment",
        serverEnvironmentId: null,
      });
      const replacement = new BearerConnectionRegistration({
        target: replacementTarget,
        profile: new BearerConnectionProfile({
          ...BEARER_PROFILE,
          label: replacementTarget.label,
          httpBaseUrl: "https://replacement.example.test",
          wsBaseUrl: "wss://replacement.example.test",
        }),
        credential: new BearerConnectionCredential({ token: "replacement-credential" }),
      });
      const harness = yield* makeHarness([]);

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.register(first);
        yield* registry.start;
        yield* awaitConnectionState(
          registry,
          BEARER_TARGET.environmentId,
          (state) => state.phase === "connected",
        );

        yield* harness.externalRegistrationStore.register(replacement);
        expect(yield* registry.rollbackRegistration(first)).toBe(false);

        const entry = (yield* SubscriptionRef.get(registry.entries)).get(
          BEARER_TARGET.environmentId,
        );
        expect(entry?.target).toEqual(replacement.target);
        expect(Option.getOrThrow(entry?.profile ?? Option.none())).toEqual(replacement.profile);
        yield* awaitConnectionState(
          registry,
          BEARER_TARGET.environmentId,
          (state) => state.phase === "connected",
        );
        expect(yield* Ref.get(harness.sessions)).toHaveLength(2);
        expect(yield* Ref.get(harness.releasedSessions)).toBe(1);
        expect(yield* Ref.get(harness.cacheClears)).toEqual([]);
        expect(yield* Ref.get(harness.ownedDataClears)).toEqual([]);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("preserves explicit disconnect intent across catalog profile drift", () =>
    Effect.gen(function* () {
      const first = new BearerConnectionRegistration({
        target: BEARER_TARGET,
        profile: BEARER_PROFILE,
        credential: new BearerConnectionCredential({ token: "first-credential" }),
      });
      const replacementTarget = new BearerConnectionTarget({
        ...BEARER_TARGET,
        label: "Replacement environment",
        serverEnvironmentId: null,
      });
      const replacement = new BearerConnectionRegistration({
        target: replacementTarget,
        profile: new BearerConnectionProfile({
          ...BEARER_PROFILE,
          label: replacementTarget.label,
          httpBaseUrl: "https://replacement.example.test",
          wsBaseUrl: "wss://replacement.example.test",
        }),
        credential: new BearerConnectionCredential({ token: "replacement-credential" }),
      });
      const connectionAttempts = yield* Ref.make(0);
      const harness = yield* makeHarness([], [], [], {
        beforeSessionConnect: () => Ref.update(connectionAttempts, (count) => count + 1),
      });

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.register(first);
        yield* awaitConnectionState(
          registry,
          BEARER_TARGET.environmentId,
          (state) => state.phase === "connected",
        );
        yield* registry.disconnect(BEARER_TARGET.environmentId);
        yield* awaitConnectionState(
          registry,
          BEARER_TARGET.environmentId,
          (state) => state.desired === false,
        );
        const original = yield* registry.run(
          BEARER_TARGET.environmentId,
          EnvironmentSupervisor.EnvironmentSupervisor,
        );
        // Every state any later supervisor publishes, from the first one it is built with.
        const replacementStates = yield* Ref.make<ReadonlyArray<SupervisorConnectionState>>([]);
        yield* registry
          .followStream(
            BEARER_TARGET.environmentId,
            Stream.unwrap(
              EnvironmentSupervisor.EnvironmentSupervisor.pipe(
                Effect.map((supervisor) =>
                  supervisor === original
                    ? Stream.empty
                    : SubscriptionRef.changes(supervisor.state),
                ),
              ),
            ),
          )
          .pipe(
            Stream.runForEach((state) => Ref.update(replacementStates, (all) => [...all, state])),
            Effect.forkChild({ startImmediately: true }),
          );

        yield* harness.externalRegistrationStore.register(replacement);
        expect(yield* registry.rollbackRegistration(first)).toBe(false);
        for (let iteration = 0; iteration < 100; iteration += 1) {
          yield* Effect.yieldNow;
        }

        const state = yield* registry.state(BEARER_TARGET.environmentId);
        expect(state.desired).toBe(false);
        expect(state.phase).toBe("available");
        const published = yield* Ref.get(replacementStates);
        expect(published.length).toBeGreaterThan(0);
        for (const replacementState of published) {
          expect(replacementState).toMatchObject({ desired: false, phase: "available" });
        }
        expect(yield* Ref.get(connectionAttempts)).toBe(1);
        expect(yield* Ref.get(harness.sessions)).toHaveLength(1);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("finishes rollback before installing a queued replacement", () =>
    Effect.gen(function* () {
      const first = new BearerConnectionRegistration({
        target: BEARER_TARGET,
        profile: BEARER_PROFILE,
        credential: new BearerConnectionCredential({ token: "first-credential" }),
      });
      const replacement = new BearerConnectionRegistration({
        target: BEARER_TARGET,
        profile: BEARER_PROFILE,
        credential: new BearerConnectionCredential({ token: "replacement-credential" }),
      });
      const rollbackStarted = yield* Deferred.make<void>();
      const releaseRollback = yield* Deferred.make<void>();
      const harness = yield* makeHarness([], [], [], {
        beforeRegistrationRemove: () =>
          Deferred.succeed(rollbackStarted, undefined).pipe(
            Effect.andThen(Deferred.await(releaseRollback)),
          ),
      });

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.register(first);
        const rollbackFiber = yield* Effect.forkChild(registry.rollbackRegistration(first));
        yield* Deferred.await(rollbackStarted);
        const replacementFiber = yield* Effect.forkChild(registry.register(replacement));
        yield* Deferred.succeed(releaseRollback, undefined);

        expect(yield* Fiber.join(rollbackFiber)).toBe(true);
        yield* Fiber.join(replacementFiber);
        expect((yield* Ref.get(harness.storedCredentials)).get(BEARER_TARGET.connectionId)).toEqual(
          replacement.credential,
        );
        expect(
          (yield* SubscriptionRef.get(registry.entries)).get(BEARER_TARGET.environmentId)?.target,
        ).toEqual(BEARER_TARGET);
        expect(yield* Ref.get(harness.cacheClears)).toEqual([BEARER_TARGET.environmentId]);
        expect(yield* Ref.get(harness.ownedDataClears)).toEqual([BEARER_TARGET.environmentId]);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("starts platform environments without persisting or removing them", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness([]);

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.registerPlatform(new PrimaryConnectionRegistration({ target: TARGET }));
        yield* awaitConnectionState(
          registry,
          TARGET.environmentId,
          (state) => state.phase === "connected",
        );

        expect((yield* Ref.get(harness.storedTargets)).has(TARGET.environmentId)).toBe(false);
        expect(
          (yield* SubscriptionRef.get(registry.entries)).get(TARGET.environmentId)?.target,
        ).toEqual(TARGET);

        const error = yield* Effect.flip(registry.remove(TARGET.environmentId));
        expect(error._tag).toBe("PlatformEnvironmentRemovalError");
        expect(
          (yield* SubscriptionRef.get(registry.entries)).get(TARGET.environmentId)?.target,
        ).toEqual(TARGET);
      }).pipe(Effect.provide(harness.layer));
    }),
  );

  it.effect("gives a primary platform registration precedence over persisted registrations", () =>
    Effect.gen(function* () {
      const shadowedTarget = new RelayConnectionTarget({
        environmentId: TARGET.environmentId,
        label: "Shadowed relay environment",
      });
      const harness = yield* makeHarness([shadowedTarget]);

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.registerPlatform(new PrimaryConnectionRegistration({ target: TARGET }));

        expect(
          (yield* SubscriptionRef.get(registry.entries)).get(TARGET.environmentId)?.target,
        ).toEqual(TARGET);
        expect((yield* Ref.get(harness.storedTargets)).has(TARGET.environmentId)).toBe(false);

        yield* registry.register(new RelayConnectionRegistration({ target: shadowedTarget }));

        expect(
          (yield* SubscriptionRef.get(registry.entries)).get(TARGET.environmentId)?.target,
        ).toEqual(TARGET);
        expect((yield* Ref.get(harness.storedTargets)).has(TARGET.environmentId)).toBe(false);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("rechecks platform ownership after waiting for the environment lease", () =>
    Effect.gen(function* () {
      const registrationStarted = yield* Deferred.make<void>();
      const continueRegistration = yield* Deferred.make<void>();
      const shadowedTarget = new RelayConnectionTarget({
        environmentId: TARGET.environmentId,
        label: "Shadowed relay environment",
      });
      const harness = yield* makeHarness([], [], [], {
        beforeRegistrationRegister: () =>
          Deferred.succeed(registrationStarted, undefined).pipe(
            Effect.andThen(Deferred.await(continueRegistration)),
          ),
      });

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        const persistedRegistration = yield* registry
          .register(new RelayConnectionRegistration({ target: shadowedTarget }))
          .pipe(Effect.forkChild({ startImmediately: true }));
        yield* Deferred.await(registrationStarted);

        const platformRegistration = yield* registry
          .registerPlatform(new PrimaryConnectionRegistration({ target: TARGET }))
          .pipe(Effect.forkChild({ startImmediately: true }));
        yield* Effect.yieldNow;
        const removal = yield* Effect.flip(registry.remove(TARGET.environmentId)).pipe(
          Effect.forkChild({ startImmediately: true }),
        );

        yield* Deferred.succeed(continueRegistration, undefined);
        yield* Fiber.join(persistedRegistration);
        yield* Fiber.join(platformRegistration);
        const error = yield* Fiber.join(removal);

        expect(error._tag).toBe("PlatformEnvironmentRemovalError");
        expect(
          (yield* SubscriptionRef.get(registry.entries)).get(TARGET.environmentId)?.target,
        ).toEqual(TARGET);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("does not reacquire a runtime while its registration is being removed", () =>
    Effect.gen(function* () {
      const removalStarted = yield* Deferred.make<void>();
      const continueRemoval = yield* Deferred.make<void>();
      const harness = yield* makeHarness([TARGET], [], [], {
        beforeRegistrationRemove: () =>
          Deferred.succeed(removalStarted, undefined).pipe(
            Effect.andThen(Deferred.await(continueRemoval)),
          ),
      });

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.start;
        yield* awaitConnectionState(
          registry,
          TARGET.environmentId,
          (state) => state.phase === "connected",
        );

        const removal = yield* Effect.forkChild(registry.remove(TARGET.environmentId));
        yield* Deferred.await(removalStarted);

        const stateLookup = yield* Effect.forkChild(
          Effect.flip(registry.state(TARGET.environmentId)),
        );
        yield* Effect.yieldNow;
        expect(yield* Ref.get(harness.sessions)).toHaveLength(1);

        yield* Deferred.succeed(continueRemoval, undefined);
        yield* Fiber.join(removal);
        const error = yield* Fiber.join(stateLookup);
        expect(error._tag).toBe("EnvironmentNotRegisteredError");
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect("retains a healthy runtime when the platform repeats an identical registration", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness([]);

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        const registration = new PrimaryConnectionRegistration({ target: TARGET });
        yield* registry.registerPlatform(registration);
        yield* awaitConnectionState(
          registry,
          TARGET.environmentId,
          (state) => state.phase === "connected",
        );

        yield* registry.registerPlatform(registration);

        expect(yield* Ref.get(harness.sessions)).toHaveLength(1);
      }).pipe(Effect.provide(harness.layer), Effect.scoped);
    }),
  );

  it.effect(
    "retains shell and thread caches for a desired unavailable platform environment until disable",
    () =>
      Effect.gen(function* () {
        const harness = yield* makeHarness(
          [],
          [],
          [
            [
              "local:wsl:Ubuntu",
              new BearerConnectionCredential({ token: "previous-live-session-token" }),
            ],
          ],
        );
        const unavailableTarget = new UnavailableConnectionTarget({
          environmentId: TARGET.environmentId,
          label: "WSL (Ubuntu)",
          connectionId: "local:wsl:Ubuntu",
          configuredDistro: "Ubuntu",
          detail: "the configured WSL distribution could not start",
        });
        const unavailableRegistration = new UnavailableConnectionRegistration({
          target: unavailableTarget,
        });

        yield* Effect.gen(function* () {
          const registry = yield* EnvironmentRegistry.EnvironmentRegistry;

          yield* registry.reconcilePlatform([unavailableRegistration]);
          yield* awaitConnectionState(
            registry,
            TARGET.environmentId,
            (state) => state.phase === "backoff",
          );

          expect(
            (yield* SubscriptionRef.get(registry.entries)).get(TARGET.environmentId)?.target,
          ).toEqual(unavailableTarget);
          expect((yield* Ref.get(harness.shellCache)).get(TARGET.environmentId)).toEqual(
            CACHED_SNAPSHOT,
          );
          expect(
            (yield* Ref.get(harness.threadCache)).get(TARGET.environmentId)?.get(CACHED_THREAD.id),
          ).toEqual(CACHED_THREAD);
          expect(yield* Ref.get(harness.cacheClears)).toEqual([]);
          expect((yield* Ref.get(harness.storedCredentials)).has("local:wsl:Ubuntu")).toBe(false);

          yield* registry.reconcilePlatform([]);
          expect((yield* Ref.get(harness.shellCache)).has(TARGET.environmentId)).toBe(false);
          expect((yield* Ref.get(harness.threadCache)).has(TARGET.environmentId)).toBe(false);
          expect(yield* Ref.get(harness.cacheClears)).toEqual([TARGET.environmentId]);

          yield* registry.reconcilePlatform([]);
          expect(yield* Ref.get(harness.cacheClears)).toEqual([TARGET.environmentId]);
        }).pipe(Effect.provide(harness.layer), Effect.scoped);
      }),
  );

  it.effect("removes all owned SSH state only on explicit removal", () =>
    Effect.gen(function* () {
      const harness = yield* makeHarness(
        [SSH_CONNECTION],
        [SSH_PROFILE],
        [
          [
            SSH_CONNECTION.connectionId,
            new BearerConnectionCredential({ token: "temporary-token" }),
          ],
        ],
      );

      yield* Effect.gen(function* () {
        const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
        yield* registry.start;
        const refreshedTarget: DesktopSshEnvironmentTarget = {
          alias: "refreshed",
          hostname: "refreshed.example.test",
          username: "operator",
          port: 2222,
        };
        yield* Ref.update(harness.storedProfiles, (current) => {
          const next = new Map(current);
          next.set(
            SSH_CONNECTION.connectionId,
            new SshConnectionProfile({
              connectionId: SSH_CONNECTION.connectionId,
              environmentId: SSH_CONNECTION.environmentId,
              label: SSH_CONNECTION.label,
              target: refreshedTarget,
            }),
          );
          return next;
        });
        yield* registry.remove(SSH_CONNECTION.environmentId);

        expect((yield* Ref.get(harness.storedProfiles)).has(SSH_CONNECTION.connectionId)).toBe(
          false,
        );
        expect((yield* Ref.get(harness.storedCredentials)).has(SSH_CONNECTION.connectionId)).toBe(
          false,
        );
        expect((yield* Ref.get(harness.storedRemoteTokens)).has(SSH_CONNECTION.environmentId)).toBe(
          false,
        );
        expect(yield* Ref.get(harness.disconnectedSshTargets)).toEqual([refreshedTarget]);
      }).pipe(Effect.provide(harness.layer));
    }),
  );
});

describe("persisted target labels", () => {
  it("recognizes persistable class instances rather than structurally matching objects", () => {
    expect(isPersistedConnectionTarget(RELAY_TARGET)).toBe(true);
    expect(isPersistedConnectionTarget(TARGET)).toBe(false);
    expect(isPersistedConnectionTarget({ ...RELAY_TARGET })).toBe(false);
  });

  it.each([
    BEARER_TARGET,
    RELAY_TARGET,
    new SshConnectionTarget({
      environmentId: EnvironmentId.make("env-ssh"),
      connectionId: "ssh-1",
      label: "SSH box",
    }),
  ])("preserves class equality and every field except label for $_tag", (target) => {
    const same = withPersistedTargetLabel(target, target.label);
    const changed = withPersistedTargetLabel(target, "New name");
    expect(Equal.equals(same, target)).toBe(true);
    expect(changed).toBeInstanceOf(target.constructor);
    expect(Equal.equals(changed, target)).toBe(false);
    expect({ ...changed }).toEqual({ ...target, label: "New name" });
  });
});
