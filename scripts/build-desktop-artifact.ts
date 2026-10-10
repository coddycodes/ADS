#!/usr/bin/env node

import * as NodeRuntime from "@effect/platform-node/NodeRuntime";
import * as NodeServices from "@effect/platform-node/NodeServices";
import { HostProcessArchitecture, HostProcessPlatform } from "@bibcode/shared/hostProcess";
import { readBiBCodeEnvironmentVariable } from "@bibcode/shared/environmentIdentity";
import { resolveSpawnCommand } from "@bibcode/shared/shell";
import * as ConfigProvider from "effect/ConfigProvider";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Layer from "effect/Layer";
import * as Path from "effect/Path";
import * as PlatformError from "effect/PlatformError";
import * as Schema from "effect/Schema";
import * as Stream from "effect/Stream";
import * as NodeCrypto from "node:crypto";
import * as NodeUtil from "node:util";
import { ChildProcess, ChildProcessSpawner } from "effect/unstable/process";

import { getDefaultBuildArch, type BuildArch } from "./lib/build-target-arch.ts";
import {
  requireReleaseTarget,
  symbolArchiveName,
  type ReleaseArch,
  type ReleasePlatform,
  type TauriUpdaterTarget,
} from "./lib/release-targets.ts";

export type TauriBuildPlatform = ReleasePlatform;
export type TauriBuildArch = Extract<BuildArch, ReleaseArch>;
export type TauriUpdaterManifestTarget = TauriUpdaterTarget;

export const TAURI_UPDATER_TARGETS = {
  mac: {
    arm64: requireReleaseTarget("mac", "arm64").updaterTarget,
    x64: requireReleaseTarget("mac", "x64").updaterTarget,
  },
  linux: {
    arm64: requireReleaseTarget("linux", "arm64").updaterTarget,
    x64: requireReleaseTarget("linux", "x64").updaterTarget,
  },
  win: {
    arm64: requireReleaseTarget("win", "arm64").updaterTarget,
    x64: requireReleaseTarget("win", "x64").updaterTarget,
  },
} as const;

interface TauriPlatformConfig {
  readonly hostPlatform: NodeJS.Platform;
  readonly defaultTarget: string;
  readonly allowedTargets: ReadonlyArray<string>;
  readonly archChoices: ReadonlyArray<TauriBuildArch>;
}

export const TAURI_PLATFORM_CONFIG: Record<TauriBuildPlatform, TauriPlatformConfig> = {
  mac: {
    hostPlatform: "darwin",
    defaultTarget: "dmg",
    allowedTargets: ["app", "dmg"],
    archChoices: ["arm64", "x64"],
  },
  linux: {
    hostPlatform: "linux",
    defaultTarget: "appimage",
    allowedTargets: ["appimage", "deb", "rpm"],
    archChoices: ["x64", "arm64"],
  },
  win: {
    hostPlatform: "win32",
    defaultTarget: "nsis",
    allowedTargets: ["nsis", "msi"],
    archChoices: ["x64", "arm64"],
  },
};

export interface TauriBuildCliInput {
  readonly platform?: string;
  readonly target?: string;
  readonly arch?: string;
  readonly outputDir?: string;
  readonly skipBuild?: boolean;
  readonly verbose?: boolean;
  readonly allowCrossPlatform?: boolean;
  readonly updater?: boolean;
  readonly symbols?: boolean;
}

export interface TauriBuildHost {
  readonly platform: NodeJS.Platform;
  readonly arch: NodeJS.Architecture;
}

export interface SpawnPlan {
  readonly command: string;
  readonly args: ReadonlyArray<string>;
  readonly cwd: string;
}

/** Release symbols: dumped from the unstripped binary, which is then stripped before bundling. */
export interface TauriSymbolsPlan {
  readonly archive: string;
  readonly dumpCommand: SpawnPlan;
  /**
   * Writes a stripped copy to `output`, which then replaces `binary`. Cargo's `binary` is a hard
   * link to its cached `deps/` artifact, which stripping in place would strip too, so the next
   * build of unchanged sources would hand dump_syms a binary without symbols.
   */
  readonly strip:
    | { readonly command: SpawnPlan; readonly output: string; readonly binary: string }
    | undefined;
}

export interface TauriBuildPlan {
  readonly platform: TauriBuildPlatform;
  readonly target: string;
  readonly bundleDirectoryName: string;
  readonly arch: TauriBuildArch;
  readonly rustTarget: string;
  readonly outputDir: string;
  readonly bundleDir: string;
  readonly updater: boolean;
  readonly updaterManifestTarget?: TauriUpdaterManifestTarget;
  readonly updaterBundleDir?: string;
  readonly skipBuild: boolean;
  readonly verbose: boolean;
  /** `tauri build --no-bundle`: compiles the app. */
  readonly buildCommand: SpawnPlan;
  /** `tauri bundle`: packages the binary the build left in `target/<triple>/release`. */
  readonly bundleCommand: SpawnPlan;
  readonly symbols: TauriSymbolsPlan | undefined;
}

interface MutableTauriBuildCliInput {
  platform?: string;
  target?: string;
  arch?: string;
  outputDir?: string;
  skipBuild?: boolean;
  verbose?: boolean;
  allowCrossPlatform?: boolean;
  updater?: boolean;
  symbols?: boolean;
}

export interface TauriUpdaterArtifactDescriptor {
  readonly target: TauriUpdaterManifestTarget;
  readonly artifact: string;
  readonly signature: string;
}

const TauriUpdaterArtifactDescriptorJson = Schema.fromJsonString(
  Schema.Struct({
    target: Schema.Union([
      Schema.Literal("windows-aarch64"),
      Schema.Literal("windows-x86_64"),
      Schema.Literal("linux-aarch64"),
      Schema.Literal("linux-x86_64"),
      Schema.Literal("darwin-x86_64"),
      Schema.Literal("darwin-aarch64"),
    ]),
    artifact: Schema.String,
    signature: Schema.String,
  }),
);
const encodeTauriUpdaterArtifactDescriptor = Schema.encodeSync(TauriUpdaterArtifactDescriptorJson);

export class TauriDesktopBuildConfigurationError extends Error {
  override readonly name = "TauriDesktopBuildConfigurationError";
}

export class TauriDesktopBuildHostMismatchError extends Error {
  override readonly name = "TauriDesktopBuildHostMismatchError";
  readonly platform: TauriBuildPlatform;
  readonly hostPlatform: NodeJS.Platform;

  constructor(platform: TauriBuildPlatform, hostPlatform: NodeJS.Platform) {
    super(
      `Tauri ${platform} artifacts require a ${TAURI_PLATFORM_CONFIG[platform].hostPlatform} host. Current host is ${hostPlatform}.`,
    );
    this.platform = platform;
    this.hostPlatform = hostPlatform;
  }
}

export class TauriDesktopBuildDirectoryMissingError extends Error {
  override readonly name = "TauriDesktopBuildDirectoryMissingError";
  readonly bundleDir: string;

