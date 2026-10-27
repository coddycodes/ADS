#!/usr/bin/env node

// @effect-diagnostics nodeBuiltinImport:off - Standalone Windows measurement controller.
import * as NodeChildProcess from "node:child_process";
import * as NodeCrypto from "node:crypto";
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import * as NodePerfHooks from "node:perf_hooks";
import * as NodeTimersPromises from "node:timers/promises";
import * as NodeUtil from "node:util";

const DEFAULT_DURATION_MS = 600_000;
const DEFAULT_QUEUE_WARMUPS = 20;
const DEFAULT_QUEUE_SAMPLES = 200;
const READY_TIMEOUT_MS = 120_000;
const PROBE_TIMEOUT_MS = 30_000;
const SHUTDOWN_TIMEOUT_MS = 60_000;
const GIT_FIELD_SEPARATOR = "\u001f";

export interface MeasureVcsRuntimeInput {
  readonly durationMs: number;
  readonly queueWarmups: number;
  readonly queueSamples: number;
  readonly outputDirectory?: string;
}

export interface GitLaunchRecord {
  readonly timestampMs: number;
  readonly pid: number;
  readonly startedAt: string;
  readonly parentPid: number;
  readonly parentStartedAt: string;
  readonly args: ReadonlyArray<string>;
}

export interface GitMeasurementIdentity {
  readonly serverPid: number;
  readonly serverStartedAt: string;
  readonly startInclusiveMs: number;
  readonly endExclusiveMs: number;
  readonly physicalRepositories: number;
}

export interface GitArgumentGroup {
  readonly category: string;
  readonly count: number;
  readonly args: ReadonlyArray<string>;
}

export interface GitLaunchSummary {
  readonly durationMs: number;
  readonly physicalRepositories: number;
  readonly directLaunches: number;
  readonly launchesPerMinutePerPhysicalRepository: number;
  readonly recordsInsideWindow: number;
  readonly recordsOutsideWindow: number;
  readonly nonDirectRecords: number;
  readonly wrongParentIdentityRecords: number;
  readonly argumentGroups: ReadonlyArray<GitArgumentGroup>;
}

export interface WindowsProcessIdentity {
  readonly pid: number;
  readonly startedAt: string;
  readonly executable: string;
}

export interface MeasurementBuildPlan {
  readonly targetDirectory: string;
  readonly environment: NodeJS.ProcessEnv;
}

interface MeasurementArtifacts {
  readonly serverExecutable: string;
  readonly shimExecutable: string;
}

interface ReadyEvidence {
  readonly pid: number;
  readonly address: string;
  readonly executable: string;
  readonly baseDir: string;
  readonly repository: string;
  readonly commonDir: string;
  readonly physicalRepositories: number;
  readonly worktrees: number;
  readonly subscribers: number;
}

export class VcsRuntimeMeasurementError extends Error {
  override readonly name = "VcsRuntimeMeasurementError";
}

function currentEpochMs(): number {
  return NodePerfHooks.performance.timeOrigin + NodePerfHooks.performance.now();
}

export function createMeasurementBuildPlan(
  outputDirectory: string,
  inheritedEnvironment: NodeJS.ProcessEnv,
): MeasurementBuildPlan {
  const targetDirectory = NodePath.join(outputDirectory, "cargo-target");
  return {
    targetDirectory,
    environment: { ...inheritedEnvironment, CARGO_TARGET_DIR: targetDirectory },
  };
}

export function parseCargoExampleArtifacts(
  output: string,
  targetDirectory: string,
): MeasurementArtifacts {
  const artifacts = new Map<string, string>();
  for (const line of output.split(/\r?\n/)) {
    if (!line.trimStart().startsWith("{")) continue;
    let parsed: unknown;
    try {
      parsed = JSON.parse(line);
    } catch (cause) {
      throw new VcsRuntimeMeasurementError(
        `Cargo emitted malformed JSON: ${cause instanceof Error ? cause.message : String(cause)}`,
      );
    }
    if (!parsed || typeof parsed !== "object") continue;
    const record = parsed as Record<string, unknown>;
    const target = record.target as Record<string, unknown> | undefined;
    const name = target?.name;
    if (
      record.reason !== "compiler-artifact" ||
      typeof name !== "string" ||
      !Array.isArray(target?.kind) ||
      !target.kind.includes("example") ||
      !["measure_vcs_runtime_server", "measure_vcs_git_shim"].includes(name)
    ) {
      continue;
    }
    if (artifacts.has(name)) {
      throw new VcsRuntimeMeasurementError(`Cargo emitted a duplicate ${name} artifact.`);
    }
    if (typeof record.executable !== "string" || record.executable.length === 0) {
      throw new VcsRuntimeMeasurementError(`Cargo ${name} artifact has no executable path.`);
    }
    const executable = NodePath.resolve(record.executable);
    const relative = NodePath.relative(NodePath.resolve(targetDirectory), executable);
    if (relative.startsWith("..") || NodePath.isAbsolute(relative)) {
      throw new VcsRuntimeMeasurementError(
        `Cargo ${name} artifact is outside the evidence target.`,
      );
    }
    artifacts.set(name, executable);
  }
  const serverExecutable = artifacts.get("measure_vcs_runtime_server");
  const shimExecutable = artifacts.get("measure_vcs_git_shim");
  if (!serverExecutable || !shimExecutable) {
    throw new VcsRuntimeMeasurementError("Cargo output is missing a measurement example artifact.");
  }
  return { serverExecutable, shimExecutable };
}

