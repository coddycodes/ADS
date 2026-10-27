import {
  EventId,
  MessageId,
  ThreadId,
  TurnId,
  type OrchestrationThreadActivity,
} from "@coddy/contracts";
import { describe, expect, it } from "vite-plus/test";

import {
  deriveActiveWorkStartedAt,
  deriveActivePlanState,
  derivePhase,
  derivePendingApprovals,
  derivePendingUserInputs,
  deriveTimelineEntries,
  deriveWorkLogEntries,
  findLatestProposedPlan,
  findSidebarProposedPlan,
  formatDuration,
  formatElapsed,
  hasActionableProposedPlan,
  inferCheckpointTurnCountByTurnId,
  isLatestTurnSettled,
  workEntryIndicatesToolFailure,
  workEntryIndicatesToolNeutralStatus,
  workEntryIndicatesToolSuccess,
  workLogEntryIsToolLike,
} from "./session-logic";

let nextActivityId = 0;

function makeActivity(overrides: {
  id?: string;
  createdAt?: string;
  kind?: string;
  summary?: string;
  tone?: OrchestrationThreadActivity["tone"];
  payload?: Record<string, unknown>;
  turnId?: string;
  sequence?: number;
}): OrchestrationThreadActivity {
  const payload = overrides.payload ?? {};
  return {
    id: EventId.make(overrides.id ?? `activity-${nextActivityId++}`),
    createdAt: overrides.createdAt ?? "2026-02-23T00:00:00.000Z",
    kind: overrides.kind ?? "tool.started",
    summary: overrides.summary ?? "Tool call",
    tone: overrides.tone ?? "tool",
    payload,
    turnId: overrides.turnId ? TurnId.make(overrides.turnId) : null,
    ...(overrides.sequence !== undefined ? { sequence: overrides.sequence } : {}),
  };
}

describe("derivePendingApprovals", () => {
  it("tracks open approvals and removes resolved ones", () => {
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "approval-open",
        createdAt: "2026-02-23T00:00:01.000Z",
        kind: "approval.requested",
        summary: "Command approval requested",
        tone: "approval",
        payload: {
          requestId: "req-1",
          requestKind: "command",
          detail: "bun run lint",
        },
      }),
      makeActivity({
        id: "approval-close",
        createdAt: "2026-02-23T00:00:02.000Z",
        kind: "approval.resolved",
        summary: "Approval resolved",
        tone: "info",
        payload: { requestId: "req-2" },
      }),
      makeActivity({
        id: "approval-closed-request",
        createdAt: "2026-02-23T00:00:01.500Z",
        kind: "approval.requested",
        summary: "File-change approval requested",
        tone: "approval",
        payload: { requestId: "req-2", requestKind: "file-change" },
      }),
    ];

    expect(derivePendingApprovals(activities)).toEqual([
      {
        requestId: "req-1",
        requestKind: "command",
        createdAt: "2026-02-23T00:00:01.000Z",
        detail: "bun run lint",
      },
    ]);
  });

  it("maps canonical requestType payloads into pending approvals", () => {
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "approval-open-request-type",
        createdAt: "2026-02-23T00:00:01.000Z",
        kind: "approval.requested",
        summary: "Command approval requested",
        tone: "approval",
        payload: {
          requestId: "req-request-type",
          requestType: "command_execution_approval",
          detail: "pwd",
        },
      }),
    ];

    expect(derivePendingApprovals(activities)).toEqual([
      {
        requestId: "req-request-type",
        requestKind: "command",
        createdAt: "2026-02-23T00:00:01.000Z",
        detail: "pwd",
      },
    ]);
  });

  it("clears stale pending approvals when provider reports unknown pending request", () => {
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "approval-open-stale",
        createdAt: "2026-02-23T00:00:01.000Z",
        kind: "approval.requested",
        summary: "Command approval requested",
        tone: "approval",
        payload: {
          requestId: "req-stale-1",
          requestKind: "command",
        },
      }),
      makeActivity({
        id: "approval-failed-stale",
        createdAt: "2026-02-23T00:00:02.000Z",
        kind: "provider.approval.respond.failed",
        summary: "Provider approval response failed",
        tone: "error",
        payload: {
          requestId: "req-stale-1",
          detail: "Unknown pending permission request: req-stale-1",
        },
      }),
    ];

    expect(derivePendingApprovals(activities)).toEqual([]);
  });

  it("clears stale pending approvals when the backend marks them stale after restart", () => {
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "approval-open-stale-restart",
        createdAt: "2026-02-23T00:00:01.000Z",
        kind: "approval.requested",
        summary: "Command approval requested",
        tone: "approval",
        payload: {
          requestId: "req-stale-restart-1",
          requestKind: "command",
        },
      }),
      makeActivity({
        id: "approval-failed-stale-restart",
        createdAt: "2026-02-23T00:00:02.000Z",
        kind: "provider.approval.respond.failed",
        summary: "Provider approval response failed",
        tone: "error",
        payload: {
          requestId: "req-stale-restart-1",
          detail:
            "Stale pending approval request: req-stale-restart-1. Provider callback state does not survive app restarts or recovered sessions. Restart the turn to continue.",
        },
      }),
    ];

    expect(derivePendingApprovals(activities)).toEqual([]);
  });
});

describe("derivePendingUserInputs", () => {
  it("tracks open structured prompts and removes resolved ones", () => {
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "user-input-open",
        createdAt: "2026-02-23T00:00:01.000Z",
        kind: "user-input.requested",
        summary: "User input requested",
        tone: "info",
        payload: {
          requestId: "req-user-input-1",
          questions: [
            {
              id: "sandbox_mode",
              header: "Sandbox",
              question: "Which mode should be used?",
              options: [
                {
                  label: "workspace-write",
                  description: "Allow workspace writes only",
                },
              ],
              multiSelect: true,
            },
          ],
        },
      }),
      makeActivity({
        id: "user-input-resolved",
        createdAt: "2026-02-23T00:00:02.000Z",
        kind: "user-input.resolved",
        summary: "User input submitted",
        tone: "info",
        payload: {
          requestId: "req-user-input-2",
          answers: {
            sandbox_mode: "workspace-write",
          },
        },
      }),
      makeActivity({
        id: "user-input-open-2",
        createdAt: "2026-02-23T00:00:01.500Z",
        kind: "user-input.requested",
        summary: "User input requested",
        tone: "info",
        payload: {
          requestId: "req-user-input-2",
          questions: [
            {
              id: "approval",
              header: "Approval",
              question: "Continue?",
              options: [
                {
                  label: "yes",
                  description: "Continue execution",
                },
              ],
              multiSelect: false,
            },
          ],
        },
      }),
    ];

    expect(derivePendingUserInputs(activities)).toEqual([
      {
        requestId: "req-user-input-1",
        createdAt: "2026-02-23T00:00:01.000Z",
        questions: [
          {
            id: "sandbox_mode",
            header: "Sandbox",
            question: "Which mode should be used?",
            options: [
              {
                label: "workspace-write",
                description: "Allow workspace writes only",
              },
            ],
            multiSelect: true,
          },
        ],
      },
    ]);
  });

  it("clears stale pending user-input prompts when the provider reports an orphaned request", () => {
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "user-input-open-stale",
        createdAt: "2026-02-23T00:00:01.000Z",
        kind: "user-input.requested",
        summary: "User input requested",
        tone: "info",
        payload: {
          requestId: "req-user-input-stale-1",
          questions: [
            {
              id: "sandbox_mode",
              header: "Sandbox",
              question: "Which mode should be used?",
              options: [
                {
                  label: "workspace-write",
                  description: "Allow workspace writes only",
                },
              ],
              multiSelect: false,
            },
          ],
        },
      }),
      makeActivity({
        id: "user-input-failed-stale",
        createdAt: "2026-02-23T00:00:02.000Z",
        kind: "provider.user-input.respond.failed",
        summary: "Provider user input response failed",
        tone: "error",
        payload: {
          requestId: "req-user-input-stale-1",
          detail:
            "Provider adapter request failed (codex) for item/tool/requestUserInput: Unknown pending Codex user input request: req-user-input-stale-1",
        },
      }),
    ];

    expect(derivePendingUserInputs(activities)).toEqual([]);
  });
});