  constructor(bundleDir: string) {
    super(`Tauri build completed but no bundle directory was found at ${bundleDir}.`);
    this.bundleDir = bundleDir;
  }
}

export class TauriDesktopBuildNoArtifactsProducedError extends Error {
  override readonly name = "TauriDesktopBuildNoArtifactsProducedError";
  readonly bundleDir: string;

  constructor(bundleDir: string) {
    super(`Tauri build completed but no artifacts were produced in ${bundleDir}.`);
    this.bundleDir = bundleDir;
  }
}

export class TauriDesktopBuildUnsafePathError extends Error {
  override readonly name = "TauriDesktopBuildUnsafePathError";
  readonly bundleDir: string;
  readonly outputDir: string;

  constructor(bundleDir: string, outputDir: string) {
    super(
      `Tauri bundle source and artifact output must be separate, non-overlapping directories: ${bundleDir} -> ${outputDir}.`,
    );
    this.bundleDir = bundleDir;
    this.outputDir = outputDir;
  }
}

export interface TauriDesktopBuildRollbackFailure {
  readonly operation:
    | "inspect-output"
    | "quarantine-output"
    | "remove-output"
    | "restore-output"
    | "remove-staging"
    | "cleanup-backup";
  readonly path: string;
  readonly cause: unknown;
}

export interface TauriDesktopBuildRecoveryPath {
  readonly kind: "backup" | "quarantine" | "staging";
  readonly path: string;
}

export class TauriDesktopBuildPublicationError extends Error {
  override readonly name = "TauriDesktopBuildPublicationError";
  readonly operation: "copy" | "validate-staging" | "swap";
  readonly outputDir: string;
  override readonly cause: unknown;
  readonly rollbackFailures: Array<TauriDesktopBuildRollbackFailure>;
  readonly recoveryPaths: Array<TauriDesktopBuildRecoveryPath>;

  constructor(
    operation: "copy" | "validate-staging" | "swap",
    outputDir: string,
    cause: unknown,
    rollbackFailures: Array<TauriDesktopBuildRollbackFailure>,
    recoveryPaths: Array<TauriDesktopBuildRecoveryPath>,
  ) {
    const baseMessage = `Failed to ${operation.replace("-", " ")} Tauri artifacts at ${outputDir}.`;
    super(baseMessage);
    this.operation = operation;
    this.outputDir = outputDir;
    this.cause = cause;
    this.rollbackFailures = rollbackFailures;
    this.recoveryPaths = recoveryPaths;
    Object.defineProperty(this, "message", {
      configurable: true,
      enumerable: false,
      get: () =>
        this.recoveryPaths.length === 0
          ? baseMessage
          : `${baseMessage} Recovery artifacts retained at: ${this.recoveryPaths
              .map((recovery) => `${recovery.kind}=${recovery.path}`)
              .join(", ")}.`,
    });
  }
}

export interface TauriArtifactPublicationOptions {
  readonly transactionId?: (() => string) | undefined;
  readonly ownershipToken?: (() => string) | undefined;
  readonly copy?: FileSystem.FileSystem["copy"] | undefined;
  readonly exists?: FileSystem.FileSystem["exists"] | undefined;
  readonly makeDirectory?: FileSystem.FileSystem["makeDirectory"] | undefined;
  readonly move?: FileSystem.FileSystem["rename"] | undefined;
  readonly readFileString?: FileSystem.FileSystem["readFileString"] | undefined;
  readonly readDirectory?: FileSystem.FileSystem["readDirectory"] | undefined;
  readonly realPath?: FileSystem.FileSystem["realPath"] | undefined;
  readonly remove?: FileSystem.FileSystem["remove"] | undefined;
  readonly stat?: FileSystem.FileSystem["stat"] | undefined;
  readonly stream?: FileSystem.FileSystem["stream"] | undefined;
  readonly writeFileString?: FileSystem.FileSystem["writeFileString"] | undefined;
}

const TAURI_ARTIFACT_OWNER_FILE = ".bibcode-publication-owner";

const RepoRoot = Effect.service(Path.Path).pipe(
  Effect.flatMap((path) => path.fromFileUrl(new URL("..", import.meta.url))),
);

const tryBuildConfiguration = <A>(evaluate: () => A) =>
  Effect.try({
    try: evaluate,
    catch: (cause) => cause as TauriDesktopBuildConfigurationError,
  });

function compactEnv(env: Readonly<Record<string, string | undefined>>): Record<string, string> {
  return Object.fromEntries(
    Object.entries(env).filter((entry): entry is [string, string] => entry[1] !== undefined),
  );
}

function envBoolean(
  env: Readonly<Record<string, string | undefined>>,
  key: string,
): boolean | undefined {
  const suffix = key.startsWith("BIBCODE_") ? key.slice("BIBCODE_".length) : key;
  const value = readBiBCodeEnvironmentVariable(env, suffix)?.trim().toLowerCase();
  if (!value) return undefined;
  if (value === "1" || value === "true" || value === "yes") return true;
  if (value === "0" || value === "false" || value === "no") return false;
  throw new TauriDesktopBuildConfigurationError(`${key} must be true/false or 1/0.`);
}

/**
 * The release profile dump_syms needs: unstripped and with line tables, plus a dSYM on macOS and
 * an explicit GNU build-id (the debug ID of Linux symbols and minidumps) on Linux. Only symbol
 * builds use it; the shipped profile still strips.
 */
export function symbolBuildEnvironment(
  platform: TauriBuildPlatform,
  env: NodeJS.ProcessEnv,
): NodeJS.ProcessEnv {
  const buildId = "-C link-arg=-Wl,--build-id=sha1";
  return {
    ...env,
    CARGO_PROFILE_RELEASE_STRIP: "none",
    CARGO_PROFILE_RELEASE_DEBUG: "line-tables-only",
    ...(platform === "mac" ? { CARGO_PROFILE_RELEASE_SPLIT_DEBUGINFO: "packed" } : {}),
    ...(platform === "linux"
      ? { RUSTFLAGS: env.RUSTFLAGS ? `${env.RUSTFLAGS} ${buildId}` : buildId }
      : {}),
  };
}

export function detectHostTauriBuildPlatform(
  hostPlatform: NodeJS.Platform,
): TauriBuildPlatform | undefined {
  if (hostPlatform === "darwin") return "mac";
  if (hostPlatform === "linux") return "linux";
  if (hostPlatform === "win32") return "win";
  return undefined;
}

function parseTauriBuildPlatform(value: string | undefined): TauriBuildPlatform | undefined {
  if (value === undefined) return undefined;
  const normalized = value.trim().toLowerCase();
  if (normalized === "mac" || normalized === "linux" || normalized === "win") return normalized;
  throw new TauriDesktopBuildConfigurationError(
    `Unsupported Tauri platform '${value}'. Expected mac, linux, or win.`,
  );
}