function positiveInteger(value: string | undefined, fallback: number, label: string): number {
  if (value === undefined) return fallback;
  const parsed = Number(value);
  if (!Number.isInteger(parsed) || parsed <= 0) {
    throw new VcsRuntimeMeasurementError(`${label} must be a positive integer.`);
  }
  return parsed;
}

export function parseMeasureVcsRuntimeArgs(argv: ReadonlyArray<string>): MeasureVcsRuntimeInput {
  const { values } = NodeUtil.parseArgs({
    args: [...argv],
    options: {
      "duration-ms": { type: "string" },
      "queue-warmups": { type: "string" },
      "queue-samples": { type: "string" },
      "output-dir": { type: "string" },
    },
    allowPositionals: false,
  });
  return {
    durationMs: positiveInteger(values["duration-ms"], DEFAULT_DURATION_MS, "--duration-ms"),
    queueWarmups: positiveInteger(
      values["queue-warmups"],
      DEFAULT_QUEUE_WARMUPS,
      "--queue-warmups",
    ),
    queueSamples: positiveInteger(
      values["queue-samples"],
      DEFAULT_QUEUE_SAMPLES,
      "--queue-samples",
    ),
    ...(values["output-dir"] ? { outputDirectory: values["output-dir"] } : {}),
  };
}

function finiteNumber(value: unknown, label: string, line: number): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) {
    throw new VcsRuntimeMeasurementError(`Git log line ${line} has invalid ${label}.`);
  }
  return value;
}

function decimalIdentity(value: unknown, label: string, line: number): string {
  if (typeof value !== "string" || !/^\d+$/.test(value)) {
    throw new VcsRuntimeMeasurementError(`Git log line ${line} has invalid ${label}.`);
  }
  return value;
}

export function parseGitLaunchLog(text: string): ReadonlyArray<GitLaunchRecord> {
  const records = text.split(/\r?\n/).flatMap((line, index) => {
    if (line.length === 0) return [];
    let parsed: unknown;
    try {
      parsed = JSON.parse(line);
    } catch (cause) {
      throw new VcsRuntimeMeasurementError(
        `Git log line ${index + 1} is not complete JSON: ${cause instanceof Error ? cause.message : String(cause)}`,
      );
    }
    if (!parsed || typeof parsed !== "object") {
      throw new VcsRuntimeMeasurementError(`Git log line ${index + 1} is not an object.`);
    }
    const record = parsed as Record<string, unknown>;
    if (
      !Array.isArray(record.args) ||
      !record.args.every((argument) => typeof argument === "string")
    ) {
      throw new VcsRuntimeMeasurementError(`Git log line ${index + 1} has invalid args.`);
    }
    return [
      {
        timestampMs: finiteNumber(record.timestampMs, "timestampMs", index + 1),
        pid: finiteNumber(record.pid, "pid", index + 1),
        startedAt: decimalIdentity(record.startedAt, "startedAt", index + 1),
        parentPid: finiteNumber(record.parentPid, "parentPid", index + 1),
        parentStartedAt: decimalIdentity(record.parentStartedAt, "parentStartedAt", index + 1),
        args: record.args,
      },
    ];
  });
  const identities = new Set<string>();
  for (const record of records) {
    const identity = `${record.pid}:${record.startedAt}`;
    if (identities.has(identity)) {
      throw new VcsRuntimeMeasurementError(`Git log repeats shim identity ${identity}.`);
    }
    identities.add(identity);
  }
  return records;
}

function exactArgs(args: ReadonlyArray<string>, expected: ReadonlyArray<string>): boolean {
  return args.length === expected.length && args.every((value, index) => value === expected[index]);
}

