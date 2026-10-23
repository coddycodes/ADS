import { describe, expect, it } from "@effect/vitest";
import { EnvironmentId, WS_METHODS } from "@bibcode/contracts";
import * as Cause from "effect/Cause";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as Fiber from "effect/Fiber";
import * as Latch from "effect/Latch";
import * as Layer from "effect/Layer";
import * as Schema from "effect/Schema";
import * as Stream from "effect/Stream";
import { AsyncResult, Atom, AtomRegistry } from "effect/reactivity";
import * as Rpc from "effect/rpc/Rpc";
import * as RpcClient from "effect/rpc/RpcClient";
import * as RpcGroup from "effect/rpc/RpcGroup";

import {
  environmentRpcKey,
  type AtomCommandResult,
  createAtomCommandScheduler,
  createEnvironmentRpcCommand,
  createEnvironmentRpcQueryAtomFamily,
  createEnvironmentRpcStreamCommand,
  createEnvironmentRpcSubscriptionAtomFamily,
  createRuntimeCommand,
  executeAtomCommand,
  executeAtomQuery,
  isAtomCommandInterrupted,
  mapAtomCommandResult,
  parseEnvironmentRpcKey,
  runAtomCommand,
  settleAsyncResult,
  settlePromise,
  squashAtomCommandFailure,
} from "./runtime.ts";

describe("settleAsyncResult", () => {
  it("preserves successful values and typed failures", async () => {
    const success = await settleAsyncResult(() => Promise.resolve(Exit.succeed("done")));
    expect(AsyncResult.isSuccess(success)).toBe(true);
    if (AsyncResult.isSuccess(success)) {
      expect(success.value).toBe("done");
    }

    const expectedFailure = new Error("request failed");
    const failure = await settleAsyncResult(() => Promise.resolve(Exit.fail(expectedFailure)));
    expect(AsyncResult.isFailure(failure)).toBe(true);
    if (AsyncResult.isFailure(failure)) {
      expect(Cause.hasDies(failure.cause)).toBe(false);
      expect(Cause.squash(failure.cause)).toBe(expectedFailure);
    }
  });

  it("encodes thrown and rejected promises as defects", async () => {
    const thrownDefect = new Error("thrown defect");
    const thrown = await settleAsyncResult<void, never>(() => {
      throw thrownDefect;
    });
    expect(AsyncResult.isFailure(thrown)).toBe(true);
    if (AsyncResult.isFailure(thrown)) {
      expect(Cause.hasDies(thrown.cause)).toBe(true);
      expect(Cause.squash(thrown.cause)).toBe(thrownDefect);
    }

    const rejectedDefect = new Error("rejected defect");
    const rejected = await settleAsyncResult<void, never>(() => Promise.reject(rejectedDefect));
    expect(AsyncResult.isFailure(rejected)).toBe(true);
    if (AsyncResult.isFailure(rejected)) {
      expect(Cause.hasDies(rejected.cause)).toBe(true);
      expect(Cause.squash(rejected.cause)).toBe(rejectedDefect);
    }
  });
});

