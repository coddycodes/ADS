// @effect-diagnostics nodeBuiltinImport:off
import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";

import { HostProcessPlatform } from "@coddy/shared/hostProcess";
import { assert, it } from "@effect/vitest";
import { Effect } from "effect";
import { vi } from "vite-plus/test";

import {
  copyWorkspaceManifestFixture,
  makeReleaseSmokeRuntime,
  releaseSmokeWorkspaceFiles,
  runReleaseSmoke,
  runReleaseSmokeMain,
  type ReleaseSmokeRuntime,
} from "./release-smoke.ts";

const releasePackageJsonFiles = [
  "apps/server/package.json",
  "apps/desktop/package.json",
  "apps/web/package.json",
  "packages/contracts/package.json",
] as const;

const releaseRustFiles = ["apps/server/Cargo.toml", "apps/desktop/src-tauri/Cargo.toml"] as const;

function makeFixtureRoot(includePatches = true): string {
  const root = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "coddy-studio-release-source-"));
  for (const relativePath of releaseSmokeWorkspaceFiles) {
    const filePath = NodePath.join(root, relativePath);
    NodeFS.mkdirSync(NodePath.dirname(filePath), { recursive: true });
    if (relativePath.endsWith("package.json")) {
      NodeFS.writeFileSync(
        filePath,
        `${JSON.stringify({ name: relativePath, version: "1.2.3" })}\n`,
      );
    } else if (relativePath.endsWith("Cargo.toml")) {
      const packageName = relativePath.includes("desktop") ? "coddy-studio-desktop" : "coddy-studio-server";
      NodeFS.writeFileSync(filePath, `[package]\nname = "${packageName}"\nversion = "1.2.3"\n`);
    } else if (relativePath === "Cargo.lock") {
      NodeFS.writeFileSync(
        filePath,
        'version = 4\n\n[[package]]\nname = "coddy-studio-desktop"\nversion = "1.2.3"\n\n[[package]]\nname = "coddy-studio-server"\nversion = "1.2.3"\n',
      );
    } else {
      NodeFS.writeFileSync(filePath, `${relativePath}\n`);
    }
  }
  NodeFS.mkdirSync(NodePath.join(root, "scripts"), { recursive: true });
  NodeFS.writeFileSync(
    NodePath.join(root, "scripts/update-release-package-versions.ts"),
    "fixture",
  );
  NodeFS.writeFileSync(NodePath.join(root, "scripts/resolve-nightly-release.ts"), "fixture");
  if (includePatches) {
    NodeFS.mkdirSync(NodePath.join(root, "patches"));
    NodeFS.writeFileSync(NodePath.join(root, "patches/example.patch"), "patch");
  }
  return root;
}

function successfulRuntime(
  calls: string[],
  lockfile = "lockfileVersion: '9.0'\n",
): ReleaseSmokeRuntime {
  return {
    execFile(command, args, options) {
      calls.push(`exec:${command}:${args.join(" ")}:${options.cwd}`);
      const rootIndex = args.indexOf("--root");
      const targetRoot = rootIndex >= 0 ? args[rootIndex + 1] : undefined;
      if (args[0]?.endsWith("update-release-package-versions.ts")) {
        assert.ok(targetRoot);
        for (const relativePath of releasePackageJsonFiles) {
          const filePath = NodePath.join(targetRoot, relativePath);
          const packageJson = JSON.parse(NodeFS.readFileSync(filePath, "utf8")) as Record<
            string,
            unknown
          >;
          NodeFS.writeFileSync(
            filePath,
            `${JSON.stringify({ ...packageJson, version: "9.9.9-smoke.0" })}\n`,
          );
        }
        for (const relativePath of releaseRustFiles) {
          const filePath = NodePath.join(targetRoot, relativePath);
          NodeFS.writeFileSync(
            filePath,
            NodeFS.readFileSync(filePath, "utf8").replace(
              /^version\s*=\s*"[^"]+"\s*$/m,
              'version = "9.9.9-smoke.0"',
            ),
          );
        }
        const cargoLockPath = NodePath.join(targetRoot, "Cargo.lock");
        NodeFS.writeFileSync(
          cargoLockPath,
          NodeFS.readFileSync(cargoLockPath, "utf8").replace(
            /(\[\[package\]\]\r?\nname = "coddy-studio-(?:desktop|server)"\r?\nversion = ")[^"]+("\r?$)/gm,
            (_match, prefix: string, suffix: string) => `${prefix}9.9.9-smoke.0${suffix}`,
          ),
        );
        return "";
      }
      return [
        "version=9.9.10-nightly.20260413.321",
        "tag=v9.9.10-nightly.20260413.321",
        "name=Coddy Studio Nightly 9.9.10-nightly.20260413.321 (abcdef123456)",
      ].join("\n");
    },
    spawn(command, args, options) {
      calls.push(`spawn:${command}:${args.join(" ")}:${options.cwd}`);
      NodeFS.writeFileSync(NodePath.join(options.cwd, "pnpm-lock.yaml"), lockfile);
      return {
        stdout: "install complete\nWARN deprecated subdependencies found\n",
        stderr: "",
        status: 0,
      };
    },
  };
}