describe("deriveActivePlanState", () => {
  it("returns the latest plan update for the active turn", () => {
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "plan-old",
        createdAt: "2026-02-23T00:00:01.000Z",
        kind: "turn.plan.updated",
        summary: "Plan updated",
        tone: "info",
        turnId: "turn-1",
        payload: {
          explanation: "Initial plan",
          plan: [{ step: "Inspect code", status: "pending" }],
        },
      }),
      makeActivity({
        id: "plan-latest",
        createdAt: "2026-02-23T00:00:02.000Z",
        kind: "turn.plan.updated",
        summary: "Plan updated",
        tone: "info",
        turnId: "turn-1",
        payload: {
          explanation: "Refined plan",
          plan: [{ step: "Implement Codex user input", status: "inProgress" }],
        },
      }),
    ];

    expect(deriveActivePlanState(activities, TurnId.make("turn-1"))).toEqual({
      createdAt: "2026-02-23T00:00:02.000Z",
      turnId: "turn-1",
      explanation: "Refined plan",
      steps: [{ step: "Implement Codex user input", status: "inProgress" }],
    });
  });

  it("falls back to the most recent plan from a previous turn", () => {
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "plan-from-turn-1",
        createdAt: "2026-02-23T00:00:01.000Z",
        kind: "turn.plan.updated",
        summary: "Plan updated",
        tone: "info",
        turnId: "turn-1",
        payload: {
          plan: [{ step: "Write tests", status: "completed" }],
        },
      }),
    ];

    // Current turn is turn-2, which has no plan activity — should fall back to turn-1's plan
    const result = deriveActivePlanState(activities, TurnId.make("turn-2"));
    expect(result).toEqual({
      createdAt: "2026-02-23T00:00:01.000Z",
      turnId: "turn-1",
      steps: [{ step: "Write tests", status: "completed" }],
    });
  });
});

describe("findLatestProposedPlan", () => {
  it("prefers the latest proposed plan for the active turn", () => {
    expect(
      findLatestProposedPlan(
        [
          {
            id: "plan:thread-1:turn:turn-1",
            turnId: TurnId.make("turn-1"),
            planMarkdown: "# Older",
            implementedAt: null,
            implementationThreadId: null,
            createdAt: "2026-02-23T00:00:01.000Z",
            updatedAt: "2026-02-23T00:00:01.000Z",
          },
          {
            id: "plan:thread-1:turn:turn-1",
            turnId: TurnId.make("turn-1"),
            planMarkdown: "# Latest",
            implementedAt: null,
            implementationThreadId: null,
            createdAt: "2026-02-23T00:00:01.000Z",
            updatedAt: "2026-02-23T00:00:02.000Z",
          },
          {
            id: "plan:thread-1:turn:turn-2",
            turnId: TurnId.make("turn-2"),
            planMarkdown: "# Different turn",
            implementedAt: null,
            implementationThreadId: null,
            createdAt: "2026-02-23T00:00:03.000Z",
            updatedAt: "2026-02-23T00:00:03.000Z",
          },
        ],
        TurnId.make("turn-1"),
      ),
    ).toEqual({
      id: "plan:thread-1:turn:turn-1",
      turnId: "turn-1",
      planMarkdown: "# Latest",
      implementedAt: null,
      implementationThreadId: null,
      createdAt: "2026-02-23T00:00:01.000Z",
      updatedAt: "2026-02-23T00:00:02.000Z",
    });
  });

  it("falls back to the most recently updated proposed plan", () => {
    const latestPlan = findLatestProposedPlan(
      [
        {
          id: "plan:thread-1:turn:turn-1",
          turnId: TurnId.make("turn-1"),
          planMarkdown: "# First",
          implementedAt: null,
          implementationThreadId: null,
          createdAt: "2026-02-23T00:00:01.000Z",
          updatedAt: "2026-02-23T00:00:01.000Z",
        },
        {
          id: "plan:thread-1:turn:turn-2",
          turnId: TurnId.make("turn-2"),
          planMarkdown: "# Latest",
          implementedAt: null,
          implementationThreadId: null,
          createdAt: "2026-02-23T00:00:02.000Z",
          updatedAt: "2026-02-23T00:00:03.000Z",
        },
      ],
      null,
    );

    expect(latestPlan?.planMarkdown).toBe("# Latest");
  });
});

describe("hasActionableProposedPlan", () => {
  it("returns true for an unimplemented proposed plan", () => {
    expect(
      hasActionableProposedPlan({
        id: "plan-1",
        turnId: TurnId.make("turn-1"),
        planMarkdown: "# Plan",
        implementedAt: null,
        implementationThreadId: null,
        createdAt: "2026-02-23T00:00:00.000Z",
        updatedAt: "2026-02-23T00:00:01.000Z",
      }),
    ).toBe(true);
  });

  it("returns false for a proposed plan already implemented elsewhere", () => {
    expect(
      hasActionableProposedPlan({
        id: "plan-1",
        turnId: TurnId.make("turn-1"),
        planMarkdown: "# Plan",
        implementedAt: "2026-02-23T00:00:02.000Z",
        implementationThreadId: ThreadId.make("thread-implement"),
        createdAt: "2026-02-23T00:00:00.000Z",
        updatedAt: "2026-02-23T00:00:02.000Z",
      }),
    ).toBe(false);
  });
});

describe("findSidebarProposedPlan", () => {
  it("prefers the running turn source proposed plan when available on the same thread", () => {
    expect(
      findSidebarProposedPlan({
        threads: [
          {
            id: ThreadId.make("thread-1"),
            proposedPlans: [
              {
                id: "plan-1",
                turnId: TurnId.make("turn-plan"),
                planMarkdown: "# Source plan",
                implementedAt: "2026-02-23T00:00:03.000Z",
                implementationThreadId: ThreadId.make("thread-2"),
                createdAt: "2026-02-23T00:00:01.000Z",
                updatedAt: "2026-02-23T00:00:02.000Z",
              },
            ],
          },
          {
            id: ThreadId.make("thread-2"),
            proposedPlans: [
              {
                id: "plan-2",
                turnId: TurnId.make("turn-other"),
                planMarkdown: "# Latest elsewhere",
                implementedAt: null,
                implementationThreadId: null,
                createdAt: "2026-02-23T00:00:04.000Z",
                updatedAt: "2026-02-23T00:00:05.000Z",
              },
            ],
          },
        ],
        latestTurn: {
          turnId: TurnId.make("turn-implementation"),
          sourceProposedPlan: {
            threadId: ThreadId.make("thread-1"),
            planId: "plan-1",
          },
        },
        latestTurnSettled: false,
        threadId: ThreadId.make("thread-1"),
      }),
    ).toEqual({
      id: "plan-1",
      turnId: "turn-plan",
      planMarkdown: "# Source plan",
      implementedAt: "2026-02-23T00:00:03.000Z",
      implementationThreadId: "thread-2",
      createdAt: "2026-02-23T00:00:01.000Z",
      updatedAt: "2026-02-23T00:00:02.000Z",
    });
  });

  it("falls back to the latest proposed plan once the turn is settled", () => {
    expect(
      findSidebarProposedPlan({
        threads: [
          {
            id: ThreadId.make("thread-1"),
            proposedPlans: [
              {
                id: "plan-1",
                turnId: TurnId.make("turn-plan"),
                planMarkdown: "# Older",
                implementedAt: null,
                implementationThreadId: null,
                createdAt: "2026-02-23T00:00:01.000Z",
                updatedAt: "2026-02-23T00:00:02.000Z",
              },
              {
                id: "plan-2",
                turnId: TurnId.make("turn-latest"),
                planMarkdown: "# Latest",
                implementedAt: null,
                implementationThreadId: null,
                createdAt: "2026-02-23T00:00:03.000Z",
                updatedAt: "2026-02-23T00:00:04.000Z",
              },
            ],
          },
        ],
        latestTurn: {
          turnId: TurnId.make("turn-implementation"),
          sourceProposedPlan: {
            threadId: ThreadId.make("thread-1"),
            planId: "plan-1",
          },
        },
        latestTurnSettled: true,
        threadId: ThreadId.make("thread-1"),
      })?.planMarkdown,
    ).toBe("# Latest");
  });
});

describe("workEntryIndicatesToolFailure", () => {
  const base = {
    id: "w1",
    createdAt: "2026-01-01T00:00:00.000Z",
    label: "Read",
  };

  it("is true for error tone", () => {
    expect(
      workEntryIndicatesToolFailure({
        ...base,
        tone: "error",
        detail: "nothing special",
      }),
    ).toBe(true);
  });

  it("is true when lifecycle says failed even if detail is empty", () => {
    expect(
      workEntryIndicatesToolFailure({
        ...base,
        tone: "tool",
        toolLifecycleStatus: "failed",
      }),
    ).toBe(true);
  });

  it("detects file-not-found style tool output with completed lifecycle", () => {
    expect(
      workEntryIndicatesToolFailure({
        ...base,
        tone: "tool",
        toolLifecycleStatus: "completed",
        detail: "File not found: C:\\foo\\nonexistent.ts",
      }),
    ).toBe(true);
  });

  it("detects glob no files and PowerShell command errors", () => {
    expect(
      workEntryIndicatesToolFailure({
        ...base,
        label: "Glob",
        tone: "tool",
        detail: "No files found",
      }),
    ).toBe(true);
    expect(
      workEntryIndicatesToolFailure({
        ...base,
        label: "Bash",
        tone: "tool",
        detail:
          "The term 'this_is_not_a_command' is not recognized as the name of a cmdlet, function, script file, or operable program.",
      }),
    ).toBe(true);
  });

  it("is false for successful completed tools", () => {
    expect(
      workEntryIndicatesToolFailure({
        ...base,
        tone: "tool",
        toolLifecycleStatus: "completed",
        detail: "Found 3 matching files",
      }),
    ).toBe(false);
  });

  it("treats successful tool rows as success candidates", () => {
    expect(
      workEntryIndicatesToolSuccess({
        ...base,
        tone: "tool",
        toolLifecycleStatus: "completed",
        detail: "ok",
      }),
    ).toBe(true);
    expect(
      workEntryIndicatesToolSuccess({
        ...base,
        tone: "tool",
        toolLifecycleStatus: "inProgress",
        detail: "…",
      }),
    ).toBe(false);
    expect(workEntryIndicatesToolSuccess({ ...base, tone: "thinking", detail: "…" })).toBe(false);
    expect(
      workEntryIndicatesToolNeutralStatus({
        ...base,
        tone: "tool",
        toolLifecycleStatus: "inProgress",
        detail: "…",
      }),
    ).toBe(true);
    expect(
      workEntryIndicatesToolNeutralStatus({
        ...base,
        tone: "tool",
        toolLifecycleStatus: "completed",
        detail: "ok",
      }),
    ).toBe(false);
  });

  it("does not run heuristics on non-tool info rows", () => {
    expect(
      workEntryIndicatesToolFailure({
        ...base,
        label: "Context compacted",
        tone: "info",
        detail: "File not found in conversation",
      }),
    ).toBe(false);
  });
});

