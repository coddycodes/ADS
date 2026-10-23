import { describe, expect, it } from "@effect/vitest";

import { EnvironmentId, TerminalSessionSnapshot, ThreadId } from "@bibcode/contracts";
import { Effect, Latch, Layer, Stream } from "effect";
import { AsyncResult, Atom, AtomRegistry } from "effect/reactivity";

import { EnvironmentRegistry } from "../connection/registry.ts";
import {
  accumulateTerminalMetadataEvents,
  createTerminalEnvironmentAtoms,
  terminalTranscriptRuntimeKey,
} from "./terminal.ts";
import {
  applyTerminalMetadataStreamEvent,
  combineTerminalSessionState,
  EMPTY_TERMINAL_SESSION_STATE,
  selectRunningSubprocessTerminalIds,
} from "./terminalSession.ts";
import {
  EMPTY_TERMINAL_METADATA_SNAPSHOT,
  type TerminalMetadataSnapshot,
} from "./terminalTranscriptRuntime.ts";

const TARGET = {
  environmentId: EnvironmentId.make("env-local"),
  threadId: ThreadId.make("thread-1"),
  terminalId: "term-1",
} as const;

const BASE_SNAPSHOT: TerminalSessionSnapshot = {
  oscColorResponderActive: false,
  firstAttachmentGrant: false,
  threadId: TARGET.threadId,
  terminalId: TARGET.terminalId,
  cwd: "/repo",
  worktreePath: null,
  status: "running",
  pid: 123,
  history: "hello",
  exitCode: null,
  exitSignal: null,
  label: "Terminal 1",
  updatedAt: "2026-04-01T00:00:00.000Z",
};

const summary = () =>
  applyTerminalMetadataStreamEvent([], {
    type: "snapshot",
    terminals: [
      {
        threadId: BASE_SNAPSHOT.threadId,
        terminalId: BASE_SNAPSHOT.terminalId,
        cwd: BASE_SNAPSHOT.cwd,
        worktreePath: BASE_SNAPSHOT.worktreePath,
        status: "running",
        pid: BASE_SNAPSHOT.pid,
        exitCode: BASE_SNAPSHOT.exitCode,
        exitSignal: BASE_SNAPSHOT.exitSignal,
        updatedAt: BASE_SNAPSHOT.updatedAt,
        hasRunningSubprocess: false,
        label: BASE_SNAPSHOT.label,
        centerPanel: false,
      },
    ],
  })[0]!;