it("copies the complete workspace fixture with optional patches", () => {
  const source = makeFixtureRoot(true);
  const target = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "coddy-studio-release-copy-"));
  const sourceWithoutPatches = makeFixtureRoot(false);
  const targetWithoutPatches = NodeFS.mkdtempSync(
    NodePath.join(NodeOS.tmpdir(), "coddy-studio-release-copy-no-patches-"),
  );
  try {
    copyWorkspaceManifestFixture(source, target);
    copyWorkspaceManifestFixture(sourceWithoutPatches, targetWithoutPatches);
    for (const relativePath of releaseSmokeWorkspaceFiles) {
      assert.equal(NodeFS.existsSync(NodePath.join(target, relativePath)), true);
      assert.equal(NodeFS.existsSync(NodePath.join(targetWithoutPatches, relativePath)), true);
    }
    assert.equal(
      NodeFS.readFileSync(NodePath.join(target, "patches/example.patch"), "utf8"),
      "patch",
    );
    assert.equal(NodeFS.existsSync(NodePath.join(targetWithoutPatches, "patches")), false);
  } finally {
    NodeFS.rmSync(source, { recursive: true, force: true });
    NodeFS.rmSync(target, { recursive: true, force: true });
    NodeFS.rmSync(sourceWithoutPatches, { recursive: true, force: true });
    NodeFS.rmSync(targetWithoutPatches, { recursive: true, force: true });
  }
});

it("includes every Rust release version file in the smoke workspace", () => {
  for (const relativePath of [
    "apps/server/Cargo.toml",
    "apps/desktop/src-tauri/Cargo.toml",
    "Cargo.lock",
  ]) {
    assert.include(releaseSmokeWorkspaceFiles as ReadonlyArray<string>, relativePath);
  }
});

it.effect("lists the legacy diagnostic archive with the platform archive tool", () =>
  Effect.gen(function* () {
    const archivePath = NodePath.resolve(
      import.meta.dirname,
      "../apps/server/tests/fixtures/diagnostic-bundle-v4.zip",
    );
    const platform = yield* HostProcessPlatform;
    // Address Windows' bundled bsdtar by absolute path rather than by name.
    // Resolving `tar` through PATH finds Git for Windows' GNU tar first, which
    // cannot read a zip at all and additionally reads a leading `X:` in an
    // absolute path as a remote `host:path`. bsdtar is the archive tool the
    // platform actually ships, and it is what a user opening a diagnostic
    // bundle would get from a stock shell.
    const systemRoot = process.env["SystemRoot"] ?? "C:\\Windows";
    const [command, args] =
      platform === "win32"
        ? ([NodePath.win32.join(systemRoot, "System32", "tar.exe"), ["-tf", archivePath]] as const)
        : (["unzip", ["-t", archivePath]] as const);
    const result = NodeChildProcess.spawnSync(command, args, {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });

    assert.equal(result.error, undefined);
    assert.equal(result.status, 0, result.stderr);
    for (const entry of ["server.log", "server.trace.ndjson", "frontend.log"]) {
      assert.include(result.stdout, entry);
    }
  }),
);

it("executes the release command plan and always removes its temporary workspace", () => {
  const source = makeFixtureRoot();
  const tempRoot = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "coddy-studio-release-run-"));
  const calls: string[] = [];
  const stdout: string[] = [];
  const stderr: string[] = [];
  try {
    runReleaseSmoke({
      repoRoot: source,
      tempRoot,
      runtime: successfulRuntime(calls),
      stdout: (text) => stdout.push(text),
      stderr: (text) => stderr.push(text),
      log: (text) => stdout.push(`${text}\n`),
    });

    assert.equal(calls.length, 3);
    assert.include(calls[0] ?? "", "update-release-package-versions.ts 9.9.9-smoke.0 --root");
    assert.include(calls[1] ?? "", "vp:install --lockfile-only --ignore-scripts");
    assert.include(calls[2] ?? "", "resolve-nightly-release.ts --date 20260413");
    assert.include(stdout.join(""), "install complete");
    assert.notInclude(stdout.join(""), "deprecated subdependencies");
    assert.equal(stderr.join(""), "");
    assert.include(stdout.join(""), "Release smoke checks passed.");
    assert.equal(NodeFS.existsSync(tempRoot), false);
  } finally {
    NodeFS.rmSync(source, { recursive: true, force: true });
    NodeFS.rmSync(tempRoot, { recursive: true, force: true });
  }
});

