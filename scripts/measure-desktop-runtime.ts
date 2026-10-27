#!/usr/bin/env node

// @effect-diagnostics nodeBuiltinImport:off
import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import * as NodePerfHooks from "node:perf_hooks";
import * as NodeTimersPromises from "node:timers/promises";
import * as NodeUtil from "node:util";

export interface RuntimeProcessRow {
  readonly pid: number;
  readonly ppid: number;
  readonly processName: string;
  readonly command: string;
  readonly privateBytes: number;
  readonly privateBytesMetric?: "private" | "rss-approximation";
  readonly workingSetBytes: number;
  readonly mainWindowTitle: string;
  readonly startedAtEpochMs?: number;
}

export interface RuntimeProcessSummary {
  readonly pid: number;
  readonly processName: string;
  readonly command: string;
  readonly privateBytes: number;
  readonly workingSetBytes: number;
  readonly mainWindowTitle: string;
}

export interface RuntimeMeasurementInput {
  readonly label: string;
  readonly command: string;
  readonly args: ReadonlyArray<string>;
  readonly cwd?: string;
  readonly readyUrl?: string;
  readonly windowTitle?: string;
  readonly idleMs: number;
  readonly timeoutMs: number;
  readonly env: Readonly<Record<string, string>>;
  readonly keepRunning: boolean;
  readonly jsonOut?: string;
  readonly markdownOut?: string;
  readonly verbose?: boolean;
}

export interface RuntimeMeasurementSummary {
  readonly label: string;
  readonly rootPid: number;
  readonly sampledAt: string;
  readonly startupMs: number | null;
  readonly windowReadyMs: number | null;
  readonly readyUrl: string | null;
  readonly windowTitle: string | null;
  readonly idleMs: number;
  readonly processIds: ReadonlyArray<number>;
  readonly processCount: number;
  readonly totalPrivateBytes: number;
  readonly privateBytesMetric: "private" | "rss-approximation";
  readonly totalWorkingSetBytes: number;
  readonly topProcesses: ReadonlyArray<RuntimeProcessSummary>;
}

export class DesktopRuntimeMeasurementError extends Error {
  override readonly name = "DesktopRuntimeMeasurementError";
}

export interface LaunchedChildProcess {
  readonly pid: number;
  readonly killed: boolean;
  readonly identity: RuntimeProcessIdentity;
  readonly termination: Promise<never>;
  dispose(): void;
  unref(): void;
}

export interface RuntimeProcessIdentity {
  readonly pid: number;
  readonly processGroupId?: number;
  readonly startedAtEpochMs: number;
  readonly windowsOwnedProcessStartTimes?: Map<number, number>;
}

interface SpawnedChildProcess {
  readonly pid: number | undefined;
  readonly killed: boolean;
  kill(signal?: NodeJS.Signals): boolean;
  unref(): void;
  once(event: "error", listener: (cause: unknown) => void): this;
  on(event: "error", listener: (cause: unknown) => void): this;
  on(
    event: "exit" | "close",
    listener: (code: number | null, signal: NodeJS.Signals | null) => void,
  ): this;
  off(event: "error", listener: (cause: unknown) => void): this;
  off(
    event: "exit" | "close",
    listener: (code: number | null, signal: NodeJS.Signals | null) => void,
  ): this;
}

export interface RuntimeSpawnDependencies {
  readonly platform: NodeJS.Platform;
  readonly epochNow: () => number;
  readonly spawn: (
    command: string,
    args: ReadonlyArray<string>,
    options: NodeChildProcess.SpawnOptions,
  ) => SpawnedChildProcess;
  readonly readProcessRows?:
    | ((signal: AbortSignal) => Promise<ReadonlyArray<RuntimeProcessRow>>)
    | undefined;
}

export interface ReadyUrlDependencies {
  readonly now: () => number;
  readonly sleep: (ms: number, signal?: AbortSignal) => Promise<void>;
  readonly fetch: (
    url: string,
    options: { readonly signal: AbortSignal },
  ) => Promise<{
    readonly status: number;
    readonly body?: { readonly cancel: () => Promise<void> } | null;
  }>;
  readonly scheduleAbort: (abort: () => void, delayMs: number) => unknown;
  readonly clearAbort: (handle: unknown) => void;
}

export interface WindowTitleDependencies {
  readonly now: () => number;
  readonly sleep: (ms: number, signal?: AbortSignal) => Promise<void>;
  readonly readProcessRows: (signal: AbortSignal) => Promise<ReadonlyArray<RuntimeProcessRow>>;
}

export interface RuntimeCleanupDependencies {
  readonly platform: NodeJS.Platform;
  readonly execFile: (
    command: string,
    args: ReadonlyArray<string>,
    options: {
      readonly encoding: "utf8";
      readonly stdio: "ignore";
      readonly timeout: number;
    },
  ) => unknown;
  readonly kill: (pid: number, signal: NodeJS.Signals | 0) => void;
  readonly readProcessRows: (signal: AbortSignal) => Promise<ReadonlyArray<RuntimeProcessRow>>;
  readonly sleep: (ms: number) => Promise<void>;
}

