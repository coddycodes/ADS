import * as Cause from "effect/Cause";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import type * as Exit from "effect/Exit";
import * as ExitRuntime from "effect/Exit";
import * as Fiber from "effect/Fiber";
import * as Option from "effect/Option";
import * as Tracer from "effect/Tracer";
import { OtlpResource, OtlpTracer } from "effect/observability";

import {
  isRotatingFileSinkTerminalError,
  RotatingFileSink,
  type RotatingFileSinkFileSystem,
} from "./logging.ts";
import {
  isSensitiveTraceKey,
  REDACTED_TRACE_VALUE,
  sanitizeTraceText,
  UN_SERIALIZABLE_TRACE_VALUE,
} from "./traceSanitization.ts";

export { sanitizeTraceText } from "./traceSanitization.ts";

const FLUSH_BUFFER_THRESHOLD = 32;
export const DEFAULT_MAX_PENDING_TRACE_RECORDS = 1_024;
export const DEFAULT_MAX_PENDING_TRACE_BYTES = 4 * 1_024 * 1_024;
export const TRACE_NORMALIZATION_MAX_DEPTH = 8;
export const TRACE_NORMALIZATION_MAX_NODES = 512;
export const TRACE_NORMALIZATION_MAX_CHARACTERS = 32_768;
const TRUNCATED_TRACE_VALUE = "[Truncated]";

export type TraceAttributes = Readonly<Record<string, unknown>>;

export interface TraceRecordEvent {
  readonly name: string;
  readonly timeUnixNano: string;
  readonly attributes: Readonly<Record<string, unknown>>;
}

export interface TraceRecordLink {
  readonly traceId: string;
  readonly spanId: string;
  readonly attributes: Readonly<Record<string, unknown>>;
}

interface BaseTraceRecord {
  readonly name: string;
  readonly kind: string;
  readonly traceId: string;
  readonly spanId: string;
  readonly parentSpanId?: string;
  readonly sampled: boolean;
  readonly startTimeUnixNano: string;
  readonly endTimeUnixNano: string;
  readonly durationMs: number;
  readonly attributes: Readonly<Record<string, unknown>>;
  readonly events: ReadonlyArray<TraceRecordEvent>;
  readonly links: ReadonlyArray<TraceRecordLink>;
}

export interface EffectTraceRecord extends BaseTraceRecord {
  readonly type: "effect-span";
  readonly exit:
    | {
        readonly _tag: "Success";
      }
    | {
        readonly _tag: "Interrupted";
        readonly cause: string;
        readonly classification?: "Interrupted";
        readonly reasons?: ReadonlyArray<TraceExitReason>;
      }
    | {
        readonly _tag: "Failure";
        readonly cause: string;
        readonly classification?: "Failure" | "Defect" | "Combined" | "Unknown";
        readonly reasons?: ReadonlyArray<TraceExitReason>;
      };
}

export type TraceExitReason = "Failure" | "Defect" | "Interrupted";

export interface OtlpTraceRecord extends BaseTraceRecord {
  readonly type: "otlp-span";
  readonly resourceAttributes: Readonly<Record<string, unknown>>;
  readonly scope: Readonly<{
    readonly name?: string;
    readonly version?: string;
    readonly attributes: Readonly<Record<string, unknown>>;
  }>;
  readonly status?:
    | {
        readonly code?: string;
        readonly message?: string;
      }
    | undefined;
}

export type TraceRecord = EffectTraceRecord | OtlpTraceRecord;

function isStructuralTag(value: unknown): value is string {
  return (
    typeof value === "string" &&
    value.length > 0 &&
    value.length <= 128 &&
    /^[A-Za-z][A-Za-z0-9._:/-]*$/.test(value)
  );
}

export function errorTag(error: unknown): string {
  try {
    if (typeof error === "object" && error !== null && "_tag" in error) {
      return isStructuralTag(error._tag) ? error._tag : "TaggedError";
    }
    if (error instanceof Error) {
      return isStructuralTag(error.name) ? error.name : "Error";
    }
  } catch {
    return "UnknownError";
  }
  return typeof error;
}

export function causeErrorTag(cause: Cause.Cause<unknown>): string {
  const failure = Cause.findErrorOption(cause);
  if (Option.isSome(failure)) {
    return errorTag(failure.value);
  }
  return cause.reasons[0]?._tag ?? "Empty";
}

export interface TraceSinkOptions {
  readonly filePath: string;
  readonly maxBytes: number;
  readonly maxFiles: number;
  readonly batchWindowMs: number;
  readonly maxPendingRecords?: number;
  readonly maxPendingBytes?: number;
  readonly onDiagnostic?: (diagnostic: TraceSinkDiagnostic) => void;
  readonly fileSystem?: RotatingFileSinkFileSystem;
}