describe("deriveWorkLogEntries", () => {
  it("omits tool started entries and keeps completed entries", () => {
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "tool-complete",
        createdAt: "2026-02-23T00:00:03.000Z",
        summary: "Tool call complete",
        kind: "tool.completed",
      }),
      makeActivity({
        id: "tool-start",
        createdAt: "2026-02-23T00:00:02.000Z",
        summary: "Tool call",
        kind: "tool.started",
      }),
    ];

    const entries = deriveWorkLogEntries(activities);
    expect(entries.map((entry) => entry.id)).toEqual(["tool-complete"]);
  });

  it("omits task.started but shows task.progress and task.completed", () => {
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "task-start",
        createdAt: "2026-02-23T00:00:01.000Z",
        kind: "task.started",
        summary: "default task started",
        tone: "info",
      }),
      makeActivity({
        id: "task-progress",
        createdAt: "2026-02-23T00:00:02.000Z",
        kind: "task.progress",
        summary: "Updating files",
        tone: "info",
      }),
      makeActivity({
        id: "task-complete",
        createdAt: "2026-02-23T00:00:03.000Z",
        kind: "task.completed",
        summary: "Task completed",
        tone: "info",
      }),
    ];

    const entries = deriveWorkLogEntries(activities);
    expect(entries.map((entry) => entry.id)).toEqual(["task-progress", "task-complete"]);
  });

  it("uses payload summary as label for task entries when available", () => {
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "task-progress-with-summary",
        createdAt: "2026-02-23T00:00:02.000Z",
        kind: "task.progress",
        summary: "Reasoning update",
        tone: "info",
        payload: { summary: "Searching for API endpoints" },
      }),
    ];

    const entries = deriveWorkLogEntries(activities);
    expect(entries[0]?.label).toBe("Searching for API endpoints");
  });

  it("uses payload detail as label for task.completed and preserves error tone", () => {
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "task-completed-failed",
        createdAt: "2026-02-23T00:00:03.000Z",
        kind: "task.completed",
        summary: "Task failed",
        tone: "error",
        payload: { detail: "Failed to deploy changes" },
      }),
    ];

    const entries = deriveWorkLogEntries(activities);
    expect(entries[0]?.label).toBe("Failed to deploy changes");
    expect(entries[0]?.tone).toBe("error");
  });

  it("keeps tool entries from every turn and tags each with its turn id", () => {
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "turn-1-tool",
        turnId: "turn-1",
        summary: "Tool call complete",
        kind: "tool.completed",
      }),
      makeActivity({
        id: "turn-2-tool",
        turnId: "turn-2",
        summary: "Tool call complete",
        kind: "tool.completed",
      }),
    ];

    const entries = deriveWorkLogEntries(activities);
    expect(entries.map((entry) => entry.id)).toEqual(["turn-1-tool", "turn-2-tool"]);
    expect(entries.map((entry) => entry.turnId)).toEqual([
      TurnId.make("turn-1"),
      TurnId.make("turn-2"),
    ]);
  });

  it("omits checkpoint captured info entries", () => {
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "checkpoint",
        createdAt: "2026-02-23T00:00:01.000Z",
        summary: "Checkpoint captured",
        tone: "info",
      }),
      makeActivity({
        id: "tool-complete",
        createdAt: "2026-02-23T00:00:02.000Z",
        summary: "Ran command",
        tone: "tool",
        kind: "tool.completed",
      }),
    ];

    const entries = deriveWorkLogEntries(activities);
    expect(entries.map((entry) => entry.id)).toEqual(["tool-complete"]);
  });

  it("omits ExitPlanMode lifecycle entries once the plan card is shown", () => {
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "exit-plan-updated",
        createdAt: "2026-02-23T00:00:01.000Z",
        kind: "tool.updated",
        summary: "Tool call",
        payload: {
          detail: 'ExitPlanMode: {"allowedPrompts":[{"tool":"Bash","prompt":"run tests"}]}',
        },
      }),
      makeActivity({
        id: "exit-plan-completed",
        createdAt: "2026-02-23T00:00:02.000Z",
        kind: "tool.completed",
        summary: "Tool call",
        payload: {
          detail: "ExitPlanMode: {}",
        },
      }),
      makeActivity({
        id: "real-work-log",
        createdAt: "2026-02-23T00:00:03.000Z",
        kind: "tool.completed",
        summary: "Ran command",
        payload: {
          itemType: "command_execution",
          detail: "Bash: bun test",
        },
      }),
    ];

    const entries = deriveWorkLogEntries(activities);
    expect(entries.map((entry) => entry.id)).toEqual(["real-work-log"]);
  });

  it("orders work log by activity sequence when present", () => {
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "second",
        createdAt: "2026-02-23T00:00:03.000Z",
        sequence: 2,
        summary: "Tool call complete",
        kind: "tool.completed",
      }),
      makeActivity({
        id: "first",
        createdAt: "2026-02-23T00:00:04.000Z",
        sequence: 1,
        summary: "Tool call complete",
        kind: "tool.completed",
      }),
    ];

    const entries = deriveWorkLogEntries(activities);
    expect(entries.map((entry) => entry.id)).toEqual(["first", "second"]);
  });

  it("extracts command text for command tool activities", () => {
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "command-tool",
        kind: "tool.completed",
        summary: "Ran command",
        payload: {
          itemType: "command_execution",
          data: {
            item: {
              command: ["bun", "run", "lint"],
            },
          },
        },
      }),
    ];

    const [entry] = deriveWorkLogEntries(activities);
    expect(entry?.command).toBe("bun run lint");
  });

  it("extracts failed tool lifecycle status from item payloads", () => {
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "tool-failed",
        kind: "tool.updated",
        summary: "Glob",
        tone: "tool",
        payload: {
          itemType: "mcp_tool_call",
          status: "failed",
          detail: "No files found",
        },
      }),
    ];

    const [entry] = deriveWorkLogEntries(activities);
    expect(entry?.toolLifecycleStatus).toBe("failed");
  });

  it("defaults tool.completed entries to completed lifecycle status", () => {
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "tool-done",
        kind: "tool.completed",
        summary: "Glob",
        tone: "tool",
        payload: {
          itemType: "mcp_tool_call",
          detail: "Found 3 files",
        },
      }),
    ];

    const [entry] = deriveWorkLogEntries(activities);
    expect(entry?.toolLifecycleStatus).toBe("completed");
  });

  it("preserves MCP server, tool, arguments, and results for expanded display", () => {
    const item = {
      type: "mcpToolCall",
      server: "coddy-studio",
      tool: "preview_status",
      arguments: {},
      status: "completed",
      result: { content: [{ type: "text", text: "attached" }] },
    };
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "mcp-tool-done",
        kind: "tool.completed",
        summary: "coddy-studio · preview_status",
        payload: {
          itemType: "mcp_tool_call",
          title: "coddy-studio · preview_status",
          data: { item },
        },
      }),
    ];

    const [entry] = deriveWorkLogEntries(activities);
    expect(entry?.toolTitle).toBe("coddy-studio · preview_status");
    expect(entry?.toolData).toEqual(item);
  });

  it("keeps MCP payloads while collapsing lifecycle updates", () => {
    const item = {
      type: "mcpToolCall",
      server: "coddy-studio",
      tool: "preview_snapshot",
      arguments: { interactiveOnly: true },
      status: "completed",
    };
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "mcp-tool-progress",
        kind: "tool.updated",
        summary: "coddy-studio · preview_snapshot",
        payload: {
          itemType: "mcp_tool_call",
          toolCallId: "call-1",
          data: { item },
        },
      }),
      makeActivity({
        id: "mcp-tool-complete",
        kind: "tool.completed",
        summary: "coddy-studio · preview_snapshot",
        payload: {
          itemType: "mcp_tool_call",
          toolCallId: "call-1",
        },
      }),
    ];

    const [entry] = deriveWorkLogEntries(activities);
    expect(entry?.toolData).toEqual(item);
  });

  it("unwraps PowerShell command wrappers for displayed command text", () => {
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "command-tool-windows-wrapper",
        kind: "tool.completed",
        summary: "Ran command",
        payload: {
          itemType: "command_execution",
          data: {
            item: {
              command: "\"C:\\Program Files\\PowerShell\\7\\pwsh.exe\" -Command 'bun run lint'",
            },
          },
        },
      }),
    ];

    const [entry] = deriveWorkLogEntries(activities);
    expect(entry?.command).toBe("bun run lint");
    expect(entry?.rawCommand).toBe(
      "\"C:\\Program Files\\PowerShell\\7\\pwsh.exe\" -Command 'bun run lint'",
    );
  });

  it("unwraps PowerShell command wrappers from argv-style command payloads", () => {
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "command-tool-windows-wrapper-argv",
        kind: "tool.completed",
        summary: "Ran command",
        payload: {
          itemType: "command_execution",
          data: {
            item: {
              command: ["C:\\Program Files\\PowerShell\\7\\pwsh.exe", "-Command", "rg -n foo ."],
            },
          },
        },
      }),
    ];

    const [entry] = deriveWorkLogEntries(activities);
    expect(entry?.command).toBe("rg -n foo .");
    expect(entry?.rawCommand).toBe(
      '"C:\\Program Files\\PowerShell\\7\\pwsh.exe" -Command "rg -n foo ."',
    );
  });

  it("extracts command text from command detail when structured command metadata is missing", () => {
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "command-tool-windows-detail-fallback",
        kind: "tool.completed",
        summary: "Ran command",
        payload: {
          itemType: "command_execution",
          detail:
            '"C:\\Program Files\\PowerShell\\7\\pwsh.exe" -NoLogo -NoProfile -Command \'rg -n -F "new Date()" .\' <exited with exit code 0>',
        },
      }),
    ];

    const [entry] = deriveWorkLogEntries(activities);
    expect(entry?.command).toBe('rg -n -F "new Date()" .');
    expect(entry?.rawCommand).toBe(
      `"C:\\Program Files\\PowerShell\\7\\pwsh.exe" -NoLogo -NoProfile -Command 'rg -n -F "new Date()" .'`,
    );
  });

  it("does not unwrap shell commands when no wrapper flag is present", () => {
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "command-tool-shell-script",
        kind: "tool.completed",
        summary: "Ran command",
        payload: {
          itemType: "command_execution",
          data: {
            item: {
              command: "bash script.sh",
            },
          },
        },
      }),
    ];

    const [entry] = deriveWorkLogEntries(activities);
    expect(entry?.command).toBe("bash script.sh");
    expect(entry?.rawCommand).toBeUndefined();
  });

  it("keeps compact Codex tool metadata used for icons and labels", () => {
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "tool-with-metadata",
        kind: "tool.completed",
        summary: "bash",
        payload: {
          itemType: "command_execution",
          title: "bash",
          status: "completed",
          detail: '{ "dev": "vite dev --port 3000" } <exited with exit code 0>',
          data: {
            item: {
              command: ["bun", "run", "dev"],
              result: {
                content: '{ "dev": "vite dev --port 3000" } <exited with exit code 0>',
                exitCode: 0,
              },
            },
          },
        },
      }),
    ];

    const [entry] = deriveWorkLogEntries(activities);
    expect(entry).toMatchObject({
      command: "bun run dev",
      detail: '{ "dev": "vite dev --port 3000" }',
      itemType: "command_execution",
      toolTitle: "bash",
    });
  });

  it("extracts changed file paths for file-change tool activities", () => {
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "file-tool",
        kind: "tool.completed",
        summary: "File change",
        payload: {
          itemType: "file_change",
          data: {
            item: {
              changes: [
                { path: "apps/web/src/components/ChatView.tsx" },
                { filename: "apps/web/src/session-logic.ts" },
              ],
            },
          },
        },
      }),
    ];

    const [entry] = deriveWorkLogEntries(activities);
    expect(entry?.changedFiles).toEqual([
      "apps/web/src/components/ChatView.tsx",
      "apps/web/src/session-logic.ts",
    ]);
  });

  it("drops duplicated tool detail when it only repeats the title", () => {
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "read-file-generic",
        kind: "tool.completed",
        summary: "Read File",
        payload: {
          itemType: "dynamic_tool_call",
          title: "Read File",
          detail: "Read File",
        },
      }),
    ];

    const [entry] = deriveWorkLogEntries(activities);
    expect(entry?.toolTitle).toBe("Read File");
    expect(entry?.detail).toBeUndefined();
  });

  it("uses grep raw output summaries instead of repeating the generic tool label", () => {
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "grep-update",
        createdAt: "2026-02-23T00:00:01.000Z",
        kind: "tool.updated",
        summary: "grep",
        payload: {
          itemType: "web_search",
          title: "grep",
          detail: "grep",
          data: {
            toolCallId: "tool-grep-1",
            kind: "search",
            rawInput: {},
          },
        },
      }),
      makeActivity({
        id: "grep-complete",
        createdAt: "2026-02-23T00:00:02.000Z",
        kind: "tool.completed",
        summary: "grep",
        payload: {
          itemType: "web_search",
          title: "grep",
          detail: "grep",
          data: {
            toolCallId: "tool-grep-1",
            kind: "search",
            rawOutput: {
              totalFiles: 19,
              truncated: false,
            },
          },
        },
      }),
    ];

    const entries = deriveWorkLogEntries(activities);
    expect(entries).toHaveLength(1);
    expect(entries[0]).toMatchObject({
      id: "grep-complete",
      toolTitle: "grep",
      detail: "19 files",
      itemType: "web_search",
    });
  });

  it("uses completed read-file output previews and still collapses the same tool call", () => {
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "read-update",
        createdAt: "2026-02-23T00:00:01.000Z",
        kind: "tool.updated",
        summary: "Read File",
        payload: {
          itemType: "dynamic_tool_call",
          title: "Read File",
          detail: "Read File",
          data: {
            toolCallId: "tool-read-1",
            kind: "read",
            rawInput: {},
          },
        },
      }),
      makeActivity({
        id: "read-complete",
        createdAt: "2026-02-23T00:00:02.000Z",
        kind: "tool.completed",
        summary: "Read File",
        payload: {
          itemType: "dynamic_tool_call",
          title: "Read File",
          detail: "Read File",
          data: {
            toolCallId: "tool-read-1",
            kind: "read",
            rawOutput: {
              content:
                'import * as Effect from "effect/Effect"\nimport * as Layer from "effect/Layer"\n',
            },
          },
        },
      }),
    ];

    const entries = deriveWorkLogEntries(activities);
    expect(entries).toHaveLength(1);
    expect(entries[0]).toMatchObject({
      id: "read-complete",
      toolTitle: "Read File",
      detail: 'import * as Effect from "effect/Effect"',
      itemType: "dynamic_tool_call",
    });
  });

  it("does not use command stdout as the detail when Cursor omits the command input", () => {
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "cursor-command-complete",
        createdAt: "2026-04-16T22:40:42.221Z",
        kind: "tool.completed",
        summary: "Ran command",
        payload: {
          itemType: "command_execution",
          title: "Ran command",
          data: {
            toolCallId: "toolu_vrtx_01WypXgRM8PPygBtrVAZwzy5",
            kind: "execute",
            rawInput: {},
            rawOutput: {
              exitCode: 0,
              stdout: "total 960\napps\npackages\n",
              stderr: "",
            },
          },
        },
      }),
    ];

    const [entry] = deriveWorkLogEntries(activities);
    expect(entry).toMatchObject({
      id: "cursor-command-complete",
      label: "Ran command",
      itemType: "command_execution",
      toolTitle: "Ran command",
    });
    expect(entry?.detail).toBeUndefined();
    expect(entry?.command).toBeUndefined();
  });

  it("collapses legacy completed tool rows that are missing tool metadata", () => {
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "legacy-read-update",
        createdAt: "2026-02-23T00:00:01.000Z",
        kind: "tool.updated",
        summary: "Read File",
        payload: {
          itemType: "dynamic_tool_call",
          title: "Read File",
          detail: "Read File",
          data: {
            toolCallId: "tool-read-legacy",
            kind: "read",
            rawInput: {},
          },
        },
      }),
      makeActivity({
        id: "legacy-read-complete",
        createdAt: "2026-02-23T00:00:02.000Z",
        kind: "tool.completed",
        summary: "Read File",
        payload: {
          itemType: "dynamic_tool_call",
          title: "Read File",
          detail: "Read File",
        },
      }),
    ];

    const entries = deriveWorkLogEntries(activities);
    expect(entries).toHaveLength(1);
    expect(entries[0]).toMatchObject({
      id: "legacy-read-complete",
      toolTitle: "Read File",
      itemType: "dynamic_tool_call",
    });
    expect(entries[0]?.detail).toBeUndefined();
  });

  it("collapses repeated lifecycle updates for the same tool call into one entry", () => {
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "tool-update-1",
        createdAt: "2026-02-23T00:00:01.000Z",
        kind: "tool.updated",
        summary: "Tool call",
        payload: {
          itemType: "dynamic_tool_call",
          title: "Tool call",
          detail: 'Read: {"file_path":"/tmp/app.ts"}',
        },
      }),
      makeActivity({
        id: "tool-update-2",
        createdAt: "2026-02-23T00:00:02.000Z",
        kind: "tool.updated",
        summary: "Tool call",
        payload: {
          itemType: "dynamic_tool_call",
          title: "Tool call",
          detail: 'Read: {"file_path":"/tmp/app.ts"}',
          data: {
            item: {
              command: ["sed", "-n", "1,40p", "/tmp/app.ts"],
            },
          },
        },
      }),
      makeActivity({
        id: "tool-complete",
        createdAt: "2026-02-23T00:00:03.000Z",
        kind: "tool.completed",
        summary: "Tool call completed",
        payload: {
          itemType: "dynamic_tool_call",
          title: "Tool call",
          detail: 'Read: {"file_path":"/tmp/app.ts"}',
        },
      }),
    ];

    const entries = deriveWorkLogEntries(activities);

    expect(entries).toHaveLength(1);
    expect(entries[0]).toMatchObject({
      id: "tool-complete",
      createdAt: "2026-02-23T00:00:03.000Z",
      label: "Tool call completed",
      detail: 'Read: {"file_path":"/tmp/app.ts"}',
      command: "sed -n 1,40p /tmp/app.ts",
      itemType: "dynamic_tool_call",
      toolTitle: "Tool call",
    });
  });

  it("keeps separate tool entries when an identical call starts after the prior one completed", () => {
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "tool-1-update",
        createdAt: "2026-02-23T00:00:01.000Z",
        kind: "tool.updated",
        summary: "Tool call",
        payload: {
          itemType: "dynamic_tool_call",
          title: "Tool call",
          detail: 'Read: {"file_path":"/tmp/app.ts"}',
        },
      }),
      makeActivity({
        id: "tool-1-complete",
        createdAt: "2026-02-23T00:00:02.000Z",
        kind: "tool.completed",
        summary: "Tool call completed",
        payload: {
          itemType: "dynamic_tool_call",
          title: "Tool call",
          detail: 'Read: {"file_path":"/tmp/app.ts"}',
        },
      }),
      makeActivity({
        id: "tool-2-update",
        createdAt: "2026-02-23T00:00:03.000Z",
        kind: "tool.updated",
        summary: "Tool call",
        payload: {
          itemType: "dynamic_tool_call",
          title: "Tool call",
          detail: 'Read: {"file_path":"/tmp/app.ts"}',
        },
      }),
      makeActivity({
        id: "tool-2-complete",
        createdAt: "2026-02-23T00:00:04.000Z",
        kind: "tool.completed",
        summary: "Tool call completed",
        payload: {
          itemType: "dynamic_tool_call",
          title: "Tool call",
          detail: 'Read: {"file_path":"/tmp/app.ts"}',
        },
      }),
    ];

    const entries = deriveWorkLogEntries(activities);

    expect(entries.map((entry) => entry.id)).toEqual(["tool-1-complete", "tool-2-complete"]);
  });

  it("collapses same-timestamp lifecycle rows even when completed sorts before updated by id", () => {
    const activities: OrchestrationThreadActivity[] = [
      makeActivity({
        id: "z-update-earlier",
        createdAt: "2026-02-23T00:00:01.000Z",
        kind: "tool.updated",
        summary: "Tool call",
        payload: {
          itemType: "dynamic_tool_call",
          title: "Tool call",
          detail: 'Read: {"file_path":"/tmp/app.ts"}',
        },
      }),
      makeActivity({
        id: "a-complete-same-timestamp",
        createdAt: "2026-02-23T00:00:02.000Z",
        kind: "tool.completed",
        summary: "Tool call",
        payload: {
          itemType: "dynamic_tool_call",
          title: "Tool call",
          detail: 'Read: {"file_path":"/tmp/app.ts"}',
        },
      }),
      makeActivity({
        id: "z-update-same-timestamp",
        createdAt: "2026-02-23T00:00:02.000Z",
        kind: "tool.updated",
        summary: "Tool call",
        payload: {
          itemType: "dynamic_tool_call",
          title: "Tool call",
          detail: 'Read: {"file_path":"/tmp/app.ts"}',
        },
      }),
    ];

    const entries = deriveWorkLogEntries(activities);

    expect(entries).toHaveLength(1);
    expect(entries[0]?.id).toBe("a-complete-same-timestamp");
  });
});