function parseTauriBuildArch(value: string | undefined): TauriBuildArch | undefined {
  if (value === undefined) return undefined;
  const normalized = value.trim().toLowerCase();
  if (normalized === "arm64" || normalized === "x64") return normalized;
  throw new TauriDesktopBuildConfigurationError(
    `Unsupported Tauri arch '${value}'. Expected arm64 or x64.`,
  );
}

function normalizeTauriBundleTarget(platform: TauriBuildPlatform, target: string): string {
  const normalized = target.trim().toLowerCase();
  const config = TAURI_PLATFORM_CONFIG[platform];
  if (!config.allowedTargets.some((allowedTarget) => allowedTarget === normalized)) {
    throw new TauriDesktopBuildConfigurationError(
      `Unsupported Tauri ${platform} target '${target}'. Expected one of: ${config.allowedTargets.join(", ")}.`,
    );
  }
  return normalized;
}

export function resolveTauriRustTarget(platform: TauriBuildPlatform, arch: TauriBuildArch): string {
  return requireReleaseTarget(platform, arch).rustTarget;
}

function resolveTauriUpdaterManifestTarget(
  platform: TauriBuildPlatform,
  arch: TauriBuildArch,
): TauriUpdaterManifestTarget {
  return requireReleaseTarget(platform, arch).updaterTarget;
}

function withHostRuntime(host: TauriBuildHost, env: Readonly<Record<string, string | undefined>>) {
  return Effect.provide(
    Layer.mergeAll(
      Layer.succeed(HostProcessPlatform, host.platform),
      Layer.succeed(HostProcessArchitecture, host.arch),
      ConfigProvider.layer(ConfigProvider.fromEnv({ env: compactEnv(env) })),
    ),
  );
}

const resolveDefaultArch = Effect.fn("resolveDefaultTauriArch")(function* (
  platform: TauriBuildPlatform,
  host: TauriBuildHost,
  env: Readonly<Record<string, string | undefined>>,
) {
  const arch = yield* getDefaultBuildArch(platform, TAURI_PLATFORM_CONFIG[platform]).pipe(
    withHostRuntime(host, env),
  );
  const parsedArch = yield* tryBuildConfiguration(() => parseTauriBuildArch(arch));
  return parsedArch as TauriBuildArch;
});

export const resolveTauriBuildPlan = Effect.fn("resolveTauriBuildPlan")(function* (
  input: TauriBuildCliInput,
  env: Readonly<Record<string, string | undefined>> = process.env,
  hostInput?: TauriBuildHost,
  repoRootInput?: string,
) {
  const path = yield* Path.Path;
  const repoRoot = repoRootInput ?? (yield* RepoRoot);
  const host = hostInput ?? {
    platform: yield* HostProcessPlatform,
    arch: yield* HostProcessArchitecture,
  };
  const platform =
    (yield* tryBuildConfiguration(() =>
      parseTauriBuildPlatform(
        input.platform ?? readBiBCodeEnvironmentVariable(env, "TAURI_DESKTOP_PLATFORM"),
      ),
    )) ?? detectHostTauriBuildPlatform(host.platform);
  if (!platform) {
    return yield* Effect.fail(
      new TauriDesktopBuildConfigurationError(
        `Unsupported host platform '${host.platform}'. Pass --platform on a supported host.`,
      ),
    );
  }

  const arch =
    (yield* tryBuildConfiguration(() =>
      parseTauriBuildArch(input.arch ?? readBiBCodeEnvironmentVariable(env, "TAURI_DESKTOP_ARCH")),
    )) ?? (yield* resolveDefaultArch(platform, host, env));
  const target = yield* tryBuildConfiguration(() =>
    normalizeTauriBundleTarget(
      platform,
      input.target ??
        readBiBCodeEnvironmentVariable(env, "TAURI_DESKTOP_TARGET") ??
        TAURI_PLATFORM_CONFIG[platform].defaultTarget,
    ),
  );
  const allowCrossPlatform =
    input.allowCrossPlatform ??
    (yield* tryBuildConfiguration(() =>
      envBoolean(env, "BIBCODE_TAURI_DESKTOP_ALLOW_CROSS_PLATFORM"),
    )) ??
    false;
  const updater =
    input.updater ??
    (yield* tryBuildConfiguration(() => envBoolean(env, "BIBCODE_TAURI_DESKTOP_UPDATER"))) ??
    false;
  const skipBuild =
    input.skipBuild ??
    (yield* tryBuildConfiguration(() => envBoolean(env, "BIBCODE_TAURI_DESKTOP_SKIP_BUILD"))) ??
    false;
  if (input.symbols === true && skipBuild) {
    return yield* Effect.fail(
      new TauriDesktopBuildConfigurationError(
        "--symbols dumps the binary a build produces; it cannot be combined with --skip-build.",
      ),
    );
  }
  if (!allowCrossPlatform && host.platform !== TAURI_PLATFORM_CONFIG[platform].hostPlatform) {
    return yield* Effect.fail(new TauriDesktopBuildHostMismatchError(platform, host.platform));
  }

  const rustTarget = resolveTauriRustTarget(platform, arch);
  const outputDir = path.resolve(
    repoRoot,
    input.outputDir ??
      readBiBCodeEnvironmentVariable(env, "TAURI_DESKTOP_OUTPUT_DIR") ??
      path.join("release", "desktop", `${platform}-${arch}`),
  );
  const bundleDirectoryName = target === "app" ? "macos" : target;
  const bundleDir = path.join(
    repoRoot,
    "target",
    rustTarget,
    "release",
    "bundle",
    bundleDirectoryName,
  );
  const updaterManifestTarget = updater
    ? yield* tryBuildConfiguration(() => resolveTauriUpdaterManifestTarget(platform, arch))
    : undefined;
  const updaterBundleDir =
    updater && platform === "mac" && target === "dmg"
      ? path.join(repoRoot, "target", rustTarget, "release", "bundle", "macos")
      : undefined;
  const tauriCommand = (step: "build" | "bundle"): SpawnPlan => ({
    command: "vp",
    args: [
      "run",
      "--filter",
      "@bibcode/desktop",
      step,
      ...(step === "build" ? ["--no-bundle"] : []),
      ...(updater
        ? ["--config", path.join(repoRoot, "apps/desktop/src-tauri/tauri.release.conf.json")]
        : []),
      ...(step === "bundle"
        ? ["--bundles", updater && platform === "mac" && target === "dmg" ? "app,dmg" : target]
        : []),
      "--target",
      rustTarget,
      // tauri-bundler logs the generated bundle_dmg.sh (create-dmg, hdiutil,
      // AppleScript) output only at debug level, so a DMG build without
      // --verbose reports a failed attempt as an opaque "failed to run
      // bundle_dmg.sh". Verbose logging keeps the first failure diagnosable.
      ...(step === "bundle" && platform === "mac" && target === "dmg" ? ["--verbose"] : []),
    ],
    cwd: repoRoot,
  });

  return {
    platform,
    target,
    bundleDirectoryName,
    arch,
    rustTarget,
    outputDir,
    bundleDir,
    updater,
    updaterManifestTarget,
    updaterBundleDir,
    skipBuild,
    verbose:
      input.verbose ??
      (yield* tryBuildConfiguration(() => envBoolean(env, "BIBCODE_TAURI_DESKTOP_VERBOSE"))) ??
      false,
    buildCommand: tauriCommand("build"),
    bundleCommand: tauriCommand("bundle"),
    symbols:
      input.symbols === true ? yield* planSymbols(platform, arch, rustTarget, repoRoot) : undefined,
  };
});

