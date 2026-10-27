import type {
  ActivityActorSummary,
  ActivityChange,
  ActivityActorControl,
  ActivityCancellationOperationSummary,
  ActivityControlChange,
  ActivityControlDelta,
  ActivityControlSnapshot,
  ActivityDelta,
  ActivityEntry,
  ActivityLifecycle,
  ActivityRecordId,
  ActivitySectionHealth,
  ActivitySnapshot,
  ActivityWorkItemSummary,
} from "@coddy/contracts";
import { ACTIVITY_PAGE_MAX_LENGTH } from "@coddy/contracts";
import * as Arr from "effect/Array";
import * as DateTime from "effect/DateTime";
import * as Option from "effect/Option";
import * as Order from "effect/Order";

export const ACTIVITY_RECENT_ENTRY_LIMIT = 200;
export const ACTIVITY_RECENT_OWNER_LIMIT = 200;

export interface EnvironmentActivityState {
  readonly snapshot: Option.Option<ActivitySnapshot>;
  readonly status: "empty" | "synchronizing" | "live" | "stale";
  readonly error: Option.Option<string>;
  readonly recentEntries: ReadonlyMap<ActivityRecordId, ReadonlyArray<ActivityEntry>>;
}

export type ActivityDeltaResult =
  | { readonly kind: "applied"; readonly snapshot: ActivitySnapshot }
  | { readonly kind: "duplicate" }
  | { readonly kind: "gap" };

export type ActivityControlDeltaResult = ActivityDeltaResult;

export type EnvironmentActivityDeltaResult =
  | { readonly kind: "applied"; readonly state: EnvironmentActivityState }
  | { readonly kind: "duplicate"; readonly state: EnvironmentActivityState }
  | { readonly kind: "gap"; readonly state: EnvironmentActivityState };

type ActivitySummary = ActivityActorSummary | ActivityWorkItemSummary;

export function activityActorControl(
  snapshot: ActivitySnapshot,
  actorId: ActivityRecordId,
): ActivityActorControl | null {
  const matches = Arr.filter(snapshot.control.actors, (control) => control.actorId === actorId);
  return matches.length === 1 ? (matches[0] ?? null) : null;
}

export function activityCancellationOperation(
  snapshot: ActivitySnapshot,
  rootActorId: ActivityRecordId,
): ActivityCancellationOperationSummary | null {
  const matches = Arr.filter(
    snapshot.control.operations,
    (operation) => operation.rootActorId === rootActorId,
  );
  return matches.length === 1 ? (matches[0] ?? null) : null;
}

function absurd(value: never): never {
  throw new Error(`Unexpected activity variant: ${String(value)}`);
}

function isTerminal(status: ActivityLifecycle): boolean {
  switch (status) {
    case "completed":
    case "failed":
    case "cancelled":
    case "interrupted":
      return true;
    case "starting":
    case "running":
    case "waiting":
    case "unknown":
      return false;
  }
}

const activitySummaryOrder = Order.make<ActivitySummary>((left, right) => {
  const leftTerminal = isTerminal(left.status);
  const rightTerminal = isTerminal(right.status);
  if (leftTerminal !== rightTerminal) {
    return leftTerminal ? (1 as const) : (-1 as const);
  }

  const leftTime = left.updatedAt;
  const rightTime = right.updatedAt;
  const timeOrder = Order.Number(
    DateTime.toEpochMillis(DateTime.makeUnsafe(rightTime)),
    DateTime.toEpochMillis(DateTime.makeUnsafe(leftTime)),
  );
  if (timeOrder !== 0) {
    return timeOrder;
  }
  return Order.String(right.id, left.id);
});

function timestampMillis(value: string): number | undefined {
  const parsed = DateTime.make(value);
  return Option.isSome(parsed) ? DateTime.toEpochMillis(parsed.value) : undefined;
}

function hasValidChronology(summary: ActivitySummary): boolean {
  const startedAt = timestampMillis(summary.startedAt);
  const updatedAt = timestampMillis(summary.updatedAt);
  if (startedAt === undefined || updatedAt === undefined || startedAt > updatedAt) {
    return false;
  }

  if (summary.terminalAt === null) {
    return !isTerminal(summary.status);
  }

  const terminalAt = timestampMillis(summary.terminalAt);
  return (
    isTerminal(summary.status) &&
    terminalAt !== undefined &&
    startedAt <= terminalAt &&
    terminalAt <= updatedAt
  );
}

