import { describe, expect, it } from "@effect/vitest";
import {
  EnvironmentId,
  TerminalInputError,
  ThreadId,
  WS_METHODS,
  type ServerConfig,
  type TerminalMetadataStreamEvent,
  type TerminalSessionSnapshot,
} from "@coddy/contracts";
import { Deferred, Effect, Fiber, Layer, Option, Stream, SubscriptionRef } from "effect";
import { AsyncResult, Atom, AtomRegistry } from "effect/reactivity";
import { EnvironmentRegistry } from "../connection/registry.ts";
import { EnvironmentSupervisor } from "../connection/supervisor.ts";
import type { RpcSession } from "../rpc/session.ts";
import type { WsRpcProtocolClient } from "../rpc/protocol.ts";
import type { AtomCommand } from "./runtime.ts";
import { createTerminalEnvironmentAtoms } from "./terminal.ts";

const target = {
  environmentId: EnvironmentId.make("env"),
  input: { threadId: ThreadId.make("thread"), terminalId: "terminal" },
};
const lifecycleTarget = { ...target, input: { ...target.input, cwd: "/repo", cols: 80, rows: 24 } };
const snapshot: TerminalSessionSnapshot = {
  oscColorResponderActive: false,
  firstAttachmentGrant: false,
  ...target.input,
  cwd: "/repo",
  worktreePath: null,
  status: "running",
  pid: 1,
  history: "",
  exitCode: null,
  exitSignal: null,
  label: "Terminal",
  updatedAt: "2026-09-08T00:00:00.000Z",
};

// One delivered chunk, as the RPC client drains queued stream events together.
const metadataBatch: ReadonlyArray<TerminalMetadataStreamEvent> = [
  { type: "snapshot", terminals: [] },
  { type: "remove", threadId: "thread", terminalId: "closed" },
  {
    type: "upsert",
    terminal: {
      threadId: "thread",
      terminalId: "terminal",
      cwd: "/repo",
      worktreePath: null,
      status: "running",
      pid: 1,
      exitCode: null,
      exitSignal: null,
      hasRunningSubprocess: false,
      label: "Terminal",
      updatedAt: "2026-09-08T00:00:00.000Z",
      centerPanel: true,
    },
  },
];

const makeHarness = Effect.fn("terminalCommands.makeHarness")(function* (ordered: boolean) {
  const writes: string[] = [];
  const begins: number[] = [];
  const cancellations: string[] = [];
  let failLifecycle = false;
  const lifecycle = () =>
    failLifecycle
      ? Effect.fail(new TerminalInputError({ code: "closed", message: "Lifecycle failed." }))
      : Effect.succeed(snapshot);
  const session: RpcSession = {
    client: {
      [WS_METHODS.terminalOpen]: lifecycle,
      [WS_METHODS.terminalRestart]: lifecycle,
      [WS_METHODS.terminalClose]: () => Effect.void,
      [WS_METHODS.subscribeTerminalMetadata]: () => Stream.fromIterable(metadataBatch),
      [WS_METHODS.terminalBeginInput]: (input: { attachmentSequence: number }) =>
        Effect.sync(() => {
          begins.push(input.attachmentSequence);
          return { inputId: `lease-${input.attachmentSequence}` };
        }),
      [WS_METHODS.terminalCancelInput]: (input: { inputId: string }) =>
        Effect.sync(() => {
          cancellations.push(input.inputId);
        }),
      [WS_METHODS.terminalWrite]: (input: { data: string }) =>
        Effect.sync(() => {
          writes.push(input.data);
        }),
      [WS_METHODS.terminalWriteInput]: (input: {
        data: string;
        inputId: string;
        sequence: number;
      }) =>
        Effect.sync(() => {
          writes.push(input.data);
          return { inputId: input.inputId, sequence: input.sequence };
        }),
    } as unknown as WsRpcProtocolClient,
    initialConfig: Effect.succeed({
      environment: { capabilities: { terminalOrderedInput: ordered } },
    } as ServerConfig),
    ready: Effect.void,
    probe: Effect.void,
    closed: Effect.never,
    e2eeAuthenticated: Effect.succeed(null),
  };
  const activeSession = yield* SubscriptionRef.make(Option.some(session));
  const supervisor = EnvironmentSupervisor.of({
    target: { environmentId: target.environmentId },
    session: activeSession,
  } as never);
  const environmentRegistry = EnvironmentRegistry.of({
    run: <A, E, R>(_environmentId: EnvironmentId, effect: Effect.Effect<A, E, R>) =>
      Effect.provideService(effect, EnvironmentSupervisor, supervisor),
    followStream: <A, E, R>(_environmentId: EnvironmentId, stream: Stream.Stream<A, E, R>) =>
      Stream.provideService(stream, EnvironmentSupervisor, supervisor),
  } as never);
  const atoms = createTerminalEnvironmentAtoms(
    Atom.runtime(Layer.succeed(EnvironmentRegistry, environmentRegistry)),
  );
  const registry = AtomRegistry.make();
  const invoke = <I, A, E>(command: AtomCommand<I, A, E>, input: I) =>
    Effect.promise(() => command.run(registry, input));
  return {
    atoms,
    registry,
    invoke,
    writes,
    begins,
    cancellations,
    session,
    activeSession,
    fail: () => {
      failLifecycle = true;
    },
  };
});

