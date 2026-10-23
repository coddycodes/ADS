import { Schema } from "effect";
import { describe, expect, it } from "vite-plus/test";

import * as ActivityModule from "./activity.ts";
import * as ContractsPackage from "./index.ts";
import {
  ActivityDelta,
  ActivityDetailPage,
  ActivityError,
  ActivityListDetailInput,
  ActivityListRosterInput,
  ActivityScopeRef,
  ActivitySnapshot,
  ActivityStreamItem,
} from "./activity.ts";

// The Rust server rejects unknown control-input fields; Effect 4 parse options
// apply per operation, so the strict decoders opt in explicitly.
const strictParseOptions = { onExcessProperty: "error" } as const;
const decodeActivityDelta = Schema.decodeUnknownSync(ActivityDelta);
const decodeActivityError = Schema.decodeUnknownSync(ActivityError);
const decodeActivityDetailPage = Schema.decodeUnknownSync(ActivityDetailPage);
const decodeActivityListDetailInput = Schema.decodeUnknownSync(ActivityListDetailInput);
const decodeActivityListRosterInput = Schema.decodeUnknownSync(ActivityListRosterInput);
const decodeActivityScopeRef = Schema.decodeUnknownSync(ActivityScopeRef);
const decodeActivitySnapshot = Schema.decodeUnknownSync(ActivitySnapshot);
const decodeActivityStreamItem = Schema.decodeUnknownSync(ActivityStreamItem);

const actor = {
  _tag: "actor" as const,
  id: "actor:child-1",
  parentActorId: null,
  name: "Explore provider events",
  role: "explorer",
  providerType: "worker",
  status: "running" as const,
  summary: "Reading App Server schemas",
  startedAt: "2026-07-22T12:00:00Z",
  updatedAt: "2026-07-22T12:00:01Z",
  terminalAt: null,
};

const snapshot = {
  protocolVersion: 2 as const,
  scopeId: "thread:thread-1",
  scope: { _tag: "thread" as const, threadId: "thread-1" },
  revision: 3,
  provider: "codex",
  providerInstanceId: "codex",
  capabilities: {
    actors: true,
    attributedActivity: true,
    backgroundWork: true,
    historyRecovery: "full" as const,
    terminalObservation: false,
    targetedActorCancellation: true,
  },
  observationState: "live" as const,
  sections: {
    subagents: { state: "live" as const, message: null, retryable: false },
    backgroundTasks: { state: "live" as const, message: null, retryable: false },
  },
  counts: {
    subagents: { active: 1, done: 0 },
    backgroundTasks: { active: 0, done: 0 },
  },
  actors: [actor],
  workItems: [],
  actorsHasMore: false,
  workItemsHasMore: false,
  control: {
    scopeId: "thread:thread-1",
    revision: 8,
    actors: [
      {
        actorId: "actor:child-1",
        state: "available",
        controlRevision: 3,
        activeDescendantCount: 2,
      },
    ],
    operations: [],
  },
  updatedAt: "2026-07-22T12:00:01Z",
};
const delta = {
  scopeId: snapshot.scopeId,
  previousRevision: 3,
  revision: 4,
  changes: [{ kind: "actor-upserted" as const, actor }],
  updatedAt: "2026-07-22T12:00:02Z",
};

