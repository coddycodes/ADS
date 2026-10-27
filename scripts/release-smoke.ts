// @effect-diagnostics nodeBuiltinImport:off
import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import * as NodeURL from "node:url";

import {
  releaseCargoLockFile,
  releasePackageFiles,
  releaseRustPackageFiles,
} from "./update-release-package-versions.ts";

const defaultRepoRoot = NodePath.resolve(
  NodePath.dirname(NodeURL.fileURLToPath(import.meta.url)),
  "..",
);

export const releaseSmokeWorkspaceFiles = [
  "package.json",
  "pnpm-lock.yaml",
  "pnpm-workspace.yaml",
  ...releasePackageFiles,
  ...releaseRustPackageFiles,
  releaseCargoLockFile,
  "apps/marketing/package.json",
  "infra/relay/package.json",
  "oxlint-plugin-coddy-studio/package.json",
  "packages/client-runtime/package.json",
  "packages/shared/package.json",
  "scripts/package.json",
] as const;

const releaseSmokeVersion = "9.9.9-smoke.0";

export interface ReleaseSmokeExecOptions {
  readonly cwd: string;
  readonly encoding?: "utf8";
  readonly stdio?: "inherit";
}

export interface ReleaseSmokeSpawnOptions {
  readonly cwd: string;
  readonly encoding: "utf8";
  readonly stdio: ["ignore", "pipe", "pipe"];
}

export interface ReleaseSmokeSpawnResult {
  readonly stdout: string;
  readonly stderr: string;
  readonly status: number | null;
  readonly error?: Error;
}

export interface ReleaseSmokeRuntime {
  readonly execFile: (
    command: string,
    args: ReadonlyArray<string>,
    options: ReleaseSmokeExecOptions,
  ) => string;
  readonly spawn: (
    command: string,
    args: ReadonlyArray<string>,
    options: ReleaseSmokeSpawnOptions,
  ) => ReleaseSmokeSpawnResult;
}

export interface ReleaseSmokeOptions {
  readonly repoRoot?: string;
  readonly tempRoot?: string;
  readonly runtime: ReleaseSmokeRuntime;
  readonly stdout?: (text: string) => void;
  readonly stderr?: (text: string) => void;
  readonly log?: (text: string) => void;
}

export function makeReleaseSmokeRuntime(
  childProcess: Pick<typeof NodeChildProcess, "execFileSync" | "spawnSync"> = NodeChildProcess,
): ReleaseSmokeRuntime {
  return {
    execFile(command, args, options) {
      const output = childProcess.execFileSync(command, [...args], options);
      return typeof output === "string" ? output : "";
    },
    spawn(command, args, options) {
      const result = childProcess.spawnSync(command, [...args], options);
      return {
        stdout: String(result.stdout),
        stderr: String(result.stderr),
        status: result.status,
        ...(result.error ? { error: result.error } : {}),
      };
    },
  };
}

const defaultRuntime = makeReleaseSmokeRuntime();

export function copyWorkspaceManifestFixture(sourceRoot: string, targetRoot: string): void {
  for (const relativePath of releaseSmokeWorkspaceFiles) {
    const sourcePath = NodePath.resolve(sourceRoot, relativePath);
    const destinationPath = NodePath.resolve(targetRoot, relativePath);
    NodeFS.mkdirSync(NodePath.dirname(destinationPath), { recursive: true });
    NodeFS.cpSync(sourcePath, destinationPath);
  }

  const patchesDirectory = NodePath.resolve(sourceRoot, "patches");
  if (NodeFS.existsSync(patchesDirectory)) {
    NodeFS.cpSync(patchesDirectory, NodePath.resolve(targetRoot, "patches"), { recursive: true });
  }
}

function assertContains(haystack: string, needle: string, message: string): void {
  if (!haystack.includes(needle)) throw new Error(message);
}

function assertPackageVersion(filePath: string, version: string): void {
  const packageJson = JSON.parse(NodeFS.readFileSync(filePath, "utf8")) as {
    readonly version?: unknown;
  };
  if (packageJson.version !== version) {
    throw new Error(`Expected ${filePath} to have version ${version}.`);
  }
}

function assertCargoVersion(filePath: string, version: string, expectedOccurrences: number): void {
  const source = NodeFS.readFileSync(filePath, "utf8");
  const escapedVersion = version.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const occurrences = source.match(
    new RegExp(`^version\\s*=\\s*"${escapedVersion}"\\s*$`, "gm"),
  )?.length;
  if (occurrences !== expectedOccurrences) {
    throw new Error(
      `Expected ${filePath} to contain ${expectedOccurrences} Cargo version entries for ${version}.`,
    );
  }
}

