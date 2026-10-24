import { EnvironmentId, GitCloneOperationError, WS_METHODS } from "@bibcode/contracts";
import { describe, expect, it } from "@effect/vitest";
import * as Cause from "effect/Cause";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as Fiber from "effect/Fiber";
import * as Option from "effect/Option";
import * as Stream from "effect/Stream";
import * as SubscriptionRef from "effect/SubscriptionRef";
import { RpcClientError } from "effect/rpc";

import {
  AVAILABLE_CONNECTION_STATE,
  ConnectionTransientError,
  type SupervisorConnectionState,
} from "../connection/model.ts";
import type { ConnectionCatalogEntry } from "../connection/catalog.ts";
import { EnvironmentRegistry } from "../connection/registry.ts";
import { EnvironmentSupervisor } from "../connection/supervisor.ts";
import { EnvironmentRpcUnavailableError } from "../rpc/client.ts";
import type { WsRpcProtocolClient } from "../rpc/protocol.ts";
import type { RpcSession } from "../rpc/session.ts";
import {
  cancelCloneOnNextSession,
  pendingCloneCancelKey,
  cloneWithReattach,
  registerCloneCancel,
  type VcsCloneProgress,
  VcsCloneStoppedError,
} from "./vcsClone.ts";

const ENVIRONMENT_ID = EnvironmentId.make("environment-1");
const CLONE = { url: "https://example.test/demo.git", parentDir: "/code" } as const;
const CONNECTED: SupervisorConnectionState = {
  ...AVAILABLE_CONNECTION_STATE,
  desired: true,
  phase: "connected",
};
const LOST_SOCKET = new RpcClientError.RpcClientError({
  reason: new RpcClientError.RpcClientDefect({
    message: "socket closed",
    cause: new Error("socket closed"),
  }),
});

/** What a scripted RPC method answers: a transport loss, a typed failure, or a value. */
type Answer = Effect.Effect<
  unknown,
  RpcClientError.RpcClientError | EnvironmentRpcUnavailableError | GitCloneOperationError
>;

function session(
  capable: boolean | undefined,
  methods: {
    readonly clone?: (input: unknown) => Answer;
    readonly cancel?: (input: unknown) => Answer;
  },
): RpcSession {
  const capabilities = capable === undefined ? {} : { vcsCloneReattach: capable };
  return {
    client: {
      [WS_METHODS.vcsClone]: methods.clone ?? (() => Effect.never),
      [WS_METHODS.vcsCancelClone]: methods.cancel ?? (() => Effect.never),
    } as unknown as WsRpcProtocolClient,
    initialConfig: Effect.succeed({ environment: { capabilities } } as never),
    ready: Effect.void,
    probe: Effect.void,
    closed: Effect.never,
    e2eeAuthenticated: Effect.succeed(null),
  };
}

const makeSupervisor = Effect.fn("TestVcsClone.makeSupervisor")(function* (
  initial: RpcSession | null,
) {
  const sessionRef = yield* SubscriptionRef.make<Option.Option<RpcSession>>(
    initial === null ? Option.none() : Option.some(initial),
  );
  const stateRef = yield* SubscriptionRef.make<SupervisorConnectionState>(CONNECTED);
  const target = { environmentId: ENVIRONMENT_ID, label: "Remote" };
  const supervisor = EnvironmentSupervisor.of({
    target,
    session: sessionRef,
    state: stateRef,
  } as never);
  // One fixed supervisor: these tests cover the loops themselves. The registry replacing the
  // supervisor, or removing the environment, is covered by vcsClone.registry.test.ts.
  const entries = yield* SubscriptionRef.make<ReadonlyMap<EnvironmentId, ConnectionCatalogEntry>>(
    new Map([[ENVIRONMENT_ID, { target } as ConnectionCatalogEntry]]),
  );
  const followStream: EnvironmentRegistry["Service"]["followStream"] = (_environmentId, stream) =>
    Stream.provideService(stream, EnvironmentSupervisor, supervisor);
  const run: EnvironmentRegistry["Service"]["run"] = (_environmentId, effect) =>
    Effect.provideService(effect, EnvironmentSupervisor, supervisor);
  const registry = EnvironmentRegistry.of({ entries, followStream, run } as never);
  const provide = <A, E>(
    effect: Effect.Effect<A, E, EnvironmentSupervisor | EnvironmentRegistry>,
  ): Effect.Effect<A, E> =>
    effect.pipe(
      Effect.provideService(EnvironmentSupervisor, supervisor),
      Effect.provideService(EnvironmentRegistry, registry),
    );
  return { provide, sessionRef, stateRef };
});