export class TraceSinkConfigurationError extends Error {
  readonly option = "batchWindowMs" as const;
  readonly received: number;

  constructor(received: number) {
    super(`batchWindowMs must be finite and > 0 (received ${received})`);
    this.name = "TraceSinkConfigurationError";
    this.received = received;
  }
}

export interface TraceSinkDiagnostic {
  readonly event:
    | "buffer-overflow"
    | "record-too-large"
    | "sink-failure"
    | "sink-terminal-failure"
    | "sink-recovered";
  readonly pendingRecords: number;
  readonly pendingBytes: number;
  readonly droppedRecords: number;
  readonly consecutiveFailures: number;
  readonly maxPendingRecords: number;
  readonly maxPendingBytes: number;
  readonly reason?: string;
}

export interface TraceSinkStats {
  readonly pendingRecords: number;
  readonly pendingBytes: number;
  readonly droppedRecords: number;
  readonly consecutiveFailures: number;
  readonly maxPendingRecords: number;
  readonly maxPendingBytes: number;
}

export interface TraceSink {
  readonly filePath: string;
  push: (record: TraceRecord) => void;
  flush: Effect.Effect<void>;
  close: () => Effect.Effect<void>;
  stats?: () => TraceSinkStats;
}

const preparedTracePushes = new WeakMap<TraceSink, (record: EffectTraceRecord) => void>();

export interface LocalFileTracerOptions extends TraceSinkOptions {
  readonly delegate?: Tracer.Tracer;
  readonly sink?: TraceSink;
}

type OtlpSpan = OtlpTracer.ScopeSpan["spans"][number];
type OtlpSpanEvent = OtlpSpan["events"][number];
type OtlpSpanLink = OtlpSpan["links"][number];
type OtlpSpanStatus = OtlpSpan["status"];

interface SerializableSpan {
  readonly name: string;
  readonly traceId: string;
  readonly spanId: string;
  readonly parent: Option.Option<Tracer.AnySpan>;
  readonly status: Tracer.SpanStatus;
  readonly sampled: boolean;
  readonly kind: Tracer.SpanKind;
  readonly attributes: ReadonlyMap<string, unknown>;
  readonly links: ReadonlyArray<Tracer.SpanLink>;
  readonly events: ReadonlyArray<
    readonly [name: string, startTime: bigint, attributes: Record<string, unknown>]
  >;
}

function isPlainObject(value: unknown): value is Record<string, unknown> {
  if (typeof value !== "object" || value === null) {
    return false;
  }
  const prototype = Object.getPrototypeOf(value);
  return prototype === Object.prototype || prototype === null;
}

function markSeen(value: object, seen: WeakSet<object>): boolean {
  if (seen.has(value)) {
    return true;
  }
  seen.add(value);
  return false;
}

interface TraceNormalizationContext {
  readonly seen: WeakSet<object>;
  nodes: number;
  characters: number;
  exhausted: boolean;
}

function makeTraceNormalizationContext(): TraceNormalizationContext {
  return {
    seen: new WeakSet(),
    nodes: 0,
    characters: 0,
    exhausted: false,
  };
}

function truncateTraceValue(context: TraceNormalizationContext): string {
  context.exhausted = true;
  context.characters = TRACE_NORMALIZATION_MAX_CHARACTERS;
  return TRUNCATED_TRACE_VALUE;
}

function emitTraceText(value: string, context: TraceNormalizationContext): string {
  const sanitized = sanitizeTraceText(value);
  if (
    context.characters + sanitized.length >
    TRACE_NORMALIZATION_MAX_CHARACTERS - TRUNCATED_TRACE_VALUE.length
  ) {
    return truncateTraceValue(context);
  }
  context.characters += sanitized.length;
  return sanitized;
}

function beginTraceNode(context: TraceNormalizationContext, depth: number): boolean {
  if (
    context.exhausted ||
    depth > TRACE_NORMALIZATION_MAX_DEPTH ||
    context.nodes >= TRACE_NORMALIZATION_MAX_NODES
  ) {
    return false;
  }
  context.nodes += 1;
  return true;
}

function hasTraceNodeBudget(context: TraceNormalizationContext, depth: number): boolean {
  return (
    !context.exhausted &&
    depth <= TRACE_NORMALIZATION_MAX_DEPTH &&
    context.nodes < TRACE_NORMALIZATION_MAX_NODES
  );
}

function normalizeUnkeyedProperty(
  container: object,
  key: PropertyKey,
  context: TraceNormalizationContext,
  depth: number,
): unknown {
  try {
    return normalizeJsonValue(Reflect.get(container, key), context, depth);
  } catch {
    return emitTraceText(UN_SERIALIZABLE_TRACE_VALUE, context);
  }
}