export function classifyGitArguments(args: ReadonlyArray<string>): string {
  if (args.includes("--numstat")) return "numstat";
  if (exactArgs(args, ["symbolic-ref", "--quiet", "--short", "HEAD"])) return "current-ref";
  if (args[0] === "symbolic-ref") return "default-ref";
  if (args.includes("--untracked-files=all")) return "local-status";
  if (args.includes("--untracked-files=no")) return "remote-status";
  if (args[0] === "fetch") return "fetch";
  if (args[0] === "for-each-ref") return "upstream-discovery";
  if (args.includes("--git-common-dir")) return "common-dir-discovery";
  if (args.includes("--is-inside-work-tree")) return "repository-probe";
  if (exactArgs(args, ["remote"])) return "remote-list";
  if (exactArgs(args, ["config", "--get", "remote.origin.url"])) return "provider-discovery";
  return "other";
}

export function summarizeGitLaunches(
  records: ReadonlyArray<GitLaunchRecord>,
  identity: GitMeasurementIdentity,
): GitLaunchSummary {
  const durationMs = identity.endExclusiveMs - identity.startInclusiveMs;
  if (durationMs <= 0 || identity.physicalRepositories <= 0) {
    throw new VcsRuntimeMeasurementError(
      "Measurement window and repository count must be positive.",
    );
  }
  const inside = records.filter(
    (record) =>
      record.timestampMs >= identity.startInclusiveMs &&
      record.timestampMs < identity.endExclusiveMs,
  );
  const direct = inside.filter(
    (record) =>
      record.parentPid === identity.serverPid &&
      record.parentStartedAt === identity.serverStartedAt,
  );
  const nonDirectRecords = inside.filter(
    (record) => record.parentPid !== identity.serverPid,
  ).length;
  const wrongParentIdentityRecords = inside.filter(
    (record) =>
      record.parentPid === identity.serverPid &&
      record.parentStartedAt !== identity.serverStartedAt,
  ).length;
  const grouped = new Map<string, { args: ReadonlyArray<string>; count: number }>();
  for (const record of direct) {
    const key = record.args.join(GIT_FIELD_SEPARATOR);
    const current = grouped.get(key);
    grouped.set(key, { args: record.args, count: (current?.count ?? 0) + 1 });
  }
  const argumentGroups = [...grouped.values()]
    .map((group) => ({
      category: classifyGitArguments(group.args),
      count: group.count,
      args: group.args,
    }))
    .toSorted(
      (left, right) => right.count - left.count || left.category.localeCompare(right.category),
    );
  return {
    durationMs,
    physicalRepositories: identity.physicalRepositories,
    directLaunches: direct.length,
    launchesPerMinutePerPhysicalRepository:
      direct.length / (durationMs / 60_000) / identity.physicalRepositories,
    recordsInsideWindow: inside.length,
    recordsOutsideWindow: records.length - inside.length,
    nonDirectRecords,
    wrongParentIdentityRecords,
    argumentGroups,
  };
}

function commandPath(name: string): string {
  const output = NodeChildProcess.execFileSync("where.exe", [name], { encoding: "utf8" });
  const first = output.split(/\r?\n/).find((entry) => entry.trim().length > 0);
  if (!first) throw new VcsRuntimeMeasurementError(`Could not resolve ${name}.`);
  return NodeFS.realpathSync(first.trim());
}

function runGit(git: string, cwd: string, args: ReadonlyArray<string>): string {
  return NodeChildProcess.execFileSync(git, ["-C", cwd, ...args], {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  }).trim();
}

export interface WindowsProcessSnapshotEntry extends WindowsProcessIdentity {
  readonly parentPid: number;
}

interface WindowsProcessTreeIdentity extends WindowsProcessSnapshotEntry {
  readonly depth: number;
}

interface OwnedMeasurementProcess {
  readonly child: NodeChildProcess.ChildProcess;
  readonly identity: WindowsProcessIdentity;
  readonly exit: Promise<{ code: number | null; signal: NodeJS.Signals | null }>;
  readonly stopPath: string;
  readonly gracefulTimeoutMs?: number;
  shutdown?: Promise<{ code: number | null; signal: NodeJS.Signals | null }>;
}

export function parseWindowsProcessSnapshot(
  text: string,
): ReadonlyArray<WindowsProcessSnapshotEntry> {
  if (text.trim().length === 0) return [];
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch (cause) {
    throw new VcsRuntimeMeasurementError(
      `Windows process snapshot is invalid JSON: ${cause instanceof Error ? cause.message : String(cause)}`,
    );
  }
  const rows = Array.isArray(parsed) ? parsed : [parsed];
  const seen = new Set<number>();
  return rows.map((row, index) => {
    if (!row || typeof row !== "object") {
      throw new VcsRuntimeMeasurementError(`Windows process snapshot row ${index + 1} is invalid.`);
    }
    const value = row as Record<string, unknown>;
    const pid = finiteNumber(value.pid, "process pid", index + 1);
    if (seen.has(pid)) {
      throw new VcsRuntimeMeasurementError(`Windows process snapshot has duplicate PID ${pid}.`);
    }
    seen.add(pid);
    if (typeof value.executable !== "string") {
      throw new VcsRuntimeMeasurementError(
        `Windows process snapshot row ${index + 1} has invalid executable.`,
      );
    }
    return {
      pid,
      parentPid: finiteNumber(value.parentPid, "process parentPid", index + 1),
      startedAt: decimalIdentity(value.startedAt, "startedAt", index + 1),
      executable: value.executable,
    };
  });
}

