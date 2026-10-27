#!/usr/bin/env node
// @effect-diagnostics nodeBuiltinImport:off - This repository-level coverage gate shells out to Cargo tooling directly.
// @effect-diagnostics globalConsole:off - The standalone coverage gate reports its source-function result to the invoking terminal.
import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";

const REPOSITORY_ROOT = NodePath.resolve(import.meta.dirname, "..");
const RUST_COVERAGE_ARGS = [
  "llvm-cov",
  "--workspace",
  "--all-targets",
  "--include-build-script",
  "--fail-under-lines",
  "90",
  "--fail-under-regions",
  "90",
  "--jobs",
  "1",
] as const;

export interface RustCoverageCommand {
  readonly command: string;
  readonly args: readonly string[];
  readonly cwd: string;
}

export interface SourceFunctionCoverage {
  readonly total: number;
  readonly covered: number;
  readonly percent: number;
}

interface LlvmCoverageFunction {
  readonly count?: unknown;
  readonly filenames?: unknown;
  readonly regions?: unknown;
}

interface LlvmCoverageReport {
  readonly data?: ReadonlyArray<{
    readonly functions?: ReadonlyArray<LlvmCoverageFunction>;
  }>;
}

const SOURCE_FUNCTION_COVERAGE_MINIMUM = 90;
const COVERAGE_REPORT_FILENAME = "llvm-cov-report.json";

export interface SpawnSyncResultLike {
  readonly status: number | null;
  readonly error?: Error | undefined;
}

export type SpawnSyncLike = (
  command: string,
  args: readonly string[],
  options: NodeChildProcess.SpawnSyncOptions,
) => SpawnSyncResultLike;

export function buildRustCoverageCommand(
  options: {
    readonly platform?: NodeJS.Platform | undefined;
    readonly repoRoot?: string | undefined;
  } = {},
): RustCoverageCommand {
  // oxlint-disable-next-line coddy/no-global-process-runtime -- Standalone coverage gate targets the actual host platform when no explicit platform override is supplied.
  const platform = options.platform ?? process.platform;
  const repoRoot = options.repoRoot ?? REPOSITORY_ROOT;
  const reportPath = NodePath.join(repoRoot, "target", COVERAGE_REPORT_FILENAME);
  const args = [...RUST_COVERAGE_ARGS, "--json", "--output-path", reportPath] as const;

  if (platform === "win32") {
    return {
      command: process.execPath,
      args: [NodePath.join(repoRoot, "scripts", "run-msvc.mjs"), "cargo", ...args],
      cwd: repoRoot,
    };
  }

  return {
    command: "cargo",
    args,
    cwd: repoRoot,
  };
}

function isRepositoryFile(filename: string, repoRoot: string): boolean {
  const relative = NodePath.relative(repoRoot, filename);
  return relative !== "" && relative !== ".." && !relative.startsWith(`..${NodePath.sep}`);
}

export function summarizeSourceFunctionCoverage(
  report: unknown,
  repoRoot: string,
): SourceFunctionCoverage {
  const definitions = new Map<string, boolean>();
  const data = (report as LlvmCoverageReport).data ?? [];
  for (const unit of data) {
    for (const fn of unit.functions ?? []) {
      const filename =
        Array.isArray(fn.filenames) && typeof fn.filenames[0] === "string" ? fn.filenames[0] : null;
      const region =
        Array.isArray(fn.regions) && Array.isArray(fn.regions[0]) ? fn.regions[0] : null;
      if (
        filename === null ||
        !isRepositoryFile(filename, repoRoot) ||
        region === null ||
        region.length < 4 ||
        !region.slice(0, 4).every((value) => typeof value === "number")
      ) {
        continue;
      }
      const key = `${filename}:${region.slice(0, 4).join(":")}`;
      definitions.set(
        key,
        definitions.get(key) === true || (typeof fn.count === "number" && fn.count > 0),
      );
    }
  }
  const total = definitions.size;
  const covered = [...definitions.values()].filter(Boolean).length;
  return {
    total,
    covered,
    percent: total === 0 ? 0 : (covered / total) * 100,
  };
}

export function runRustCoverageCheck(
  options: {
    readonly platform?: NodeJS.Platform | undefined;
    readonly repoRoot?: string | undefined;
    readonly spawnSync?: SpawnSyncLike | undefined;
  } = {},
): number {
  const command = buildRustCoverageCommand(options);
  const spawnSync: SpawnSyncLike = options.spawnSync ?? NodeChildProcess.spawnSync;
  const result = spawnSync(command.command, [...command.args], {
    cwd: command.cwd,
    stdio: "inherit",
    shell: false,
  });

  if (result.error) {
    throw new Error(`Failed to start Rust coverage command "${command.command}".`, {
      cause: result.error,
    });
  }

  const status = result.status ?? 1;
  if (status !== 0) {
    return status;
  }
  const reportPath = NodePath.join(command.cwd, "target", COVERAGE_REPORT_FILENAME);
  const report = JSON.parse(NodeFS.readFileSync(reportPath, "utf8")) as unknown;
  const functions = summarizeSourceFunctionCoverage(report, command.cwd);
  const percent = functions.percent.toFixed(2);
  console.log(
    `Source functions: ${percent}% (${functions.covered}/${functions.total}; minimum ${SOURCE_FUNCTION_COVERAGE_MINIMUM}%)`,
  );
  return functions.percent >= SOURCE_FUNCTION_COVERAGE_MINIMUM ? 0 : 1;
}

if (import.meta.main) {
  process.exit(runRustCoverageCheck());
}