function normalizedPropertyKey(key: PropertyKey, context: TraceNormalizationContext): string {
  return emitTraceText(String(key), context);
}

function normalizeMapKey(key: unknown, context: TraceNormalizationContext): string {
  try {
    return emitTraceText(String(key), context);
  } catch {
    return emitTraceText(UN_SERIALIZABLE_TRACE_VALUE, context);
  }
}

function normalizeEnumerableProperties(
  value: object,
  context: TraceNormalizationContext,
  depth: number,
  excluded: ReadonlySet<PropertyKey> = new Set(),
): Record<string, unknown> {
  const output: Record<string, unknown> = {};
  for (const key in value) {
    if (context.exhausted) {
      break;
    }
    if (!Object.hasOwn(value, key) || excluded.has(key)) {
      continue;
    }
    const outputKey = normalizedPropertyKey(key, context);
    if (context.exhausted) {
      output[outputKey] = null;
      break;
    }
    if (isSensitiveTraceKey(String(key))) {
      output[outputKey] = emitTraceText(REDACTED_TRACE_VALUE, context);
      continue;
    }
    if (!hasTraceNodeBudget(context, depth)) {
      output[TRUNCATED_TRACE_VALUE] = truncateTraceValue(context);
      break;
    }
    try {
      const propertyValue = Reflect.get(value, key);
      if (propertyValue !== undefined) {
        output[outputKey] = normalizeJsonValue(propertyValue, context, depth);
      }
    } catch {
      output[outputKey] = emitTraceText(UN_SERIALIZABLE_TRACE_VALUE, context);
    }
  }
  return output;
}

function normalizeError(
  value: Error,
  context: TraceNormalizationContext,
  depth: number,
): Record<string, unknown> {
  if (markSeen(value, context.seen)) {
    return {
      [normalizedPropertyKey("name", context)]: emitTraceText("Error", context),
      [normalizedPropertyKey("message", context)]: emitTraceText("[Circular]", context),
    };
  }
  const output: Record<string, unknown> = {};
  const nameKey = normalizedPropertyKey("name", context);
  output[nameKey] = normalizeUnkeyedProperty(value, "name", context, depth);
  if (context.exhausted) {
    return output;
  }
  const messageKey = normalizedPropertyKey("message", context);
  output[messageKey] = normalizeUnkeyedProperty(value, "message", context, depth);
  if (context.exhausted) {
    return output;
  }
  const stack = normalizeUnkeyedProperty(value, "stack", context, depth);
  if (typeof stack === "string" && stack.length > 0) {
    output[normalizedPropertyKey("stack", context)] = stack;
  }
  if (!context.exhausted && Object.hasOwn(value, "cause")) {
    output[normalizedPropertyKey("cause", context)] = normalizeUnkeyedProperty(
      value,
      "cause",
      context,
      depth,
    );
  }
  if (!context.exhausted) {
    Object.assign(
      output,
      normalizeEnumerableProperties(
        value,
        context,
        depth,
        new Set(["name", "message", "stack", "cause"]),
      ),
    );
  }
  return output;
}