function writeFilteredInstallOutput(output: string, write: (text: string) => void): void {
  const filteredOutput = output
    .split(/\r?\n/)
    .filter((line) => !line.includes("deprecated subdependencies found"))
    .join("\n");
  if (filteredOutput.trim() !== "") {
    write(`${filteredOutput.replace(/\n+$/, "")}\n`);
  }
}

function runLockfileInstall(
  targetRoot: string,
  runtime: ReleaseSmokeRuntime,
  stdout: (text: string) => void,
  stderr: (text: string) => void,
): void {
  const result = runtime.spawn("vp", ["install", "--lockfile-only", "--ignore-scripts"], {
    cwd: targetRoot,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  writeFilteredInstallOutput(result.stdout, stdout);
  writeFilteredInstallOutput(result.stderr, stderr);
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error("Command failed: vp install --lockfile-only --ignore-scripts");
  }
}

export function runReleaseSmoke(options: ReleaseSmokeOptions): void {
  const repoRoot = options.repoRoot ?? defaultRepoRoot;
  const tempRoot =
    options.tempRoot ??
    NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "coddy-studio-release-smoke-"));
  const runtime = options.runtime;
  const stdout = options.stdout ?? ((text: string) => process.stdout.write(text));
  const stderr = options.stderr ?? ((text: string) => process.stderr.write(text));
  const log = options.log ?? ((text: string) => process.stdout.write(`${text}\n`));

  try {
    copyWorkspaceManifestFixture(repoRoot, tempRoot);
    runtime.execFile(
      process.execPath,
      [
        NodePath.resolve(repoRoot, "scripts/update-release-package-versions.ts"),
        releaseSmokeVersion,
        "--root",
        tempRoot,
      ],
      { cwd: repoRoot, stdio: "inherit" },
    );

    NodeFS.rmSync(NodePath.resolve(tempRoot, "pnpm-lock.yaml"), { force: true });
    runLockfileInstall(tempRoot, runtime, stdout, stderr);
    const lockfile = NodeFS.readFileSync(NodePath.resolve(tempRoot, "pnpm-lock.yaml"), "utf8");
    assertContains(lockfile, "lockfileVersion:", "Expected pnpm-lock.yaml to be regenerated.");

    for (const relativePath of [
      "apps/server/package.json",
      "apps/desktop/package.json",
      "apps/web/package.json",
      "packages/contracts/package.json",
    ]) {
      assertPackageVersion(NodePath.resolve(tempRoot, relativePath), releaseSmokeVersion);
    }
    for (const relativePath of releaseRustPackageFiles) {
      assertCargoVersion(NodePath.resolve(tempRoot, relativePath), releaseSmokeVersion, 1);
    }
    assertCargoVersion(
      NodePath.resolve(tempRoot, releaseCargoLockFile),
      releaseSmokeVersion,
      releaseRustPackageFiles.length,
    );

    const nightlyReleaseMetadata = runtime.execFile(
      process.execPath,
      [
        NodePath.resolve(repoRoot, "scripts/resolve-nightly-release.ts"),
        "--date",
        "20260413",
        "--run-number",
        "321",
        "--sha",
        "abcdef1234567890",
        "--root",
        tempRoot,
      ],
      { cwd: repoRoot, encoding: "utf8" },
    );
    assertContains(
      nightlyReleaseMetadata,
      "version=9.9.10-nightly.20260413.321",
      "Expected nightly metadata to contain the derived nightly version.",
    );
    assertContains(
      nightlyReleaseMetadata,
      "tag=v9.9.10-nightly.20260413.321",
      "Expected nightly metadata to contain the derived nightly tag.",
    );
    assertContains(
      nightlyReleaseMetadata,
      "name=Coddy Studio Nightly 9.9.10-nightly.20260413.321 (abcdef123456)",
      "Expected nightly metadata to include the short commit SHA in the release name.",
    );

    log("Release smoke checks passed.");
  } finally {
    NodeFS.rmSync(tempRoot, { recursive: true, force: true });
  }
}

export function runReleaseSmokeMain(isMain: boolean, options: ReleaseSmokeOptions): boolean {
  if (!isMain) return false;
  runReleaseSmoke(options);
  return true;
}

runReleaseSmokeMain(import.meta.main, { runtime: defaultRuntime });
