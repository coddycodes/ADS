// @effect-diagnostics nodeBuiltinImport:off - Native controller tests own disposable host processes and paths.
import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import * as NodeTimersPromises from "node:timers/promises";

import { describe, expect, it } from "vite-plus/test";

import {
  VcsRuntimeMeasurementError,
  captureWindowsProcessIdentity,
  classifyGitArguments,
  createMeasurementBuildPlan,
  hasExactProcessIdentity,
  parseCargoExampleArtifacts,
  parseGitLaunchLog,
  parseMeasureVcsRuntimeArgs,
  parseWindowsProcessSnapshot,
  runWithOwnedMeasurementProcess,
  selectOwnedProcessTree,
  summarizeGitLaunches,
  type GitLaunchRecord,
} from "./measure-vcs-runtime.ts";

const record = (
  timestampMs: number,
  parentPid: number,
  parentStartedAt: string,
  args: readonly string[],
): GitLaunchRecord => ({
  timestampMs,
  pid: timestampMs + 10,
  startedAt: String(timestampMs + 20),
  parentPid,
  parentStartedAt,
  args,
});

describe("measure VCS runtime arguments", () => {
  it("uses the ten-minute and production queue defaults", () => {
    expect(parseMeasureVcsRuntimeArgs([])).toEqual({
      durationMs: 600_000,
      queueWarmups: 20,
      queueSamples: 200,
    });
  });

  it("accepts a short smoke duration and bounded queue sizes", () => {
    expect(
      parseMeasureVcsRuntimeArgs([
        "--duration-ms",
        "3000",
        "--queue-warmups",
        "2",
        "--queue-samples",
        "10",
        "--output-dir",
        "C:/evidence",
      ]),
    ).toEqual({
      durationMs: 3000,
      queueWarmups: 2,
      queueSamples: 10,
      outputDirectory: "C:/evidence",
    });
  });
});

describe("measurement artifact identity", () => {
  it("uses compiler artifacts under a target triple and never selects a stale root artifact", () => {
    const root = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "coddy-studio-vcs-build-plan-"));
    try {
      const repositoryRoot = NodePath.join(root, "repository");
      const outputDirectory = NodePath.join(root, "evidence");
      const stale = NodePath.join(
        repositoryRoot,
        "target/debug/examples/measure_vcs_runtime_server.exe",
      );
      NodeFS.mkdirSync(NodePath.dirname(stale), { recursive: true });
      NodeFS.writeFileSync(stale, "stale");

      const plan = createMeasurementBuildPlan(outputDirectory, {
        CARGO_TARGET_DIR: NodePath.join(root, "alternate-target"),
        CARGO_BUILD_TARGET: "x86_64-pc-windows-msvc",
      });

      expect(plan.environment.CARGO_TARGET_DIR).toBe(
        NodePath.join(outputDirectory, "cargo-target"),
      );
      expect(plan.environment.CARGO_BUILD_TARGET).toBe("x86_64-pc-windows-msvc");
      const artifacts = parseCargoExampleArtifacts(
        [
          JSON.stringify({
            reason: "compiler-artifact",
            target: { name: "measure_vcs_runtime_server", kind: ["example"] },
            executable: NodePath.join(
              outputDirectory,
              "cargo-target/x86_64-pc-windows-msvc/debug/examples/measure_vcs_runtime_server.exe",
            ),
          }),
          JSON.stringify({
            reason: "compiler-artifact",
            target: { name: "measure_vcs_git_shim", kind: ["example"] },
            executable: NodePath.join(
              outputDirectory,
              "cargo-target/x86_64-pc-windows-msvc/debug/examples/measure_vcs_git_shim.exe",
            ),
          }),
        ].join("\n"),
        plan.targetDirectory,
      );
      expect(artifacts.serverExecutable).toContain("x86_64-pc-windows-msvc");
      expect(artifacts.serverExecutable).not.toBe(stale);
      expect(artifacts.shimExecutable).toContain("x86_64-pc-windows-msvc");
    } finally {
      NodeFS.rmSync(root, { recursive: true, force: true });
    }
  });

  it("rejects missing, duplicate, and out-of-target compiler artifacts", () => {
    const artifact = (name: string, executable: string) =>
      JSON.stringify({
        reason: "compiler-artifact",
        target: { name, kind: ["example"] },
        executable,
      });
    expect(() =>
      parseCargoExampleArtifacts(
        artifact("measure_vcs_runtime_server", "C:/evidence/cargo-target/server.exe"),
        "C:/evidence/cargo-target",
      ),
    ).toThrow("missing");
    expect(() =>
      parseCargoExampleArtifacts(
        [
          artifact("measure_vcs_runtime_server", "C:/evidence/cargo-target/server.exe"),
          artifact("measure_vcs_runtime_server", "C:/evidence/cargo-target/server-2.exe"),
          artifact("measure_vcs_git_shim", "C:/evidence/cargo-target/shim.exe"),
        ].join("\n"),
        "C:/evidence/cargo-target",
      ),
    ).toThrow("duplicate");
    expect(() =>
      parseCargoExampleArtifacts(
        [
          artifact("measure_vcs_runtime_server", "C:/stale/server.exe"),
          artifact("measure_vcs_git_shim", "C:/evidence/cargo-target/shim.exe"),
        ].join("\n"),
        "C:/evidence/cargo-target",
      ),
    ).toThrow("outside");
  });
});