describe("deriveTimelineEntries", () => {
  it("includes proposed plans alongside messages and work entries in chronological order", () => {
    const entries = deriveTimelineEntries(
      [
        {
          id: MessageId.make("message-1"),
          role: "assistant",
          text: "hello",
          createdAt: "2026-02-23T00:00:01.000Z",
          turnId: null,
          updatedAt: "2026-02-23T00:00:01.000Z",
          streaming: false,
        },
      ],
      [
        {
          id: "plan:thread-1:turn:turn-1",
          turnId: TurnId.make("turn-1"),
          planMarkdown: "# Ship it",
          implementedAt: null,
          implementationThreadId: null,
          createdAt: "2026-02-23T00:00:02.000Z",
          updatedAt: "2026-02-23T00:00:02.000Z",
        },
      ],
      [
        {
          id: "work-1",
          createdAt: "2026-02-23T00:00:03.000Z",
          label: "Ran tests",
          tone: "tool",
        },
      ],
    );

    expect(entries.map((entry) => entry.kind)).toEqual(["message", "proposed-plan", "work"]);
    expect(entries[1]).toMatchObject({
      kind: "proposed-plan",
      proposedPlan: {
        planMarkdown: "# Ship it",
        implementedAt: null,
        implementationThreadId: null,
      },
    });
  });
});

