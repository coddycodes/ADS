import * as Effect from "effect/Effect";
import * as Schema from "effect/Schema";
import { TrimmedNonEmptyString } from "./baseSchemas.ts";
import { ProviderDriverKind, ProviderInstanceId } from "./providerInstance.ts";

/**
 * Client-side id for the first shell opened on a thread. Ids are uniformly
 * `term-N`; there's no "default" intrinsic. Kept as a named constant so callers
 * that want "the primary shell" don't hardcode `"term-1"`.
 */
export const DEFAULT_TERMINAL_ID = "term-1";

export const TERMINAL_COLS_MIN = 1;
export const TERMINAL_COLS_MAX = 1000;
export const TERMINAL_ROWS_MIN = 1;
export const TERMINAL_ROWS_MAX = 500;
export const TERMINAL_SIZE_CLAIM_MAX_LENGTH = 128;

const TrimmedNonEmptyStringSchema = TrimmedNonEmptyString;
const TerminalColsSchema = Schema.Int.check(Schema.isGreaterThanOrEqualTo(TERMINAL_COLS_MIN)).check(
  Schema.isLessThanOrEqualTo(TERMINAL_COLS_MAX),
);
const TerminalRowsSchema = Schema.Int.check(Schema.isGreaterThanOrEqualTo(TERMINAL_ROWS_MIN)).check(
  Schema.isLessThanOrEqualTo(TERMINAL_ROWS_MAX),
);
const TerminalIdSchema = TrimmedNonEmptyStringSchema.check(Schema.isMaxLength(128));
const TerminalSizeClaimSchema = TrimmedNonEmptyStringSchema.check(
  Schema.isMaxLength(TERMINAL_SIZE_CLAIM_MAX_LENGTH),
);

export const TerminalSize = Schema.Struct({
  cols: TerminalColsSchema,
  rows: TerminalRowsSchema,
  sizeClaim: Schema.NullOr(TerminalSizeClaimSchema),
});
export type TerminalSize = typeof TerminalSize.Type;
const TerminalEnvKeySchema = Schema.String.check(
  Schema.isPattern(/^[A-Za-z_][A-Za-z0-9_]*$/),
).check(Schema.isMaxLength(128));
const TerminalEnvValueSchema = Schema.String.check(Schema.isMaxLength(8_192));
const TerminalEnvSchema = Schema.Record(Schema.String, TerminalEnvValueSchema).check(
  Schema.isPropertyNames(TerminalEnvKeySchema),
  Schema.isMaxProperties(128),
);

export const TERMINAL_LAUNCH_EXECUTABLE_MAX_LENGTH = 4_096;
export const TERMINAL_LAUNCH_ARGUMENT_MAX_LENGTH = 8_192;
export const TERMINAL_LAUNCH_ARGUMENT_MAX_COUNT = 64;
export const TERMINAL_LAUNCH_LABEL_MAX_LENGTH = 128;

export const ProviderTerminalActivityLaunch = Schema.Struct({
  driverKind: ProviderDriverKind,
  providerInstanceId: ProviderInstanceId,
});
export type ProviderTerminalActivityLaunch = typeof ProviderTerminalActivityLaunch.Type;

export const TerminalLaunchCommand = Schema.Struct({
  executable: TrimmedNonEmptyStringSchema.check(
    Schema.isMaxLength(TERMINAL_LAUNCH_EXECUTABLE_MAX_LENGTH),
  ),
  args: Schema.Array(
    Schema.String.check(Schema.isMaxLength(TERMINAL_LAUNCH_ARGUMENT_MAX_LENGTH)),
  ).check(Schema.isMaxLength(TERMINAL_LAUNCH_ARGUMENT_MAX_COUNT)),
  label: Schema.optional(
    TrimmedNonEmptyStringSchema.check(Schema.isMaxLength(TERMINAL_LAUNCH_LABEL_MAX_LENGTH)),
  ),
  env: Schema.optional(TerminalEnvSchema),
  activity: Schema.optional(ProviderTerminalActivityLaunch),
});
export type TerminalLaunchCommand = typeof TerminalLaunchCommand.Type;

export const TerminalThreadInput = Schema.Struct({
  threadId: TrimmedNonEmptyStringSchema,
});
export type TerminalThreadInput = typeof TerminalThreadInput.Type;