function normalizeJsonValueUnsafe(
  value: unknown,
  context: TraceNormalizationContext,
  depth: number,
): unknown {
  if (
    value === null ||
    value === undefined ||
    typeof value === "number" ||
    typeof value === "boolean"
  ) {
    const normalized = value ?? null;
    const text = String(normalized);
    return emitTraceText(text, context) === text ? normalized : TRUNCATED_TRACE_VALUE;
  }
  if (typeof value === "string") {
    return emitTraceText(value, context);
  }
  if (typeof value === "bigint") {
    return emitTraceText(value.toString(), context);
  }
  if (value instanceof Date) {
    return emitTraceText(
      Number.isNaN(value.getTime()) ? "Invalid Date" : value.toISOString(),
      context,
    );
  }
  if (value instanceof URL || value instanceof URLSearchParams) {
    return emitTraceText(value.toString(), context);
  }
  if (value instanceof Error) {
    return normalizeError(value, context, depth + 1);
  }
  if (Array.isArray(value)) {
    if (markSeen(value, context.seen)) {
      return emitTraceText("[Circular]", context);
    }
    const output: Array<unknown> = [];
    for (let index = 0; index < value.length; index += 1) {
      if (!hasTraceNodeBudget(context, depth + 1)) {
        output.push(truncateTraceValue(context));
        break;
      }
      const normalized = normalizeUnkeyedProperty(value, index, context, depth + 1);
      output.push(normalized);
      if (context.exhausted) {
        break;
      }
    }
    return output;
  }
  if (value instanceof Map) {
    if (markSeen(value, context.seen)) {
      return emitTraceText("[Circular]", context);
    }
    const output: Record<string, unknown> = {};
    const iterator = value.entries();
    while (hasTraceNodeBudget(context, depth + 1)) {
      const next = iterator.next();
      if (next.done) {
        break;
      }
      const entry = next.value;
      const key = entry[0];
      const normalizedKey = normalizeMapKey(key, context);
      if (context.exhausted) {
        output[normalizedKey] = null;
        break;
      }
      if (isSensitiveTraceKey(normalizedKey)) {
        output[normalizedKey] = emitTraceText(REDACTED_TRACE_VALUE, context);
      } else {
        const entryValue = entry[1];
        output[normalizedKey] = normalizeJsonValue(entryValue, context, depth + 1);
      }
      if (context.exhausted) {
        break;
      }
    }
    if (!context.exhausted && !hasTraceNodeBudget(context, depth + 1)) {
      output[TRUNCATED_TRACE_VALUE] = truncateTraceValue(context);
    }
    return output;
  }
  if (value instanceof Set) {
    if (markSeen(value, context.seen)) {
      return emitTraceText("[Circular]", context);
    }
    const output: Array<unknown> = [];
    const iterator = value.values();
    while (hasTraceNodeBudget(context, depth + 1)) {
      const next = iterator.next();
      if (next.done) {
        break;
      }
      output.push(normalizeJsonValue(next.value, context, depth + 1));
      if (context.exhausted) {
        break;
      }
    }
    if (!context.exhausted && !hasTraceNodeBudget(context, depth + 1)) {
      output.push(truncateTraceValue(context));
    }
    return output;
  }
  if (!isPlainObject(value)) {
    return emitTraceText(String(value), context);
  }
  if (markSeen(value, context.seen)) {
    return emitTraceText("[Circular]", context);
  }
  return normalizeEnumerableProperties(value, context, depth + 1);
}

function normalizeJsonValue(
  value: unknown,
  context: TraceNormalizationContext = makeTraceNormalizationContext(),
  depth = 0,
): unknown {
  if (!beginTraceNode(context, depth)) {
    return truncateTraceValue(context);
  }
  try {
    return normalizeJsonValueUnsafe(value, context, depth);
  } catch {
    return emitTraceText(UN_SERIALIZABLE_TRACE_VALUE, context);
  }
}

export function compactTraceAttributes(
  attributes: Readonly<Record<string, unknown>>,
): TraceAttributes {
  const normalized = normalizeJsonValue(attributes);
  return typeof normalized === "object" && normalized !== null && !Array.isArray(normalized)
    ? (normalized as TraceAttributes)
    : { value: normalized };
}

function normalizedTraceString(value: string, context: TraceNormalizationContext): string {
  return normalizeJsonValue(value, context) as string;
}

function normalizedTraceAttributes(
  value: unknown,
  context: TraceNormalizationContext,
  depth: number,
): TraceAttributes {
  const normalized = normalizeJsonValue(value, context, depth);
  return typeof normalized === "object" && normalized !== null && !Array.isArray(normalized)
    ? (normalized as TraceAttributes)
    : { value: normalized };
}

function normalizeSpanAttributes(
  attributes: ReadonlyMap<string, unknown>,
  context: TraceNormalizationContext,
): TraceAttributes {
  if (!(attributes instanceof Map)) {
    return { value: emitTraceText(UN_SERIALIZABLE_TRACE_VALUE, context) };
  }
  if (!beginTraceNode(context, 1)) {
    return { [TRUNCATED_TRACE_VALUE]: truncateTraceValue(context) };
  }
  const output: Record<string, unknown> = {};
  const iterator = attributes.entries();
  while (hasTraceNodeBudget(context, 2)) {
    const next = iterator.next();
    if (next.done) {
      return output;
    }
    const entry = next.value;
    let key: string;
    try {
      key = String(entry[0]);
    } catch {
      key = UN_SERIALIZABLE_TRACE_VALUE;
    }
    const outputKey = normalizedPropertyKey(key, context);
    if (isSensitiveTraceKey(key)) {
      output[outputKey] = emitTraceText(REDACTED_TRACE_VALUE, context);
      continue;
    }
    output[outputKey] = normalizeJsonValue(entry[1], context, 2);
  }
  output[TRUNCATED_TRACE_VALUE] = truncateTraceValue(context);
  return output;
}

