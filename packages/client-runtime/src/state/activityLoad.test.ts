import {
  ACTIVITY_PAGE_MAX_LENGTH,
  ActivityRecordId,
  ActivityScopeId,
  ProviderDriverKind,
  ThreadId,
  type ActivityActorSummary,
  type ActivityControlDelta,
  type ActivityDelta,
  type ActivitySnapshot,
  type ActivityWorkItemSummary,
} from "@coddy/contracts";
import { describe, expect, it } from "@effect/vitest";
import * as Option from "effect/Option";

import {
  applyActivityDelta,
  applyEnvironmentActivityControlDelta,
  applyEnvironmentActivityDelta,
  type EnvironmentActivityState,
} from "./activityReducer.ts";

const SCOPE_ID = ActivityScopeId.make("scope:load-thread");
const THREAD_ID = ThreadId.make("load-thread");
const BASE_TIMESTAMP = "2026-07-22T12:00:00.0000Z";
const ACTIVITY_LOAD_ASSERTION_LIMIT_MS = 20_000;
const ACTIVITY_LOAD_TEST_TIMEOUT_MS = 25_000;

function timestamp(index: number): string {
  return `2026-07-22T12:00:00.${String(index).padStart(4, "0")}Z`;
}

function actor(index: number, overrides: Partial<ActivityActorSummary> = {}): ActivityActorSummary {
  const id = ActivityRecordId.make(`actor:${String(index).padStart(4, "0")}`);
  const updatedAt = timestamp(index);
  return {
    _tag: "actor",
    id,
    parentActorId: null,
    name: `Actor ${index}`,
    role: "worker",
    providerType: "codex",
    status: "running",
    summary: `Processing event ${index}`,
    startedAt: updatedAt,
    updatedAt,
    terminalAt: null,
    ...overrides,
  };
}

function workItem(
  index: number,
  overrides: Partial<ActivityWorkItemSummary> = {},
): ActivityWorkItemSummary {
  const id = ActivityRecordId.make(`work:${String(index).padStart(4, "0")}`);
  const updatedAt = timestamp(index);
  return {
    _tag: "workItem",
    id,
    ownerActorId: null,
    name: `Work ${index}`,
    workKind: "command",
    command: "vp check",
    cwd: "/workspace",
    status: "running",
    summary: `Processing event ${index}`,
    startedAt: updatedAt,
    updatedAt,
    terminalAt: null,
    ...overrides,
  };
}

