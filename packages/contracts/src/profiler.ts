import * as Schema from "effect/Schema";

export const PROFILER_CLIENT_BATCH_LIMITS = { records: 64, histograms: 64 } as const;

export const ProfilerSeverity = Schema.Literals(["info", "warn", "error"]);
export const ProfilerFindingState = Schema.Literals(["open", "resolved"]);

export const ProfilerFinding = Schema.Struct({
  id: Schema.String,
  fingerprint: Schema.String,
  kind: Schema.String,
  severity: ProfilerSeverity,
  firstSeen: Schema.String,
  lastSeen: Schema.String,
  count: Schema.Number,
  source: Schema.String,
  title: Schema.String,
  summary: Schema.String,
  state: ProfilerFindingState,
});
export type ProfilerFinding = typeof ProfilerFinding.Type;

const CapabilityState = Schema.Literals(["available", "unavailable", "disabled"]);

export const ProfilerStatus = Schema.Struct({
  enabled: Schema.Boolean,
  recordingSince: Schema.NullOr(Schema.String),
  diskBytes: Schema.Number,
  eventCount: Schema.Number,
  droppedEvents: Schema.Number,
  lastCrashAt: Schema.NullOr(Schema.String),
  capabilities: Schema.Struct({
    nativeDumps: CapabilityState,
    osCrashRecords: CapabilityState,
    storage: Schema.Literals(["ok", "failing"]),
  }),
  findings: Schema.Array(ProfilerFinding),
});
export type ProfilerStatus = typeof ProfilerStatus.Type;

export const ProfilerClientRecord = Schema.Struct({
  correlationId: Schema.NullOr(Schema.String),
  method: Schema.String,
  rttMs: Schema.Number,
  renderMs: Schema.NullOr(Schema.Number),
  startedAtMs: Schema.Number,
});

/** Log-bucket counts per method, bucket i covers (1.12^i, 1.12^(i+1)] ms. */
export const ProfilerClientHistogram = Schema.Struct({
  method: Schema.String,
  buckets: Schema.Array(Schema.Tuple([Schema.Number, Schema.Number])),
});

export const ProfilerClientBatch = Schema.Struct({
  clientId: Schema.String,
  capabilities: Schema.Array(Schema.String),
  windowMs: Schema.Number,
  records: Schema.Array(ProfilerClientRecord).check(
    Schema.isMaxLength(PROFILER_CLIENT_BATCH_LIMITS.records),
  ),
  histograms: Schema.Array(ProfilerClientHistogram).check(
    Schema.isMaxLength(PROFILER_CLIENT_BATCH_LIMITS.histograms),
  ),
  stalls: Schema.Struct({ count: Schema.Number, totalMs: Schema.Number, maxMs: Schema.Number }),
  interactions: Schema.Struct({ count: Schema.Number, maxMs: Schema.Number }),
  jsHeapBytes: Schema.NullOr(Schema.Number),
});
export type ProfilerClientBatch = typeof ProfilerClientBatch.Type;

export const ProfilerClientBatchResult = Schema.Struct({ accepted: Schema.Boolean });
export type ProfilerClientBatchResult = typeof ProfilerClientBatchResult.Type;

/** The batch is over 64 KiB, 64 records or 64 histograms. Nothing was recorded. */
export class ProfilerClientBatchTooLarge extends Schema.TaggedError<ProfilerClientBatchTooLarge>()(
  "ProfilerClientBatchTooLarge",
  {
    message: Schema.String,
  },
) {}

/** A field is missing, of the wrong type, too long or out of range. Nothing was recorded. */
export class ProfilerClientBatchInvalid extends Schema.TaggedError<ProfilerClientBatchInvalid>()(
  "ProfilerClientBatchInvalid",
  {
    message: Schema.String,
  },
) {}