describe("atom command result helpers", () => {
  it("maps successful command values", () => {
    const result = mapAtomCommandResult(AsyncResult.success(2), (value) => value * 3);

    expect(result._tag).toBe("Success");
    if (result._tag === "Success") {
      expect(result.value).toBe(6);
    }
  });

  it("preserves failures while mapping", () => {
    const result = mapAtomCommandResult(
      AsyncResult.failure<number, string>(Cause.fail("nope")),
      (value) => value * 3,
    );

    expect(result._tag).toBe("Failure");
    if (result._tag === "Failure") {
      expect(Cause.squash(result.cause)).toBe("nope");
    }
  });

  it("distinguishes interruption from other failures", () => {
    const interrupted = AsyncResult.failure(Cause.interrupt(1));
    const failed = AsyncResult.failure(Cause.fail("nope"));

    expect(isAtomCommandInterrupted(interrupted)).toBe(true);
    expect(isAtomCommandInterrupted(failed)).toBe(false);
    expect(squashAtomCommandFailure(failed)).toBe("nope");
  });

  it("settles raw promise boundaries as successes or defects", async () => {
    const success = await settlePromise(() => Promise.resolve("done"));
    expect(success._tag).toBe("Success");

    const defect = new Error("raw promise rejected");
    const failure = await settlePromise(() => Promise.reject(defect));
    expect(failure._tag).toBe("Failure");
    if (failure._tag === "Failure") {
      expect(Cause.hasDies(failure.cause)).toBe(true);
      expect(Cause.squash(failure.cause)).toBe(defect);
    }
  });

  it("reports expected failures and defects through separate policies", async () => {
    const warnings: string[] = [];
    const errors: string[] = [];
    const reporter = {
      warn: (message: string) => {
        warnings.push(message);
      },
      error: (message: string) => {
        errors.push(message);
      },
    };

    await executeAtomCommand(() => Promise.resolve(Exit.fail("nope")), { label: "save" }, reporter);
    await executeAtomCommand(
      () => Promise.resolve(Exit.fail("ignored")),
      { label: "quiet save", reportFailure: false },
      reporter,
    );
    await executeAtomCommand(
      () => Promise.reject(new Error("defect")),
      { label: "quiet save", reportFailure: false },
      reporter,
    );
    await executeAtomCommand(
      () => Promise.resolve(Exit.interrupt(1)),
      { label: "interrupted" },
      reporter,
    );

    expect(warnings).toEqual(["[atom-command] save failed"]);
    expect(errors).toEqual(["[atom-command] quiet save defected"]);
  });
});

describe("environment RPC factory options", () => {
  it("constructs query, subscription, unary, and stream factories with optional policies", () => {
    const runtime = Atom.runtime(Layer.empty) as never;
    const scheduler = createAtomCommandScheduler();
    const concurrency = {
      mode: "serial" as const,
      key: ({ environmentId }: { environmentId: string }) => environmentId,
    };
    const target = { environmentId: EnvironmentId.make("environment-1"), input: {} } as never;

    createEnvironmentRpcQueryAtomFamily(runtime, {
      label: "query-defaults",
      tag: WS_METHODS.previewList,
    })(target);
    createEnvironmentRpcQueryAtomFamily(runtime, {
      label: "query-configured",
      tag: WS_METHODS.previewList,
      staleTimeMs: 1,
      idleTtlMs: 2,
      refreshIntervalMs: 3,
    })(target);
    createEnvironmentRpcQueryAtomFamily(runtime, {
      label: "query-refresh-policy",
      tag: WS_METHODS.previewList,
      refreshIntervalMs: () => null,
    })(target);

    createEnvironmentRpcSubscriptionAtomFamily(runtime, {
      label: "subscription-defaults",
      tag: WS_METHODS.subscribePreviewEvents,
    })(target);
    createEnvironmentRpcSubscriptionAtomFamily(runtime, {
      label: "subscription-configured",
      tag: WS_METHODS.subscribePreviewEvents,
      idleTtlMs: 0,
      transform: (stream) => stream,
    })(target);

    expect(
      createEnvironmentRpcCommand(runtime, {
        label: "command-defaults",
        tag: WS_METHODS.previewRefresh,
      }),
    ).toBeDefined();
    expect(
      createEnvironmentRpcCommand(runtime, {
        label: "command-configured",
        tag: WS_METHODS.previewRefresh,
        scheduler,
        concurrency,
      } as never),
    ).toBeDefined();

    expect(
      createEnvironmentRpcStreamCommand(runtime, {
        label: "stream-defaults",
        tag: WS_METHODS.gitRunStackedAction,
      }),
    ).toBeDefined();
    expect(
      createEnvironmentRpcStreamCommand(runtime, {
        label: "stream-configured",
        tag: WS_METHODS.gitRunStackedAction,
        scheduler,
        concurrency,
      } as never),
    ).toBeDefined();
  });
});

