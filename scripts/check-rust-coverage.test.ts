// @effect-diagnostics nodeBuiltinImport:off - Coverage runner tests inspect the tooling script on disk and stub process execution.
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";
import { describe, expect, it } from "vite-plus/test";

const REPOSITORY_ROOT = NodePath.resolve(import.meta.dirname, "..");
const SCRIPT_PATH = NodePath.join(REPOSITORY_ROOT, "scripts", "check-rust-coverage.ts");

async function importModule() {
  return import("./check-rust-coverage.ts");
}

function spawnResult(status: number | null) {
  return {
    status,
  };
}

describe("Rust coverage runner", () => {
  it("exists as repository tooling", () => {
    expect(NodeFS.existsSync(SCRIPT_PATH)).toBe(true);
  });

  it("constructs the enforced cargo-llvm-cov command on non-Windows hosts", async () => {
    const { buildRustCoverageCommand } = await importModule();

    expect(
      buildRustCoverageCommand({
        platform: "linux",
        repoRoot: "/repo",
      }),
    ).toEqual({
      command: "cargo",
      args: [
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
        "--json",
        "--output-path",
        NodePath.join("/repo", "target", "llvm-cov-report.json"),
      ],
      cwd: "/repo",
    });
  });

  it("routes Windows coverage through the MSVC bootstrap helper", async () => {
    const { buildRustCoverageCommand } = await importModule();

    expect(
      buildRustCoverageCommand({
        platform: "win32",
        repoRoot: "X:/coddy-studio",
      }),
    ).toEqual({
      command: process.execPath,
      args: [
        NodePath.join("X:/coddy-studio", "scripts", "run-msvc.mjs"),
        "cargo",
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
        "--json",
        "--output-path",
        NodePath.join("X:/coddy-studio", "target", "llvm-cov-report.json"),
      ],
      cwd: "X:/coddy-studio",
    });
  });

  it("propagates the underlying process exit code", async () => {
    const { runRustCoverageCheck } = await importModule();

    const exitCode = runRustCoverageCheck({
      platform: "linux",
      repoRoot: "/repo",
      spawnSync: () => spawnResult(7),
    });

    expect(exitCode).toBe(7);
  });

  it("counts source functions once across duplicate crate instantiations", async () => {
    const { summarizeSourceFunctionCoverage } = await importModule();
    const report = {
      data: [
        {
          functions: [
            {
              count: 0,
              filenames: ["/repo/src/lib.rs"],
              regions: [[10, 1, 12, 2]],
            },
            {
              count: 4,
              filenames: ["/repo/src/lib.rs"],
              regions: [[10, 1, 12, 2]],
            },
            {
              count: 0,
              filenames: ["/repo/src/lib.rs"],
              regions: [[20, 1, 21, 2]],
            },
            {
              count: 0,
              filenames: ["/external/registry/src/dependency.rs"],
              regions: [[1, 1, 2, 2]],
            },
          ],
        },
      ],
    };

    expect(summarizeSourceFunctionCoverage(report, "/repo")).toEqual({
      total: 2,
      covered: 1,
      percent: 50,
    });
  });

  it("maps null process status to a failing exit code", async () => {
    const { runRustCoverageCheck } = await importModule();

    const exitCode = runRustCoverageCheck({
      platform: "linux",
      repoRoot: "/repo",
      spawnSync: () => spawnResult(null),
    });

    expect(exitCode).toBe(1);
  });

  it("surfaces spawnSync.error when the coverage child cannot start", async () => {
    const { runRustCoverageCheck } = await importModule();
    const spawnError = Object.assign(new Error("spawn cargo ENOENT"), {
      code: "ENOENT",
    });

    expect(() =>
      runRustCoverageCheck({
        platform: "linux",
        repoRoot: "/repo",
        spawnSync: () => ({
          status: null,
          error: spawnError,
        }),
      }),
    ).toThrowError(/Failed to start Rust coverage command "cargo"/);
  });
});
