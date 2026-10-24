import { ProviderDriverKind } from "@bibcode/contracts";
import { describe, expect, it } from "vite-plus/test";
import {
  computeStableMessagesTimelineRows,
  computeMessageDurationStart,
  deriveMessagesTimelineRows,
  normalizeCompactToolLabel,
  resolveAssistantMessageCopyState,
  resolveTimelineIsAtEnd,
  resolveTimelineMinimapHasPersistentGutter,
  resolveTimelineMinimapHeightStyle,
  resolveTimelineMinimapIndexFromPointer,
  resolveTimelineMinimapTopPercent,
  type MessagesTimelineRow,
} from "./MessagesTimeline.logic";

describe("timeline minimap geometry", () => {
  it("resolves legacy and near-end state values", () => {
    expect(resolveTimelineIsAtEnd(undefined)).toBeUndefined();
    expect(resolveTimelineIsAtEnd({ isAtEnd: true })).toBe(true);
    expect(resolveTimelineIsAtEnd({ isAtEnd: true, isNearEnd: false })).toBe(false);
  });

  it("clamps minimap height and marker positions", () => {
    expect(resolveTimelineMinimapHeightStyle(0)).toContain("1px");
    expect(resolveTimelineMinimapTopPercent(1, 1)).toBe(0);
    expect(resolveTimelineMinimapTopPercent(-2, 3)).toBe(0);
    expect(resolveTimelineMinimapTopPercent(9, 3)).toBe(100);
  });

  it("rejects unusable pointer geometry and rounds valid pointers", () => {
    expect(
      resolveTimelineMinimapIndexFromPointer({
        itemCount: 0,
        railTop: 10,
        railHeight: 100,
        pointerY: 20,
      }),
    ).toBeNull();
    expect(
      resolveTimelineMinimapIndexFromPointer({
        itemCount: 2,
        railTop: 10,
        railHeight: 0,
        pointerY: 20,
      }),
    ).toBeNull();
    expect(
      resolveTimelineMinimapIndexFromPointer({
        itemCount: 1,
        railTop: 10,
        railHeight: 100,
        pointerY: 80,
      }),
    ).toBe(0);
    expect(
      resolveTimelineMinimapIndexFromPointer({
        itemCount: 5,
        railTop: 10,
        railHeight: 100,
        pointerY: 58,
      }),
    ).toBe(2);
  });

  it("requires a finite viewport with enough side gutter", () => {
    expect(resolveTimelineMinimapHasPersistentGutter(Number.NaN)).toBe(false);
    expect(resolveTimelineMinimapHasPersistentGutter(0)).toBe(false);
    expect(resolveTimelineMinimapHasPersistentGutter(800)).toBe(false);
    expect(resolveTimelineMinimapHasPersistentGutter(900)).toBe(true);
  });
});

describe("computeMessageDurationStart", () => {
  it("returns message createdAt when there is no preceding user message", () => {
    const result = computeMessageDurationStart([
      {
        id: "a1",
        role: "assistant",
        createdAt: "2026-01-01T00:00:05Z",
        updatedAt: "2026-01-01T00:00:10Z",
        streaming: false,
      },
    ]);
    expect(result).toEqual(new Map([["a1", "2026-01-01T00:00:05Z"]]));
  });

  it("uses the user message createdAt for the first assistant response", () => {
    const result = computeMessageDurationStart([
      {
        id: "u1",
        role: "user",
        createdAt: "2026-01-01T00:00:00Z",
        updatedAt: "2026-01-01T00:00:00Z",
        streaming: false,
      },
      {
        id: "a1",
        role: "assistant",
        createdAt: "2026-01-01T00:00:30Z",
        updatedAt: "2026-01-01T00:00:30Z",
        streaming: false,
      },
    ]);

    expect(result).toEqual(
      new Map([
        ["u1", "2026-01-01T00:00:00Z"],
        ["a1", "2026-01-01T00:00:00Z"],
      ]),
    );
  });

  it("uses the previous completed assistant updatedAt for subsequent assistant responses", () => {
    const result = computeMessageDurationStart([
      {
        id: "u1",
        role: "user",
        createdAt: "2026-01-01T00:00:00Z",
        updatedAt: "2026-01-01T00:00:00Z",
        streaming: false,
      },
      {
        id: "a1",
        role: "assistant",
        createdAt: "2026-01-01T00:00:30Z",
        updatedAt: "2026-01-01T00:00:30Z",
        streaming: false,
      },
      {
        id: "a2",
        role: "assistant",
        createdAt: "2026-01-01T00:00:55Z",
        updatedAt: "2026-01-01T00:00:55Z",
        streaming: false,
      },
    ]);

    expect(result).toEqual(
      new Map([
        ["u1", "2026-01-01T00:00:00Z"],
        ["a1", "2026-01-01T00:00:00Z"],
        ["a2", "2026-01-01T00:00:30Z"],
      ]),
    );
  });

  it("does not advance the boundary for a streaming message", () => {
    const result = computeMessageDurationStart([
      {
        id: "u1",
        role: "user",
        createdAt: "2026-01-01T00:00:00Z",
        updatedAt: "2026-01-01T00:00:00Z",
        streaming: false,
      },
      {
        id: "a1",
        role: "assistant",
        createdAt: "2026-01-01T00:00:30Z",
        updatedAt: "2026-01-01T00:00:40Z",
        streaming: true,
      },
      {
        id: "a2",
        role: "assistant",
        createdAt: "2026-01-01T00:00:55Z",
        updatedAt: "2026-01-01T00:00:55Z",
        streaming: false,
      },
    ]);

    expect(result).toEqual(
      new Map([
        ["u1", "2026-01-01T00:00:00Z"],
        ["a1", "2026-01-01T00:00:00Z"],
        ["a2", "2026-01-01T00:00:00Z"],
      ]),
    );
  });

  it("resets the boundary on a new user message", () => {
    const result = computeMessageDurationStart([
      {
        id: "u1",
        role: "user",
        createdAt: "2026-01-01T00:00:00Z",
        updatedAt: "2026-01-01T00:00:00Z",
        streaming: false,
      },
      {
        id: "a1",
        role: "assistant",
        createdAt: "2026-01-01T00:00:30Z",
        updatedAt: "2026-01-01T00:00:30Z",
        streaming: false,
      },
      {
        id: "u2",
        role: "user",
        createdAt: "2026-01-01T00:01:00Z",
        updatedAt: "2026-01-01T00:01:00Z",
        streaming: false,
      },
      {
        id: "a2",
        role: "assistant",
        createdAt: "2026-01-01T00:01:20Z",
        updatedAt: "2026-01-01T00:01:20Z",
        streaming: false,
      },
    ]);

    expect(result).toEqual(
      new Map([
        ["u1", "2026-01-01T00:00:00Z"],
        ["a1", "2026-01-01T00:00:00Z"],
        ["u2", "2026-01-01T00:01:00Z"],
        ["a2", "2026-01-01T00:01:00Z"],
      ]),
    );
  });

  it("handles system messages without affecting the boundary", () => {
    const result = computeMessageDurationStart([
      {
        id: "u1",
        role: "user",
        createdAt: "2026-01-01T00:00:00Z",
        updatedAt: "2026-01-01T00:00:00Z",
        streaming: false,
      },
      {
        id: "s1",
        role: "system",
        createdAt: "2026-01-01T00:00:01Z",
        updatedAt: "2026-01-01T00:00:01Z",
        streaming: false,
      },
      {
        id: "a1",
        role: "assistant",
        createdAt: "2026-01-01T00:00:30Z",
        updatedAt: "2026-01-01T00:00:30Z",
        streaming: false,
      },
    ]);

    expect(result).toEqual(
      new Map([
        ["u1", "2026-01-01T00:00:00Z"],
        ["s1", "2026-01-01T00:00:00Z"],
        ["a1", "2026-01-01T00:00:00Z"],
      ]),
    );
  });

  it("returns empty map for empty input", () => {
    expect(computeMessageDurationStart([])).toEqual(new Map());
  });
});

