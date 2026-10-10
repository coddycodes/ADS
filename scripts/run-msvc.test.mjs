import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import { describe, expect, it, vi } from "vite-plus/test";

import {
  WINDOWS_PACKAGING_PREFLIGHT_EXIT_CODE,
  canonicalizeCargoTestTarget,
  defaultWindowsCargoRunner,
  discoverVcVarsAll,
  isTauriBuildCommand,
  msvcToolchain,
  quoteCmdArg,
  resolveMsvcArchitecture,
  run,
  runMsvc,
  windowsPackagingPreflight,
} from "./run-msvc.mjs";

const directoryAliasCapability = (() => {
  const fixtureRoot = NodeFS.mkdtempSync(
    NodePath.join(NodeOS.tmpdir(), "bibcode-directory-alias-capability-"),
  );
  const physicalRoot = NodePath.join(fixtureRoot, "physical");
  const aliasRoot = NodePath.join(fixtureRoot, "alias");
  try {
    NodeFS.mkdirSync(physicalRoot);
    NodeFS.symlinkSync(physicalRoot, aliasRoot, "junction");
    return true;
  } catch {
    return false;
  } finally {
    NodeFS.rmSync(fixtureRoot, { recursive: true, force: true });
  }
})();

describe("run-msvc", () => {
  it("selects the ARM64 toolchain from an explicit Rust target", () => {
    expect(
      resolveMsvcArchitecture(["cargo", "build", "--target", "aarch64-pc-windows-msvc"], {
        PROCESSOR_ARCHITECTURE: "AMD64",
      }),
    ).toBe("arm64");
    expect(msvcToolchain("arm64")).toEqual({
      cargoRunnerKey: "CARGO_TARGET_AARCH64_PC_WINDOWS_MSVC_RUNNER",
      vcvarsArgument: "arm64",
      vsComponent: "Microsoft.VisualStudio.Component.VC.Tools.ARM64",
    });
  });

  it("uses explicit configuration before native host architecture", () => {
    expect(
      resolveMsvcArchitecture([], {
        CARGO_BUILD_TARGET: "x86_64-pc-windows-msvc",
        PROCESSOR_ARCHITECTURE: "ARM64",
      }),
    ).toBe("x64");
    expect(
      resolveMsvcArchitecture([], {
        PROCESSOR_ARCHITECTURE: "AMD64",
        PROCESSOR_ARCHITEW6432: "ARM64",
        TAURI_DESKTOP_ARCH: "x64",
      }),
    ).toBe("x64");
    expect(
      resolveMsvcArchitecture([], {
        PROCESSOR_ARCHITECTURE: "AMD64",
        PROCESSOR_ARCHITEW6432: "ARM64",
      }),
    ).toBe("arm64");
  });

  it("keeps the existing x64 toolchain contract", () => {
    expect(resolveMsvcArchitecture([], { PROCESSOR_ARCHITECTURE: "AMD64" })).toBe("x64");
    expect(msvcToolchain("x64")).toEqual({
      cargoRunnerKey: "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUNNER",
      vcvarsArgument: "x64",
      vsComponent: "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
    });
  });

  it("quotes only command arguments that require cmd escaping", () => {
    expect(quoteCmdArg("safe/path:value-1")).toBe("safe/path:value-1");
    expect(quoteCmdArg('two words "quoted"')).toBe('"two words \\"quoted\\""');
  });

  it("builds a quoted Cargo target runner command", () => {
    expect(
      defaultWindowsCargoRunner({
        command: "custom-node",
        repoRoot: "C:/repo root",
      }),
    ).toBe('custom-node "C:\\repo root\\scripts\\run-windows-cargo-target.mjs"');
  });

  it("normalizes missing child statuses", () => {
    expect(run("tool", [], {}, () => ({ status: 7 }))).toBe(7);
    expect(run("tool", [], {}, () => ({ status: null }))).toBe(1);
  });

  it("discovers vcvarsall through vswhere or the Build Tools fallback", () => {
    expect(discoverVcVarsAll({ programFilesX86: "" })).toBeNull();

    const root = "C:/Program Files (x86)";
    const vswhere = NodePath.join(root, "Microsoft Visual Studio", "Installer", "vswhere.exe");
    const install = "C:/Visual Studio";
    const candidate = NodePath.join(install, "VC", "Auxiliary", "Build", "vcvarsall.bat");
    expect(
      discoverVcVarsAll({
        programFilesX86: root,
        existsSync: (path) => path === vswhere || path === candidate,
        spawnSync: () => ({ stdout: `\r\n${install}\r\n` }),
      }),
    ).toBe(candidate);

    const fallback = NodePath.join(
      root,
      "Microsoft Visual Studio",
      "2022",
      "BuildTools",
      "VC",
      "Auxiliary",
      "Build",
      "vcvarsall.bat",
    );
    expect(
      discoverVcVarsAll({
        programFilesX86: root,
        existsSync: (path) => path === fallback,
      }),
    ).toBe(fallback);
    expect(discoverVcVarsAll({ programFilesX86: root, existsSync: () => false })).toBeNull();
    expect(
      discoverVcVarsAll({
        programFilesX86: root,
        existsSync: (path) => path === vswhere,
        spawnSync: () => ({ stdout: "\n" }),
      }),
    ).toBeNull();
    expect(
      discoverVcVarsAll({
        programFilesX86: root,
        existsSync: (path) => path === vswhere,
        spawnSync: () => ({ stdout: `${install}\n` }),
      }),
    ).toBeNull();
  });

  it("runs directly without Visual Studio and reports missing commands", () => {
    const consoleError = vi.fn();
    expect(runMsvc([], { consoleError })).toBe(2);
    expect(consoleError).toHaveBeenCalledOnce();

    const spawnSync = vi.fn(() => ({ status: 4 }));
    expect(
      runMsvc(["cargo", "test"], {
        programFilesX86: "C:/missing",
        existsSync: () => false,
        spawnSync,
      }),
    ).toBe(4);
    expect(spawnSync).toHaveBeenCalledWith(
      "cargo",
      ["test"],
      expect.objectContaining({ shell: false }),
    );
  });

  it("canonicalizes an explicit Cargo test target before launch", () => {
    const mkdirSync = vi.fn();
    const realpathSync = vi.fn(() => "/private/tmp/bibcode-task9f-target");
    const spawnSync = vi.fn(() => ({ status: 0 }));
    const resolvedTarget = NodePath.resolve("/tmp/bibcode-task9f-target");

    expect(
      runMsvc(["cargo", "test", "-p", "bibcode-desktop"], {
        env: { CARGO_TARGET_DIR: "/tmp/bibcode-task9f-target" },
        programFilesX86: "",
        mkdirSync,
        realpathSync,
        spawnSync,
      }),
    ).toBe(0);

    expect(mkdirSync).toHaveBeenCalledWith(resolvedTarget, {
      recursive: true,
    });
    expect(realpathSync).toHaveBeenCalledWith(resolvedTarget);
    expect(spawnSync).toHaveBeenCalledWith(
      "cargo",
      ["test", "-p", "bibcode-desktop"],
      expect.objectContaining({
        env: expect.objectContaining({
          CARGO_TARGET_DIR: "/private/tmp/bibcode-task9f-target",
        }),
      }),
    );
  });

  it("uses the native canonical target identity on every platform", () => {
    const resolvedTarget = NodePath.resolve("/tmp/bibcode-task9f-target");
    for (const canonicalTarget of [
      "/private/tmp/bibcode-task9f-target",
      "/tmp/bibcode-task9f-target",
      "C:\\isolated\\bibcode-task9f-target",
    ]) {
      const mkdirSync = vi.fn();
      const realpathSync = vi.fn(() => canonicalTarget);
      const configured = {
        CARGO_TARGET_DIR: "/tmp/bibcode-task9f-target",
        SENTINEL: "kept",
      };

      expect(
        canonicalizeCargoTestTarget(["cargo", "test"], configured, {
          mkdirSync,
          realpathSync,
        }),
      ).toEqual({ CARGO_TARGET_DIR: canonicalTarget, SENTINEL: "kept" });
      expect(mkdirSync).toHaveBeenCalledWith(resolvedTarget, {
        recursive: true,
      });
      expect(realpathSync).toHaveBeenCalledWith(resolvedTarget);
      expect(configured.CARGO_TARGET_DIR).toBe("/tmp/bibcode-task9f-target");
    }
  });

  it("leaves unrelated commands and implicit targets outside canonicalization", () => {
    const mkdirSync = vi.fn();
    const realpathSync = vi.fn();
    const configured = { CARGO_TARGET_DIR: "relative-target", SENTINEL: "kept" };
    const options = { cwd: "/repo", mkdirSync, realpathSync };

    expect(canonicalizeCargoTestTarget(["cargo", "check"], configured, options)).toBe(configured);
    expect(canonicalizeCargoTestTarget(["vp", "test"], configured, options)).toBe(configured);
    expect(canonicalizeCargoTestTarget(["cargo", "test"], { SENTINEL: "kept" }, options)).toEqual({
      SENTINEL: "kept",
    });
    const emptyTarget = { CARGO_TARGET_DIR: "", SENTINEL: "kept" };
    expect(canonicalizeCargoTestTarget(["cargo", "test"], emptyTarget, options)).toBe(emptyTarget);
    expect(mkdirSync).not.toHaveBeenCalled();
    expect(realpathSync).not.toHaveBeenCalled();
  });

  it("resolves relative Cargo test targets against the launch directory", () => {
    const mkdirSync = vi.fn();
    const realpathSync = vi.fn(() => "/canonical/repo/isolated-target");
    const configured = { CARGO_TARGET_DIR: "isolated-target", SENTINEL: "kept" };
    const resolvedTarget = NodePath.resolve("/repo", "isolated-target");

    const canonical = canonicalizeCargoTestTarget(["cargo", "test"], configured, {
      cwd: "/repo",
      mkdirSync,
      realpathSync,
    });

    expect(mkdirSync).toHaveBeenCalledWith(resolvedTarget, { recursive: true });
    expect(realpathSync).toHaveBeenCalledWith(resolvedTarget);
    expect(canonical).toEqual({
      CARGO_TARGET_DIR: "/canonical/repo/isolated-target",
      SENTINEL: "kept",
    });
    expect(configured.CARGO_TARGET_DIR).toBe("isolated-target");
  });

  it("creates and canonicalizes real absent and relative Cargo test targets", () => {
    const fixtureRoot = NodeFS.mkdtempSync(
      NodePath.join(NodeOS.tmpdir(), "bibcode-cargo-target-contract-"),
    );
    const physicalRoot = NodePath.join(fixtureRoot, "physical");
    const spawnSync = vi.fn(() => ({ status: 0 }));

    try {
      NodeFS.mkdirSync(physicalRoot);

      const absentTarget = NodePath.join(physicalRoot, "absent-target");
      expect(
        runMsvc(["cargo", "test"], {
          env: { CARGO_TARGET_DIR: absentTarget },
          programFilesX86: "",
          spawnSync,
        }),
      ).toBe(0);
      expect(NodeFS.statSync(NodePath.join(physicalRoot, "absent-target")).isDirectory()).toBe(
        true,
      );
      expect(spawnSync).toHaveBeenLastCalledWith(
        "cargo",
        ["test"],
        expect.objectContaining({
          env: expect.objectContaining({
            CARGO_TARGET_DIR: NodeFS.realpathSync.native(
              NodePath.join(physicalRoot, "absent-target"),
            ),
          }),
        }),
      );

      NodeFS.mkdirSync(NodePath.join(physicalRoot, "existing-target"));
      expect(
        runMsvc(["cargo", "test"], {
          cwd: physicalRoot,
          env: { CARGO_TARGET_DIR: "existing-target" },
          programFilesX86: "",
          spawnSync,
        }),
      ).toBe(0);
      expect(spawnSync).toHaveBeenLastCalledWith(
        "cargo",
        ["test"],
        expect.objectContaining({
          env: expect.objectContaining({
            CARGO_TARGET_DIR: NodeFS.realpathSync.native(
              NodePath.join(physicalRoot, "existing-target"),
            ),
          }),
        }),
      );
    } finally {
      NodeFS.rmSync(fixtureRoot, { recursive: true, force: true });
    }
  });

  it.runIf(directoryAliasCapability)(
    "canonicalizes a real directory-alias Cargo test target",
    () => {
      const fixtureRoot = NodeFS.mkdtempSync(
        NodePath.join(NodeOS.tmpdir(), "bibcode-cargo-target-alias-contract-"),
      );
      const physicalRoot = NodePath.join(fixtureRoot, "physical");
      const aliasRoot = NodePath.join(fixtureRoot, "alias");
      const spawnSync = vi.fn(() => ({ status: 0 }));

      try {
        NodeFS.mkdirSync(physicalRoot);
        NodeFS.symlinkSync(physicalRoot, aliasRoot, "junction");
        expect(
          runMsvc(["cargo", "test"], {
            env: { CARGO_TARGET_DIR: NodePath.join(aliasRoot, "target") },
            programFilesX86: "",
            spawnSync,
          }),
        ).toBe(0);
        expect(spawnSync).toHaveBeenLastCalledWith(
          "cargo",
          ["test"],
          expect.objectContaining({
            env: expect.objectContaining({
              CARGO_TARGET_DIR: NodeFS.realpathSync.native(NodePath.join(physicalRoot, "target")),
            }),
          }),
        );
      } finally {
        NodeFS.rmSync(fixtureRoot, { recursive: true, force: true });
      }
    },
  );

  it("writes, runs, and removes an MSVC wrapper even when cleanup fails", () => {
    const writeFileSync = vi.fn();
    const rmSync = vi.fn(() => {
      throw new Error("already removed");
    });
    const spawnSync = vi
      .fn()
      .mockReturnValueOnce({ stdout: "C:/Visual Studio\n" })
      .mockReturnValueOnce({ status: 0 });

    expect(
      runMsvc(["cargo", "test name"], {
        // Pin the toolchain so the expected wrapper name does not depend on the
        // host architecture (ARM64 hosts otherwise resolve the arm64 toolchain).
        env: { CARGO_BUILD_TARGET: "x86_64-pc-windows-msvc" },
        programFilesX86: "C:/Program Files (x86)",
        existsSync: () => true,
        spawnSync,
        comspec: "custom-cmd.exe",
        tmpdir: "C:/tmp",
        pid: 12,
        now: () => 34,
        writeFileSync,
        rmSync,
      }),
    ).toBe(0);
    expect(writeFileSync).toHaveBeenCalledWith(
      NodePath.join("C:/tmp", "bibcode-msvc-x64-12-34.cmd"),
      expect.stringContaining('cargo "test name"'),
    );
    expect(spawnSync).toHaveBeenLastCalledWith(
      "custom-cmd.exe",
      ["/d", "/c", NodePath.join("C:/tmp", "bibcode-msvc-x64-12-34.cmd")],
      expect.objectContaining({
        env: expect.objectContaining({
          CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUNNER: expect.stringContaining(
            "run-windows-cargo-target.mjs",
          ),
        }),
      }),
    );
    expect(rmSync).toHaveBeenCalledOnce();
  });

  it("uses platform defaults for wrapper paths and filesystem cleanup", () => {
    const originalComspec = process.env.ComSpec;
    const spawnSync = vi
      .fn()
      .mockReturnValueOnce({ stdout: "C:/Visual Studio\n" })
      .mockReturnValueOnce({ status: 0 })
      .mockReturnValueOnce({ stdout: "C:/Visual Studio\n" })
      .mockReturnValueOnce({ status: 0 });
    try {
      process.env.ComSpec = "true";
      expect(
        runMsvc(["cargo", "test"], {
          programFilesX86: "C:/Program Files (x86)",
          existsSync: () => true,
          spawnSync,
        }),
      ).toBe(0);

      delete process.env.ComSpec;
      expect(
        runMsvc(["cargo", "test"], {
          programFilesX86: "C:/Program Files (x86)",
          existsSync: () => true,
          spawnSync,
        }),
      ).toBe(0);
    } finally {
      if (originalComspec === undefined) {
        delete process.env.ComSpec;
      } else {
        process.env.ComSpec = originalComspec;
      }
    }
  });

  it("refuses Windows Tauri packaging under the SYSTEM profile before spawning anything", () => {
    const systemProfile = String.raw`C:\Windows\System32\config\systemprofile\AppData\Local`;
    const diagnostic = windowsPackagingPreflight(
      ["pnpm", "exec", "tauri", "build", "--bundles", "nsis"],
      { USERNAME: "WIN11$", LOCALAPPDATA: systemProfile },
      "win32",
    );
    expect(diagnostic).toContain("USERNAME is `WIN11$`");
    expect(diagnostic).toContain(
      `LOCALAPPDATA resolves under the system profile (${systemProfile})`,
    );
    expect(diagnostic).toContain("error 0x2");
    expect(diagnostic).toContain("prlctl exec 'Windows 11' --current-user");
    expect(diagnostic).toContain("No Rust build was started.");
    expect(
      windowsPackagingPreflight(
        ["tauri", "build"],
        { USERNAME: "system", USERPROFILE: "C:/Users/system" },
        "win32",
      ),
    ).toContain("USERNAME is `system`");
    expect(
      windowsPackagingPreflight(
        ["tauri", "build"],
        { USERNAME: "admin", USERPROFILE: "C:/Windows/System32/config/systemprofile" },
        "win32",
      ),
    ).toContain("USERPROFILE resolves under the system profile");

    const consoleError = vi.fn();
    const spawnSync = vi.fn(() => ({ status: 0 }));
    expect(
      runMsvc(["pnpm", "exec", "tauri", "build"], {
        env: { USERNAME: "WIN11$" },
        platform: "win32",
        programFilesX86: "",
        consoleError,
        spawnSync,
      }),
    ).toBe(WINDOWS_PACKAGING_PREFLIGHT_EXIT_CODE);
    expect(spawnSync).not.toHaveBeenCalled();
    expect(consoleError).toHaveBeenCalledOnce();
    expect(consoleError.mock.calls[0][0]).toContain("[run-msvc]");
  });

  it("lets interactive-account packaging and non-packaging commands proceed", () => {
    const interactive = {
      USERNAME: "admin",
      LOCALAPPDATA: String.raw`C:\Users\admin\AppData\Local`,
      USERPROFILE: String.raw`C:\Users\admin`,
    };
    expect(
      windowsPackagingPreflight(["pnpm", "exec", "tauri", "build"], interactive, "win32"),
    ).toBe(null);
    expect(
      windowsPackagingPreflight(["pnpm", "exec", "tauri", "dev"], { USERNAME: "SYSTEM" }, "win32"),
    ).toBe(null);
    expect(
      windowsPackagingPreflight(
        ["cargo", "check", "--all-targets"],
        { USERNAME: "WIN11$" },
        "win32",
      ),
    ).toBe(null);
    expect(
      windowsPackagingPreflight(
        ["pnpm", "exec", "tauri", "build"],
        { USERNAME: "SYSTEM" },
        "darwin",
      ),
    ).toBe(null);
    expect(isTauriBuildCommand(["pnpm", "exec", "tauri", "build"])).toBe(true);
    expect(isTauriBuildCommand(["tauri", "info", "build"])).toBe(false);
    // NSIS packaging runs in `tauri bundle` after a `--no-bundle` build.
    expect(isTauriBuildCommand(["pnpm", "exec", "tauri", "bundle", "--bundles", "nsis"])).toBe(
      true,
    );
    expect(
      windowsPackagingPreflight(
        ["pnpm", "exec", "tauri", "bundle"],
        { USERNAME: "SYSTEM" },
        "win32",
      ),
    ).toContain("SYSTEM profile");

    const spawnSync = vi.fn(() => ({ status: 0 }));
    expect(
      runMsvc(["pnpm", "exec", "tauri", "build"], {
        env: interactive,
        platform: "win32",
        programFilesX86: "",
        spawnSync,
      }),
    ).toBe(0);
    expect(spawnSync).toHaveBeenCalledOnce();
  });
});