function captureWindowsProcessSnapshot(
  rootPid?: number,
  requestedPids: ReadonlyArray<number> = [],
): ReadonlyArray<WindowsProcessSnapshotEntry> {
  const script = [
    "$rows = @(Get-CimInstance Win32_Process)",
    "$targets = [System.Collections.Generic.HashSet[int]]::new()",
    "$requested = @($env:CODDY_STUDIO_VCS_SNAPSHOT_PIDS -split ',' | Where-Object { $_ -match '^\\d+$' })",
    "foreach ($pidValue in $requested) { [void]$targets.Add([int]$pidValue) }",
    "$root = [int]$env:CODDY_STUDIO_VCS_SNAPSHOT_ROOT_PID",
    "if ($root -gt 0) {",
    "[void]$targets.Add($root)",
    "do {",
    "$added = $false",
    "foreach ($row in $rows) {",
    "if ($targets.Contains([int]$row.ParentProcessId) -and $targets.Add([int]$row.ProcessId)) { $added = $true }",
    "}",
    "} while ($added)",
    "}",
    "@($rows | Where-Object { $targets.Contains([int]$_.ProcessId) } | ForEach-Object {",
    "if ($null -eq $_.CreationDate -or [string]::IsNullOrWhiteSpace([string]$_.ExecutablePath)) { return }",
    "$p = Get-Process -Id ([int]$_.ProcessId) -ErrorAction SilentlyContinue",
    "if ($null -eq $p -or [string]::IsNullOrWhiteSpace([string]$p.Path)) { return }",
    "$exactStartedAt = [int64]$p.StartTime.ToUniversalTime().ToFileTimeUtc()",
    "$cimStartedAt = [int64]$_.CreationDate.ToUniversalTime().ToFileTimeUtc()",
    "if ([Math]::Abs($exactStartedAt - $cimStartedAt) -gt 9) { return }",
    "if (-not ([IO.Path]::GetFullPath([string]$p.Path)).Equals([IO.Path]::GetFullPath([string]$_.ExecutablePath), [StringComparison]::OrdinalIgnoreCase)) { return }",
    "[pscustomobject]@{",
    "pid = [int]$_.ProcessId",
    "parentPid = [int]$_.ParentProcessId",
    "startedAt = [string]$exactStartedAt",
    "executable = [string]$p.Path",
    "}",
    "}) | ConvertTo-Json -Compress",
  ].join("\n");
  return parseWindowsProcessSnapshot(
    NodeChildProcess.execFileSync(
      "powershell.exe",
      ["-NoProfile", "-NonInteractive", "-Command", script],
      {
        encoding: "utf8",
        env: {
          ...process.env,
          CODDY_STUDIO_VCS_SNAPSHOT_ROOT_PID: String(rootPid ?? 0),
          CODDY_STUDIO_VCS_SNAPSHOT_PIDS: requestedPids.join(","),
        },
        timeout: PROBE_TIMEOUT_MS,
      },
    ),
  );
}

export function captureWindowsProcessIdentity(pid: number): WindowsProcessIdentity {
  const identity = captureWindowsProcessSnapshot(undefined, [pid]).find(
    (entry) => entry.pid === pid,
  );
  if (!identity) {
    throw new VcsRuntimeMeasurementError(`Process ${pid} is absent from the Windows snapshot.`);
  }
  return identity;
}

function normalizeWindowsExecutable(executable: string): string {
  return NodePath.win32.normalize(executable).toLowerCase();
}

export function hasExactProcessIdentity(
  snapshot: ReadonlyArray<WindowsProcessSnapshotEntry>,
  identity: WindowsProcessIdentity,
): boolean {
  return snapshot.some(
    (entry) =>
      entry.pid === identity.pid &&
      entry.startedAt === identity.startedAt &&
      normalizeWindowsExecutable(entry.executable) ===
        normalizeWindowsExecutable(identity.executable),
  );
}

