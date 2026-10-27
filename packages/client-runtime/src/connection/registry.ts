import { EnvironmentId } from "@coddy/contracts";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Equal from "effect/Equal";
import * as Exit from "effect/Exit";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Ref from "effect/Ref";
import * as Schema from "effect/Schema";
import * as Scope from "effect/Scope";
import * as Semaphore from "effect/Semaphore";
import * as Stream from "effect/Stream";
import * as SubscriptionRef from "effect/SubscriptionRef";

import * as ClientCapabilities from "../platform/capabilities.ts";
import {
  type ConnectionCatalogEntry,
  type ConnectionProfile,
  type ConnectionRegistration,
  type PlatformConnectionRegistration,
  type PrimaryConnectionRegistration,
  SshConnectionProfile,
  connectionRegistrationCatalogEntry,
} from "./catalog.ts";
import * as ConnectionCredentialStore from "./credentialStore.ts";
import * as ConnectionProfileStore from "./profileStore.ts";
import * as Connectivity from "./connectivity.ts";
import {
  type ConnectionAttemptError,
  ConnectionBlockedError,
  type ConnectionTarget,
  type NetworkStatus,
  type SupervisorConnectionState,
  isPersistedConnectionTarget,
  withPersistedTargetLabel,
} from "./model.ts";
import { PLATFORM_ENVIRONMENT_NAME_MESSAGE, SERVER_NAME_REQUIRED_MESSAGE } from "./presentation.ts";
import * as Persistence from "../platform/persistence.ts";
import * as EnvironmentSupervisor from "./supervisor.ts";
import * as ConnectionDriver from "./driver.ts";
import * as ConnectionWakeups from "./wakeups.ts";
import { EnvironmentSelection } from "./selection.ts";

const isSshConnectionProfile = Schema.is(SshConnectionProfile);

export class EnvironmentNotRegisteredError extends Schema.TaggedError<EnvironmentNotRegisteredError>()(
  "EnvironmentNotRegisteredError",
  {
    environmentId: EnvironmentId,
  },
) {
  override get message(): string {
    return `Environment ${this.environmentId} is not registered.`;
  }
}

export class PlatformEnvironmentRemovalError extends Schema.TaggedError<PlatformEnvironmentRemovalError>()(
  "PlatformEnvironmentRemovalError",
  {
    environmentId: EnvironmentId,
  },
) {
  override get message(): string {
    return `Platform-managed environment ${this.environmentId} cannot be removed.`;
  }
}

export class EnvironmentRegistry extends Context.Service<
  EnvironmentRegistry,
  {
    readonly entries: SubscriptionRef.SubscriptionRef<
      ReadonlyMap<EnvironmentId, ConnectionCatalogEntry>
    >;
    readonly networkStatus: SubscriptionRef.SubscriptionRef<NetworkStatus>;
    readonly start: Effect.Effect<void>;
    readonly register: (
      registration: ConnectionRegistration,
    ) => Effect.Effect<void, Persistence.ConnectionPersistenceError>;
    readonly rollbackRegistration: (
      registration: ConnectionRegistration,
    ) => Effect.Effect<
      boolean,
      | Persistence.ConnectionPersistenceError
      | ConnectionAttemptError
      | EnvironmentNotRegisteredError
      | PlatformEnvironmentRemovalError
    >;
    readonly registerPlatform: (registration: PrimaryConnectionRegistration) => Effect.Effect<void>;
    readonly reconcilePlatform: (
      registrations: ReadonlyArray<PlatformConnectionRegistration>,
    ) => Effect.Effect<void>;
    readonly remove: (
      environmentId: EnvironmentId,
    ) => Effect.Effect<
      void,
      | Persistence.ConnectionPersistenceError
      | ConnectionAttemptError
      | EnvironmentNotRegisteredError
      | PlatformEnvironmentRemovalError
    >;
    readonly removeRelayEnvironments: () => Effect.Effect<
      void,
      | Persistence.ConnectionPersistenceError
      | ConnectionAttemptError
      | PlatformEnvironmentRemovalError
    >;
    /**
     * Saves a new client-local name for a saved environment. Only the catalog
     * label changes: the live runtime, its session, desired intent, and every
     * followed stream stay as they are.
     */
    readonly rename: (
      environmentId: EnvironmentId,
      label: string,
    ) => Effect.Effect<
      void,
      | Persistence.ConnectionPersistenceError
      | ConnectionAttemptError
      | EnvironmentNotRegisteredError
    >;
    readonly retryNow: (environmentId: EnvironmentId) => Effect.Effect<void>;
    readonly connect: (environmentId: EnvironmentId) => Effect.Effect<void>;
    readonly disconnect: (environmentId: EnvironmentId) => Effect.Effect<void>;
    readonly acceptStorageIdentity: (
      environmentId: EnvironmentId,
    ) => Effect.Effect<
      void,
      Persistence.ConnectionPersistenceError | EnvironmentNotRegisteredError
    >;
    readonly state: (
      environmentId: EnvironmentId,
    ) => Effect.Effect<SupervisorConnectionState, EnvironmentNotRegisteredError>;
    readonly stateChanges: (
      environmentId: EnvironmentId,
    ) => Stream.Stream<SupervisorConnectionState, EnvironmentNotRegisteredError>;
    readonly run: <A, E, R>(
      environmentId: EnvironmentId,
      effect: Effect.Effect<A, E, R>,
    ) => Effect.Effect<
      A,
      E | EnvironmentNotRegisteredError,
      Exclude<R, EnvironmentSupervisor.EnvironmentSupervisor>
    >;
    readonly runStream: <A, E, R>(
      environmentId: EnvironmentId,
      stream: Stream.Stream<A, E, R>,
    ) => Stream.Stream<
      A,
      E | EnvironmentNotRegisteredError,
      Exclude<R, EnvironmentSupervisor.EnvironmentSupervisor>
    >;
    readonly followStream: <A, E, R>(
      environmentId: EnvironmentId,
      stream: Stream.Stream<A, E, R>,
    ) => Stream.Stream<A, E, Exclude<R, EnvironmentSupervisor.EnvironmentSupervisor>>;
  }