describe("atomic Windows process snapshots", () => {
  it("builds the immutable descendant closure and rejects PID reuse", () => {
    const captured = parseWindowsProcessSnapshot(
      JSON.stringify([
        { pid: 10, parentPid: 1, startedAt: "100", executable: "C:/fixture/server.exe" },
        { pid: 11, parentPid: 10, startedAt: "110", executable: "C:/fixture/git.exe" },
        { pid: 12, parentPid: 11, startedAt: "120", executable: "C:/fixture/git-child.exe" },
      ]),
    );
    const tree = selectOwnedProcessTree(captured, captured[0]!);
    expect(tree.map(({ pid, depth }) => [pid, depth])).toEqual([
      [12, 2],
      [11, 1],
      [10, 0],
    ]);

    const reused = parseWindowsProcessSnapshot(
      JSON.stringify([
        { pid: 10, parentPid: 1, startedAt: "100", executable: "C:/fixture/server.exe" },
        { pid: 11, parentPid: 10, startedAt: "999", executable: "C:/fixture/git.exe" },
      ]),
    );
    expect(hasExactProcessIdentity(reused, tree[1]!)).toBe(false);
    expect(
      hasExactProcessIdentity(
        parseWindowsProcessSnapshot(
          JSON.stringify([
            { pid: 11, parentPid: 10, startedAt: "110", executable: "C:/other/git.exe" },
          ]),
        ),
        tree[1]!,
      ),
    ).toBe(false);
  });

  it("rejects duplicate snapshot PIDs and invalid decimal FILETIME", () => {
    expect(() =>
      parseWindowsProcessSnapshot(
        JSON.stringify([
          { pid: 10, parentPid: 1, startedAt: "100", executable: "C:/server.exe" },
          { pid: 10, parentPid: 1, startedAt: "101", executable: "C:/replacement.exe" },
        ]),
      ),
    ).toThrow("duplicate");
    expect(() =>
      parseWindowsProcessSnapshot(
        JSON.stringify([{ pid: 10, parentPid: 1, startedAt: "1.5", executable: "C:/server.exe" }]),
      ),
    ).toThrow("startedAt");
  });
});

// oxlint-disable-next-line coddy/no-global-process-runtime -- Native Windows process cleanup smoke is unavailable on other hosts.
const windowsIt = NodeOS.platform() === "win32" ? it : it.skip;