function waitFor(predicate: () => boolean, message: string) {
  return Effect.gen(function* () {
    for (let attempt = 0; attempt < 200; attempt += 1) {
      if (predicate()) return;
      yield* Effect.yieldNow;
    }
    return yield* Effect.die(message);
  });
}

function failureOf<A, E>(exit: Exit.Exit<A, E>): unknown {
  return Exit.isFailure(exit) ? Cause.squash(exit.cause) : null;
}

describe("cloneWithReattach", () => {
  it.effect("sends today's single call without fields when the server lacks the capability", () =>
    Effect.gen(function* () {
      const inputs: Array<unknown> = [];
      const { provide } = yield* makeSupervisor(
        session(undefined, {
          clone: (input) => {
            inputs.push(input);
            return Effect.succeed({ path: "/code/demo" });
          },
        }),
      );
      const progress: Array<VcsCloneProgress> = [];
      const result = yield* cloneWithReattach({
        ...CLONE,
        onProgress: (next) => progress.push(next),
      }).pipe(provide);

      expect(result).toEqual({ path: "/code/demo" });
      expect(inputs).toEqual([CLONE]);
      expect(progress).toEqual([{ phase: "cloning", reattach: false }]);
    }),
  );

  it.effect("keeps today's failure on a lost socket without the capability", () =>
    Effect.gen(function* () {
      let calls = 0;
      const { provide } = yield* makeSupervisor(
        session(false, {
          clone: () => {
            calls += 1;
            return Effect.fail(LOST_SOCKET);
          },
        }),
      );
      const exit = yield* Effect.exit(cloneWithReattach(CLONE).pipe(provide));
      expect(failureOf(exit)).toBe(LOST_SOCKET);
      expect(calls).toBe(1);
    }),
  );

  it.effect("fails at once when no session exists, as today", () =>
    Effect.gen(function* () {
      const { provide } = yield* makeSupervisor(null);
      const exit = yield* Effect.exit(cloneWithReattach(CLONE).pipe(provide));
      expect(failureOf(exit)).toBeInstanceOf(EnvironmentRpcUnavailableError);
    }),
  );

  it.effect("re-attaches on the next session after every kind of transport loss", () =>
    Effect.gen(function* () {
      const inputs: Array<unknown> = [];
      const failing = (failure: Answer) =>
        session(true, {
          clone: (input) => {
            inputs.push(input);
            return failure;
          },
        });
      const first = failing(Effect.fail(LOST_SOCKET));
      const later = [
        // Effect's RPC client resumes a pending call with an interrupt when its socket closes.
        failing(Effect.failCause(Cause.interrupt())),
        failing(
          Effect.fail(
            new EnvironmentRpcUnavailableError({
              environmentId: ENVIRONMENT_ID,
              message: "Remote is not connected.",
            }),
          ),
        ),
        session(true, {
          clone: (input) => {
            inputs.push(input);
            return Effect.succeed({ path: "/code/demo" });
          },
        }),
      ];
      const { provide, sessionRef } = yield* makeSupervisor(first);
      const progress: Array<VcsCloneProgress> = [];
      const fiber = yield* cloneWithReattach({
        ...CLONE,
        onProgress: (next) => progress.push(next),
      }).pipe(provide, Effect.forkChild);

      for (let index = 0; index < later.length; index += 1) {
        yield* waitFor(
          () => inputs.length === index + 1 && progress.at(-1)?.phase === "reconnecting",
          `attempt ${index + 1} did not start reconnecting`,
        );
        yield* SubscriptionRef.set(sessionRef, Option.some(later[index]!));
      }

      expect(yield* Fiber.join(fiber)).toEqual({ path: "/code/demo" });
      expect(inputs).toEqual([
        { ...CLONE, detach: true },
        { ...CLONE, detach: true, attach: true },
        { ...CLONE, detach: true, attach: true },
        { ...CLONE, detach: true, attach: true },
      ]);
      expect(progress.map((entry) => entry.phase)).toEqual([
        "cloning",
        "reconnecting",
        "cloning",
        "reconnecting",
        "cloning",
        "reconnecting",
        "cloning",
      ]);
      expect(progress.every((entry) => entry.reattach)).toBe(true);
    }),
  );

  it.effect("returns a typed failure without re-attaching", () =>
    Effect.gen(function* () {
      const busy = new GitCloneOperationError({
        reason: "busy",
        destination: "/code/demo",
        message: "Another clone into /code/demo is in progress.",
      });
      let calls = 0;
      const { provide } = yield* makeSupervisor(
        session(true, {
          clone: () => {
            calls += 1;
            return Effect.fail(busy);
          },
        }),
      );
      const exit = yield* Effect.exit(cloneWithReattach(CLONE).pipe(provide));
      expect(failureOf(exit)).toBe(busy);
      expect(calls).toBe(1);
    }),
  );

  it.effect("ends without re-attaching when its own fiber is interrupted", () =>
    Effect.gen(function* () {
      let calls = 0;
      const { provide } = yield* makeSupervisor(
        session(true, {
          clone: () => {
            calls += 1;
            return Effect.never;
          },
        }),
      );
      const progress: Array<VcsCloneProgress> = [];
      const fiber = yield* cloneWithReattach({
        ...CLONE,
        onProgress: (next) => progress.push(next),
      }).pipe(provide, Effect.forkChild);
      yield* waitFor(() => calls === 1, "the clone call did not start");
      yield* Fiber.interrupt(fiber);
      expect(progress).toEqual([{ phase: "cloning", reattach: true }]);
      expect(calls).toBe(1);
    }),
  );

  for (const [label, stopped] of [
    ["blocked", { ...CONNECTED, phase: "blocked" }],
    ["disconnected by the user", AVAILABLE_CONNECTION_STATE],
  ] as const) {
    it.effect(`stops re-attaching when the environment is ${label}`, () =>
      Effect.gen(function* () {
        const { provide, sessionRef, stateRef } = yield* makeSupervisor(
          session(true, { clone: () => Effect.fail(LOST_SOCKET) }),
        );
        const progress: Array<VcsCloneProgress> = [];
        const fiber = yield* cloneWithReattach({
          ...CLONE,
          onProgress: (next) => progress.push(next),
        }).pipe(provide, Effect.forkChild);
        yield* waitFor(() => progress.at(-1)?.phase === "reconnecting", "not reconnecting");
        yield* SubscriptionRef.set(sessionRef, Option.none());
        yield* SubscriptionRef.set(stateRef, stopped as SupervisorConnectionState);

        const error = failureOf(yield* Effect.exit(Fiber.join(fiber)));
        expect(error).toBeInstanceOf(VcsCloneStoppedError);
        expect((error as VcsCloneStoppedError).reason).toBe("environment-unavailable");
      }),
    );
  }

  it.effect("reports a session without the capability as stopped and starts nothing there", () =>
    Effect.gen(function* () {
      let legacyCalls = 0;
      const { provide, sessionRef } = yield* makeSupervisor(
        session(true, { clone: () => Effect.fail(LOST_SOCKET) }),
      );
      const progress: Array<VcsCloneProgress> = [];
      const fiber = yield* cloneWithReattach({
        ...CLONE,
        onProgress: (next) => progress.push(next),
      }).pipe(provide, Effect.forkChild);
      yield* waitFor(() => progress.at(-1)?.phase === "reconnecting", "not reconnecting");
      yield* SubscriptionRef.set(
        sessionRef,
        Option.some(
          session(false, {
            clone: () => {
              legacyCalls += 1;
              return Effect.succeed({ path: "/code/demo" });
            },
          }),
        ),
      );

      const error = failureOf(yield* Effect.exit(Fiber.join(fiber)));
      expect(error).toBeInstanceOf(VcsCloneStoppedError);
      expect((error as VcsCloneStoppedError).reason).toBe("reattach-unsupported");
      expect(legacyCalls).toBe(0);
    }),
  );
});