describe("deriveWorkLogEntries context window handling", () => {
  it("excludes context window updates from the work log", () => {
    const entries = deriveWorkLogEntries([
      makeActivity({
        id: "context-1",
        turnId: "turn-1",
        kind: "context-window.updated",
        summary: "Context window updated",
        tone: "info",
      }),
      makeActivity({
        id: "tool-1",
        turnId: "turn-1",
        kind: "tool.completed",
        summary: "Ran command",
        tone: "tool",
      }),
    ]);

    expect(entries).toHaveLength(1);
    expect(entries[0]?.label).toBe("Ran command");
  });

  it("keeps context compaction activities as normal work log entries", () => {
    const entries = deriveWorkLogEntries([
      makeActivity({
        id: "compaction-1",
        turnId: "turn-1",
        kind: "context-compaction",
        summary: "Context compacted",
        tone: "info",
      }),
    ]);

    expect(entries).toHaveLength(1);
    expect(entries[0]?.label).toBe("Context compacted");
  });
});

describe("isLatestTurnSettled", () => {
  const latestTurn = {
    turnId: TurnId.make("turn-1"),
    startedAt: "2026-02-27T21:10:00.000Z",
    completedAt: "2026-02-27T21:10:06.000Z",
  } as const;

  it("returns false while the same turn is still active in a running session", () => {
    expect(
      isLatestTurnSettled(latestTurn, {
        status: "running",
        activeTurnId: TurnId.make("turn-1"),
      }),
    ).toBe(false);
  });

  it("returns false while any turn is running to avoid stale latest-turn banners", () => {
    expect(
      isLatestTurnSettled(latestTurn, {
        status: "running",
        activeTurnId: TurnId.make("turn-2"),
      }),
    ).toBe(false);
  });

  it("returns true once the session is no longer running that turn", () => {
    expect(
      isLatestTurnSettled(latestTurn, {
        status: "ready",
        activeTurnId: null,
      }),
    ).toBe(true);
  });

  it("returns false when turn timestamps are incomplete", () => {
    expect(
      isLatestTurnSettled(
        {
          turnId: TurnId.make("turn-1"),
          startedAt: null,
          completedAt: "2026-02-27T21:10:06.000Z",
        },
        null,
      ),
    ).toBe(false);
  });
});

describe("deriveActiveWorkStartedAt", () => {
  const latestTurn = {
    turnId: TurnId.make("turn-1"),
    startedAt: "2026-02-27T21:10:00.000Z",
    completedAt: "2026-02-27T21:10:06.000Z",
  } as const;

  it("prefers the in-flight turn start when the latest turn is not settled", () => {
    expect(
      deriveActiveWorkStartedAt(
        latestTurn,
        {
          status: "running",
          activeTurnId: TurnId.make("turn-1"),
        },
        "2026-02-27T21:11:00.000Z",
      ),
    ).toBe("2026-02-27T21:10:00.000Z");
  });

  it("uses the new send start while the session is running a different turn", () => {
    expect(
      deriveActiveWorkStartedAt(
        latestTurn,
        {
          status: "running",
          activeTurnId: TurnId.make("turn-2"),
        },
        "2026-02-27T21:11:00.000Z",
      ),
    ).toBe("2026-02-27T21:11:00.000Z");
  });

  it("falls back to sendStartedAt once the latest turn is settled", () => {
    expect(
      deriveActiveWorkStartedAt(
        latestTurn,
        {
          status: "ready",
          activeTurnId: null,
        },
        "2026-02-27T21:11:00.000Z",
      ),
    ).toBe("2026-02-27T21:11:00.000Z");
  });

  it("uses sendStartedAt for a fresh send after the prior turn completed", () => {
    expect(
      deriveActiveWorkStartedAt(
        {
          turnId: TurnId.make("turn-1"),
          startedAt: "2026-02-27T21:10:00.000Z",
          completedAt: "2026-02-27T21:10:06.000Z",
        },
        null,
        "2026-02-27T21:11:00.000Z",
      ),
    ).toBe("2026-02-27T21:11:00.000Z");
  });
});