describe("owned measurement process cleanup", () => {
  const spawnTree = async (root: string, graceful: boolean) => {
    const stopPath = NodePath.join(root, "stop");
    const readyPath = NodePath.join(root, "descendants.json");
    const childScript = [
      'const { spawn } = require("node:child_process")',
      'const fs = require("node:fs")',
      "const grandchild = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], { stdio: 'ignore', windowsHide: true })",
      "fs.writeFileSync(process.argv[1], JSON.stringify([process.pid, grandchild.pid]))",
      "setInterval(() => {}, 1000)",
    ].join(";");
    const parentScript = [
      'const { spawn } = require("node:child_process")',
      'const fs = require("node:fs")',
      `spawn(process.execPath, ['-e', ${JSON.stringify(childScript)}, process.argv[1]], { stdio: 'ignore', windowsHide: true })`,
      graceful
        ? "setInterval(() => { if (fs.existsSync(process.argv[2])) process.exit(0) }, 10)"
        : "setInterval(() => {}, 1000)",
    ].join(";");
    const child = NodeChildProcess.spawn(
      process.execPath,
      ["-e", parentScript, readyPath, stopPath],
      {
        stdio: "ignore",
        windowsHide: true,
      },
    );
    const exit = new Promise<{ code: number | null; signal: NodeJS.Signals | null }>((resolve) => {
      child.once("exit", (code, signal) => resolve({ code, signal }));
    });
    for (let attempts = 0; !NodeFS.existsSync(readyPath) && attempts < 100; attempts += 1) {
      await NodeTimersPromises.setTimeout(10);
    }
    expect(NodeFS.existsSync(readyPath)).toBe(true);
    return {
      child,
      exit,
      identity: captureWindowsProcessIdentity(child.pid!),
      stopPath,
    };
  };

  windowsIt(
    "reaps an exact child and grandchild after a forced timeout",
    async () => {
      const root = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "coddy-studio-vcs-timeout-"));
      try {
        const owned = await spawnTree(root, false);
        await expect(
          runWithOwnedMeasurementProcess({ ...owned, gracefulTimeoutMs: 50 }, async () => {
            throw new Error("active-probe");
          }),
        ).rejects.toThrow("active-probe");
      } finally {
        NodeFS.rmSync(root, { recursive: true, force: true });
      }
    },
    180_000,
  );

  windowsIt(
    "reaps an orphaned child and grandchild after graceful parent exit",
    async () => {
      const root = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "coddy-studio-vcs-graceful-"));
      try {
        const owned = await spawnTree(root, true);
        await expect(runWithOwnedMeasurementProcess(owned, async () => "ok")).resolves.toBe("ok");
      } finally {
        NodeFS.rmSync(root, { recursive: true, force: true });
      }
    },
    180_000,
  );
});

describe("Git launch parser", () => {
  it("decodes complete records and rejects a partial concurrent write", () => {
    const encoded = `${JSON.stringify(record(100, 7, "70", ["remote"]))}\n`;
    expect(parseGitLaunchLog(encoded)).toEqual([record(100, 7, "70", ["remote"])]);
    expect(() => parseGitLaunchLog('{"timestampMs":100')).toThrow(VcsRuntimeMeasurementError);
  });

  it("rejects missing identity and argument fields", () => {
    expect(() => parseGitLaunchLog('{"timestampMs":100,"args":[]}')).toThrow("invalid pid");
    expect(() =>
      parseGitLaunchLog(
        '{"timestampMs":100,"pid":1,"startedAt":"2","parentPid":3,"parentStartedAt":"4","args":[5]}',
      ),
    ).toThrow("invalid args");
  });

  it("rejects duplicate shim process identities", () => {
    const duplicate = JSON.stringify(record(100, 7, "70", ["remote"]));
    expect(() => parseGitLaunchLog(`${duplicate}\n${duplicate}\n`)).toThrow(
      "repeats shim identity",
    );
  });
});

describe("Git launch identity, window, and classification", () => {
  it("keeps only exact direct-parent records in the half-open window", () => {
    const records = [
      record(100, 7, "70", ["symbolic-ref", "--quiet", "--short", "HEAD"]),
      record(199, 7, "70", ["fetch", "--quiet", "--", "origin"]),
      record(150, 8, "80", ["fetch", "--quiet", "--", "remote-child"]),
      record(151, 7, "71", ["remote"]),
      record(99, 7, "70", ["remote"]),
      record(200, 7, "70", ["remote"]),
    ];

    const summary = summarizeGitLaunches(records, {
      serverPid: 7,
      serverStartedAt: "70",
      startInclusiveMs: 100,
      endExclusiveMs: 200,
      physicalRepositories: 1,
    });

    expect(summary).toMatchObject({
      durationMs: 100,
      directLaunches: 2,
      recordsInsideWindow: 4,
      recordsOutsideWindow: 2,
      nonDirectRecords: 1,
      wrongParentIdentityRecords: 1,
    });
    expect(summary.argumentGroups).toEqual([
      {
        category: "current-ref",
        count: 1,
        args: ["symbolic-ref", "--quiet", "--short", "HEAD"],
      },
      {
        category: "fetch",
        count: 1,
        args: ["fetch", "--quiet", "--", "origin"],
      },
    ]);
  });

  it.each([
    [["-c", "core.quotePath=false", "status", "--untracked-files=all"], "local-status"],
    [["status", "--untracked-files=no"], "remote-status"],
    [["diff", "--numstat"], "numstat"],
    [["for-each-ref", "refs/heads"], "upstream-discovery"],
    [["rev-parse", "--git-common-dir"], "common-dir-discovery"],
    [["rev-parse", "--is-inside-work-tree"], "repository-probe"],
    [["remote"], "remote-list"],
    [["config", "--get", "remote.origin.url"], "provider-discovery"],
  ])("classifies %j as %s", (args, expected) => {
    expect(classifyGitArguments(args)).toBe(expected);
  });
});