describe("normalizeCompactToolLabel", () => {
  it("removes trailing completion wording from command labels", () => {
    expect(normalizeCompactToolLabel("Ran command complete")).toBe("Ran command");
  });

  it("removes trailing completion wording from other labels", () => {
    expect(normalizeCompactToolLabel("Read file completed")).toBe("Read file");
  });
});

describe("resolveAssistantMessageCopyState", () => {
  it("returns enabled copy state for completed assistant messages", () => {
    expect(
      resolveAssistantMessageCopyState({
        showCopyButton: true,
        text: "Ship it",
        streaming: false,
      }),
    ).toEqual({
      text: "Ship it",
      visible: true,
    });
  });

  it("hides copy while an assistant message is still streaming", () => {
    expect(
      resolveAssistantMessageCopyState({
        showCopyButton: true,
        text: "Still streaming",
        streaming: true,
      }),
    ).toEqual({
      text: "Still streaming",
      visible: false,
    });
  });

  it("hides copy for empty completed assistant messages", () => {
    expect(
      resolveAssistantMessageCopyState({
        showCopyButton: true,
        text: "   ",
        streaming: false,
      }),
    ).toEqual({
      text: null,
      visible: false,
    });
  });

  it("hides copy for non-terminal assistant messages", () => {
    expect(
      resolveAssistantMessageCopyState({
        showCopyButton: false,
        text: "Interim thought",
        streaming: false,
      }),
    ).toEqual({
      text: "Interim thought",
      visible: false,
    });
  });
});

