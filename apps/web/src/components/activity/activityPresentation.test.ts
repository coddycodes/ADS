import type { ActivityLifecycle, ActivitySnapshot } from "@coddy/contracts";
import { describe, expect, it } from "vite-plus/test";

import {
  activityElapsedLabel,
  activityStatusLabel,
  selectActivityDockVisibility,
} from "./activityPresentation";

const baseSnapshot = {
  protocolVersion: 2,
  scopeId: "scope-1",
  scope: { _tag: "thread", threadId: "thread-1" },
  revision: 1,
  provider: "codex",
  providerInstanceId: "codex",
  capabilities: {
    actors: true,
    attributedActivity: true,
    backgroundWork: true,
    historyRecovery: "full",
    terminalObservation: false,
    targetedActorCancellation: false,
  },
  observationState: "live",
  sections: {
    subagents: { state: "live", message: null, retryable: false },
    backgroundTasks: { state: "live", message: null, retryable: false },
  },
  counts: {
    subagents: { active: 0, done: 0 },
    backgroundTasks: { active: 0, done: 0 },
  },
  actors: [],
  workItems: [],
  actorsHasMore: false,
  workItemsHasMore: false,
  control: { scopeId: "scope-1", revision: 0, actors: [], operations: [] },
  updatedAt: "2026-07-22T12:00:00Z",
} as unknown as ActivitySnapshot;

function snapshot(
  input: {
    readonly actors?: boolean;
    readonly backgroundWork?: boolean;
    readonly subagentsState?: ActivitySnapshot["sections"]["subagents"]["state"];
    readonly backgroundTasksState?: ActivitySnapshot["sections"]["backgroundTasks"]["state"];
    readonly subagentsActive?: number;
    readonly subagentsDone?: number;
    readonly backgroundTasksActive?: number;
    readonly backgroundTasksDone?: number;
  } = {},
): ActivitySnapshot {
  return {
    ...baseSnapshot,
    capabilities: {
      ...baseSnapshot.capabilities,
      ...(input.actors === undefined ? {} : { actors: input.actors }),
      ...(input.backgroundWork === undefined ? {} : { backgroundWork: input.backgroundWork }),
    },
    sections: {
      subagents: {
        ...baseSnapshot.sections.subagents,
        ...(input.subagentsState === undefined ? {} : { state: input.subagentsState }),
      },
      backgroundTasks: {
        ...baseSnapshot.sections.backgroundTasks,
        ...(input.backgroundTasksState === undefined ? {} : { state: input.backgroundTasksState }),
      },
    },
    counts: {
      subagents: {
        active: input.subagentsActive ?? 0,
        done: input.subagentsDone ?? 0,
      },
      backgroundTasks: {
        active: input.backgroundTasksActive ?? 0,
        done: input.backgroundTasksDone ?? 0,
      },
    },
  };
}

describe("selectActivityDockVisibility", () => {
  it("hides a missing snapshot", () => {
    expect(selectActivityDockVisibility(null)).toEqual({
      visible: false,
      showSubagents: false,
      showBackgroundTasks: false,
    });
  });

  it("shows Subagents for either exact non-zero active or done count", () => {
    expect(selectActivityDockVisibility(snapshot({ subagentsActive: 1 }))).toEqual({
      visible: true,
      showSubagents: true,
      showBackgroundTasks: false,
    });
    expect(selectActivityDockVisibility(snapshot({ subagentsDone: 1 }))).toEqual({
      visible: true,
      showSubagents: true,
      showBackgroundTasks: false,
    });
  });

  it("checks counts without adding them and overflowing", () => {
    expect(
      selectActivityDockVisibility(
        snapshot({
          subagentsActive: Number.MAX_VALUE,
          subagentsDone: Number.MAX_VALUE,
        }),
      ),
    ).toEqual({
      visible: true,
      showSubagents: true,
      showBackgroundTasks: false,
    });
  });

  it("always hides an unsupported section despite capability or malformed counts", () => {
    const visibility = selectActivityDockVisibility(
      snapshot({
        actors: true,
        backgroundWork: false,
        subagentsState: "unsupported",
        backgroundTasksState: "unsupported",
        subagentsActive: 2,
        backgroundTasksActive: 99,
      }),
    );

    expect(visibility).toEqual({
      visible: false,
      showSubagents: false,
      showBackgroundTasks: false,
    });
  });

  it("retains only the downgraded stale section with records", () => {
    expect(
      selectActivityDockVisibility(
        snapshot({
          actors: false,
          backgroundWork: false,
          subagentsState: "live",
          backgroundTasksState: "stale",
          subagentsActive: 1,
          backgroundTasksDone: 3,
        }),
      ),
    ).toEqual({
      visible: true,
      showSubagents: false,
      showBackgroundTasks: true,
    });
  });

  it("retains errored history after downgrade but not live phantom sections", () => {
    expect(
      selectActivityDockVisibility(
        snapshot({
          actors: false,
          backgroundWork: false,
          subagentsState: "error",
          backgroundTasksState: "live",
          subagentsDone: 1,
          backgroundTasksActive: 1,
        }),
      ),
    ).toEqual({
      visible: true,
      showSubagents: true,
      showBackgroundTasks: false,
    });
  });

  it("keeps all-zero supported sections hidden until a record exists", () => {
    expect(selectActivityDockVisibility(snapshot())).toEqual({
      visible: false,
      showSubagents: false,
      showBackgroundTasks: false,
    });
  });
});