describe("environmentRpcKey", () => {
  it("isolates subscription state by environment and cwd", () => {
    const environmentId = EnvironmentId.make("environment-1");
    const originalTarget = {
      environmentId,
      input: { cwd: "/repo/original" },
    };
    const nextTarget = {
      environmentId,
      input: { cwd: "/repo/next" },
    };

    expect(environmentRpcKey(originalTarget)).not.toBe(environmentRpcKey(nextTarget));
    expect(environmentRpcKey(originalTarget)).toBe(environmentRpcKey({ ...originalTarget }));
    expect(
      environmentRpcKey({
        environmentId: EnvironmentId.make("environment-2"),
        input: originalTarget.input,
      }),
    ).not.toBe(environmentRpcKey(originalTarget));
  });

  it("round-trips the collision-safe atom-family target key", () => {
    const target = {
      environmentId: EnvironmentId.make("environment:1"),
      input: { threadId: "thread:1", terminalId: "terminal:1" },
    };

    expect(parseEnvironmentRpcKey<typeof target.input>(environmentRpcKey(target))).toEqual(target);
  });
});

describe("Atom.fn mutation semantics", () => {
  it.effect("interrupts the previous invocation when the same mutation atom is written again", () =>
    Effect.gen(function* () {
      const firstLatch = Latch.makeUnsafe();
      const secondLatch = Latch.makeUnsafe();
      const interrupted: string[] = [];
      const mutation = Atom.fn((id: "first" | "second") =>
        (id === "first" ? firstLatch : secondLatch).await.pipe(
          Effect.as(id),
          Effect.onInterrupt(() =>
            Effect.sync(() => {
              interrupted.push(id);
            }),
          ),
        ),
      );
      const registry = AtomRegistry.make();
      const unmount = registry.mount(mutation);

      registry.set(mutation, "first");
      registry.set(mutation, "second");
      yield* Effect.yieldNow;

      expect(interrupted).toEqual(["first"]);

      secondLatch.openUnsafe();
      expect(
        yield* AtomRegistry.getResult(registry, mutation, {
          suspendOnWaiting: true,
        }),
      ).toBe("second");

      unmount();
      registry.dispose();
    }),
  );

  it.effect("keeps stream mutations waiting until the final emitted value", () =>
    Effect.gen(function* () {
      const completionLatch = Latch.makeUnsafe();
      const mutation = Atom.fn(() =>
        Stream.make("progress").pipe(
          Stream.concat(Stream.fromEffect(completionLatch.await.pipe(Effect.as("done")))),
        ),
      );
      const registry = AtomRegistry.make();
      const unmount = registry.mount(mutation);

      registry.set(mutation, undefined);

      const progress = registry.get(mutation);
      expect(AsyncResult.isSuccess(progress)).toBe(true);
      if (AsyncResult.isSuccess(progress)) {
        expect(progress.value).toBe("progress");
        expect(progress.waiting).toBe(true);
      }

      completionLatch.openUnsafe();
      expect(
        yield* AtomRegistry.getResult(registry, mutation, {
          suspendOnWaiting: true,
        }),
      ).toBe("done");

      unmount();
      registry.dispose();
    }),
  );

  it.effect(
    "allows concurrent effects to finish but does not correlate results to individual writes",
    () =>
      Effect.gen(function* () {
        const firstLatch = Latch.makeUnsafe();
        const secondLatch = Latch.makeUnsafe();
        const mutation = Atom.fn<never, "first" | "second", "first" | "second">(
          (id: "first" | "second") =>
            (id === "first" ? firstLatch : secondLatch).await.pipe(Effect.as(id)),
          { concurrent: true },
        );
        const registry = AtomRegistry.make();
        const unmount = registry.mount(mutation);

        registry.set(mutation, "first");
        const firstResult = yield* AtomRegistry.getResult(registry, mutation, {
          suspendOnWaiting: true,
        }).pipe(Effect.forkChild({ startImmediately: true }));
        registry.set(mutation, "second");
        const secondResult = yield* AtomRegistry.getResult(registry, mutation, {
          suspendOnWaiting: true,
        }).pipe(Effect.forkChild({ startImmediately: true }));

        secondLatch.openUnsafe();
        yield* Effect.yieldNow;

        const stillWaiting = registry.get(mutation);
        expect(stillWaiting.waiting).toBe(true);

        firstLatch.openUnsafe();

        expect(yield* Fiber.join(firstResult)).toBe("first");
        expect(yield* Fiber.join(secondResult)).toBe("first");

        unmount();
        registry.dispose();
      }),
  );
});