describe("deriveMessagesTimelineRows", () => {
  it("only enables assistant copy for the terminal assistant message in a turn", () => {
    const rows = deriveMessagesTimelineRows({
      timelineEntries: [
        {
          id: "user-1-entry",
          kind: "message",
          createdAt: "2026-01-01T00:00:00Z",
          message: {
            id: "user-1" as never,
            role: "user",
            text: "Write a poem",
            turnId: null,
            createdAt: "2026-01-01T00:00:00Z",
            updatedAt: "2026-01-01T00:00:00Z",
            streaming: false,
          },
        },
        {
          id: "assistant-thought-entry",
          kind: "message",
          createdAt: "2026-01-01T00:00:10Z",
          message: {
            id: "assistant-thought" as never,
            role: "assistant",
            text: "I should ground this first.",
            turnId: "turn-1" as never,
            createdAt: "2026-01-01T00:00:10Z",
            updatedAt: "2026-01-01T00:00:11Z",
            streaming: false,
          },
        },
        {
          id: "assistant-final-entry",
          kind: "message",
          createdAt: "2026-01-01T00:00:20Z",
          message: {
            id: "assistant-final" as never,
            role: "assistant",
            text: "Here is the poem.",
            turnId: "turn-1" as never,
            createdAt: "2026-01-01T00:00:20Z",
            updatedAt: "2026-01-01T00:00:30Z",
            streaming: false,
          },
        },
      ],
      expandedTurnIds: new Set(["turn-1" as never]),
      isWorking: false,
      activeTurnStartedAt: null,
      turnDiffSummaryByAssistantMessageId: new Map(),
      revertTurnCountByUserMessageId: new Map(),
    });

    const assistantRows = rows.filter(
      (row): row is Extract<(typeof rows)[number], { kind: "message" }> =>
        row.kind === "message" && row.message.role === "assistant",
    );

    expect(assistantRows).toHaveLength(2);
    expect(assistantRows[0]?.showAssistantCopyButton).toBe(false);
    expect(assistantRows[1]?.showAssistantCopyButton).toBe(true);
  });

  it("marks only the active assistant turn as streaming for copy controls", () => {
    const rows = deriveMessagesTimelineRows({
      timelineEntries: [
        {
          id: "assistant-one-entry",
          kind: "message",
          createdAt: "2026-01-01T00:00:10Z",
          message: {
            id: "assistant-one" as never,
            role: "assistant",
            text: "Earlier response.",
            turnId: "turn-1" as never,
            createdAt: "2026-01-01T00:00:10Z",
            updatedAt: "2026-01-01T00:00:11Z",
            streaming: false,
          },
        },
        {
          id: "assistant-two-entry",
          kind: "message",
          createdAt: "2026-01-01T00:00:20Z",
          message: {
            id: "assistant-two" as never,
            role: "assistant",
            text: "Active response.",
            turnId: "turn-2" as never,
            createdAt: "2026-01-01T00:00:20Z",
            updatedAt: "2026-01-01T00:00:30Z",
            streaming: false,
          },
        },
      ],
      latestTurn: {
        turnId: "turn-2" as never,
        state: "running",
        startedAt: "2026-01-01T00:00:19Z",
        completedAt: null,
      },
      isWorking: false,
      activeTurnStartedAt: null,
      turnDiffSummaryByAssistantMessageId: new Map(),
      revertTurnCountByUserMessageId: new Map(),
    });

    const assistantRows = rows.filter(
      (row): row is Extract<(typeof rows)[number], { kind: "message" }> =>
        row.kind === "message" && row.message.role === "assistant",
    );

    expect(assistantRows[0]?.assistantCopyStreaming).toBe(false);
    expect(assistantRows[1]?.assistantCopyStreaming).toBe(true);
  });

  it("projects assistant diff summaries and user revert counts onto the affected rows", () => {
    const assistantTurnDiffSummary = {
      turnId: "turn-1" as never,
      completedAt: "2026-01-01T00:00:30Z",
      assistantMessageId: "assistant-1" as never,
      checkpointTurnCount: 2,
      checkpointRef: "checkpoint-1" as never,
      status: "ready" as const,
      files: [{ path: "src/index.ts", kind: "modified", additions: 3, deletions: 1 }],
    };

    const rows = deriveMessagesTimelineRows({
      timelineEntries: [
        {
          id: "user-entry",
          kind: "message",
          createdAt: "2026-01-01T00:00:00Z",
          message: {
            id: "user-1" as never,
            role: "user",
            text: "Do the thing",
            turnId: null,
            createdAt: "2026-01-01T00:00:00Z",
            updatedAt: "2026-01-01T00:00:00Z",
            streaming: false,
          },
        },
        {
          id: "assistant-entry",
          kind: "message",
          createdAt: "2026-01-01T00:00:20Z",
          message: {
            id: "assistant-1" as never,
            role: "assistant",
            text: "Done",
            turnId: "turn-1" as never,
            createdAt: "2026-01-01T00:00:20Z",
            updatedAt: "2026-01-01T00:00:30Z",
            streaming: false,
          },
        },
      ],
      isWorking: false,
      activeTurnStartedAt: null,
      turnDiffSummaryByAssistantMessageId: new Map([
        ["assistant-1" as never, assistantTurnDiffSummary],
      ]),
      revertTurnCountByUserMessageId: new Map([["user-1" as never, 1]]),
    });

    const userRow = rows.find(
      (row): row is Extract<(typeof rows)[number], { kind: "message" }> =>
        row.kind === "message" && row.message.role === "user",
    );
    const assistantRow = rows.find(
      (row): row is Extract<(typeof rows)[number], { kind: "message" }> =>
        row.kind === "message" && row.message.role === "assistant",
    );

    expect(userRow?.revertTurnCount).toBe(1);
    expect(assistantRow?.assistantTurnDiffSummary).toBe(assistantTurnDiffSummary);
  });

  it("folds settled-turn commentary and work behind a Worked-for row", () => {
    const timelineEntries = [
      {
        id: "user-entry",
        kind: "message" as const,
        createdAt: "2026-01-01T00:00:00Z",
        message: {
          id: "user-1" as never,
          role: "user" as const,
          text: "Build it",
          turnId: null,
          createdAt: "2026-01-01T00:00:00Z",
          updatedAt: "2026-01-01T00:00:00Z",
          streaming: false,
        },
      },
      {
        id: "assistant-thought-entry",
        kind: "message" as const,
        createdAt: "2026-01-01T00:00:05Z",
        message: {
          id: "assistant-thought" as never,
          role: "assistant" as const,
          text: "Looking around first.",
          turnId: "turn-1" as never,
          createdAt: "2026-01-01T00:00:05Z",
          updatedAt: "2026-01-01T00:00:06Z",
          streaming: false,
        },
      },
      {
        id: "work-entry-1",
        kind: "work" as const,
        createdAt: "2026-01-01T00:00:08Z",
        entry: {
          id: "work-1",
          createdAt: "2026-01-01T00:00:08Z",
          turnId: "turn-1" as never,
          label: "Ran command",
          tone: "tool" as const,
        },
      },
      {
        id: "assistant-final-entry",
        kind: "message" as const,
        createdAt: "2026-01-01T00:00:20Z",
        message: {
          id: "assistant-final" as never,
          role: "assistant" as const,
          text: "Done",
          turnId: "turn-1" as never,
          createdAt: "2026-01-01T00:00:20Z",
          updatedAt: "2026-01-01T00:00:22Z",
          streaming: false,
        },
      },
    ];

    const collapsedRows = deriveMessagesTimelineRows({
      timelineEntries,
      isWorking: false,
      activeTurnStartedAt: null,
      turnDiffSummaryByAssistantMessageId: new Map(),
      revertTurnCountByUserMessageId: new Map(),
    });

    const foldRow = collapsedRows.find(
      (row): row is Extract<(typeof collapsedRows)[number], { kind: "turn-fold" }> =>
        row.kind === "turn-fold",
    );
    expect(foldRow?.turnId).toBe("turn-1");
    expect(foldRow?.expanded).toBe(false);
    // User message boundary (00:00:00) → terminal message updatedAt (00:00:22).
    expect(foldRow?.label).toBe("Worked for 22s");
    expect(collapsedRows.map((row) => row.id)).toEqual([
      "user-entry",
      "turn-fold:turn-1",
      "assistant-final-entry",
    ]);

    const expandedRows = deriveMessagesTimelineRows({
      timelineEntries,
      expandedTurnIds: new Set(["turn-1" as never]),
      isWorking: false,
      activeTurnStartedAt: null,
      turnDiffSummaryByAssistantMessageId: new Map(),
      revertTurnCountByUserMessageId: new Map(),
    });

    expect(expandedRows.map((row) => row.id)).toEqual([
      "user-entry",
      "turn-fold:turn-1",
      "assistant-thought-entry",
      "work-entry-1",
      "assistant-final-entry",
    ]);
    expect(
      expandedRows.find((row) => row.kind === "turn-fold" && row.expanded === true),
    ).toBeDefined();
  });

  it("derives a sane duration for a steer-superseded turn with one instant commentary message", () => {
    // A steer ends the previous turn early: its only message completes the
    // instant it is created, and trailing work entries land after it. The
    // fold duration must span from the user message that started the turn to
    // the last entry, not message createdAt → message updatedAt (~0ms).
    const rows = deriveMessagesTimelineRows({
      timelineEntries: [
        {
          id: "user-entry",
          kind: "message",
          createdAt: "2026-01-01T00:00:00Z",
          message: {
            id: "user-1" as never,
            role: "user" as const,
            text: "do it once more",
            turnId: null,
            createdAt: "2026-01-01T00:00:00Z",
            updatedAt: "2026-01-01T00:00:00Z",
            streaming: false,
          },
        },
        {
          id: "assistant-commentary-entry",
          kind: "message",
          createdAt: "2026-01-01T00:00:09Z",
          message: {
            id: "assistant-commentary" as never,
            role: "assistant" as const,
            text: "Kicking off call 1.",
            turnId: "turn-1" as never,
            createdAt: "2026-01-01T00:00:09Z",
            updatedAt: "2026-01-01T00:00:09Z",
            streaming: false,
          },
        },
        {
          id: "work-entry-1",
          kind: "work",
          createdAt: "2026-01-01T00:00:12Z",
          entry: {
            id: "work-1",
            createdAt: "2026-01-01T00:00:12Z",
            turnId: "turn-1" as never,
            label: "Ran command",
            tone: "tool" as const,
          },
        },
        {
          id: "steer-user-entry",
          kind: "message",
          createdAt: "2026-01-01T00:00:14Z",
          message: {
            id: "user-2" as never,
            role: "user" as const,
            text: "actually do 15",
            turnId: null,
            createdAt: "2026-01-01T00:00:14Z",
            updatedAt: "2026-01-01T00:00:14Z",
            streaming: false,
          },
        },
        {
          id: "assistant-next-turn-entry",
          kind: "message",
          createdAt: "2026-01-01T00:00:17Z",
          message: {
            id: "assistant-next" as never,
            role: "assistant" as const,
            text: "One down — adjusting.",
            turnId: "turn-2" as never,
            createdAt: "2026-01-01T00:00:17Z",
            updatedAt: "2026-01-01T00:00:17Z",
            streaming: true,
          },
        },
      ],
      latestTurn: {
        turnId: "turn-2" as never,
        state: "running",
        startedAt: "2026-01-01T00:00:14Z",
        completedAt: null,
      },
      isWorking: true,
      activeTurnStartedAt: "2026-01-01T00:00:14Z",
      turnDiffSummaryByAssistantMessageId: new Map(),
      revertTurnCountByUserMessageId: new Map(),
    });

    const foldRow = rows.find(
      (row): row is Extract<(typeof rows)[number], { kind: "turn-fold" }> =>
        row.kind === "turn-fold",
    );
    // User message (00:00:00) → trailing work entry (00:00:12).
    expect(foldRow?.turnId).toBe("turn-1");
    expect(foldRow?.label).toBe("Worked for 12s");
  });

  it("uses latest-turn timings and the stopped label for an interrupted latest turn", () => {
    const rows = deriveMessagesTimelineRows({
      timelineEntries: [
        {
          id: "work-entry-1",
          kind: "work",
          createdAt: "2026-01-01T00:00:05Z",
          entry: {
            id: "work-1",
            createdAt: "2026-01-01T00:00:05Z",
            turnId: "turn-1" as never,
            label: "Ran command",
            tone: "tool" as const,
          },
        },
      ],
      latestTurn: {
        turnId: "turn-1" as never,
        state: "interrupted",
        startedAt: "2026-01-01T00:00:00Z",
        completedAt: "2026-01-01T00:00:47Z",
      },
      isWorking: false,
      activeTurnStartedAt: null,
      turnDiffSummaryByAssistantMessageId: new Map(),
      revertTurnCountByUserMessageId: new Map(),
    });

    expect(rows).toEqual([
      expect.objectContaining({
        kind: "turn-fold",
        turnId: "turn-1",
        label: "You stopped after 47s",
        expanded: false,
      }),
    ]);
  });

  it("keeps the previous turn folded while a newly sent message awaits its turn", () => {
    // Right after send, isWorking is true but latestTurn still points at the
    // previous, settled turn — it must stay folded through that window.
    const rows = deriveMessagesTimelineRows({
      timelineEntries: [
        {
          id: "work-entry-1",
          kind: "work",
          createdAt: "2026-01-01T00:00:05Z",
          entry: {
            id: "work-1",
            createdAt: "2026-01-01T00:00:05Z",
            turnId: "turn-1" as never,
            label: "Ran command",
            tone: "tool" as const,
          },
        },
        {
          id: "assistant-final-entry",
          kind: "message",
          createdAt: "2026-01-01T00:00:20Z",
          message: {
            id: "assistant-final" as never,
            role: "assistant",
            text: "Done",
            turnId: "turn-1" as never,
            createdAt: "2026-01-01T00:00:20Z",
            updatedAt: "2026-01-01T00:00:22Z",
            streaming: false,
          },
        },
        {
          id: "user-followup-entry",
          kind: "message",
          createdAt: "2026-01-01T00:01:00Z",
          message: {
            id: "user-followup" as never,
            role: "user",
            text: "yooo",
            turnId: null,
            createdAt: "2026-01-01T00:01:00Z",
            updatedAt: "2026-01-01T00:01:00Z",
            streaming: false,
          },
        },
      ],
      latestTurn: {
        turnId: "turn-1" as never,
        state: "completed",
        startedAt: "2026-01-01T00:00:00Z",
        completedAt: "2026-01-01T00:00:22Z",
      },
      isWorking: true,
      activeTurnStartedAt: "2026-01-01T00:01:00Z",
      turnDiffSummaryByAssistantMessageId: new Map(),
      revertTurnCountByUserMessageId: new Map(),
    });

    expect(rows.map((row) => row.id)).toEqual([
      "turn-fold:turn-1",
      "assistant-final-entry",
      "user-followup-entry",
      "working-indicator-row",
    ]);
    const finalRow = rows.find((row) => row.id === "assistant-final-entry");
    expect(finalRow?.kind === "message" && finalRow.showAssistantMeta).toBe(true);
  });

  it("does not fold the active in-progress turn", () => {
    const rows = deriveMessagesTimelineRows({
      timelineEntries: [
        {
          id: "assistant-thought-entry",
          kind: "message",
          createdAt: "2026-01-01T00:00:05Z",
          message: {
            id: "assistant-thought" as never,
            role: "assistant",
            text: "Working on it.",
            turnId: "turn-1" as never,
            createdAt: "2026-01-01T00:00:05Z",
            updatedAt: "2026-01-01T00:00:06Z",
            streaming: false,
          },
        },
        {
          id: "work-entry-1",
          kind: "work",
          createdAt: "2026-01-01T00:00:08Z",
          entry: {
            id: "work-1",
            createdAt: "2026-01-01T00:00:08Z",
            turnId: "turn-1" as never,
            label: "Ran command",
            tone: "tool" as const,
          },
        },
      ],
      latestTurn: {
        turnId: "turn-1" as never,
        state: "running",
        startedAt: "2026-01-01T00:00:00Z",
        completedAt: null,
      },
      isWorking: true,
      activeTurnStartedAt: "2026-01-01T00:00:00Z",
      turnDiffSummaryByAssistantMessageId: new Map(),
      revertTurnCountByUserMessageId: new Map(),
    });

    expect(rows.some((row) => row.kind === "turn-fold")).toBe(false);
    expect(rows.map((row) => row.id)).toEqual([
      "assistant-thought-entry",
      "work-entry-1",
      "working-indicator-row",
    ]);
  });

  it("does not fold the session's running turn when latestTurn regresses", () => {
    const rows = deriveMessagesTimelineRows({
      timelineEntries: [
        {
          id: "previous-work-entry",
          kind: "work",
          createdAt: "2026-01-01T00:00:05Z",
          entry: {
            id: "previous-work",
            createdAt: "2026-01-01T00:00:05Z",
            turnId: "turn-1" as never,
            label: "Read files",
            tone: "tool" as const,
          },
        },
        {
          id: "user-followup-entry",
          kind: "message",
          createdAt: "2026-01-01T00:01:00Z",
          message: {
            id: "user-followup" as never,
            role: "user",
            text: "continue",
            turnId: null,
            createdAt: "2026-01-01T00:01:00Z",
            updatedAt: "2026-01-01T00:01:00Z",
            streaming: false,
          },
        },
        {
          id: "running-work-entry",
          kind: "work",
          createdAt: "2026-01-01T00:01:05Z",
          entry: {
            id: "running-work",
            createdAt: "2026-01-01T00:01:05Z",
            turnId: "turn-2" as never,
            label: "Searched files",
            tone: "tool" as const,
          },
        },
      ],
      latestTurn: {
        turnId: "turn-1" as never,
        state: "completed",
        startedAt: "2026-01-01T00:00:00Z",
        completedAt: "2026-01-01T00:00:25Z",
      },
      runningTurnId: "turn-2" as never,
      isWorking: true,
      activeTurnStartedAt: "2026-01-01T00:01:00Z",
      turnDiffSummaryByAssistantMessageId: new Map(),
      revertTurnCountByUserMessageId: new Map(),
    });

    expect(rows.filter((row) => row.kind === "turn-fold").map((row) => row.turnId)).toEqual([
      "turn-1",
    ]);
    expect(rows.map((row) => row.id)).toContain("running-work-entry");
  });

  it("only shows assistant metadata on the terminal assistant message", () => {
    const rows = deriveMessagesTimelineRows({
      timelineEntries: [
        {
          id: "assistant-thought-entry",
          kind: "message",
          createdAt: "2026-01-01T00:00:10Z",
          message: {
            id: "assistant-thought" as never,
            role: "assistant",
            text: "Checking first.",
            turnId: "turn-1" as never,
            createdAt: "2026-01-01T00:00:10Z",
            updatedAt: "2026-01-01T00:00:11Z",
            streaming: false,
          },
        },
        {
          id: "assistant-final-entry",
          kind: "message",
          createdAt: "2026-01-01T00:00:20Z",
          message: {
            id: "assistant-final" as never,
            role: "assistant",
            text: "Done.",
            turnId: "turn-1" as never,
            createdAt: "2026-01-01T00:00:20Z",
            updatedAt: "2026-01-01T00:00:30Z",
            streaming: false,
          },
        },
      ],
      expandedTurnIds: new Set(["turn-1" as never]),
      isWorking: false,
      activeTurnStartedAt: null,
      turnDiffSummaryByAssistantMessageId: new Map(),
      revertTurnCountByUserMessageId: new Map(),
    });

    const assistantRows = rows.filter(
      (row): row is Extract<(typeof rows)[number], { kind: "message" }> =>
        row.kind === "message" && row.message.role === "assistant",
    );

    expect(assistantRows.map((row) => row.showAssistantMeta)).toEqual([false, true]);
  });

  it("withholds assistant metadata while the active turn is still in progress", () => {
    const rows = deriveMessagesTimelineRows({
      timelineEntries: [
        {
          id: "assistant-thought-entry",
          kind: "message",
          createdAt: "2026-01-01T00:00:10Z",
          message: {
            id: "assistant-thought" as never,
            role: "assistant",
            text: "Working on it.",
            turnId: "turn-1" as never,
            createdAt: "2026-01-01T00:00:10Z",
            updatedAt: "2026-01-01T00:00:11Z",
            streaming: false,
          },
        },
      ],
      latestTurn: {
        turnId: "turn-1" as never,
        state: "running",
        startedAt: "2026-01-01T00:00:00Z",
        completedAt: null,
      },
      isWorking: true,
      activeTurnStartedAt: "2026-01-01T00:00:00Z",
      turnDiffSummaryByAssistantMessageId: new Map(),
      revertTurnCountByUserMessageId: new Map(),
    });

    const assistantRow = rows.find(
      (row): row is Extract<(typeof rows)[number], { kind: "message" }> =>
        row.kind === "message" && row.message.role === "assistant",
    );

    expect(assistantRow?.showAssistantMeta).toBe(false);
    expect(assistantRow?.showAssistantCopyButton).toBe(false);
  });

  it("models work log overflow expansion as inserted list rows", () => {
    const timelineEntries = [
      {
        id: "work-entry-1",
        kind: "work" as const,
        createdAt: "2026-01-01T00:00:01Z",
        entry: {
          id: "work-1",
          createdAt: "2026-01-01T00:00:01Z",
          label: "read",
          detail: "Reading package.json",
          tone: "tool" as const,
        },
      },
      {
        id: "work-entry-2",
        kind: "work" as const,
        createdAt: "2026-01-01T00:00:02Z",
        entry: {
          id: "work-2",
          createdAt: "2026-01-01T00:00:02Z",
          label: "edit",
          detail: "Editing MessagesTimeline.tsx",
          tone: "tool" as const,
        },
      },
      {
        id: "work-entry-3",
        kind: "work" as const,
        createdAt: "2026-01-01T00:00:03Z",
        entry: {
          id: "work-3",
          createdAt: "2026-01-01T00:00:03Z",
          label: "test",
          detail: "Running tests",
          tone: "tool" as const,
        },
      },
    ];

    const baseInput = {
      timelineEntries,
      isWorking: false,
      activeTurnStartedAt: null,
      turnDiffSummaryByAssistantMessageId: new Map(),
      revertTurnCountByUserMessageId: new Map(),
    };
    const collapsedRows = deriveMessagesTimelineRows(baseInput);
    const expandedRows = deriveMessagesTimelineRows({
      ...baseInput,
      expandedWorkGroupIds: new Set(["work-group:work-entry-1"]),
    });

    expect(collapsedRows.map((row) => row.id)).toEqual(["work-3", "work-toggle:work-entry-1"]);
    expect(collapsedRows.find((row) => row.kind === "work-toggle")).toMatchObject({
      groupId: "work-group:work-entry-1",
      hiddenCount: 2,
      expanded: false,
      onlyToolEntries: true,
    });
    expect(expandedRows.map((row) => row.id)).toEqual([
      "work-1",
      "work-2",
      "work-3",
      "work-toggle:work-entry-1",
    ]);
    expect(expandedRows.find((row) => row.kind === "work-toggle")).toMatchObject({
      expanded: true,
    });
  });

  it("keeps system messages while excluding neutral in-progress tool rows", () => {
    const rows = deriveMessagesTimelineRows({
      timelineEntries: [
        {
          id: "system-entry",
          kind: "message",
          createdAt: "2026-01-01T00:00:00Z",
          message: {
            id: "system" as never,
            role: "system",
            text: "System note",
            turnId: null,
            createdAt: "2026-01-01T00:00:00Z",
            updatedAt: "2026-01-01T00:00:00Z",
            streaming: false,
          },
        },
        {
          id: "work-entry",
          kind: "work",
          createdAt: "2026-01-01T00:00:01Z",
          entry: {
            id: "work",
            createdAt: "2026-01-01T00:00:01Z",
            label: "running",
            detail: "Still running",
            tone: "tool",
            toolLifecycleStatus: "inProgress",
          },
        },
      ],
      isWorking: false,
      activeTurnStartedAt: null,
      turnDiffSummaryByAssistantMessageId: new Map(),
      revertTurnCountByUserMessageId: new Map(),
    });

    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({ kind: "message", id: "system-entry" });
  });

  it("does not fold a settled group containing a streaming message", () => {
    const assistant = (id: string, streaming: boolean, offset: number) => ({
      id: `${id}-entry`,
      kind: "message" as const,
      createdAt: `2026-01-01T00:00:0${offset}Z`,
      message: {
        id: id as never,
        role: "assistant" as const,
        text: id,
        turnId: "streamed-turn" as never,
        createdAt: `2026-01-01T00:00:0${offset}Z`,
        updatedAt: `2026-01-01T00:00:0${offset}Z`,
        streaming,
      },
    });
    const rows = deriveMessagesTimelineRows({
      timelineEntries: [assistant("partial", true, 1), assistant("final", false, 2)],
      isWorking: false,
      activeTurnStartedAt: null,
      turnDiffSummaryByAssistantMessageId: new Map(),
      revertTurnCountByUserMessageId: new Map(),
    });

    expect(rows.some((row) => row.kind === "turn-fold")).toBe(false);
  });

  it("renders unkeyed assistant messages and proposed plans", () => {
    const rows = deriveMessagesTimelineRows({
      timelineEntries: [
        {
          id: "assistant-entry",
          kind: "message",
          createdAt: "2026-01-01T00:00:00Z",
          message: {
            id: "assistant" as never,
            role: "assistant",
            text: "Done",
            turnId: null,
            createdAt: "2026-01-01T00:00:00Z",
            updatedAt: "2026-01-01T00:00:01Z",
            streaming: false,
          },
        },
        {
          id: "plan-entry",
          kind: "proposed-plan",
          createdAt: "2026-01-01T00:00:02Z",
          proposedPlan: { steps: [] } as never,
        },
      ],
      isWorking: false,
      activeTurnStartedAt: null,
      turnDiffSummaryByAssistantMessageId: new Map(),
      revertTurnCountByUserMessageId: new Map(),
    });

    expect(rows.map((row) => row.kind)).toEqual(["message", "proposed-plan"]);
  });

  it("uses fallback fold labels when timestamps are invalid", () => {
    const turnId = "invalid-turn" as never;
    const rows = deriveMessagesTimelineRows({
      timelineEntries: [
        {
          id: "work-entry",
          kind: "work",
          createdAt: "invalid",
          entry: {
            id: "work",
            turnId,
            createdAt: "invalid",
            label: "thinking",
            detail: "Working",
            tone: "thinking",
          },
        },
      ],
      latestTurn: {
        turnId,
        state: "interrupted",
        startedAt: "invalid",
        completedAt: "2026-01-01T00:00:01Z",
      },
      isWorking: false,
      activeTurnStartedAt: null,
      turnDiffSummaryByAssistantMessageId: new Map(),
      revertTurnCountByUserMessageId: new Map(),
    });

    expect(rows.find((row) => row.kind === "turn-fold")).toMatchObject({
      label: "You stopped this response",
    });
  });

  it("keeps a valid terminal timestamp when trailing work has an invalid timestamp", () => {
    const turnId = "trailing-work-turn" as never;
    const rows = deriveMessagesTimelineRows({
      timelineEntries: [
        {
          id: "assistant-entry",
          kind: "message",
          createdAt: "2026-01-01T00:00:00Z",
          message: {
            id: "assistant" as never,
            role: "assistant",
            text: "Done",
            turnId,
            createdAt: "2026-01-01T00:00:00Z",
            updatedAt: "2026-01-01T00:00:01Z",
            streaming: false,
          },
        },
        {
          id: "work-entry",
          kind: "work",
          createdAt: "invalid",
          entry: {
            id: "work",
            turnId,
            createdAt: "invalid",
            label: "thinking",
            detail: "Trailing work",
            tone: "thinking",
          },
        },
      ],
      isWorking: false,
      activeTurnStartedAt: null,
      turnDiffSummaryByAssistantMessageId: new Map(),
      revertTurnCountByUserMessageId: new Map(),
    });

    expect(rows.find((row) => row.kind === "turn-fold")).toMatchObject({
      label: "Worked for 1.0s",
    });
  });
});