export interface RuntimeMeasurementHost {
  readonly spawn: (input: RuntimeMeasurementInput) => Promise<LaunchedChildProcess>;
  readonly monitorOwnership?:
    | ((identity: RuntimeProcessIdentity, signal: AbortSignal) => Promise<void>)
    | undefined;
  readonly waitForReadyUrl: (
    url: string,
    timeoutMs: number,
    signal: AbortSignal,
  ) => Promise<number>;
  readonly waitForWindowTitle: (
    rootPid: number,
    titleNeedle: string,
    timeoutMs: number,
    signal: AbortSignal,
  ) => Promise<number>;
  readonly sleep: (ms: number, signal?: AbortSignal) => Promise<void>;
  readonly now: () => number;
  readonly sampledAt: () => string;
  readonly readProcessRows: (signal: AbortSignal) => Promise<ReadonlyArray<RuntimeProcessRow>>;
  readonly cleanup: (identity: RuntimeProcessIdentity) => Promise<void>;
  readonly writeTextFile: (filePath: string, text: string) => void;
  readonly stdout: (text: string) => void;
  readonly stderr: (text: string) => void;
}

const WINDOWS_PROCESS_SAMPLER_TIMEOUT_MS = 15_000;
const WINDOWS_PROCESS_CLEANUP_TIMEOUT_MS = 15_000;
const OWNERSHIP_SAMPLE_INTERVAL_MS = 100;

function normalizeRepeatedArgFlags(argv: ReadonlyArray<string>): ReadonlyArray<string> {
  const normalized: string[] = [];
  for (let index = 0; index < argv.length; index += 1) {
    const entry = argv[index];
    if (entry === "--arg") {
      const value = argv[index + 1];
      if (value === undefined) {
        throw new DesktopRuntimeMeasurementError("--arg requires a value.");
      }
      normalized.push(`--arg=${value}`);
      index += 1;
    } else {
      normalized.push(entry!);
    }
  }
  return normalized;
}

function toOptionalString(value: unknown): string | undefined {
  return typeof value === "string" && value.length > 0 ? value : undefined;
}

function toStringArray(value: unknown): ReadonlyArray<string> {
  if (Array.isArray(value))
    return value.filter((entry): entry is string => typeof entry === "string");
  return [];
}

function parsePositiveInteger(value: string | undefined, fallback: number, label: string): number {
  if (value === undefined) return fallback;
  const parsed = Number(value);
  if (!Number.isInteger(parsed) || parsed <= 0) {
    throw new DesktopRuntimeMeasurementError(`${label} must be a positive integer.`);
  }
  return parsed;
}

function parseEnvEntries(entries: ReadonlyArray<string>): Record<string, string> {
  const env: Record<string, string> = {};
  for (const entry of entries) {
    const separator = entry.indexOf("=");
    if (separator <= 0) {
      throw new DesktopRuntimeMeasurementError(
        `Invalid --env entry '${entry}'. Expected KEY=VALUE.`,
      );
    }
    env[entry.slice(0, separator)] = entry.slice(separator + 1);
  }
  return env;
}

export function parseMeasureDesktopRuntimeArgs(
  argv: ReadonlyArray<string>,
): RuntimeMeasurementInput {
  const { values } = NodeUtil.parseArgs({
    args: [...normalizeRepeatedArgFlags(argv)],
    options: {
      label: { type: "string" },
      command: { type: "string" },
      arg: { type: "string", multiple: true },
      cwd: { type: "string" },
      "ready-url": { type: "string" },
      "window-title": { type: "string" },
      "idle-ms": { type: "string" },
      "timeout-ms": { type: "string" },
      env: { type: "string", multiple: true },
      "keep-running": { type: "boolean" },
      "json-out": { type: "string" },
      "markdown-out": { type: "string" },
      verbose: { type: "boolean" },
    },
    allowPositionals: false,
  });

  const label = toOptionalString(values.label);
  const command = toOptionalString(values.command);
  if (!label) {
    throw new DesktopRuntimeMeasurementError("--label is required.");
  }
  if (!command) {
    throw new DesktopRuntimeMeasurementError("--command is required.");
  }

  return {
    label,
    command,
    args: toStringArray(values.arg),
    ...(toOptionalString(values.cwd) ? { cwd: String(values.cwd) } : {}),
    ...(toOptionalString(values["ready-url"]) ? { readyUrl: String(values["ready-url"]) } : {}),
    ...(toOptionalString(values["window-title"])
      ? { windowTitle: String(values["window-title"]) }
      : {}),
    idleMs: parsePositiveInteger(toOptionalString(values["idle-ms"]), 30_000, "--idle-ms"),
    timeoutMs: parsePositiveInteger(
      toOptionalString(values["timeout-ms"]),
      120_000,
      "--timeout-ms",
    ),
    env: parseEnvEntries(toStringArray(values.env)),
    keepRunning: values["keep-running"] === true,
    ...(toOptionalString(values["json-out"]) ? { jsonOut: String(values["json-out"]) } : {}),
    ...(toOptionalString(values["markdown-out"])
      ? { markdownOut: String(values["markdown-out"]) }
      : {}),
    ...(values.verbose === true ? { verbose: true } : {}),
  };
}

