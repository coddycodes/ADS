// The clone re-attach loop and the next-session cancel against the real environment registry:
// a supervisor the registry replaces, and an environment the registry removes. Only the
// connection driver's sessions are scripted; the registry and its supervisors are real.
import { EnvironmentId, WS_METHODS } from "@bibcode/contracts";
import { makeTestExecutionEnvironmentCapabilities } from "@bibcode/shared/testSupport";
import { describe, expect, it } from "@effect/vitest";
import * as Cause from "effect/Cause";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Stream from "effect/Stream";
import { AsyncResult, Atom, AtomRegistry } from "effect/reactivity";
import { RpcClientError } from "effect/rpc";

import { PrimaryConnectionRegistration } from "../connection/catalog.ts";
import * as Connectivity from "../connection/connectivity.ts";
import * as ConnectionCredentialStore from "../connection/credentialStore.ts";
import * as ConnectionDriver from "../connection/driver.ts";
import {
  type PreparedConnection,
  PrimaryConnectionTarget,
  type SupervisorConnectionState,
} from "../connection/model.ts";
import * as ConnectionProfileStore from "../connection/profileStore.ts";
import * as EnvironmentRegistry from "../connection/registry.ts";
import * as ConnectionWakeups from "../connection/wakeups.ts";
import * as ClientCapabilities from "../platform/capabilities.ts";
import * as Persistence from "../platform/persistence.ts";
import type { WsRpcProtocolClient } from "../rpc/protocol.ts";
import type { RpcSession } from "../rpc/session.ts";
import { runAtomCommand, type AtomCommandResult } from "./runtime.ts";
import { createVcsEnvironmentAtoms } from "./vcs.ts";
import { registerCloneCancel, type VcsCloneProgress, VcsCloneStoppedError } from "./vcsClone.ts";

const ENVIRONMENT_ID = EnvironmentId.make("clone-registry-environment");
const TARGET = new PrimaryConnectionTarget({
  environmentId: ENVIRONMENT_ID,
  label: "Remote",
  httpBaseUrl: "https://clone.example.test",
  wsBaseUrl: "wss://clone.example.test",
});
/** A changed platform registration for the same environment: the registry replaces its supervisor. */
const MOVED_TARGET = new PrimaryConnectionTarget({
  environmentId: ENVIRONMENT_ID,
  label: "Remote (moved)",
  httpBaseUrl: "https://clone-moved.example.test",
  wsBaseUrl: "wss://clone-moved.example.test",
});
/**
 * Each test clones its own URL: the pending-cancel registry is module-level, so a cancel one test
 * leaves pending must not hold another test's clone.
 */
function cloneOf(name: string) {
  return { url: `https://example.test/${name}/demo.git`, parentDir: "/code" } as const;
}
const LOST_SOCKET = new RpcClientError.RpcClientError({
  reason: new RpcClientError.RpcClientDefect({
    message: "socket closed",
    cause: new Error("socket closed"),
  }),
});
/** A loop that never follows the registry waits forever; the test timeout records that hang. */
const TEST_TIMEOUT_MS = 10_000;

type Answer = Effect.Effect<unknown, RpcClientError.RpcClientError>;

interface Call {
  readonly session: number;
  readonly input: unknown;
}

function unused(name: string) {
  return () => Effect.die(new Error(`${name} is not used by these tests.`));
}