describe("settled turn answer visibility", () => {
  const turnId = "turn-1" as never;
  const at = (seconds: number) => new Date(Date.UTC(2026, 0, 1, 0, 0, seconds)).toISOString();
  const user = {
    id: "user-entry",
    kind: "message" as const,
    createdAt: at(0),
    message: {
      id: "user-1" as never,
      role: "user" as const,
      text: "Resolve the conflicts",
      turnId: null,
      createdAt: at(0),
      updatedAt: at(0),
      streaming: false,
    },
  };
  const assistant = (id: string, text: string, seconds: number) => ({
    id: `${id}-entry`,
    kind: "message" as const,
    createdAt: at(seconds),
    message: {
      id: id as never,
      role: "assistant" as const,
      text,
      turnId,
      createdAt: at(seconds),
      updatedAt: at(seconds + 1),
      streaming: false,
    },
  });
  const work = (id: string, seconds: number) => ({
    id: `${id}-entry`,
    kind: "work" as const,
    createdAt: at(seconds),
    entry: {
      id,
      createdAt: at(seconds),
      turnId,
      label: "Ran command",
      tone: "tool" as const,
    },
  });
  const answerText = [
    "## Conflicts resolved",
    "",
    "- `src/app.ts`: kept the new router and the old guard.",
    "- `src/build.ts`: took the release branch version.",
  ].join("\n");
  const base = {
    isWorking: false,
    activeTurnStartedAt: null,
    turnDiffSummaryByAssistantMessageId: new Map(),
    revertTurnCountByUserMessageId: new Map(),
  };
  const messageRow = (rows: MessagesTimelineRow[], id: string) =>
    rows.find(
      (row): row is Extract<MessagesTimelineRow, { kind: "message" }> =>
        row.kind === "message" && row.id === id,
    );

  it("keeps a substantive answer visible when the turn ends on a status line", () => {
    const rows = deriveMessagesTimelineRows({
      ...base,
      timelineEntries: [
        user,
        assistant("thought", "Looking at the conflicts first.", 5),
        work("work-1", 8),
        assistant("answer", answerText, 20),
        work("work-2", 25),
        assistant("status", "Waiting for the build.", 30),
      ],
    });

    expect(rows.map((row) => row.id)).toEqual([
      "user-entry",
      "turn-fold:turn-1",
      "answer-entry",
      "status-entry",
    ]);
    // The answer is a finished answer: it can be copied like the last message.
    expect(messageRow(rows, "answer-entry")?.showAssistantCopyButton).toBe(true);
    expect(messageRow(rows, "status-entry")?.showAssistantMeta).toBe(true);
  });

  it("keeps a long unformatted answer visible above a closing status line", () => {
    const longAnswer =
      "Good, this is very close to the reference now. Three fixes came out of that comparison: " +
      "the header spacing was off by one step, the sidebar used the wrong muted token, and the " +
      "empty state text wrapped too early on narrow windows. All three are fixed in this commit.";
    const rows = deriveMessagesTimelineRows({
      ...base,
      timelineEntries: [
        user,
        work("work-1", 8),
        assistant("answer", longAnswer, 20),
        assistant("status", "Now repo housekeeping, then I'll publish a preview link.", 30),
      ],
    });

    expect(rows.map((row) => row.id)).toEqual([
      "user-entry",
      "turn-fold:turn-1",
      "answer-entry",
      "status-entry",
    ]);
  });

  it("still folds short commentary when the last message is the answer", () => {
    const rows = deriveMessagesTimelineRows({
      ...base,
      timelineEntries: [
        user,
        assistant("structured-progress", "Plan:\n- read the logs\n- fix the parser", 5),
        work("work-1", 8),
        assistant("answer", answerText, 20),
      ],
    });

    expect(rows.map((row) => row.id)).toEqual(["user-entry", "turn-fold:turn-1", "answer-entry"]);
  });

  it("does not move a completed turn's answer when later output arrives under its id", () => {
    const latestTurn = {
      turnId,
      state: "completed" as const,
      startedAt: at(0),
      completedAt: at(60),
    };
    const settledEntries = [
      user,
      assistant("thought", "Checking the branch.", 5),
      work("work-1", 8),
      assistant("answer", "Done. The branch is merged.", 50),
    ];

    const settled = deriveMessagesTimelineRows({
      ...base,
      timelineEntries: settledEntries,
      latestTurn,
    });
    expect(settled.map((row) => row.id)).toEqual([
      "user-entry",
      "turn-fold:turn-1",
      "answer-entry",
    ]);

    const withLateOutput = deriveMessagesTimelineRows({
      ...base,
      timelineEntries: [...settledEntries, assistant("late", "Background check finished.", 200)],
      latestTurn,
    });
    expect(withLateOutput.map((row) => row.id)).toEqual([
      "user-entry",
      "turn-fold:turn-1",
      "answer-entry",
      "late-entry",
    ]);
    expect(messageRow(withLateOutput, "answer-entry")?.showAssistantMeta).toBe(true);
    // The fold spans the output that ran past the recorded completion.
    const fold = withLateOutput.find((row) => row.kind === "turn-fold");
    expect(fold?.kind === "turn-fold" && fold.label).toBe("Worked for 3m 21s");
  });
});