describe("pending cancel keys", () => {
  it("ignore the parent folder's spelling and keep the exact URL and the leaf", () => {
    const url = "https://example.test/org/demo.git";
    const key = pendingCloneCancelKey("env", { url, parentDir: "/code" });
    for (const parentDir of ["/code/", "/code/.", "C:\\code", "C:/code", "c:\\code", "~/code"]) {
      expect(pendingCloneCancelKey("env", { url, parentDir })).toBe(key);
    }
    // The leaf follows the server's rule: `directoryName`, else the URL's last segment.
    expect(
      pendingCloneCancelKey("env", { url, parentDir: "/elsewhere", directoryName: "demo" }),
    ).toBe(key);
    // The server cancels only a clone of the exact URL, so other URLs and leaves never share it.
    expect(pendingCloneCancelKey("env", { url: `${url}/`, parentDir: "/code" })).not.toBe(key);
    expect(
      pendingCloneCancelKey("env", {
        url: "https://example.test/other.git",
        parentDir: "/code",
        directoryName: "demo",
      }),
    ).not.toBe(key);
    expect(
      pendingCloneCancelKey("env", { url, parentDir: "/code", directoryName: "custom" }),
    ).not.toBe(key);
    expect(pendingCloneCancelKey("other-env", { url, parentDir: "/code" })).not.toBe(key);
  });
});