export function selectOwnedProcessTree(
  snapshot: ReadonlyArray<WindowsProcessSnapshotEntry>,
  root: WindowsProcessIdentity,
): ReadonlyArray<WindowsProcessTreeIdentity> {
  if (!hasExactProcessIdentity(snapshot, root)) {
    throw new VcsRuntimeMeasurementError("Measurement server identity changed before cleanup.");
  }
  const depths = new Map<number, number>([[root.pid, 0]]);
  for (let depth = 1; ; depth += 1) {
    let added = false;
    for (const row of snapshot) {
      if (!depths.has(row.pid) && depths.get(row.parentPid) === depth - 1) {
        depths.set(row.pid, depth);
        added = true;
      }
    }
    if (!added) break;
  }
  return snapshot
    .filter((entry) => depths.has(entry.pid))
    .map((entry) => ({ ...entry, depth: depths.get(entry.pid)! }))
    .toSorted((left, right) => right.depth - left.depth);
}

function terminateExactProcesses(identities: ReadonlyArray<WindowsProcessTreeIdentity>): void {
  const script = [
    "$items = @($env:CODDY_STUDIO_VCS_TERMINATE_IDENTITIES | ConvertFrom-Json)",
    "foreach ($item in $items) {",
    "try {",
    "$p = Get-Process -Id ([int]$item.pid) -ErrorAction SilentlyContinue",
    "if ($null -eq $p) { continue }",
    "$actual = [string][int64]$p.StartTime.ToUniversalTime().ToFileTimeUtc()",
    "if ($actual -ne [string]$item.startedAt) { continue }",
    "$expectedExecutable = [IO.Path]::GetFullPath([string]$item.executable)",
    "$actualExecutable = [IO.Path]::GetFullPath([string]$p.Path)",
    "if (-not $actualExecutable.Equals($expectedExecutable, [StringComparison]::OrdinalIgnoreCase)) { continue }",
    "$p.Kill(); [void]$p.WaitForExit(5000)",
    "} catch { continue }",
    "}",
    "exit 0",
  ].join("\n");
  NodeChildProcess.execFileSync(
    "powershell.exe",
    ["-NoProfile", "-NonInteractive", "-Command", script],
    {
      env: {
        ...process.env,
        CODDY_STUDIO_VCS_TERMINATE_IDENTITIES: JSON.stringify(identities),
      },
      stdio: "ignore",
      timeout: PROBE_TIMEOUT_MS,
    },
  );
}

async function waitForExit(
  exit: OwnedMeasurementProcess["exit"],
  timeoutMs: number,
): Promise<{ code: number | null; signal: NodeJS.Signals | null } | undefined> {
  const controller = new AbortController();
  const timeout = NodeTimersPromises.setTimeout(timeoutMs, undefined, {
    signal: controller.signal,
  });
  try {
    return await Promise.race([exit, timeout]);
  } finally {
    controller.abort();
    await Promise.allSettled([timeout]);
  }
}

async function shutdownOwnedMeasurementProcess(
  owned: OwnedMeasurementProcess,
): Promise<{ code: number | null; signal: NodeJS.Signals | null }> {
  if (owned.shutdown) return owned.shutdown;
  owned.shutdown = (async () => {
    const captured = new Map<string, WindowsProcessTreeIdentity>();
    const captureCurrentTree = () => {
      const snapshot = captureWindowsProcessSnapshot(
        owned.identity.pid,
        [...captured.values()].map((identity) => identity.pid),
      );
      if (!hasExactProcessIdentity(snapshot, owned.identity)) return;
      for (const identity of selectOwnedProcessTree(snapshot, owned.identity)) {
        captured.set(`${identity.pid}:${identity.startedAt}`, identity);
      }
    };
    captureCurrentTree();
    NodeFS.writeFileSync(owned.stopPath, "");
    captureCurrentTree();
    let terminal = await waitForExit(owned.exit, owned.gracefulTimeoutMs ?? SHUTDOWN_TIMEOUT_MS);
    captureCurrentTree();
    const beforeTermination = captureWindowsProcessSnapshot(
      owned.identity.pid,
      [...captured.values()].map((identity) => identity.pid),
    );
    const live = [...captured.values()]
      .filter((identity) => hasExactProcessIdentity(beforeTermination, identity))
      .toSorted((left, right) => right.depth - left.depth);
    terminateExactProcesses(live.filter((identity) => identity.pid !== owned.identity.pid));
    if (hasExactProcessIdentity(beforeTermination, owned.identity)) {
      owned.child.kill();
    }
    terminal ??= await waitForExit(owned.exit, SHUTDOWN_TIMEOUT_MS);
    if (!terminal) {
      throw new VcsRuntimeMeasurementError(
        "Measurement server did not exit after exact-tree termination.",
      );
    }
    const finalSnapshot = captureWindowsProcessSnapshot(
      undefined,
      [...captured.values()].map((identity) => identity.pid),
    );
    const survivors = [...captured.values()].filter((identity) =>
      hasExactProcessIdentity(finalSnapshot, identity),
    );
    if (survivors.length !== 0) {
      throw new VcsRuntimeMeasurementError("A captured measurement process survived cleanup.");
    }
    return terminal;
  })();
  return owned.shutdown;
}

