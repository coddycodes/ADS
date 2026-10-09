import {
  type ProfilerClientBatch,
  PROFILER_CLIENT_BATCH_LIMITS,
  WS_METHODS,
} from "@bibcode/contracts";

import { randomHex } from "../lib/utils";

/** The server joins a request to its own timing by this header (`CORRELATION_HEADER`). */
export const PROFILER_CORRELATION_HEADER = "x-bibcode-profiler-rid";
/** Must match `BUCKET_GROWTH` and `BUCKETS` in the server's `latency.rs`. */
const BUCKET_GROWTH = 1.12;
const BUCKETS = 100;
/** Requests slower than this are also sent individually, for the server to join. */
const RECORD_OVER_MS = 250;
const STALL_OVER_MS = 50;
const HEARTBEAT_MS = 500;
/**
 * A heartbeat this overdue means the machine was suspended, not that the main thread was busy.
 * On Windows `performance.now()` keeps counting through a sleep, and nothing else says so.
 */
const SUSPENDED_AFTER_MS = 60_000;
const FLUSH_EVERY_MS = 10_000;
/** The server's caps: a longer sample spans a sleep, and a longer window is rejected. */
const MAX_SAMPLE_MS = 3_600_000;
const MAX_WINDOW_MS = 600_000;
const INTERACTION_THRESHOLD_MS = 104;
/** Answers that mean this environment will never accept reports in this session. */
const REFUSALS = new Set(["EnvironmentAuthorizationError", "EnvironmentOperationForbiddenError"]);

export interface ClientCollector {
  observeRequest(
    environmentId: string,
    method: string,
  ): { correlationId: string | null; complete: () => void };
  setEnabledEnvironments(ids: ReadonlySet<string>): void;
  flush(): Promise<void>;
  dispose(): void;
}

export interface ClientCollectorDeps {
  readonly now: () => number;
  readonly send: (environmentId: string, batch: ProfilerClientBatch) => Promise<unknown>;
  readonly supportedEntryTypes: readonly string[];
  readonly clientId: string;
  /** Defaults to `requestAnimationFrame`. */
  readonly raf?: (callback: () => void) => unknown;
}

interface PendingRecord {
  readonly correlationId: string;
  readonly method: string;
  readonly rttMs: number;
  readonly startedAtMs: number;
  renderMs: number | null;
  /** The render measurement has finished, or was abandoned; until then it is not sent. */
  rendered: boolean;
}

interface EnvironmentWindow {
  readonly records: PendingRecord[];
  /** Method → bucket → count. */
  readonly histograms: Map<string, Map<number, number>>;
}

/** `interactionId` is missing on engines that predate it. */
interface SlowEvent {
  readonly duration: number;
  readonly interactionId?: number;
}

const UNOBSERVED = { correlationId: null, complete: () => undefined };

/** Log-scale bucket, identical to the server's `bucket_of`. */
export function bucketOf(ms: number): number {
  if (!(ms > 1)) return 0;
  return Math.min(Math.floor(Math.log(ms) / Math.log(BUCKET_GROWTH)), BUCKETS - 1);
}

function isHidden(): boolean {
  return typeof document !== "undefined" && document.visibilityState === "hidden";
}

function jsHeapBytes(): number | null {
  const memory = (performance as { memory?: { usedJSHeapSize?: number } }).memory;
  return memory?.usedJSHeapSize ?? null;
}

function isRefusal(error: unknown): boolean {
  const tag = (error as { _tag?: unknown } | null)?._tag;
  return typeof tag === "string" && REFUSALS.has(tag);
}

/**
 * Measures what the user waits for (Decision 8): per-request round trip and render time,
 * main-thread stalls and slow interactions, sent to each environment whose profiler is on.
 * Timers, listeners and observers exist only while at least one environment is enabled, and
 * every failure is swallowed: profiling never surfaces an error.
 */