describe("pending clone cancels", () => {
  it.effect("hold a clone into the same folder until every earlier cancel has settled", () =>
    Effect.gen(function* () {
      const inputs: Array<unknown> = [];
      const { provide } = yield* makeSupervisor(
        session(true, {
          clone: (input) => {
            inputs.push(input);
            return Effect.succeed({ path: "/code/demo" });
          },
        }),
      );
      const releaseFirst = registerCloneCancel(ENVIRONMENT_ID, CLONE);
      const releaseSecond = registerCloneCancel(ENVIRONMENT_ID, { ...CLONE, parentDir: "/code/" });
      const releaseElsewhere = registerCloneCancel(ENVIRONMENT_ID, {
        ...CLONE,
        directoryName: "elsewhere",
      });
      const fiber = yield* cloneWithReattach(CLONE).pipe(provide, Effect.forkChild);
      for (let attempt = 0; attempt < 20; attempt += 1) yield* Effect.yieldNow;
      expect(inputs).toEqual([]);

      releaseFirst();
      for (let attempt = 0; attempt < 20; attempt += 1) yield* Effect.yieldNow;
      expect(inputs).toEqual([]);

      releaseSecond();
      expect(yield* Fiber.join(fiber)).toEqual({ path: "/code/demo" });
      expect(inputs).toEqual([{ ...CLONE, detach: true }]);
      // A cancel for another folder never held this clone.
      releaseElsewhere();
    }),
  );

  it.effect("hold a clone into any spelling of the parent folder, but not another URL", () =>
    Effect.gen(function* () {
      const inputs: Array<unknown> = [];
      const { provide } = yield* makeSupervisor(
        session(true, {
          clone: (input) => {
            inputs.push(input);
            return Effect.succeed({ path: "/code/demo" });
          },
        }),
      );
      // Only the server can canonicalize a host path, so every spelling of the parent is held.
      const release = registerCloneCancel(ENVIRONMENT_ID, { ...CLONE, parentDir: "/code" });
      const aliases = ["/code/", "/code/.", "C:\\code", "C:/code", "~/code"];
      const fibers = yield* Effect.forEach(aliases, (parentDir) =>
        cloneWithReattach({ ...CLONE, parentDir }).pipe(provide, Effect.forkChild),
      );
      for (let attempt = 0; attempt < 20; attempt += 1) yield* Effect.yieldNow;
      expect(inputs).toEqual([]);

      // A cancel only ever stops a clone of its exact URL, so another URL is not held.
      const other = {
        url: "https://example.test/other.git",
        parentDir: "/code",
        directoryName: "demo",
      };
      expect(yield* cloneWithReattach(other).pipe(provide)).toEqual({ path: "/code/demo" });
      expect(inputs).toEqual([{ ...other, detach: true }]);

      release();
      yield* Effect.forEach(fibers, Fiber.join);
      expect(inputs).toHaveLength(1 + aliases.length);
    }),
  );
});