for (const ordered of [false, true]) {
  describe(ordered ? "ordered lifecycle commands" : "legacy lifecycle commands", () => {
    it.effect("supports immediate open->write after close and preserves an idempotent open", () =>
      Effect.gen(function* () {
        const h = yield* makeHarness(ordered);
        expect(AsyncResult.isSuccess(yield* h.invoke(h.atoms.open, lifecycleTarget))).toBe(true);
        expect(
          AsyncResult.isSuccess(
            yield* h.invoke(h.atoms.write, {
              ...target,
              input: { ...target.input, data: "initial" },
            }),
          ),
        ).toBe(true);
        const initialBegins = h.begins.length;
        expect(AsyncResult.isSuccess(yield* h.invoke(h.atoms.open, lifecycleTarget))).toBe(true);
        expect(h.begins).toHaveLength(initialBegins);
        expect(AsyncResult.isSuccess(yield* h.invoke(h.atoms.close, target))).toBe(true);
        expect(AsyncResult.isSuccess(yield* h.invoke(h.atoms.open, lifecycleTarget))).toBe(true);
        expect(
          AsyncResult.isSuccess(
            yield* h.invoke(h.atoms.write, {
              ...target,
              input: { ...target.input, data: "after close" },
            }),
          ),
        ).toBe(true);
        yield* h.invoke(h.atoms.resetInput, target);
        expect(AsyncResult.isSuccess(yield* h.invoke(h.atoms.open, lifecycleTarget))).toBe(true);
        expect(
          AsyncResult.isSuccess(
            yield* h.invoke(h.atoms.write, {
              ...target,
              input: { ...target.input, data: "after reset" },
            }),
          ),
        ).toBe(true);
        expect(h.writes).toEqual(["initial", "after close", "after reset"]);
        expect(h.begins).toHaveLength(ordered ? 3 : 0);
        yield* h.invoke(h.atoms.resetInput, target);
        h.registry.dispose();
      }),
    );
    it.effect("supports immediate restart->write", () =>
      Effect.gen(function* () {
        const h = yield* makeHarness(ordered);
        expect(AsyncResult.isSuccess(yield* h.invoke(h.atoms.open, lifecycleTarget))).toBe(true);
        expect(AsyncResult.isSuccess(yield* h.invoke(h.atoms.restart, lifecycleTarget))).toBe(true);
        expect(h.cancellations).toHaveLength(ordered ? 1 : 0);
        expect(h.begins).toHaveLength(ordered ? 2 : 0);
        expect(
          AsyncResult.isSuccess(
            yield* h.invoke(h.atoms.write, {
              ...target,
              input: { ...target.input, data: "after restart" },
            }),
          ),
        ).toBe(true);
        expect(h.writes).toEqual(["after restart"]);
        yield* h.invoke(h.atoms.resetInput, target);
        h.registry.dispose();
      }),
    );
    it.effect(
      "keeps delayed open confirmation on its captured session without invalidating a replacement",
      () =>
        Effect.gen(function* () {
          const h = yield* makeHarness(ordered);
          const replacement = yield* makeHarness(ordered);
          const started = yield* Deferred.make<void>();
          const release = yield* Deferred.make<void>();
          (h.session.client as Record<string, unknown>)[WS_METHODS.terminalOpen] = () =>
            Deferred.succeed(started, undefined).pipe(
              Effect.andThen(Deferred.await(release)),
              Effect.as(snapshot),
            );
          const opening = yield* h.invoke(h.atoms.open, lifecycleTarget).pipe(Effect.forkChild);
          yield* Deferred.await(started);
          yield* SubscriptionRef.set(h.activeSession, Option.some(replacement.session));
          expect(AsyncResult.isSuccess(yield* h.invoke(h.atoms.prepareInput, target))).toBe(true);
          yield* Deferred.succeed(release, undefined);
          expect(AsyncResult.isFailure(yield* Fiber.join(opening))).toBe(true);
          expect(
            AsyncResult.isSuccess(
              yield* h.invoke(h.atoms.write, {
                ...target,
                input: { ...target.input, data: "new session" },
              }),
            ),
          ).toBe(true);
          expect(h.begins).toEqual([]);
          expect(h.writes).toEqual([]);
          expect(replacement.writes).toEqual(["new session"]);
          yield* h.invoke(h.atoms.resetInput, target);
          h.registry.dispose();
          replacement.registry.dispose();
        }),
    );
    it.effect("does not resume fenced input after failed open or restart", () =>
      Effect.gen(function* () {
        const h = yield* makeHarness(ordered);
        yield* h.invoke(h.atoms.resetInput, target);
        h.fail();
        expect(AsyncResult.isFailure(yield* h.invoke(h.atoms.open, lifecycleTarget))).toBe(true);
        expect(
          AsyncResult.isFailure(
            yield* h.invoke(h.atoms.write, {
              ...target,
              input: { ...target.input, data: "after failed open" },
            }),
          ),
        ).toBe(true);
        expect(AsyncResult.isFailure(yield* h.invoke(h.atoms.restart, lifecycleTarget))).toBe(true);
        expect(
          AsyncResult.isFailure(
            yield* h.invoke(h.atoms.write, {
              ...target,
              input: { ...target.input, data: "after failed restart" },
            }),
          ),
        ).toBe(true);
        expect(h.writes).toEqual([]);
        expect(h.begins).toEqual([]);
        h.registry.dispose();
      }),
    );
  });
}

describe("terminal metadata events", () => {
  it.effect("delivers every event of a batch, including removals", () =>
    Effect.gen(function* () {
      const h = yield* makeHarness(true);
      const received: TerminalMetadataStreamEvent[] = [];
      const done = yield* Deferred.make<void>();
      const unsubscribe = h.registry.subscribe(
        h.atoms.metadataEvents({ environmentId: target.environmentId, input: {} }),
        (result) => {
          if (!AsyncResult.isSuccess(result)) return;
          received.push(...result.value);
          if (received.length >= metadataBatch.length) Deferred.doneUnsafe(done, Effect.void);
        },
        { immediate: true },
      );
      yield* Deferred.await(done);
      unsubscribe();
      expect(received.map((event) => event.type)).toEqual(["snapshot", "remove", "upsert"]);
    }),
  );
});