export function collectProcessTree(
  rows: ReadonlyArray<RuntimeProcessRow>,
  rootPid: number,
): ReadonlyArray<RuntimeProcessRow> {
  const rowByPid = new Map(rows.map((row) => [row.pid, row]));
  const rootStartedAtEpochMs = rowByPid.get(rootPid)?.startedAtEpochMs;
  const childrenByParent = new Map<number, RuntimeProcessRow[]>();
  for (const row of rows) {
    const bucket = childrenByParent.get(row.ppid) ?? [];
    bucket.push(row);
    childrenByParent.set(row.ppid, bucket);
  }

  const discovered = new Set<number>([rootPid]);
  const queue = [rootPid];
  for (let index = 0; index < queue.length; index += 1) {
    const pid = queue[index]!;
    for (const child of childrenByParent.get(pid) ?? []) {
      if (discovered.has(child.pid)) continue;
      if (
        rootStartedAtEpochMs !== undefined &&
        child.startedAtEpochMs !== undefined &&
        child.startedAtEpochMs + 1_000 < rootStartedAtEpochMs
      ) {
        continue;
      }
      discovered.add(child.pid);
      queue.push(child.pid);
    }
  }

  return rows.filter((row) => discovered.has(row.pid));
}

export function summarizeProcessTree(input: {
  readonly label: string;
  readonly rootPid: number;
  readonly rows: ReadonlyArray<RuntimeProcessRow>;
  readonly sampledAt: string;
  readonly startupMs?: number | null;
  readonly windowReadyMs?: number | null;
  readonly readyUrl?: string | null;
  readonly windowTitle?: string | null;
  readonly idleMs: number;
}): RuntimeMeasurementSummary {
  const tree = collectProcessTree(input.rows, input.rootPid);
  const topProcesses = tree
    .map((row) => ({
      pid: row.pid,
      processName: row.processName,
      command: row.command,
      privateBytes: row.privateBytes,
      workingSetBytes: row.workingSetBytes,
      mainWindowTitle: row.mainWindowTitle,
    }))
    .toSorted((a, b) => b.privateBytes - a.privateBytes)
    .slice(0, 12);

  return {
    label: input.label,
    rootPid: input.rootPid,
    sampledAt: input.sampledAt,
    startupMs: input.startupMs ?? null,
    windowReadyMs: input.windowReadyMs ?? null,
    readyUrl: input.readyUrl ?? null,
    windowTitle: input.windowTitle ?? null,
    idleMs: input.idleMs,
    processIds: tree.map((row) => row.pid),
    processCount: tree.length,
    totalPrivateBytes: tree.reduce((total, row) => total + row.privateBytes, 0),
    privateBytesMetric: tree.some((row) => row.privateBytesMetric === "rss-approximation")
      ? "rss-approximation"
      : "private",
    totalWorkingSetBytes: tree.reduce((total, row) => total + row.workingSetBytes, 0),
    topProcesses,
  };
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} bytes`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MiB`;
  return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GiB`;
}

function formatDuration(ms: number | null): string {
  return ms === null ? "not captured" : `${(ms / 1000).toFixed(2)} s`;
}

export function renderMeasurementMarkdown(summary: RuntimeMeasurementSummary): string {
  const processMemoryLabel =
    summary.privateBytesMetric === "rss-approximation" ? "RSS (approx.)" : "Private";
  const rows = [
    ["Label", summary.label],
    ["Sampled at", summary.sampledAt],
    ["Root PID", String(summary.rootPid)],
    ["Startup readiness", formatDuration(summary.startupMs)],
    ["Window readiness", formatDuration(summary.windowReadyMs)],
    ["Idle delay", formatDuration(summary.idleMs)],
    ["Process count", String(summary.processCount)],
    [
      summary.privateBytesMetric === "rss-approximation"
        ? "Total private bytes (RSS approximation)"
        : "Total private bytes",
      formatBytes(summary.totalPrivateBytes),
    ],
    ["Total working set", formatBytes(summary.totalWorkingSetBytes)],
  ];

  const topProcesses = summary.topProcesses
    .map(
      (process) =>
        `| ${process.pid} | ${process.processName} | ${formatBytes(process.privateBytes)} | ${formatBytes(process.workingSetBytes)} | \`${process.command.replaceAll("|", "\\|")}\` |`,
    )
    .join("\n");

  return [
    `## Desktop Runtime Measurement: ${summary.label}`,
    "",
    "| Metric | Value |",
    "| --- | --- |",
    ...rows.map(([name, value]) => `| ${name} | ${value} |`),
    "",
    "### Top Processes",
    "",
    `| PID | Name | ${processMemoryLabel} | Working Set | Command |`,
    "| --- | --- | --- | --- | --- |",
    topProcesses || "| - | - | - | - | - |",
    "",
  ].join("\n");
}

function asNumber(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) ? value : 0;
}