describe("session display helpers", () => {
  const baseEntry = {
    id: "entry-1",
    createdAt: "2026-02-27T21:10:00.000Z",
    label: "Status",
    tone: "info" as const,
  };

  it("classifies every supported tool-like signal", () => {
    expect(workLogEntryIsToolLike(baseEntry)).toBe(false);
    expect(workLogEntryIsToolLike({ ...baseEntry, tone: "tool" })).toBe(true);
    expect(workLogEntryIsToolLike({ ...baseEntry, tone: "thinking" })).toBe(true);
    expect(workLogEntryIsToolLike({ ...baseEntry, tone: "error" })).toBe(true);
    expect(workLogEntryIsToolLike({ ...baseEntry, command: "   " })).toBe(false);
    expect(workLogEntryIsToolLike({ ...baseEntry, command: "git status" })).toBe(true);
    expect(workLogEntryIsToolLike({ ...baseEntry, requestKind: "file-read" })).toBe(true);
    expect(workLogEntryIsToolLike({ ...baseEntry, itemType: "command_execution" })).toBe(true);
  });

  it.each([
    "File not found: README.md",
    "No files found",
    "spawn ENOENT",
    "No such file or directory",
    "No such file",
    "Cannot find path C:\\missing because it does not exist",
    "CommandNotFoundException",
    "foo is not recognized as the name of a cmdlet",
    "The term 'foo' is not recognized",
    "A parameter cannot be found that matches parameter name Bad",
    "command not found",
    "output <exited with exit code 1>",
    "process exited with exit code 2",
    "exit code: 3",
  ])("recognizes actionable failure text: %s", (detail) => {
    expect(workEntryIndicatesToolFailure({ ...baseEntry, tone: "tool", detail })).toBe(true);
  });

  it("distinguishes success, neutral, and non-tool rows", () => {
    const success = {
      ...baseEntry,
      tone: "tool" as const,
      toolLifecycleStatus: "completed" as const,
    };
    expect(workEntryIndicatesToolSuccess(success)).toBe(true);
    expect(workEntryIndicatesToolNeutralStatus(success)).toBe(false);

    for (const entry of [
      { ...baseEntry, tone: "thinking" as const },
      { ...baseEntry, tone: "tool" as const, toolLifecycleStatus: "inProgress" as const },
      { ...baseEntry, tone: "tool" as const, toolLifecycleStatus: "stopped" as const },
    ]) {
      expect(workEntryIndicatesToolSuccess(entry)).toBe(false);
      expect(workEntryIndicatesToolNeutralStatus(entry)).toBe(true);
    }

    expect(workEntryIndicatesToolFailure({ ...baseEntry, tone: "tool", detail: "" })).toBe(false);
    expect(workEntryIndicatesToolFailure({ ...baseEntry, tone: "tool", command: "ok" })).toBe(
      false,
    );
    expect(workEntryIndicatesToolFailure(baseEntry)).toBe(false);
    expect(workEntryIndicatesToolSuccess(baseEntry)).toBe(false);
    expect(workEntryIndicatesToolNeutralStatus(baseEntry)).toBe(false);

    const failed = {
      ...baseEntry,
      tone: "tool" as const,
      toolLifecycleStatus: "failed" as const,
    };
    expect(workEntryIndicatesToolSuccess(failed)).toBe(false);
    expect(workEntryIndicatesToolNeutralStatus(failed)).toBe(false);
  });

  it.each([
    [Number.NaN, "0ms"],
    [Number.POSITIVE_INFINITY, "0ms"],
    [-1, "0ms"],
    [0, "1ms"],
    [499.4, "499ms"],
    [999, "999ms"],
    [1_000, "1.0s"],
    [9_949, "9.9s"],
    [9_950, "10s"],
    [10_000, "10s"],
    [59_999, "60s"],
    [60_000, "1m"],
    [61_000, "1m 1s"],
    [119_999, "2m"],
  ])("formats %s milliseconds as %s", (duration, expected) => {
    expect(formatDuration(duration)).toBe(expected);
  });

  it("formats elapsed timestamps and rejects incomplete or invalid ranges", () => {
    expect(formatElapsed("2026-01-01T00:00:00.000Z", undefined)).toBeNull();
    expect(formatElapsed("invalid", "2026-01-01T00:00:01.000Z")).toBeNull();
    expect(formatElapsed("2026-01-01T00:00:00.000Z", "invalid")).toBeNull();
    expect(formatElapsed("2026-01-01T00:00:02.000Z", "2026-01-01T00:00:01.000Z")).toBeNull();
    expect(formatElapsed("2026-01-01T00:00:00.000Z", "2026-01-01T00:00:01.250Z")).toBe("1.3s");
  });
});

describe("malformed orchestration payload handling", () => {
  it("ignores approvals without complete request identity and kind", () => {
    const activities = [
      makeActivity({ kind: "approval.requested", payload: {} }),
      makeActivity({
        kind: "approval.requested",
        payload: { requestId: 4, requestKind: "command" },
      }),
      makeActivity({ kind: "approval.requested", payload: { requestId: "missing-kind" } }),
      makeActivity({
        kind: "approval.requested",
        payload: { requestId: "dynamic", requestType: "dynamic_tool_call" },
      }),
      makeActivity({
        kind: "approval.requested",
        payload: { requestId: "read", requestType: "file_read_approval" },
      }),
      makeActivity({
        kind: "approval.requested",
        payload: { requestId: "change", requestType: "apply_patch_approval" },
      }),
      makeActivity({
        kind: "approval.requested",
        payload: { requestId: "unknown", requestType: "unsupported" },
      }),
    ];

    expect(derivePendingApprovals(activities)).toEqual([
      expect.objectContaining({ requestId: "dynamic", requestKind: "command" }),
      expect.objectContaining({ requestId: "read", requestKind: "file-read" }),
      expect.objectContaining({ requestId: "change", requestKind: "file-change" }),
    ]);
  });

  it("keeps only fully structured user-input questions and options", () => {
    const activities = [
      makeActivity({
        kind: "user-input.requested",
        payload: { requestId: "not-array", questions: {} },
      }),
      makeActivity({
        kind: "user-input.requested",
        payload: {
          requestId: "mixed",
          questions: [
            null,
            { id: 1, header: "Bad", question: "Bad", options: [] },
            { id: "empty", header: "Empty", question: "Empty?", options: [null, { label: 1 }] },
            {
              id: "valid",
              header: "Choice",
              question: "Pick one",
              options: [
                null,
                { label: "Broken", description: 7 },
                { label: "Keep", description: "A valid choice" },
              ],
              multiSelect: true,
            },
          ],
        },
      }),
    ];

    expect(derivePendingUserInputs(activities)).toEqual([
      {
        requestId: "mixed",
        createdAt: "2026-02-23T00:00:00.000Z",
        questions: [
          {
            id: "valid",
            header: "Choice",
            question: "Pick one",
            options: [{ label: "Keep", description: "A valid choice" }],
            multiSelect: true,
          },
        ],
      },
    ]);
  });

  it("ignores null payloads and non-stale provider response failures", () => {
    const nullPayload = {
      ...makeActivity({ kind: "approval.requested" }),
      payload: null,
    } as unknown as OrchestrationThreadActivity;
    expect(derivePendingApprovals([nullPayload])).toEqual([]);

    expect(
      derivePendingUserInputs([
        makeActivity({
          kind: "user-input.requested",
          payload: {
            requestId: "open",
            questions: [
              {
                id: "question",
                header: "Choice",
                question: "Pick one",
                options: [{ label: "One", description: "First" }],
              },
            ],
          },
        }),
        makeActivity({
          kind: "provider.user-input.respond.failed",
          payload: { requestId: "open", detail: "temporary transport failure" },
        }),
      ]),
    ).toEqual([
      expect.objectContaining({
        requestId: "open",
        questions: [expect.objectContaining({ multiSelect: false })],
      }),
    ]);
  });

  it("handles empty, primitive, and incomplete request payloads", () => {
    const nullPayload = {
      ...makeActivity({ kind: "user-input.requested" }),
      payload: null,
    } as unknown as OrchestrationThreadActivity;
    expect(
      derivePendingUserInputs([
        nullPayload,
        makeActivity({
          kind: "user-input.requested",
          payload: { requestId: 7, questions: [] },
        }),
        makeActivity({
          kind: "user-input.requested",
          payload: { requestId: "empty", questions: [] },
        }),
        makeActivity({
          kind: "provider.user-input.respond.failed",
          payload: { requestId: "empty" },
        }),
      ]),
    ).toEqual([]);

    expect(
      derivePendingApprovals([
        makeActivity({
          kind: "approval.requested",
          payload: { requestId: "change", requestType: "file_change_approval" },
        }),
      ]),
    ).toEqual([expect.objectContaining({ requestId: "change", requestKind: "file-change" })]);
  });

  it("returns no active plan for absent or unusable plan updates", () => {
    expect(deriveActivePlanState([], undefined)).toBeNull();
    expect(
      deriveActivePlanState(
        [makeActivity({ kind: "turn.plan.updated", payload: { plan: "not-an-array" } })],
        undefined,
      ),
    ).toBeNull();
    expect(
      deriveActivePlanState(
        [
          makeActivity({
            kind: "turn.plan.updated",
            payload: { plan: [null, "bad", { step: 1 }, { step: "Queued", status: "other" }] },
          }),
        ],
        undefined,
      ),
    ).toMatchObject({ steps: [{ step: "Queued", status: "pending" }] });
    expect(
      deriveActivePlanState(
        [
          makeActivity({
            kind: "turn.plan.updated",
            payload: { plan: [null, "bad", { step: 1 }] },
          }),
        ],
        undefined,
      ),
    ).toBeNull();

    const primitivePayload = {
      ...makeActivity({ kind: "turn.plan.updated" }),
      payload: "not-an-object",
    } as unknown as OrchestrationThreadActivity;
    expect(deriveActivePlanState([primitivePayload], undefined)).toBeNull();
  });

  it("uses plan ids as deterministic tie breakers", () => {
    const tiedPlans = [
      {
        id: "plan-a",
        turnId: TurnId.make("turn-tied"),
        planMarkdown: "# A",
        implementedAt: null,
        implementationThreadId: null,
        createdAt: "2026-02-23T00:00:01.000Z",
        updatedAt: "2026-02-23T00:00:02.000Z",
      },
      {
        id: "plan-b",
        turnId: TurnId.make("turn-tied"),
        planMarkdown: "# B",
        implementedAt: null,
        implementationThreadId: null,
        createdAt: "2026-02-23T00:00:01.000Z",
        updatedAt: "2026-02-23T00:00:02.000Z",
      },
    ];

    expect(findLatestProposedPlan(tiedPlans, TurnId.make("turn-tied"))?.id).toBe("plan-b");
    expect(findLatestProposedPlan(tiedPlans, null)?.id).toBe("plan-b");
  });

  it("falls back safely when proposed plans or source references are missing", () => {
    expect(findLatestProposedPlan([], null)).toBeNull();
    expect(
      findSidebarProposedPlan({
        threads: [],
        latestTurn: {
          turnId: TurnId.make("turn-missing"),
          sourceProposedPlan: {
            threadId: ThreadId.make("thread-missing"),
            planId: "plan-missing",
          },
        },
        latestTurnSettled: false,
        threadId: ThreadId.make("thread-active"),
      }),
    ).toBeNull();
    expect(hasActionableProposedPlan(null)).toBe(false);
    expect(
      findSidebarProposedPlan({
        threads: [],
        latestTurn: null,
        latestTurnSettled: true,
        threadId: null,
      }),
    ).toBeNull();
    expect(
      findSidebarProposedPlan({
        threads: [],
        latestTurn: {
          turnId: TurnId.make("turn-without-source"),
          sourceProposedPlan: undefined,
        },
        latestTurnSettled: false,
        threadId: ThreadId.make("thread-active"),
      }),
    ).toBeNull();
  });
});

