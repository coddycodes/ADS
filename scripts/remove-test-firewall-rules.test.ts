import { describe, expect, it, vi } from "vite-plus/test";

import {
  FIREWALL_RULE_QUERY_SCRIPT,
  FIREWALL_RULE_REMOVE_SCRIPT,
  FirewallCleanupTargetError,
  PROTECTED_FIREWALL_RULE_NAMES,
  assertCleanupTarget,
  normalizeWindowsProgramPath,
  parseFirewallCleanupArgs,
  parseFirewallRuleRecords,
  runFirewallCleanup,
  selectTestGeneratedFirewallRules,
  type WindowsFirewallRuleRecord,
} from "./remove-test-firewall-rules.ts";

const testExecutable = String.raw`C:\coddy-studio-validation\CoddyStudio\target\aarch64-pc-windows-msvc\release\coddy-studio-desktop.exe`;
const installedExecutable = String.raw`C:\Program Files\Coddy Studio\Coddy Studio.exe`;

function rule(overrides: Partial<WindowsFirewallRuleRecord>): WindowsFirewallRuleRecord {
  return {
    name: "{00000000-0000-0000-0000-000000000000}",
    displayName: "Coddy Studio",
    direction: "Inbound",
    action: "Block",
    enabled: "True",
    profile: "Public",
    program: testExecutable,
    ...overrides,
  };
}

