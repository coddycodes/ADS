#!/usr/bin/env node
// @effect-diagnostics nodeBuiltinImport:off - Windows-only validation cleanup drives PowerShell directly.
// @effect-diagnostics globalConsole:off - Standalone cleanup CLI reports to the operator terminal.
//
// Removes Windows Defender Firewall rules that Windows itself generated for one
// exact test executable (the WebDriver-enabled desktop build launched during
// packaged E2E validation). Cancelling or accepting the "Windows Security
// Alert" prompt leaves program-scoped inbound rules behind; this helper selects
// rules by that exact program path only, never touches the app-managed
// `Coddy Studio Remote Access` rule or any installed Coddy Studio copy, and verifies
// that zero matching rules remain.
//
// Usage (elevated PowerShell, from the repository root):
//   node scripts/remove-test-firewall-rules.ts --executable <path-to-test-exe> [--dry-run]
import * as NodeChildProcess from "node:child_process";

/** The app-managed rule created by Share this host. Never removed by this helper. */
export const PROTECTED_FIREWALL_RULE_NAMES: ReadonlyArray<string> = ["Coddy Studio Remote Access"];

export interface WindowsFirewallRuleRecord {
  readonly name: string;
  readonly displayName: string;
  readonly direction: string;
  readonly action: string;
  readonly enabled: string;
  readonly profile: string;
  readonly program: string | null;
}

export interface FirewallRuleSelection {
  readonly remove: ReadonlyArray<WindowsFirewallRuleRecord>;
  readonly protectedSkipped: ReadonlyArray<WindowsFirewallRuleRecord>;
}

export class FirewallCleanupTargetError extends Error {
  override readonly name = "FirewallCleanupTargetError";
}

/**
 * Normalizes a Windows program path for exact comparison: strips the `\\?\`
 * prefix, unifies separators, trims trailing separators, and lower-cases
 * (NTFS is case-insensitive).
 */
export function normalizeWindowsProgramPath(value: string): string {
  return value
    .trim()
    .replace(/^\\\\\?\\/, "")
    .replaceAll("/", "\\")
    .replace(/\\+$/, "")
    .toLowerCase();
}

const INSTALLED_LOCATION_PATTERNS: ReadonlyArray<RegExp> = [
  /^[a-z]:\\program files( \(x86\))?\\/i,
  /^[a-z]:\\users\\[^\\]+\\appdata\\local\\programs\\/i,
  /^[a-z]:\\windows\\/i,
];

/**
 * Rejects targets that are not a repository-built test executable. Installed
 * Coddy Studio copies live under Program Files or the per-user Programs directory and
 * must never lose rules through this helper.
 */
export function assertCleanupTarget(executablePath: string): string {
  const trimmed = executablePath.trim();
  if (trimmed.length === 0) {
    throw new FirewallCleanupTargetError("--executable requires the exact test executable path.");
  }
  if (!/^[a-z]:\\/i.test(trimmed.replace(/^\\\\\?\\/, "").replaceAll("/", "\\"))) {
    throw new FirewallCleanupTargetError(
      `--executable must be an absolute Windows path, received ${trimmed}.`,
    );
  }
  if (!trimmed.toLowerCase().endsWith(".exe")) {
    throw new FirewallCleanupTargetError(
      `--executable must name an .exe file, received ${trimmed}.`,
    );
  }
  const normalized = normalizeWindowsProgramPath(trimmed);
  for (const pattern of INSTALLED_LOCATION_PATTERNS) {
    if (pattern.test(normalized)) {
      throw new FirewallCleanupTargetError(
        `${trimmed} is an installed location; this helper only cleans rules for a repository-built test executable.`,
      );
    }
  }
  return trimmed;
}

/**
 * Selects rules whose program is exactly the test executable. Protected rule
 * names are reported separately and never removed, even when program-scoped to
 * the same executable.
 */