/** A real registry whose driver hands out scripted sessions, numbered from 1. */
function makeHarness() {
  const calls = { clone: [] as Array<Call>, cancel: [] as Array<Call> };
  /** The label of every target the driver was asked to connect, in order. */
  const connects: Array<string> = [];
  const script: {
    clone: (session: number) => Answer;
    cancel: (session: number) => Answer;
    /** Targets whose connection attempt never finishes, so they never deliver a session. */
    hangConnect: (label: string) => boolean;
  } = {
    clone: () => Effect.never,
    cancel: () => Effect.never,
    hangConnect: () => false,
  };
  let sessions = 0;
  const driver = ConnectionDriver.ConnectionDriver.of({
    connect: (entry, reportProgress) =>
      Effect.gen(function* () {
        const target = entry.target;
        connects.push(target.label);
        if (script.hangConnect(target.label)) {
          return yield* Effect.never;
        }
        const prepared: PreparedConnection = {
          environmentId: target.environmentId,
          label: target.label,
          descriptor: {
            environmentId: target.environmentId,
            label: target.label,
            platform: { os: "linux", arch: "x64" },
            serverVersion: "0.0.0-test",
            storageInstanceId: "store-test",
            bootId: null,
            remoteUpdateSupport: null,
            remoteProtocolVersion: 1,
            minCompatibleRemoteProtocol: 1,
            capabilities: makeTestExecutionEnvironmentCapabilities({ vcsCloneReattach: true }),
          },
          httpBaseUrl: "https://clone.example.test",
          socketUrl: "wss://clone.example.test/ws",
          httpAuthorization: null,
          e2ee: null,
          target,
        };
        yield* reportProgress({ stage: "preparing" });
        yield* reportProgress({ stage: "opening", prepared });
        sessions += 1;
        const number = sessions;
        const session: RpcSession = {
          client: {
            [WS_METHODS.vcsClone]: (input: unknown) =>
              Effect.suspend(() => {
                calls.clone.push({ session: number, input });
                return script.clone(number);
              }),
            [WS_METHODS.vcsCancelClone]: (input: unknown) =>
              Effect.suspend(() => {
                calls.cancel.push({ session: number, input });
                return script.cancel(number);
              }),
          } as unknown as WsRpcProtocolClient,
          initialConfig: Effect.succeed({
            environment: { capabilities: { vcsCloneReattach: true } },
          } as never),
          ready: Effect.void,
          probe: Effect.void,
          closed: Effect.never,
          e2eeAuthenticated: Effect.succeed(null),
        };
        yield* reportProgress({ stage: "synchronizing", prepared });
        return { prepared, session };
      }),
  });
  const layer = EnvironmentRegistry.layer.pipe(
    Layer.provide(
      Layer.mergeAll(
        Layer.succeed(
          Persistence.ConnectionTargetStore,
          Persistence.ConnectionTargetStore.of({ list: Effect.succeed([]) }),
        ),
        Layer.succeed(
          Persistence.ConnectionRegistrationStore,
          Persistence.ConnectionRegistrationStore.of({
            register: unused("register"),
            removeIfMatching: unused("removeIfMatching"),
            remove: unused("remove"),
            relabel: unused("relabel"),
          }),
        ),
        Layer.succeed(
          Persistence.AcceptedStorageIdentityStore,
          Persistence.AcceptedStorageIdentityStore.of({
            get: unused("get"),
            accept: unused("accept"),
            rollbackAcceptance: unused("rollbackAcceptance"),
            transition: unused("transition"),
          }),
        ),
        Layer.succeed(
          Persistence.EnvironmentCacheStore,
          Persistence.EnvironmentCacheStore.of({
            loadShell: () => Effect.succeed(Option.none()),
            saveShell: unused("saveShell"),
            loadThread: () => Effect.succeed(Option.none()),
            saveThread: unused("saveThread"),
            removeThread: unused("removeThread"),
            clear: () => Effect.void,
          }),
        ),
        Layer.succeed(
          ConnectionProfileStore.ConnectionProfileStore,
          ConnectionProfileStore.ConnectionProfileStore.of({
            get: () => Effect.succeed(Option.none()),
            put: unused("profile put"),
            remove: unused("profile remove"),
          }),
        ),
        Layer.succeed(
          ConnectionCredentialStore.ConnectionCredentialStore,
          ConnectionCredentialStore.ConnectionCredentialStore.of({
            get: () => Effect.succeed(Option.none()),
            put: unused("credential put"),
            remove: () => Effect.void,
            putIfSaved: unused("putIfSaved"),
          }),
        ),
        Layer.succeed(
          ClientCapabilities.SshEnvironmentGateway,
          ClientCapabilities.SshEnvironmentGateway.of({
            provision: unused("provision"),
            ensureTunnel: unused("ensureTunnel"),
            mintBearer: unused("mintBearer"),
            disconnect: unused("disconnect"),
          }),
        ),
        Layer.succeed(
          Connectivity.Connectivity,
          Connectivity.Connectivity.of({ status: Effect.succeed("online"), changes: Stream.never }),
        ),
        Layer.succeed(
          ConnectionWakeups.ConnectionWakeups,
          ConnectionWakeups.ConnectionWakeups.of({
            focusVisibility: Stream.never,
            changes: Stream.never,
          }),
        ),
        Layer.succeed(ConnectionDriver.ConnectionDriver, driver),
      ),
    ),
  );
  return { layer, calls, connects, script };
}