describe("terminal session metadata", () => {
  it.effect("does not emit an empty metadata seed before the first server event", () =>
    Effect.gen(function* () {
      const metadata = yield* accumulateTerminalMetadataEvents(
        Stream.make({
          type: "snapshot" as const,
          terminals: [{ ...summary(), hasRunningSubprocess: false }],
        }),
      ).pipe(Stream.runCollect);

      expect(metadata).toHaveLength(1);
      expect(metadata[0]).toHaveLength(1);
    }),
  );

  it("prefers attach metadata over a stale summary after attach has emitted", () => {
    const staleSummary = { ...summary(), consoleTheme: "dark" as const };
    const attached: TerminalMetadataSnapshot = {
      status: "error",
      error: "Terminal disconnected.",
      consoleTheme: "light",
      generation: 1,
      revision: 2,
    };

    expect(combineTerminalSessionState(staleSummary, attached)).toEqual({
      summary: staleSummary,
      status: "error",
      error: "Terminal disconnected.",
      consoleTheme: "light",
      hasRunningSubprocess: false,
      updatedAt: BASE_SNAPSHOT.updatedAt,
      generation: 1,
    });
  });

  it("uses summary status before the attach stream has emitted", () => {
    expect(combineTerminalSessionState(summary(), EMPTY_TERMINAL_METADATA_SNAPSHOT)).toMatchObject({
      status: "running",
      error: null,
      generation: 0,
    });
  });

  it("returns metadata-only session state with no reactive transcript or version", () => {
    const state = combineTerminalSessionState(null, {
      status: "running",
      error: null,
      consoleTheme: null,
      generation: 3,
      revision: 9,
    });

    expect(state).toEqual({
      summary: null,
      status: "running",
      error: null,
      consoleTheme: null,
      hasRunningSubprocess: false,
      updatedAt: null,
      generation: 3,
    });
    expect("buffer" in state).toBe(false);
    expect("version" in state).toBe(false);
    expect("revision" in state).toBe(false);
    expect("buffer" in EMPTY_TERMINAL_SESSION_STATE).toBe(false);
  });

  it("does not treat an idle running shell as a running subprocess", () => {
    const idleSession = {
      target: TARGET,
      state: {
        ...combineTerminalSessionState(null, EMPTY_TERMINAL_METADATA_SNAPSHOT),
        status: "running" as const,
        hasRunningSubprocess: false,
      },
    };
    const activeSession = {
      target: { ...TARGET, terminalId: "term-2" },
      state: {
        ...idleSession.state,
        hasRunningSubprocess: true,
      },
    };

    expect(selectRunningSubprocessTerminalIds([idleSession, activeSession])).toEqual(["term-2"]);
  });

  it("reduces terminal metadata snapshots, upserts, and removals", () => {
    const initial = applyTerminalMetadataStreamEvent([], {
      type: "snapshot",
      terminals: [summary()],
    });
    const updated = applyTerminalMetadataStreamEvent(initial, {
      type: "upsert",
      terminal: {
        ...initial[0]!,
        hasRunningSubprocess: true,
      },
    });
    const removed = applyTerminalMetadataStreamEvent(updated, {
      type: "remove",
      threadId: TARGET.threadId,
      terminalId: TARGET.terminalId,
    });

    expect(updated).toHaveLength(1);
    expect(updated[0]?.hasRunningSubprocess).toBe(true);
    expect(removed).toEqual([]);
  });

  it.effect("keeps speculative attach snapshot reads side-effect free", () =>
    Effect.gen(function* () {
      let sourceStarts = 0;
      const target = {
        environmentId: TARGET.environmentId,
        input: {
          threadId: TARGET.threadId,
          terminalId: TARGET.terminalId,
          cwd: "/repo",
        },
      };
      const environmentRegistry = EnvironmentRegistry.of({
        followStream: () =>
          Stream.unwrap(
            Effect.sync(() => {
              sourceStarts += 1;
              return Stream.never;
            }),
          ),
      } as never);
      const atoms = createTerminalEnvironmentAtoms(
        Atom.runtime(Layer.succeed(EnvironmentRegistry, environmentRegistry)),
      );
      const atomRegistry = AtomRegistry.make();

      expect(atomRegistry.get(atoms.attachSnapshot(target))).toEqual({
        metadata: EMPTY_TERMINAL_METADATA_SNAPSHOT,
        transcriptRuntime: null,
      });
      yield* Effect.yieldNow;

      expect(sourceStarts).toBe(0);
      expect(atoms.transcriptRuntimes.size()).toBe(0);
      expect(
        atoms.transcriptRuntimes.referenceCount(
          terminalTranscriptRuntimeKey(TARGET.environmentId, target.input),
        ),
      ).toBe(0);
      atomRegistry.dispose();
    }),
  );

  it("keeps one lazy transcript registry per terminal atom environment", () => {
    const first = createTerminalEnvironmentAtoms(Atom.runtime(Layer.empty) as never);
    const second = createTerminalEnvironmentAtoms(Atom.runtime(Layer.empty) as never);

    expect(first.transcriptRuntimes).not.toBe(second.transcriptRuntimes);
    expect(first.transcriptRuntimes.size()).toBe(0);
    first.attachSnapshot({
      environmentId: TARGET.environmentId,
      input: {
        threadId: TARGET.threadId,
        terminalId: TARGET.terminalId,
        cwd: "/repo",
      },
    });
    expect(first.transcriptRuntimes.size()).toBe(0);
  });

  it.effect(
    "starts one attach lease after commit and publishes the runtime with its snapshot",
    () =>
      Effect.gen(function* () {
        const sourceStarted = Latch.makeUnsafe();
        let sourceStarts = 0;
        const environmentRegistry = EnvironmentRegistry.of({
          followStream: () =>
            Stream.fromEffect(
              Effect.sync(() => {
                sourceStarts += 1;
                sourceStarted.openUnsafe();
                return {
                  type: "snapshot" as const,
                  snapshot: BASE_SNAPSHOT,
                };
              }),
            ).pipe(Stream.concat(Stream.never)),
        } as never);
        const atoms = createTerminalEnvironmentAtoms(
          Atom.runtime(Layer.succeed(EnvironmentRegistry, environmentRegistry)),
        );
        const target = {
          environmentId: TARGET.environmentId,
          input: {
            threadId: TARGET.threadId,
            terminalId: TARGET.terminalId,
            cwd: "/repo",
          },
        };
        const attachProducer = atoms.attachProducer(target);
        const attachSnapshot = atoms.attachSnapshot(target);
        const atomRegistry = AtomRegistry.make();
        const snapshotPublished = Latch.makeUnsafe();
        const snapshotCleared = Latch.makeUnsafe();
        let didPublishSnapshot = false;
        const unsubscribeSnapshot = atomRegistry.subscribe(
          attachSnapshot,
          (snapshot) => {
            if (snapshot.metadata.generation === 1) {
              didPublishSnapshot = true;
              snapshotPublished.openUnsafe();
            } else if (didPublishSnapshot && snapshot.transcriptRuntime === null) {
              snapshotCleared.openUnsafe();
            }
          },
          { immediate: true },
        );
        const unmountFirst = atomRegistry.mount(attachProducer);
        const unmountSecond = atomRegistry.mount(attachProducer);

        yield* sourceStarted.await;
        yield* snapshotPublished.await;
        const snapshot = atomRegistry.get(attachSnapshot);
        expect(snapshot.metadata).toMatchObject({
          generation: 1,
          revision: 1,
          status: "running",
        });
        expect(snapshot.transcriptRuntime).toBe(
          atoms.transcriptRuntimes.get(
            terminalTranscriptRuntimeKey(TARGET.environmentId, target.input),
          ),
        );
        expect(sourceStarts).toBe(1);
        expect(atoms.transcriptRuntimes.size()).toBe(1);
        expect(
          atoms.transcriptRuntimes.referenceCount(
            terminalTranscriptRuntimeKey(TARGET.environmentId, target.input),
          ),
        ).toBe(1);

        unmountFirst();
        yield* Effect.yieldNow;
        expect(atoms.transcriptRuntimes.size()).toBe(1);

        unmountSecond();
        yield* snapshotCleared.await;
        yield* Effect.yieldNow;
        expect(atoms.transcriptRuntimes.size()).toBe(0);
        expect(atomRegistry.get(attachSnapshot)).toEqual({
          metadata: EMPTY_TERMINAL_METADATA_SNAPSHOT,
          transcriptRuntime: null,
        });
        unsubscribeSnapshot();
        atomRegistry.dispose();
      }),
  );

  it.effect("releases the attach lease and clears its snapshot after source failure", () =>
    Effect.gen(function* () {
      let sourceStarts = 0;
      const target = {
        environmentId: TARGET.environmentId,
        input: {
          threadId: TARGET.threadId,
          terminalId: TARGET.terminalId,
          cwd: "/repo",
        },
      };
      const environmentRegistry = EnvironmentRegistry.of({
        followStream: () =>
          Stream.unwrap(
            Effect.sync(() => {
              sourceStarts += 1;
              return Stream.fail("attach failed");
            }),
          ),
      } as never);
      const atoms = createTerminalEnvironmentAtoms(
        Atom.runtime(Layer.succeed(EnvironmentRegistry, environmentRegistry)),
      );
      const attachProducer = atoms.attachProducer(target);
      const attachSnapshot = atoms.attachSnapshot(target);
      const atomRegistry = AtomRegistry.make();
      const unmount = atomRegistry.mount(attachProducer);

      yield* Effect.yieldNow;
      yield* Effect.yieldNow;

      expect(sourceStarts).toBe(1);
      expect(AsyncResult.isFailure(atomRegistry.get(attachProducer))).toBe(true);
      expect(atoms.transcriptRuntimes.size()).toBe(0);
      expect(atomRegistry.get(attachSnapshot)).toEqual({
        metadata: EMPTY_TERMINAL_METADATA_SNAPSHOT,
        transcriptRuntime: null,
      });

      unmount();
      atomRegistry.dispose();
    }),
  );
});