const decodeDesktopPackageVersion = Schema.decodeEffect(
  Schema.fromJsonString(Schema.Struct({ version: Schema.String })),
);

const planSymbols = Effect.fn("planTauriSymbols")(function* (
  platform: TauriBuildPlatform,
  arch: TauriBuildArch,
  rustTarget: string,
  repoRoot: string,
) {
  const fs = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  // apps/desktop/package.json is the desktop version source; release CI aligns it first.
  const { version } = yield* fs
    .readFileString(path.join(repoRoot, "apps", "desktop", "package.json"))
    .pipe(Effect.flatMap(decodeDesktopPackageVersion));
  const release = path.join(repoRoot, "target", rustTarget, "release");
  // Tauri ships the binary under Cargo's name (no mainBinaryName override).
  const binary = path.join(release, platform === "win" ? "bibcode-desktop.exe" : "bibcode-desktop");
  const archive = path.join(
    release,
    symbolArchiveName("desktop", version, requireReleaseTarget(platform, arch)),
  );
  const debugFile =
    platform === "mac"
      ? ["--dsym", `${binary}.dSYM`]
      : platform === "win"
        ? ["--pdb", path.join(release, "bibcode_desktop.pdb")]
        : [];
  return {
    archive,
    dumpCommand: {
      command: process.execPath,
      args: [
        path.join(repoRoot, "scripts", "dump-symbols.ts"),
        "--binary",
        binary,
        "--module-name",
        path.basename(binary),
        "--out",
        path.join(release, "symbols"),
        ...debugFile,
        "--zip",
        archive,
      ],
      cwd: repoRoot,
    },
    // The shipped profile strips symbols; Windows keeps them in the PDB, which is not shipped.
    strip:
      platform === "win"
        ? undefined
        : {
            command: {
              command: "strip",
              args: [platform === "mac" ? "-x" : "--strip-all", "-o", `${binary}.stripped`, binary],
              cwd: repoRoot,
            },
            output: `${binary}.stripped`,
            binary,
          },
  } satisfies TauriSymbolsPlan;
});

export const TAURI_BUILD_ATTEMPTS = 3;

export interface DetachedDmgImage {
  readonly imagePath: string;
  readonly device: string;
}

export interface DmgMountCleanupReport {
  readonly detached: ReadonlyArray<DetachedDmgImage>;
  readonly failures: ReadonlyArray<{ readonly imagePath: string; readonly detail: string }>;
}

const EMPTY_DMG_MOUNT_CLEANUP: DmgMountCleanupReport = { detached: [], failures: [] };

interface HdiutilImage {
  readonly imagePath: string;
  readonly devices: ReadonlyArray<string>;
}

/**
 * Reads the images `hdiutil info -plist` reports. The XML lists each image's
 * `image-path` before its `system-entities`, so a linear scan of key/string
 * pairs attributes every `dev-entry` to the most recent image.
 */
export function parseHdiutilImages(plist: string): ReadonlyArray<HdiutilImage> {
  const images: Array<{ imagePath: string; devices: string[] }> = [];
  const pairs = plist.matchAll(/<key>([^<]*)<\/key>\s*<string>([^<]*)<\/string>/g);
  for (const [, key, value] of pairs) {
    if (key === "image-path") {
      images.push({ imagePath: decodePlistText(value ?? ""), devices: [] });
    } else if (key === "dev-entry" && images.length > 0) {
      images[images.length - 1]!.devices.push(decodePlistText(value ?? ""));
    }
  }
  return images;
}

function decodePlistText(value: string): string {
  return value
    .replaceAll("&lt;", "<")
    .replaceAll("&gt;", ">")
    .replaceAll("&quot;", '"')
    .replaceAll("&apos;", "'")
    .replaceAll("&amp;", "&");
}

/**
 * The intermediate read/write image the generated `bundle_dmg.sh` attaches is
 * `rw.<name>.dmg` inside the bundle's dmg directory. Only an image at exactly
 * that location belongs to this build; anything else on the host (a user's
 * mounted release DMG with the same volume name, another worktree's build)
 * is never touched.
 */
export function isRunOwnedIntermediateDmg(
  path: Path.Path,
  bundleDir: string,
  imagePath: string,
): boolean {
  const parent = path.dirname(imagePath);
  const name = path.basename(imagePath);
  return (
    path.resolve(parent) === path.resolve(bundleDir) &&
    name.startsWith("rw.") &&
    name.endsWith(".dmg")
  );
}

const captureStdout = Effect.fn("captureSpawnStdout")(function* (
  command: string,
  args: ReadonlyArray<string>,
  env: NodeJS.ProcessEnv,
) {
  const spawner = yield* ChildProcessSpawner.ChildProcessSpawner;
  const child = yield* spawner.spawn(
    ChildProcess.make(command, args, { env, stdout: "pipe", stderr: "inherit" }),
  );
  const stdout = yield* child.stdout.pipe(Stream.decodeText(), Stream.mkString);
  const exitCode = yield* child.exitCode;
  return { exitCode: Number(exitCode), stdout };
});

/**
 * Detaches the intermediate images a failed DMG attempt left mounted so the
 * next attempt starts from a clean host. Cleanup is scoped to this build's
 * bundle directory and never relies on the retry itself; every detach outcome
 * is reported so a leaked mount is visible even when the retry succeeds.
 */
export const detachRunOwnedDmgMounts = Effect.fn("detachRunOwnedDmgMounts")(function* (
  plan: Pick<TauriBuildPlan, "platform" | "target" | "bundleDir">,
  env: NodeJS.ProcessEnv,
) {
  if (plan.platform !== "mac" || plan.target !== "dmg") {
    return EMPTY_DMG_MOUNT_CLEANUP;
  }
  const path = yield* Path.Path;
  const info = yield* captureStdout("hdiutil", ["info", "-plist"], env).pipe(
    Effect.catchCause((cause) =>
      Effect.succeed({ exitCode: -1, stdout: "", detail: String(cause) as string | undefined }),
    ),
  );
  if (info.exitCode !== 0) {
    return {
      detached: [],
      failures: [
        {
          imagePath: plan.bundleDir,
          detail: `hdiutil info exited with code ${String(info.exitCode)}; mounted images could not be inspected.`,
        },
      ],
    } satisfies DmgMountCleanupReport;
  }
  const detached: DetachedDmgImage[] = [];
  const failures: Array<{ readonly imagePath: string; readonly detail: string }> = [];
  for (const image of parseHdiutilImages(info.stdout)) {
    if (!isRunOwnedIntermediateDmg(path, plan.bundleDir, image.imagePath)) continue;
    const device = image.devices[0];
    if (device === undefined) {
      failures.push({ imagePath: image.imagePath, detail: "no device entry was reported" });
      continue;
    }
    const result = yield* captureStdout("hdiutil", ["detach", device, "-force"], env).pipe(
      Effect.catchCause((cause) => Effect.succeed({ exitCode: -1, stdout: String(cause) })),
    );
    if (result.exitCode === 0) {
      detached.push({ imagePath: image.imagePath, device });
    } else {
      failures.push({
        imagePath: image.imagePath,
        detail: `hdiutil detach ${device} exited with code ${String(result.exitCode)}`,
      });
    }
  }
  return { detached, failures } satisfies DmgMountCleanupReport;
});