function normalizeSpanEvents(
  events: SerializableSpan["events"],
  context: TraceNormalizationContext,
): ReadonlyArray<TraceRecordEvent> {
  if (!Array.isArray(events) || !beginTraceNode(context, 1)) {
    return [{ name: TRUNCATED_TRACE_VALUE, timeUnixNano: "0", attributes: {} }];
  }
  const output: Array<TraceRecordEvent> = [];
  for (let index = 0; index < events.length; index += 1) {
    if (!beginTraceNode(context, 2)) {
      truncateTraceValue(context);
      output.push({ name: TRUNCATED_TRACE_VALUE, timeUnixNano: "0", attributes: {} });
      break;
    }
    try {
      const event = events[index]!;
      output.push({
        name: normalizedTraceString(event[0], context),
        timeUnixNano: normalizedTraceString(String(event[1]), context),
        attributes: normalizedTraceAttributes(event[2], context, 3),
      });
    } catch {
      output.push({
        name: UN_SERIALIZABLE_TRACE_VALUE,
        timeUnixNano: "0",
        attributes: {},
      });
    }
    if (context.exhausted) {
      break;
    }
  }
  return output;
}

function normalizeSpanLinks(
  links: SerializableSpan["links"],
  context: TraceNormalizationContext,
): ReadonlyArray<TraceRecordLink> {
  if (!Array.isArray(links) || !beginTraceNode(context, 1)) {
    return [{ traceId: TRUNCATED_TRACE_VALUE, spanId: TRUNCATED_TRACE_VALUE, attributes: {} }];
  }
  const output: Array<TraceRecordLink> = [];
  for (let index = 0; index < links.length; index += 1) {
    if (!beginTraceNode(context, 2)) {
      truncateTraceValue(context);
      output.push({
        traceId: TRUNCATED_TRACE_VALUE,
        spanId: TRUNCATED_TRACE_VALUE,
        attributes: {},
      });
      break;
    }
    try {
      const link = links[index]!;
      output.push({
        traceId: normalizedTraceString(link.span.traceId, context),
        spanId: normalizedTraceString(link.span.spanId, context),
        attributes: normalizedTraceAttributes(link.attributes, context, 3),
      });
    } catch {
      output.push({
        traceId: UN_SERIALIZABLE_TRACE_VALUE,
        spanId: UN_SERIALIZABLE_TRACE_VALUE,
        attributes: {},
      });
    }
    if (context.exhausted) {
      break;
    }
  }
  return output;
}

function formatTraceExit(
  exit: Exit.Exit<unknown, unknown>,
  context: TraceNormalizationContext,
): EffectTraceRecord["exit"] {
  if (ExitRuntime.isSuccess(exit)) {
    return { _tag: "Success" };
  }

  const reasons: Array<TraceExitReason> = [];
  const causeParts: Array<string> = [];
  for (let index = 0; index < exit.cause.reasons.length; index += 1) {
    if (!hasTraceNodeBudget(context, 1)) {
      causeParts.push(truncateTraceValue(context));
      break;
    }
    const reason = exit.cause.reasons[index]!;
    const classification: TraceExitReason = Cause.isFailReason(reason)
      ? "Failure"
      : Cause.isDieReason(reason)
        ? "Defect"
        : "Interrupted";
    if (!reasons.includes(classification)) {
      reasons.push(classification);
    }
    const detail = Cause.isFailReason(reason)
      ? reason.error
      : Cause.isDieReason(reason)
        ? reason.defect
        : { fiberId: reason.fiberId ?? null };
    const normalized = normalizeJsonValue(detail, context, 1);
    causeParts.push(typeof normalized === "string" ? normalized : JSON.stringify(normalized));
  }
  const classification =
    reasons.length === 0 ? "Unknown" : reasons.length === 1 ? reasons[0]! : "Combined";
  const cause =
    causeParts.length === 0 ? emitTraceText("[Empty Cause]", context) : causeParts.join("; ");
  if (classification === "Interrupted") {
    return {
      _tag: "Interrupted",
      cause,
      classification,
      reasons,
    };
  }
  return {
    _tag: "Failure",
    cause,
    classification,
    reasons,
  };
}

export function spanToTraceRecord(span: SerializableSpan): EffectTraceRecord {
  const context = makeTraceNormalizationContext();
  const status = span.status as Extract<Tracer.SpanStatus, { _tag: "Ended" }>;
  const parentSpanId = Option.getOrUndefined(span.parent)?.spanId;
  const name = normalizedTraceString(span.name, context);
  const traceId = normalizedTraceString(span.traceId, context);
  const spanId = normalizedTraceString(span.spanId, context);
  const normalizedParentSpanId = parentSpanId
    ? normalizedTraceString(parentSpanId, context)
    : undefined;
  const kind = normalizedTraceString(span.kind, context);
  const startTimeUnixNano = normalizedTraceString(String(status.startTime), context);
  const endTimeUnixNano = normalizedTraceString(String(status.endTime), context);
  const exit = formatTraceExit(status.exit, context);
  const attributes = normalizeSpanAttributes(span.attributes, context);
  const events = normalizeSpanEvents(span.events, context);
  const links = normalizeSpanLinks(span.links, context);

  return {
    type: "effect-span",
    name,
    traceId,
    spanId,
    ...(normalizedParentSpanId ? { parentSpanId: normalizedParentSpanId } : {}),
    sampled: span.sampled,
    kind,
    startTimeUnixNano,
    endTimeUnixNano,
    durationMs: Number(status.endTime - status.startTime) / 1_000_000,
    attributes,
    events,
    links,
    exit,
  };
}