describe("working row answer delivery", () => {
  const workingRowFor = (streaming: boolean) => {
    const rows = deriveMessagesTimelineRows({
      timelineEntries: [
        {
          id: "assistant-entry",
          kind: "message",
          createdAt: "2026-01-01T00:00:05Z",
          message: {
            id: "assistant-1" as never,
            role: "assistant" as const,
            text: "Hi! I'm doing well.",
            turnId: "turn-1" as never,
            createdAt: "2026-01-01T00:00:05Z",
            updatedAt: "2026-01-01T00:00:05Z",
            streaming,
          },
        },
      ],
      latestTurn: {
        turnId: "turn-1" as never,
        state: "running",
        startedAt: "2026-01-01T00:00:00Z",
        completedAt: null,
      },
      isWorking: true,
      activeTurnStartedAt: "2026-01-01T00:00:00Z",
      turnDiffSummaryByAssistantMessageId: new Map(),
      revertTurnCountByUserMessageId: new Map(),
    });
    return rows.find(
      (row): row is Extract<(typeof rows)[number], { kind: "working" }> => row.kind === "working",
    );
  };

  it("reports a settled answer on a turn the provider has not closed", () => {
    // Codex sends `turn/completed` tens of seconds after the final answer
    // (measured 19.8s-114.9s), so the turn is still running while the answer is
    // already on screen. Reporting that as a plain wait blamed BiBCode for the
    // provider's lag.
    expect(workingRowFor(false)?.answerDelivered).toBe(true);
  });

  it("still reports a plain wait while the answer is streaming", () => {
    expect(workingRowFor(true)?.answerDelivered).toBe(false);
  });
});

