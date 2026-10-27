#!/usr/bin/env node
import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import * as NodeURL from "node:url";

const targetArgument = (args) => {
  const index = args.indexOf("--target");
  if (index >= 0) return args[index + 1];
  return args.find((argument) => argument.startsWith("--target="))?.slice("--target=".length);
};

const normalizeArchitecture = (value) => {
  const normalized = value?.trim().toLowerCase();
  if (!normalized) return undefined;
  if (normalized.includes("aarch64") || normalized.includes("arm64")) return "arm64";
  if (normalized.includes("x86_64") || normalized.includes("amd64") || normalized === "x64") {
    return "x64";
  }
  return undefined;
};

export function resolveMsvcArchitecture(args, env) {
  return (
    normalizeArchitecture(targetArgument(args)) ??
    normalizeArchitecture(env.CARGO_BUILD_TARGET) ??
    normalizeArchitecture(env.TAURI_DESKTOP_ARCH) ??
    normalizeArchitecture(env.PROCESSOR_ARCHITEW6432) ??
    normalizeArchitecture(env.PROCESSOR_ARCHITECTURE) ??
    "x64"
  );
}

export function msvcToolchain(architecture) {
  return architecture === "arm64"
    ? {
        cargoRunnerKey: "CARGO_TARGET_AARCH64_PC_WINDOWS_MSVC_RUNNER",
        vcvarsArgument: "arm64",
        vsComponent: "Microsoft.VisualStudio.Component.VC.Tools.ARM64",
      }
    : {
        cargoRunnerKey: "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUNNER",
        vcvarsArgument: "x64",
        vsComponent: "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
      };
}

export function run(command, commandArgs, options = {}, spawnSync = NodeChildProcess.spawnSync) {
  return (
    spawnSync(command, commandArgs, {
      stdio: "inherit",
      shell: false,
      ...options,
    }).status ?? 1
  );
}

export function discoverVcVarsAll(options = {}) {
  const architecture = options.architecture ?? "x64";
  const toolchain = msvcToolchain(architecture);
  const programFilesX86 = options.programFilesX86 ?? process.env["ProgramFiles(x86)"];
  const existsSync = options.existsSync ?? NodeFS.existsSync;
  const spawnSync = options.spawnSync ?? NodeChildProcess.spawnSync;
  if (!programFilesX86) {
    return null;
  }

  const vswhere = NodePath.join(
    programFilesX86,
    "Microsoft Visual Studio",
    "Installer",
    "vswhere.exe",
  );
  if (existsSync(vswhere)) {
    const result = spawnSync(
      vswhere,
      [
        "-latest",
        "-products",
        "*",
        "-requires",
        toolchain.vsComponent,
        "-property",
        "installationPath",
      ],
      { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] },
    );
    const installationPath = result.stdout.trim().split(/\r?\n/).at(0);
    if (installationPath) {
      const candidate = NodePath.join(
        installationPath,
        "VC",
        "Auxiliary",
        "Build",
        "vcvarsall.bat",
      );
      if (existsSync(candidate)) {
        return candidate;
      }
    }
  }

  const fallback = NodePath.join(
    programFilesX86,
    "Microsoft Visual Studio",
    "2022",
    "BuildTools",
    "VC",
    "Auxiliary",
    "Build",
    "vcvarsall.bat",
  );
  return existsSync(fallback) ? fallback : null;
}

export function quoteCmdArg(value) {
  if (/^[A-Za-z0-9_./:\\-]+$/.test(value)) {
    return value;
  }
  return `"${value.replaceAll('"', '\\"')}"`;
}

export function defaultWindowsCargoRunner(options = {}) {
  const command = options.command ?? "node";
  const repoRoot = options.repoRoot ?? NodePath.resolve(import.meta.dirname, "..");
  return [command, NodePath.win32.join(repoRoot, "scripts", "run-windows-cargo-target.mjs")]
    .map(quoteCmdArg)
    .join(" ");
}

export const WINDOWS_PACKAGING_PREFLIGHT_EXIT_CODE = 3;

const SYSTEM_PROFILE_SEGMENT = /[\\/]config[\\/]systemprofile(?:[\\/]|$)/i;

/** `tauri build`, or `tauri bundle`, which packages an app that `tauri build --no-bundle` compiled. */
export function isTauriBuildCommand(args) {
  const tauriIndex = args.indexOf("tauri");
  return tauriIndex >= 0 && ["build", "bundle"].includes(args[tauriIndex + 1]);
}

/**
 * Detects Windows packaging runs that cannot complete. Tauri downloads the NSIS
 * toolset into the account cache, and the SYSTEM profile
 * (`C:\Windows\System32\config\systemprofile`) is subject to x86 filesystem
 * redirection on ARM64, so the x86 NSIS bootstrapper fails with "Unable to
 * start child process, error 0x2" only after the Rust release build has already
 * finished. Parallels `prlctl exec` without `--current-user` and scheduled
 * tasks are the usual sources. The check reads only the environment so it can
 * fail before any compilation starts.
 *
 * @returns {string | null} an actionable diagnostic, or null when packaging may proceed.
 */