describe("incomplete timing state", () => {
  it("treats a completed turn with no session as settled", () => {
    expect(
      isLatestTurnSettled(
        {
          turnId: TurnId.make("turn-complete"),
          startedAt: "2026-02-27T21:10:00.000Z",
          completedAt: "2026-02-27T21:10:01.000Z",
        },
        null,
      ),
    ).toBe(true);
  });

  it("falls back to the send timestamp when the latest turn has no start", () => {
    expect(
      deriveActiveWorkStartedAt(
        {
          turnId: TurnId.make("turn-incomplete"),
          startedAt: null,
          completedAt: null,
        },
        { status: "ready", activeTurnId: null },
        "2026-02-27T21:11:00.000Z",
      ),
    ).toBe("2026-02-27T21:11:00.000Z");
  });

  it("falls back to the send timestamp when a matching running turn has no recorded start", () => {
    expect(
      deriveActiveWorkStartedAt(
        {
          turnId: TurnId.make("turn-running"),
          startedAt: null,
          completedAt: null,
        },
        { status: "running", activeTurnId: TurnId.make("turn-running") },
        "2026-02-27T21:11:00.000Z",
      ),
    ).toBe("2026-02-27T21:11:00.000Z");
  });

  it("does not settle a started turn until it has a completion timestamp", () => {
    expect(
      isLatestTurnSettled(
        {
          turnId: TurnId.make("turn-started"),
          startedAt: "2026-02-27T21:10:00.000Z",
          completedAt: null,
        },
        null,
      ),
    ).toBe(false);
  });
});