export function parseTauriArtifactCliArgs(argv: ReadonlyArray<string>): TauriBuildCliInput {
  const { values } = NodeUtil.parseArgs({
    args: [...argv],
    options: {
      platform: { type: "string" },
      target: { type: "string" },
      arch: { type: "string" },
      "output-dir": { type: "string" },
      "skip-build": { type: "boolean" },
      verbose: { type: "boolean" },
      "allow-cross-platform": { type: "boolean" },
      updater: { type: "boolean" },
      symbols: { type: "boolean" },
    },
    allowPositionals: false,
  });

  const input: MutableTauriBuildCliInput = {};
  if (typeof values.platform === "string") input.platform = values.platform;
  if (typeof values.target === "string") input.target = values.target;
  if (typeof values.arch === "string") input.arch = values.arch;
  if (typeof values["output-dir"] === "string") input.outputDir = values["output-dir"];
  if (typeof values["skip-build"] === "boolean") input.skipBuild = values["skip-build"];
  if (typeof values.verbose === "boolean") input.verbose = values.verbose;
  if (typeof values["allow-cross-platform"] === "boolean") {
    input.allowCrossPlatform = values["allow-cross-platform"];
  }
  if (typeof values.updater === "boolean") input.updater = values.updater;
  if (typeof values.symbols === "boolean") input.symbols = values.symbols;
  return input;
}

const runSpawnPlan = Effect.fn("runTauriSpawnPlan")(function* (
  plan: SpawnPlan,
  env: NodeJS.ProcessEnv,
  label = "Tauri build command",
) {
  const spawner = yield* ChildProcessSpawner.ChildProcessSpawner;
  const spawnCommand = yield* resolveSpawnCommand(plan.command, plan.args, { env });
  const child = yield* spawner.spawn(
    ChildProcess.make(spawnCommand.command, spawnCommand.args, {
      cwd: plan.cwd,
      env,
      shell: spawnCommand.shell,
      stdout: "inherit",
      stderr: "inherit",
    }),
  );
  const exitCode = yield* child.exitCode;
  if (exitCode !== 0) {
    return yield* Effect.fail(
      new TauriDesktopBuildConfigurationError(`${label} exited with code ${exitCode}.`),
    );
  }
});

interface ArtifactManifestEntry {
  readonly checksum: string;
  readonly path: string;
  readonly type: string;
  readonly size: number;
}

const pathContains = (path: Path.Path, parent: string, child: string): boolean => {
  const relative = path.relative(parent, child);
  return (
    relative === "" ||
    (relative !== ".." &&
      !relative.startsWith("../") &&
      !relative.startsWith("..\\") &&
      !path.isAbsolute(relative))
  );
};

const collectArtifactPathManifest = (
  source: string,
  publishedPath: string,
  readDirectory: FileSystem.FileSystem["readDirectory"],
  stat: FileSystem.FileSystem["stat"],
  stream: FileSystem.FileSystem["stream"],
  path: Path.Path,
): Effect.Effect<ReadonlyArray<ArtifactManifestEntry>, PlatformError.PlatformError> => {
  const visit = (
    sourcePath: string,
    targetPath: string,
  ): Effect.Effect<Array<ArtifactManifestEntry>, PlatformError.PlatformError> =>
    Effect.gen(function* () {
      const info = yield* stat(sourcePath);
      const checksum =
        info.type === "File"
          ? yield* stream(sourcePath).pipe(
              Stream.runFold(
                () => NodeCrypto.createHash("sha256"),
                (hash, chunk) => hash.update(chunk),
              ),
              Effect.map((hash) => hash.digest("hex")),
            )
          : "";
      const manifest: Array<ArtifactManifestEntry> = [
        {
          checksum,
          path: targetPath,
          type: info.type,
          size: info.type === "File" ? Number(info.size) : 0,
        },
      ];
      if (info.type === "Directory") {
        for (const entry of (yield* readDirectory(sourcePath)).toSorted()) {
          manifest.push(
            ...(yield* visit(path.join(sourcePath, entry), path.join(targetPath, entry))),
          );
        }
      }
      return manifest;
    });
  return visit(source, publishedPath);
};

const collectArtifactManifest = (
  root: string,
  readDirectory: FileSystem.FileSystem["readDirectory"],
  stat: FileSystem.FileSystem["stat"],
  stream: FileSystem.FileSystem["stream"],
  path: Path.Path,
): Effect.Effect<ReadonlyArray<ArtifactManifestEntry>, PlatformError.PlatformError> =>
  Effect.gen(function* () {
    const manifests = yield* Effect.forEach((yield* readDirectory(root)).toSorted(), (entry) =>
      collectArtifactPathManifest(path.join(root, entry), entry, readDirectory, stat, stream, path),
    );
    return manifests.flat();
  });

const manifestsMatch = (
  source: ReadonlyArray<ArtifactManifestEntry>,
  staged: ReadonlyArray<ArtifactManifestEntry>,
): boolean => {
  const byPath = (left: ArtifactManifestEntry, right: ArtifactManifestEntry) =>
    left.path === right.path ? 0 : left.path < right.path ? -1 : 1;
  const sortedSource = source.toSorted(byPath);
  const sortedStaged = staged.toSorted(byPath);
  return (
    sortedSource.length === sortedStaged.length &&
    sortedSource.every((entry, index) => {
      const other = sortedStaged[index];
      return (
        other?.path === entry.path &&
        other.type === entry.type &&
        other.size === entry.size &&
        other.checksum === entry.checksum
      );
    })
  );
};

const normalizeTauriReleaseAssetBasename = (basename: string): string | undefined => {
  const normalized = basename.replace(/[^A-Za-z0-9._-]+/g, "_").replace(/^\.+|\.+$/g, "");
  return /[A-Za-z0-9]/.test(normalized) ? normalized : undefined;
};