/** Terminal ids are ALWAYS chosen by the client and sent explicitly — no server-side allocation. */
const TerminalSessionInput = Schema.Struct({
  ...TerminalThreadInput.fields,
  terminalId: TerminalIdSchema,
});
export type TerminalSessionInput = Schema.Codec.Encoded<typeof TerminalSessionInput>;

export const TerminalOpenInput = Schema.Struct({
  ...TerminalSessionInput.fields,
  cwd: TrimmedNonEmptyStringSchema,
  worktreePath: Schema.optional(Schema.NullOr(TrimmedNonEmptyStringSchema)),
  cols: Schema.optional(TerminalColsSchema),
  rows: Schema.optional(TerminalRowsSchema),
  env: Schema.optional(TerminalEnvSchema),
  command: Schema.optional(TerminalLaunchCommand),
  /** Marks a host thread's center panel so every client can adopt it; sticky once set. */
  centerPanel: Schema.optional(Schema.Boolean),
});
export type TerminalOpenInput = typeof TerminalOpenInput.Type;

export const TerminalAttachInput = Schema.Struct({
  ...TerminalSessionInput.fields,
  cwd: Schema.optional(TrimmedNonEmptyStringSchema),
  worktreePath: Schema.optional(Schema.NullOr(TrimmedNonEmptyStringSchema)),
  cols: Schema.optional(TerminalColsSchema),
  rows: Schema.optional(TerminalRowsSchema),
  env: Schema.optional(TerminalEnvSchema),
  command: Schema.optional(TerminalLaunchCommand),
  restartIfNotRunning: Schema.optional(Schema.Boolean),
  sizeClaim: Schema.optional(TerminalSizeClaimSchema),
  /** Same marker as `TerminalOpenInput.centerPanel`, for attaches that create the session. */
  centerPanel: Schema.optional(Schema.Boolean),
});
export type TerminalAttachInput = typeof TerminalAttachInput.Type;

export const TerminalWriteInput = Schema.Struct({
  ...TerminalSessionInput.fields,
  data: Schema.String.check(Schema.isNonEmpty()).check(Schema.isMaxLength(65_536)),
});
export type TerminalWriteInput = Schema.Codec.Encoded<typeof TerminalWriteInput>;

const TerminalInputSequence = Schema.Int.check(Schema.isGreaterThanOrEqualTo(0)).check(
  Schema.isLessThanOrEqualTo(Number.MAX_SAFE_INTEGER),
);

export const TerminalBeginInput = Schema.Struct({
  ...TerminalSessionInput.fields,
  attachmentSequence: TerminalInputSequence,
});
export type TerminalBeginInput = Schema.Codec.Encoded<typeof TerminalBeginInput>;

export const TerminalInputLease = Schema.Struct({ inputId: TrimmedNonEmptyStringSchema });
export type TerminalInputLease = typeof TerminalInputLease.Type;

export const TerminalWriteInputFrame = Schema.Struct({
  ...TerminalSessionInput.fields,
  inputId: TrimmedNonEmptyStringSchema,
  sequence: TerminalInputSequence,
  data: Schema.String.check(Schema.isNonEmpty()).check(Schema.isMaxLength(16_384)),
});
export type TerminalWriteInputFrame = typeof TerminalWriteInputFrame.Type;

export const TerminalInputAcknowledgement = Schema.Struct({
  inputId: TrimmedNonEmptyStringSchema,
  sequence: TerminalInputSequence,
});
export type TerminalInputAcknowledgement = typeof TerminalInputAcknowledgement.Type;

export const TerminalCancelInput = Schema.Struct({
  ...TerminalSessionInput.fields,
  inputId: TrimmedNonEmptyStringSchema,
});
export type TerminalCancelInput = typeof TerminalCancelInput.Type;

export const TerminalResizeInput = Schema.Struct({
  ...TerminalSessionInput.fields,
  cols: TerminalColsSchema,
  rows: TerminalRowsSchema,
  sizeClaim: Schema.optional(TerminalSizeClaimSchema),
});
export type TerminalResizeInput = Schema.Codec.Encoded<typeof TerminalResizeInput>;

export const TerminalClearInput = TerminalSessionInput;
export type TerminalClearInput = Schema.Codec.Encoded<typeof TerminalClearInput>;