export function selectTestGeneratedFirewallRules(
  rules: ReadonlyArray<WindowsFirewallRuleRecord>,
  executablePath: string,
  protectedRuleNames: ReadonlyArray<string> = PROTECTED_FIREWALL_RULE_NAMES,
): FirewallRuleSelection {
  const target = normalizeWindowsProgramPath(executablePath);
  const protectedNames = new Set(protectedRuleNames.map((name) => name.toLowerCase()));
  const remove: WindowsFirewallRuleRecord[] = [];
  const protectedSkipped: WindowsFirewallRuleRecord[] = [];
  for (const rule of rules) {
    if (rule.program === null || normalizeWindowsProgramPath(rule.program) !== target) {
      continue;
    }
    if (
      protectedNames.has(rule.displayName.toLowerCase()) ||
      protectedNames.has(rule.name.toLowerCase())
    ) {
      protectedSkipped.push(rule);
      continue;
    }
    remove.push(rule);
  }
  return { remove, protectedSkipped };
}

/** PowerShell that lists every rule program-scoped to `$env:CODDY_STUDIO_FIREWALL_TARGET` as JSON. */
export const FIREWALL_RULE_QUERY_SCRIPT = [
  "$ErrorActionPreference = 'Stop'",
  "$target = $env:CODDY_STUDIO_FIREWALL_TARGET",
  "$filters = @(Get-NetFirewallApplicationFilter | Where-Object { $_.Program -and ($_.Program -ieq $target) })",
  "$rules = @($filters | Get-NetFirewallRule)",
  "$records = @($rules | ForEach-Object {",
  "  $filter = $_ | Get-NetFirewallApplicationFilter",
  "  [pscustomobject]@{",
  "    name = [string]$_.Name",
  "    displayName = [string]$_.DisplayName",
  "    direction = [string]$_.Direction",
  "    action = [string]$_.Action",
  "    enabled = [string]$_.Enabled",
  "    profile = [string]$_.Profile",
  "    program = if ($filter.Program) { [string]$filter.Program } else { $null }",
  "  }",
  "})",
  "ConvertTo-Json -InputObject $records -Compress -Depth 3",
].join("\n");

/** PowerShell that removes the rules whose unique names arrive in `$env:CODDY_STUDIO_FIREWALL_RULE_NAMES`. */
export const FIREWALL_RULE_REMOVE_SCRIPT = [
  "$ErrorActionPreference = 'Stop'",
  "$names = $env:CODDY_STUDIO_FIREWALL_RULE_NAMES -split '\\|'",
  "foreach ($name in $names) { if ($name) { Remove-NetFirewallRule -Name $name } }",
].join("\n");

export function parseFirewallRuleRecords(json: string): ReadonlyArray<WindowsFirewallRuleRecord> {
  const trimmed = json.trim();
  if (trimmed.length === 0) return [];
  const parsed: unknown = JSON.parse(trimmed);
  const entries = Array.isArray(parsed) ? parsed : [parsed];
  return entries.map((entry) => {
    const record = entry as Record<string, unknown>;
    const text = (key: string): string => (typeof record[key] === "string" ? record[key] : "");
    return {
      name: text("name"),
      displayName: text("displayName"),
      direction: text("direction"),
      action: text("action"),
      enabled: text("enabled"),
      profile: text("profile"),
      program: typeof record.program === "string" ? record.program : null,
    };
  });
}

export interface FirewallCleanupCliInput {
  readonly executable?: string;
  readonly dryRun: boolean;
}

export function parseFirewallCleanupArgs(argv: ReadonlyArray<string>): FirewallCleanupCliInput {
  let executable: string | undefined;
  let dryRun = false;
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--executable") {
      executable = argv[index + 1];
      index += 1;
    } else if (argument?.startsWith("--executable=")) {
      executable = argument.slice("--executable=".length);
    } else if (argument === "--dry-run") {
      dryRun = true;
    } else {
      throw new FirewallCleanupTargetError(`Unknown argument ${argument ?? ""}.`);
    }
  }
  return { ...(executable === undefined ? {} : { executable }), dryRun };
}

