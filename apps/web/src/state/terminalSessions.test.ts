import { EnvironmentId, ThreadId } from "@coddy/contracts";
import { beforeEach, describe, expect, it, vi } from "vite-plus/test";

const harness = vi.hoisted(() => ({
  queryResults: [] as Array<{ data: unknown; error: unknown }>,
  queryInputs: [] as unknown[],
  atomValueInputs: [] as unknown[],
  subscribeCalls: [] as Array<{
    atom: unknown;
    callback: (result: unknown) => void;
    options: unknown;
  }>,
  producerAtom: { kind: "attach-producer" },
  refreshedAtoms: [] as unknown[],
  snapshotAtom: { kind: "attach-snapshot" },
  snapshotValue: {
    metadata: { emptyMetadata: true },
    transcriptRuntime: null,
  } as { metadata: unknown; transcriptRuntime: unknown },
  producerState: {
    atom: null,
    error: null,
  } as { atom: unknown; error: string | null },
  retainedTranscript: null as unknown,
  attachProducer: vi.fn((_input: unknown) => null as unknown),
  attachSnapshot: vi.fn((_input: unknown) => null as unknown),
  metadata: vi.fn((input: unknown) => ({ kind: "metadata", input })),
  combine: vi.fn((summary: unknown, metadata: unknown) => ({
    summary,
    metadata,
    status: "running",
  })),
  formatError: vi.fn((_cause: unknown) => "formatted attach failure"),
  selectRunning: vi.fn((sessions: unknown) => sessions),
}));

vi.mock("react", () => ({
  useCallback: (callback: unknown) => callback,
  useMemo: (factory: () => unknown) => factory(),
  // The retained transcript starts as `null`; the producer error starts as an object.
  useState: (initial: unknown) =>
    initial === null
      ? [
          harness.retainedTranscript,
          (update: unknown) => {
            harness.retainedTranscript = update;
          },
        ]
      : [
          harness.producerState,
          (
            update:
              | { atom: unknown; error: string | null }
              | ((previous: { atom: unknown; error: string | null }) => {
                  atom: unknown;
                  error: string | null;
                }),
          ) => {
            harness.producerState =
              typeof update === "function" ? update(harness.producerState) : update;
          },
        ],
}));
vi.mock("@effect/atom-react", () => ({
  useAtomRefresh: (atom: unknown) => () => harness.refreshedAtoms.push(atom),
  useAtomSubscribe: (atom: unknown, callback: (result: unknown) => void, options: unknown) => {
    harness.subscribeCalls.push({ atom, callback, options });
  },
  useAtomValue: (atom: unknown) => {
    harness.atomValueInputs.push(atom);
    return harness.snapshotValue;
  },
}));
vi.mock("@coddy/client-runtime/state/terminal", () => ({
  combineTerminalSessionState: (summary: unknown, metadata: unknown) =>
    harness.combine(summary, metadata),
  EMPTY_TERMINAL_ATTACH_SNAPSHOT: {
    metadata: { emptyMetadata: true },
    transcriptRuntime: null,
  },
  EMPTY_TERMINAL_METADATA_SNAPSHOT: { emptyMetadata: true },
  EMPTY_TERMINAL_SESSION_STATE: { emptySession: true },
  selectRunningSubprocessTerminalIds: (sessions: unknown) => harness.selectRunning(sessions),
}));
vi.mock("./query", () => ({
  formatEnvironmentQueryError: (cause: unknown) => harness.formatError(cause),
  useEnvironmentQuery: (input: unknown) => {
    const index = harness.queryInputs.length;
    harness.queryInputs.push(input);
    return harness.queryResults[index] ?? { data: null, error: null };
  },
}));
vi.mock("./terminal", () => ({
  terminalEnvironment: {
    attachProducer: (input: unknown) => {
      harness.attachProducer(input);
      return harness.producerAtom;
    },
    attachSnapshot: (input: unknown) => {
      harness.attachSnapshot(input);
      return harness.snapshotAtom;
    },
    metadata: (input: unknown) => harness.metadata(input),
  },
}));

import {
  useAttachedTerminalSession,
  useKnownTerminalSessions,
  useThreadRunningTerminalIds,
} from "./terminalSessions";

const environmentId = EnvironmentId.make("environment-1");
const terminal = { threadId: "thread-1", terminalId: "terminal-1", cwd: "/repo" } as never;

beforeEach(() => {
  harness.queryResults = [];
  harness.queryInputs.length = 0;
  harness.atomValueInputs.length = 0;
  harness.subscribeCalls.length = 0;
  harness.refreshedAtoms.length = 0;
  harness.snapshotValue = {
    metadata: { emptyMetadata: true },
    transcriptRuntime: null,
  };
  harness.producerState = { atom: null, error: null };
  harness.retainedTranscript = null;
  harness.attachProducer.mockClear();
  harness.attachSnapshot.mockClear();
  harness.metadata.mockClear();
  harness.combine.mockClear();
  harness.formatError.mockClear();
  harness.selectRunning.mockClear();
});