>()("@coddy/client-runtime/connection/registry/EnvironmentRegistry") {}

interface EnvironmentServiceScope {
  readonly targetRef: Ref.Ref<ConnectionTarget>;
  readonly entry: ConnectionCatalogEntry;
  readonly supervisor: EnvironmentSupervisor.EnvironmentSupervisor["Service"];
  readonly scope: Scope.Closeable;
}

export const make = Effect.gen(function* () {
  const storage = yield* Persistence.ConnectionTargetStore;
  const registrations = yield* Persistence.ConnectionRegistrationStore;
  const identities = yield* Persistence.AcceptedStorageIdentityStore;
  const cache = yield* Persistence.EnvironmentCacheStore;
  const ownedDataCleanup = yield* Persistence.EnvironmentOwnedDataCleanup;
  const profiles = yield* ConnectionProfileStore.ConnectionProfileStore;
  const credentials = yield* ConnectionCredentialStore.ConnectionCredentialStore;
  const connectivity = yield* Connectivity.Connectivity;
  const driver = yield* ConnectionDriver.ConnectionDriver;
  const wakeups = yield* ConnectionWakeups.ConnectionWakeups;
  const selection = yield* EnvironmentSelection;
  const ssh = yield* ClientCapabilities.SshEnvironmentGateway;
  const persistedTargets = yield* storage.list;
  const initialEntries = new Map(
    yield* Effect.forEach(
      persistedTargets,
      Effect.fn("EnvironmentRegistry.loadCatalogEntry")(function* (target) {
        const profile =
          target._tag === "BearerConnectionTarget" || target._tag === "SshConnectionTarget"
            ? yield* profiles.get(target.connectionId)
            : Option.none();
        return [
          target.environmentId,
          { target, profile } satisfies ConnectionCatalogEntry,
        ] as const;
      }),
      { concurrency: "unbounded" },
    ),
  );
  const entries =
    yield* SubscriptionRef.make<ReadonlyMap<EnvironmentId, ConnectionCatalogEntry>>(initialEntries);
  // One token per environment, replaced whenever the registry installs a new
  // runtime for it and removed with the environment. Followed streams re-bind
  // on token changes rather than on entry changes, so a rename (which
  // republishes `entries` with a new label) leaves them and their live
  // subscriptions alone, while every installation, even of an identical
  // entry, moves them to the new supervisor.
  const installations = yield* SubscriptionRef.make<ReadonlyMap<EnvironmentId, number>>(
    new Map(persistedTargets.map((target) => [target.environmentId, 0])),
  );
  const nextInstallation = yield* Ref.make(1);
  const networkStatus = yield* SubscriptionRef.make(yield* connectivity.status);
  const serviceScopes = yield* SubscriptionRef.make<
    ReadonlyMap<EnvironmentId, EnvironmentServiceScope>
  >(new Map());
  const platformEnvironmentIds = yield* Ref.make<ReadonlySet<EnvironmentId>>(new Set());
  const persistedTargetsByEnvironment = yield* Ref.make<
    ReadonlyMap<EnvironmentId, ConnectionTarget>
  >(new Map(persistedTargets.map((target) => [target.environmentId, target])));
  const desiredStates = yield* Ref.make<ReadonlyMap<EnvironmentId, boolean>>(
    new Map(persistedTargets.map((target) => [target.environmentId, true])),
  );
  interface LeaseLock {
    readonly semaphore: Semaphore.Semaphore;
    readonly users: number;
  }

  const leaseLocks = yield* Ref.make<ReadonlyMap<EnvironmentId, LeaseLock>>(new Map());
  const leaseLocksGuard = yield* Semaphore.make(1);
  const started = yield* Ref.make(false);

  const withLeaseLock = <A, E, R>(
    environmentId: EnvironmentId,
    effect: Effect.Effect<A, E, R>,
  ): Effect.Effect<A, E, R> =>
    Effect.acquireUseRelease(
      leaseLocksGuard.withPermits(1)(
        Effect.gen(function* () {
          const current = yield* Ref.get(leaseLocks);
          const existing = current.get(environmentId);
          if (existing !== undefined) {
            yield* Ref.set(
              leaseLocks,
              new Map(current).set(environmentId, {
                semaphore: existing.semaphore,
                users: existing.users + 1,
              }),
            );
            return existing.semaphore;
          }
          const semaphore = yield* Semaphore.make(1);
          yield* Ref.set(leaseLocks, new Map(current).set(environmentId, { semaphore, users: 1 }));
          return semaphore;
        }),
      ),
      (semaphore) => semaphore.withPermits(1)(effect),
      (semaphore) =>
        leaseLocksGuard.withPermits(1)(
          Ref.update(leaseLocks, (current) => {
            const existing = current.get(environmentId);
            if (existing === undefined || existing.semaphore !== semaphore) {
              return current;
            }
            const next = new Map(current);
            if (existing.users === 1) {
              next.delete(environmentId);
            } else {
              next.set(environmentId, {
                semaphore,
                users: existing.users - 1,
              });
            }
            return next;
          }),
        ),
    ).pipe(Effect.withSpan("EnvironmentRegistry.withLeaseLock"));

  // Called under the environment lease lock. These publication helpers own
  // catalog metadata and installation identity; metadata alone never rebinds followers.
  const relabelEntryLocked = Effect.fn("EnvironmentRegistry.relabelEntryLocked")(function* (
    entry: ConnectionCatalogEntry,
  ) {
    yield* SubscriptionRef.update(entries, (current) =>
      new Map(current).set(entry.target.environmentId, entry),
    );
  });

  const publishInstallationLocked = Effect.fn("EnvironmentRegistry.publishInstallationLocked")(
    function* (entry: ConnectionCatalogEntry) {
      yield* relabelEntryLocked(entry);
      const installation = yield* Ref.getAndUpdate(nextInstallation, (value) => value + 1);
      yield* SubscriptionRef.update(installations, (current) =>
        new Map(current).set(entry.target.environmentId, installation),
      );
    },
  );

  const removeEntryLocked = Effect.fn("EnvironmentRegistry.removeEntryLocked")(function* (
    environmentId: EnvironmentId,
  ) {
    yield* SubscriptionRef.update(entries, (current) => {
      const next = new Map(current);
      next.delete(environmentId);
      return next;
    });
    yield* SubscriptionRef.update(installations, (current) => {
      const next = new Map(current);
      next.delete(environmentId);
      return next;
    });
  });

  const getEntry = Effect.fn("EnvironmentRegistry.getEntry")(function* (
    environmentId: EnvironmentId,
  ) {
    const entry = (yield* SubscriptionRef.get(entries)).get(environmentId);
    if (entry === undefined) {
      return yield* new EnvironmentNotRegisteredError({
        environmentId,
      });
    }
    return entry;
  });

  const closeServiceScope = Effect.fn("EnvironmentRegistry.closeServiceScope")(function* (
    environmentId: EnvironmentId,
  ) {
    const current = yield* SubscriptionRef.get(serviceScopes);
    const lease = current.get(environmentId);
    if (lease === undefined) {
      return;
    }
    const next = new Map(current);
    next.delete(environmentId);
    yield* SubscriptionRef.set(serviceScopes, next);
    yield* Scope.close(lease.scope, Exit.void);
  });

  // A supervisor is built with the stored intent, so the first state it publishes already
  // carries that intent: a desired replacement never reads as disconnected, even briefly.
  const createServiceScope = Effect.fn("EnvironmentRegistry.createServiceScope")(
    (entry: ConnectionCatalogEntry, desired: boolean) =>
      Effect.uninterruptible(
        Effect.gen(function* () {
          const environmentId = entry.target.environmentId;
          const scope = yield* Scope.make();
          const targetRef = yield* Ref.make(entry.target);
          const supervisor = yield* EnvironmentSupervisor.make(entry, {
            targetRef,
            initiallyDesired: desired,
          }).pipe(
            Effect.provideService(Connectivity.Connectivity, connectivity),
            Effect.provideService(ConnectionDriver.ConnectionDriver, driver),
            Effect.provideService(ConnectionWakeups.ConnectionWakeups, wakeups),
            Effect.provideService(EnvironmentSelection, selection),
            Scope.provide(scope),
            Effect.onError(() => Scope.close(scope, Exit.void)),
          );
          yield* SubscriptionRef.update(serviceScopes, (current) => {
            const next = new Map(current);
            next.set(environmentId, { entry, supervisor, scope, targetRef });
            return next;
          });
          return supervisor;
        }),
      ),
  );

  const installEntryLocked = Effect.fn("EnvironmentRegistry.installEntryLocked")(function* (
    entry: ConnectionCatalogEntry,
    options?: { readonly retainEquivalentRuntime?: boolean },
  ) {
    const target = entry.target;
    const previous = (yield* SubscriptionRef.get(entries)).get(target.environmentId);
    const existingScope = (yield* SubscriptionRef.get(serviceScopes)).get(target.environmentId);
    if (
      options?.retainEquivalentRuntime === true &&
      previous !== undefined &&
      Equal.equals(previous, entry) &&
      existingScope !== undefined &&
      Equal.equals(existingScope.entry, entry)
    ) {
      return existingScope.supervisor;
    }

    yield* closeServiceScope(target.environmentId);
    yield* publishInstallationLocked(entry);
    const desired = (yield* Ref.get(desiredStates)).get(target.environmentId) ?? false;
    return yield* createServiceScope(entry, desired);
  });

  const acquireSupervisorLocked = Effect.fn("EnvironmentRegistry.acquireSupervisorLocked")(
    function* (environmentId: EnvironmentId): Effect.fn.Return<
      {
        readonly created: boolean;
        readonly supervisor: EnvironmentSupervisor.EnvironmentSupervisor["Service"];
      },
      EnvironmentNotRegisteredError
    > {
      const entry = yield* getEntry(environmentId);
      const existing = (yield* SubscriptionRef.get(serviceScopes)).get(environmentId);
      if (existing !== undefined) {
        if (Equal.equals(existing.entry, entry)) {
          return { created: false, supervisor: existing.supervisor };
        }
        return { created: true, supervisor: yield* installEntryLocked(entry) };
      }
      const desired = (yield* Ref.get(desiredStates)).get(environmentId) ?? false;
      const supervisor = yield* createServiceScope(entry, desired);
      return { created: true, supervisor };
    },
  );

  const acquireSupervisor = Effect.fn("EnvironmentRegistry.acquireSupervisor")(function* (
    environmentId: EnvironmentId,
  ) {
    return yield* withLeaseLock(
      environmentId,
      acquireSupervisorLocked(environmentId).pipe(Effect.map(({ supervisor }) => supervisor)),
    );
  });

  const run: EnvironmentRegistry["Service"]["run"] = Effect.fn("EnvironmentRegistry.run")(
    function* <A, E, R>(environmentId: EnvironmentId, effect: Effect.Effect<A, E, R>) {
      const supervisor = yield* acquireSupervisor(environmentId);
      return yield* Effect.provideService(
        effect,
        EnvironmentSupervisor.EnvironmentSupervisor,
        supervisor,
      );
    },
  );

  const runStream: EnvironmentRegistry["Service"]["runStream"] = <A, E, R>(
    environmentId: EnvironmentId,
    stream: Stream.Stream<A, E, R>,
  ) =>
    Stream.unwrap(
      acquireSupervisor(environmentId).pipe(
        Effect.map((supervisor) =>
          Stream.provideService(stream, EnvironmentSupervisor.EnvironmentSupervisor, supervisor),
        ),
      ),
    );

  const followStream: EnvironmentRegistry["Service"]["followStream"] = <A, E, R>(
    environmentId: EnvironmentId,
    stream: Stream.Stream<A, E, R>,
  ) =>
    Stream.concat(
      Stream.fromEffect(SubscriptionRef.get(installations)),
      SubscriptionRef.changes(installations),
    ).pipe(
      Stream.map((current) => Option.fromUndefinedOr(current.get(environmentId))),
      Stream.changes,
      Stream.switchMap(
        Option.match({
          onNone: () => Stream.empty,
          onSome: () =>
            Stream.unwrap(
              acquireSupervisor(environmentId).pipe(
                Effect.match({
                  onFailure: () => Stream.empty,
                  onSuccess: (supervisor) =>
                    Stream.provideService(
                      stream,
                      EnvironmentSupervisor.EnvironmentSupervisor,
                      supervisor,
                    ),
                }),
              ),
            ),
        }),
      ),
    );

  const start = Effect.gen(function* () {
    if (yield* Ref.getAndSet(started, true)) {
      return;
    }
    yield* Effect.forEach(
      persistedTargets,
      (target) =>
        acquireSupervisor(target.environmentId).pipe(
          Effect.catchTag("EnvironmentNotRegisteredError", () => Effect.void),
        ),
      {
        concurrency: "unbounded",
        discard: true,
      },
    );
  }).pipe(Effect.withSpan("EnvironmentRegistry.start"));

  const register = Effect.fn("EnvironmentRegistry.register")(function* (
    registration: ConnectionRegistration,
  ) {
    const entry = connectionRegistrationCatalogEntry(registration);
    const environmentId = entry.target.environmentId;
    yield* withLeaseLock(
      environmentId,
      Effect.gen(function* () {
        if ((yield* Ref.get(platformEnvironmentIds)).has(environmentId)) {
          return;
        }
        yield* registrations.register(registration);
        yield* Ref.update(persistedTargetsByEnvironment, (current) => {
          const next = new Map(current);
          next.set(environmentId, registration.target);
          return next;
        });
        yield* Ref.update(desiredStates, (current) => {
          const next = new Map(current);
          next.set(environmentId, true);
          return next;
        });
        yield* installEntryLocked(entry);
      }),
    );
  });

  const installPlatformRegistration = Effect.fn("EnvironmentRegistry.installPlatformRegistration")(
    function* (registration: PlatformConnectionRegistration) {
      const entry = connectionRegistrationCatalogEntry(registration);
      const target = entry.target;
      yield* withLeaseLock(
        target.environmentId,
        Effect.gen(function* () {
          yield* Ref.update(platformEnvironmentIds, (current) => {
            const next = new Set(current);
            next.add(target.environmentId);
            return next;
          });
          yield* Ref.update(desiredStates, (current) => {
            if (current.has(target.environmentId)) {
              return current;
            }
            const next = new Map(current);
            next.set(target.environmentId, true);
            return next;
          });

          // Secondary desktop-local backends (e.g. a parallel WSL backend) live
          // on their own loopback origin, so they authenticate with a bearer
          // token instead of the primary's same-origin cookie. Stash it where
          // the resolver's bearer broker looks it up.
          if (registration._tag === "BearerConnectionRegistration") {
            yield* credentials.put(registration.target.connectionId, registration.credential).pipe(
              Effect.catch((error) =>
                Effect.logWarning("Could not store the platform bearer credential.", {
                  environmentId: target.environmentId,
                  error,
                }),
              ),
            );
          } else if (registration._tag === "UnavailableConnectionRegistration") {
            // A desired-but-unavailable desktop backend must not retain a
            // credential from its previous live registration. Its typed
            // target keeps identity/cache state without remaining usable.
            yield* credentials.remove(registration.target.connectionId).pipe(
              Effect.catch((error) =>
                Effect.logWarning("Could not clear the unavailable platform credential.", {
                  environmentId: target.environmentId,
                  error,
                }),
              ),
            );
          }

          const persistedTarget = (yield* Ref.get(persistedTargetsByEnvironment)).get(
            target.environmentId,
          );
          if (persistedTarget !== undefined) {
            yield* registrations.remove(persistedTarget).pipe(
              Effect.tap(() =>
                Ref.update(persistedTargetsByEnvironment, (current) => {
                  const next = new Map(current);
                  next.delete(target.environmentId);
                  return next;
                }),
              ),
              Effect.catch((error) =>
                Effect.logWarning(
                  "Could not remove a persisted registration shadowed by a platform environment.",
                  {
                    environmentId: target.environmentId,
                    error,
                  },
                ),
              ),
            );
          }

          yield* installEntryLocked(entry, { retainEquivalentRuntime: true });
        }),
      );
    },
  );

  // Tear down a platform-managed environment that the host no longer reports
  // (e.g. the user turned the parallel WSL backend off). Platform environments
  // bypass the user-facing `remove` guard since they are reconciled from the
  // bootstrap rather than removed by hand.
  const removePlatformEnvironment = Effect.fn("EnvironmentRegistry.removePlatformEnvironment")(
    function* (environmentId: EnvironmentId) {
      yield* withLeaseLock(
        environmentId,
        Effect.gen(function* () {
          const entry = (yield* SubscriptionRef.get(entries)).get(environmentId);
          yield* Ref.update(platformEnvironmentIds, (current) => {
            const next = new Set(current);
            next.delete(environmentId);
            return next;
          });
          yield* Ref.update(desiredStates, (current) => {
            const next = new Map(current);
            next.delete(environmentId);
            return next;
          });
          yield* closeServiceScope(environmentId);
          yield* removeEntryLocked(environmentId);
          if (
            entry !== undefined &&
            (entry.target._tag === "BearerConnectionTarget" ||
              entry.target._tag === "UnavailableConnectionTarget")
          ) {
            yield* credentials.remove(entry.target.connectionId).pipe(
              Effect.catch((error) =>
                Effect.logWarning("Could not clear the platform bearer credential.", {
                  environmentId,
                  error,
                }),
              ),
            );
          }
          yield* Effect.all(
            [
              cache.clear(environmentId).pipe(
                Effect.catch((error) =>
                  Effect.logWarning("Could not clear cached environment data after removal.", {
                    environmentId,
                    error,
                  }),
                ),
              ),
              ownedDataCleanup.clear(environmentId),
            ],
            { concurrency: "unbounded", discard: true },
          );
        }),
      );
    },
  );

  const registerPlatform = Effect.fn("EnvironmentRegistry.registerPlatform")(function* (
    registration: PrimaryConnectionRegistration,
  ) {
    yield* installPlatformRegistration(registration);
  });

  // Reconcile the full set of platform-managed environments against what the
  // host currently reports: add/refresh the desired ones and tear down any
  // platform environment that disappeared (WSL toggled off, distro switched).
  const reconcilePlatform = Effect.fn("EnvironmentRegistry.reconcilePlatform")(function* (
    platformRegistrations: ReadonlyArray<PlatformConnectionRegistration>,
  ) {
    const desiredIds = new Set(
      platformRegistrations.map((registration) => registration.target.environmentId),
    );
    const currentPlatformIds = yield* Ref.get(platformEnvironmentIds);
    yield* Effect.forEach(
      currentPlatformIds,
      (environmentId) =>
        desiredIds.has(environmentId) ? Effect.void : removePlatformEnvironment(environmentId),
      { discard: true },
    );
    yield* Effect.forEach(platformRegistrations, installPlatformRegistration, { discard: true });
  });

  const cleanupRemovedEntryLocked = Effect.fn("EnvironmentRegistry.cleanupRemovedEntryLocked")(
    function* (entry: ConnectionCatalogEntry, profile: Option.Option<ConnectionProfile>) {
      const target = entry.target;
      yield* Ref.update(persistedTargetsByEnvironment, (current) => {
        const next = new Map(current);
        next.delete(target.environmentId);
        return next;
      });
      yield* Ref.update(desiredStates, (current) => {
        const next = new Map(current);
        next.delete(target.environmentId);
        return next;
      });
      yield* closeServiceScope(target.environmentId);
      yield* removeEntryLocked(target.environmentId);
      yield* Effect.all(
        [
          cache.clear(target.environmentId).pipe(
            Effect.catch((error) =>
              Effect.logWarning("Could not clear cached environment data after removal.", {
                environmentId: target.environmentId,
                error,
              }),
            ),
          ),
          ownedDataCleanup.clear(target.environmentId),
        ],
        { concurrency: "unbounded", discard: true },
      );

      if (
        target._tag === "SshConnectionTarget" &&
        Option.isSome(profile) &&
        isSshConnectionProfile(profile.value)
      ) {
        yield* ssh.disconnect(profile.value.target).pipe(
          Effect.tapError((error) =>
            Effect.logWarning("Could not disconnect the managed SSH environment.", {
              environmentId: target.environmentId,
              error,
            }),
          ),
          Effect.ignore,
        );
      }
    },
  );

  const removeLocked = Effect.fn("EnvironmentRegistry.removeLocked")(function* (
    environmentId: EnvironmentId,
  ) {
    if ((yield* Ref.get(platformEnvironmentIds)).has(environmentId)) {
      return yield* new PlatformEnvironmentRemovalError({
        environmentId,
      });
    }
    const entry = yield* getEntry(environmentId);
    const profile =
      entry.target._tag === "BearerConnectionTarget" || entry.target._tag === "SshConnectionTarget"
        ? yield* profiles.get(entry.target.connectionId)
        : entry.profile;
    yield* registrations.remove(entry.target);
    yield* cleanupRemovedEntryLocked(entry, profile);
  });

  const remove = Effect.fn("EnvironmentRegistry.remove")(function* (environmentId: EnvironmentId) {
    return yield* withLeaseLock(environmentId, removeLocked(environmentId));
  });

  const rollbackRegistration = Effect.fn("EnvironmentRegistry.rollbackRegistration")(function* (
    registration: ConnectionRegistration,
  ) {
    const environmentId = registration.target.environmentId;
    return yield* withLeaseLock(
      environmentId,
      Effect.gen(function* () {
        const removal = yield* registrations.removeIfMatching(registration);
        if (!removal.removed) {
          if ((yield* Ref.get(platformEnvironmentIds)).has(environmentId)) {
            yield* Ref.update(persistedTargetsByEnvironment, (current) => {
              const next = new Map(current);
              if (removal.current === null) next.delete(environmentId);
              else next.set(environmentId, removal.current.target);
              return next;
            });
            return false;
          }
          if (removal.current !== null) {
            const currentEntry = removal.current;
            yield* Ref.update(persistedTargetsByEnvironment, (current) => {
              const next = new Map(current);
              next.set(environmentId, currentEntry.target);
              return next;
            });
            yield* installEntryLocked(currentEntry);
            return false;
          }
          const entry = (yield* SubscriptionRef.get(entries)).get(environmentId);
          if (entry !== undefined) {
            yield* cleanupRemovedEntryLocked(entry, entry.profile);
          }
          return false;
        }
        if ((yield* Ref.get(platformEnvironmentIds)).has(environmentId)) {
          yield* Ref.update(persistedTargetsByEnvironment, (current) => {
            const next = new Map(current);
            next.delete(environmentId);
            return next;
          });
          return true;
        }
        const entry = yield* getEntry(environmentId);
        if (!Equal.equals(entry, connectionRegistrationCatalogEntry(registration))) return true;
        yield* cleanupRemovedEntryLocked(entry, entry.profile);
        return true;
      }),
    );
  });

  const removeRelayEnvironments = Effect.fn("EnvironmentRegistry.removeRelayEnvironments")(
    function* () {
      const relayEnvironmentIds = [...(yield* SubscriptionRef.get(entries)).values()]
        .filter((entry) => entry.target._tag === "RelayConnectionTarget")
        .map((entry) => entry.target.environmentId);

      yield* Effect.forEach(
        relayEnvironmentIds,
        (environmentId) =>
          remove(environmentId).pipe(
            Effect.catchTag("EnvironmentNotRegisteredError", () => Effect.void),
          ),
        {
          concurrency: "unbounded",
          discard: true,
        },
      );
    },
  );

  const renameBlocked = (detail: string) =>
    new ConnectionBlockedError({ reason: "configuration", detail });

  const rename = Effect.fn("EnvironmentRegistry.rename")(function* (
    environmentId: EnvironmentId,
    label: string,
  ) {
    const name = label.trim();
    if (name === "") {
      return yield* renameBlocked(SERVER_NAME_REQUIRED_MESSAGE);
    }
    yield* withLeaseLock(
      environmentId,
      Effect.uninterruptible(
        Effect.gen(function* () {
          if ((yield* Ref.get(platformEnvironmentIds)).has(environmentId)) {
            return yield* renameBlocked(PLATFORM_ENVIRONMENT_NAME_MESSAGE);
          }
          const entry = yield* getEntry(environmentId);
          const target = entry.target;
          if (!isPersistedConnectionTarget(target)) {
            return yield* renameBlocked(PLATFORM_ENVIRONMENT_NAME_MESSAGE);
          }
          if (target.label === name) {
            return;
          }
          const saved = yield* registrations.relabel(environmentId, name);
          if (Option.isNone(saved)) {
            return yield* new EnvironmentNotRegisteredError({ environmentId });
          }
          // Relabel the in-memory entry rather than adopting the durable
          // target, so a registration changed by another store can never be
          // passed off as the runtime installed here.
          const renamed: ConnectionCatalogEntry = {
            target: withPersistedTargetLabel(target, name),
            profile: entry.profile,
          };
          yield* Ref.update(persistedTargetsByEnvironment, (current) => {
            const next = new Map(current);
            next.set(environmentId, renamed.target);
            return next;
          });
          // The live runtime now serves the renamed entry. Recording that keeps
          // later acquisitions from mistaking the label change for catalog
          // drift and replacing (reconnecting) the supervisor.
          const lease = (yield* SubscriptionRef.get(serviceScopes)).get(environmentId);
          if (lease !== undefined) {
            yield* Ref.set(lease.targetRef, renamed.target);
          }
          yield* SubscriptionRef.update(serviceScopes, (current) => {
            const lease = current.get(environmentId);
            if (lease === undefined) {
              return current;
            }
            const next = new Map(current);
            next.set(environmentId, { ...lease, entry: renamed });
            return next;
          });
          yield* relabelEntryLocked(renamed);
        }),
      ),
    );
  });

  const retryNow = (environmentId: EnvironmentId) =>
    withLeaseLock(
      environmentId,
      Effect.gen(function* () {
        // A retry request is connection intent: record it so the kick is not
        // a silent no-op for an environment the user had disconnected, and so
        // later catalog drift cannot resurrect a stale desired=false.
        yield* getEntry(environmentId);
        yield* Ref.update(desiredStates, (current) => {
          const next = new Map(current);
          next.set(environmentId, true);
          return next;
        });
        const acquired = yield* acquireSupervisorLocked(environmentId);
        if (!acquired.created) {
          yield* acquired.supervisor.connect;
        }
        yield* acquired.supervisor.retryNow;
      }),
    ).pipe(
      Effect.catchTag("EnvironmentNotRegisteredError", () => Effect.void),
      Effect.withSpan("EnvironmentRegistry.retryNow"),
    );
  const connect = (environmentId: EnvironmentId) =>
    withLeaseLock(
      environmentId,
      Effect.gen(function* () {
        yield* getEntry(environmentId);
        yield* Ref.update(desiredStates, (current) => {
          const next = new Map(current);
          next.set(environmentId, true);
          return next;
        });
        const acquired = yield* acquireSupervisorLocked(environmentId);
        if (!acquired.created) {
          yield* acquired.supervisor.connect;
        }
      }),
    ).pipe(
      Effect.catchTag("EnvironmentNotRegisteredError", () => Effect.void),
      Effect.withSpan("EnvironmentRegistry.connect"),
    );
  const disconnect = (environmentId: EnvironmentId) =>
    withLeaseLock(
      environmentId,
      Effect.gen(function* () {
        const entry = (yield* SubscriptionRef.get(entries)).get(environmentId);
        if (entry === undefined) {
          return;
        }
        yield* Ref.update(desiredStates, (current) => {
          const next = new Map(current);
          next.set(environmentId, false);
          return next;
        });
        const supervisor = (yield* SubscriptionRef.get(serviceScopes)).get(
          environmentId,
        )?.supervisor;
        if (supervisor !== undefined) {
          yield* supervisor.disconnect;
        }
      }),
    ).pipe(Effect.withSpan("EnvironmentRegistry.disconnect"));
  const acceptStorageIdentity = Effect.fn("EnvironmentRegistry.acceptStorageIdentity")(function* (
    environmentId: EnvironmentId,
  ) {
    yield* withLeaseLock(
      environmentId,
      Effect.uninterruptible(
        Effect.gen(function* () {
          yield* getEntry(environmentId);
          const supervisor = (yield* SubscriptionRef.get(serviceScopes)).get(
            environmentId,
          )?.supervisor;
          if (supervisor === undefined) {
            return yield* new Persistence.ConnectionPersistenceError({
              operation: "accept-storage-identity",
              message: "The environment is not currently blocked by a persistent store change.",
            });
          }
          const current = yield* SubscriptionRef.get(supervisor.state);
          if (
            current.phase !== "blocked" ||
            current.lastFailure?._tag !== "ConnectionStorageChangedError"
          ) {
            return yield* new Persistence.ConnectionPersistenceError({
              operation: "accept-storage-identity",
              message: "The environment is not currently blocked by a persistent store change.",
            });
          }
          const storageFailure = current.lastFailure;
          const adopted = yield* identities.transition(
            storageFailure.targetKey,
            (acceptedStorageInstanceId) =>
              acceptedStorageInstanceId === storageFailure.acceptedStorageInstanceId
                ? {
                    result: true,
                    mutation: {
                      _tag: "Set",
                      storageInstanceId: storageFailure.reportedStorageInstanceId,
                    },
                  }
                : { result: false, mutation: { _tag: "Keep" } },
          );
          if (!adopted) {
            return yield* new Persistence.ConnectionPersistenceError({
              operation: "accept-storage-identity",
              message: "The accepted persistent store changed before it could be adopted.",
            });
          }
          yield* supervisor.retryNow;
        }),
      ),
    );
  });
  const state = Effect.fn("EnvironmentRegistry.state")(function* (environmentId: EnvironmentId) {
    const supervisor = yield* acquireSupervisor(environmentId);
    return yield* SubscriptionRef.get(supervisor.state);
  });
  const stateChanges = (environmentId: EnvironmentId) =>
    followStream(
      environmentId,
      Stream.unwrap(
        EnvironmentSupervisor.EnvironmentSupervisor.pipe(
          Effect.map((supervisor) => SubscriptionRef.changes(supervisor.state)),
        ),
      ),
    );

  yield* Effect.addFinalizer(() =>
    SubscriptionRef.get(serviceScopes).pipe(
      Effect.flatMap((current) =>
        Effect.forEach(current.values(), (lease) => Scope.close(lease.scope, Exit.void), {
          concurrency: "unbounded",
          discard: true,
        }),
      ),
    ),
  );
  yield* connectivity.changes.pipe(
    Stream.runForEach((status) => SubscriptionRef.set(networkStatus, status)),
    Effect.forkScoped,
  );

  return EnvironmentRegistry.of({
    entries,
    networkStatus,
    start,
    register,
    rollbackRegistration,
    registerPlatform,
    reconcilePlatform,
    remove,
    removeRelayEnvironments,
    rename,
    retryNow,
    connect,
    disconnect,
    acceptStorageIdentity,
    state,
    stateChanges,
    run,
    runStream,
    followStream,
  });
});

export const layer = Layer.effect(EnvironmentRegistry, make);