interface PowerShellRunner {
  (script: string, env: Readonly<Record<string, string>>): { stdout: string; status: number };
}

function runPowerShell(script: string, env: Readonly<Record<string, string>>) {
  const result = NodeChildProcess.spawnSync(
    "powershell.exe",
    ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script],
    { encoding: "utf8", env: { ...process.env, ...env }, stdio: ["ignore", "pipe", "inherit"] },
  );
  if (result.error) throw result.error;
  return { stdout: result.stdout ?? "", status: result.status ?? 1 };
}

export function runFirewallCleanup(
  input: FirewallCleanupCliInput,
  options: {
    readonly powerShell?: PowerShellRunner;
    readonly write?: (text: string) => void;
    readonly platform?: NodeJS.Platform;
  } = {},
): number {
  const write = options.write ?? ((text: string) => process.stdout.write(text));
  // oxlint-disable-next-line coddy/no-global-process-runtime -- The standalone cleanup CLI samples the host platform once and still accepts an injected platform in tests.
  const platform = options.platform ?? process.platform;
  if (platform !== "win32") {
    write(
      "[firewall-cleanup] Windows Defender Firewall rules exist only on Windows; nothing to do.\n",
    );
    return 0;
  }
  const executable = assertCleanupTarget(input.executable ?? "");
  const powerShell = options.powerShell ?? runPowerShell;
  const query = () => {
    const result = powerShell(FIREWALL_RULE_QUERY_SCRIPT, { CODDY_STUDIO_FIREWALL_TARGET: executable });
    if (result.status !== 0) {
      throw new Error(`Firewall rule query exited with code ${String(result.status)}.`);
    }
    return parseFirewallRuleRecords(result.stdout);
  };

  const before = selectTestGeneratedFirewallRules(query(), executable);
  for (const rule of before.protectedSkipped) {
    write(`[firewall-cleanup] Keeping protected rule ${rule.displayName} (${rule.name}).\n`);
  }
  if (before.remove.length === 0) {
    write(`[firewall-cleanup] No test-generated rules reference ${executable}.\n`);
    return 0;
  }
  for (const rule of before.remove) {
    write(
      `[firewall-cleanup] ${input.dryRun ? "Would remove" : "Removing"} ${rule.displayName} (${rule.name}) ${rule.direction}/${rule.action}/${rule.profile} enabled=${rule.enabled}.\n`,
    );
  }
  if (input.dryRun) return 0;

  const removal = powerShell(FIREWALL_RULE_REMOVE_SCRIPT, {
    CODDY_STUDIO_FIREWALL_RULE_NAMES: before.remove.map((rule) => rule.name).join("|"),
  });
  if (removal.status !== 0) {
    write(
      `[firewall-cleanup] Removal exited with code ${String(removal.status)}; rerun from an elevated PowerShell.\n`,
    );
    return removal.status;
  }
  const after = selectTestGeneratedFirewallRules(query(), executable);
  if (after.remove.length > 0) {
    for (const rule of after.remove) {
      write(`[firewall-cleanup] Still present: ${rule.displayName} (${rule.name}).\n`);
    }
    return 1;
  }
  write(
    `[firewall-cleanup] Verified zero test-generated rules remain for ${executable} (${String(before.remove.length)} removed).\n`,
  );
  return 0;
}

export function runFirewallCleanupMain(
  isMain: boolean,
  argv: ReadonlyArray<string> = process.argv.slice(2),
): boolean {
  if (!isMain) return false;
  try {
    process.exitCode = runFirewallCleanup(parseFirewallCleanupArgs(argv));
  } catch (error) {
    console.error(`[firewall-cleanup] ${error instanceof Error ? error.message : String(error)}`);
    process.exitCode = 2;
  }
  return true;
}

runFirewallCleanupMain(import.meta.main);