function snapshot(overrides: Partial<ActivitySnapshot> = {}): ActivitySnapshot {
  return {
    protocolVersion: 2,
    scopeId: SCOPE_ID,
    scope: { _tag: "thread", threadId: THREAD_ID },
    revision: 0,
    provider: ProviderDriverKind.make("codex"),
    providerInstanceId: null,
    capabilities: {
      actors: true,
      attributedActivity: true,
      backgroundWork: true,
      historyRecovery: "full",
      terminalObservation: false,
      targetedActorCancellation: true,
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
    control: {
      scopeId: SCOPE_ID,
      revision: 0,
      actors: [],
      operations: [],
    },
    updatedAt: BASE_TIMESTAMP,
    ...overrides,
  };
}

function delta(index: number): ActivityDelta {
  const revision = index + 1;
  const change =
    index % 2 === 0
      ? { kind: "actor-upserted" as const, actor: actor(index) }
      : { kind: "work-item-upserted" as const, workItem: workItem(index) };
  return {
    scopeId: SCOPE_ID,
    previousRevision: index,
    revision,
    changes: [change],
    updatedAt: timestamp(index),
  };
}

function state(): EnvironmentActivityState {
  return {
    snapshot: Option.some(snapshot()),
    status: "live",
    error: Option.none(),
    recentEntries: new Map(),
  };
}

function applyLoadRevisions(): EnvironmentActivityState {
  let current = state();
  const revisions = Array.from({ length: 5_000 }, (_, index) => delta(index));
  for (const batch of Array.from({ length: revisions.length / 25 }, (_, batchIndex) =>
    revisions.slice(batchIndex * 25, batchIndex * 25 + 25),
  )) {
    for (const next of batch) {
      const result = applyEnvironmentActivityDelta(current, next);
      if (result.kind !== "applied") {
        throw new Error(`load revision ${next.revision} was not applied`);
      }
      current = result.state;
    }
  }
  return current;
}

describe("activity load reducer", () => {
  it(
    "keeps 5,000 ordered revisions within the snapshot page caps",
    () => {
      const startedAt = performance.now();
      const current = applyLoadRevisions();
      const loaded = Option.getOrThrow(current.snapshot);
      const elapsedMs = performance.now() - startedAt;

      expect(loaded.revision).toBe(5_000);
      expect(loaded.actors).toHaveLength(ACTIVITY_PAGE_MAX_LENGTH);
      expect(loaded.workItems).toHaveLength(ACTIVITY_PAGE_MAX_LENGTH);
      expect(loaded.actors[0]?.id).toBe(ActivityRecordId.make("actor:4998"));
      expect(loaded.actors.at(-1)?.id).toBe(ActivityRecordId.make("actor:4600"));
      expect(loaded.workItems[0]?.id).toBe(ActivityRecordId.make("work:4999"));
      expect(loaded.workItems.at(-1)?.id).toBe(ActivityRecordId.make("work:4601"));
      expect(loaded.actorsHasMore).toBe(true);
      expect(loaded.workItemsHasMore).toBe(true);
      expect(elapsedMs, "activity reducer load elapsed time").toBeLessThan(
        ACTIVITY_LOAD_ASSERTION_LIMIT_MS,
      );
    },
    ACTIVITY_LOAD_TEST_TIMEOUT_MS,
  );

  it("reconciles after a capped page loses an active row instead of silently dropping its refill", () => {
    const capped = snapshot({
      revision: 1,
      actors: Array.from({ length: ACTIVITY_PAGE_MAX_LENGTH }, (_, index) => actor(index)),
      workItems: Array.from({ length: ACTIVITY_PAGE_MAX_LENGTH }, (_, index) => workItem(index)),
      actorsHasMore: true,
      workItemsHasMore: true,
    });
    expect(capped.scopeId).toBe(SCOPE_ID);
    expect(capped.revision).toBe(1);

    const overflow = applyActivityDelta(capped, {
      scopeId: capped.scopeId,
      previousRevision: 1,
      revision: 2,
      changes: [{ kind: "actor-upserted", actor: actor(9_999) }],
      updatedAt: timestamp(9_999),
    });
    expect(overflow.kind).toBe("applied");
    if (overflow.kind === "applied") {
      expect(overflow.snapshot.actors[0]?.id).toBe(ActivityRecordId.make("actor:9999"));
      expect(overflow.snapshot.actorsHasMore).toBe(true);
    }

    const demotion = applyActivityDelta(capped, {
      scopeId: capped.scopeId,
      previousRevision: 1,
      revision: 2,
      changes: [
        {
          kind: "actor-upserted",
          actor: actor(0, {
            status: "completed",
            updatedAt: timestamp(9_999),
            terminalAt: timestamp(9_999),
          }),
        },
        { kind: "work-item-removed", workItemId: workItem(1).id },
      ],
      updatedAt: timestamp(9_999),
    });
    expect(demotion).toEqual({ kind: "gap" });
  });

  it(
    "preserves the final bounded snapshot across duplicate and gap revisions",
    () => {
      const applied = applyLoadRevisions();
      const loaded = Option.getOrThrow(applied.snapshot);

      const duplicate = applyEnvironmentActivityDelta(applied, {
        ...delta(4_999),
        previousRevision: 4_999,
        revision: 5_000,
      });
      expect(duplicate.kind).toBe("duplicate");
      expect(duplicate.state).toBe(applied);

      const gap = applyEnvironmentActivityDelta(applied, {
        ...delta(5_001),
        previousRevision: 5_001,
        revision: 5_002,
      });
      expect(gap.kind).toBe("gap");
      expect(gap.state.status).toBe("stale");
      expect(Option.getOrThrow(gap.state.snapshot)).toBe(loaded);
      expect(Option.getOrThrow(gap.state.snapshot).revision).toBe(5_000);
      expect(Option.getOrThrow(gap.state.snapshot).actors).toHaveLength(ACTIVITY_PAGE_MAX_LENGTH);
      expect(Option.getOrThrow(gap.state.snapshot).workItems).toHaveLength(
        ACTIVITY_PAGE_MAX_LENGTH,
      );
    },
    ACTIVITY_LOAD_TEST_TIMEOUT_MS,
  );

  it("keeps requested control state across duplicate, reordered, and gapped load revisions", () => {
    const actorId = ActivityRecordId.make("actor:control-load");
    let current: EnvironmentActivityState = {
      ...state(),
      snapshot: Option.some(
        snapshot({
          actors: [actor(0, { id: actorId, name: "Control load" })],
          control: {
            scopeId: SCOPE_ID,
            revision: 0,
            actors: [],
            operations: [],
          },
        }),
      ),
    };

    const controls = Array.from({ length: 5_000 }, (_, index): ActivityControlDelta => ({
      scopeId: SCOPE_ID,
      previousRevision: index,
      revision: index + 1,
      changes: [
        {
          kind: "actor-upserted",
          actor: {
            actorId,
            state: index === 4_999 ? "requested" : "available",
            controlRevision: index + 1,
            activeDescendantCount: 1,
          },
        },
      ],
    }));
    for (const next of controls) {
      const result = applyEnvironmentActivityControlDelta(current, next);
      if (result.kind !== "applied") {
        throw new Error(`control load revision ${next.revision} was not applied`);
      }
      current = result.state;
    }

    const loaded = Option.getOrThrow(current.snapshot);
    expect(loaded.revision).toBe(0);
    expect(loaded.control.revision).toBe(5_000);
    expect(loaded.control.actors).toEqual([
      {
        actorId,
        state: "requested",
        controlRevision: 5_000,
        activeDescendantCount: 1,
      },
    ]);

    const duplicate = applyEnvironmentActivityControlDelta(current, controls[4_999]!);
    expect(duplicate.kind).toBe("duplicate");
    expect(duplicate.state).toBe(current);

    const reordered = applyEnvironmentActivityControlDelta(current, controls[4_998]!);
    expect(reordered.kind).toBe("duplicate");
    expect(reordered.state).toBe(current);

    const gap = applyEnvironmentActivityControlDelta(current, {
      scopeId: SCOPE_ID,
      previousRevision: 5_001,
      revision: 5_002,
      changes: [],
    });
    expect(gap.kind).toBe("gap");
    expect(gap.state.status).toBe("stale");
    expect(Option.getOrThrow(gap.state.snapshot).control).toBe(loaded.control);
    expect(Option.getOrThrow(gap.state.snapshot).control.actors[0]?.state).toBe("requested");
  });
});