export const TerminalRestartInput = Schema.Struct({
  ...TerminalSessionInput.fields,
  cwd: TrimmedNonEmptyStringSchema,
  worktreePath: Schema.optional(Schema.NullOr(TrimmedNonEmptyStringSchema)),
  cols: TerminalColsSchema,
  rows: TerminalRowsSchema,
  env: Schema.optional(TerminalEnvSchema),
  command: Schema.optional(TerminalLaunchCommand),
});
export type TerminalRestartInput = typeof TerminalRestartInput.Type;

export const TerminalCloseInput = Schema.Struct({
  ...TerminalThreadInput.fields,
  terminalId: Schema.optional(TerminalIdSchema),
  deleteHistory: Schema.optional(Schema.Boolean),
});
export type TerminalCloseInput = typeof TerminalCloseInput.Type;

export const TerminalSessionStatus = Schema.Literals(["starting", "running", "exited", "error"]);
export type TerminalSessionStatus = typeof TerminalSessionStatus.Type;

export const TerminalConsoleTheme = Schema.Literals(["light", "dark"]);
export type TerminalConsoleTheme = typeof TerminalConsoleTheme.Type;

export const TerminalSessionSnapshot = Schema.Struct({
  threadId: Schema.String.check(Schema.isNonEmpty()),
  terminalId: Schema.String.check(Schema.isNonEmpty()),
  cwd: Schema.String.check(Schema.isNonEmpty()),
  worktreePath: Schema.NullOr(TrimmedNonEmptyStringSchema),
  status: TerminalSessionStatus,
  pid: Schema.NullOr(Schema.Int.check(Schema.isGreaterThan(0))),
  history: Schema.String,
  exitCode: Schema.NullOr(Schema.Int),
  exitSignal: Schema.NullOr(Schema.Int),
  /** Palette applied to a persistent native console when the PTY was spawned. */
  consoleTheme: Schema.optional(TerminalConsoleTheme),
  /** Server-computed display title (idle shell vs subprocess command). */
  label: Schema.String.check(Schema.isMaxLength(128)),
  updatedAt: Schema.String,
  sequence: Schema.optional(Schema.Int.check(Schema.isGreaterThanOrEqualTo(0))),
  /** Applied PTY size; absent on older servers. */
  size: Schema.optional(TerminalSize),
  oscColorResponderActive: Schema.Boolean.pipe(Schema.withDecodingDefault(Effect.succeed(false))),
  /** First-attachment grant: one startup-history reply pass, issued only before any claim, or a claimed stream surviving a restart, has consumed it. */
  firstAttachmentGrant: Schema.Boolean.pipe(Schema.withDecodingDefault(Effect.succeed(false))),
});
export type TerminalSessionSnapshot = typeof TerminalSessionSnapshot.Type;

export const TerminalSummary = Schema.Struct({
  threadId: Schema.String.check(Schema.isNonEmpty()),
  terminalId: Schema.String.check(Schema.isNonEmpty()),
  cwd: Schema.String.check(Schema.isNonEmpty()),
  worktreePath: Schema.NullOr(TrimmedNonEmptyStringSchema),
  status: TerminalSessionStatus,
  pid: Schema.NullOr(Schema.Int.check(Schema.isGreaterThan(0))),
  exitCode: Schema.NullOr(Schema.Int),
  exitSignal: Schema.NullOr(Schema.Int),
  /** Palette applied to a persistent native console when the PTY was spawned. */
  consoleTheme: Schema.optional(TerminalConsoleTheme),
  hasRunningSubprocess: Schema.Boolean,
  /** Server-computed display title (idle shell vs subprocess command). */
  label: Schema.String.check(Schema.isMaxLength(128)),
  updatedAt: Schema.String,
  /** True when the session is a host thread's center panel; false on older servers. */
  centerPanel: Schema.Boolean.pipe(Schema.withDecodingDefault(Effect.succeed(false))),
  /** Launch command the session was started with, when it was started with one. */
  command: Schema.optional(TerminalLaunchCommand),
});
export type TerminalSummary = typeof TerminalSummary.Type;

const TerminalMetadataSnapshotEvent = Schema.Struct({
  type: Schema.Literal("snapshot"),
  terminals: Schema.Array(TerminalSummary),
});

const TerminalMetadataUpsertEvent = Schema.Struct({
  type: Schema.Literal("upsert"),
  terminal: TerminalSummary,
});

/** Only `closed` means a client closed the session; absent on older servers. */
export const TerminalRemovalReason = Schema.Literals(["closed", "restarted", "shutdown"]);
export type TerminalRemovalReason = typeof TerminalRemovalReason.Type;