interface BufferedTraceRecord {
  readonly line: string;
  readonly bytes: number;
}

function traceUtf8ByteLength(value: string): number {
  return new TextEncoder().encode(value).byteLength;
}

export const makeTraceSink = Effect.fn("makeTraceSink")(function* (options: TraceSinkOptions) {
  if (!Number.isFinite(options.batchWindowMs) || options.batchWindowMs <= 0) {
    throw new TraceSinkConfigurationError(options.batchWindowMs);
  }
  const sink = new RotatingFileSink({
    filePath: options.filePath,
    maxBytes: options.maxBytes,
    maxFiles: options.maxFiles,
    throwOnError: true,
    ...(options.fileSystem ? { fileSystem: options.fileSystem } : {}),
  });

  const configuredMaxPendingRecords = options.maxPendingRecords;
  const maxPendingRecords =
    configuredMaxPendingRecords !== undefined &&
    Number.isSafeInteger(configuredMaxPendingRecords) &&
    configuredMaxPendingRecords > 0
      ? configuredMaxPendingRecords
      : DEFAULT_MAX_PENDING_TRACE_RECORDS;
  const configuredMaxPendingBytes = options.maxPendingBytes;
  const maxPendingBytes =
    configuredMaxPendingBytes !== undefined &&
    Number.isSafeInteger(configuredMaxPendingBytes) &&
    configuredMaxPendingBytes > 0
      ? configuredMaxPendingBytes
      : DEFAULT_MAX_PENDING_TRACE_BYTES;
  const buffer: Array<BufferedTraceRecord> = [];
  let pendingBytes = 0;
  let droppedRecords = 0;
  let consecutiveFailures = 0;
  let flushing = false;
  let reporting = false;
  let terminal = false;
  let closed = false;
  let closeStarted = false;
  const closeCompletion = yield* Deferred.make<void>();

  const stats = (): TraceSinkStats => ({
    pendingRecords: buffer.length,
    pendingBytes,
    droppedRecords,
    consecutiveFailures,
    maxPendingRecords,
    maxPendingBytes,
  });

  const report = (diagnostic: Omit<TraceSinkDiagnostic, keyof TraceSinkStats>): void => {
    if (reporting || options.onDiagnostic === undefined) {
      return;
    }
    reporting = true;
    try {
      options.onDiagnostic({ ...diagnostic, ...stats() });
    } catch {
      return;
    } finally {
      reporting = false;
    }
  };

  const enforceBound = (): void => {
    let overflow = 0;
    while (buffer.length > maxPendingRecords || pendingBytes > maxPendingBytes) {
      const dropped = buffer.shift()!;
      pendingBytes -= dropped.bytes;
      overflow += 1;
    }
    if (overflow === 0) {
      return;
    }
    droppedRecords += overflow;
    report({ event: "buffer-overflow" });
  };

  const flushUnsafe = () => {
    if (buffer.length === 0 || flushing || terminal) {
      return;
    }

    flushing = true;
    const recordCount = buffer.length;
    const chunk = buffer.map((entry) => entry.line).join("");
    const chunkBytes = buffer.reduce((total, entry) => total + entry.bytes, 0);

    try {
      sink.write(chunk);
      buffer.splice(0, recordCount);
      pendingBytes -= chunkBytes;
      const recovered = consecutiveFailures > 0;
      consecutiveFailures = 0;
      if (recovered) {
        report({ event: "sink-recovered" });
      }
    } catch (error) {
      consecutiveFailures += 1;
      if (isRotatingFileSinkTerminalError(error)) {
        terminal = true;
        droppedRecords += buffer.length;
        buffer.splice(0, buffer.length);
        pendingBytes = 0;
        report({ event: "sink-terminal-failure", reason: errorTag(error) });
      } else {
        report({ event: "sink-failure", reason: errorTag(error) });
      }
    } finally {
      flushing = false;
    }
  };

  const flush = Effect.sync(() => {
    if (!closed) {
      flushUnsafe();
    }
  }).pipe(Effect.withTracerEnabled(false));

  const retryFiber = yield* Effect.forkScoped(
    Effect.sleep(options.batchWindowMs).pipe(Effect.andThen(flush), Effect.forever),
  );
  const closeCleanup = Fiber.interrupt(retryFiber).pipe(
    Effect.andThen(Fiber.await(retryFiber)),
    Effect.andThen(Effect.sync(flushUnsafe)),
    Effect.asVoid,
  );
  const close = (): Effect.Effect<void> =>
    Effect.uninterruptible(
      Effect.suspend(() => {
        if (closeStarted) {
          return Deferred.await(closeCompletion);
        }
        closeStarted = true;
        closed = true;
        return Deferred.into(closeCleanup, closeCompletion).pipe(
          Effect.andThen(Deferred.await(closeCompletion)),
        );
      }),
    ).pipe(Effect.withTracerEnabled(false));
  yield* Effect.addFinalizer(() => close().pipe(Effect.ignore));

  const enqueue = (record: TraceRecord, prepared: boolean): void => {
    if (closed || terminal) {
      droppedRecords += 1;
      return;
    }
    const normalizedRecord = prepared ? record : normalizeJsonValue(record);
    const line = `${JSON.stringify(normalizedRecord)}\n`;
    const bytes = traceUtf8ByteLength(line);
    if (bytes > maxPendingBytes) {
      droppedRecords += 1;
      report({ event: "record-too-large" });
      return;
    }
    buffer.push({ line, bytes });
    pendingBytes += bytes;
    enforceBound();
    if (buffer.length >= FLUSH_BUFFER_THRESHOLD && consecutiveFailures === 0) {
      flushUnsafe();
    }
  };

  const traceSink = {
    filePath: options.filePath,
    push(record) {
      enqueue(record, false);
    },
    flush,
    close,
    stats,
  } satisfies TraceSink;
  preparedTracePushes.set(traceSink, (record) => enqueue(record, true));
  return traceSink;
});