export const copyTauriBundleArtifacts = Effect.fn("copyTauriBundleArtifacts")(function* (
  plan: Pick<TauriBuildPlan, "bundleDir" | "outputDir"> & {
    readonly updaterBundleDir?: string | undefined;
    readonly updaterManifestTarget?: TauriUpdaterManifestTarget | undefined;
    /** Files published beside the bundles under their own names, such as the symbol archive. */
    readonly extraArtifacts?: ReadonlyArray<string> | undefined;
  },
  options: TauriArtifactPublicationOptions = {},
) {
  const fs = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  const copy = options.copy ?? fs.copy;
  const exists = options.exists ?? fs.exists;
  const makeDirectory = options.makeDirectory ?? fs.makeDirectory;
  const move = options.move ?? fs.rename;
  const readFileString = options.readFileString ?? fs.readFileString;
  const readDirectory = options.readDirectory ?? fs.readDirectory;
  const realPath = options.realPath ?? fs.realPath;
  const remove = options.remove ?? fs.remove;
  const stat = options.stat ?? fs.stat;
  const stream = options.stream ?? fs.stream;
  const writeFileString = options.writeFileString ?? fs.writeFileString;
  const bundleDir = path.resolve(plan.bundleDir);
  const updaterBundleDir = plan.updaterBundleDir ? path.resolve(plan.updaterBundleDir) : undefined;
  const outputDir = path.resolve(plan.outputDir);
  const bundleExists = yield* exists(bundleDir);
  if (!bundleExists) {
    return yield* Effect.fail(new TauriDesktopBuildDirectoryMissingError(bundleDir));
  }
  if (updaterBundleDir && !(yield* exists(updaterBundleDir))) {
    return yield* Effect.fail(new TauriDesktopBuildDirectoryMissingError(updaterBundleDir));
  }

  const outputExists = yield* exists(outputDir);
  const canonicalBundleDir = yield* realPath(bundleDir);
  const canonicalUpdaterBundleDir = updaterBundleDir
    ? yield* realPath(updaterBundleDir)
    : undefined;
  const canonicalOutputDir = outputExists ? yield* realPath(outputDir) : outputDir;
  if (
    pathContains(path, canonicalBundleDir, canonicalOutputDir) ||
    pathContains(path, canonicalOutputDir, canonicalBundleDir) ||
    (canonicalUpdaterBundleDir &&
      (pathContains(path, canonicalUpdaterBundleDir, canonicalOutputDir) ||
        pathContains(path, canonicalOutputDir, canonicalUpdaterBundleDir)))
  ) {
    return yield* Effect.fail(new TauriDesktopBuildUnsafePathError(bundleDir, outputDir));
  }

  const entries = (yield* readDirectory(bundleDir)).toSorted();
  if (entries.length === 0) {
    return yield* Effect.fail(new TauriDesktopBuildNoArtifactsProducedError(bundleDir));
  }

  const transactionId = options.transactionId?.() ?? NodeCrypto.randomUUID();
  const ownershipToken = options.ownershipToken?.() ?? NodeCrypto.randomUUID();
  const outputParent = path.dirname(outputDir);
  const outputName = path.basename(outputDir);
  const stagingDir = path.join(outputParent, `.${outputName}.bibcode-${transactionId}.stage`);
  const backupDir = path.join(outputParent, `.${outputName}.bibcode-${transactionId}.backup`);
  const quarantineDir = path.join(
    outputParent,
    `.${outputName}.bibcode-${transactionId}.quarantine`,
  );
  const ownerMarker = (directory: string) => path.join(directory, TAURI_ARTIFACT_OWNER_FILE);
  const rollbackFailures: Array<TauriDesktopBuildRollbackFailure> = [];
  const recoveryPaths: Array<TauriDesktopBuildRecoveryPath> = [];
  const state = {
    committed: false,
    stagingAttempted: false,
    backupReady: false,
    replacementAttempted: false,
  };

  const publicationError = (cause: unknown) =>
    new TauriDesktopBuildPublicationError(
      "copy",
      outputDir,
      cause,
      rollbackFailures,
      recoveryPaths,
    );
  const stagedArtifacts: ReadonlyArray<{ readonly source: string; readonly target: string }> =
    yield* Effect.forEach(entries, (entry) => {
      const source = path.join(bundleDir, entry);
      return fs.stat(source).pipe(
        Effect.mapError(publicationError),
        Effect.flatMap((info) => {
          if (info.type !== "File") return Effect.succeed({ source, target: entry });
          const target = normalizeTauriReleaseAssetBasename(entry);
          return target
            ? Effect.succeed({ source, target })
            : Effect.fail(
                publicationError(
                  new Error(
                    `Artifact basename ${JSON.stringify(entry)} has no meaningful release-safe name.`,
                  ),
                ),
              );
        }),
      );
    });
  let additionalUpdaterArtifacts: ReadonlyArray<{
    readonly source: string;
    readonly target: string;
  }> = [];
  let updaterDescriptor: TauriUpdaterArtifactDescriptor | undefined;

  if (plan.updaterManifestTarget) {
    const payloadDir = updaterBundleDir ?? bundleDir;
    const payloadEntries = updaterBundleDir
      ? (yield* readDirectory(updaterBundleDir)).toSorted()
      : entries;
    const signatures = payloadEntries.filter((entry) => entry.endsWith(".sig"));
    if (signatures.length !== 1) {
      return yield* Effect.fail(
        publicationError(new Error(`Expected exactly one updater signature in ${payloadDir}.`)),
      );
    }
    const signature = signatures[0]!;
    const artifact = signature.slice(0, -".sig".length);
    if (!payloadEntries.includes(artifact)) {
      return yield* Effect.fail(
        publicationError(new Error(`Updater signature ${signature} has no payload sibling.`)),
      );
    }
    if (updaterBundleDir && !artifact.endsWith(".app.tar.gz")) {
      return yield* Effect.fail(
        publicationError(
          new Error(`macOS updater payload ${artifact} must be an .app.tar.gz archive.`),
        ),
      );
    }
    const [artifactInfo, signatureInfo] = yield* Effect.all([
      stat(path.join(payloadDir, artifact)),
      stat(path.join(payloadDir, signature)),
    ]).pipe(Effect.mapError(publicationError));
    if (artifactInfo.type !== "File" || signatureInfo.type !== "File") {
      return yield* Effect.fail(
        publicationError(new Error("Updater payload and signature must be files.")),
      );
    }

    const publishedArtifact = updaterBundleDir
      ? `bibcode-update-${plan.updaterManifestTarget}.app.tar.gz`
      : stagedArtifacts.find(({ source }) => source === path.join(bundleDir, artifact))!.target;
    const publishedSignature = updaterBundleDir
      ? `${publishedArtifact}.sig`
      : stagedArtifacts.find(({ source }) => source === path.join(bundleDir, signature))!.target;
    updaterDescriptor = {
      target: plan.updaterManifestTarget,
      artifact: publishedArtifact,
      signature: publishedSignature,
    };
    if (updaterBundleDir) {
      const duplicateSourceBasename = [artifact, signature].find((entry) =>
        entries.includes(entry),
      );
      if (duplicateSourceBasename) {
        return yield* Effect.fail(
          publicationError(new Error(`Duplicate artifact basename ${duplicateSourceBasename}.`)),
        );
      }
      additionalUpdaterArtifacts = [
        { source: path.join(updaterBundleDir, artifact), target: publishedArtifact },
        { source: path.join(updaterBundleDir, signature), target: publishedSignature },
      ];
    }
  }
  const allStagedArtifacts = [
    ...stagedArtifacts,
    ...additionalUpdaterArtifacts,
    ...(plan.extraArtifacts ?? []).map((source) => ({
      source: path.resolve(source),
      target: path.basename(source),
    })),
  ];
  const duplicateTarget = allStagedArtifacts.find(
    (artifact, index) =>
      allStagedArtifacts.findIndex((candidate) => candidate.target === artifact.target) !== index,
  );
  if (duplicateTarget) {
    return yield* Effect.fail(
      publicationError(new Error(`Duplicate artifact basename ${duplicateTarget.target}.`)),
    );
  }
  const updaterDescriptorName = updaterDescriptor
    ? `updater-${updaterDescriptor.target}.json`
    : undefined;
  if (
    updaterDescriptorName &&
    allStagedArtifacts.some((artifact) => artifact.target === updaterDescriptorName)
  ) {
    return yield* Effect.fail(
      publicationError(new Error(`Duplicate artifact basename ${updaterDescriptorName}.`)),
    );
  }

  const recordCleanup = (
    operation: TauriDesktopBuildRollbackFailure["operation"],
    target: string,
    effect: Effect.Effect<void, PlatformError.PlatformError>,
  ) =>
    effect.pipe(
      Effect.catch((cause) =>
        Effect.sync(() => {
          rollbackFailures.push({ operation, path: target, cause });
        }),
      ),
    );

  const isOwnedQuarantine = readFileString(ownerMarker(quarantineDir)).pipe(
    Effect.match({
      onFailure: () => false,
      onSuccess: (owner) => owner === ownershipToken,
    }),
  );

  const reportRecoveryPath = (kind: TauriDesktopBuildRecoveryPath["kind"], target: string) =>
    exists(target).pipe(
      Effect.match({
        onFailure: () => true,
        onSuccess: (present) => present,
      }),
      Effect.tap((present) =>
        Effect.sync(() => {
          if (present && !recoveryPaths.some((recovery) => recovery.path === target)) {
            recoveryPaths.push({ kind, path: target });
          }
        }),
      ),
      Effect.asVoid,
    );

  const auditRecoveryPaths = Effect.gen(function* () {
    yield* reportRecoveryPath("backup", backupDir);
    yield* reportRecoveryPath("quarantine", quarantineDir);
    yield* reportRecoveryPath("staging", stagingDir);
  });

  const inspectOutput = (assumePresent: boolean) =>
    exists(outputDir).pipe(
      Effect.match({
        onFailure: (cause) => {
          rollbackFailures.push({ operation: "inspect-output", path: outputDir, cause });
          return assumePresent;
        },
        onSuccess: (present) => present,
      }),
    );

  return yield* Effect.acquireUseRelease(
    Effect.void,
    () =>
      Effect.gen(function* () {
        yield* makeDirectory(outputParent, { recursive: true });
        yield* makeDirectory(stagingDir).pipe(
          Effect.mapError(
            (cause) =>
              new TauriDesktopBuildPublicationError(
                "copy",
                outputDir,
                cause,
                rollbackFailures,
                recoveryPaths,
              ),
          ),
        );
        state.stagingAttempted = true;

        for (const artifact of allStagedArtifacts) {
          yield* copy(artifact.source, path.join(stagingDir, artifact.target)).pipe(
            Effect.mapError(
              (cause) =>
                new TauriDesktopBuildPublicationError(
                  "copy",
                  outputDir,
                  cause,
                  rollbackFailures,
                  recoveryPaths,
                ),
            ),
          );
        }

        const validatedSourceManifest = yield* Effect.forEach(allStagedArtifacts, (artifact) =>
          collectArtifactPathManifest(
            artifact.source,
            artifact.target,
            readDirectory,
            stat,
            stream,
            path,
          ),
        ).pipe(
          Effect.map((manifests) => manifests.flat()),
          Effect.mapError(
            (cause) =>
              new TauriDesktopBuildPublicationError(
                "validate-staging",
                outputDir,
                cause,
                rollbackFailures,
                recoveryPaths,
              ),
          ),
        );
        const stagedManifest = yield* collectArtifactManifest(
          stagingDir,
          readDirectory,
          stat,
          stream,
          path,
        ).pipe(
          Effect.mapError(
            (cause) =>
              new TauriDesktopBuildPublicationError(
                "validate-staging",
                outputDir,
                cause,
                rollbackFailures,
                recoveryPaths,
              ),
          ),
        );
        if (!manifestsMatch(validatedSourceManifest, stagedManifest)) {
          return yield* Effect.fail(
            new TauriDesktopBuildPublicationError(
              "validate-staging",
              outputDir,
              new Error("Staged artifact manifest does not match the source bundle."),
              rollbackFailures,
              recoveryPaths,
            ),
          );
        }

        if (updaterDescriptor) {
          yield* writeFileString(
            path.join(stagingDir, `updater-${updaterDescriptor.target}.json`),
            encodeTauriUpdaterArtifactDescriptor(updaterDescriptor),
          ).pipe(
            Effect.mapError(
              (cause) =>
                new TauriDesktopBuildPublicationError(
                  "copy",
                  outputDir,
                  cause,
                  rollbackFailures,
                  recoveryPaths,
                ),
            ),
          );
        }

        yield* writeFileString(ownerMarker(stagingDir), ownershipToken).pipe(
          Effect.mapError(
            (cause) =>
              new TauriDesktopBuildPublicationError(
                "swap",
                outputDir,
                cause,
                rollbackFailures,
                recoveryPaths,
              ),
          ),
        );

        if (outputExists) {
          yield* move(outputDir, backupDir).pipe(
            Effect.mapError(
              (cause) =>
                new TauriDesktopBuildPublicationError(
                  "swap",
                  outputDir,
                  cause,
                  rollbackFailures,
                  recoveryPaths,
                ),
            ),
          );
          state.backupReady = true;
        }
        state.replacementAttempted = true;
        yield* move(stagingDir, outputDir).pipe(
          Effect.mapError(
            (cause) =>
              new TauriDesktopBuildPublicationError(
                "swap",
                outputDir,
                cause,
                rollbackFailures,
                recoveryPaths,
              ),
          ),
        );
        state.stagingAttempted = false;
        state.committed = true;
        return [
          ...allStagedArtifacts.map((artifact) => path.join(outputDir, artifact.target)),
          ...(updaterDescriptor
            ? [path.join(outputDir, `updater-${updaterDescriptor.target}.json`)]
            : []),
        ];
      }),
    () =>
      Effect.gen(function* () {
        let committedBackupCleanupFailure: { readonly cause: unknown } | undefined;
        if (!state.committed && state.replacementAttempted) {
          if (yield* inspectOutput(false)) {
            const quarantineSucceeded = yield* move(outputDir, quarantineDir).pipe(
              Effect.match({
                onFailure: (cause) => {
                  rollbackFailures.push({
                    operation: "quarantine-output",
                    path: outputDir,
                    cause,
                  });
                  return false;
                },
                onSuccess: () => true,
              }),
            );
            if (quarantineSucceeded) {
              if (yield* isOwnedQuarantine) {
                yield* recordCleanup(
                  "remove-output",
                  quarantineDir,
                  remove(quarantineDir, { recursive: true, force: true }),
                );
              } else if (!(yield* inspectOutput(true))) {
                yield* recordCleanup("restore-output", outputDir, move(quarantineDir, outputDir));
              }
            }
          }
          if (state.backupReady && !(yield* inspectOutput(true))) {
            yield* move(backupDir, outputDir).pipe(
              Effect.match({
                onFailure: (cause) => {
                  rollbackFailures.push({
                    operation: "restore-output",
                    path: outputDir,
                    cause,
                  });
                },
                onSuccess: () => {
                  state.backupReady = false;
                },
              }),
            );
          }
        }
        if (state.stagingAttempted) {
          yield* recordCleanup(
            "remove-staging",
            stagingDir,
            remove(stagingDir, { recursive: true, force: true }),
          );
        }
        if (state.backupReady && state.committed) {
          yield* remove(backupDir, { recursive: true, force: true }).pipe(
            Effect.match({
              onFailure: (cause) => {
                rollbackFailures.push({ operation: "cleanup-backup", path: backupDir, cause });
                committedBackupCleanupFailure = { cause };
              },
              onSuccess: () => {
                state.backupReady = false;
              },
            }),
          );
        }
        if (committedBackupCleanupFailure) {
          return yield* Effect.fail(
            new TauriDesktopBuildPublicationError(
              "swap",
              outputDir,
              committedBackupCleanupFailure.cause,
              rollbackFailures,
              recoveryPaths,
            ),
          );
        }
      }).pipe(Effect.ensuring(auditRecoveryPaths)),
  );
});