function resolveSummaryUpdate<T extends ActivitySummary>(current: T, incoming: T): T {
  if (!hasValidChronology(incoming)) {
    return current;
  }
  if (isTerminal(current.status) && !isTerminal(incoming.status)) {
    return current;
  }

  const currentUpdatedAt = timestampMillis(current.updatedAt);
  const incomingUpdatedAt = timestampMillis(incoming.updatedAt);
  if (
    currentUpdatedAt === undefined ||
    incomingUpdatedAt === undefined ||
    incomingUpdatedAt < currentUpdatedAt
  ) {
    return current;
  }

  return incoming;
}

interface SummaryUpsert<T extends ActivitySummary> {
  readonly summaries: ReadonlyArray<T>;
  readonly overflow: boolean;
}

function upsertSummary<T extends ActivitySummary>(
  summaries: ReadonlyArray<T>,
  incoming: T,
): SummaryUpsert<T> {
  if (!hasValidChronology(incoming)) {
    return { summaries, overflow: false };
  }

  const index = Arr.findFirstIndex(summaries, (summary) => summary.id === incoming.id);
  const unsorted = Option.match(index, {
    onNone: () => Arr.append(summaries, incoming),
    onSome: (position) => {
      const current = summaries[position];
      if (current === undefined) {
        return Arr.append(summaries, incoming);
      }
      const resolved = resolveSummaryUpdate(current, incoming);
      if (resolved === current) {
        return summaries;
      }
      return Option.getOrElse(Arr.replace(summaries, position, resolved), () =>
        Arr.append(summaries, incoming),
      );
    },
  });
  if (unsorted === summaries) {
    return { summaries, overflow: false };
  }
  const ordered = Arr.sort(unsorted, activitySummaryOrder);
  const overflow = ordered.length > ACTIVITY_PAGE_MAX_LENGTH;
  return {
    summaries: Arr.take(ordered, ACTIVITY_PAGE_MAX_LENGTH),
    overflow,
  };
}

function removeSummary<T extends ActivitySummary>(
  summaries: ReadonlyArray<T>,
  id: ActivityRecordId,
): ReadonlyArray<T> {
  if (!Arr.some(summaries, (summary) => summary.id === id)) {
    return summaries;
  }
  return Arr.filter(summaries, (summary) => summary.id !== id);
}

function healthEqual(left: ActivitySectionHealth, right: ActivitySectionHealth): boolean {
  return (
    left.state === right.state &&
    left.message === right.message &&
    left.retryable === right.retryable
  );
}

function applyChange(snapshot: ActivitySnapshot, change: ActivityChange): ActivitySnapshot {
  switch (change.kind) {
    case "scope-updated":
      return {
        ...snapshot,
        capabilities: change.capabilities,
        observationState: change.observationState,
        sections: {
          subagents: healthEqual(snapshot.sections.subagents, change.sections.subagents)
            ? snapshot.sections.subagents
            : change.sections.subagents,
          backgroundTasks: healthEqual(
            snapshot.sections.backgroundTasks,
            change.sections.backgroundTasks,
          )
            ? snapshot.sections.backgroundTasks
            : change.sections.backgroundTasks,
        },
        counts: change.counts,
      };
    case "actor-upserted": {
      const result = upsertSummary(snapshot.actors, change.actor);
      return {
        ...snapshot,
        actors: result.summaries,
        actorsHasMore: snapshot.actorsHasMore || result.overflow,
      };
    }
    case "actor-removed":
      return {
        ...snapshot,
        actors: removeSummary(snapshot.actors, change.actorId),
      };
    case "work-item-upserted": {
      const result = upsertSummary(snapshot.workItems, change.workItem);
      return {
        ...snapshot,
        workItems: result.summaries,
        workItemsHasMore: snapshot.workItemsHasMore || result.overflow,
      };
    }
    case "work-item-removed":
      return {
        ...snapshot,
        workItems: removeSummary(snapshot.workItems, change.workItemId),
      };
    case "entry-appended":
    case "entries-replaced":
      return snapshot;
    default:
      return absurd(change);
  }
}

function visibleActiveSummaryBecomesTerminal<T extends ActivitySummary>(
  summaries: ReadonlyArray<T>,
  hasMore: boolean,
  incoming: T,
): boolean {
  if (!hasMore) {
    return false;
  }
  const current = Arr.findFirst(summaries, (summary) => summary.id === incoming.id);
  if (Option.isNone(current) || isTerminal(current.value.status)) {
    return false;
  }
  return isTerminal(resolveSummaryUpdate(current.value, incoming).status);
}