function asString(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function asOptionalEpochMs(value: unknown): number | undefined {
  const parsed = Date.parse(asString(value));
  return Number.isFinite(parsed) ? parsed : undefined;
}

export function normalizeWindowsProcessRows(raw: unknown): ReadonlyArray<RuntimeProcessRow> {
  const entries = Array.isArray(raw) ? raw : raw && typeof raw === "object" ? [raw] : [];
  return entries.flatMap((entry) => {
    if (!entry || typeof entry !== "object") return [];
    const record = entry as Record<string, unknown>;
    const pid = asNumber(record.ProcessId);
    if (pid <= 0) return [];
    const startedAtEpochMs = asOptionalEpochMs(record.CreationTimeIso);
    return [
      {
        pid,
        ppid: asNumber(record.ParentProcessId),
        processName: asString(record.ProcessName),
        command: asString(record.CommandLine) || asString(record.ProcessName),
        privateBytes: asNumber(record.PrivateMemorySize64),
        workingSetBytes: asNumber(record.WorkingSet64) || asNumber(record.WorkingSetSize),
        mainWindowTitle: asString(record.MainWindowTitle),
        ...(startedAtEpochMs !== undefined ? { startedAtEpochMs } : {}),
      },
    ];
  });
}

export function buildWindowsProcessRowsCommand(): string {
  return [
    "$runtime = @{}",
    "Get-Process | ForEach-Object { $runtime[[int]$_.Id] = $_ }",
    "$rows = Get-CimInstance Win32_Process | ForEach-Object {",
    "$p = $runtime[[int]$_.ProcessId]",
    "[pscustomobject]@{",
    "ProcessId = [int]$_.ProcessId",
    "ParentProcessId = [int]$_.ParentProcessId",
    "ProcessName = if ($p) { $p.ProcessName } else { $_.Name }",
    "CommandLine = if ($_.CommandLine) { $_.CommandLine } else { $_.Name }",
    "WorkingSetSize = [double]$_.WorkingSetSize",
    "PrivateMemorySize64 = if ($p) { [double]$p.PrivateMemorySize64 } else { 0 }",
    "WorkingSet64 = if ($p) { [double]$p.WorkingSet64 } else { [double]$_.WorkingSetSize }",
    "MainWindowTitle = if ($p) { [string]$p.MainWindowTitle } else { '' }",
    "CreationTimeIso = if ($p) { try { [string]$p.StartTime.ToUniversalTime().ToString('o') } catch { '' } } else { '' }",
    "}",
    "}",
    "$rows | ConvertTo-Json -Compress -Depth 4",
  ].join("\n");
}

export function parsePosixProcessRows(output: string): ReadonlyArray<RuntimeProcessRow> {
  return output.split(/\r?\n/).flatMap((line) => {
    const match = line.trim().match(/^(\d+)\s+(\d+)\s+(\d+)\s+(\S+)\s+(.*)$/);
    if (!match) return [];
    const rssBytes = Number(match[3]) * 1024;
    return [
      {
        pid: Number(match[1]),
        ppid: Number(match[2]),
        processName: match[4]!,
        command: match[5]!,
        privateBytes: rssBytes,
        // Preserve the established JSON field while identifying that POSIX `ps` only reports RSS.
        privateBytesMetric: "rss-approximation",
        workingSetBytes: rssBytes,
        mainWindowTitle: "",
      },
    ];
  });
}

export interface RuntimeProcessRowsCommandRunner {
  (
    command: string,
    args: ReadonlyArray<string>,
    options: {
      readonly encoding: "utf8";
      readonly maxBuffer: number;
      readonly signal: AbortSignal;
      readonly timeout?: number;
    },
  ): Promise<string>;
}

export interface RuntimeExecFile {
  (
    command: string,
    args: ReadonlyArray<string>,
    options: {
      readonly encoding: "utf8";
      readonly maxBuffer: number;
      readonly signal: AbortSignal;
      readonly timeout?: number;
    },
    callback: (error: Error | null, stdout: string, stderr: string) => void,
  ): NodeChildProcess.ChildProcess;
}

export function makeRuntimeProcessRowsCommandRunner(
  execFile: RuntimeExecFile,
): RuntimeProcessRowsCommandRunner {
  return (command, args, options) =>
    new Promise((resolve, reject) => {
      execFile(command, args, options, (error, stdout) => {
        if (error) {
          reject(error);
          return;
        }
        resolve(stdout);
      });
    });
}

export function makeRuntimeSleep(
  timeout: (
    ms: number,
    value: undefined,
    options: { readonly signal: AbortSignal } | undefined,
  ) => Promise<unknown>,
): (ms: number, signal?: AbortSignal) => Promise<void> {
  return async (ms, signal) => {
    await timeout(ms, undefined, signal ? { signal } : undefined);
  };
}

export function currentEpochMs(): number {
  return NodePerfHooks.performance.timeOrigin + NodePerfHooks.performance.now();
}

export function makeRuntimeProcessRowsReader(
  platform: NodeJS.Platform,
  run: RuntimeProcessRowsCommandRunner,
): (signal: AbortSignal) => Promise<ReadonlyArray<RuntimeProcessRow>> {
  if (platform === "win32") {
    return async (signal) =>
      normalizeWindowsProcessRows(
        JSON.parse(
          (await run(
            "powershell.exe",
            ["-NoProfile", "-NonInteractive", "-Command", buildWindowsProcessRowsCommand()],
            {
              encoding: "utf8",
              maxBuffer: 16 * 1024 * 1024,
              signal,
              timeout: WINDOWS_PROCESS_SAMPLER_TIMEOUT_MS,
            },
          )) || "[]",
        ),
      );
  }
  return async (signal) =>
    parsePosixProcessRows(
      await run("ps", ["-axo", "pid=,ppid=,rss=,comm=,command="], {
        encoding: "utf8",
        maxBuffer: 16 * 1024 * 1024,
        signal,
      }),
    );
}

const abortCause = (signal: AbortSignal): unknown => signal.reason;

export async function waitForReadyUrl(
  url: string,
  timeoutMs: number,
  dependencies: ReadyUrlDependencies,
  signal: AbortSignal = new AbortController().signal,
): Promise<number> {
  const startedAt = dependencies.now();
  while (dependencies.now() - startedAt < timeoutMs) {
    if (signal.aborted) throw abortCause(signal);
    const controller = new AbortController();
    const cancelAttempt = () => controller.abort(abortCause(signal));
    signal.addEventListener("abort", cancelAttempt, { once: true });
    const timeout = dependencies.scheduleAbort(() => controller.abort(), 2_000);
    try {
      const response = await dependencies.fetch(url, { signal: controller.signal });
      if (response.status < 500) {
        await response.body?.cancel().catch(() => undefined);
        return dependencies.now() - startedAt;
      }
      await response.body?.cancel().catch(() => undefined);
    } catch (cause) {
      if (signal.aborted) throw abortCause(signal);
      void cause;
      // Keep polling until timeout.
    } finally {
      dependencies.clearAbort(timeout);
      signal.removeEventListener("abort", cancelAttempt);
    }
    await dependencies.sleep(500, signal);
  }
  throw new DesktopRuntimeMeasurementError(`Timed out waiting for ready URL: ${url}`);
}

export async function waitForWindowTitle(
  rootPid: number,
  titleNeedle: string,
  timeoutMs: number,
  dependencies: WindowTitleDependencies,
  signal: AbortSignal = new AbortController().signal,
): Promise<number> {
  const startedAt = dependencies.now();
  const normalizedNeedle = titleNeedle.toLowerCase();
  while (dependencies.now() - startedAt < timeoutMs) {
    if (signal.aborted) throw abortCause(signal);
    const tree = collectProcessTree(await dependencies.readProcessRows(signal), rootPid);
    if (tree.some((row) => row.mainWindowTitle.toLowerCase().includes(normalizedNeedle))) {
      return dependencies.now() - startedAt;
    }
    await dependencies.sleep(500, signal);
  }
  throw new DesktopRuntimeMeasurementError(`Timed out waiting for window title: ${titleNeedle}`);
}

export function collectOwnedProcessTree(
  rows: ReadonlyArray<RuntimeProcessRow>,
  identity: RuntimeProcessIdentity,
): ReadonlyArray<RuntimeProcessRow> {
  const ownedStartTimes =
    identity.windowsOwnedProcessStartTimes ?? new Map([[identity.pid, identity.startedAtEpochMs]]);
  const ownedPids = new Set<number>();
  const verifiedStartTimes = new Map<number, number>();
  for (const row of rows) {
    if (row.startedAtEpochMs === undefined) continue;
    const expectedStart = ownedStartTimes.get(row.pid);
    if (expectedStart === undefined) continue;
    if (row.startedAtEpochMs === expectedStart) {
      ownedPids.add(row.pid);
      verifiedStartTimes.set(row.pid, row.startedAtEpochMs);
    }
  }

  let discovered = true;
  while (discovered) {
    discovered = false;
    for (const row of rows) {
      const parentStartedAtEpochMs = verifiedStartTimes.get(row.ppid);
      if (
        row.startedAtEpochMs !== undefined &&
        !ownedPids.has(row.pid) &&
        parentStartedAtEpochMs !== undefined &&
        row.startedAtEpochMs >= parentStartedAtEpochMs
      ) {
        ownedPids.add(row.pid);
        verifiedStartTimes.set(row.pid, row.startedAtEpochMs);
        discovered = true;
      }
    }
  }

  const ownedRows = rows.filter((row) => ownedPids.has(row.pid));
  for (const row of ownedRows) {
    ownedStartTimes.set(row.pid, row.startedAtEpochMs!);
  }
  return ownedRows;
}

export async function monitorRuntimeProcessOwnership(
  identity: RuntimeProcessIdentity,
  dependencies: Pick<RuntimeMeasurementHost, "readProcessRows" | "sleep">,
  signal: AbortSignal,
): Promise<void> {
  while (!signal.aborted) {
    let rows: ReadonlyArray<RuntimeProcessRow>;
    try {
      rows = await dependencies.readProcessRows(signal);
    } catch (cause) {
      if (signal.aborted) throw abortCause(signal);
      throw cause;
    }
    collectOwnedProcessTree(rows, identity);
    await dependencies.sleep(OWNERSHIP_SAMPLE_INTERVAL_MS, signal);
  }
  throw abortCause(signal);
}

export async function cleanupRuntimeProcess(
  identity: RuntimeProcessIdentity,
  dependencies: RuntimeCleanupDependencies,
): Promise<void> {
  if (dependencies.platform === "win32") {
    const rows = await dependencies.readProcessRows(new AbortController().signal).catch(() => []);
    for (const process of collectOwnedProcessTree(rows, identity).toReversed()) {
      try {
        const startedAtEpochMs = process.startedAtEpochMs!;
        const command = [
          `$expected = ${Math.trunc(startedAtEpochMs)}`,
          `$process = Get-Process -Id ${process.pid} -ErrorAction SilentlyContinue`,
          "if ($process) {",
          "$actual = ([DateTimeOffset]$process.StartTime.ToUniversalTime()).ToUnixTimeMilliseconds()",
          `if ($actual -eq $expected) { Stop-Process -Id ${process.pid} -Force -ErrorAction SilentlyContinue }`,
          "}",
        ].join("\n");
        dependencies.execFile(
          "powershell.exe",
          ["-NoProfile", "-NonInteractive", "-Command", command],
          {
            encoding: "utf8",
            stdio: "ignore",
            timeout: WINDOWS_PROCESS_CLEANUP_TIMEOUT_MS,
          },
        );
      } catch {
        // The verified descendant may have exited between sampling and termination.
      }
    }
    return;
  }

  const processGroupId = identity.processGroupId;
  if (processGroupId === undefined) return;
  try {
    dependencies.kill(-processGroupId, "SIGTERM");
  } catch {
    return;
  }
  await dependencies.sleep(2_000);
  try {
    dependencies.kill(-processGroupId, 0);
  } catch {
    return;
  }
  try {
    dependencies.kill(-processGroupId, "SIGKILL");
  } catch {
    // The process group exited after the liveness probe.
  }
}

function logMeasurementPhase(
  input: RuntimeMeasurementInput,
  message: string,
  host: RuntimeMeasurementHost,
): void {
  if (!input.verbose) return;
  host.stderr(`[measure-desktop-runtime] ${message}\n`);
}

export function spawnDesktopProcess(
  input: RuntimeMeasurementInput,
  dependencies: RuntimeSpawnDependencies,
): Promise<LaunchedChildProcess> {
  return new Promise((resolve, reject) => {
    const child = dependencies.spawn(input.command, [...input.args], {
      cwd: input.cwd,
      detached: dependencies.platform !== "win32",
      env: { ...process.env, ...input.env },
      stdio: "ignore",
      windowsHide: false,
    });

    let launched = false;
    let disposed = false;
    let terminationSettled = false;
    let rejectTermination!: (cause: unknown) => void;
    const termination = new Promise<never>((_resolve, rejectUnexpected) => {
      rejectTermination = rejectUnexpected;
    });
    void termination.catch(() => undefined);

    const dispose = () => {
      if (disposed) return;
      disposed = true;
      child.off("error", onError);
      child.off("exit", onExit);
      child.off("close", onClose);
    };
    const failSpawn = (cause: unknown) => {
      if (launched || disposed) return;
      dispose();
      reject(
        new DesktopRuntimeMeasurementError(
          `Failed to start desktop process '${input.command}': ${
            cause instanceof Error ? cause.message : String(cause)
          }`,
        ),
      );
    };
    const failRuntime = (message: string, cause?: unknown) => {
      if (!launched || disposed || terminationSettled) return;
      terminationSettled = true;
      rejectTermination(
        new DesktopRuntimeMeasurementError(
          cause instanceof Error ? `${message}: ${cause.message}` : message,
        ),
      );
    };
    function onError(cause: unknown): void {
      if (!launched) {
        failSpawn(cause);
        return;
      }
      failRuntime(`Desktop process '${input.command}' emitted an error`, cause);
    }
    function onExit(code: number | null, signal: NodeJS.Signals | null): void {
      failRuntime(
        `Desktop process '${input.command}' exited unexpectedly (code ${String(code)}, signal ${String(signal)})`,
      );
    }
    function onClose(code: number | null, signal: NodeJS.Signals | null): void {
      failRuntime(
        `Desktop process '${input.command}' closed unexpectedly (code ${String(code)}, signal ${String(signal)})`,
      );
    }

    child.on("error", onError);
    child.on("exit", onExit);
    child.on("close", onClose);
    if (child.pid === undefined) {
      queueMicrotask(() => failSpawn("process id was not assigned"));
      return;
    }

    const pid = child.pid;
    launched = true;
    const resolveIdentity = async () => {
      let identity: RuntimeProcessIdentity;
      if (dependencies.platform === "win32") {
        if (!dependencies.readProcessRows) {
          throw new DesktopRuntimeMeasurementError(
            `Cannot verify Windows process identity for launched process ${pid}.`,
          );
        }
        const controller = new AbortController();
        const sampledRows = await Promise.race([
          dependencies.readProcessRows(controller.signal),
          termination,
        ]).finally(() => controller.abort());
        const root = sampledRows.find(
          (row) => row.pid === pid && row.startedAtEpochMs !== undefined,
        );
        if (root?.startedAtEpochMs === undefined) {
          throw new DesktopRuntimeMeasurementError(
            `Cannot verify Windows process identity for launched process ${pid}.`,
          );
        }
        identity = {
          pid,
          startedAtEpochMs: root.startedAtEpochMs,
          windowsOwnedProcessStartTimes: new Map([[pid, root.startedAtEpochMs]]),
        };
        collectOwnedProcessTree(sampledRows, identity);
      } else {
        identity = {
          pid,
          processGroupId: pid,
          startedAtEpochMs: dependencies.epochNow(),
        };
      }
      resolve({
        pid,
        killed: child.killed,
        identity,
        termination,
        dispose,
        unref: () => child.unref(),
      });
    };
    void resolveIdentity().catch((cause) => {
      try {
        child.kill();
      } catch {
        // The process may already have exited while its identity was sampled.
      }
      dispose();
      reject(
        cause instanceof DesktopRuntimeMeasurementError
          ? cause
          : new DesktopRuntimeMeasurementError(
              `Failed to verify Windows process identity for launched process ${pid}: ${cause instanceof Error ? cause.message : String(cause)}`,
            ),
      );
    });
  });
}

export interface RuntimeMeasurementSystem {
  readonly platform: NodeJS.Platform;
  readonly epochNow: () => number;
  readonly spawnProcess: (
    command: string,
    args: ReadonlyArray<string>,
    options: NodeChildProcess.SpawnOptions,
  ) => SpawnedChildProcess;
  readonly now: () => number;
  readonly sampledAt: () => string;
  readonly sleep: (ms: number, signal?: AbortSignal) => Promise<void>;
  readonly fetch: ReadyUrlDependencies["fetch"];
  readonly scheduleAbort: ReadyUrlDependencies["scheduleAbort"];
  readonly clearAbort: ReadyUrlDependencies["clearAbort"];
  readonly readProcessRows: (signal: AbortSignal) => Promise<ReadonlyArray<RuntimeProcessRow>>;
  readonly execFile: RuntimeCleanupDependencies["execFile"];
  readonly kill: RuntimeCleanupDependencies["kill"];
  readonly writeTextFile: RuntimeMeasurementHost["writeTextFile"];
  readonly stdout: RuntimeMeasurementHost["stdout"];
  readonly stderr: RuntimeMeasurementHost["stderr"];
}

export function makeRuntimeMeasurementHost(
  system: RuntimeMeasurementSystem,
): RuntimeMeasurementHost {
  const readyDependencies: ReadyUrlDependencies = {
    now: system.now,
    sleep: system.sleep,
    fetch: system.fetch,
    scheduleAbort: system.scheduleAbort,
    clearAbort: system.clearAbort,
  };
  const windowDependencies: WindowTitleDependencies = {
    now: system.now,
    sleep: system.sleep,
    readProcessRows: system.readProcessRows,
  };
  const cleanupDependencies: RuntimeCleanupDependencies = {
    platform: system.platform,
    execFile: system.execFile,
    kill: system.kill,
    readProcessRows: system.readProcessRows,
    sleep: (ms) => system.sleep(ms),
  };
  return {
    spawn: (input) =>
      spawnDesktopProcess(input, {
        platform: system.platform,
        epochNow: system.epochNow,
        spawn: system.spawnProcess,
        readProcessRows: system.readProcessRows,
      }),
    ...(system.platform === "win32"
      ? {
          monitorOwnership: (identity: RuntimeProcessIdentity, signal: AbortSignal) =>
            monitorRuntimeProcessOwnership(
              identity,
              { readProcessRows: system.readProcessRows, sleep: system.sleep },
              signal,
            ),
        }
      : {}),
    waitForReadyUrl: (url, timeoutMs, signal) =>
      waitForReadyUrl(url, timeoutMs, readyDependencies, signal),
    waitForWindowTitle: (rootPid, titleNeedle, timeoutMs, signal) =>
      waitForWindowTitle(rootPid, titleNeedle, timeoutMs, windowDependencies, signal),
    sleep: system.sleep,
    now: system.now,
    sampledAt: system.sampledAt,
    readProcessRows: system.readProcessRows,
    cleanup: (identity) => cleanupRuntimeProcess(identity, cleanupDependencies),
    writeTextFile: system.writeTextFile,
    stdout: system.stdout,
    stderr: system.stderr,
  };
}

export function currentSampledAt(): string {
  // @effect-diagnostics-next-line globalDate:off - Runtime measurements record a wall-clock sample timestamp.
  return new Date().toISOString();
}

const defaultRuntimeSleep = makeRuntimeSleep(NodeTimersPromises.setTimeout);
const defaultProcessRowsCommandRunner = makeRuntimeProcessRowsCommandRunner(
  NodeChildProcess.execFile as RuntimeExecFile,
);

// oxlint-disable-next-line coddy/no-global-process-runtime -- Standalone CLI samples the actual host platform and injects it into every adapter.
const defaultHostPlatform = NodeOS.platform();

const defaultRuntimeMeasurementHost = makeRuntimeMeasurementHost({
  platform: defaultHostPlatform,
  epochNow: currentEpochMs,
  spawnProcess: NodeChildProcess.spawn as RuntimeMeasurementSystem["spawnProcess"],
  now: NodePerfHooks.performance.now.bind(NodePerfHooks.performance),
  sampledAt: currentSampledAt,
  sleep: defaultRuntimeSleep,
  fetch: globalThis.fetch,
  scheduleAbort: globalThis.setTimeout,
  clearAbort: globalThis.clearTimeout as ReadyUrlDependencies["clearAbort"],
  readProcessRows: makeRuntimeProcessRowsReader(
    defaultHostPlatform,
    defaultProcessRowsCommandRunner,
  ),
  execFile: NodeChildProcess.execFileSync as RuntimeCleanupDependencies["execFile"],
  kill: process.kill.bind(process),
  writeTextFile,
  stdout: process.stdout.write.bind(process.stdout),
  stderr: process.stderr.write.bind(process.stderr),
});

async function runWhileChildAlive<T>(
  operation: Promise<T>,
  settlements: ReadonlyArray<Promise<unknown>>,
  child: LaunchedChildProcess,
  controller: AbortController,
  ownershipMonitoring: Promise<void> | undefined,
): Promise<T> {
  const monitorSettlements = ownershipMonitoring ? [ownershipMonitoring] : [];
  try {
    return (await Promise.race([...monitorSettlements, child.termination, operation])) as T;
  } catch (cause) {
    controller.abort(cause);
    await Promise.allSettled([...settlements, ...monitorSettlements]);
    throw cause;
  }
}

export async function measureDesktopRuntime(
  input: RuntimeMeasurementInput,
  host: RuntimeMeasurementHost = defaultRuntimeMeasurementHost,
): Promise<RuntimeMeasurementSummary> {
  const child = await host.spawn(input);
  child.unref();
  logMeasurementPhase(input, `spawned pid ${child.pid}`, host);

  let startupMs: number | null = null;
  let windowReadyMs: number | null = null;
  const startedAt = host.now();
  const controller = new AbortController();
  const ownershipMonitoring = host.monitorOwnership?.(child.identity, controller.signal);
  void ownershipMonitoring?.catch(() => undefined);
  try {
    const readiness: Array<Promise<void>> = [];
    if (input.readyUrl) {
      readiness.push(
        host
          .waitForReadyUrl(input.readyUrl, input.timeoutMs, controller.signal)
          .then((duration) => {
            startupMs = duration;
          }),
      );
    }
    if (input.windowTitle) {
      readiness.push(
        host
          .waitForWindowTitle(child.pid, input.windowTitle, input.timeoutMs, controller.signal)
          .then((duration) => {
            windowReadyMs = duration;
          }),
      );
    }
    if (readiness.length > 0) {
      logMeasurementPhase(input, "waiting for readiness gates", host);
      await runWhileChildAlive(
        Promise.all(readiness),
        readiness,
        child,
        controller,
        ownershipMonitoring,
      );
      logMeasurementPhase(input, "readiness gates completed", host);
    } else {
      startupMs = host.now() - startedAt;
    }

    logMeasurementPhase(input, `idling for ${input.idleMs} ms`, host);
    const idle = host.sleep(input.idleMs, controller.signal);
    await runWhileChildAlive(idle, [idle], child, controller, ownershipMonitoring);
    logMeasurementPhase(input, "sampling process table", host);
    const sampling = host.readProcessRows(controller.signal);
    const rows = await runWhileChildAlive(
      sampling,
      [sampling],
      child,
      controller,
      ownershipMonitoring,
    );
    logMeasurementPhase(input, `sampled ${rows.length} process rows`, host);
    collectOwnedProcessTree(rows, child.identity);
    const summary = summarizeProcessTree({
      label: input.label,
      rootPid: child.pid,
      rows,
      sampledAt: host.sampledAt(),
      startupMs,
      windowReadyMs,
      readyUrl: input.readyUrl ?? null,
      windowTitle: input.windowTitle ?? null,
      idleMs: input.idleMs,
    });
    if (summary.processCount === 0 || !summary.processIds.includes(child.pid)) {
      throw new DesktopRuntimeMeasurementError(
        `Runtime process sample did not contain launched process ${child.pid}.`,
      );
    }
    if (summary.totalPrivateBytes <= 0 || summary.totalWorkingSetBytes <= 0) {
      throw new DesktopRuntimeMeasurementError(
        "Runtime process sample reported invalid zero memory totals.",
      );
    }
    return summary;
  } finally {
    if (!controller.signal.aborted) {
      controller.abort(new DesktopRuntimeMeasurementError("Runtime measurement completed."));
    }
    if (ownershipMonitoring) await Promise.allSettled([ownershipMonitoring]);
    child.dispose();
    if (!input.keepRunning) {
      logMeasurementPhase(input, "cleanup starting", host);
      await host.cleanup(child.identity);
      logMeasurementPhase(input, "cleanup completed", host);
    }
  }
}

export function writeTextFile(filePath: string, text: string): void {
  NodeFS.mkdirSync(NodePath.dirname(NodePath.resolve(filePath)), { recursive: true });
  NodeFS.writeFileSync(filePath, text);
}

export async function runMeasureDesktopRuntimeCli(
  argv: ReadonlyArray<string>,
  host: RuntimeMeasurementHost,
): Promise<RuntimeMeasurementSummary> {
  const input = parseMeasureDesktopRuntimeArgs(argv);
  const summary = await measureDesktopRuntime(input, host);
  const json = `${JSON.stringify(summary, null, 2)}\n`;
  if (input.jsonOut) host.writeTextFile(input.jsonOut, json);
  if (input.markdownOut) {
    host.writeTextFile(input.markdownOut, renderMeasurementMarkdown(summary));
  }
  host.stdout(json);
  return summary;
}

export async function runMeasureDesktopRuntimeMain(
  isMain: boolean,
  argv: ReadonlyArray<string>,
  host: RuntimeMeasurementHost,
  setExitCode: (code: number) => void,
): Promise<boolean> {
  if (!isMain) return false;
  try {
    await runMeasureDesktopRuntimeCli(argv, host);
  } catch (error) {
    host.stderr(`${error instanceof Error ? error.message : String(error)}\n`);
    setExitCode(1);
  }
  return true;
}

export function setProcessExitCode(code: number): void {
  process.exitCode = code;
}

void runMeasureDesktopRuntimeMain(
  import.meta.main,
  process.argv.slice(2),
  defaultRuntimeMeasurementHost,
  setProcessExitCode,
);