describe("activityStatusLabel", () => {
  it("returns an exhaustive distinct label for every lifecycle", () => {
    const statuses: readonly ActivityLifecycle[] = [
      "starting",
      "running",
      "waiting",
      "completed",
      "failed",
      "cancelled",
      "interrupted",
      "unknown",
    ];
    const labels = statuses.map(activityStatusLabel);

    expect(labels).toEqual([
      "Starting",
      "Running",
      "Waiting",
      "Completed",
      "Failed",
      "Cancelled",
      "Interrupted",
      "Unknown",
    ]);
    expect(new Set(labels).size).toBe(statuses.length);
  });
});

describe("activityElapsedLabel", () => {
  const now = "2026-07-22T12:00:00Z";

  it("formats deterministic second, minute, hour, and day boundaries", () => {
    expect(activityElapsedLabel("2026-07-22T12:00:00Z", now)).toBe("0s");
    expect(activityElapsedLabel("2026-07-22T11:59:59Z", now)).toBe("1s");
    expect(activityElapsedLabel("2026-07-22T11:59:00Z", now)).toBe("1m");
    expect(activityElapsedLabel("2026-07-22T11:00:00Z", now)).toBe("1h");
    expect(activityElapsedLabel("2026-07-21T12:00:00Z", now)).toBe("1d");
  });

  it("parses RFC3339 offsets as real instants", () => {
    expect(activityElapsedLabel("2026-07-22T11:59:30Z", now)).toBe("30s");
    expect(activityElapsedLabel("2026-07-22T08:59:30-03:00", now)).toBe("30s");
    expect(activityElapsedLabel("2026-07-22T13:59:00+02:00", now)).toBe("1m");
  });

  it("accepts the RFC3339 offset boundary at positive and negative 14 hours", () => {
    expect(activityElapsedLabel("2026-07-23T01:59:00+14:00", now)).toBe("1m");
    expect(activityElapsedLabel("2026-07-21T21:59:00-14:00", now)).toBe("1m");
  });

  it("rejects offsets beyond positive or negative 14 hours", () => {
    const invalidOffsetLabels = [
      activityElapsedLabel("2026-07-23T01:58:00+14:01", now),
      activityElapsedLabel("2026-07-21T21:58:00-14:01", now),
      activityElapsedLabel("2026-07-23T10:58:00+23:59", now),
      activityElapsedLabel("2026-07-21T12:02:00-23:59", now),
    ];

    expect(invalidOffsetLabels).toEqual(["0s", "0s", "0s", "0s"]);
  });

  it("clamps future instants to zero", () => {
    expect(activityElapsedLabel("2026-07-22T12:01:00Z", now)).toBe("0s");
  });

  it("handles invalid or non-RFC3339 input without NaN or Infinity", () => {
    const invalidLabels = [
      activityElapsedLabel("invalid", now),
      activityElapsedLabel("2026-02-30T12:00:00Z", now),
      activityElapsedLabel("2026-07-22", now),
      activityElapsedLabel(now, "invalid"),
    ];

    expect(invalidLabels).toEqual(["0s", "0s", "0s", "0s"]);
    for (const label of invalidLabels) {
      expect(label).not.toMatch(/NaN|Infinity/);
    }
  });
});