it("adapts Node child-process results without launching commands", () => {
  const error = new Error("spawn adapter failure");
  let execReturnsBuffer = false;
  let spawnReturnsError = false;
  const runtime = makeReleaseSmokeRuntime({
    execFileSync: (() => (execReturnsBuffer ? Buffer.from("bytes") : "text")) as never,
    spawnSync: (() => ({
      stdout: "out",
      stderr: "err",
      status: spawnReturnsError ? null : 0,
      signal: null,
      pid: 1,
      output: [],
      ...(spawnReturnsError ? { error } : {}),
    })) as never,
  });

  assert.equal(runtime.execFile("node", [], { cwd: "X:/tmp" }), "text");
  execReturnsBuffer = true;
  assert.equal(runtime.execFile("node", [], { cwd: "X:/tmp" }), "");
  assert.deepStrictEqual(
    runtime.spawn("vp", [], {
      cwd: "X:/tmp",
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    }),
    { stdout: "out", stderr: "err", status: 0 },
  );
  spawnReturnsError = true;
  assert.equal(
    runtime.spawn("vp", [], {
      cwd: "X:/tmp",
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    }).error,
    error,
  );
});

it("uses default repository, temp, and output adapters with an injected process runtime", () => {
  const calls: string[] = [];
  let generatedRoot: string | undefined;
  const runtime = successfulRuntime(calls);
  const wrappedRuntime: ReleaseSmokeRuntime = {
    ...runtime,
    spawn(command, args, options) {
      generatedRoot = options.cwd;
      const result = runtime.spawn(command, args, options);
      return { ...result, stderr: "install warning\n" };
    },
  };
  const stdout = vi.spyOn(process.stdout, "write").mockImplementation(() => true);
  const stderr = vi.spyOn(process.stderr, "write").mockImplementation(() => true);
  try {
    runReleaseSmoke({ runtime: wrappedRuntime });
    assert.ok(generatedRoot);
    assert.equal(NodeFS.existsSync(generatedRoot), false);
    assert.ok(stdout.mock.calls.length >= 2);
    assert.equal(stderr.mock.calls.length, 1);
  } finally {
    stdout.mockRestore();
    stderr.mockRestore();
    if (generatedRoot) NodeFS.rmSync(generatedRoot, { recursive: true, force: true });
  }
});

it("propagates process and validation failures without leaking temporary workspaces", () => {
  const cases = [
    {
      name: "spawn error",
      mutate(runtime: ReleaseSmokeRuntime): ReleaseSmokeRuntime {
        return {
          ...runtime,
          spawn: () => ({ stdout: "", stderr: "", status: null, error: new Error("spawn failed") }),
        };
      },
      message: "spawn failed",
    },
    {
      name: "nonzero install",
      mutate(runtime: ReleaseSmokeRuntime): ReleaseSmokeRuntime {
        return { ...runtime, spawn: () => ({ stdout: "", stderr: "install failed", status: 12 }) };
      },
      message: "Command failed: vp install",
    },
    {
      name: "invalid lockfile",
      mutate(_runtime: ReleaseSmokeRuntime): ReleaseSmokeRuntime {
        return successfulRuntime([], "not a lockfile\n");
      },
      message: "Expected pnpm-lock.yaml to be regenerated",
    },
    {
      name: "wrong package version",
      mutate(runtime: ReleaseSmokeRuntime): ReleaseSmokeRuntime {
        return {
          ...runtime,
          execFile(command, args, options) {
            if (args[0]?.endsWith("update-release-package-versions.ts")) return "";
            return runtime.execFile(command, args, options);
          },
        };
      },
      message: "to have version 9.9.9-smoke.0",
    },
    {
      name: "missing nightly metadata",
      mutate(runtime: ReleaseSmokeRuntime): ReleaseSmokeRuntime {
        let call = 0;
        return {
          ...runtime,
          execFile(command, args, options) {
            call += 1;
            if (call === 2) return "version=wrong";
            return runtime.execFile(command, args, options);
          },
        };
      },
      message: "Expected nightly metadata",
    },
  ] as const;

  for (const testCase of cases) {
    const source = makeFixtureRoot();
    const tempRoot = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "coddy-studio-release-fail-"));
    const runtime = testCase.mutate(successfulRuntime([]));
    try {
      assert.throws(
        () =>
          runReleaseSmoke({
            repoRoot: source,
            tempRoot,
            runtime,
            stdout: () => undefined,
            stderr: () => undefined,
            log: () => undefined,
          }),
        testCase.message,
        testCase.name,
      );
      assert.equal(NodeFS.existsSync(tempRoot), false, testCase.name);
    } finally {
      NodeFS.rmSync(source, { recursive: true, force: true });
      NodeFS.rmSync(tempRoot, { recursive: true, force: true });
    }
  }
});

it("runs the smoke workflow only for the direct CLI entrypoint", () => {
  const source = makeFixtureRoot();
  const tempRoot = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "coddy-studio-release-main-"));
  try {
    assert.equal(runReleaseSmokeMain(false, { runtime: successfulRuntime([]) }), false);
    assert.equal(
      runReleaseSmokeMain(true, {
        repoRoot: source,
        tempRoot,
        runtime: successfulRuntime([]),
        stdout: () => undefined,
        stderr: () => undefined,
        log: () => undefined,
      }),
      true,
    );
  } finally {
    NodeFS.rmSync(source, { recursive: true, force: true });
    NodeFS.rmSync(tempRoot, { recursive: true, force: true });
  }
});