describe("cancelCloneOnNextSession", () => {
  it.effect("waits for a session and re-sends after a lost socket", () =>
    Effect.gen(function* () {
      const inputs: Array<unknown> = [];
      const { provide, sessionRef } = yield* makeSupervisor(null);
      const fiber = yield* cancelCloneOnNextSession(CLONE).pipe(provide, Effect.forkChild);
      for (let attempt = 0; attempt < 20; attempt += 1) yield* Effect.yieldNow;
      expect(inputs).toEqual([]);

      yield* SubscriptionRef.set(
        sessionRef,
        Option.some(
          session(true, {
            cancel: (input) => {
              inputs.push(input);
              return Effect.fail(LOST_SOCKET);
            },
          }),
        ),
      );
      yield* waitFor(() => inputs.length === 1, "the cancel did not go out");
      yield* SubscriptionRef.set(
        sessionRef,
        Option.some(
          session(true, {
            cancel: (input) => {
              inputs.push(input);
              return Effect.succeed({ cancelled: true });
            },
          }),
        ),
      );

      expect(yield* Fiber.join(fiber)).toEqual({ cancelled: true });
      expect(inputs).toEqual([CLONE, CLONE]);
    }),
  );

  it.effect("stops when the environment is blocked", () =>
    Effect.gen(function* () {
      const { provide, stateRef } = yield* makeSupervisor(null);
      const fiber = yield* cancelCloneOnNextSession(CLONE).pipe(provide, Effect.forkChild);
      const blocked: SupervisorConnectionState = { ...CONNECTED, phase: "blocked" };
      yield* SubscriptionRef.set(stateRef, blocked);
      const error = failureOf(yield* Effect.exit(Fiber.join(fiber)));
      expect(error).toBeInstanceOf(VcsCloneStoppedError);
      expect((error as VcsCloneStoppedError).reason).toBe("environment-unavailable");
    }),
  );

  it.effect("reports a host without the capability as stopped instead of sending", () =>
    Effect.gen(function* () {
      let sent = 0;
      const { provide, sessionRef } = yield* makeSupervisor(null);
      const fiber = yield* cancelCloneOnNextSession(CLONE).pipe(provide, Effect.forkChild);
      // The host came back as a build without re-attach: its restart already ended the clone.
      yield* SubscriptionRef.set(
        sessionRef,
        Option.some(
          session(false, {
            cancel: () => {
              sent += 1;
              return Effect.succeed({ cancelled: true });
            },
          }),
        ),
      );

      const error = failureOf(yield* Effect.exit(Fiber.join(fiber)));
      expect(error).toBeInstanceOf(VcsCloneStoppedError);
      expect((error as VcsCloneStoppedError).reason).toBe("reattach-unsupported");
      expect(sent).toBe(0);
    }),
  );

  it.effect("waits past a session whose configuration failed", () =>
    Effect.gen(function* () {
      const inputs: Array<unknown> = [];
      const broken: RpcSession = {
        ...session(true, {}),
        initialConfig: Effect.fail(
          new ConnectionTransientError({ reason: "transport", detail: "Remote dropped." }),
        ),
      };
      const { provide, sessionRef } = yield* makeSupervisor(broken);
      const fiber = yield* cancelCloneOnNextSession(CLONE).pipe(provide, Effect.forkChild);
      for (let attempt = 0; attempt < 20; attempt += 1) yield* Effect.yieldNow;
      yield* SubscriptionRef.set(
        sessionRef,
        Option.some(
          session(true, {
            cancel: (input) => {
              inputs.push(input);
              return Effect.succeed({ cancelled: false });
            },
          }),
        ),
      );

      expect(yield* Fiber.join(fiber)).toEqual({ cancelled: false });
      expect(inputs).toEqual([CLONE]);
    }),
  );
});
