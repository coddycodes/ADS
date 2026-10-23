import * as DateTime from "effect/DateTime";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";

import { NonNegativeInt, PositiveInt, ThreadId, TrimmedNonEmptyString } from "./baseSchemas.ts";
import { ProviderDriverKind, ProviderInstanceId } from "./providerInstance.ts";

export const ACTIVITY_ID_MAX_LENGTH = 256;
export const ACTIVITY_LABEL_MAX_LENGTH = 256;
export const ACTIVITY_SUMMARY_MAX_LENGTH = 2_048;
export const ACTIVITY_DETAIL_MAX_LENGTH = 16_384;
export const ACTIVITY_CURSOR_MAX_LENGTH = 512;
export const ACTIVITY_PAGE_MAX_LENGTH = 200;
const ACTIVITY_TIMESTAMP_MAX_LENGTH = 64;

const ACTIVITY_TIMESTAMP_PATTERN =
  /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/;
const ActivityId = TrimmedNonEmptyString.check(Schema.isMaxLength(ACTIVITY_ID_MAX_LENGTH));
const ActivityThreadId = ThreadId.check(Schema.isMaxLength(ACTIVITY_ID_MAX_LENGTH));
const ActivityLabel = TrimmedNonEmptyString.check(Schema.isMaxLength(ACTIVITY_LABEL_MAX_LENGTH));
const ActivitySummaryText = Schema.String.check(Schema.isMaxLength(ACTIVITY_SUMMARY_MAX_LENGTH));
const ActivityDetailText = Schema.String.check(Schema.isMaxLength(ACTIVITY_DETAIL_MAX_LENGTH));
const ActivityTimestamp = Schema.String.check(
  Schema.isMaxLength(ACTIVITY_TIMESTAMP_MAX_LENGTH),
  Schema.isPattern(ACTIVITY_TIMESTAMP_PATTERN),
  Schema.makeFilter((value) => Option.isSome(DateTime.make(value)), {
    expected: "an ISO 8601 date-time string",
  }),
  Schema.makeFilter(
    (value) => {
      const calendarDate = value.slice(0, 10);
      return Option.match(DateTime.make(calendarDate), {
        onNone: () => false,
        onSome: (parsed) => DateTime.formatIso(parsed).slice(0, 10) === calendarDate,
      });
    },
    { expected: "an ISO 8601 date-time string with a valid calendar date" },
  ),
);
export const ActivityScopeId = ActivityId.pipe(Schema.brand("ActivityScopeId"));
export type ActivityScopeId = typeof ActivityScopeId.Type;
export const ActivityRecordId = ActivityId.pipe(Schema.brand("ActivityRecordId"));
export type ActivityRecordId = typeof ActivityRecordId.Type;
export const ActivityEntryId = ActivityId.pipe(Schema.brand("ActivityEntryId"));
export type ActivityEntryId = typeof ActivityEntryId.Type;

export const ActivitySection = Schema.Literals(["subagents", "backgroundTasks"]);
export type ActivitySection = typeof ActivitySection.Type;
export const ActivityRecordKind = Schema.Literals(["actor", "workItem"]);
export type ActivityRecordKind = typeof ActivityRecordKind.Type;
export const ActivityLifecycle = Schema.Literals([
  "starting",
  "running",
  "waiting",
  "completed",
  "failed",
  "cancelled",
  "interrupted",
  "unknown",
]);
export type ActivityLifecycle = typeof ActivityLifecycle.Type;
export const ActivityObservationState = Schema.Literals(["live", "reconnecting", "stale", "error"]);
export type ActivityObservationState = typeof ActivityObservationState.Type;

export const ActivitySectionObservationState = Schema.Literals([
  "unsupported",
  "live",
  "stale",
  "error",
]);
export type ActivitySectionObservationState = typeof ActivitySectionObservationState.Type;

export const ActivitySectionHealth = Schema.Struct({
  state: ActivitySectionObservationState,
  message: Schema.NullOr(ActivitySummaryText),
  retryable: Schema.Boolean,
});
export type ActivitySectionHealth = typeof ActivitySectionHealth.Type;