const TerminalMetadataRemoveEvent = Schema.Struct({
  type: Schema.Literal("remove"),
  threadId: Schema.String.check(Schema.isNonEmpty()),
  terminalId: Schema.String.check(Schema.isNonEmpty()),
  reason: Schema.optional(TerminalRemovalReason),
});

export const TerminalMetadataStreamEvent = Schema.Union([
  TerminalMetadataSnapshotEvent,
  TerminalMetadataUpsertEvent,
  TerminalMetadataRemoveEvent,
]);
export type TerminalMetadataStreamEvent = typeof TerminalMetadataStreamEvent.Type;

const TerminalEventBaseSchema = Schema.Struct({
  threadId: Schema.String.check(Schema.isNonEmpty()),
  terminalId: Schema.String.check(Schema.isNonEmpty()),
  sequence: Schema.optional(Schema.Int.check(Schema.isGreaterThanOrEqualTo(0))),
});

const TerminalStartedEvent = Schema.Struct({
  ...TerminalEventBaseSchema.fields,
  type: Schema.Literal("started"),
  snapshot: TerminalSessionSnapshot,
});

const TerminalOutputEvent = Schema.Struct({
  ...TerminalEventBaseSchema.fields,
  type: Schema.Literal("output"),
  data: Schema.String,
});

const TerminalResizedEvent = Schema.Struct({
  ...TerminalEventBaseSchema.fields,
  size: TerminalSize,
  type: Schema.Literal("resized"),
});

const TerminalExitedEvent = Schema.Struct({
  ...TerminalEventBaseSchema.fields,
  type: Schema.Literal("exited"),
  exitCode: Schema.NullOr(Schema.Int),
  exitSignal: Schema.NullOr(Schema.Int),
});

const TerminalClosedEvent = Schema.Struct({
  ...TerminalEventBaseSchema.fields,
  type: Schema.Literal("closed"),
});

const TerminalErrorEvent = Schema.Struct({
  ...TerminalEventBaseSchema.fields,
  type: Schema.Literal("error"),
  message: Schema.String.check(Schema.isNonEmpty()),
});

const TerminalClearedEvent = Schema.Struct({
  ...TerminalEventBaseSchema.fields,
  type: Schema.Literal("cleared"),
});

const TerminalRestartedEvent = Schema.Struct({
  ...TerminalEventBaseSchema.fields,
  type: Schema.Literal("restarted"),
  snapshot: TerminalSessionSnapshot,
});

const TerminalActivityEvent = Schema.Struct({
  ...TerminalEventBaseSchema.fields,
  type: Schema.Literal("activity"),
  hasRunningSubprocess: Schema.Boolean,
  label: Schema.String.check(Schema.isMaxLength(128)),
});

export const TerminalEvent = Schema.Union([
  TerminalStartedEvent,
  TerminalOutputEvent,
  TerminalExitedEvent,
  TerminalClosedEvent,
  TerminalErrorEvent,
  TerminalClearedEvent,
  TerminalRestartedEvent,
  TerminalActivityEvent,
]);
export type TerminalEvent = typeof TerminalEvent.Type;

const TerminalAttachSnapshotEvent = Schema.Struct({
  type: Schema.Literal("snapshot"),
  snapshot: TerminalSessionSnapshot,
  /**
   * The server's catch-up after its stream fell behind: the same attachment and process, so
   * only the transcript is replaced. Absent on the first snapshot of an attachment.
   */
  resync: Schema.optionalKey(Schema.Boolean),
});

export const TerminalAttachStreamEvent = Schema.Union([
  TerminalAttachSnapshotEvent,
  TerminalOutputEvent,
  TerminalExitedEvent,
  TerminalClosedEvent,
  TerminalErrorEvent,
  TerminalClearedEvent,
  TerminalRestartedEvent,
  TerminalActivityEvent,
  TerminalResizedEvent,
]);
export type TerminalAttachStreamEvent = typeof TerminalAttachStreamEvent.Type;

export class TerminalCwdNotFoundError extends Schema.TaggedError<TerminalCwdNotFoundError>()(
  "TerminalCwdNotFoundError",
  {
    cwd: Schema.String,
  },
) {
  override get message() {
    return `Terminal cwd does not exist: ${this.cwd}`;
  }
}

export class TerminalCwdNotDirectoryError extends Schema.TaggedError<TerminalCwdNotDirectoryError>()(
  "TerminalCwdNotDirectoryError",
  {
    cwd: Schema.String,
  },
) {
  override get message() {
    return `Terminal cwd is not a directory: ${this.cwd}`;
  }
}