function awaitConnected(registry: EnvironmentRegistry.EnvironmentRegistry["Service"]) {
  const connected = (state: SupervisorConnectionState) => state.phase === "connected";
  return registry
    .stateChanges(ENVIRONMENT_ID)
    .pipe(Stream.filter(connected), Stream.runHead, Effect.map(Option.getOrThrow));
}

function waitFor(predicate: () => boolean, message: string) {
  return Effect.gen(function* () {
    for (let attempt = 0; attempt < 2_000; attempt += 1) {
      if (predicate()) return;
      yield* Effect.yieldNow;
    }
    return yield* Effect.die(message);
  });
}

function failureOf(result: AtomCommandResult<unknown, unknown>): unknown {
  return result._tag === "Failure" ? Cause.squash(result.cause) : null;
}

function valueOf(result: AtomCommandResult<unknown, unknown>): unknown {
  return AsyncResult.isSuccess(result) ? result.value : null;
}

/** The registry's service and the commands, as the web app builds them. */
const commandsFor = Effect.fn("TestVcsCloneRegistry.commandsFor")(function* () {
  const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
  const vcs = createVcsEnvironmentAtoms(
    Atom.runtime(Layer.succeed(EnvironmentRegistry.EnvironmentRegistry, registry)),
  );
  return { registry, vcs, atomRegistry: AtomRegistry.make() };
});