describe("executeAtomQuery", () => {
  it("keeps concurrent query results correlated to their atoms", async () => {
    const firstLatch = Latch.makeUnsafe();
    const secondLatch = Latch.makeUnsafe();
    const firstAtom = Atom.make(firstLatch.await.pipe(Effect.as("first")));
    const secondAtom = Atom.make(secondLatch.await.pipe(Effect.as("second")));
    const scheduled = new Set<() => void>();
    const registry = AtomRegistry.make({
      scheduleTask: (task) => {
        scheduled.add(task);
        return () => scheduled.delete(task);
      },
    });

    const firstResult = executeAtomQuery(registry, firstAtom);
    const secondResult = executeAtomQuery(registry, secondAtom);

    secondLatch.openUnsafe();
    firstLatch.openUnsafe();

    const [first, second] = await Promise.all([firstResult, secondResult]);
    expect(first._tag).toBe("Success");
    expect(second._tag).toBe("Success");
    if (first._tag === "Success" && second._tag === "Success") {
      expect(first.value).toBe("first");
      expect(second.value).toBe("second");
    }
    while (scheduled.size > 0) {
      const current = [...scheduled];
      scheduled.clear();
      for (const task of current) task();
    }
    expect(registry.getNodes().size).toBe(0);

    registry.dispose();
  });
});