function requiresSnapshotReconciliation(
  snapshot: ActivitySnapshot,
  changes: ReadonlyArray<ActivityChange>,
): boolean {
  for (const change of changes) {
    switch (change.kind) {
      case "actor-removed":
        if (
          snapshot.actorsHasMore &&
          Arr.some(snapshot.actors, (actor) => actor.id === change.actorId)
        ) {
          return true;
        }
        break;
      case "work-item-removed":
        if (
          snapshot.workItemsHasMore &&
          Arr.some(snapshot.workItems, (workItem) => workItem.id === change.workItemId)
        ) {
          return true;
        }
        break;
      case "actor-upserted":
        if (
          visibleActiveSummaryBecomesTerminal(snapshot.actors, snapshot.actorsHasMore, change.actor)
        ) {
          return true;
        }
        break;
      case "work-item-upserted":
        if (
          visibleActiveSummaryBecomesTerminal(
            snapshot.workItems,
            snapshot.workItemsHasMore,
            change.workItem,
          )
        ) {
          return true;
        }
        break;
      case "scope-updated":
      case "entry-appended":
      case "entries-replaced":
        break;
      default:
        absurd(change);
    }
  }
  return false;
}

export function applyActivityDelta(
  snapshot: ActivitySnapshot,
  delta: ActivityDelta,
): ActivityDeltaResult {
  if (delta.scopeId !== snapshot.scopeId) {
    return { kind: "gap" };
  }
  if (delta.revision <= snapshot.revision) {
    return { kind: "duplicate" };
  }
  if (delta.previousRevision !== snapshot.revision) {
    return { kind: "gap" };
  }
  if (requiresSnapshotReconciliation(snapshot, delta.changes)) {
    return { kind: "gap" };
  }

  const next = Arr.reduce(delta.changes, snapshot, applyChange);
  return {
    kind: "applied",
    snapshot: {
      ...next,
      revision: delta.revision,
      updatedAt: delta.updatedAt,
    },
  };
}

function upsertById<T>(
  values: ReadonlyArray<T>,
  incoming: T,
  id: (value: T) => ActivityRecordId,
): ReadonlyArray<T> {
  const incomingId = id(incoming);
  const index = Arr.findFirstIndex(values, (value) => id(value) === incomingId);
  if (Option.isNone(index)) {
    return Arr.append(values, incoming);
  }
  return Option.getOrElse(Arr.replace(values, index.value, incoming), () => values);
}

function removeById<T>(
  values: ReadonlyArray<T>,
  removedId: ActivityRecordId,
  id: (value: T) => ActivityRecordId,
): ReadonlyArray<T> {
  return Arr.some(values, (value) => id(value) === removedId)
    ? Arr.filter(values, (value) => id(value) !== removedId)
    : values;
}

function applyControlChange(
  control: ActivityControlSnapshot,
  change: ActivityControlChange,
): ActivityControlSnapshot {
  switch (change.kind) {
    case "actor-upserted":
      return {
        ...control,
        actors: upsertById(control.actors, change.actor, (actor) => actor.actorId),
      };
    case "actor-removed":
      return {
        ...control,
        actors: removeById(control.actors, change.actorId, (actor) => actor.actorId),
      };
    case "operation-upserted":
      return {
        ...control,
        operations: upsertById(
          control.operations,
          change.operation,
          (operation) => operation.rootActorId,
        ),
      };
    case "operation-removed":
      return {
        ...control,
        operations: removeById(
          control.operations,
          change.rootActorId,
          (operation) => operation.rootActorId,
        ),
      };
    default:
      return absurd(change);
  }
}

export function applyActivityControlSnapshot(
  snapshot: ActivitySnapshot,
  incoming: ActivityControlSnapshot,
): ActivityControlDeltaResult {
  if (incoming.scopeId !== snapshot.scopeId) {
    return { kind: "gap" };
  }
  if (incoming.revision <= snapshot.control.revision) {
    return { kind: "duplicate" };
  }
  return {
    kind: "applied",
    snapshot: {
      ...snapshot,
      control: incoming,
    },
  };
}

export function applyActivityControlDelta(
  snapshot: ActivitySnapshot,
  delta: ActivityControlDelta,
): ActivityControlDeltaResult {
  if (delta.scopeId !== snapshot.scopeId) {
    return { kind: "gap" };
  }
  if (delta.revision <= snapshot.control.revision) {
    return { kind: "duplicate" };
  }
  if (delta.previousRevision !== snapshot.control.revision) {
    return { kind: "gap" };
  }

  const nextControl = Arr.reduce(delta.changes, snapshot.control, applyControlChange);
  if (
    nextControl.actors.length > ACTIVITY_PAGE_MAX_LENGTH ||
    nextControl.operations.length > ACTIVITY_PAGE_MAX_LENGTH
  ) {
    return { kind: "gap" };
  }
  return {
    kind: "applied",
    snapshot: {
      ...snapshot,
      control: {
        ...nextControl,
        revision: delta.revision,
      },
    },
  };
}