export function windowsPackagingPreflight(
  args,
  env,
  // oxlint-disable-next-line coddy/no-global-process-runtime -- The standalone launcher samples the host platform once and still accepts an injected platform in tests.
  platform = NodeOS.platform(),
) {
  if (platform !== "win32" || !isTauriBuildCommand(args)) {
    return null;
  }
  const username = (env.USERNAME ?? "").trim();
  const reasons = [];
  if (username.length > 0 && (username.endsWith("$") || username.toUpperCase() === "SYSTEM")) {
    reasons.push(`USERNAME is \`${username}\`, a machine or service account`);
  }
  for (const name of ["LOCALAPPDATA", "APPDATA", "USERPROFILE"]) {
    const value = env[name];
    if (typeof value === "string" && SYSTEM_PROFILE_SEGMENT.test(value)) {
      reasons.push(`${name} resolves under the system profile (${value})`);
    }
  }
  if (reasons.length === 0) {
    return null;
  }
  return [
    "Windows packaging is running under the SYSTEM profile, so Tauri would cache the NSIS",
    "toolset where x86 filesystem redirection makes it unusable for the NSIS bootstrapper",
    '("Unable to start child process, error 0x2").',
    `Detected: ${reasons.join("; ")}.`,
    "Run the build as the logged-in interactive user instead, for example",
    "`prlctl exec 'Windows 11' --current-user ...` from a Parallels host, and install",
    "workspace dependencies with that same account so pnpm hard links stay readable.",
    "No Rust build was started.",
  ].join(" ");
}

export function canonicalizeCargoTestTarget(args, env, options = {}) {
  if (args[0] !== "cargo" || args[1] !== "test") {
    return env;
  }

  const configuredTarget = env.CARGO_TARGET_DIR;
  if (configuredTarget === undefined || configuredTarget.length === 0) {
    return env;
  }

  const targetDirectory = NodePath.resolve(options.cwd ?? process.cwd(), configuredTarget);
  const mkdirSync = options.mkdirSync ?? NodeFS.mkdirSync;
  const realpathSync = options.realpathSync ?? NodeFS.realpathSync.native;
  mkdirSync(targetDirectory, { recursive: true });
  return {
    ...env,
    CARGO_TARGET_DIR: realpathSync(targetDirectory),
  };
}

export function runMsvc(args, options = {}) {
  const consoleError = options.consoleError ?? console.error;
  const spawnSync = options.spawnSync ?? NodeChildProcess.spawnSync;
  if (args.length === 0) {
    consoleError("Usage: node scripts/run-msvc.mjs <command> <arguments>");
    return 2;
  }

  const configuredInputEnv = { ...process.env, ...options.env };
  const architecture = resolveMsvcArchitecture(args, configuredInputEnv);
  const toolchain = msvcToolchain(architecture);
  const configuredEnv = {
    ...process.env,
    [toolchain.cargoRunnerKey]:
      options.env?.[toolchain.cargoRunnerKey] ??
      process.env[toolchain.cargoRunnerKey] ??
      defaultWindowsCargoRunner({ repoRoot: options.repoRoot }),
    ...options.env,
  };
  const env = canonicalizeCargoTestTarget(args, configuredEnv, options);
  const preflightFailure = windowsPackagingPreflight(args, env, options.platform);
  if (preflightFailure !== null) {
    consoleError(`[run-msvc] ${preflightFailure}`);
    return WINDOWS_PACKAGING_PREFLIGHT_EXIT_CODE;
  }
  const vcvarsall = discoverVcVarsAll({
    architecture,
    programFilesX86: options.programFilesX86,
    existsSync: options.existsSync,
    spawnSync,
  });
  if (!vcvarsall) {
    return run(args[0], args.slice(1), { env }, spawnSync);
  }

  const comspec = options.comspec ?? process.env.ComSpec ?? "cmd.exe";
  const scriptPath = NodePath.join(
    options.tmpdir ?? NodeOS.tmpdir(),
    `coddy-studio-msvc-${architecture}-${options.pid ?? process.pid}-${options.now?.() ?? Date.now()}.cmd`,
  );
  const writeFileSync = options.writeFileSync ?? NodeFS.writeFileSync;
  const rmSync = options.rmSync ?? NodeFS.rmSync;
  writeFileSync(
    scriptPath,
    [
      "@echo off",
      `call "${vcvarsall}" ${toolchain.vcvarsArgument}`,
      "if errorlevel 1 exit /b %errorlevel%",
      args.map(quoteCmdArg).join(" "),
      "exit /b %errorlevel%",
      "",
    ].join("\r\n"),
  );

  const status = run(comspec, ["/d", "/c", scriptPath], { env }, spawnSync);
  try {
    rmSync(scriptPath, { force: true });
  } catch {}
  return status;
}

if (
  process.argv[1] !== undefined &&
  import.meta.url === NodeURL.pathToFileURL(process.argv[1]).href
) {
  process.exit(runMsvc(process.argv.slice(2)));
}