export function createClientCollector(deps: ClientCollectorDeps): ClientCollector {
  const { now, send, clientId } = deps;
  const raf =
    deps.raf ??
    (typeof requestAnimationFrame === "function"
      ? (callback: () => void) => requestAnimationFrame(callback)
      : null);
  const longFrames = deps.supportedEntryTypes.includes("long-animation-frame");
  const eventTiming = deps.supportedEntryTypes.includes("event");
  const capabilities = [
    longFrames ? "long-animation-frame" : "heartbeat",
    ...(eventTiming ? ["event"] : []),
    ...(jsHeapBytes() === null ? [] : ["memory"]),
  ];

  /** The enabled environments, each with the window being collected for it. */
  const windows = new Map<string, EnvironmentWindow>();
  const refused = new Set<string>();
  /** Environments with a batch on the wire: one at a time, so a slow server cannot pile up. */
  const uploading = new Set<string>();
  let counter = 0;
  /**
   * Bumped whenever observation was interrupted (a visibility change, a suspend, a stop): a
   * sample that saw it changed spans the gap and is dropped.
   */
  let epoch = 0;
  let windowStart = 0;
  let stalls = { count: 0, totalMs: 0, maxMs: 0 };
  let interactions = { count: 0, maxMs: 0 };
  let lastInteractionId = 0;
  let running = false;
  let flushTimer: ReturnType<typeof setInterval> | undefined;
  let heartbeat: ReturnType<typeof setTimeout> | undefined;
  let expectedBeat = 0;
  let observers: PerformanceObserver[] = [];

  const addStall = (ms: number) => {
    stalls.count += 1;
    stalls.totalMs += ms;
    stalls.maxMs = Math.max(stalls.maxMs, ms);
  };

  // The heartbeat runs in both modes: it is the stall signal without Long Animation Frames,
  // and the suspend signal everywhere.
  const scheduleBeat = () => {
    expectedBeat = now() + HEARTBEAT_MS;
    heartbeat = setTimeout(beat, HEARTBEAT_MS);
  };
  const beat = () => {
    const lateness = now() - expectedBeat;
    if (lateness >= SUSPENDED_AFTER_MS) epoch += 1;
    else if (!longFrames && lateness > STALL_OVER_MS) addStall(lateness);
    scheduleBeat();
  };
  const pauseBeat = () => {
    clearTimeout(heartbeat);
    heartbeat = undefined;
  };
  /** Whether the machine just woke and the heartbeat has not run since. */
  const suspended = () => heartbeat !== undefined && now() - expectedBeat >= SUSPENDED_AFTER_MS;

  const onVisibilityChange = () => {
    epoch += 1;
    if (isHidden()) pauseBeat();
    else if (heartbeat === undefined) scheduleBeat();
  };

  const observe = <E>(init: PerformanceObserverInit, onEntry: (entry: E) => void) => {
    try {
      const observer = new PerformanceObserver((list) => {
        for (const entry of list.getEntries()) onEntry(entry as E);
      });
      observer.observe(init);
      observers.push(observer);
    } catch {
      // An engine that lists the type but refuses the observer measures nothing.
    }
  };

  const start = () => {
    if (running) return;
    running = true;
    windowStart = now();
    stalls = { count: 0, totalMs: 0, maxMs: 0 };
    interactions = { count: 0, maxMs: 0 };
    flushTimer = setInterval(() => void flush(), FLUSH_EVERY_MS);
    if (typeof document !== "undefined") {
      document.addEventListener("visibilitychange", onVisibilityChange);
    }
    if (!isHidden()) scheduleBeat();
    if (longFrames) {
      observe<PerformanceEntry>({ type: "long-animation-frame" }, (entry) =>
        addStall(entry.duration),
      );
    }
    if (eventTiming) {
      const init = { type: "event", durationThreshold: INTERACTION_THRESHOLD_MS };
      observe<SlowEvent>(init as PerformanceObserverInit, (entry) => {
        // 0 marks an event that is not part of an interaction (hover, scroll); older engines
        // have no `interactionId`, and every slow event counts there.
        if (entry.interactionId === 0) return;
        if (entry.interactionId === undefined || entry.interactionId !== lastInteractionId) {
          interactions.count += 1;
          lastInteractionId = entry.interactionId ?? 0;
        }
        interactions.maxMs = Math.max(interactions.maxMs, entry.duration);
      });
    }
  };

  const stop = () => {
    if (!running) return;
    running = false;
    // A request in flight across a stop and a restart is not measured.
    epoch += 1;
    clearInterval(flushTimer);
    pauseBeat();
    if (typeof document !== "undefined") {
      document.removeEventListener("visibilitychange", onVisibilityChange);
    }
    for (const observer of observers) observer.disconnect();
    observers = [];
  };

  const record = (window: EnvironmentWindow, method: string, rttMs: number) => {
    let buckets = window.histograms.get(method);
    if (buckets === undefined) {
      if (window.histograms.size >= PROFILER_CLIENT_BATCH_LIMITS.histograms) return;
      buckets = new Map();
      window.histograms.set(method, buckets);
    }
    const bucket = bucketOf(rttMs);
    buckets.set(bucket, (buckets.get(bucket) ?? 0) + 1);
  };

  const measureRender = (pending: PendingRecord) => {
    if (raf === null) {
      pending.rendered = true;
      return;
    }
    const receivedAt = now();
    const seen = epoch;
    // The second frame callback runs after the frame that painted the response.
    raf(() =>
      raf(() => {
        const renderMs = now() - receivedAt;
        if (seen === epoch && !suspended() && renderMs <= MAX_SAMPLE_MS) {
          pending.renderMs = renderMs;
        }
        pending.rendered = true;
      }),
    );
  };

  const report = async (environmentId: string, batch: ProfilerClientBatch) => {
    uploading.add(environmentId);
    try {
      await send(environmentId, batch);
    } catch (error) {
      if (isRefusal(error)) {
        refused.add(environmentId);
        windows.delete(environmentId);
        if (windows.size === 0) stop();
      }
    } finally {
      uploading.delete(environmentId);
    }
  };

  const flush = async () => {
    if (windows.size === 0) return;
    const at = now();
    const windowMs = Math.min(Math.max(at - windowStart, 0), MAX_WINDOW_MS);
    windowStart = at;
    // Capped like every duration, so one pathological window cannot get a batch rejected.
    const clientWide = {
      stalls: {
        count: stalls.count,
        totalMs: Math.min(stalls.totalMs, MAX_SAMPLE_MS),
        maxMs: Math.min(stalls.maxMs, MAX_SAMPLE_MS),
      },
      interactions: {
        count: interactions.count,
        maxMs: Math.min(interactions.maxMs, MAX_SAMPLE_MS),
      },
    };
    stalls = { count: 0, totalMs: 0, maxMs: 0 };
    interactions = { count: 0, maxMs: 0 };
    const quiet = clientWide.stalls.count === 0 && clientWide.interactions.count === 0;
    const heap = jsHeapBytes();
    const reports: Promise<void>[] = [];
    for (const [environmentId, window] of windows) {
      // A record still waiting for its frame goes out with a later batch.
      const waiting = window.records.filter((pending) => !pending.rendered);
      windows.set(environmentId, { records: waiting, histograms: new Map() });
      // While the previous batch is on the wire, this window is dropped.
      if (uploading.has(environmentId)) continue;
      const records = window.records.filter((pending) => pending.rendered);
      if (quiet && records.length === 0 && window.histograms.size === 0) continue;
      reports.push(
        report(environmentId, {
          clientId,
          capabilities,
          windowMs,
          records: records.map(({ correlationId, method, rttMs, renderMs, startedAtMs }) => ({
            correlationId,
            method,
            rttMs,
            renderMs,
            startedAtMs,
          })),
          histograms: Array.from(window.histograms, ([method, buckets]) => ({
            method,
            buckets: Array.from(buckets),
          })),
          stalls: clientWide.stalls,
          interactions: clientWide.interactions,
          jsHeapBytes: heap,
        }),
      );
    }
    await Promise.all(reports);
  };

  return {
    observeRequest(environmentId, method) {
      if (!windows.has(environmentId) || method === WS_METHODS.serverReportProfilerClientBatch) {
        return UNOBSERVED;
      }
      counter += 1;
      const correlationId = `${clientId}:${counter}`;
      const startedAtMs = now();
      const seen = isHidden() ? -1 : epoch;
      let completed = false;
      return {
        correlationId,
        complete: () => {
          if (completed) return;
          completed = true;
          const rttMs = now() - startedAtMs;
          const window = windows.get(environmentId);
          if (window === undefined || seen !== epoch || suspended()) return;
          if (!(rttMs >= 0 && rttMs <= MAX_SAMPLE_MS)) return;
          record(window, method, rttMs);
          if (rttMs <= RECORD_OVER_MS) return;
          if (window.records.length >= PROFILER_CLIENT_BATCH_LIMITS.records) return;
          const pending: PendingRecord = {
            correlationId,
            method,
            rttMs,
            startedAtMs,
            renderMs: null,
            rendered: false,
          };
          window.records.push(pending);
          measureRender(pending);
        },
      };
    },
    setEnabledEnvironments(ids) {
      for (const environmentId of windows.keys()) {
        if (ids.has(environmentId)) continue;
        windows.delete(environmentId);
        // Its requests in flight must not count once it is enabled again. Other
        // environments' requests in flight are dropped too; toggling is rare.
        epoch += 1;
      }
      for (const environmentId of ids) {
        if (!refused.has(environmentId) && !windows.has(environmentId)) {
          windows.set(environmentId, { records: [], histograms: new Map() });
        }
      }
      if (windows.size > 0) start();
      else stop();
    },
    flush,
    dispose() {
      windows.clear();
      stop();
    },
  };
}

let sender: ClientCollectorDeps["send"] | null = null;

/** Connects the app collector to the RPC that delivers its batches; `null` disconnects it. */
export function setClientBatchSender(next: ClientCollectorDeps["send"] | null): void {
  sender = next;
}

/** The app's collector. `startClientCollector` connects it to the environments' settings. */
export const clientCollector = createClientCollector({
  now: () => performance.now(),
  send: (environmentId, batch) =>
    sender === null ? Promise.resolve() : sender(environmentId, batch),
  supportedEntryTypes:
    typeof PerformanceObserver === "undefined" ? [] : PerformanceObserver.supportedEntryTypes,
  clientId: randomHex(8),
});