describe("remove-test-firewall-rules", () => {
  it("normalizes program paths for exact comparison", () => {
    expect(normalizeWindowsProgramPath(String.raw`\\?\C:\Repo\Target\App.EXE`)).toBe(
      String.raw`c:\repo\target\app.exe`,
    );
    expect(normalizeWindowsProgramPath("C:/Repo/Target/app.exe")).toBe(
      String.raw`c:\repo\target\app.exe`,
    );
    expect(normalizeWindowsProgramPath(testExecutable)).toBe(testExecutable.toLowerCase());
  });

  it("selects only rules program-scoped to the exact test executable", () => {
    const tcp = rule({ name: "{tcp}", displayName: "Coddy Studio" });
    const udp = rule({ name: "{udp}", displayName: "Coddy Studio" });
    const allowed = rule({ name: "{allow}", action: "Allow", profile: "Private" });
    const caseVariant = rule({ name: "{case}", program: testExecutable.toUpperCase() });
    const otherBuild = rule({
      name: "{other}",
      program: testExecutable.replace("release", "debug"),
    });
    const installed = rule({ name: "{installed}", program: installedExecutable });
    const unrelated = rule({ name: "{edge}", displayName: "Microsoft Edge", program: null });
    const managed = rule({
      name: "{managed}",
      displayName: PROTECTED_FIREWALL_RULE_NAMES[0]!,
      action: "Allow",
    });

    const selection = selectTestGeneratedFirewallRules(
      [tcp, udp, allowed, caseVariant, otherBuild, installed, unrelated, managed],
      testExecutable,
    );

    expect(selection.remove.map((entry) => entry.name)).toEqual([
      "{tcp}",
      "{udp}",
      "{allow}",
      "{case}",
    ]);
    expect(selection.protectedSkipped).toEqual([managed]);
  });

  it("refuses installed or malformed cleanup targets", () => {
    expect(assertCleanupTarget(testExecutable)).toBe(testExecutable);
    expect(() => assertCleanupTarget("")).toThrow(FirewallCleanupTargetError);
    expect(() => assertCleanupTarget("coddy-studio-desktop.exe")).toThrow(/absolute Windows path/);
    expect(() => assertCleanupTarget(String.raw`C:\repo\target\release\coddy-studio-desktop`)).toThrow(
      /\.exe/,
    );
    expect(() => assertCleanupTarget(installedExecutable)).toThrow(/installed location/);
    expect(() =>
      assertCleanupTarget(String.raw`C:\Users\admin\AppData\Local\Programs\Coddy Studio\Coddy Studio.exe`),
    ).toThrow(/installed location/);
    expect(() => assertCleanupTarget(String.raw`C:\Windows\System32\svchost.exe`)).toThrow(
      /installed location/,
    );
  });

  it("parses the PowerShell JSON for one rule, many rules, and no rules", () => {
    expect(parseFirewallRuleRecords("")).toEqual([]);
    expect(parseFirewallRuleRecords("[]")).toEqual([]);
    const single = parseFirewallRuleRecords(
      JSON.stringify({
        name: "{a}",
        displayName: "Coddy Studio",
        direction: "Inbound",
        action: "Block",
        enabled: "True",
        profile: "Public",
        program: testExecutable,
      }),
    );
    expect(single).toEqual([rule({ name: "{a}" })]);
    expect(parseFirewallRuleRecords(JSON.stringify([{ name: "{b}", program: null }]))[0]).toEqual({
      name: "{b}",
      displayName: "",
      direction: "",
      action: "",
      enabled: "",
      profile: "",
      program: null,
    });
  });

  it("queries by exact program, removes by unique rule name, and verifies zero remain", () => {
    const calls: Array<{ script: string; env: Readonly<Record<string, string>> }> = [];
    let removed = false;
    const powerShell = vi.fn((script: string, env: Readonly<Record<string, string>>) => {
      calls.push({ script, env });
      if (script === FIREWALL_RULE_REMOVE_SCRIPT) {
        removed = true;
        return { stdout: "", status: 0 };
      }
      const rules = removed
        ? [rule({ name: "{managed}", displayName: "Coddy Studio Remote Access", action: "Allow" })]
        : [
            rule({ name: "{tcp}" }),
            rule({ name: "{udp}" }),
            rule({ name: "{managed}", displayName: "Coddy Studio Remote Access", action: "Allow" }),
          ];
      return { stdout: JSON.stringify(rules), status: 0 };
    });
    const writes: string[] = [];

    const exitCode = runFirewallCleanup(
      { executable: testExecutable, dryRun: false },
      { powerShell, platform: "win32", write: (text) => writes.push(text) },
    );

    expect(exitCode).toBe(0);
    expect(calls.map((call) => call.script)).toEqual([
      FIREWALL_RULE_QUERY_SCRIPT,
      FIREWALL_RULE_REMOVE_SCRIPT,
      FIREWALL_RULE_QUERY_SCRIPT,
    ]);
    expect(calls[0]!.env).toEqual({ CODDY_STUDIO_FIREWALL_TARGET: testExecutable });
    expect(calls[1]!.env).toEqual({ CODDY_STUDIO_FIREWALL_RULE_NAMES: "{tcp}|{udp}" });
    expect(writes.join("")).toContain("Keeping protected rule Coddy Studio Remote Access");
    expect(writes.join("")).toContain("Verified zero test-generated rules remain");
    expect(FIREWALL_RULE_QUERY_SCRIPT).toContain("$_.Program -ieq $target");
    expect(FIREWALL_RULE_REMOVE_SCRIPT).toContain("Remove-NetFirewallRule -Name $name");
  });

  it("fails when rules survive removal and never removes during a dry run", () => {
    const survivingPowerShell = vi.fn((script: string) =>
      script === FIREWALL_RULE_REMOVE_SCRIPT
        ? { stdout: "", status: 0 }
        : { stdout: JSON.stringify([rule({ name: "{tcp}" })]), status: 0 },
    );
    const writes: string[] = [];
    expect(
      runFirewallCleanup(
        { executable: testExecutable, dryRun: false },
        { powerShell: survivingPowerShell, platform: "win32", write: (text) => writes.push(text) },
      ),
    ).toBe(1);
    expect(writes.join("")).toContain("Still present: Coddy Studio ({tcp})");

    const dryRunPowerShell = vi.fn(() => ({
      stdout: JSON.stringify([rule({ name: "{tcp}" })]),
      status: 0,
    }));
    expect(
      runFirewallCleanup(
        { executable: testExecutable, dryRun: true },
        { powerShell: dryRunPowerShell, platform: "win32", write: () => undefined },
      ),
    ).toBe(0);
    expect(dryRunPowerShell).toHaveBeenCalledOnce();
  });

  it("reports elevation failures and stays inert off Windows", () => {
    const deniedPowerShell = vi.fn((script: string) =>
      script === FIREWALL_RULE_REMOVE_SCRIPT
        ? { stdout: "", status: 5 }
        : { stdout: JSON.stringify([rule({ name: "{tcp}" })]), status: 0 },
    );
    const writes: string[] = [];
    expect(
      runFirewallCleanup(
        { executable: testExecutable, dryRun: false },
        { powerShell: deniedPowerShell, platform: "win32", write: (text) => writes.push(text) },
      ),
    ).toBe(5);
    expect(writes.join("")).toContain("elevated PowerShell");

    const powerShell = vi.fn();
    expect(
      runFirewallCleanup(
        { executable: testExecutable, dryRun: false },
        { powerShell, platform: "darwin", write: () => undefined },
      ),
    ).toBe(0);
    expect(powerShell).not.toHaveBeenCalled();
  });

  it("parses the CLI arguments", () => {
    expect(parseFirewallCleanupArgs(["--executable", testExecutable, "--dry-run"])).toEqual({
      executable: testExecutable,
      dryRun: true,
    });
    expect(parseFirewallCleanupArgs([`--executable=${testExecutable}`])).toEqual({
      executable: testExecutable,
      dryRun: false,
    });
    expect(() => parseFirewallCleanupArgs(["--force"])).toThrow(/Unknown argument/);
  });
});