describe("clone commands follow the registry's current supervisor", () => {
  it.effect(
    "re-attach on the replacement supervisor's session after the registry replaces it",
    () =>
      Effect.gen(function* () {
        const harness = makeHarness();
        const CLONE = cloneOf("replaced");
        harness.script.clone = (session) =>
          session === 1 ? Effect.fail(LOST_SOCKET) : Effect.succeed({ path: "/code/demo" });

        yield* Effect.gen(function* () {
          const { registry, vcs, atomRegistry } = yield* commandsFor();
          yield* registry.registerPlatform(new PrimaryConnectionRegistration({ target: TARGET }));
          yield* awaitConnected(registry);
          const progress: Array<VcsCloneProgress> = [];
          const clone = runAtomCommand(atomRegistry, vcs.clone, {
            environmentId: ENVIRONMENT_ID,
            input: { ...CLONE, onProgress: (next) => progress.push(next) },
          });
          yield* waitFor(() => progress.at(-1)?.phase === "reconnecting", "not reconnecting");

          // A changed platform registration: the registry retires the supervisor the clone
          // started on and installs a new one with a new session.
          yield* registry.reconcilePlatform([
            new PrimaryConnectionRegistration({ target: MOVED_TARGET }),
          ]);

          expect(valueOf(yield* Effect.promise(() => clone))).toEqual({ path: "/code/demo" });
          expect(harness.calls.clone).toEqual([
            { session: 1, input: { ...CLONE, detach: true } },
            { session: 2, input: { ...CLONE, detach: true, attach: true } },
          ]);
          atomRegistry.dispose();
        }).pipe(Effect.provide(harness.layer), Effect.scoped);
      }),
    TEST_TIMEOUT_MS,
  );

  it.effect(
    "send a pending cancel on the replacement supervisor and release its record",
    () =>
      Effect.gen(function* () {
        const harness = makeHarness();
        const CLONE = cloneOf("cancel-replaced");
        harness.script.cancel = (session) =>
          session === 1 ? Effect.fail(LOST_SOCKET) : Effect.succeed({ cancelled: true });
        harness.script.clone = () => Effect.succeed({ path: "/code/demo" });

        yield* Effect.gen(function* () {
          const { registry, vcs, atomRegistry } = yield* commandsFor();
          yield* registry.registerPlatform(new PrimaryConnectionRegistration({ target: TARGET }));
          yield* awaitConnected(registry);
          const cancel = runAtomCommand(atomRegistry, vcs.cancelClone, {
            environmentId: ENVIRONMENT_ID,
            input: CLONE,
          });
          yield* waitFor(() => harness.calls.cancel.length === 1, "the cancel was not sent");

          yield* registry.reconcilePlatform([
            new PrimaryConnectionRegistration({ target: MOVED_TARGET }),
          ]);

          expect(valueOf(yield* Effect.promise(() => cancel))).toEqual({ cancelled: true });
          expect(harness.calls.cancel.map((call) => call.session)).toEqual([1, 2]);
          // The settled cancel released its record: a clone into that folder dispatches at once.
          const clone = yield* Effect.promise(() =>
            runAtomCommand(atomRegistry, vcs.clone, {
              environmentId: ENVIRONMENT_ID,
              input: CLONE,
            }),
          );
          expect(valueOf(clone)).toEqual({ path: "/code/demo" });
          expect(harness.calls.clone).toEqual([{ session: 2, input: { ...CLONE, detach: true } }]);
          atomRegistry.dispose();
        }).pipe(Effect.provide(harness.layer), Effect.scoped);
      }),
    TEST_TIMEOUT_MS,
  );

  it.effect(
    "stop both loops when the environment is removed, and release the cancel's record",
    () =>
      Effect.gen(function* () {
        const harness = makeHarness();
        const CLONE = cloneOf("removed");
        harness.script.clone = (session) =>
          session === 1 ? Effect.fail(LOST_SOCKET) : Effect.succeed({ path: "/code/demo" });
        harness.script.cancel = () => Effect.fail(LOST_SOCKET);

        yield* Effect.gen(function* () {
          const { registry, vcs, atomRegistry } = yield* commandsFor();
          yield* registry.registerPlatform(new PrimaryConnectionRegistration({ target: TARGET }));
          yield* awaitConnected(registry);
          const progress: Array<VcsCloneProgress> = [];
          const clone = runAtomCommand(
            atomRegistry,
            vcs.clone,
            {
              environmentId: ENVIRONMENT_ID,
              input: { ...CLONE, onProgress: (next) => progress.push(next) },
            },
            { reportFailure: false },
          );
          yield* waitFor(() => progress.at(-1)?.phase === "reconnecting", "not reconnecting");
          const cancel = runAtomCommand(
            atomRegistry,
            vcs.cancelClone,
            { environmentId: ENVIRONMENT_ID, input: CLONE },
            { reportFailure: false },
          );
          yield* waitFor(() => harness.calls.cancel.length === 1, "the cancel was not sent");

          // The host no longer reports this platform environment: the registry removes it.
          yield* registry.reconcilePlatform([]);

          for (const result of [
            yield* Effect.promise(() => clone),
            yield* Effect.promise(() => cancel),
          ]) {
            const error = failureOf(result);
            expect(error).toBeInstanceOf(VcsCloneStoppedError);
            expect((error as VcsCloneStoppedError).reason).toBe("environment-unavailable");
          }

          // The stopped cancel released its record: once the environment is back, a clone into
          // that folder dispatches at once on the new supervisor's session.
          yield* registry.registerPlatform(new PrimaryConnectionRegistration({ target: TARGET }));
          yield* awaitConnected(registry);
          const again = yield* Effect.promise(() =>
            runAtomCommand(atomRegistry, vcs.clone, {
              environmentId: ENVIRONMENT_ID,
              input: CLONE,
            }),
          );
          expect(valueOf(again)).toEqual({ path: "/code/demo" });
          expect(harness.calls.clone.map((call) => call.session)).toEqual([1, 2]);
          atomRegistry.dispose();
        }).pipe(Effect.provide(harness.layer), Effect.scoped);
      }),
    TEST_TIMEOUT_MS,
  );

  it.effect(
    "name the environment by its latest label when it is removed after a replacement",
    () =>
      Effect.gen(function* () {
        const harness = makeHarness();
        const CLONE = cloneOf("replaced-then-removed");
        harness.script.clone = () => Effect.fail(LOST_SOCKET);
        harness.script.cancel = () => Effect.fail(LOST_SOCKET);
        // The replacement never connects, so neither loop ever takes a session from it.
        harness.script.hangConnect = (label) => label === MOVED_TARGET.label;

        yield* Effect.gen(function* () {
          const { registry, vcs, atomRegistry } = yield* commandsFor();
          yield* registry.registerPlatform(new PrimaryConnectionRegistration({ target: TARGET }));
          yield* awaitConnected(registry);
          const progress: Array<VcsCloneProgress> = [];
          const clone = runAtomCommand(
            atomRegistry,
            vcs.clone,
            {
              environmentId: ENVIRONMENT_ID,
              input: { ...CLONE, onProgress: (next) => progress.push(next) },
            },
            { reportFailure: false },
          );
          yield* waitFor(() => progress.at(-1)?.phase === "reconnecting", "not reconnecting");
          const cancel = runAtomCommand(
            atomRegistry,
            vcs.cancelClone,
            { environmentId: ENVIRONMENT_ID, input: CLONE },
            { reportFailure: false },
          );
          yield* waitFor(() => harness.calls.cancel.length === 1, "the cancel was not sent");

          yield* registry.reconcilePlatform([
            new PrimaryConnectionRegistration({ target: MOVED_TARGET }),
          ]);
          yield* waitFor(
            () => harness.connects.includes(MOVED_TARGET.label),
            "the replacement never tried to connect",
          );
          yield* registry.reconcilePlatform([]);

          for (const result of [
            yield* Effect.promise(() => clone),
            yield* Effect.promise(() => cancel),
          ]) {
            const error = failureOf(result);
            expect(error).toBeInstanceOf(VcsCloneStoppedError);
            expect((error as VcsCloneStoppedError).message).toBe(
              `Can't reconnect to ${MOVED_TARGET.label}.`,
            );
          }
          atomRegistry.dispose();
        }).pipe(Effect.provide(harness.layer), Effect.scoped);
      }),
    TEST_TIMEOUT_MS,
  );

  for (const [label, disconnectFirst] of [
    ["disconnect, then replacement", true],
    ["replacement, then disconnect", false],
  ] as const) {
    it.effect(
      `stop both loops when the user disconnects as the registry replaces the supervisor (${label})`,
      () =>
        Effect.gen(function* () {
          const harness = makeHarness();
          const CLONE = cloneOf(`disconnect-${disconnectFirst ? "first" : "last"}`);
          harness.script.clone = () => Effect.fail(LOST_SOCKET);
          harness.script.cancel = () => Effect.fail(LOST_SOCKET);
          // The replacement never connects, so neither loop ever takes a session from it.
          harness.script.hangConnect = (label) => label === MOVED_TARGET.label;

          yield* Effect.gen(function* () {
            const { registry, vcs, atomRegistry } = yield* commandsFor();
            yield* registry.registerPlatform(new PrimaryConnectionRegistration({ target: TARGET }));
            yield* awaitConnected(registry);
            const progress: Array<VcsCloneProgress> = [];
            const clone = runAtomCommand(
              atomRegistry,
              vcs.clone,
              {
                environmentId: ENVIRONMENT_ID,
                input: { ...CLONE, onProgress: (next) => progress.push(next) },
              },
              { reportFailure: false },
            );
            yield* waitFor(() => progress.at(-1)?.phase === "reconnecting", "not reconnecting");
            const cancel = runAtomCommand(
              atomRegistry,
              vcs.cancelClone,
              { environmentId: ENVIRONMENT_ID, input: CLONE },
              { reportFailure: false },
            );
            yield* waitFor(() => harness.calls.cancel.length === 1, "the cancel was not sent");

            // Back to back in this fiber, so neither supervisor's loop runs in between.
            const disconnect = registry.disconnect(ENVIRONMENT_ID);
            const replace = registry.reconcilePlatform([
              new PrimaryConnectionRegistration({ target: MOVED_TARGET }),
            ]);
            if (disconnectFirst) {
              yield* disconnect;
              yield* replace;
            } else {
              yield* replace;
              yield* disconnect;
            }

            for (const result of [
              yield* Effect.promise(() => clone),
              yield* Effect.promise(() => cancel),
            ]) {
              const error = failureOf(result);
              expect(error).toBeInstanceOf(VcsCloneStoppedError);
              expect((error as VcsCloneStoppedError).message).toBe(
                `Can't reconnect to ${MOVED_TARGET.label}.`,
              );
            }
            atomRegistry.dispose();
          }).pipe(Effect.provide(harness.layer), Effect.scoped);
        }),
      TEST_TIMEOUT_MS,
    );
  }

  it.effect(
    "dispatch on the replacement supervisor after waiting behind a pending cancel",
    () =>
      Effect.gen(function* () {
        const harness = makeHarness();
        const CLONE = cloneOf("gate-replaced");
        harness.script.clone = () => Effect.succeed({ path: "/code/demo" });

        yield* Effect.gen(function* () {
          const { registry, vcs, atomRegistry } = yield* commandsFor();
          yield* registry.registerPlatform(new PrimaryConnectionRegistration({ target: TARGET }));
          yield* awaitConnected(registry);
          // A cancel of this URL into this leaf is still pending, so the clone waits for it.
          const release = registerCloneCancel(ENVIRONMENT_ID, CLONE);
          const clone = runAtomCommand(
            atomRegistry,
            vcs.clone,
            { environmentId: ENVIRONMENT_ID, input: CLONE },
            { reportFailure: false },
          );
          for (let attempt = 0; attempt < 50; attempt += 1) yield* Effect.yieldNow;
          expect(harness.calls.clone).toEqual([]);

          // The registry replaces the supervisor during that wait, and the new one connects.
          yield* registry.reconcilePlatform([
            new PrimaryConnectionRegistration({ target: MOVED_TARGET }),
          ]);
          yield* awaitConnected(registry);
          release();

          const result = yield* Effect.promise(() => clone);
          expect(failureOf(result)).toBeNull();
          expect(valueOf(result)).toEqual({ path: "/code/demo" });
          expect(harness.calls.clone).toEqual([{ session: 2, input: { ...CLONE, detach: true } }]);
          atomRegistry.dispose();
        }).pipe(Effect.provide(harness.layer), Effect.scoped);
      }),
    TEST_TIMEOUT_MS,
  );

  it.effect(
    "stop a clone waiting behind a pending cancel when the environment is removed",
    () =>
      Effect.gen(function* () {
        const harness = makeHarness();
        const CLONE = cloneOf("gate-removed");
        harness.script.clone = () => Effect.succeed({ path: "/code/demo" });

        yield* Effect.gen(function* () {
          const { registry, vcs, atomRegistry } = yield* commandsFor();
          yield* registry.registerPlatform(new PrimaryConnectionRegistration({ target: TARGET }));
          yield* awaitConnected(registry);
          const release = registerCloneCancel(ENVIRONMENT_ID, CLONE);
          const clone = runAtomCommand(
            atomRegistry,
            vcs.clone,
            { environmentId: ENVIRONMENT_ID, input: CLONE },
            { reportFailure: false },
          );
          for (let attempt = 0; attempt < 50; attempt += 1) yield* Effect.yieldNow;

          yield* registry.reconcilePlatform([]);
          release();

          const error = failureOf(yield* Effect.promise(() => clone));
          expect(error).toBeInstanceOf(VcsCloneStoppedError);
          expect((error as VcsCloneStoppedError).reason).toBe("environment-unavailable");
          expect(harness.calls.clone).toEqual([]);
          atomRegistry.dispose();
        }).pipe(Effect.provide(harness.layer), Effect.scoped);
      }),
    TEST_TIMEOUT_MS,
  );
});