describe("computeStableMessagesTimelineRows", () => {
  it("treats a working row whose answer just landed as changed", () => {
    // `answerDelivered` flips mid-turn while `id` and `createdAt` stay put. If
    // stability compared only those, the row object would be reused and the new
    // label would be computed but never rendered.
    const before: MessagesTimelineRow = {
      kind: "working",
      id: "working-indicator-row",
      createdAt: "2026-08-20T12:00:00.000Z",
      answerDelivered: false,
    };
    const stable = computeStableMessagesTimelineRows([before], {
      byId: new Map(),
      result: [],
    });
    expect(stable.result[0]).toBe(before);

    const after: MessagesTimelineRow = { ...before, answerDelivered: true };
    const next = computeStableMessagesTimelineRows([after], stable);
    expect(next).not.toBe(stable);
    expect(next.result[0]).toBe(after);

    // An identical row is still reused, so the fix does not defeat stability.
    const unchanged = computeStableMessagesTimelineRows([{ ...after }], next);
    expect(unchanged.result[0]).toBe(after);
  });

  it("stabilizes working and proposed-plan rows by their variant fields", () => {
    const working: MessagesTimelineRow = {
      kind: "working",
      id: "working",
      createdAt: null,
      answerDelivered: false,
    };
    const plan = { steps: [] } as never;
    const proposed: MessagesTimelineRow = {
      kind: "proposed-plan",
      id: "plan",
      createdAt: "2026-01-01T00:00:00Z",
      proposedPlan: plan,
    };
    const initial = computeStableMessagesTimelineRows([working, proposed], {
      byId: new Map(),
      result: [],
    });

    expect(computeStableMessagesTimelineRows([{ ...working }, { ...proposed }], initial)).toBe(
      initial,
    );
    expect(
      computeStableMessagesTimelineRows(
        [
          { ...working, createdAt: "2026-01-01T00:00:01Z" },
          { ...proposed, proposedPlan: { steps: [] } as never },
        ],
        initial,
      ),
    ).not.toBe(initial);
  });

  it("detects row-kind and final work-toggle field changes", () => {
    const createdAt = "2026-01-01T00:00:00Z";
    const toggle: MessagesTimelineRow = {
      kind: "work-toggle",
      id: "shared",
      createdAt,
      groupId: "group",
      hiddenCount: 2,
      expanded: false,
      onlyToolEntries: true,
    };
    const initial = computeStableMessagesTimelineRows([toggle], {
      byId: new Map(),
      result: [],
    });
    expect(
      computeStableMessagesTimelineRows([{ ...toggle, onlyToolEntries: false }], initial),
    ).not.toBe(initial);
    expect(
      computeStableMessagesTimelineRows(
        [{ kind: "working", id: "shared", createdAt, answerDelivered: false }],
        initial,
      ),
    ).not.toBe(initial);
  });

  it("checks every turn-fold stability field", () => {
    const fold: MessagesTimelineRow = {
      kind: "turn-fold",
      id: "fold",
      createdAt: "2026-01-01T00:00:00Z",
      turnId: "turn" as never,
      label: "Worked for 1s",
      expanded: false,
    };
    const initial = computeStableMessagesTimelineRows([fold], {
      byId: new Map(),
      result: [],
    });
    expect(computeStableMessagesTimelineRows([{ ...fold }], initial)).toBe(initial);
    expect(
      computeStableMessagesTimelineRows([{ ...fold, createdAt: "2026-01-01T00:00:01Z" }], initial),
    ).not.toBe(initial);
    expect(computeStableMessagesTimelineRows([{ ...fold, label: "Worked" }], initial)).not.toBe(
      initial,
    );
    expect(computeStableMessagesTimelineRows([{ ...fold, expanded: true }], initial)).not.toBe(
      initial,
    );
  });

  it("returns the previous result when row order and content are unchanged", () => {
    const firstUserMessage = {
      id: "user-1" as never,
      role: "user" as const,
      text: "First",
      turnId: null,
      createdAt: "2026-01-01T00:00:00Z",
      updatedAt: "2026-01-01T00:00:00Z",
      streaming: false,
    };
    const secondUserMessage = {
      id: "user-2" as never,
      role: "user" as const,
      text: "Second",
      turnId: null,
      createdAt: "2026-01-01T00:00:10Z",
      updatedAt: "2026-01-01T00:00:10Z",
      streaming: false,
    };

    const rows = deriveMessagesTimelineRows({
      timelineEntries: [
        {
          id: "entry-user-1",
          kind: "message",
          createdAt: firstUserMessage.createdAt,
          message: firstUserMessage,
        },
        {
          id: "entry-user-2",
          kind: "message",
          createdAt: secondUserMessage.createdAt,
          message: secondUserMessage,
        },
      ],
      isWorking: false,
      activeTurnStartedAt: null,
      turnDiffSummaryByAssistantMessageId: new Map(),
      revertTurnCountByUserMessageId: new Map(),
    });

    const initial = computeStableMessagesTimelineRows(rows, {
      byId: new Map(),
      result: [],
    });

    const repeated = computeStableMessagesTimelineRows(rows, initial);

    expect(repeated).toBe(initial);
    expect(repeated.result).toBe(initial.result);
  });

  it("reuses work rows when equivalent timeline derivations create new grouped arrays", () => {
    const firstWorkEntry = {
      id: "work-1",
      createdAt: "2026-01-01T00:00:00Z",
      label: "thinking",
      detail: "Inspecting repository state",
      tone: "thinking" as const,
    };
    const secondWorkEntry = {
      id: "work-2",
      createdAt: "2026-01-01T00:00:01Z",
      label: "read",
      detail: "Reading package.json",
      tone: "tool" as const,
    };

    const createRows = () =>
      deriveMessagesTimelineRows({
        timelineEntries: [
          {
            id: "entry-work-1",
            kind: "work",
            createdAt: firstWorkEntry.createdAt,
            entry: firstWorkEntry,
          },
          {
            id: "entry-work-2",
            kind: "work",
            createdAt: secondWorkEntry.createdAt,
            entry: secondWorkEntry,
          },
        ],
        isWorking: false,
        activeTurnStartedAt: null,
        turnDiffSummaryByAssistantMessageId: new Map(),
        revertTurnCountByUserMessageId: new Map(),
      });

    const firstRows = createRows();
    const initial = computeStableMessagesTimelineRows(firstRows, {
      byId: new Map(),
      result: [],
    });
    const secondRows = createRows();

    expect(secondRows[0]).not.toBe(firstRows[0]);

    const repeated = computeStableMessagesTimelineRows(secondRows, initial);

    expect(repeated).toBe(initial);
    expect(repeated.result[0]).toBe(initial.result[0]);
  });

  it("returns a new result when row order changes without content changes", () => {
    const firstUserMessage = {
      id: "user-1" as never,
      role: "user" as const,
      text: "First",
      turnId: null,
      createdAt: "2026-01-01T00:00:00Z",
      updatedAt: "2026-01-01T00:00:00Z",
      streaming: false,
    };
    const secondUserMessage = {
      id: "user-2" as never,
      role: "user" as const,
      text: "Second",
      turnId: null,
      createdAt: "2026-01-01T00:00:10Z",
      updatedAt: "2026-01-01T00:00:10Z",
      streaming: false,
    };

    const firstRows = deriveMessagesTimelineRows({
      timelineEntries: [
        {
          id: "entry-user-1",
          kind: "message",
          createdAt: firstUserMessage.createdAt,
          message: firstUserMessage,
        },
        {
          id: "entry-user-2",
          kind: "message",
          createdAt: secondUserMessage.createdAt,
          message: secondUserMessage,
        },
      ],
      isWorking: false,
      activeTurnStartedAt: null,
      turnDiffSummaryByAssistantMessageId: new Map(),
      revertTurnCountByUserMessageId: new Map(),
    });

    const initial = computeStableMessagesTimelineRows(firstRows, {
      byId: new Map(),
      result: [],
    });

    const reordered = computeStableMessagesTimelineRows([firstRows[1]!, firstRows[0]!], initial);

    expect(reordered).not.toBe(initial);
    expect(reordered.result).toEqual([initial.result[1], initial.result[0]]);
  });
});