class LocalFileSpan implements Tracer.Span {
  readonly _tag = "Span";
  readonly name: string;
  readonly spanId: string;
  readonly traceId: string;
  readonly parent: Option.Option<Tracer.AnySpan>;
  readonly annotations: Tracer.Span["annotations"];
  readonly links: Array<Tracer.SpanLink>;
  readonly sampled: boolean;
  readonly kind: Tracer.SpanKind;

  status: Tracer.SpanStatus;
  attributes: Map<string, unknown>;
  events: Array<[name: string, startTime: bigint, attributes: Record<string, unknown>]>;
  private readonly delegate: Tracer.Span;
  private readonly push: (record: EffectTraceRecord) => void;

  constructor(
    options: Parameters<Tracer.Tracer["span"]>[0],
    delegate: Tracer.Span,
    push: (record: EffectTraceRecord) => void,
  ) {
    this.delegate = delegate;
    this.push = push;
    this.name = delegate.name;
    this.spanId = delegate.spanId;
    this.traceId = delegate.traceId;
    this.parent = options.parent;
    this.annotations = options.annotations;
    this.links = [...options.links];
    this.sampled = delegate.sampled;
    this.kind = delegate.kind;
    this.status = {
      _tag: "Started",
      startTime: options.startTime,
    };
    this.attributes = new Map();
    this.events = [];
  }

  end(endTime: bigint, exit: Exit.Exit<unknown, unknown>): void {
    this.status = {
      _tag: "Ended",
      startTime: this.status.startTime,
      endTime,
      exit,
    };
    this.delegate.end(endTime, exit);

    if (this.sampled) {
      this.push(spanToTraceRecord(this));
    }
  }

  attribute(key: string, value: unknown): void {
    this.attributes.set(key, value);
    this.delegate.attribute(key, value);
  }

  event(name: string, startTime: bigint, attributes?: Record<string, unknown>): void {
    const nextAttributes = attributes ?? {};
    this.events.push([name, startTime, nextAttributes]);
    this.delegate.event(name, startTime, nextAttributes);
  }

  addLinks(links: ReadonlyArray<Tracer.SpanLink>): void {
    this.links.push(...links);
    this.delegate.addLinks(links);
  }
}

export const makeLocalFileTracer = Effect.fn("makeLocalFileTracer")(function* (
  options: LocalFileTracerOptions,
) {
  const sink = options.sink ?? (yield* makeTraceSink(options));
  const push = preparedTracePushes.get(sink) ?? sink.push;

  const delegate =
    options.delegate ??
    Tracer.make({
      span: (spanOptions) => new Tracer.NativeSpan(spanOptions),
    });

  return Tracer.make({
    span(spanOptions) {
      return new LocalFileSpan(spanOptions, delegate.span(spanOptions), push);
    },
    ...(delegate.context ? { context: delegate.context } : {}),
  });
});

const SPAN_KIND_MAP: Record<number, OtlpTraceRecord["kind"]> = {
  1: "internal",
  2: "server",
  3: "client",
  4: "producer",
  5: "consumer",
};