function appendRecentEntries(
  current: ReadonlyMap<ActivityRecordId, ReadonlyArray<ActivityEntry>>,
  changes: ReadonlyArray<ActivityChange>,
): ReadonlyMap<ActivityRecordId, ReadonlyArray<ActivityEntry>> {
  let next: Map<ActivityRecordId, ReadonlyArray<ActivityEntry>> | undefined;

  for (const change of changes) {
    switch (change.kind) {
      case "entry-appended": {
        const entries = (next ?? current).get(change.entry.ownerId) ?? [];
        if (Arr.some(entries, (entry) => entry.id === change.entry.id)) {
          break;
        }
        next ??= new Map(current);
        next.set(
          change.entry.ownerId,
          Arr.takeRight(Arr.append(entries, change.entry), ACTIVITY_RECENT_ENTRY_LIMIT),
        );
        break;
      }
      case "entries-replaced": {
        const entries = next ?? current;
        if (entries.has(change.ownerId)) {
          next ??= new Map(current);
          next.delete(change.ownerId);
        }
        break;
      }
      case "actor-removed": {
        const entries = next ?? current;
        if (entries.has(change.actorId)) {
          next ??= new Map(current);
          next.delete(change.actorId);
        }
        break;
      }
      case "work-item-removed": {
        const entries = next ?? current;
        if (entries.has(change.workItemId)) {
          next ??= new Map(current);
          next.delete(change.workItemId);
        }
        break;
      }
      case "scope-updated":
      case "actor-upserted":
      case "work-item-upserted":
        break;
      default:
        absurd(change);
    }
  }

  if (next === undefined || next.size <= ACTIVITY_RECENT_OWNER_LIMIT) {
    return next ?? current;
  }

  const newestEntryTimestamp = (entries: ReadonlyArray<ActivityEntry>): number =>
    Arr.reduce(entries, Number.NEGATIVE_INFINITY, (newest, entry) =>
      Math.max(newest, timestampMillis(entry.createdAt) ?? Number.NEGATIVE_INFINITY),
    );
  const ownersByRecency = Array.from(next.entries()).sort(
    ([leftId, leftEntries], [rightId, rightEntries]) => {
      const recency = newestEntryTimestamp(rightEntries) - newestEntryTimestamp(leftEntries);
      return recency === 0 ? Order.String(leftId, rightId) : recency;
    },
  );
  return new Map(ownersByRecency.slice(0, ACTIVITY_RECENT_OWNER_LIMIT));
}

export function applyEnvironmentActivityDelta(
  state: EnvironmentActivityState,
  delta: ActivityDelta,
): EnvironmentActivityDeltaResult {
  if (Option.isNone(state.snapshot)) {
    return {
      kind: "gap",
      state:
        state.status === "synchronizing"
          ? state
          : {
              ...state,
              status: "synchronizing",
            },
    };
  }

  const result = applyActivityDelta(state.snapshot.value, delta);
  switch (result.kind) {
    case "duplicate":
      return { kind: "duplicate", state };
    case "gap":
      return {
        kind: "gap",
        state:
          state.status === "stale"
            ? state
            : {
                ...state,
                status: "stale",
              },
      };
    case "applied":
      return {
        kind: "applied",
        state: {
          snapshot: Option.some(result.snapshot),
          status: "live",
          error: Option.none(),
          recentEntries: appendRecentEntries(state.recentEntries, delta.changes),
        },
      };
  }
}

export function applyEnvironmentActivityControlDelta(
  state: EnvironmentActivityState,
  delta: ActivityControlDelta,
): EnvironmentActivityDeltaResult {
  if (Option.isNone(state.snapshot)) {
    return {
      kind: "gap",
      state:
        state.status === "synchronizing"
          ? state
          : {
              ...state,
              status: "synchronizing",
            },
    };
  }

  const result = applyActivityControlDelta(state.snapshot.value, delta);
  switch (result.kind) {
    case "duplicate":
      return { kind: "duplicate", state };
    case "gap":
      return {
        kind: "gap",
        state:
          state.status === "stale"
            ? state
            : {
                ...state,
                status: "stale",
              },
      };
    case "applied":
      return {
        kind: "applied",
        state: {
          ...state,
          snapshot: Option.some(result.snapshot),
          status: "live",
          error: Option.none(),
        },
      };
  }
}
