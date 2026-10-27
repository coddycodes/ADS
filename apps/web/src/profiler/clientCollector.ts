import {
  type ProfilerClientBatch,
  PROFILER_CLIENT_BATCH_LIMITS,
  WS_METHODS,
} from "@coddy/contracts";

import { APP_VERSION } from "../branding";
import { randomHex } from "../lib/utils";
import { isProfilerUnsupported, profilerRefused, rememberProfilerRefused } from "./profilerSupport";

/** The server joins a request to its own timing by this header (`CORRELATION_HEADER`). */
export const PROFILER_CORRELATION_HEADER = "x-coddy-studio-profiler-rid";
/** Must match `BUCKET_GROWTH` and `BUCKETS` in the server's `latency.rs`. */
const BUCKET_GROWTH = 1.12;
const BUCKETS = 100;
/** Requests slower than this are also sent individually, for the server to join. */
const RECORD_OVER_MS = 250;
const STALL_OVER_MS = 50;
const HEARTBEAT_MS = 500;
/**
 * The wall clock ran this much further than the monotonic one, so the machine slept: the
 * monotonic clock (`performance.now()`) stops during a suspend on every platform but Windows.
 */
const SUSPEND_DIVERGENCE_MS = 5_000;
/**
 * On Windows the monotonic clock counts a sleep too, so a heartbeat this overdue is the only
 * sign of one. It is used only with Long Animation Frames (WebView2), where the heartbeat counts
 * no stalls and so hides no hang; elsewhere a late heartbeat is a stall, however long. The
 * server's latency fence makes the same split (`SUSPEND_GAP_MS`).
 */
const OVERDUE_HEARTBEAT_MS = 60_000;
/**
 * After a sleep, the system catches up for a while (late ticks of over 1 s on macOS once the
 * display comes back, #99): the heartbeat counts no stalls this long after waking.
 */