describe("runtime command runner", () => {
  const expectRegistryCancellationToSettle = Effect.fn(
    "TestRuntime.expectRegistryCancellationToSettle",
  )(function* (action: "reset" | "dispose") {
    const acquired = yield* Deferred.make<void>();
    const finalized = yield* Deferred.make<void>();
    let finalizers = 0;
    const runtime = Atom.runtime(Layer.empty);
    const command = createRuntimeCommand(runtime, {
      label: `test.${action}`,
      execute: () =>
        Effect.acquireUseRelease(
          Deferred.succeed(acquired, undefined),
          () => Effect.never,
          () =>
            Effect.sync(() => {
              finalizers += 1;
            }).pipe(Effect.andThen(Deferred.succeed(finalized, undefined))),
        ),
    });
    const registry = AtomRegistry.make();
    const settled = yield* Deferred.make<Awaited<ReturnType<typeof command.run>>>();
    void command.run(registry, undefined).then((result) => {
      Deferred.doneUnsafe(settled, Effect.succeed(result));
    });
    yield* Deferred.await(acquired);

    registry[action]();
    yield* Deferred.await(finalized);
    const result = yield* Deferred.await(settled);

    expect(result._tag).toBe("Failure");
    if (result._tag === "Failure") {
      expect(Cause.hasInterruptsOnly(result.cause)).toBe(true);
    }
    expect(finalizers).toBe(1);
    expect(registry.getNodes().size).toBe(0);
    if (action === "reset") {
      registry.dispose();
    }
  });

  it.effect("settles an interrupted command when its registry resets", () =>
    expectRegistryCancellationToSettle("reset"),
  );

  it.effect("settles an interrupted command when its registry disposes", () =>
    expectRegistryCancellationToSettle("dispose"),
  );

  const hasNodeLabeled = (registry: AtomRegistry.AtomRegistry, label: string) =>
    [...registry.getNodes().values()].some((node) => node.atom.label?.[0] === label);

  it.effect("interrupts a running command when its abort signal fires", () =>
    Effect.gen(function* () {
      const acquired = yield* Deferred.make<void>();
      const finalized = yield* Deferred.make<void>();
      let finalizers = 0;
      const runtime = Atom.runtime(Layer.empty);
      const command = createRuntimeCommand(runtime, {
        label: "test.abort-running",
        execute: () =>
          Effect.acquireUseRelease(
            Deferred.succeed(acquired, undefined),
            () => Effect.never,
            () =>
              Effect.sync(() => {
                finalizers += 1;
              }).pipe(Effect.andThen(Deferred.succeed(finalized, undefined))),
          ),
      });
      const registry = AtomRegistry.make();
      const controller = new AbortController();
      const settled = yield* Deferred.make<Awaited<ReturnType<typeof command.run>>>();
      void command.run(registry, undefined, { signal: controller.signal }).then((result) => {
        Deferred.doneUnsafe(settled, Effect.succeed(result));
      });
      yield* Deferred.await(acquired);

      controller.abort();
      const result = yield* Deferred.await(settled);
      yield* Deferred.await(finalized);

      expect(isAtomCommandInterrupted(result)).toBe(true);
      expect(finalizers).toBe(1);
      expect(hasNodeLabeled(registry, "test.abort-running")).toBe(false);
      registry.dispose();
    }),
  );

  it.effect("sends an RPC interrupt for an in-flight request when the signal aborts", () =>
    Effect.gen(function* () {
      const requested = yield* Deferred.make<void>();
      const interruptSent = yield* Deferred.make<void>();
      const group = RpcGroup.make(Rpc.make("slow", { success: Schema.String }));
      const runtime = Atom.runtime(Layer.empty);
      const command = createRuntimeCommand(runtime, {
        label: "test.abort-rpc",
        execute: () =>
          Effect.gen(function* () {
            const { client } = yield* RpcClient.makeNoSerialization(group, {
              onFromClient: ({ message }) =>
                message._tag === "Request"
                  ? Deferred.succeed(requested, undefined).pipe(Effect.asVoid)
                  : message._tag === "Interrupt"
                    ? Deferred.succeed(interruptSent, undefined).pipe(Effect.asVoid)
                    : Effect.void,
            });
            return yield* client.slow();
          }).pipe(Effect.scoped),
      });
      const registry = AtomRegistry.make();
      const controller = new AbortController();
      const settled = yield* Deferred.make<Awaited<ReturnType<typeof command.run>>>();
      void command.run(registry, undefined, { signal: controller.signal }).then((result) => {
        Deferred.doneUnsafe(settled, Effect.succeed(result));
      });
      yield* Deferred.await(requested);

      controller.abort();
      const result = yield* Deferred.await(settled);
      yield* Deferred.await(interruptSent);

      expect(isAtomCommandInterrupted(result)).toBe(true);
      registry.dispose();
    }),
  );

  it("does not start a command whose signal already aborted", async () => {
    let executions = 0;
    const runtime = Atom.runtime(Layer.empty);
    const command = createRuntimeCommand(runtime, {
      label: "test.abort-before-start",
      execute: () =>
        Effect.sync(() => {
          executions += 1;
        }),
    });
    const registry = AtomRegistry.make();
    const controller = new AbortController();
    controller.abort();

    const result = await command.run(registry, undefined, { signal: controller.signal });

    expect(isAtomCommandInterrupted(result)).toBe(true);
    expect(executions).toBe(0);
    expect(hasNodeLabeled(registry, "test.abort-before-start")).toBe(false);
    registry.dispose();
  });

  it("skips a queued serial invocation whose signal aborts before its turn", async () => {
    const firstLatch = Latch.makeUnsafe();
    const started: string[] = [];
    const runtime = Atom.runtime(Layer.empty);
    const command = createRuntimeCommand(runtime, {
      label: "test.abort-queued",
      concurrency: { mode: "serial", key: () => "shared" },
      execute: (id: "first" | "second") =>
        Effect.sync(() => started.push(id)).pipe(
          Effect.andThen(id === "first" ? firstLatch.await : Effect.void),
          Effect.as(id),
        ),
    });
    const registry = AtomRegistry.make();
    const controller = new AbortController();

    const first = command.run(registry, "first");
    const second = command.run(registry, "second", { signal: controller.signal });
    let secondSettled = false;
    void second.then(() => {
      secondSettled = true;
    });
    controller.abort();
    for (let tick = 0; tick < 5; tick += 1) {
      await Promise.resolve();
    }
    // The aborted invocation waits for its turn in the lane before it settles.
    expect(secondSettled).toBe(false);
    firstLatch.openUnsafe();

    expect(await first).toMatchObject({ _tag: "Success", value: "first", waiting: false });
    expect(isAtomCommandInterrupted(await second)).toBe(true);
    expect(started).toEqual(["first"]);
    registry.dispose();
  });

  it("ignores the signal of a caller that joins a running single-flight execution", async () => {
    const latch = Latch.makeUnsafe();
    let executions = 0;
    const runtime = Atom.runtime(Layer.empty);
    const command = createRuntimeCommand(runtime, {
      label: "test.abort-single-flight-joiner",
      concurrency: { mode: "singleFlight", key: () => "shared" },
      execute: () =>
        Effect.sync(() => executions++).pipe(Effect.andThen(latch.await), Effect.as("done")),
    });
    const registry = AtomRegistry.make();
    const joiner = new AbortController();

    const started = command.run(registry, undefined);
    const joined = command.run(registry, undefined, { signal: joiner.signal });
    joiner.abort();
    latch.openUnsafe();

    expect(await started).toMatchObject({ _tag: "Success", value: "done" });
    expect(await joined).toMatchObject({ _tag: "Success", value: "done" });
    expect(executions).toBe(1);
    registry.dispose();
  });

  it("interrupts a single-flight execution for every joined caller when its starter aborts", async () => {
    const runtime = Atom.runtime(Layer.empty);
    const command = createRuntimeCommand(runtime, {
      label: "test.abort-single-flight-starter",
      concurrency: { mode: "singleFlight", key: () => "shared" },
      execute: () => Effect.never,
    });
    const registry = AtomRegistry.make();
    const starter = new AbortController();

    const started = command.run(registry, undefined, { signal: starter.signal });
    const joined = command.run(registry, undefined);
    starter.abort();

    expect(isAtomCommandInterrupted(await started)).toBe(true);
    expect(isAtomCommandInterrupted(await joined)).toBe(true);
    registry.dispose();
  });

  it.each([
    { aborted: "newest" as const, expected: "skipped" as const },
    { aborted: "older" as const, expected: "ran" as const },
  ])(
    "follows only the newest caller's signal in a coalesced latest run ($aborted aborts)",
    async ({ aborted, expected }) => {
      const firstLatch = Latch.makeUnsafe();
      const executed: number[] = [];
      const runtime = Atom.runtime(Layer.empty);
      const command = createRuntimeCommand(runtime, {
        label: `test.abort-latest-${aborted}`,
        concurrency: { mode: "latest", key: () => "shared" },
        execute: (value: number) =>
          Effect.sync(() => executed.push(value)).pipe(
            Effect.andThen(value === 1 ? firstLatch.await : Effect.void),
            Effect.as(value),
          ),
      });
      const registry = AtomRegistry.make();
      const older = new AbortController();
      const newest = new AbortController();

      const first = command.run(registry, 1);
      await Promise.resolve();
      const second = command.run(registry, 2, { signal: older.signal });
      const third = command.run(registry, 3, { signal: newest.signal });
      (aborted === "newest" ? newest : older).abort();
      firstLatch.openUnsafe();

      expect(await first).toMatchObject({ _tag: "Success", value: 1 });
      if (expected === "skipped") {
        expect(isAtomCommandInterrupted(await second)).toBe(true);
        expect(isAtomCommandInterrupted(await third)).toBe(true);
        expect(executed).toEqual([1]);
      } else {
        expect(await second).toMatchObject({ _tag: "Success", value: 3 });
        expect(await third).toMatchObject({ _tag: "Success", value: 3 });
        expect(executed).toEqual([1, 3]);
      }
      registry.dispose();
    },
  );

  it("interrupts a running coalesced latest run for every caller when its newest caller aborts", async () => {
    const firstLatch = Latch.makeUnsafe();
    const executed: number[] = [];
    let markCoalescedStarted!: () => void;
    const coalescedStarted = new Promise<void>((resolve) => {
      markCoalescedStarted = resolve;
    });
    const runtime = Atom.runtime(Layer.empty);
    const command = createRuntimeCommand(runtime, {
      label: "test.abort-latest-running",
      concurrency: { mode: "latest", key: () => "shared" },
      execute: (value: number) =>
        Effect.sync(() => executed.push(value)).pipe(
          Effect.andThen(
            value === 1
              ? firstLatch.await
              : Effect.sync(markCoalescedStarted).pipe(Effect.andThen(Effect.never)),
          ),
          Effect.as(value),
        ),
    });
    const registry = AtomRegistry.make();
    const newest = new AbortController();

    const first = command.run(registry, 1);
    await Promise.resolve();
    const second = command.run(registry, 2);
    const third = command.run(registry, 3, { signal: newest.signal });
    firstLatch.openUnsafe();
    expect(await first).toMatchObject({ _tag: "Success", value: 1 });
    await coalescedStarted;
    newest.abort();

    expect(isAtomCommandInterrupted(await second)).toBe(true);
    expect(isAtomCommandInterrupted(await third)).toBe(true);
    expect(executed).toEqual([1, 3]);
    registry.dispose();
  });

  it("forwards the abort signal from runAtomCommand to the command", async () => {
    const controller = new AbortController();
    const registry = AtomRegistry.make();
    let received: AbortSignal | undefined;

    await runAtomCommand(
      registry,
      {
        label: "test.forward-signal",
        run: async (_registry, _input, options) => {
          received = options?.signal;
          return AsyncResult.success(undefined);
        },
      },
      undefined,
      { signal: controller.signal },
    );

    expect(received).toBe(controller.signal);
    registry.dispose();
  });

  it("encodes custom command rejections as defects", async () => {
    const defect = new Error("custom command rejected");
    const registry = AtomRegistry.make();
    const result = await runAtomCommand(
      registry,
      {
        label: "test.rejected-command",
        run: () => Promise.reject(defect),
      },
      undefined,
      { reportDefect: false },
    );

    expect(result._tag).toBe("Failure");
    if (result._tag === "Failure") {
      expect(Cause.hasDies(result.cause)).toBe(true);
      expect(Cause.squash(result.cause)).toBe(defect);
    }
    registry.dispose();
  });

  it("settles generated command scheduler defects from direct callers", async () => {
    const defect = new Error("invalid command key");
    const runtime = Atom.runtime(Layer.empty);
    const command = createRuntimeCommand(runtime, {
      label: "test.invalid-key",
      concurrency: {
        mode: "serial",
        key: () => {
          throw defect;
        },
      },
      execute: () => Effect.void,
    });
    const registry = AtomRegistry.make();

    const result = await command.run(registry, undefined);
    expect(result._tag).toBe("Failure");
    if (result._tag === "Failure") {
      expect(Cause.hasDies(result.cause)).toBe(true);
      expect(Cause.squash(result.cause)).toBe(defect);
    }
    registry.dispose();
  });

  it("correlates parallel invocation results", async () => {
    const firstLatch = Latch.makeUnsafe();
    const secondLatch = Latch.makeUnsafe();
    const runtime = Atom.runtime(Layer.empty);
    const command = createRuntimeCommand(runtime, {
      label: "test.parallel",
      execute: (id: "first" | "second") =>
        (id === "first" ? firstLatch : secondLatch).await.pipe(Effect.as(id)),
    });
    const registry = AtomRegistry.make();

    const first = command.run(registry, "first");
    const second = command.run(registry, "second");
    secondLatch.openUnsafe();
    firstLatch.openUnsafe();

    expect(await first).toMatchObject({ _tag: "Success", value: "first", waiting: false });
    expect(await second).toMatchObject({ _tag: "Success", value: "second", waiting: false });
    registry.dispose();
  });

  it("serializes commands that share a scheduler and lane", async () => {
    const firstLatch = Latch.makeUnsafe();
    const events: string[] = [];
    const runtime = Atom.runtime(Layer.empty);
    const scheduler = createAtomCommandScheduler();
    const concurrency = { mode: "serial" as const, key: () => "shared" };
    const firstCommand = createRuntimeCommand(runtime, {
      label: "test.first",
      scheduler,
      concurrency,
      execute: () =>
        Effect.sync(() => events.push("first:start")).pipe(
          Effect.andThen(firstLatch.await),
          Effect.tap(() => Effect.sync(() => events.push("first:end"))),
        ),
    });
    const secondCommand = createRuntimeCommand(runtime, {
      label: "test.second",
      scheduler,
      concurrency,
      execute: () => Effect.sync(() => events.push("second:start")),
    });
    const registry = AtomRegistry.make();

    const first = firstCommand.run(registry, undefined);
    const second = secondCommand.run(registry, undefined);
    await Promise.resolve();
    expect(events).toEqual(["first:start"]);

    firstLatch.openUnsafe();
    await Promise.all([first, second]);
    expect(events).toEqual(["first:start", "first:end", "second:start"]);
    registry.dispose();
  });

  it("deduplicates single-flight commands by key", async () => {
    const latch = Latch.makeUnsafe();
    let executions = 0;
    const runtime = Atom.runtime(Layer.empty);
    const command = createRuntimeCommand(runtime, {
      label: "test.single-flight",
      concurrency: { mode: "singleFlight", key: (key: string) => key },
      execute: () =>
        Effect.sync(() => executions++).pipe(Effect.andThen(latch.await), Effect.as("done")),
    });
    const registry = AtomRegistry.make();

    const first = command.run(registry, "same");
    const second = command.run(registry, "same");
    latch.openUnsafe();

    expect(await first).toMatchObject({ _tag: "Success", value: "done", waiting: false });
    expect(await second).toMatchObject({ _tag: "Success", value: "done", waiting: false });
    expect(executions).toBe(1);
    registry.dispose();
  });

  it("coalesces pending latest-value commands without interrupting the active call", async () => {
    const firstLatch = Latch.makeUnsafe();
    const executed: number[] = [];
    const runtime = Atom.runtime(Layer.empty);
    const command = createRuntimeCommand(runtime, {
      label: "test.latest",
      concurrency: { mode: "latest", key: () => "shared" },
      execute: (value: number) =>
        Effect.sync(() => executed.push(value)).pipe(
          Effect.andThen(value === 1 ? firstLatch.await : Effect.void),
          Effect.as(value),
        ),
    });
    const registry = AtomRegistry.make();

    const first = command.run(registry, 1);
    await Promise.resolve();
    const second = command.run(registry, 2);
    const third = command.run(registry, 3);
    firstLatch.openUnsafe();

    expect(await first).toMatchObject({ _tag: "Success", value: 1, waiting: false });
    expect(await second).toMatchObject({ _tag: "Success", value: 3, waiting: false });
    expect(await third).toMatchObject({ _tag: "Success", value: 3, waiting: false });
    expect(executed).toEqual([1, 3]);
    registry.dispose();
  });

  it("runs latest lanes independently by key and deletes settled lane state", async () => {
    let resolveFirst!: (result: AtomCommandResult<string, never>) => void;
    const firstResult = new Promise<AtomCommandResult<string, never>>((resolve) => {
      resolveFirst = resolve;
    });
    const scheduler = createAtomCommandScheduler();
    const registry = AtomRegistry.make();
    const concurrency = { mode: "latest" as const, key: (key: string) => key };
    const executed: string[] = [];

    const first = scheduler.schedule(registry, concurrency, "first", () => {
      executed.push("first:blocked");
      return firstResult;
    });
    const other = scheduler.schedule(registry, concurrency, "other", async () => {
      executed.push("other");
      return AsyncResult.success("other");
    });

    expect(await other).toMatchObject({ _tag: "Success", value: "other" });
    expect(executed).toEqual(["first:blocked", "other"]);
    resolveFirst(AsyncResult.success("first"));
    await first;
    const replacement = await scheduler.schedule(registry, concurrency, "first", async () => {
      executed.push("first:replacement");
      return AsyncResult.success("replacement");
    });
    expect(replacement).toMatchObject({ _tag: "Success", value: "replacement" });
    expect(executed).toEqual(["first:blocked", "other", "first:replacement"]);
    registry.dispose();
  });

  it("cleans latest lanes after rejection and registry cancellation", async () => {
    const scheduler = createAtomCommandScheduler();
    const runtime = Atom.runtime(Layer.empty);
    let markStarted!: () => void;
    const started = new Promise<void>((resolve) => {
      markStarted = resolve;
    });
    const concurrency = { mode: "latest" as const, key: () => "shared" };
    const command = createRuntimeCommand(runtime, {
      label: "test.latest-cleanup",
      scheduler,
      concurrency,
      execute: (input: "cancel" | "complete") =>
        input === "cancel"
          ? Effect.sync(markStarted).pipe(Effect.andThen(Effect.never))
          : Effect.succeed(input),
    });
    const registry = AtomRegistry.make();

    const rejected = await scheduler.schedule(registry, concurrency, undefined, () =>
      Promise.reject(new Error("refresh rejected")),
    );
    expect(rejected._tag).toBe("Failure");
    if (rejected._tag === "Failure") {
      expect(Cause.hasDies(rejected.cause)).toBe(true);
    }

    const cancelled = command.run(registry, "cancel");
    await started;
    registry.reset();
    expect(isAtomCommandInterrupted(await cancelled)).toBe(true);
    expect(await command.run(registry, "complete")).toMatchObject({
      _tag: "Success",
      value: "complete",
    });
    registry.dispose();
  });
});