export function decodeOtlpTraceRecords(
  payload: OtlpTracer.TraceData,
): ReadonlyArray<OtlpTraceRecord> {
  const records: Array<OtlpTraceRecord> = [];

  for (const resourceSpan of payload.resourceSpans) {
    const resourceAttributes = decodeAttributes(resourceSpan.resource?.attributes ?? []);

    for (const scopeSpan of resourceSpan.scopeSpans) {
      for (const span of scopeSpan.spans) {
        records.push(
          otlpSpanToTraceRecord({
            resourceAttributes,
            scopeAttributes: decodeAttributes(
              "attributes" in scopeSpan.scope && Array.isArray(scopeSpan.scope.attributes)
                ? scopeSpan.scope.attributes
                : [],
            ),
            scopeName: scopeSpan.scope.name,
            scopeVersion:
              "version" in scopeSpan.scope && typeof scopeSpan.scope.version === "string"
                ? scopeSpan.scope.version
                : undefined,
            span,
          }),
        );
      }
    }
  }

  return records;
}

function otlpSpanToTraceRecord(input: {
  readonly resourceAttributes: Readonly<Record<string, unknown>>;
  readonly scopeAttributes: Readonly<Record<string, unknown>>;
  readonly scopeName: string | undefined;
  readonly scopeVersion: string | undefined;
  readonly span: OtlpSpan;
}): OtlpTraceRecord {
  return {
    type: "otlp-span",
    name: sanitizeTraceText(input.span.name),
    traceId: input.span.traceId,
    spanId: input.span.spanId,
    ...(input.span.parentSpanId ? { parentSpanId: input.span.parentSpanId } : {}),
    sampled: true,
    kind: normalizeSpanKind(input.span.kind),
    startTimeUnixNano: input.span.startTimeUnixNano,
    endTimeUnixNano: input.span.endTimeUnixNano,
    durationMs:
      Number(parseBigInt(input.span.endTimeUnixNano) - parseBigInt(input.span.startTimeUnixNano)) /
      1_000_000,
    attributes: decodeAttributes(input.span.attributes),
    resourceAttributes: input.resourceAttributes,
    scope: {
      ...(input.scopeName ? { name: sanitizeTraceText(input.scopeName) } : {}),
      ...(input.scopeVersion ? { version: sanitizeTraceText(input.scopeVersion) } : {}),
      attributes: input.scopeAttributes,
    },
    events: decodeEvents(input.span.events),
    links: decodeLinks(input.span.links),
    status: decodeStatus(input.span.status),
  };
}

function decodeStatus(input: OtlpSpanStatus): OtlpTraceRecord["status"] {
  const code = String(input.code);
  const message = input.message ? sanitizeTraceText(input.message) : input.message;

  return {
    code,
    ...(message ? { message } : {}),
  };
}

function decodeEvents(input: ReadonlyArray<OtlpSpanEvent>): ReadonlyArray<TraceRecordEvent> {
  return input.map((current) => ({
    name: sanitizeTraceText(current.name),
    timeUnixNano: current.timeUnixNano,
    attributes: decodeAttributes(current.attributes),
  }));
}

function decodeLinks(input: ReadonlyArray<OtlpSpanLink>): ReadonlyArray<TraceRecordLink> {
  return input.flatMap((current) => {
    const traceId = current.traceId;
    const spanId = current.spanId;
    return {
      traceId,
      spanId,
      attributes: decodeAttributes(current.attributes),
    };
  });
}

function decodeAttributes(
  input: ReadonlyArray<OtlpResource.KeyValue>,
): Readonly<Record<string, unknown>> {
  const entries: Record<string, unknown> = {};

  for (const attribute of input) {
    entries[attribute.key] = decodeValue(attribute.value);
  }

  return compactTraceAttributes(entries);
}

function decodeValue(input: OtlpResource.AnyValue | null | undefined): unknown {
  if (input == null) {
    return null;
  }
  if ("stringValue" in input) {
    return input.stringValue;
  }
  if ("boolValue" in input) {
    return input.boolValue;
  }
  if ("intValue" in input) {
    return input.intValue;
  }
  if ("doubleValue" in input) {
    return input.doubleValue;
  }
  if ("bytesValue" in input) {
    return input.bytesValue;
  }
  if (input.arrayValue) {
    return input.arrayValue.values.map((entry) => decodeValue(entry));
  }
  if (input.kvlistValue) {
    return decodeAttributes(input.kvlistValue.values);
  }
  return null;
}

function normalizeSpanKind(input: number): OtlpTraceRecord["kind"] {
  return SPAN_KIND_MAP[input] || "internal";
}

function parseBigInt(input: string): bigint {
  try {
    return BigInt(input);
  } catch {
    return 0n;
  }
}
