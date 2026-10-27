import {
  type EnvironmentId,
  type OrchestrationThread,
  type OrchestrationShellSnapshot,
  type ThreadId,
} from "@coddy/contracts";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";
import * as SubscriptionRef from "effect/SubscriptionRef";

import type { ConnectionCatalogEntry, ConnectionRegistration } from "../connection/catalog.ts";
import type { ConnectionTarget, PersistedConnectionTarget } from "../connection/model.ts";

export class ConnectionPersistenceError extends Schema.TaggedError<ConnectionPersistenceError>()(
  "ConnectionPersistenceError",
  {
    operation: Schema.Literals([
      "list-targets",
      "register-connection",
      "remove-connection",
      "rename-connection",
      "load-shell",
      "save-shell",
      "load-thread",
      "save-thread",
      "remove-thread",
      "clear-environment",
      "load-storage-identity",
      "accept-storage-identity",
      "reset-connection-catalog",
    ]),
    message: Schema.String,
  },
) {}

export class ConnectionTargetStore extends Context.Service<
  ConnectionTargetStore,
  {
    readonly list: Effect.Effect<ReadonlyArray<ConnectionTarget>, ConnectionPersistenceError>;
  }
>()("@coddy/client-runtime/platform/persistence/ConnectionTargetStore") {}

export interface ConnectionRegistrationRemovalResult {
  readonly removed: boolean;
  readonly current: ConnectionCatalogEntry | null;
}

export class ConnectionRegistrationStore extends Context.Service<
  ConnectionRegistrationStore,
  {
    readonly register: (
      registration: ConnectionRegistration,
    ) => Effect.Effect<void, ConnectionPersistenceError>;
    readonly removeIfMatching: (
      registration: ConnectionRegistration,
    ) => Effect.Effect<ConnectionRegistrationRemovalResult, ConnectionPersistenceError>;
    readonly remove: (target: ConnectionTarget) => Effect.Effect<void, ConnectionPersistenceError>;
    /**
     * Rewrites only the saved display label of one persisted target in a single
     * catalog transition, leaving its profile, credential, tokens, and accepted
     * identity untouched. Returns the durable target, or none when the
     * environment is not saved.
     */
    readonly relabel: (
      environmentId: EnvironmentId,
      label: string,
    ) => Effect.Effect<Option.Option<PersistedConnectionTarget>, ConnectionPersistenceError>;
  }
>()("@coddy/client-runtime/platform/persistence/ConnectionRegistrationStore") {}

export interface AcceptedStorageIdentity {
  readonly targetKey: string;
  readonly storageInstanceId: string;
}

export const AcceptedStorageIdentitySchema = Schema.Struct({
  targetKey: Schema.String,
  storageInstanceId: Schema.String,
});

export type AcceptedStorageIdentityMutation =
  | {
      readonly _tag: "Keep";
    }
  | {
      readonly _tag: "Set";
      readonly storageInstanceId: string;
    };

export interface AcceptedStorageIdentityTransition<A> {
  readonly result: A;
  readonly mutation: AcceptedStorageIdentityMutation;
}

export class AcceptedStorageIdentityStore extends Context.Service<
  AcceptedStorageIdentityStore,
  {
    readonly get: (
      targetKey: string,
    ) => Effect.Effect<Option.Option<string>, ConnectionPersistenceError>;
    readonly accept: (
      identity: AcceptedStorageIdentity,
    ) => Effect.Effect<void, ConnectionPersistenceError>;
    readonly rollbackAcceptance: (
      identity: AcceptedStorageIdentity,
      previousStorageInstanceId: string | null,
    ) => Effect.Effect<boolean, ConnectionPersistenceError>;
    readonly transition: <A>(
      targetKey: string,
      decide: (acceptedStorageInstanceId: string | null) => AcceptedStorageIdentityTransition<A>,
    ) => Effect.Effect<A, ConnectionPersistenceError>;
  }
>()("@coddy/client-runtime/platform/persistence/AcceptedStorageIdentityStore") {}

export type ConnectionCatalogHealth =
  | { readonly status: "ready" }
  | { readonly status: "recovery-required"; readonly message: string };

export class ConnectionCatalogHealthStore extends Context.Service<
  ConnectionCatalogHealthStore,
  {
    readonly state: SubscriptionRef.SubscriptionRef<ConnectionCatalogHealth>;
    readonly reset: Effect.Effect<void, ConnectionPersistenceError>;
  }
>()("@coddy/client-runtime/platform/persistence/ConnectionCatalogHealthStore") {}

export class EnvironmentCacheStore extends Context.Service<
  EnvironmentCacheStore,
  {
    readonly loadShell: (
      environmentId: EnvironmentId,
    ) => Effect.Effect<Option.Option<OrchestrationShellSnapshot>, ConnectionPersistenceError>;
    readonly saveShell: (
      environmentId: EnvironmentId,
      snapshot: OrchestrationShellSnapshot,
    ) => Effect.Effect<void, ConnectionPersistenceError>;
    readonly loadThread: (
      environmentId: EnvironmentId,
      threadId: ThreadId,
    ) => Effect.Effect<Option.Option<OrchestrationThread>, ConnectionPersistenceError>;
    readonly saveThread: (
      environmentId: EnvironmentId,
      thread: OrchestrationThread,
    ) => Effect.Effect<void, ConnectionPersistenceError>;
    readonly removeThread: (
      environmentId: EnvironmentId,
      threadId: ThreadId,
    ) => Effect.Effect<void, ConnectionPersistenceError>;
    readonly clear: (
      environmentId: EnvironmentId,
    ) => Effect.Effect<void, ConnectionPersistenceError>;
  }
>()("@coddy/client-runtime/platform/persistence/EnvironmentCacheStore") {}

export class EnvironmentOwnedDataCleanup extends Context.Reference<{
  readonly clear: (environmentId: EnvironmentId) => Effect.Effect<void>;
}>("@coddy/client-runtime/platform/persistence/EnvironmentOwnedDataCleanup", {
  defaultValue: () => ({
    clear: () => Effect.void,
  }),
}) {}