describe("activity contracts", () => {
  it("decodes feature-disabled activity failures", () => {
    // Mutation caught: omitting featureDisabled from the ActivityError wire reason union.
    expect(
      decodeActivityError({
        _tag: "ActivityError",
        reason: "featureDisabled",
        message: "Agent activity is disabled for this environment.",
      }).reason,
    ).toBe("featureDisabled");
  });

  it("decodes targeted cancellation failures without provider-native details", () => {
    // Mutation caught: omitting a typed failure that clients need to present cancellation state safely.
    for (const reason of [
      "cancellationUnsupported",
      "staleScope",
      "staleActor",
      "staleOperation",
      "providerUnavailable",
      "targetUnavailable",
      "partialCancellation",
      "dispatchTimeout",
    ]) {
      expect(
        decodeActivityError({
          _tag: "ActivityError",
          reason,
          message: "Cancellation state changed. Refresh activity and try again.",
        }).reason,
      ).toBe(reason);
    }
  });

  it("keeps activity timestamp bounds private", () => {
    expect(ActivityModule).not.toHaveProperty("ACTIVITY_TIMESTAMP_MAX_LENGTH");
    expect(ContractsPackage).not.toHaveProperty("ACTIVITY_TIMESTAMP_MAX_LENGTH");
  });

  it("round-trips a thread scope snapshot and stream item", () => {
    expect(decodeActivityScopeRef(snapshot.scope)).toEqual(snapshot.scope);
    expect(decodeActivitySnapshot(snapshot)).toEqual(snapshot);
    expect(decodeActivityStreamItem({ kind: "snapshot", snapshot })).toEqual({
      kind: "snapshot",
      snapshot,
    });
  });

  it("round-trips an ordered actor delta", () => {
    expect(decodeActivityDelta(delta)).toEqual(delta);
  });

  it("requires protocol v2 activity snapshots to carry targeted cancellation capabilities", () => {
    // Mutation caught: accepting the old v1 snapshot shape or defaulting a v2 capability.
    expect(decodeActivitySnapshot(snapshot)).toEqual(snapshot);
    expect(() =>
      decodeActivitySnapshot({
        ...snapshot,
        protocolVersion: 1,
      }),
    ).toThrow();
    expect(() =>
      decodeActivitySnapshot({
        ...snapshot,
        capabilities: {
          actors: true,
          attributedActivity: true,
          backgroundWork: true,
          historyRecovery: "full",
          terminalObservation: false,
        },
      }),
    ).toThrow();
    expect(ActivityModule.NO_ACTIVITY_CAPABILITIES.targetedActorCancellation).toBe(false);
  });

  it("round-trips independent control snapshots and deltas", () => {
    // Mutation caught: dropping control state or coupling its revisions to activity revisions.
    const controlSnapshot = {
      scopeId: snapshot.scopeId,
      revision: 12,
      actors: [
        {
          actorId: actor.id,
          state: "unsupported",
          controlRevision: 0,
          activeDescendantCount: 0,
        },
        {
          actorId: "actor:available",
          state: "available",
          controlRevision: 4,
          activeDescendantCount: 3,
        },
        {
          actorId: "actor:requested",
          state: "requested",
          controlRevision: 5,
          activeDescendantCount: 1,
        },
      ],
      operations: [
        {
          rootActorId: "actor:available",
          state: "requested",
          residualCount: 0,
          message: null,
          operationRevision: 2,
        },
        {
          rootActorId: "actor:requested",
          state: "partial",
          residualCount: 1,
          message: "One descendant could not be stopped.",
          operationRevision: 3,
        },
      ],
    } as const;
    const controlDelta = {
      scopeId: snapshot.scopeId,
      previousRevision: 12,
      revision: 13,
      changes: [
        {
          kind: "actor-upserted",
          actor: controlSnapshot.actors[1],
        },
        {
          kind: "operation-upserted",
          operation: controlSnapshot.operations[1],
        },
      ],
    } as const;
    const decodeControlSnapshot = Schema.decodeUnknownSync(
      Reflect.get(ActivityModule, "ActivityControlSnapshot") as Schema.Codec<
        unknown,
        unknown,
        never,
        never
      >,
    );
    const decodeControlDelta = Schema.decodeUnknownSync(
      Reflect.get(ActivityModule, "ActivityControlDelta") as Schema.Codec<
        unknown,
        unknown,
        never,
        never
      >,
    );

    expect(Reflect.get(ActivityModule, "ActivityControlSnapshot")).toBeDefined();
    expect(Reflect.get(ActivityModule, "ActivityControlDelta")).toBeDefined();
    expect(decodeControlSnapshot(controlSnapshot)).toEqual(controlSnapshot);
    expect(decodeControlDelta(controlDelta)).toEqual(controlDelta);
    expect(
      decodeActivityStreamItem({ kind: "control-snapshot", control: controlSnapshot }),
    ).toEqual({ kind: "control-snapshot", control: controlSnapshot });
    expect(decodeActivityStreamItem({ kind: "control-delta", delta: controlDelta })).toEqual({
      kind: "control-delta",
      delta: controlDelta,
    });
  });

  it("returns actor controls with roster and detail pages", () => {
    // Mutation caught: omitting server-authoritative actor control state from paged activity reads.
    const actorControl = snapshot.control.actors[0];
    expect(
      decodeActivityListRosterInput({
        scope: snapshot.scope,
        scopeId: snapshot.scopeId,
        section: "subagents",
        bucket: "active",
      }),
    ).toMatchObject({ scopeId: snapshot.scopeId });
    const decodeRosterPage = Schema.decodeUnknownSync(
      Reflect.get(ActivityModule, "ActivityRosterPage") as Schema.Codec<
        unknown,
        unknown,
        never,
        never
      >,
    );
    expect(
      decodeRosterPage({ records: [actor], actorControls: [actorControl], nextCursor: null }),
    ).toEqual({ records: [actor], actorControls: [actorControl], nextCursor: null });
    expect(
      decodeActivityDetailPage({
        record: actor,
        actorControl,
        entries: [],
        nextCursor: null,
      }),
    ).toEqual({ record: actor, actorControl, entries: [], nextCursor: null });
  });

  it("accepts client-only cancellation commands and every result disposition", () => {
    // Mutation caught: exposing provider-native target data or omitting one server disposition.
    const cancelInput = {
      scope: snapshot.scope,
      scopeId: snapshot.scopeId,
      actorId: actor.id,
      expectedControlRevision: 3,
    } as const;
    const retryInput = {
      scope: snapshot.scope,
      scopeId: snapshot.scopeId,
      rootActorId: actor.id,
      expectedOperationRevision: 4,
    } as const;
    const decodeCancelInput = Schema.decodeUnknownSync(
      Reflect.get(ActivityModule, "ActivityCancelSubtreeInput") as Schema.Codec<
        unknown,
        unknown,
        never,
        never
      >,
      strictParseOptions,
    );
    const decodeRetryInput = Schema.decodeUnknownSync(
      Reflect.get(ActivityModule, "ActivityRetrySubtreeCancellationInput") as Schema.Codec<
        unknown,
        unknown,
        never,
        never
      >,
      strictParseOptions,
    );
    const decodeResult = Schema.decodeUnknownSync(
      Reflect.get(ActivityModule, "ActivitySubtreeCancellationResult") as Schema.Codec<
        unknown,
        unknown,
        never,
        never
      >,
    );

    expect(Reflect.get(ActivityModule, "ActivityCancelSubtreeInput")).toBeDefined();
    expect(Reflect.get(ActivityModule, "ActivityRetrySubtreeCancellationInput")).toBeDefined();
    expect(Reflect.get(ActivityModule, "ActivitySubtreeCancellationResult")).toBeDefined();
    expect(decodeCancelInput(cancelInput)).toEqual(cancelInput);
    expect(decodeRetryInput(retryInput)).toEqual(retryInput);
    for (const result of [
      { disposition: "accepted", rootActorId: actor.id, operationRevision: 5 },
      { disposition: "inProgress", rootActorId: actor.id, operationRevision: 5 },
      { disposition: "alreadyTerminal", rootActorId: actor.id, operationRevision: null },
    ] as const) {
      expect(decodeResult(result)).toEqual(result);
    }
  });

  it("rejects malformed control state and cancellation payloads", () => {
    // Mutation caught: accepting stale/unsafe control revisions or provider-native cancellation targets.
    const controlSnapshot = snapshot.control;
    const decodeControlSnapshot = Schema.decodeUnknownSync(
      Reflect.get(ActivityModule, "ActivityControlSnapshot") as Schema.Codec<
        unknown,
        unknown,
        never,
        never
      >,
    );
    const decodeCancelInput = Schema.decodeUnknownSync(
      Reflect.get(ActivityModule, "ActivityCancelSubtreeInput") as Schema.Codec<
        unknown,
        unknown,
        never,
        never
      >,
      strictParseOptions,
    );
    const decodeRetryInput = Schema.decodeUnknownSync(
      Reflect.get(ActivityModule, "ActivityRetrySubtreeCancellationInput") as Schema.Codec<
        unknown,
        unknown,
        never,
        never
      >,
      strictParseOptions,
    );

    expect(Reflect.get(ActivityModule, "ActivityControlSnapshot")).toBeDefined();
    expect(Reflect.get(ActivityModule, "ActivityCancelSubtreeInput")).toBeDefined();
    expect(Reflect.get(ActivityModule, "ActivityRetrySubtreeCancellationInput")).toBeDefined();
    expect(() =>
      decodeControlSnapshot({
        ...controlSnapshot,
        actors: [{ ...controlSnapshot.actors[0], state: "cancelling" }],
      }),
    ).toThrow();
    expect(() =>
      decodeControlSnapshot({
        ...controlSnapshot,
        revision: -1,
      }),
    ).toThrow();
    expect(() =>
      decodeControlSnapshot({
        ...controlSnapshot,
        actors: [{ ...controlSnapshot.actors[0], activeDescendantCount: -1 }],
      }),
    ).toThrow();
    expect(() =>
      decodeControlSnapshot({
        ...controlSnapshot,
        operations: [
          {
            rootActorId: actor.id,
            state: "partial",
            residualCount: 1,
            message: "x".repeat(2_049),
            operationRevision: 1,
          },
        ],
      }),
    ).toThrow();
    expect(() =>
      decodeCancelInput({
        scope: snapshot.scope,
        scopeId: snapshot.scopeId,
        actorId: actor.id,
        expectedControlRevision: 3,
        nativeThreadId: "provider-thread-1",
      }),
    ).toThrow();
    expect(() =>
      decodeRetryInput({
        scope: { _tag: "terminal", threadId: "thread-1", terminalId: "term-1" },
        scopeId: snapshot.scopeId,
        rootActorId: actor.id,
        expectedOperationRevision: 4,
        descendantIds: ["provider-child-1"],
      }),
    ).toThrow();
  });

  it("enforces every public control collection bound and non-negative revision/count", () => {
    // Mutation caught: accepting an unbounded or negative server-authoritative control value.
    const controlSnapshot = snapshot.control;
    const actorControl = controlSnapshot.actors[0];
    const operation = {
      rootActorId: actor.id,
      state: "partial" as const,
      residualCount: 1,
      message: "One descendant could not be stopped.",
      operationRevision: 2,
    };
    const controlDelta = {
      scopeId: snapshot.scopeId,
      previousRevision: 6,
      revision: 7,
      changes: [{ kind: "actor-upserted" as const, actor: actorControl }],
    };
    const decodeControlSnapshot = Schema.decodeUnknownSync(
      Reflect.get(ActivityModule, "ActivityControlSnapshot") as Schema.Codec<
        unknown,
        unknown,
        never,
        never
      >,
    );
    const decodeControlDelta = Schema.decodeUnknownSync(
      Reflect.get(ActivityModule, "ActivityControlDelta") as Schema.Codec<
        unknown,
        unknown,
        never,
        never
      >,
    );

    expect(() =>
      decodeControlSnapshot({
        ...controlSnapshot,
        actors: Array.from({ length: 201 }, (_, index) => ({
          actorId: `actor:bounded-${index}`,
          state: "available",
          controlRevision: index,
          activeDescendantCount: 0,
        })),
      }),
    ).toThrow();
    expect(() =>
      decodeControlDelta({
        ...controlDelta,
        changes: Array.from({ length: 257 }, () => controlDelta.changes[0]),
      }),
    ).toThrow();
    expect(() =>
      decodeControlSnapshot({
        ...controlSnapshot,
        actors: [{ ...actorControl, controlRevision: -1 }],
      }),
    ).toThrow();
    expect(() =>
      decodeControlSnapshot({
        ...controlSnapshot,
        actors: [{ ...actorControl, activeDescendantCount: -1 }],
      }),
    ).toThrow();
    expect(() =>
      decodeControlSnapshot({
        ...controlSnapshot,
        operations: [{ ...operation, residualCount: -1 }],
      }),
    ).toThrow();
    expect(() =>
      decodeControlSnapshot({
        ...controlSnapshot,
        operations: [{ ...operation, operationRevision: -1 }],
      }),
    ).toThrow();
    expect(() => decodeControlSnapshot({ ...controlSnapshot, revision: -1 })).toThrow();
    expect(() => decodeControlDelta({ ...controlDelta, previousRevision: -1 })).toThrow();
    expect(() => decodeControlDelta({ ...controlDelta, revision: -1 })).toThrow();
  });

  it("rejects invalid snapshot revisions", () => {
    expect(() =>
      decodeActivitySnapshot({
        ...snapshot,
        revision: -1,
      }),
    ).toThrow();
  });

  it("rejects oversized actor labels", () => {
    expect(() =>
      decodeActivitySnapshot({
        ...snapshot,
        actors: [{ ...actor, name: "x".repeat(257) }],
      }),
    ).toThrow();
  });

  it("rejects oversized detail entry pages", () => {
    expect(() =>
      decodeActivityDetailPage({
        record: actor,
        entries: Array.from({ length: 201 }, (_, index) => ({
          id: `entry:${index}`,
          ownerKind: "actor",
          ownerId: actor.id,
          kind: "commentary",
          title: "Update",
          detail: "Working",
          tone: "info",
          createdAt: "2026-07-22T12:00:02Z",
        })),
        nextCursor: null,
      }),
    ).toThrow();
  });

  it("rejects oversized detail cursors", () => {
    expect(() =>
      decodeActivityDetailPage({
        record: actor,
        entries: [],
        nextCursor: "x".repeat(513),
      }),
    ).toThrow();
  });

  it("rejects empty delta batches", () => {
    expect(() => decodeActivityDelta({ ...delta, changes: [] })).toThrow();
  });

  it("rejects oversized delta batches", () => {
    expect(() =>
      decodeActivityDelta({
        ...delta,
        changes: Array.from({ length: 257 }, () => delta.changes[0]),
      }),
    ).toThrow();
  });

  it("rejects roster page limits outside 1 through 200", () => {
    const input = {
      scope: snapshot.scope,
      scopeId: snapshot.scopeId,
      section: "subagents",
      bucket: "active",
    } as const;

    expect(() => decodeActivityListRosterInput({ ...input, limit: 0 })).toThrow();
    expect(() => decodeActivityListRosterInput({ ...input, limit: 201 })).toThrow();
  });

  it("rejects detail page limits outside 1 through 200", () => {
    const input = {
      scope: snapshot.scope,
      scopeId: snapshot.scopeId,
      recordKind: "actor",
      recordId: actor.id,
    } as const;

    expect(() => decodeActivityListDetailInput({ ...input, limit: 0 })).toThrow();
    expect(() => decodeActivityListDetailInput({ ...input, limit: 201 })).toThrow();
  });

  it("requires an authoritative root descriptor for every activity paging request", () => {
    expect(() =>
      decodeActivityListRosterInput({
        scopeId: snapshot.scopeId,
        section: "subagents",
        bucket: "active",
      }),
    ).toThrow();
    expect(() =>
      decodeActivityListDetailInput({
        scopeId: snapshot.scopeId,
        recordKind: "actor",
        recordId: actor.id,
      }),
    ).toThrow();
  });

  it("rejects oversized thread IDs", () => {
    expect(() =>
      decodeActivityScopeRef({
        _tag: "thread",
        threadId: "x".repeat(257),
      }),
    ).toThrow();
  });

  it("rejects oversized timestamps", () => {
    expect(() =>
      decodeActivitySnapshot({
        ...snapshot,
        updatedAt: `2026-07-22T12:00:00.${"1".repeat(44)}Z`,
      }),
    ).toThrow();
  });

  it("rejects malformed timestamps", () => {
    expect(() =>
      decodeActivitySnapshot({
        ...snapshot,
        updatedAt: "not-a-timestamp",
      }),
    ).toThrow();
  });

  it("rejects parseable non-ISO timestamps", () => {
    expect(() =>
      decodeActivitySnapshot({
        ...snapshot,
        updatedAt: "July 22, 2026 12:00:00",
      }),
    ).toThrow();
  });

  it("rejects ISO-shaped timestamps with invalid calendar dates", () => {
    expect(() =>
      decodeActivitySnapshot({
        ...snapshot,
        updatedAt: "2026-02-30T12:00:00Z",
      }),
    ).toThrow();
  });
});
