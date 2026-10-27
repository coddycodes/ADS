import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";

import { describe, expect, it, vi } from "vite-plus/test";

import { COMMON_CONTROLS_V6_MANIFEST, runWindowsCargoTarget } from "./run-windows-cargo-target.mjs";

describe("run-windows-cargo-target", () => {
  it("rejects a missing executable", () => {
    const consoleError = vi.fn();

    expect(runWindowsCargoTarget([], { consoleError })).toBe(2);
    expect(consoleError).toHaveBeenCalledOnce();
  });

  it("declares asInvoker so installer-detection names such as *update* launch unelevated", () => {
    expect(COMMON_CONTROLS_V6_MANIFEST).toContain(
      '<requestedExecutionLevel level="asInvoker" uiAccess="false" />',
    );
    expect(COMMON_CONTROLS_V6_MANIFEST).toContain('name="Microsoft.Windows.Common-Controls"');
  });

  it("runs a target with a temporary Common Controls v6 sidecar manifest", () => {
    const writeFileSync = vi.fn();
    const rmSync = vi.fn();
    const spawnSync = vi.fn(() => ({ status: 7 }));
    const utimesSync = vi.fn();
    const stamp = new Date("2026-09-02T12:00:00Z");

    expect(
      runWindowsCargoTarget(["C:/target/test.exe", "--exact", "unit"], {
        writeFileSync,
        rmSync,
        spawnSync,
        utimesSync,
        now: () => stamp,
      }),
    ).toBe(7);
    expect(writeFileSync).toHaveBeenCalledWith(
      "C:/target/test.exe.manifest",
      COMMON_CONTROLS_V6_MANIFEST,
      "utf8",
    );
    expect(utimesSync).toHaveBeenCalledWith("C:/target/test.exe", stamp, stamp);
    expect(utimesSync.mock.invocationCallOrder[0]).toBeGreaterThan(
      writeFileSync.mock.invocationCallOrder[0],
    );
    expect(utimesSync.mock.invocationCallOrder[0]).toBeLessThan(
      spawnSync.mock.invocationCallOrder[0],
    );
    expect(spawnSync).toHaveBeenCalledWith("C:/target/test.exe", ["--exact", "unit"], {
      stdio: "inherit",
      shell: false,
    });
    expect(rmSync).toHaveBeenCalledWith("C:/target/test.exe.manifest", { force: true });
  });

  it("normalizes launch failures and cleanup failures", () => {
    const consoleError = vi.fn();

    expect(
      runWindowsCargoTarget(["missing.exe"], {
        consoleError,
        writeFileSync: vi.fn(),
        rmSync: () => {
          throw new Error("already removed");
        },
        spawnSync: () => ({ error: new Error("not found"), status: null }),
      }),
    ).toBe(1);
    expect(consoleError).toHaveBeenCalledWith(
      'Failed to launch Windows Cargo target "missing.exe": not found',
    );
  });

  it("uses native manifest IO and maps a missing exit status to failure", () => {
    const temporary = NodeFS.mkdtempSync(
      NodePath.join(NodeOS.tmpdir(), "coddy-studio-windows-cargo-target-"),
    );
    const executable = NodePath.join(temporary, "fixture.exe");
    const manifestPath = `${executable}.manifest`;

    try {
      expect(
        runWindowsCargoTarget([executable], {
          spawnSync: () => ({ status: null }),
        }),
      ).toBe(1);
      expect(NodeFS.existsSync(manifestPath)).toBe(false);
    } finally {
      NodeFS.rmSync(temporary, { force: true, recursive: true });
    }
  });
});