describe("useAttachedTerminalSession", () => {
  it("reattaches the selected terminal producer without replacing its process", () => {
    const state = useAttachedTerminalSession({ environmentId, terminal });
    state.reattach();
    expect(harness.refreshedAtoms).toEqual([harness.producerAtom]);
  });

  it("returns the empty state when either scope input is missing", () => {
    expect(useAttachedTerminalSession({ environmentId: null, terminal })).toEqual({
      emptySession: true,
      transcriptRuntime: null,
      reattach: expect.any(Function),
    });
    expect(harness.queryInputs).toEqual([null]);
    expect(harness.attachProducer).not.toHaveBeenCalled();
    expect(harness.attachSnapshot).not.toHaveBeenCalled();

    harness.queryInputs.length = 0;
    expect(useAttachedTerminalSession({ environmentId, terminal: null })).toEqual({
      emptySession: true,
      transcriptRuntime: null,
      reattach: expect.any(Function),
    });
    expect(harness.queryInputs[0]).not.toBeNull();
  });

  it("reads the inert attach snapshot and wires producer startup to a commit subscription", () => {
    const summary = { threadId: "thread-1", terminalId: "terminal-1" };
    harness.snapshotValue = {
      metadata: { generation: 1, revision: 1, status: "running" },
      transcriptRuntime: { kind: "runtime" },
    };
    harness.queryResults = [
      {
        data: [
          { threadId: "other", terminalId: "terminal-1" },
          { threadId: "thread-1", terminalId: "other" },
          summary,
        ],
        error: null,
      },
    ];
    expect(useAttachedTerminalSession({ environmentId, terminal })).toEqual({
      summary,
      metadata: { generation: 1, revision: 1, status: "running" },
      reattach: expect.any(Function),
      status: "running",
      transcriptRuntime: { kind: "runtime" },
    });
    expect(harness.attachProducer).toHaveBeenCalledWith({ environmentId, input: terminal });
    expect(harness.attachSnapshot).toHaveBeenCalledWith({ environmentId, input: terminal });
    expect(harness.atomValueInputs).toEqual([harness.snapshotAtom]);
    expect(harness.subscribeCalls).toMatchObject([
      { atom: harness.producerAtom, options: { immediate: true } },
    ]);
  });

  it("keeps summary metadata while the commit-scoped attach producer is gated off", () => {
    const summary = { threadId: "thread-1", terminalId: "terminal-1", status: "running" };
    harness.queryResults = [{ data: [summary], error: null }];

    expect(useAttachedTerminalSession({ environmentId, terminal, attach: false })).toEqual({
      summary,
      metadata: { emptyMetadata: true },
      status: "running",
      transcriptRuntime: null,
      reattach: expect.any(Function),
    });
    expect(harness.attachProducer).not.toHaveBeenCalled();
    expect(harness.attachSnapshot).not.toHaveBeenCalled();
    expect(harness.subscribeCalls[0]?.atom).not.toBe(harness.producerAtom);
  });

  it("preserves the last observed transcript runtime across attach failure until detach", () => {
    harness.snapshotValue = {
      metadata: { emptyMetadata: true },
      transcriptRuntime: { kind: "last-runtime" },
    };
    harness.queryResults = [{ data: undefined, error: null }];
    expect(useAttachedTerminalSession({ environmentId, terminal })).toMatchObject({
      transcriptRuntime: { kind: "last-runtime" },
    });

    harness.queryInputs.length = 0;
    harness.producerState = { atom: harness.producerAtom, error: "attach failed" };
    harness.snapshotValue = {
      metadata: { emptyMetadata: true },
      transcriptRuntime: null,
    };
    harness.queryResults = [{ data: undefined, error: null }];
    expect(useAttachedTerminalSession({ environmentId, terminal })).toEqual({
      summary: null,
      metadata: { emptyMetadata: true },
      status: "error",
      error: "attach failed",
      reattach: expect.any(Function),
      transcriptRuntime: { kind: "last-runtime" },
    });

    harness.queryInputs.length = 0;
    expect(useAttachedTerminalSession({ environmentId, terminal, attach: false })).toMatchObject({
      transcriptRuntime: null,
    });

    harness.queryInputs.length = 0;
    expect(useAttachedTerminalSession({ environmentId, terminal })).toMatchObject({
      transcriptRuntime: null,
    });
  });

  it("records producer failures from the commit subscription callback", () => {
    useAttachedTerminalSession({ environmentId, terminal });
    const cause = { kind: "attach-cause" };

    harness.subscribeCalls[0]?.callback({ _tag: "Failure", cause });

    expect(harness.formatError).toHaveBeenCalledWith(cause);
    expect(harness.producerState).toEqual({
      atom: harness.producerAtom,
      error: "formatted attach failure",
    });
  });
});

describe("useKnownTerminalSessions", () => {
  it("returns no sessions without an environment or metadata", () => {
    expect(useKnownTerminalSessions({ environmentId: null, threadId: null })).toEqual([]);
    expect(harness.queryInputs).toEqual([null]);

    harness.queryInputs.length = 0;
    harness.queryResults = [{ data: null, error: null }];
    expect(useKnownTerminalSessions({ environmentId, threadId: null })).toEqual([]);
  });

  it("filters by thread and sorts numeric terminal ids", () => {
    harness.queryResults = [
      {
        data: [
          { threadId: "thread-1", terminalId: "terminal-10" },
          { threadId: "other", terminalId: "terminal-1" },
          { threadId: "thread-1", terminalId: "terminal-2" },
        ],
        error: null,
      },
    ];
    const sessions = useKnownTerminalSessions({
      environmentId,
      threadId: ThreadId.make("thread-1"),
    });
    expect(sessions.map((session) => session.target.terminalId)).toEqual([
      "terminal-2",
      "terminal-10",
    ]);

    harness.queryInputs.length = 0;
    harness.queryResults = [
      {
        data: [
          { threadId: "thread-2", terminalId: "terminal-1" },
          { threadId: "thread-1", terminalId: "terminal-2" },
        ],
        error: null,
      },
    ];
    expect(useKnownTerminalSessions({ environmentId, threadId: null })).toHaveLength(2);
  });

  it("selects running subprocess terminal ids", () => {
    harness.queryResults = [{ data: [], error: null }];
    expect(
      useThreadRunningTerminalIds({ environmentId, threadId: ThreadId.make("thread-1") }),
    ).toEqual([]);
    expect(harness.selectRunning).toHaveBeenCalledWith([]);
  });
});