export const ActivitySectionHealthMap = Schema.Struct({
  subagents: ActivitySectionHealth,
  backgroundTasks: ActivitySectionHealth,
});
export type ActivitySectionHealthMap = typeof ActivitySectionHealthMap.Type;

export const ActivityScopeRef = Schema.Union([
  Schema.TaggedStruct("thread", { threadId: ActivityThreadId }),
  Schema.TaggedStruct("terminal", {
    threadId: ActivityThreadId,
    terminalId: ActivityId,
  }),
]);
export type ActivityScopeRef = typeof ActivityScopeRef.Type;

export const ActivityCapabilities = Schema.Struct({
  actors: Schema.Boolean,
  attributedActivity: Schema.Boolean,
  backgroundWork: Schema.Boolean,
  historyRecovery: Schema.Literals(["full", "bounded", "none"]),
  terminalObservation: Schema.Boolean,
  targetedActorCancellation: Schema.Boolean,
});
export type ActivityCapabilities = typeof ActivityCapabilities.Type;

export const NO_ACTIVITY_CAPABILITIES = {
  actors: false,
  attributedActivity: false,
  backgroundWork: false,
  historyRecovery: "none",
  terminalObservation: false,
  targetedActorCancellation: false,
} as const satisfies ActivityCapabilities;

const ActivityRecordBase = {
  id: ActivityRecordId,
  name: ActivityLabel,
  status: ActivityLifecycle,
  summary: Schema.NullOr(ActivitySummaryText),
  startedAt: ActivityTimestamp,
  updatedAt: ActivityTimestamp,
  terminalAt: Schema.NullOr(ActivityTimestamp),
};

export const ActivityActorSummary = Schema.TaggedStruct("actor", {
  ...ActivityRecordBase,
  parentActorId: Schema.NullOr(ActivityRecordId),
  role: Schema.NullOr(ActivityLabel),
  providerType: Schema.NullOr(ActivityLabel),
});
export type ActivityActorSummary = typeof ActivityActorSummary.Type;

export const ActivityWorkItemSummary = Schema.TaggedStruct("workItem", {
  ...ActivityRecordBase,
  ownerActorId: Schema.NullOr(ActivityRecordId),
  workKind: ActivityLabel,
  command: Schema.NullOr(ActivityDetailText),
  cwd: Schema.NullOr(ActivityDetailText),
});
export type ActivityWorkItemSummary = typeof ActivityWorkItemSummary.Type;

export const ActivityRecordSummary = Schema.Union([ActivityActorSummary, ActivityWorkItemSummary]);
export type ActivityRecordSummary = typeof ActivityRecordSummary.Type;

export const ActivityEntry = Schema.Struct({
  id: ActivityEntryId,
  ownerKind: ActivityRecordKind,
  ownerId: ActivityRecordId,
  kind: Schema.Literals([
    "commentary",
    "tool",
    "command",
    "result",
    "error",
    "state",
    "completion",
  ]),
  title: ActivityLabel,
  detail: Schema.NullOr(ActivityDetailText),
  tone: Schema.Literals(["info", "tool", "success", "warning", "error"]),
  createdAt: ActivityTimestamp,
});
export type ActivityEntry = typeof ActivityEntry.Type;

const ActivityCounts = Schema.Struct({ active: NonNegativeInt, done: NonNegativeInt });
export const ActivitySummaryCounts = Schema.Struct({
  subagents: ActivityCounts,
  backgroundTasks: ActivityCounts,
});
export type ActivitySummaryCounts = typeof ActivitySummaryCounts.Type;

export const ActivityActorControl = Schema.Struct({
  actorId: ActivityRecordId,
  state: Schema.Literals(["unsupported", "available", "requested"]),
  controlRevision: NonNegativeInt,
  activeDescendantCount: NonNegativeInt,
});
export type ActivityActorControl = typeof ActivityActorControl.Type;

export const ActivityCancellationOperationSummary = Schema.Struct({
  rootActorId: ActivityRecordId,
  state: Schema.Literals(["requested", "partial"]),
  residualCount: NonNegativeInt,
  message: Schema.NullOr(ActivitySummaryText),
  operationRevision: NonNegativeInt,
});
export type ActivityCancellationOperationSummary = typeof ActivityCancellationOperationSummary.Type;