/**
 * Runs a Tauri build or bundle step with bounded attempts. Every failed attempt
 * is reported with its exit code, this build's leaked intermediate DMG mounts
 * are detached and reported before the next attempt, and the final error
 * preserves the first failure instead of only the last one.
 */
const runBuildAttempts = Effect.fn("runTauriBuildAttempts")(function* (
  plan: Pick<TauriBuildPlan, "platform" | "target" | "bundleDir">,
  step: "build" | "bundle",
  command: SpawnPlan,
  env: NodeJS.ProcessEnv,
  write: (text: string) => void,
) {
  const failures: string[] = [];
  for (let attempt = 1; attempt <= TAURI_BUILD_ATTEMPTS; attempt += 1) {
    const outcome = yield* runSpawnPlan(command, env, `Tauri ${step} command`).pipe(Effect.result);
    if (outcome._tag === "Success") {
      return;
    }
    failures.push(outcome.failure.message);
    write(
      `[desktop-artifact] ${step === "build" ? "Build" : "Bundle"} attempt ${String(attempt)} of ${String(TAURI_BUILD_ATTEMPTS)} failed: ${outcome.failure.message}\n`,
    );
    const cleanup = yield* detachRunOwnedDmgMounts(plan, env);
    for (const image of cleanup.detached) {
      write(
        `[desktop-artifact] Detached this build's intermediate image ${image.imagePath} (${image.device}) left mounted by the failed attempt.\n`,
      );
    }
    for (const failure of cleanup.failures) {
      write(
        `[desktop-artifact] Could not detach ${failure.imagePath}: ${failure.detail}. Detach it manually before relying on the result.\n`,
      );
    }
  }
  return yield* Effect.fail(
    new TauriDesktopBuildConfigurationError(
      `Tauri ${step} failed after ${String(TAURI_BUILD_ATTEMPTS)} attempts. First failure: ${failures[0] ?? "unknown"}. Last failure: ${failures.at(-1) ?? "unknown"}.`,
    ),
  );
});