export async function runWithOwnedMeasurementProcess<T>(
  owned: OwnedMeasurementProcess,
  operation: () => Promise<T>,
): Promise<T> {
  try {
    return await operation();
  } finally {
    await shutdownOwnedMeasurementProcess(owned);
  }
}

function directGitChildCount(pid: number): number {
  const script = [
    `$rows = @(Get-CimInstance Win32_Process -Filter "ParentProcessId=${pid}" | Where-Object { $_.Name -ieq 'git.exe' })`,
    "$rows.Count",
  ].join("\n");
  return Number(
    NodeChildProcess.execFileSync(
      "powershell.exe",
      ["-NoProfile", "-NonInteractive", "-Command", script],
      { encoding: "utf8" },
    ).trim(),
  );
}

async function waitFor(
  predicate: () => boolean,
  timeoutMs: number,
  message: string,
  childExit?: Promise<never>,
): Promise<void> {
  const deadline = NodePerfHooks.performance.now() + timeoutMs;
  while (NodePerfHooks.performance.now() < deadline) {
    if (predicate()) return;
    const sleep = NodeTimersPromises.setTimeout(100);
    await (childExit ? Promise.race([sleep, childExit]) : sleep);
  }
  throw new VcsRuntimeMeasurementError(message);
}

async function waitForQuiescentGit(pid: number): Promise<void> {
  await waitFor(
    () => directGitChildCount(pid) === 0,
    PROBE_TIMEOUT_MS,
    "Could not establish a quiescent direct-Git boundary.",
  );
}

function fileSha256(filePath: string): string {
  return NodeCrypto.createHash("sha256").update(NodeFS.readFileSync(filePath)).digest("hex");
}

function writeJson(filePath: string, value: unknown): void {
  NodeFS.writeFileSync(filePath, `${JSON.stringify(value, null, 2)}\n`);
}

function runQueueBenchmark(
  repositoryRoot: string,
  warmups: number,
  samples: number,
): Record<string, unknown> {
  const vp = commandPath("vp.exe");
  const output = NodeChildProcess.execFileSync(
    vp,
    [
      "test",
      "run",
      "packages/client-runtime/src/state/vcsQueueBenchmark.test.ts",
      "--reporter=verbose",
    ],
    {
      cwd: repositoryRoot,
      encoding: "utf8",
      env: {
        ...process.env,
        CODDY_STUDIO_VCS_QUEUE_WARMUPS: String(warmups),
        CODDY_STUDIO_VCS_QUEUE_SAMPLES: String(samples),
      },
      maxBuffer: 16 * 1024 * 1024,
    },
  );
  const marker = output.match(/VCS_QUEUE_BENCHMARK (\{[^\r\n]+\})/);
  if (!marker) {
    throw new VcsRuntimeMeasurementError("Production-Atom queue benchmark emitted no summary.");
  }
  return JSON.parse(marker[1]!) as Record<string, unknown>;
}