describe("work-log fallback payloads", () => {
  it("renders non-object payloads without manufacturing tool metadata", () => {
    const activity = {
      ...makeActivity({ kind: "tool.completed", summary: "Finished" }),
      payload: "not-an-object",
    } as unknown as OrchestrationThreadActivity;

    expect(deriveWorkLogEntries([activity])).toEqual([
      expect.objectContaining({
        label: "Finished",
        sourceActivityKind: "tool.completed",
        toolLifecycleStatus: "completed",
      }),
    ]);
  });

  it("uses a task summary without repeating empty detail", () => {
    expect(
      deriveWorkLogEntries([
        makeActivity({
          kind: "task.completed",
          summary: "Task complete",
          payload: { summary: "Indexed project", detail: "" },
        }),
      ]),
    ).toEqual([
      expect.objectContaining({
        label: "Indexed project",
        sourceActivityKind: "task.completed",
      }),
    ]);
  });

  it("preserves explicit request kinds and ignores unknown item types", () => {
    const [entry] = deriveWorkLogEntries([
      makeActivity({
        kind: "tool.completed",
        payload: {
          itemType: "not-a-tool-type",
          requestKind: "file-change",
          data: { toolCallId: "  " },
        },
      }),
    ]);
    expect(entry).toMatchObject({ requestKind: "file-change" });
    expect(entry?.itemType).toBeUndefined();
  });

  it("keeps task detail separate when a payload summary supplies the label", () => {
    expect(
      deriveWorkLogEntries([
        makeActivity({
          kind: "task.completed",
          summary: "Task complete",
          payload: {
            summary: "Indexed project",
            detail: "Indexed 42 files <exited with exit code 0>",
          },
        }),
      ]),
    ).toEqual([
      expect.objectContaining({
        label: "Indexed project",
        detail: "Indexed 42 files",
      }),
    ]);
  });

  it("filters checkpoint rows after non-tool lifecycle filters", () => {
    expect(
      deriveWorkLogEntries([
        makeActivity({
          kind: "checkpoint.updated",
          summary: "Checkpoint captured",
          tone: "info",
        }),
      ]),
    ).toEqual([]);
  });

  it("normalizes approval-tone work rows to informational tone", () => {
    expect(
      deriveWorkLogEntries([
        makeActivity({
          kind: "approval.requested",
          summary: "Approval requested",
          tone: "approval",
        }),
      ])[0]?.tone,
    ).toBe("info");
  });

  it("merges prior command metadata and changed files into a sparse completion", () => {
    const entries = deriveWorkLogEntries([
      makeActivity({
        id: "merge-command-update",
        createdAt: "2026-02-23T00:00:01.000Z",
        kind: "tool.updated",
        summary: "Legacy command",
        payload: {
          itemType: "command_execution",
          requestKind: "command",
          data: {
            toolCallId: "merge-command",
            item: {
              command: "pwsh -Command 'echo hi'",
              changes: [{ path: "before.ts" }],
            },
          },
        },
      }),
      makeActivity({
        id: "merge-command-complete",
        createdAt: "2026-02-23T00:00:02.000Z",
        kind: "tool.completed",
        summary: "Legacy command completed",
        payload: {
          data: {
            toolCallId: "merge-command",
            item: { changes: [{ path: "after.ts" }] },
          },
        },
      }),
    ]);

    expect(entries).toEqual([
      expect.objectContaining({
        id: "merge-command-complete",
        command: "echo hi",
        rawCommand: "pwsh -Command 'echo hi'",
        changedFiles: ["before.ts", "after.ts"],
        itemType: "command_execution",
        requestKind: "command",
      }),
    ]);
  });

  it("collapses legacy blank-label completions and keeps non-tool rows separate", () => {
    const collapsed = deriveWorkLogEntries([
      makeActivity({
        id: "blank-tool-update",
        createdAt: "2026-02-23T00:00:01.000Z",
        kind: "tool.updated",
        summary: "  ",
        payload: { data: { toolCallId: "blank-tool" } },
      }),
      makeActivity({
        id: "blank-tool-complete",
        createdAt: "2026-02-23T00:00:02.000Z",
        kind: "tool.completed",
        summary: "  ",
        payload: {},
      }),
    ]);
    expect(collapsed.map((entry) => entry.id)).toEqual(["blank-tool-complete"]);

    const separated = deriveWorkLogEntries([
      makeActivity({
        id: "tool-before-status",
        createdAt: "2026-02-23T00:00:01.000Z",
        kind: "tool.updated",
        summary: "Tool update",
      }),
      makeActivity({
        id: "status-after-tool",
        createdAt: "2026-02-23T00:00:02.000Z",
        kind: "session.updated",
        summary: "Status update",
        tone: "info",
      }),
    ]);
    expect(separated.map((entry) => entry.id)).toEqual(["tool-before-status", "status-after-tool"]);
  });

  it("normalizes malformed and minimal command payloads defensively", () => {
    const entries = deriveWorkLogEntries([
      makeActivity({
        id: "command-unterminated",
        kind: "tool.completed",
        summary: "Unterminated",
        payload: { data: { item: { command: '"unterminated' } } },
      }),
      makeActivity({
        id: "command-empty-basename",
        kind: "tool.completed",
        summary: "Empty basename",
        payload: { data: { item: { command: "/ -c echo" } } },
      }),
      makeActivity({
        id: "command-bare-shell",
        kind: "tool.completed",
        summary: "Bare shell",
        payload: { data: { item: { command: "bash" } } },
      }),
      makeActivity({
        id: "command-empty-quoted-executable",
        kind: "tool.completed",
        summary: "Quoted executable",
        payload: { data: { item: { command: '"" -Command whoami' } } },
      }),
      makeActivity({
        id: "command-mixed-argv",
        kind: "tool.completed",
        summary: "Mixed argv",
        payload: {
          data: { item: { command: [null, " ", 1, "echo", 'say "hi"'] } },
        },
      }),
      makeActivity({
        id: "command-empty-argv",
        kind: "tool.completed",
        summary: "Empty argv",
        payload: { data: { item: { command: [null, "", 1] } } },
      }),
    ]);
    const byId = new Map(entries.map((entry) => [entry.id, entry]));

    expect(byId.get("command-unterminated")?.command).toBe('"unterminated');
    expect(byId.get("command-empty-basename")?.command).toBe("/ -c echo");
    expect(byId.get("command-bare-shell")?.command).toBe("bash");
    expect(byId.get("command-empty-quoted-executable")?.command).toBe('"" -Command whoami');
    expect(byId.get("command-mixed-argv")?.command).toBe('echo "say \\"hi\\""');
    expect(byId.get("command-empty-argv")?.command).toBeUndefined();
  });

  it("summarizes long, fenced, singular, and stdout tool output", () => {
    const longLine = "x".repeat(100);
    const entries = deriveWorkLogEntries([
      makeActivity({
        id: "raw-long",
        kind: "tool.completed",
        summary: "Read output",
        payload: { itemType: "dynamic_tool_call", data: { rawOutput: { content: longLine } } },
      }),
      makeActivity({
        id: "raw-fences",
        kind: "tool.completed",
        summary: "Read fences",
        payload: {
          itemType: "dynamic_tool_call",
          data: { rawOutput: { content: "```\n```" } },
        },
      }),
      makeActivity({
        id: "raw-singular",
        kind: "tool.completed",
        summary: "Search output",
        payload: {
          itemType: "web_search",
          data: { rawOutput: { totalFiles: 1, truncated: true } },
        },
      }),
      makeActivity({
        id: "raw-stdout",
        kind: "tool.completed",
        summary: "Read stdout",
        payload: {
          itemType: "dynamic_tool_call",
          data: { rawOutput: { stdout: "\n\nhello world" } },
        },
      }),
      makeActivity({
        id: "raw-interior-blank",
        kind: "tool.completed",
        summary: "Read lines",
        payload: {
          itemType: "dynamic_tool_call",
          data: { rawOutput: { content: "hello\n\nworld" } },
        },
      }),
      makeActivity({
        id: "raw-single-fence",
        kind: "tool.completed",
        summary: "Read fence",
        payload: {
          itemType: "dynamic_tool_call",
          data: { rawOutput: { content: "```" } },
        },
      }),
      makeActivity({
        id: "raw-empty",
        kind: "tool.completed",
        summary: "Read empty",
        payload: {
          itemType: "dynamic_tool_call",
          data: { rawOutput: {} },
        },
      }),
      makeActivity({
        id: "raw-repeated-heading",
        kind: "tool.completed",
        summary: "1 file+",
        payload: {
          itemType: "web_search",
          title: "1 file+",
          data: { rawOutput: { totalFiles: 1, truncated: true } },
        },
      }),
    ]);
    const byId = new Map(entries.map((entry) => [entry.id, entry]));

    expect(byId.get("raw-long")?.detail).toBe(`${"x".repeat(83)}…`);
    expect(byId.get("raw-fences")?.detail).toBe("2 lines");
    expect(byId.get("raw-singular")?.detail).toBe("1 file+");
    expect(byId.get("raw-stdout")?.detail).toBe("hello world");
    expect(byId.get("raw-interior-blank")?.detail).toBe("hello");
    expect(byId.get("raw-single-fence")?.detail).toBeUndefined();
    expect(byId.get("raw-empty")?.detail).toBeUndefined();
    expect(byId.get("raw-repeated-heading")?.detail).toBeUndefined();
  });

  it("drops an exit-code-only detail after parsing its lifecycle suffix", () => {
    const [entry] = deriveWorkLogEntries([
      makeActivity({
        kind: "tool.completed",
        summary: "Tool output",
        payload: {
          itemType: "dynamic_tool_call",
          detail: "<exited with exit code 0>",
        },
      }),
    ]);

    expect(entry?.detail).toBeUndefined();
  });

  it("bounds changed-file traversal by count and nesting depth", () => {
    const cappedFiles = Array.from({ length: 15 }, (_, index) => ({ path: `file-${index}.ts` }));
    const entries = deriveWorkLogEntries([
      makeActivity({
        id: "file-cap",
        kind: "tool.completed",
        summary: "Many files",
        payload: { itemType: "file_change", data: { files: cappedFiles } },
      }),
      makeActivity({
        id: "file-too-deep",
        kind: "tool.completed",
        summary: "Deep file",
        payload: {
          itemType: "file_change",
          data: { item: { result: { input: { data: { item: { path: "ignored.ts" } } } } } },
        },
      }),
    ]);

    expect(entries[0]?.changedFiles).toEqual(
      Array.from({ length: 12 }, (_, index) => `file-${index}.ts`),
    );
    expect(entries[1]?.changedFiles).toBeUndefined();
  });

  it("orders mixed and tied sequence values before applying lifecycle filters", () => {
    const entries = deriveWorkLogEntries([
      makeActivity({
        id: "sequenced-second",
        sequence: 2,
        createdAt: "2026-02-23T00:00:00.000Z",
        kind: "tool.completed",
        summary: "Sequenced second",
      }),
      makeActivity({
        id: "unsequenced-first",
        createdAt: "2026-02-23T00:00:01.000Z",
        kind: "tool.completed",
        summary: "Unsequenced first",
      }),
      makeActivity({
        id: "same-sequence-a",
        sequence: 1,
        createdAt: "2026-02-23T00:00:02.000Z",
        kind: "tool.completed",
        summary: "Same sequence A",
      }),
      makeActivity({
        id: "same-sequence-b",
        sequence: 1,
        createdAt: "2026-02-23T00:00:03.000Z",
        kind: "tool.completed",
        summary: "Same sequence B",
      }),
      makeActivity({
        id: "started-filtered",
        createdAt: "2026-02-23T00:00:01.000Z",
        kind: "task.started",
        summary: "Started",
      }),
    ]);

    expect(entries.map((entry) => entry.id)).toEqual([
      "unsequenced-first",
      "same-sequence-a",
      "same-sequence-b",
      "sequenced-second",
    ]);
  });
});

describe("checkpoint and phase helpers", () => {
  it("numbers checkpoints chronologically and skips sparse array holes", () => {
    const sparseSummaries: unknown[] = [];
    sparseSummaries.length = 3;
    sparseSummaries[0] = {
      turnId: TurnId.make("turn-later"),
      completedAt: "2026-01-01T00:00:02.000Z",
    };
    sparseSummaries[2] = {
      turnId: TurnId.make("turn-earlier"),
      completedAt: "2026-01-01T00:00:01.000Z",
    };

    expect(sparseSummaries).toHaveLength(3);
    expect(1 in sparseSummaries).toBe(false);
    expect(
      inferCheckpointTurnCountByTurnId(
        sparseSummaries as unknown as Parameters<typeof inferCheckpointTurnCountByTurnId>[0],
      ),
    ).toEqual({
      "turn-earlier": 1,
      "turn-later": 2,
    });
  });

  it.each([
    [null, "disconnected"],
    [{ status: "stopped" }, "disconnected"],
    [{ status: "interrupted" }, "disconnected"],
    [{ status: "error" }, "disconnected"],
    [{ status: "starting" }, "connecting"],
    [{ status: "running" }, "running"],
    [{ status: "ready" }, "ready"],
  ] as const)("maps session %o to %s", (session, expected) => {
    expect(derivePhase(session as Parameters<typeof derivePhase>[0])).toBe(expected);
  });
});