describe("queued timeline rows", () => {
  const message = (
    id: string,
    createdAt: string,
    overrides: Partial<import("../../types").ChatMessage> = {},
  ): import("../../types").ChatMessage => ({
    id: id as never,
    role: "user",
    text: id,
    turnId: null,
    createdAt,
    updatedAt: createdAt,
    streaming: false,
    delivery: { state: "queued", provider: ProviderDriverKind.make("codex"), mode: "start" },
    ...overrides,
  });
  const status = {
    label: "Sends when the turn ends.",
    canSteer: false,
    steerDisabledReason: "This provider cannot steer a running turn",
    canCancel: true,
    steering: false,
    primaryAction: null,
    canSendNow: false,
    sendNowDisabledReason: "Wait for the running turn to end",
  } as const;
  const base = {
    isWorking: true,
    activeTurnStartedAt: null,
    turnDiffSummaryByAssistantMessageId: new Map(),
    revertTurnCountByUserMessageId: new Map(),
  };
  const entry = (message: import("../../types").ChatMessage) => ({
    kind: "message" as const,
    id: message.id,
    createdAt: message.createdAt,
    message,
  });

  it("appends FIFO cards after working, marks only the head, and never duplicates normal rows", () => {
    const first = message("first", "2026-01-01T00:00:20Z");
    const second = message("second", "2026-01-01T00:00:10Z");
    const rows = deriveMessagesTimelineRows({
      ...base,
      timelineEntries: [entry(second), entry(first)],
      queuedMessages: [first, second],
      queuedStatuses: [status, status],
    });
    expect(rows.map((row) => row.kind)).toEqual(["working", "queued-message", "queued-message"]);
    expect(rows.slice(1)).toMatchObject([
      { message: first, isHead: true },
      { message: second, isHead: false },
    ]);
    const stable = computeStableMessagesTimelineRows(rows, { byId: new Map(), result: [] });
    expect(computeStableMessagesTimelineRows(rows, stable).result).toBe(stable.result);
    const refreshedRows = deriveMessagesTimelineRows({
      ...base,
      timelineEntries: [entry(second), entry(first)],
      queuedMessages: [first, second],
      queuedStatuses: [{ ...status }, { ...status }],
    });
    expect(computeStableMessagesTimelineRows(refreshedRows, stable).result).toBe(stable.result);
  });

  it.each(["pending", "sending"] as const)("keeps %s steers out of normal rows", (state) => {
    const steer = message("steer", "2026-01-01T00:00:20Z", {
      delivery: { state, mode: "steer", provider: ProviderDriverKind.make("codex") },
    });
    const rows = deriveMessagesTimelineRows({
      ...base,
      timelineEntries: [entry(steer)],
      queuedMessages: [steer],
      queuedStatuses: [{ ...status, label: "Steering…", steering: true, canCancel: false }],
    });
    expect(rows.map((row) => row.kind)).toEqual(["working", "queued-message"]);
  });

  it("adds no card for an empty queue", () => {
    expect(
      deriveMessagesTimelineRows({
        ...base,
        timelineEntries: [],
        queuedMessages: [],
        queuedStatuses: [],
      }).map((row) => row.kind),
    ).toEqual(["working"]);
  });

  it("keeps delivered steers inside their original turn and fold without moving the next turn boundary", () => {
    const turnId = "turn-1" as never;
    const user = message("user", "2026-01-01T00:00:00Z", {
      delivery: { state: "delivered", provider: ProviderDriverKind.make("codex") },
    });
    const thought = message("thought", "2026-01-01T00:00:10Z", {
      role: "assistant",
      turnId,
      delivery: undefined,
    });
    const steer = message("steer", "2026-01-01T00:00:20Z", {
      turnId,
      delivery: { state: "delivered", mode: "steer", provider: ProviderDriverKind.make("codex") },
    });
    const answer = message("answer", "2026-01-01T00:00:30Z", {
      role: "assistant",
      turnId,
      delivery: undefined,
    });
    const timelineEntries = [user, thought, steer, answer].map(entry);
    const live = deriveMessagesTimelineRows({ ...base, timelineEntries, runningTurnId: turnId });
    expect(live.map((row) => row.id)).toEqual([
      "user",
      "thought",
      "steer",
      "answer",
      "working-indicator-row",
    ]);
    expect(live.find((row) => row.id === "steer")).toMatchObject({
      kind: "message",
      message: steer,
    });
    const settled = deriveMessagesTimelineRows({ ...base, isWorking: false, timelineEntries });
    expect(settled.map((row) => row.id)).toEqual(["user", "turn-fold:turn-1", "answer"]);
    const expanded = deriveMessagesTimelineRows({
      ...base,
      isWorking: false,
      timelineEntries,
      expandedTurnIds: new Set([turnId]),
    });
    expect(expanded.map((row) => row.id)).toEqual([
      "user",
      "turn-fold:turn-1",
      "thought",
      "steer",
      "answer",
    ]);
  });
});