export const ActivityControlSnapshot = Schema.Struct({
  scopeId: ActivityScopeId,
  revision: NonNegativeInt,
  actors: Schema.Array(ActivityActorControl).check(Schema.isMaxLength(ACTIVITY_PAGE_MAX_LENGTH)),
  operations: Schema.Array(ActivityCancellationOperationSummary).check(
    Schema.isMaxLength(ACTIVITY_PAGE_MAX_LENGTH),
  ),
});
export type ActivityControlSnapshot = typeof ActivityControlSnapshot.Type;

export const ActivitySnapshot = Schema.Struct({
  protocolVersion: Schema.Literal(2),
  scopeId: ActivityScopeId,
  scope: ActivityScopeRef,
  revision: NonNegativeInt,
  provider: ProviderDriverKind,
  providerInstanceId: Schema.NullOr(ProviderInstanceId),
  capabilities: ActivityCapabilities,
  observationState: ActivityObservationState,
  sections: ActivitySectionHealthMap,
  counts: ActivitySummaryCounts,
  actors: Schema.Array(ActivityActorSummary).check(Schema.isMaxLength(ACTIVITY_PAGE_MAX_LENGTH)),
  workItems: Schema.Array(ActivityWorkItemSummary).check(
    Schema.isMaxLength(ACTIVITY_PAGE_MAX_LENGTH),
  ),
  actorsHasMore: Schema.Boolean,
  workItemsHasMore: Schema.Boolean,
  control: ActivityControlSnapshot,
  updatedAt: ActivityTimestamp,
});
export type ActivitySnapshot = typeof ActivitySnapshot.Type;

export const ActivityChange = Schema.Union([
  Schema.Struct({
    kind: Schema.Literal("scope-updated"),
    capabilities: ActivityCapabilities,
    observationState: ActivityObservationState,
    sections: ActivitySectionHealthMap,
    counts: ActivitySummaryCounts,
  }),
  Schema.Struct({ kind: Schema.Literal("actor-upserted"), actor: ActivityActorSummary }),
  Schema.Struct({ kind: Schema.Literal("actor-removed"), actorId: ActivityRecordId }),
  Schema.Struct({
    kind: Schema.Literal("work-item-upserted"),
    workItem: ActivityWorkItemSummary,
  }),
  Schema.Struct({
    kind: Schema.Literal("work-item-removed"),
    workItemId: ActivityRecordId,
  }),
  Schema.Struct({ kind: Schema.Literal("entry-appended"), entry: ActivityEntry }),
  Schema.Struct({
    kind: Schema.Literal("entries-replaced"),
    ownerKind: ActivityRecordKind,
    ownerId: ActivityRecordId,
  }),
]);
export type ActivityChange = typeof ActivityChange.Type;

export const ActivityDelta = Schema.Struct({
  scopeId: ActivityScopeId,
  previousRevision: NonNegativeInt,
  revision: PositiveInt,
  changes: Schema.Array(ActivityChange).check(Schema.isMinLength(1), Schema.isMaxLength(256)),
  updatedAt: ActivityTimestamp,
});
export type ActivityDelta = typeof ActivityDelta.Type;

export const ActivityControlChange = Schema.Union([
  Schema.Struct({ kind: Schema.Literal("actor-upserted"), actor: ActivityActorControl }),
  Schema.Struct({ kind: Schema.Literal("actor-removed"), actorId: ActivityRecordId }),
  Schema.Struct({
    kind: Schema.Literal("operation-upserted"),
    operation: ActivityCancellationOperationSummary,
  }),
  Schema.Struct({ kind: Schema.Literal("operation-removed"), rootActorId: ActivityRecordId }),
]);
export type ActivityControlChange = typeof ActivityControlChange.Type;

export const ActivityControlDelta = Schema.Struct({
  scopeId: ActivityScopeId,
  previousRevision: NonNegativeInt,
  revision: PositiveInt,
  changes: Schema.Array(ActivityControlChange).check(
    Schema.isMinLength(1),
    Schema.isMaxLength(256),
  ),
});
export type ActivityControlDelta = typeof ActivityControlDelta.Type;

