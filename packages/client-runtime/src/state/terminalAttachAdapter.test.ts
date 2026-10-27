import { describe, expect, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Latch from "effect/Latch";
import * as Stream from "effect/Stream";

import type { TerminalAttachStreamEvent, TerminalSessionSnapshot } from "@coddy/contracts";

import {
  acquireTerminalMetadataStream,
  createTerminalTranscriptRuntimeRegistry,
  projectTerminalMetadataStream,
  terminalTranscriptRuntimeKey,
} from "./terminalAttachAdapter.ts";
import { createTerminalTranscriptRuntime } from "./terminalTranscriptRuntime.ts";

const snapshotData = (
  history: string,
  status: TerminalSessionSnapshot["status"] = "running",
): TerminalSessionSnapshot => ({
  threadId: "thread-1",
  terminalId: "terminal-1",
  cwd: "/repo",
  worktreePath: null,
  status,
  pid: 1,
  history,
  oscColorResponderActive: false,
  firstAttachmentGrant: false,
  exitCode: null,
  exitSignal: null,
  label: "Terminal 1",
  updatedAt: "2026-07-17T00:00:00.000Z",
  sequence: 1,
});

const snapshot = (history: string): TerminalAttachStreamEvent => ({
  type: "snapshot",
  snapshot: snapshotData(history),
});

const output = (data: string): TerminalAttachStreamEvent => ({
  type: "output",
  threadId: "thread-1",
  terminalId: "terminal-1",
  sequence: 2,
  data,
});

describe("projectTerminalMetadataStream", () => {
  it.effect("emits every lifecycle event once and never emits output or activity (C2)", () =>
    Effect.gen(function* () {
      const runtime = createTerminalTranscriptRuntime();
      const events = Stream.make(
        output("pre"),
        {
          type: "activity",
          threadId: "thread-1",
          terminalId: "terminal-1",
          sequence: 3,
          hasRunningSubprocess: false,
          label: "Terminal 1",
        } satisfies TerminalAttachStreamEvent,
        snapshot("boot\n"),
        output("a"),
        {
          type: "resized",
          threadId: "thread-1",
          terminalId: "terminal-1",
          size: { cols: 91, rows: 42, sizeClaim: "renderer" },
        } satisfies TerminalAttachStreamEvent,
        {
          type: "cleared",
          threadId: "thread-1",
          terminalId: "terminal-1",
          sequence: 4,
        } satisfies TerminalAttachStreamEvent,
        {
          type: "error",
          threadId: "thread-1",
          terminalId: "terminal-1",
          sequence: 5,
          message: "boom",
        } satisfies TerminalAttachStreamEvent,
        {
          type: "closed",
          threadId: "thread-1",
          terminalId: "terminal-1",
          sequence: 6,
        } satisfies TerminalAttachStreamEvent,
        {
          type: "restarted",
          threadId: "thread-1",
          terminalId: "terminal-1",
          sequence: 7,
          snapshot: snapshotData("fresh\n", "starting"),
        } satisfies TerminalAttachStreamEvent,
        {
          type: "exited",
          threadId: "thread-1",
          terminalId: "terminal-1",
          sequence: 8,
          exitCode: 0,
          exitSignal: null,
        } satisfies TerminalAttachStreamEvent,
      );

      const collected = yield* Stream.runCollect(projectTerminalMetadataStream(events, runtime));
      const metadata = collected;

      expect(metadata.map((item) => item.revision)).toEqual([1, 2, 3, 4, 5, 6]);
      expect(metadata.map((item) => item.status)).toEqual([
        "running",
        "running",
        "error",
        "closed",
        "starting",
        "exited",
      ]);
      expect(metadata.map((item) => item.generation)).toEqual([1, 1, 1, 1, 2, 2]);
      expect(runtime.snapshot()).toBe("fresh\n");
    }),
  );

  it.effect(
    "an output-only stream produces zero metadata while still updating the transcript",
    () =>
      Effect.gen(function* () {
        const runtime = createTerminalTranscriptRuntime();
        const collected = yield* Stream.runCollect(
          projectTerminalMetadataStream(Stream.make(output("a"), output("b")), runtime),
        );

        expect(collected).toEqual([]);
        expect(runtime.snapshot()).toBe("ab");
        expect(runtime.metadata().revision).toBe(0);
      }),
  );

  it.effect("preserves upstream failure after ingesting preceding events", () =>
    Effect.gen(function* () {
      const runtime = createTerminalTranscriptRuntime();
      const exit = yield* projectTerminalMetadataStream(
        Stream.make(snapshot("boot"), output("tail")).pipe(Stream.concat(Stream.fail("boom"))),
        runtime,
      ).pipe(Stream.runCollect, Effect.exit);

      expect(exit._tag).toBe("Failure");
      expect(runtime.snapshot()).toBe("boottail");
    }),
  );
});

describe("createTerminalTranscriptRuntimeRegistry", () => {
  it("shares exact identity per key and cleans up only after the final balanced release", () => {
    const registry = createTerminalTranscriptRuntimeRegistry();
    const first = registry.acquire("key");
    const second = registry.acquire("key");

    expect(first).toBe(second);
    expect(registry.get("key")).toBe(first);
    expect(registry.size()).toBe(1);
    registry.release("key");
    expect(registry.get("key")).toBe(first);
    expect(registry.size()).toBe(1);
    registry.release("key");
    expect(registry.get("key")).toBeUndefined();
    expect(registry.size()).toBe(0);

    registry.release("missing");
    expect(registry.size()).toBe(0);
    expect(registry.acquire("key")).not.toBe(first);
  });

  it("keeps tuple-derived keys collision-safe", () => {
    expect(
      terminalTranscriptRuntimeKey("environment:a", {
        threadId: "thread",
        terminalId: "terminal",
      }),
    ).not.toBe(
      terminalTranscriptRuntimeKey("environment", {
        threadId: "a:thread",
        terminalId: "terminal",
      }),
    );
    expect(
      terminalTranscriptRuntimeKey("environment", {
        threadId: "thread",
        terminalId: "terminal:a",
      }),
    ).not.toBe(
      terminalTranscriptRuntimeKey("environment", {
        threadId: "thread:terminal",
        terminalId: "a",
      }),
    );
    expect(
      terminalTranscriptRuntimeKey("environment", {
        threadId: "thread",
      }),
    ).toBe(
      terminalTranscriptRuntimeKey("environment", {
        threadId: "thread",
        terminalId: undefined,
      }),
    );
  });
});

describe("acquireTerminalMetadataStream", () => {
  it.effect("acquires lazily for stream execution and releases after normal completion", () =>
    Effect.gen(function* () {
      const registry = createTerminalTranscriptRuntimeRegistry();
      const events = Stream.fromEffect(
        Effect.sync(() => {
          expect(registry.size()).toBe(1);
          expect(registry.get("key")).toBeDefined();
          return snapshot("boot");
        }),
      );
      const projected = acquireTerminalMetadataStream(events, registry, "key");

      expect(registry.size()).toBe(0);
      const collected = yield* Stream.runCollect(projected);
      expect(collected.map((metadata) => metadata.revision)).toEqual([1]);
      expect(registry.size()).toBe(0);
      expect(registry.get("key")).toBeUndefined();
    }),
  );

  it.effect("releases after upstream failure", () =>
    Effect.gen(function* () {
      const registry = createTerminalTranscriptRuntimeRegistry();
      const exit = yield* acquireTerminalMetadataStream(
        Stream.make(snapshot("boot")).pipe(Stream.concat(Stream.fail("boom"))),
        registry,
        "key",
      ).pipe(Stream.runCollect, Effect.exit);

      expect(exit._tag).toBe("Failure");
      expect(registry.size()).toBe(0);
    }),
  );

  it.effect("releases after interruption", () =>
    Effect.gen(function* () {
      const registry = createTerminalTranscriptRuntimeRegistry();
      const started = Latch.makeUnsafe();
      const events = Stream.fromEffect(
        Effect.sync(() => {
          started.openUnsafe();
          return output("started");
        }),
      ).pipe(Stream.concat(Stream.never));
      const fiber = yield* acquireTerminalMetadataStream(events, registry, "key").pipe(
        Stream.runDrain,
        Effect.forkChild({ startImmediately: true }),
      );

      yield* started.await;
      expect(registry.size()).toBe(1);
      yield* Fiber.interrupt(fiber);
      expect(registry.size()).toBe(0);
    }),
  );

  it.effect("balances independent concurrent leases for the same key", () =>
    Effect.gen(function* () {
      const registry = createTerminalTranscriptRuntimeRegistry();
      const firstStarted = Latch.makeUnsafe();
      const secondStarted = Latch.makeUnsafe();
      const makeEvents = (started: Latch.Latch) =>
        Stream.fromEffect(
          Effect.sync(() => {
            started.openUnsafe();
            return output("x");
          }),
        ).pipe(Stream.concat(Stream.never));
      const first = yield* acquireTerminalMetadataStream(
        makeEvents(firstStarted),
        registry,
        "key",
      ).pipe(Stream.runDrain, Effect.forkChild({ startImmediately: true }));
      const second = yield* acquireTerminalMetadataStream(
        makeEvents(secondStarted),
        registry,
        "key",
      ).pipe(Stream.runDrain, Effect.forkChild({ startImmediately: true }));

      yield* firstStarted.await;
      yield* secondStarted.await;
      expect(registry.size()).toBe(1);
      const shared = registry.get("key");
      expect(shared?.snapshot()).toBe("xx");

      yield* Fiber.interrupt(first);
      expect(registry.get("key")).toBe(shared);
      yield* Fiber.interrupt(second);
      expect(registry.size()).toBe(0);
    }),
  );
});