export const buildTauriDesktopArtifact = Effect.fn("buildTauriDesktopArtifact")(function* (
  input: TauriBuildCliInput,
  env: NodeJS.ProcessEnv = process.env,
  options: {
    readonly write?: (text: string) => void;
    readonly host?: TauriBuildHost;
    readonly repoRoot?: string;
  } = {},
) {
  const write = options.write ?? ((text: string) => process.stdout.write(text));
  const fs = yield* FileSystem.FileSystem;
  const plan = yield* resolveTauriBuildPlan(input, env, options.host, options.repoRoot);
  if (!plan.skipBuild) {
    write(
      `[desktop-artifact] Building ${plan.platform}/${plan.target} (${plan.arch}, ${plan.rustTarget})...\n`,
    );
    // Bundling gets the same profile, so Cargo would find the build fresh if Tauri ran it.
    const buildEnv = plan.symbols ? symbolBuildEnvironment(plan.platform, env) : env;
    yield* runBuildAttempts(plan, "build", plan.buildCommand, buildEnv, write);
    if (plan.symbols) {
      write(`[desktop-artifact] Dumping symbols into ${plan.symbols.archive}...\n`);
      yield* runSpawnPlan(plan.symbols.dumpCommand, env, "dump-symbols.ts");
      if (plan.symbols.strip) {
        yield* runSpawnPlan(plan.symbols.strip.command, env, "strip");
        yield* fs.rename(plan.symbols.strip.output, plan.symbols.strip.binary);
      }
    }
    yield* runBuildAttempts(plan, "bundle", plan.bundleCommand, buildEnv, write);
  }

  const artifacts = yield* copyTauriBundleArtifacts({
    ...plan,
    extraArtifacts: plan.symbols ? [plan.symbols.archive] : [],
  });
  write(`[desktop-artifact] Artifacts copied to ${plan.outputDir}\n`);
  if (plan.verbose) {
    for (const artifact of artifacts) {
      write(` - ${artifact}\n`);
    }
  }
  return artifacts;
});

const cliRuntimeLayer = Layer.mergeAll(NodeServices.layer);

type MainLauncher = <E, A>(effect: Effect.Effect<A, E, never>) => void;

export function runBuildTauriDesktopArtifactMain(
  isMain: boolean,
  argv: ReadonlyArray<string> = process.argv.slice(2),
  launch: MainLauncher = NodeRuntime.runMain,
): boolean {
  if (!isMain) return false;
  launch(
    buildTauriDesktopArtifact(parseTauriArtifactCliArgs(argv)).pipe(
      Effect.scoped,
      Effect.provide(cliRuntimeLayer),
    ),
  );
  return true;
}

runBuildTauriDesktopArtifactMain(import.meta.main);