export const ActivityStreamItem = Schema.Union([
  Schema.Struct({ kind: Schema.Literal("snapshot"), snapshot: ActivitySnapshot }),
  Schema.Struct({ kind: Schema.Literal("delta"), delta: ActivityDelta }),
  Schema.Struct({ kind: Schema.Literal("control-snapshot"), control: ActivityControlSnapshot }),
  Schema.Struct({ kind: Schema.Literal("control-delta"), delta: ActivityControlDelta }),
]);
export type ActivityStreamItem = typeof ActivityStreamItem.Type;

const ActivityPageCursor = TrimmedNonEmptyString.check(
  Schema.isMaxLength(ACTIVITY_CURSOR_MAX_LENGTH),
);
const ActivityPageLimit = Schema.optional(
  PositiveInt.check(Schema.isLessThanOrEqualTo(ACTIVITY_PAGE_MAX_LENGTH)),
);
export const ActivityGetSnapshotInput = ActivityScopeRef;
export const ActivityListRosterInput = Schema.Struct({
  scope: ActivityScopeRef,
  scopeId: ActivityScopeId,
  section: ActivitySection,
  bucket: Schema.Literals(["active", "done"]),
  cursor: Schema.optional(ActivityPageCursor),
  limit: ActivityPageLimit,
});
export const ActivityRosterPage = Schema.Struct({
  records: Schema.Array(ActivityRecordSummary).check(Schema.isMaxLength(ACTIVITY_PAGE_MAX_LENGTH)),
  actorControls: Schema.Array(ActivityActorControl).check(
    Schema.isMaxLength(ACTIVITY_PAGE_MAX_LENGTH),
  ),
  nextCursor: Schema.NullOr(ActivityPageCursor),
});
export const ActivityListDetailInput = Schema.Struct({
  scope: ActivityScopeRef,
  scopeId: ActivityScopeId,
  recordKind: ActivityRecordKind,
  recordId: ActivityRecordId,
  cursor: Schema.optional(ActivityPageCursor),
  limit: ActivityPageLimit,
});
export const ActivityDetailPage = Schema.Struct({
  record: ActivityRecordSummary,
  actorControl: Schema.NullOr(ActivityActorControl),
  entries: Schema.Array(ActivityEntry).check(Schema.isMaxLength(ACTIVITY_PAGE_MAX_LENGTH)),
  nextCursor: Schema.NullOr(ActivityPageCursor),
});
export type ActivityDetailPage = typeof ActivityDetailPage.Type;

export const ActivityCancelSubtreeInput = Schema.Struct({
  scope: Schema.TaggedStruct("thread", { threadId: ActivityThreadId }),
  scopeId: ActivityScopeId,
  actorId: ActivityRecordId,
  expectedControlRevision: NonNegativeInt,
});
export type ActivityCancelSubtreeInput = typeof ActivityCancelSubtreeInput.Type;

export const ActivityRetrySubtreeCancellationInput = Schema.Struct({
  scope: Schema.TaggedStruct("thread", { threadId: ActivityThreadId }),
  scopeId: ActivityScopeId,
  rootActorId: ActivityRecordId,
  expectedOperationRevision: NonNegativeInt,
});
export type ActivityRetrySubtreeCancellationInput =
  typeof ActivityRetrySubtreeCancellationInput.Type;

export const ActivitySubtreeCancellationResult = Schema.Struct({
  disposition: Schema.Literals(["accepted", "inProgress", "alreadyTerminal"]),
  rootActorId: ActivityRecordId,
  operationRevision: Schema.NullOr(NonNegativeInt),
});
export type ActivitySubtreeCancellationResult = typeof ActivitySubtreeCancellationResult.Type;

export class ActivityError extends Schema.TaggedError<ActivityError>()("ActivityError", {
  message: ActivitySummaryText,
  reason: Schema.Literals([
    "notFound",
    "invalidScope",
    "invalidCursor",
    "featureDisabled",
    "cancellationUnsupported",
    "staleScope",
    "staleActor",
    "staleOperation",
    "providerUnavailable",
    "targetUnavailable",
    "partialCancellation",
    "dispatchTimeout",
    "internal",
  ]),
}) {}