const RESUME_GRACE_MS = 20_000;
const FLUSH_EVERY_MS = 10_000;
/** The server's caps: a longer sample spans a sleep, and a longer window is rejected. */
const MAX_SAMPLE_MS = 3_600_000;
const MAX_WINDOW_MS = 600_000;
const INTERACTION_THRESHOLD_MS = 104;
/**
 * Answers that mean this environment will never accept reports in this session, besides an
 * older server not knowing the method at all.
 */
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
  /** Wall-clock time, compared with `now` to tell a suspend; defaults to `Date.now`. */
  readonly wallNow?: () => number;
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
  /** Which enabling of the environment this is: a request from an earlier one is dropped. */
  readonly recording: number;
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
  if (isProfilerUnsupported(error)) return true;
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
  const wallNow = deps.wallNow ?? Date.now;
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
  let recordings = 0;
  /**
   * Bumped when the window goes to or comes back from the background, and by a suspend the
   * heartbeat saw: a sample that saw it change spans the gap and is dropped.
   */
  let epoch = 0;
  /** The heartbeat counts no stalls before this, while the system catches up from a sleep. */
  let quietUntil = 0;
  let pausedAt = 0;
  let pausedAtWall = 0;
  let windowStart = 0;
  let stalls = { count: 0, totalMs: 0, maxMs: 0 };
  let interactions = { count: 0, maxMs: 0 };
  let lastInteractionId = 0;
  let running = false;
  let flushTimer: ReturnType<typeof setInterval> | undefined;
  let heartbeat: ReturnType<typeof setTimeout> | undefined;
  let beatScheduledAt = 0;
  let beatScheduledAtWall = 0;
  let expectedBeat = 0;
  let observers: PerformanceObserver[] = [];

  const addStall = (ms: number) => {
    stalls.count += 1;
    stalls.totalMs += ms;
    stalls.maxMs = Math.max(stalls.maxMs, ms);
  };

  /**
   * Hidden, or, where the heartbeat is the stall signal (WebKit), without focus: WebKit keeps a
   * covered window, or one on another Space, "visible" while macOS App Nap aligns its timers to
   * about 1 s (#99), and every tick would read as a stall. Its rendering waits too.
   */
  const inBackground = () =>
    isHidden() ||
    (!longFrames &&
      typeof document !== "undefined" &&
      typeof document.hasFocus === "function" &&
      !document.hasFocus());

  /** Whether the machine slept since `at` (monotonic) and `wallAt`; see `SUSPEND_DIVERGENCE_MS`. */
  const slept = (at: number, wallAt: number) =>
    wallNow() - wallAt - (now() - at) > SUSPEND_DIVERGENCE_MS;
  /** Whether a Windows sleep just ended and the heartbeat has not run since. */
  const overdue = () =>
    longFrames && heartbeat !== undefined && now() - expectedBeat >= OVERDUE_HEARTBEAT_MS;

  // The heartbeat runs in both modes: it is the stall signal without Long Animation Frames,
  // and a suspend signal everywhere.
  const scheduleBeat = () => {
    beatScheduledAt = now();
    beatScheduledAtWall = wallNow();
    expectedBeat = beatScheduledAt + HEARTBEAT_MS;
    heartbeat = setTimeout(beat, HEARTBEAT_MS);
  };
  const beat = () => {
    const lateness = now() - expectedBeat;
    if (slept(beatScheduledAt, beatScheduledAtWall) || overdue()) {
      epoch += 1;
      quietUntil = now() + RESUME_GRACE_MS;
    } else if (!longFrames && lateness > STALL_OVER_MS && now() >= quietUntil) {
      addStall(lateness);
    }
    scheduleBeat();
  };
  const pauseBeat = () => {
    if (heartbeat === undefined) return;
    clearTimeout(heartbeat);
    heartbeat = undefined;
    pausedAt = now();
    pausedAtWall = wallNow();
  };
  const resumeBeat = () => {
    if (heartbeat !== undefined) return;
    // The heartbeat could not see a sleep while it was paused.
    if (slept(pausedAt, pausedAtWall)) quietUntil = now() + RESUME_GRACE_MS;
    scheduleBeat();
  };

  const onForegroundChange = () => {
    epoch += 1;
    if (inBackground()) pauseBeat();
    else resumeBeat();
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
      document.addEventListener("visibilitychange", onForegroundChange);
    }
    // A window that cannot report focus (an embedding, a test) is treated as focused.
    if (
      !longFrames &&
      typeof window !== "undefined" &&
      typeof window.addEventListener === "function"
    ) {
      window.addEventListener("focus", onForegroundChange);
      window.addEventListener("blur", onForegroundChange);
    }
    // Started in the background, the heartbeat counts as paused from now.
    pausedAt = now();
    pausedAtWall = wallNow();
    if (!inBackground()) scheduleBeat();
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
    clearInterval(flushTimer);
    pauseBeat();
    if (typeof document !== "undefined") {
      document.removeEventListener("visibilitychange", onForegroundChange);
    }
    if (typeof window !== "undefined" && typeof window.removeEventListener === "function") {
      window.removeEventListener("focus", onForegroundChange);
      window.removeEventListener("blur", onForegroundChange);
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
    const receivedAtWall = wallNow();
    const seen = epoch;
    // The second frame callback runs after the frame that painted the response.
    raf(() =>
      raf(() => {
        const renderMs = now() - receivedAt;
        const measured = seen === epoch && !slept(receivedAt, receivedAtWall) && !overdue();
        if (measured && renderMs <= MAX_SAMPLE_MS) pending.renderMs = renderMs;
        pending.rendered = true;
      }),
    );
  };

  const report = async (environmentId: string, batch: ProfilerClientBatch) => {
    uploading.add(environmentId);
    try {
      await send(environmentId, batch);
    } catch (error) {
      if (isProfilerUnsupported(error)) rememberProfilerRefused(environmentId);
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
      // While the previous batch is on the wire, the window's records wait for the next one
      // (still at most 64, the cap `complete` keeps) and the rest of the window is dropped.
      const onTheWire = uploading.has(environmentId);
      // A record still waiting for its frame goes out with a later batch.
      const waiting = onTheWire
        ? window.records
        : window.records.filter((pending) => !pending.rendered);
      windows.set(environmentId, {
        recording: window.recording,
        records: waiting,
        histograms: new Map(),
      });
      if (onTheWire) continue;
      // The Diagnostics page may have seen the environment refuse the profiler first.
      if (profilerRefused(environmentId)) {
        refused.add(environmentId);
        windows.delete(environmentId);
        continue;
      }
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
          appVersion: APP_VERSION,
        }),
      );
    }
    if (windows.size === 0) stop();
    await Promise.all(reports);
  };

  return {
    observeRequest(environmentId, method) {
      const recording = windows.get(environmentId)?.recording;
      if (recording === undefined || method === WS_METHODS.serverReportProfilerClientBatch) {
        return UNOBSERVED;
      }
      counter += 1;
      const correlationId = `${clientId}:${counter}`;
      const startedAtMs = now();
      const startedAtWall = wallNow();
      const seen = inBackground() ? -1 : epoch;
      let completed = false;
      return {
        correlationId,
        complete: () => {
          if (completed) return;
          completed = true;
          const rttMs = now() - startedAtMs;
          const window = windows.get(environmentId);
          // Measured only within one recording of its environment, while visible and awake.
          if (window?.recording !== recording || seen !== epoch) return;
          if (slept(startedAtMs, startedAtWall) || overdue()) return;
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
        if (!ids.has(environmentId)) windows.delete(environmentId);
      }
      for (const environmentId of ids) {
        if (!refused.has(environmentId) && !windows.has(environmentId)) {
          // A new recording: its requests in flight from before stay unmeasured.
          recordings += 1;
          windows.set(environmentId, { recording: recordings, records: [], histograms: new Map() });
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
