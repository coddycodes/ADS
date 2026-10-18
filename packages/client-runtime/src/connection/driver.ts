import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as Layer from "effect/Layer";
import type * as Scope from "effect/Scope";

import * as Persistence from "../platform/persistence.ts";
import type { ConnectionCatalogEntry } from "./catalog.ts";
import type {
  ConnectionAttemptError,
  ConnectionAttemptStage,
  PreparedConnection,
} from "./model.ts";
import * as ConnectionResolver from "./resolver.ts";
import * as RpcSession from "../rpc/session.ts";
import { verifyPreparedStorageIdentity } from "./storageIdentity.ts";

export type ConnectionDriverProgress =
  | {
      readonly stage: "preparing";
    }
  | {
      readonly stage: Exclude<ConnectionAttemptStage, "preparing">;
      readonly prepared: PreparedConnection;
      /**
       * From `configuring` on: the session consumers may use before it is verified
       * (see `makeVerifiedSessionGate`); the lease returns the same object.
       */
      readonly session?: RpcSession.RpcSession;
    };

export interface EnvironmentConnectionLease {
  readonly prepared: PreparedConnection;
  readonly session: RpcSession.RpcSession;
  /**
   * Releases what the session held back while unverified. The supervisor runs it once
   * it reports the connection connected, so held data meets a connected environment.
   */
  readonly open?: Effect.Effect<void>;
}

export class ConnectionDriver extends Context.Service<
  ConnectionDriver,
  {
    readonly connect: (
      entry: ConnectionCatalogEntry,
      reportProgress: (progress: ConnectionDriverProgress) => Effect.Effect<void>,
    ) => Effect.Effect<EnvironmentConnectionLease, ConnectionAttemptError, Scope.Scope>;
  }
>()("@bibcode/client-runtime/connection/driver/ConnectionDriver") {}

export const make = Effect.gen(function* () {
  const resolver = yield* ConnectionResolver.ConnectionResolver;
  const sessions = yield* RpcSession.RpcSessionFactory;
  const identities = yield* Persistence.AcceptedStorageIdentityStore;

  const connect = Effect.fn("ConnectionDriver.connect")(function* (
    entry: ConnectionCatalogEntry,
    reportProgress: (progress: ConnectionDriverProgress) => Effect.Effect<void>,
  ) {
    const target = entry.target;
    yield* Effect.annotateCurrentSpan({
      "connection.environment.id": target.environmentId,
      "connection.target.kind": target._tag,
    });
    yield* reportProgress({ stage: "preparing" });
    const prepared = yield* resolver.prepare(entry);
    yield* verifyPreparedStorageIdentity(prepared).pipe(
      Effect.provideService(Persistence.AcceptedStorageIdentityStore, identities),
    );
    yield* reportProgress({ stage: "opening", prepared });
    const established = yield* sessions.connect(prepared);
    yield* established.connected;
    // Consumers get the session now and send their read subscriptions alongside the
    // config request instead of a round trip after it; the gate holds their data, and
    // every other call, until the config's identity check below passes.
    const gate = yield* RpcSession.makeVerifiedSessionGate(established);
    // An attempt that ends unverified, or a lease that closes, fails whatever is held.
    yield* Effect.addFinalizer(() => gate.close);
    yield* reportProgress({ stage: "configuring", prepared, session: gate.session });
    yield* Effect.gen(function* () {
      yield* established.ready;
      const initialConfig = yield* established.initialConfig;
      yield* verifyPreparedStorageIdentity(prepared, initialConfig.environment).pipe(
        Effect.provideService(Persistence.AcceptedStorageIdentityStore, identities),
      );
    }).pipe(Effect.onExit((exit) => (Exit.isSuccess(exit) ? Effect.void : gate.close)));
    yield* reportProgress({ stage: "synchronizing", prepared, session: gate.session });
    return {
      prepared,
      session: gate.session,
      open: gate.open,
    } satisfies EnvironmentConnectionLease;
  });

  return ConnectionDriver.of({ connect });
});

export const layer = Layer.effect(ConnectionDriver, make);