export async function measureVcsRuntime(
  input: MeasureVcsRuntimeInput,
): Promise<Record<string, unknown>> {
  // oxlint-disable-next-line coddy/no-global-process-runtime -- Standalone controller selects its supported native host once.
  if (process.platform !== "win32") {
    throw new VcsRuntimeMeasurementError("VCS runtime measurement is supported only on Windows.");
  }
  const repositoryRoot = NodePath.resolve(import.meta.dirname, "..");
  const outputDirectory = input.outputDirectory
    ? NodePath.resolve(input.outputDirectory)
    : NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "coddy-studio-vcs-runtime-"));
  if (input.outputDirectory) NodeFS.mkdirSync(outputDirectory);
  const fixtureRepository = NodePath.join(outputDirectory, "repository");
  const fixtureRemote = NodePath.join(outputDirectory, "remote.git");
  const stateDirectory = NodePath.join(outputDirectory, "state");
  const shimDirectory = NodePath.join(outputDirectory, "shim");
  const readyPath = NodePath.join(outputDirectory, "ready.json");
  const stopPath = NodePath.join(outputDirectory, "stop");
  const gitLogPath = NodePath.join(outputDirectory, "git-launches.jsonl");
  const serverStdoutPath = NodePath.join(outputDirectory, "server.stdout.log");
  const serverStderrPath = NodePath.join(outputDirectory, "server.stderr.log");
  const gitSummaryPath = NodePath.join(outputDirectory, "git-summary.json");
  const queueSummaryPath = NodePath.join(outputDirectory, "queue-summary.json");
  const summaryPath = NodePath.join(outputDirectory, "summary.json");
  NodeFS.mkdirSync(fixtureRepository);
  NodeFS.mkdirSync(fixtureRemote);
  NodeFS.mkdirSync(stateDirectory);
  NodeFS.mkdirSync(shimDirectory);

  const realGit = commandPath("git.exe");
  runGit(realGit, fixtureRemote, ["init", "--bare", "--initial-branch=main"]);
  runGit(realGit, fixtureRepository, ["init", "--initial-branch=main"]);
  runGit(realGit, fixtureRepository, ["config", "user.name", "Coddy Studio VCS Measurement"]);
  runGit(realGit, fixtureRepository, ["config", "user.email", "vcs-measure@example.invalid"]);
  runGit(realGit, fixtureRepository, ["config", "commit.gpgSign", "false"]);
  runGit(realGit, fixtureRepository, ["commit", "--allow-empty", "-m", "baseline"]);
  runGit(realGit, fixtureRepository, ["remote", "add", "origin", fixtureRemote]);
  runGit(realGit, fixtureRepository, ["push", "-u", "origin", "main"]);
  const worktreeCount = runGit(realGit, fixtureRepository, ["worktree", "list", "--porcelain"])
    .split(/\r?\n/)
    .filter((line) => line.startsWith("worktree ")).length;
  if (worktreeCount !== 1) {
    throw new VcsRuntimeMeasurementError(`Expected one worktree, observed ${worktreeCount}.`);
  }
  const rawCommonDir = runGit(realGit, fixtureRepository, ["rev-parse", "--git-common-dir"]);
  const commonDir = NodeFS.realpathSync(
    NodePath.isAbsolute(rawCommonDir)
      ? rawCommonDir
      : NodePath.resolve(fixtureRepository, rawCommonDir),
  );
  const build = createMeasurementBuildPlan(outputDirectory, process.env);

  const cargoOutput = NodeChildProcess.execFileSync(
    process.execPath,
    [
      "scripts/run-msvc.mjs",
      "cargo",
      "build",
      "-p",
      "coddy-studio-server",
      "--example",
      "measure_vcs_runtime_server",
      "--example",
      "measure_vcs_git_shim",
      "--message-format=json-render-diagnostics",
    ],
    {
      cwd: repositoryRoot,
      env: build.environment,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "inherit"],
      maxBuffer: 64 * 1024 * 1024,
    },
  );
  const artifacts = parseCargoExampleArtifacts(cargoOutput, build.targetDirectory);
  const serverExecutable = artifacts.serverExecutable;
  const builtShim = artifacts.shimExecutable;
  const shimExecutable = NodePath.join(shimDirectory, "git.exe");
  NodeFS.copyFileSync(builtShim, shimExecutable);
  const serverStat = NodeFS.statSync(serverExecutable);
  const serverHash = fileSha256(serverExecutable);
  const stdout = NodeFS.openSync(serverStdoutPath, "w");
  const stderr = NodeFS.openSync(serverStderrPath, "w");
  const child = NodeChildProcess.spawn(
    serverExecutable,
    [stateDirectory, fixtureRepository, commonDir, readyPath, stopPath],
    {
      cwd: repositoryRoot,
      env: {
        ...process.env,
        PATH: `${shimDirectory}${NodePath.delimiter}${process.env.PATH ?? ""}`,
        CODDY_STUDIO_VCS_MEASURE_REAL_GIT: realGit,
        CODDY_STUDIO_VCS_MEASURE_GIT_LOG: gitLogPath,
      },
      stdio: ["ignore", stdout, stderr],
      windowsHide: true,
    },
  );
  NodeFS.closeSync(stdout);
  NodeFS.closeSync(stderr);
  if (child.pid === undefined) throw new VcsRuntimeMeasurementError("Server process has no PID.");
  const exit = new Promise<{ code: number | null; signal: NodeJS.Signals | null }>((resolve) => {
    child.once("exit", (code, signal) => resolve({ code, signal }));
  });
  const childExit = exit.then<never>(({ code, signal }) => {
    throw new VcsRuntimeMeasurementError(
      `Measurement server exited unexpectedly (code ${String(code)}, signal ${String(signal)}).`,
    );
  });
  void childExit.catch(() => undefined);
  const identity = captureWindowsProcessIdentity(child.pid);
  const owned = { child, identity, exit, stopPath };
  return runWithOwnedMeasurementProcess(owned, async () => {
    await waitFor(
      () => NodeFS.existsSync(readyPath),
      READY_TIMEOUT_MS,
      "Timed out waiting for the real VCS subscription snapshot.",
      childExit,
    );
    const ready = JSON.parse(NodeFS.readFileSync(readyPath, "utf8")) as ReadyEvidence;
    if (
      ready.pid !== child.pid ||
      NodeFS.realpathSync(ready.executable) !== NodeFS.realpathSync(serverExecutable) ||
      NodeFS.realpathSync(ready.commonDir) !== commonDir ||
      ready.physicalRepositories !== 1 ||
      ready.worktrees !== 1 ||
      ready.subscribers !== 1 ||
      NodeFS.realpathSync(identity.executable) !== NodeFS.realpathSync(serverExecutable)
    ) {
      throw new VcsRuntimeMeasurementError("Ready evidence does not match the controlled runtime.");
    }
    await waitFor(
      () => {
        if (!NodeFS.existsSync(gitLogPath)) return false;
        return parseGitLaunchLog(NodeFS.readFileSync(gitLogPath, "utf8")).some(
          (record) =>
            record.parentPid === identity.pid &&
            record.parentStartedAt === identity.startedAt &&
            record.args.includes("--git-common-dir"),
        );
      },
      PROBE_TIMEOUT_MS,
      "The physical-repository owner did not emit common-directory discovery.",
      childExit,
    );
    await waitForQuiescentGit(identity.pid);
    NodeFS.writeFileSync(gitLogPath, "");
    await waitFor(
      () => {
        const records = parseGitLaunchLog(NodeFS.readFileSync(gitLogPath, "utf8"));
        return records.some(
          (record) =>
            record.parentPid === identity.pid && record.parentStartedAt === identity.startedAt,
        );
      },
      PROBE_TIMEOUT_MS,
      "The serialized post-clear Git probe produced no direct launch.",
      childExit,
    );
    await waitForQuiescentGit(identity.pid);
    NodeFS.writeFileSync(gitLogPath, "");
    const startInclusiveMs = currentEpochMs();
    const endExclusiveMs = startInclusiveMs + input.durationMs;
    while (currentEpochMs() < endExclusiveMs) {
      const remaining = endExclusiveMs - currentEpochMs();
      await Promise.race([NodeTimersPromises.setTimeout(Math.min(30_000, remaining)), childExit]);
      const current = captureWindowsProcessIdentity(identity.pid);
      if (current.startedAt !== identity.startedAt || current.executable !== identity.executable) {
        throw new VcsRuntimeMeasurementError("Measurement server process identity changed.");
      }
    }
    const terminal = await shutdownOwnedMeasurementProcess(owned);
    if (terminal.code !== 0 || terminal.signal !== null) {
      throw new VcsRuntimeMeasurementError(
        `Measurement server stopped abnormally (code ${String(terminal.code)}, signal ${String(terminal.signal)}).`,
      );
    }
    const logText = NodeFS.readFileSync(gitLogPath, "utf8");
    const records = parseGitLaunchLog(logText);
    const gitSummary = summarizeGitLaunches(records, {
      serverPid: identity.pid,
      serverStartedAt: identity.startedAt,
      startInclusiveMs,
      endExclusiveMs,
      physicalRepositories: 1,
    });
    if (gitSummary.wrongParentIdentityRecords !== 0) {
      throw new VcsRuntimeMeasurementError(
        `Git log contains ${gitSummary.wrongParentIdentityRecords} direct-parent identity failures.`,
      );
    }
    writeJson(gitSummaryPath, gitSummary);
    const queueSummary = runQueueBenchmark(repositoryRoot, input.queueWarmups, input.queueSamples);
    writeJson(queueSummaryPath, queueSummary);
    const summary = {
      outputDirectory,
      durationMs: input.durationMs,
      server: {
        pid: identity.pid,
        startedAtFileTime: identity.startedAt,
        executable: serverExecutable,
        executableBytes: serverStat.size,
        executableSha256: serverHash,
        address: ready.address,
      },
      fixture: {
        repository: fixtureRepository,
        commonDir,
        physicalRepositories: 1,
        worktrees: worktreeCount,
        subscribers: 1,
      },
      evidence: {
        ready: readyPath,
        gitLog: gitLogPath,
        gitSummary: gitSummaryPath,
        queueSummary: queueSummaryPath,
        serverStdout: serverStdoutPath,
        serverStderr: serverStderrPath,
      },
      git: gitSummary,
      queue: queueSummary,
    };
    writeJson(summaryPath, summary);
    return summary;
  });
}

export async function runMeasureVcsRuntimeMain(
  isMain: boolean,
  argv: ReadonlyArray<string>,
): Promise<boolean> {
  if (!isMain) return false;
  try {
    const result = await measureVcsRuntime(parseMeasureVcsRuntimeArgs(argv));
    process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
  } catch (cause) {
    process.stderr.write(`${cause instanceof Error ? cause.message : String(cause)}\n`);
    process.exitCode = 1;
  }
  return true;
}

void runMeasureVcsRuntimeMain(import.meta.main, process.argv.slice(2));