export class TerminalCwdStatError extends Schema.TaggedError<TerminalCwdStatError>()(
  "TerminalCwdStatError",
  {
    cwd: Schema.String,
    cause: Schema.Defect(),
  },
) {
  override get message() {
    return `Failed to access terminal cwd: ${this.cwd}`;
  }
}

export const TerminalCwdError = Schema.Union([
  TerminalCwdNotFoundError,
  TerminalCwdNotDirectoryError,
  TerminalCwdStatError,
]);
export type TerminalCwdError = typeof TerminalCwdError.Type;

export const TERMINAL_CLOSE_ERROR_REASON_MAX_LENGTH = 512;

export class TerminalCloseError extends Schema.TaggedError<TerminalCloseError>()(
  "TerminalCloseError",
  {
    reason: Schema.String.check(Schema.isNonEmpty()).check(
      Schema.isMaxLength(TERMINAL_CLOSE_ERROR_REASON_MAX_LENGTH),
    ),
  },
) {
  override get message() {
    return this.reason;
  }
}

export const TERMINAL_SPAWN_ERROR_REASON_MAX_LENGTH = 512;

export class TerminalSpawnError extends Schema.TaggedError<TerminalSpawnError>()(
  "TerminalSpawnError",
  {
    reason: Schema.String.check(Schema.isNonEmpty()).check(
      Schema.isMaxLength(TERMINAL_SPAWN_ERROR_REASON_MAX_LENGTH),
    ),
  },
) {
  override get message() {
    return this.reason;
  }
}

export class TerminalHistoryError extends Schema.TaggedError<TerminalHistoryError>()(
  "TerminalHistoryError",
  {
    operation: Schema.Literals(["read", "truncate", "migrate"]),
    threadId: Schema.String,
    terminalId: Schema.String,
    cause: Schema.optional(Schema.Defect()),
  },
) {
  override get message() {
    return `Failed to ${this.operation} terminal history for thread: ${this.threadId}, terminal: ${this.terminalId}`;
  }
}

export class TerminalSessionLookupError extends Schema.TaggedError<TerminalSessionLookupError>()(
  "TerminalSessionLookupError",
  {
    threadId: Schema.String,
    terminalId: Schema.String,
  },
) {
  override get message() {
    return `Unknown terminal thread: ${this.threadId}, terminal: ${this.terminalId}`;
  }
}

export class TerminalNotRunningError extends Schema.TaggedError<TerminalNotRunningError>()(
  "TerminalNotRunningError",
  {
    threadId: Schema.String,
    terminalId: Schema.String,
  },
) {
  override get message() {
    return `Terminal is not running for thread: ${this.threadId}, terminal: ${this.terminalId}`;
  }
}

export class TerminalWriteError extends Schema.TaggedError<TerminalWriteError>()(
  "TerminalWriteError",
  {
    threadId: Schema.String,
    terminalId: Schema.String,
    terminalPid: Schema.Finite,
    cause: Schema.Defect(),
  },
) {
  override get message() {
    return `Failed to write to terminal for thread: ${this.threadId}, terminal: ${this.terminalId}, PID: ${this.terminalPid}`;
  }
}

export class TerminalResizeError extends Schema.TaggedError<TerminalResizeError>()(
  "TerminalResizeError",
  {
    threadId: Schema.String,
    terminalId: Schema.String,
    terminalPid: Schema.Finite,
    cols: TerminalColsSchema,
    rows: TerminalRowsSchema,
    cause: Schema.Defect(),
  },
) {
  override get message() {
    return `Failed to resize terminal for thread: ${this.threadId}, terminal: ${this.terminalId}, PID: ${this.terminalPid} to ${this.cols}x${this.rows}`;
  }
}

export class TerminalInputError extends Schema.TaggedError<TerminalInputError>()(
  "TerminalInputError",
  { code: Schema.Literals(["closed", "sequence", "capacity", "write"]), message: Schema.String },
) {}

export const TerminalError = Schema.Union([
  TerminalCwdError,
  TerminalCloseError,
  TerminalSpawnError,
  TerminalHistoryError,
  TerminalSessionLookupError,
  TerminalNotRunningError,
  TerminalWriteError,
  TerminalResizeError,
]);
export type TerminalError = typeof TerminalError.Type;
