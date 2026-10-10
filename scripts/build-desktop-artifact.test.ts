import * as NodeServices from "@effect/platform-node/NodeServices";
import { HostProcessArchitecture, HostProcessPlatform } from "@bibcode/shared/hostProcess";
import { assert, it } from "@effect/vitest";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import * as Path from "effect/Path";
import * as PlatformError from "effect/PlatformError";
import * as Schema from "effect/Schema";
import * as Sink from "effect/Sink";
import * as Stream from "effect/Stream";
import { ChildProcessSpawner } from "effect/unstable/process";
import { vi } from "vite-plus/test";

import { syntheticElf, syntheticMachO } from "./lib/binary-fixtures.ts";

import {
  copyTauriBundleArtifacts,
  buildTauriDesktopArtifact,
  detachRunOwnedDmgMounts,
  detectHostTauriBuildPlatform,
  isRunOwnedIntermediateDmg,
  parseHdiutilImages,
  parseTauriArtifactCliArgs,
  resolveTauriBuildPlan,
  resolveTauriRustTarget,
  runBuildTauriDesktopArtifactMain,
  symbolBuildEnvironment,
  TAURI_BUILD_ATTEMPTS,
  TauriDesktopBuildConfigurationError,
  TauriDesktopBuildDirectoryMissingError,
  TauriDesktopBuildHostMismatchError,
  TauriDesktopBuildNoArtifactsProducedError,
  TauriDesktopBuildPublicationError,
  TauriDesktopBuildUnsafePathError,
} from "./build-desktop-artifact.ts";

const decodeUpdaterDescriptor = Schema.decodeUnknownSync(
  Schema.fromJsonString(
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
  ),
);

const processHandle = (exitCode: number, stdout = "") =>
  ChildProcessSpawner.makeHandle({
    pid: ChildProcessSpawner.ProcessId(7),
    exitCode: Effect.succeed(ChildProcessSpawner.ExitCode(exitCode)),
    isRunning: Effect.succeed(false),
    kill: () => Effect.void,
    unref: Effect.succeed(Effect.void),
    stdin: Sink.drain,
    stdout: stdout.length === 0 ? Stream.empty : Stream.make(new TextEncoder().encode(stdout)),
    stderr: Stream.empty,
    all: Stream.empty,
    getInputFd: () => Sink.drain,
    getOutputFd: () => Stream.empty,
  });

const hdiutilInfoPlist = (images: ReadonlyArray<{ path: string; device: string }>) =>
  `<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>framework</key><string>480.60.1</string><key>images</key><array>${images
    .map(
      (image) =>
        `<dict><key>blockcount</key><integer>1</integer><key>hdid-pid</key><integer>1</integer><key>image-alias</key><data>AA==</data><key>image-path</key><string>${image.path}</string><key>image-type</key><string>UDIF read/write image</string><key>system-entities</key><array><dict><key>content-hint</key><string>GUID_partition_scheme</string><key>dev-entry</key><string>${image.device}</string></dict><dict><key>content-hint</key><string>Apple_APFS</string><key>dev-entry</key><string>${image.device}s1</string><key>mount-point</key><string>/private/tmp/dmg.random</string></dict></array></dict>`,
    )
    .join("")}</array><key>revision</key><string>10.13</string></dict></plist>
`;

it.layer(NodeServices.layer)("build-desktop-artifact", (it) => {
  it("detects the supported Tauri build platform for the host OS", () => {
    assert.equal(detectHostTauriBuildPlatform("darwin"), "mac");
    assert.equal(detectHostTauriBuildPlatform("linux"), "linux");
    assert.equal(detectHostTauriBuildPlatform("win32"), "win");
    assert.equal(detectHostTauriBuildPlatform("freebsd"), undefined);
  });

  it("maps Tauri platform and architecture pairs to Rust target triples", () => {
    assert.equal(resolveTauriRustTarget("mac", "arm64"), "aarch64-apple-darwin");
    assert.equal(resolveTauriRustTarget("mac", "x64"), "x86_64-apple-darwin");
    assert.equal(resolveTauriRustTarget("linux", "arm64"), "aarch64-unknown-linux-gnu");
    assert.equal(resolveTauriRustTarget("linux", "x64"), "x86_64-unknown-linux-gnu");
    assert.equal(resolveTauriRustTarget("win", "arm64"), "aarch64-pc-windows-msvc");
    assert.equal(resolveTauriRustTarget("win", "x64"), "x86_64-pc-windows-msvc");
  });

  it.effect("plans a Windows NSIS build through the canonical desktop package", () =>
    Effect.gen(function* () {
      const path = yield* Path.Path;
      const repoRoot = path.resolve("X:/repo");
      const plan = yield* resolveTauriBuildPlan(
        {
          platform: "win",
          target: "nsis",
          arch: "x64",
          outputDir: "artifacts/tauri-win",
        },
        {},
        { platform: "win32", arch: "x64" },
        repoRoot,
      );

      assert.equal(plan.platform, "win");
      assert.equal(plan.target, "nsis");
      assert.equal(plan.bundleDirectoryName, "nsis");
      assert.equal(plan.arch, "x64");
      assert.equal(plan.rustTarget, "x86_64-pc-windows-msvc");
      assert.equal(plan.outputDir, path.join(repoRoot, "artifacts", "tauri-win"));
      assert.equal(
        plan.bundleDir,
        path.join(repoRoot, "target", "x86_64-pc-windows-msvc", "release", "bundle", "nsis"),
      );
      // Compiling and bundling are separate Tauri runs, so release builds can dump symbols and
      // strip the binary in between.
      assert.deepStrictEqual(plan.buildCommand, {
        command: "vp",
        cwd: repoRoot,
        args: [
          "run",
          "--filter",
          "@bibcode/desktop",
          "build",
          "--no-bundle",
          "--target",
          "x86_64-pc-windows-msvc",
        ],
      });
      assert.deepStrictEqual(plan.bundleCommand, {
        command: "vp",
        cwd: repoRoot,
        args: [
          "run",
          "--filter",
          "@bibcode/desktop",
          "bundle",
          "--bundles",
          "nsis",
          "--target",
          "x86_64-pc-windows-msvc",
        ],
      });
      assert.isUndefined(plan.symbols);
    }),
  );

  it.effect("plans an updater-enabled macOS DMG build with the stable release overlay", () =>
    Effect.gen(function* () {
      const path = yield* Path.Path;
      const repoRoot = path.resolve("X:/repo");
      const plan = yield* resolveTauriBuildPlan(
        { platform: "mac", target: "dmg", arch: "arm64", updater: true },
        {},
        { platform: "darwin", arch: "arm64" },
        repoRoot,
      );

      assert.equal(plan.updaterManifestTarget, "darwin-aarch64");
      assert.equal(
        plan.updaterBundleDir,
        path.join(repoRoot, "target", "aarch64-apple-darwin", "release", "bundle", "macos"),
      );
      // The overlay is compiled into the app and also enables the updater bundles.
      assert.deepEqual(plan.buildCommand.args, [
        "run",
        "--filter",
        "@bibcode/desktop",
        "build",
        "--no-bundle",
        "--config",
        path.join(repoRoot, "apps/desktop/src-tauri/tauri.release.conf.json"),
        "--target",
        "aarch64-apple-darwin",
      ]);
      assert.deepEqual(plan.bundleCommand.args, [
        "run",
        "--filter",
        "@bibcode/desktop",
        "bundle",
        "--config",
        path.join(repoRoot, "apps/desktop/src-tauri/tauri.release.conf.json"),
        "--bundles",
        "app,dmg",
        "--target",
        "aarch64-apple-darwin",
        "--verbose",
      ]);
    }),
  );

  it.effect("plans updater-enabled Linux and Windows ARM64 builds", () =>
    Effect.gen(function* () {
      const path = yield* Path.Path;
      const repoRoot = path.resolve("X:/repo");
      const linux = yield* resolveTauriBuildPlan(
        { platform: "linux", target: "appimage", arch: "arm64", updater: true },
        {},
        { platform: "linux", arch: "arm64" },
        repoRoot,
      );
      const windows = yield* resolveTauriBuildPlan(
        { platform: "win", target: "nsis", arch: "arm64", updater: true },
        {},
        { platform: "win32", arch: "arm64" },
        repoRoot,
      );

      assert.equal(linux.updaterManifestTarget, "linux-aarch64");
      assert.equal(linux.rustTarget, "aarch64-unknown-linux-gnu");
      assert.equal(windows.updaterManifestTarget, "windows-aarch64");
      assert.equal(windows.rustTarget, "aarch64-pc-windows-msvc");
    }),
  );

  it.effect("keeps ordinary macOS DMG builds scoped to the DMG bundle", () =>
    Effect.gen(function* () {
      const path = yield* Path.Path;
      const repoRoot = path.resolve("X:/repo");
      const plan = yield* resolveTauriBuildPlan(
        { platform: "mac", target: "dmg", arch: "x64" },
        {},
        { platform: "darwin", arch: "x64" },
        repoRoot,
      );

      assert.isUndefined(plan.updaterBundleDir);
      assert.deepEqual(plan.bundleCommand.args, [
        "run",
        "--filter",
        "@bibcode/desktop",
        "bundle",
        "--bundles",
        "dmg",
        "--target",
        "x86_64-apple-darwin",
        "--verbose",
      ]);
    }),
  );

  it.effect("uses Tauri-specific environment defaults", () =>
    Effect.gen(function* () {
      const path = yield* Path.Path;
      const repoRoot = path.resolve("X:/repo");
      const plan = yield* resolveTauriBuildPlan(
        {},
        {
          BIBCODE_TAURI_DESKTOP_PLATFORM: "linux",
          BIBCODE_TAURI_DESKTOP_TARGET: "deb",
          BIBCODE_TAURI_DESKTOP_ARCH: "arm64",
          BIBCODE_TAURI_DESKTOP_OUTPUT_DIR: "release/custom-tauri",
          BIBCODE_TAURI_DESKTOP_SKIP_BUILD: "1",
          BIBCODE_TAURI_DESKTOP_VERBOSE: "true",
        },
        { platform: "linux", arch: "x64" },
        repoRoot,
      );

      assert.equal(plan.platform, "linux");
      assert.equal(plan.target, "deb");
      assert.equal(plan.arch, "arm64");
      assert.equal(plan.rustTarget, "aarch64-unknown-linux-gnu");
      assert.equal(plan.skipBuild, true);
      assert.equal(plan.verbose, true);
      assert.equal(plan.outputDir, path.join(repoRoot, "release", "custom-tauri"));
    }),
  );

  it.effect("rejects cross-platform builds unless explicitly allowed", () =>
    Effect.gen(function* () {
      const path = yield* Path.Path;
      const error = yield* resolveTauriBuildPlan(
        { platform: "mac", arch: "arm64" },
        {},
        { platform: "win32", arch: "x64" },
        path.resolve("X:/repo"),
      ).pipe(Effect.flip);

      assert.instanceOf(error, TauriDesktopBuildHostMismatchError);
    }),
  );

  it("parses CLI flags into a typed Tauri artifact input", () => {
    assert.deepStrictEqual(
      parseTauriArtifactCliArgs([
        "--platform",
        "win",
        "--target",
        "msi",
        "--arch",
        "arm64",
        "--output-dir",
        "release/tauri",
        "--skip-build",
        "--verbose",
        "--allow-cross-platform",
        "--updater",
        "--symbols",
      ]),
      {
        platform: "win",
        target: "msi",
        arch: "arm64",
        outputDir: "release/tauri",
        skipBuild: true,
        verbose: true,
        allowCrossPlatform: true,
        updater: true,
        symbols: true,
      },
    );
    assert.deepStrictEqual(parseTauriArtifactCliArgs([]), {});
  });

  it.effect("resolves repository and architecture defaults from the host", () =>
    Effect.gen(function* () {
      const path = yield* Path.Path;
      const repoRoot = yield* path.fromFileUrl(new URL("..", import.meta.url));
      const plan = yield* resolveTauriBuildPlan(
        { skipBuild: true },
        { UNUSED: undefined, BIBCODE_TAURI_DESKTOP_VERBOSE: "no" },
        { platform: "linux", arch: "arm64" },
      );

      assert.equal(plan.platform, "linux");
      assert.equal(plan.arch, "arm64");
      assert.equal(plan.target, "appimage");
      assert.equal(plan.bundleDirectoryName, "appimage");
      assert.equal(plan.verbose, false);
      assert.equal(plan.outputDir, path.join(repoRoot, "release", "desktop", "linux-arm64"));
    }),
  );

  it.effect("uses host services and maps macOS app bundles to the macos directory", () =>
    Effect.gen(function* () {
      const path = yield* Path.Path;
      const repoRoot = path.resolve("X:/repo");
      const plan = yield* resolveTauriBuildPlan(
        { platform: "mac", target: "app", allowCrossPlatform: true },
        {},
        undefined,
        repoRoot,
      ).pipe(
        Effect.provideService(HostProcessPlatform, "win32"),
        Effect.provideService(HostProcessArchitecture, "x64"),
      );

      assert.equal(plan.arch, "x64");
      assert.equal(plan.bundleDirectoryName, "macos");
      assert.include(plan.bundleDir, path.join("bundle", "macos"));
    }),
  );

  it.effect("copies Tauri bundle outputs into the artifact directory", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tempDir = yield* fs.makeTempDirectoryScoped({
        prefix: "bibcode-tauri-artifact-",
      });
      const bundleDir = path.join(tempDir, "bundle");
      const outputDir = path.join(tempDir, "out");
      yield* fs.makeDirectory(path.join(bundleDir, "nested"), { recursive: true });
      yield* fs.writeFileString(path.join(bundleDir, "BiBCode_0.2.996_x64-setup.exe"), "installer");
      yield* fs.writeFileString(path.join(bundleDir, "nested", "manifest.json"), "{}");

      const artifacts = yield* copyTauriBundleArtifacts({ bundleDir, outputDir });

      assert.deepStrictEqual(
        artifacts.toSorted(),
        [
          path.join(outputDir, "BiBCode_0.2.996_x64-setup.exe"),
          path.join(outputDir, "nested"),
        ].toSorted(),
      );
      assert.equal(
        yield* fs.readFileString(path.join(outputDir, "BiBCode_0.2.996_x64-setup.exe")),
        "installer",
      );
      assert.equal(yield* fs.readFileString(path.join(outputDir, "nested", "manifest.json")), "{}");
    }),
  );

  it.effect("normalizes leading and trailing periods without changing internal periods", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tempDir = yield* fs.makeTempDirectoryScoped({
        prefix: "bibcode-release-boundary-periods-",
      });
      const bundleDir = path.join(tempDir, "bundle");
      const outputDir = path.join(tempDir, "output");
      yield* fs.makeDirectory(bundleDir);
      yield* fs.writeFileString(path.join(bundleDir, ".BiBCode_0.2.996_aarch64.dmg"), "leading");
      yield* fs.writeFileString(path.join(bundleDir, "BiBCode_0.2.996_x64-setup.exe."), "trailing");

      const artifacts = yield* copyTauriBundleArtifacts({ bundleDir, outputDir });

      assert.deepStrictEqual(
        artifacts.toSorted(),
        [
          path.join(outputDir, "BiBCode_0.2.996_aarch64.dmg"),
          path.join(outputDir, "BiBCode_0.2.996_x64-setup.exe"),
        ].toSorted(),
      );
      for (const entry of yield* fs.readDirectory(outputDir)) {
        if (entry === ".bibcode-publication-owner") continue;
        assert.match(entry, /^[A-Za-z0-9_-](?:[A-Za-z0-9._-]*[A-Za-z0-9_-])?$/);
      }
    }),
  );

  it.effect("publishes a macOS updater archive and descriptor beside the DMG", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tempDir = yield* fs.makeTempDirectoryScoped({ prefix: "bibcode-macos-updater-" });
      const bundleDir = path.join(tempDir, "bundle", "dmg");
      const updaterBundleDir = path.join(tempDir, "bundle", "macos");
      const outputDir = path.join(tempDir, "out");
      yield* fs.makeDirectory(bundleDir, { recursive: true });
      yield* fs.makeDirectory(updaterBundleDir, { recursive: true });
      yield* fs.writeFileString(path.join(bundleDir, "BiBCode_0.2.996_aarch64.dmg"), "dmg");
      yield* fs.writeFileString(path.join(updaterBundleDir, "BiBCode.app.tar.gz"), "archive");
      yield* fs.writeFileString(path.join(updaterBundleDir, "BiBCode.app.tar.gz.sig"), "signature");

      yield* copyTauriBundleArtifacts({
        bundleDir,
        updaterBundleDir,
        updaterManifestTarget: "darwin-aarch64",
        outputDir,
      });

      assert.deepEqual(
        decodeUpdaterDescriptor(
          yield* fs.readFileString(path.join(outputDir, "updater-darwin-aarch64.json")),
        ),
        {
          target: "darwin-aarch64",
          artifact: "bibcode-update-darwin-aarch64.app.tar.gz",
          signature: "bibcode-update-darwin-aarch64.app.tar.gz.sig",
        },
      );
      assert.equal(
        yield* fs.exists(path.join(outputDir, "bibcode-update-darwin-aarch64.app.tar.gz")),
        true,
      );
      assert.equal(
        yield* fs.readFileString(path.join(outputDir, "BiBCode_0.2.996_aarch64.dmg")),
        "dmg",
      );
      assert.equal(yield* fs.exists(path.join(outputDir, "BiBCode.app.tar.gz")), false);
      for (const entry of yield* fs.readDirectory(outputDir)) {
        if (entry === ".bibcode-publication-owner") continue;
        const info = yield* fs.stat(path.join(outputDir, entry));
        if (info.type === "File") {
          assert.match(entry, /^[A-Za-z0-9_-](?:[A-Za-z0-9._-]*[A-Za-z0-9_-])?$/);
        }
      }
    }),
  );

  it.effect("publishes a Linux AppImage and adjacent updater signature", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tempDir = yield* fs.makeTempDirectoryScoped({ prefix: "bibcode-linux-updater-" });
      const bundleDir = path.join(tempDir, "bundle", "appimage");
      const outputDir = path.join(tempDir, "out");
      const artifact = ".BiBCode_0.2.996_amd64.AppImage";
      const publishedArtifact = "BiBCode_0.2.996_amd64.AppImage";
      const appDir = path.join(bundleDir, "BiBCode.AppDir");
      yield* fs.makeDirectory(path.join(appDir, "usr", "bin"), { recursive: true });
      yield* fs.writeFileString(path.join(appDir, "AppRun"), "launcher");
      yield* fs.writeFileString(path.join(appDir, "usr.desktop"), "desktop entry");
      yield* fs.writeFileString(path.join(appDir, "usr", "bin", "bibcode-desktop"), "binary");
      yield* fs.writeFileString(path.join(bundleDir, artifact), "appimage");
      yield* fs.writeFileString(path.join(bundleDir, `${artifact}.sig`), "signature");

      const artifacts = yield* copyTauriBundleArtifacts({
        bundleDir,
        updaterManifestTarget: "linux-x86_64",
        outputDir,
      });

      assert.deepEqual(
        artifacts.toSorted(),
        [
          path.join(outputDir, publishedArtifact),
          path.join(outputDir, `${publishedArtifact}.sig`),
          path.join(outputDir, "BiBCode.AppDir"),
          path.join(outputDir, "updater-linux-x86_64.json"),
        ].toSorted(),
      );
      assert.deepEqual(
        decodeUpdaterDescriptor(
          yield* fs.readFileString(path.join(outputDir, "updater-linux-x86_64.json")),
        ),
        {
          target: "linux-x86_64",
          artifact: publishedArtifact,
          signature: `${publishedArtifact}.sig`,
        },
      );
      assert.equal(yield* fs.readFileString(path.join(outputDir, publishedArtifact)), "appimage");
      assert.equal(
        yield* fs.readFileString(path.join(outputDir, `${publishedArtifact}.sig`)),
        "signature",
      );
      assert.equal(
        yield* fs.readFileString(
          path.join(outputDir, "BiBCode.AppDir", "usr", "bin", "bibcode-desktop"),
        ),
        "binary",
      );
      for (const entry of yield* fs.readDirectory(outputDir)) {
        if (entry === ".bibcode-publication-owner") continue;
        const info = yield* fs.stat(path.join(outputDir, entry));
        if (info.type === "File") {
          assert.match(entry, /^[A-Za-z0-9_-](?:[A-Za-z0-9._-]*[A-Za-z0-9_-])?$/);
        }
      }
    }),
  );

  it.effect("publishes a Windows updater with release-safe payload and signature names", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tempDir = yield* fs.makeTempDirectoryScoped({ prefix: "bibcode-windows-updater-" });
      const bundleDir = path.join(tempDir, "bundle", "nsis");
      const outputDir = path.join(tempDir, "out");
      const artifact = "BiBCode_0.2.996_x64-setup.exe";
      const publishedArtifact = "BiBCode_0.2.996_x64-setup.exe";
      yield* fs.makeDirectory(bundleDir, { recursive: true });
      yield* fs.writeFileString(path.join(bundleDir, artifact), "installer");
      yield* fs.writeFileString(path.join(bundleDir, `${artifact}.sig`), "signature");

      const artifacts = yield* copyTauriBundleArtifacts({
        bundleDir,
        updaterManifestTarget: "windows-x86_64",
        outputDir,
      });

      assert.deepEqual(
        artifacts.toSorted(),
        [
          path.join(outputDir, publishedArtifact),
          path.join(outputDir, `${publishedArtifact}.sig`),
          path.join(outputDir, "updater-windows-x86_64.json"),
        ].toSorted(),
      );
      assert.deepEqual(
        decodeUpdaterDescriptor(
          yield* fs.readFileString(path.join(outputDir, "updater-windows-x86_64.json")),
        ),
        {
          target: "windows-x86_64",
          artifact: publishedArtifact,
          signature: `${publishedArtifact}.sig`,
        },
      );
      for (const entry of yield* fs.readDirectory(outputDir)) {
        if (entry === ".bibcode-publication-owner") continue;
        const info = yield* fs.stat(path.join(outputDir, entry));
        if (info.type === "File") {
          assert.match(entry, /^[A-Za-z0-9_-](?:[A-Za-z0-9._-]*[A-Za-z0-9_-])?$/);
        }
      }
    }),
  );

  it.effect("rejects release basename normalization collisions before staging", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tempDir = yield* fs.makeTempDirectoryScoped({
        prefix: "bibcode-release-basename-collision-",
      });
      const bundleDir = path.join(tempDir, "bundle");
      const outputDir = path.join(tempDir, "output");
      const stageDir = path.join(tempDir, ".output.bibcode-normalization-collision.stage");
      let stagingAttempted = false;
      yield* fs.makeDirectory(bundleDir);
      yield* fs.writeFileString(path.join(bundleDir, "BiBCode (Dev).dmg"), "first");
      yield* fs.writeFileString(path.join(bundleDir, "BiBCode [Dev].dmg"), "second");

      const error = yield* copyTauriBundleArtifacts(
        { bundleDir, outputDir },
        {
          transactionId: () => "normalization-collision",
          makeDirectory: (target, options) => {
            if (target === stageDir) stagingAttempted = true;
            return fs.makeDirectory(target, options);
          },
        },
      ).pipe(Effect.flip);

      assert.instanceOf(error, TauriDesktopBuildPublicationError);
      assert.include(String(error.cause), "Duplicate artifact basename BiBCode_Dev_.dmg.");
      assert.isFalse(stagingAttempted);
      assert.isFalse(yield* fs.exists(outputDir));
    }),
  );

  it.effect("rejects a basename with no meaningful release-safe characters before staging", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tempDir = yield* fs.makeTempDirectoryScoped({
        prefix: "bibcode-release-meaningless-basename-",
      });
      const bundleDir = path.join(tempDir, "bundle");
      const outputDir = path.join(tempDir, "output");
      const stageDir = path.join(tempDir, ".output.bibcode-meaningless-basename.stage");
      let stagingAttempted = false;
      yield* fs.makeDirectory(bundleDir);
      yield* fs.writeFileString(path.join(bundleDir, "((("), "artifact");

      const error = yield* copyTauriBundleArtifacts(
        { bundleDir, outputDir },
        {
          transactionId: () => "meaningless-basename",
          makeDirectory: (target, options) => {
            if (target === stageDir) stagingAttempted = true;
            return fs.makeDirectory(target, options);
          },
        },
      ).pipe(Effect.flip);

      assert.instanceOf(error, TauriDesktopBuildPublicationError);
      assert.include(String(error.cause), "has no meaningful release-safe name");
      assert.isFalse(stagingAttempted);
      assert.isFalse(yield* fs.exists(outputDir));
    }),
  );

  it.effect("rejects boundary-period normalization collisions before staging", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tempDir = yield* fs.makeTempDirectoryScoped({
        prefix: "bibcode-release-boundary-collision-",
      });
      const bundleDir = path.join(tempDir, "bundle");
      const outputDir = path.join(tempDir, "output");
      const stageDir = path.join(tempDir, ".output.bibcode-boundary-collision.stage");
      let stagingAttempted = false;
      yield* fs.makeDirectory(bundleDir);
      yield* fs.writeFileString(path.join(bundleDir, ".BiBCode.dmg"), "hidden");
      yield* fs.writeFileString(path.join(bundleDir, "BiBCode.dmg"), "visible");

      const error = yield* copyTauriBundleArtifacts(
        { bundleDir, outputDir },
        {
          transactionId: () => "boundary-collision",
          makeDirectory: (target, options) => {
            if (target === stageDir) stagingAttempted = true;
            return fs.makeDirectory(target, options);
          },
        },
      ).pipe(Effect.flip);

      assert.instanceOf(error, TauriDesktopBuildPublicationError);
      assert.include(String(error.cause), "Duplicate artifact basename BiBCode.dmg.");
      assert.isFalse(stagingAttempted);
      assert.isFalse(yield* fs.exists(outputDir));
    }),
  );

  it.effect("rejects a primary artifact that collides with the updater descriptor", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tempDir = yield* fs.makeTempDirectoryScoped({
        prefix: "bibcode-updater-descriptor-collision-",
      });
      const bundleDir = path.join(tempDir, "bundle", "dmg");
      const updaterBundleDir = path.join(tempDir, "bundle", "macos");
      const outputDir = path.join(tempDir, "out");
      yield* fs.makeDirectory(bundleDir, { recursive: true });
      yield* fs.makeDirectory(updaterBundleDir, { recursive: true });
      yield* fs.writeFileString(path.join(bundleDir, "BiBCode.dmg"), "dmg");
      yield* fs.writeFileString(path.join(bundleDir, "updater-darwin-aarch64.json"), "artifact");
      yield* fs.writeFileString(path.join(updaterBundleDir, "BiBCode.app.tar.gz"), "archive");
      yield* fs.writeFileString(path.join(updaterBundleDir, "BiBCode.app.tar.gz.sig"), "signature");

      const error = yield* copyTauriBundleArtifacts({
        bundleDir,
        updaterBundleDir,
        updaterManifestTarget: "darwin-aarch64",
        outputDir,
      }).pipe(Effect.flip);

      assert.instanceOf(error, TauriDesktopBuildPublicationError);
      assert.isFalse(yield* fs.exists(outputDir));
    }),
  );

  it.effect("rejects invalid macOS updater payload layouts before publication", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const cases: ReadonlyArray<{
        readonly name: string;
        readonly files: ReadonlyArray<string>;
        readonly duplicatePrimary?: string;
      }> = [
        { name: "missing signature", files: ["BiBCode.app.tar.gz"] },
        {
          name: "multiple signatures",
          files: [
            "BiBCode.app.tar.gz",
            "BiBCode.app.tar.gz.sig",
            "Other.app.tar.gz",
            "Other.app.tar.gz.sig",
          ],
        },
        { name: "signature without payload", files: ["BiBCode.app.tar.gz.sig"] },
        {
          name: "duplicate basename",
          files: ["BiBCode.app.tar.gz", "BiBCode.app.tar.gz.sig"],
          duplicatePrimary: "BiBCode.app.tar.gz",
        },
      ] as const;

      for (const testCase of cases) {
        const tempDir = yield* fs.makeTempDirectoryScoped({ prefix: "bibcode-invalid-updater-" });
        const bundleDir = path.join(tempDir, "bundle", "dmg");
        const updaterBundleDir = path.join(tempDir, "bundle", "macos");
        const outputDir = path.join(tempDir, "out");
        yield* fs.makeDirectory(bundleDir, { recursive: true });
        yield* fs.makeDirectory(updaterBundleDir, { recursive: true });
        yield* fs.writeFileString(path.join(bundleDir, "BiBCode.dmg"), "dmg");
        if (testCase.duplicatePrimary) {
          yield* fs.writeFileString(path.join(bundleDir, testCase.duplicatePrimary), "duplicate");
        }
        for (const file of testCase.files) {
          yield* fs.writeFileString(path.join(updaterBundleDir, file), file);
        }

        const error = yield* copyTauriBundleArtifacts({
          bundleDir,
          updaterBundleDir,
          updaterManifestTarget: "darwin-aarch64",
          outputDir,
        }).pipe(Effect.flip);
        assert.instanceOf(error, TauriDesktopBuildPublicationError, testCase.name);
        assert.isFalse(yield* fs.exists(outputDir), testCase.name);
      }
    }),
  );

  it.effect("reports a missing Tauri bundle directory with structural context", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tempDir = yield* fs.makeTempDirectoryScoped({
        prefix: "bibcode-missing-tauri-bundle-",
      });
      const bundleDir = path.join(tempDir, "missing");
      const error = yield* copyTauriBundleArtifacts({
        bundleDir,
        outputDir: path.join(tempDir, "unused-output"),
      }).pipe(Effect.flip);

      assert.instanceOf(error, TauriDesktopBuildDirectoryMissingError);
      assert.equal(error.bundleDir, bundleDir);
    }),
  );

  it.effect("rejects empty bundle directories", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tempDir = yield* fs.makeTempDirectoryScoped({ prefix: "bibcode-empty-tauri-bundle-" });
      const bundleDir = path.join(tempDir, "bundle");
      yield* fs.makeDirectory(bundleDir);

      const error = yield* copyTauriBundleArtifacts({
        bundleDir,
        outputDir: path.join(tempDir, "output"),
      }).pipe(Effect.flip);

      assert.instanceOf(error, TauriDesktopBuildNoArtifactsProducedError);
      assert.equal(error.bundleDir, bundleDir);
    }),
  );

  it.effect("overwrites existing artifact entries", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tempDir = yield* fs.makeTempDirectoryScoped({
        prefix: "bibcode-overwrite-tauri-bundle-",
      });
      const bundleDir = path.join(tempDir, "bundle");
      const outputDir = path.join(tempDir, "output");
      yield* fs.makeDirectory(bundleDir);
      yield* fs.makeDirectory(outputDir);
      yield* fs.writeFileString(path.join(bundleDir, "artifact.txt"), "new");
      yield* fs.writeFileString(path.join(outputDir, "artifact.txt"), "old");

      yield* copyTauriBundleArtifacts({ bundleDir, outputDir });

      assert.equal(yield* fs.readFileString(path.join(outputDir, "artifact.txt")), "new");
    }),
  );

  it.effect("rejects identical and overlapping source/output publication paths", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tempDir = yield* fs.makeTempDirectoryScoped({ prefix: "bibcode-unsafe-publication-" });
      const bundleDir = path.join(tempDir, "bundle");
      yield* fs.makeDirectory(path.join(bundleDir, "nested"), { recursive: true });
      yield* fs.writeFileString(path.join(bundleDir, "artifact.txt"), "artifact");

      for (const outputDir of [bundleDir, path.join(bundleDir, "nested"), tempDir]) {
        const error = yield* copyTauriBundleArtifacts({ bundleDir, outputDir }).pipe(Effect.flip);
        assert.instanceOf(error, TauriDesktopBuildUnsafePathError);
        assert.equal(error.bundleDir, bundleDir);
        assert.equal(error.outputDir, outputDir);
      }

      assert.equal(yield* fs.readFileString(path.join(bundleDir, "artifact.txt")), "artifact");
    }),
  );

  it.effect("keeps prior output and cleans staging when copying or validation fails", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tempDir = yield* fs.makeTempDirectoryScoped({
        prefix: "bibcode-publication-failures-",
      });
      const bundleDir = path.join(tempDir, "bundle");
      const outputDir = path.join(tempDir, "output");
      yield* fs.makeDirectory(bundleDir);
      yield* fs.makeDirectory(outputDir);
      yield* fs.writeFileString(path.join(bundleDir, "artifact.txt"), "new");
      yield* fs.writeFileString(path.join(outputDir, "artifact.txt"), "old");

      const copyCause = PlatformError.systemError({
        _tag: "PermissionDenied",
        module: "FileSystem",
        method: "copy",
        pathOrDescriptor: bundleDir,
      });
      const copyError = yield* copyTauriBundleArtifacts(
        { bundleDir, outputDir },
        {
          transactionId: () => "copy-failure",
          copy: () => Effect.fail(copyCause),
        },
      ).pipe(Effect.flip);
      assert.instanceOf(copyError, TauriDesktopBuildPublicationError);
      assert.equal(copyError.operation, "copy");
      assert.strictEqual(copyError.cause, copyCause);
      assert.equal(yield* fs.readFileString(path.join(outputDir, "artifact.txt")), "old");
      assert.isFalse(yield* fs.exists(path.join(tempDir, ".output.bibcode-copy-failure.stage")));

      const validationError = yield* copyTauriBundleArtifacts(
        { bundleDir, outputDir },
        {
          transactionId: () => "validation-failure",
          stat: (target) =>
            fs
              .stat(target)
              .pipe(
                Effect.map((info) =>
                  target.includes(".bibcode-validation-failure.stage") && info.type === "File"
                    ? { ...info, size: FileSystem.Size(Number(info.size) + 1) }
                    : info,
                ),
              ),
        },
      ).pipe(Effect.flip);
      assert.instanceOf(validationError, TauriDesktopBuildPublicationError);
      assert.equal(validationError.operation, "validate-staging");
      assert.equal(yield* fs.readFileString(path.join(outputDir, "artifact.txt")), "old");
      assert.isFalse(
        yield* fs.exists(path.join(tempDir, ".output.bibcode-validation-failure.stage")),
      );

      const checksumError = yield* copyTauriBundleArtifacts(
        { bundleDir, outputDir },
        {
          transactionId: () => "checksum-failure",
          stream: (target, options) =>
            target.includes(".bibcode-checksum-failure.stage")
              ? Stream.make(new TextEncoder().encode("bad"))
              : fs.stream(target, options),
        },
      ).pipe(Effect.flip);
      assert.instanceOf(checksumError, TauriDesktopBuildPublicationError);
      assert.equal(checksumError.operation, "validate-staging");
      assert.equal(yield* fs.readFileString(path.join(outputDir, "artifact.txt")), "old");
      assert.isFalse(
        yield* fs.exists(path.join(tempDir, ".output.bibcode-checksum-failure.stage")),
      );
    }),
  );

  it.effect("rolls back the prior output when the atomic publication swap fails", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tempDir = yield* fs.makeTempDirectoryScoped({
        prefix: "bibcode-publication-rollback-",
      });
      const bundleDir = path.join(tempDir, "bundle");
      const outputDir = path.join(tempDir, "output");
      const stageDir = path.join(tempDir, ".output.bibcode-swap-failure.stage");
      const backupDir = path.join(tempDir, ".output.bibcode-swap-failure.backup");
      yield* fs.makeDirectory(bundleDir);
      yield* fs.makeDirectory(outputDir);
      yield* fs.writeFileString(path.join(bundleDir, "artifact.txt"), "new");
      yield* fs.writeFileString(path.join(outputDir, "artifact.txt"), "old");

      const swapCause = PlatformError.systemError({
        _tag: "PermissionDenied",
        module: "FileSystem",
        method: "rename",
        pathOrDescriptor: stageDir,
      });
      const moves: string[] = [];
      const error = yield* copyTauriBundleArtifacts(
        { bundleDir, outputDir },
        {
          transactionId: () => "swap-failure",
          move: (source, target) => {
            moves.push(`${source} -> ${target}`);
            return source === stageDir && target === outputDir
              ? Effect.fail(swapCause)
              : fs.rename(source, target);
          },
        },
      ).pipe(Effect.flip);

      assert.instanceOf(error, TauriDesktopBuildPublicationError);
      assert.equal(error.operation, "swap");
      assert.strictEqual(error.cause, swapCause);
      assert.deepStrictEqual(error.rollbackFailures, []);
      assert.deepStrictEqual(error.recoveryPaths, []);
      assert.notInclude(error.message, "Recovery artifacts retained");
      assert.include(moves, `${outputDir} -> ${backupDir}`);
      assert.include(moves, `${backupDir} -> ${outputDir}`);
      assert.equal(yield* fs.readFileString(path.join(outputDir, "artifact.txt")), "old");
      assert.isFalse(yield* fs.exists(stageDir));
      assert.isFalse(yield* fs.exists(backupDir));
    }),
  );

  it.effect("removes a first publication when failure occurs after the output swap", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tempDir = yield* fs.makeTempDirectoryScoped({
        prefix: "bibcode-first-publish-failure-",
      });
      const bundleDir = path.join(tempDir, "bundle");
      const outputDir = path.join(tempDir, "output");
      const stageDir = path.join(tempDir, ".output.bibcode-first-failure.stage");
      const backupDir = path.join(tempDir, ".output.bibcode-first-failure.backup");
      yield* fs.makeDirectory(bundleDir);
      yield* fs.writeFileString(path.join(bundleDir, "artifact.txt"), "new");
      const cause = PlatformError.systemError({
        _tag: "Unknown",
        module: "FileSystem",
        method: "post-swap",
        pathOrDescriptor: outputDir,
      });

      const error = yield* copyTauriBundleArtifacts(
        { bundleDir, outputDir },
        {
          transactionId: () => "first-failure",
          move: (source, target) =>
            source === stageDir && target === outputDir
              ? fs.rename(source, target).pipe(Effect.flatMap(() => Effect.fail(cause)))
              : fs.rename(source, target),
        },
      ).pipe(Effect.flip);

      assert.instanceOf(error, TauriDesktopBuildPublicationError);
      assert.isFalse(yield* fs.exists(outputDir));
      assert.isFalse(yield* fs.exists(stageDir));
      assert.isFalse(yield* fs.exists(backupDir));
    }),
  );

  it.effect("removes a first publication when interrupted after the output swap", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tempDir = yield* fs.makeTempDirectoryScoped({
        prefix: "bibcode-first-publish-interrupt-",
      });
      const bundleDir = path.join(tempDir, "bundle");
      const outputDir = path.join(tempDir, "output");
      const stageDir = path.join(tempDir, ".output.bibcode-first-interrupt.stage");
      const backupDir = path.join(tempDir, ".output.bibcode-first-interrupt.backup");
      const swapped = yield* Deferred.make<void>();
      yield* fs.makeDirectory(bundleDir);
      yield* fs.writeFileString(path.join(bundleDir, "artifact.txt"), "new");

      const publication = yield* copyTauriBundleArtifacts(
        { bundleDir, outputDir },
        {
          transactionId: () => "first-interrupt",
          move: (source, target) =>
            source === stageDir && target === outputDir
              ? fs.rename(source, target).pipe(
                  Effect.tap(() => Deferred.succeed(swapped, undefined)),
                  Effect.andThen(Effect.never),
                )
              : fs.rename(source, target),
        },
      ).pipe(Effect.forkChild({ startImmediately: true }));

      yield* Deferred.await(swapped);
      assert.isTrue(yield* fs.exists(outputDir));
      yield* Fiber.interrupt(publication);

      assert.isFalse(yield* fs.exists(outputDir));
      assert.isFalse(yield* fs.exists(stageDir));
      assert.isFalse(yield* fs.exists(backupDir));
    }),
  );

  it.effect("does not delete a competitor that replaces output after ownership is read", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tempDir = yield* fs.makeTempDirectoryScoped({ prefix: "bibcode-owner-read-race-" });
      const bundleDir = path.join(tempDir, "bundle");
      const outputDir = path.join(tempDir, "output");
      const stageDir = path.join(tempDir, ".output.bibcode-owner-read-race.stage");
      const capturedDir = path.join(tempDir, "captured-publication");
      const ownerFile = ".bibcode-publication-owner";
      const outputOwner = path.join(outputDir, ownerFile);
      const stageOwner = path.join(stageDir, ownerFile);
      const quarantineOwner = path.join(
        tempDir,
        ".output.bibcode-owner-read-race.quarantine",
        ownerFile,
      );
      let ownershipReads = 0;
      yield* fs.makeDirectory(bundleDir);
      yield* fs.writeFileString(path.join(bundleDir, "artifact.txt"), "ours");
      const postSwapCause = PlatformError.systemError({
        _tag: "Unknown",
        module: "FileSystem",
        method: "post-swap",
        pathOrDescriptor: outputDir,
      });

      yield* copyTauriBundleArtifacts(
        { bundleDir, outputDir },
        {
          transactionId: () => "owner-read-race",
          ownershipToken: () => "ours",
          move: (source, target) =>
            source === stageDir && target === outputDir
              ? fs.rename(source, target).pipe(Effect.flatMap(() => Effect.fail(postSwapCause)))
              : fs.rename(source, target),
          readFileString: (target) => {
            const read = fs.readFileString(target);
            if (target !== outputOwner && target !== stageOwner && target !== quarantineOwner)
              return read;
            return read.pipe(
              Effect.flatMap((owner) =>
                Effect.gen(function* () {
                  ownershipReads += 1;
                  if (target === outputOwner) {
                    yield* fs.rename(outputDir, capturedDir);
                  }
                  yield* fs.makeDirectory(outputDir);
                  yield* fs.writeFileString(path.join(outputDir, "artifact.txt"), "competitor");
                  yield* fs.writeFileString(path.join(outputDir, ownerFile), "competitor");
                  return owner;
                }),
              ),
            );
          },
        },
      ).pipe(Effect.flip);

      assert.equal(ownershipReads, 1);
      assert.equal(yield* fs.readFileString(path.join(outputDir, "artifact.txt")), "competitor");
      assert.equal(yield* fs.readFileString(path.join(outputDir, ownerFile)), "competitor");
      assert.isFalse(yield* fs.exists(stageDir));
    }),
  );

  it.effect("preserves a competing publication when the output swap loses the race", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tempDir = yield* fs.makeTempDirectoryScoped({
        prefix: "bibcode-competing-publish-fail-",
      });
      const bundleDir = path.join(tempDir, "bundle");
      const outputDir = path.join(tempDir, "output");
      const stageDir = path.join(tempDir, ".output.bibcode-race-failure.stage");
      const backupDir = path.join(tempDir, ".output.bibcode-race-failure.backup");
      yield* fs.makeDirectory(bundleDir);
      yield* fs.writeFileString(path.join(bundleDir, "artifact.txt"), "ours");
      const raceCause = PlatformError.systemError({
        _tag: "AlreadyExists",
        module: "FileSystem",
        method: "rename",
        pathOrDescriptor: outputDir,
      });

      const error = yield* copyTauriBundleArtifacts(
        { bundleDir, outputDir },
        {
          transactionId: () => "race-failure",
          ownershipToken: () => "ours",
          move: (source, target) =>
            source === stageDir && target === outputDir
              ? Effect.gen(function* () {
                  yield* fs.makeDirectory(outputDir);
                  yield* fs.writeFileString(path.join(outputDir, "artifact.txt"), "competitor");
                  yield* fs.writeFileString(
                    path.join(outputDir, ".bibcode-publication-owner"),
                    "competitor",
                  );
                  return yield* raceCause;
                })
              : fs.rename(source, target),
        },
      ).pipe(Effect.flip);

      assert.instanceOf(error, TauriDesktopBuildPublicationError);
      assert.equal(yield* fs.readFileString(path.join(outputDir, "artifact.txt")), "competitor");
      assert.equal(
        yield* fs.readFileString(path.join(outputDir, ".bibcode-publication-owner")),
        "competitor",
      );
      assert.isFalse(yield* fs.exists(stageDir));
      assert.isFalse(yield* fs.exists(backupDir));
    }),
  );

  it.effect("preserves a competing publication when its ownership marker is unavailable", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tempDir = yield* fs.makeTempDirectoryScoped({ prefix: "bibcode-competing-no-owner-" });
      const bundleDir = path.join(tempDir, "bundle");
      const outputDir = path.join(tempDir, "output");
      const stageDir = path.join(tempDir, ".output.bibcode-race-no-owner.stage");
      yield* fs.makeDirectory(bundleDir);
      yield* fs.writeFileString(path.join(bundleDir, "artifact.txt"), "ours");
      const raceCause = PlatformError.systemError({
        _tag: "AlreadyExists",
        module: "FileSystem",
        method: "rename",
        pathOrDescriptor: outputDir,
      });

      yield* copyTauriBundleArtifacts(
        { bundleDir, outputDir },
        {
          transactionId: () => "race-no-owner",
          ownershipToken: () => "ours",
          move: (source, target) =>
            source === stageDir && target === outputDir
              ? Effect.gen(function* () {
                  yield* fs.makeDirectory(outputDir);
                  yield* fs.writeFileString(path.join(outputDir, "artifact.txt"), "competitor");
                  return yield* raceCause;
                })
              : fs.rename(source, target),
        },
      ).pipe(Effect.flip);

      assert.equal(yield* fs.readFileString(path.join(outputDir, "artifact.txt")), "competitor");
      assert.isFalse(yield* fs.exists(stageDir));
    }),
  );

  it.effect("reports quarantined foreign output when a newer publication appears", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tempDir = yield* fs.makeTempDirectoryScoped({ prefix: "bibcode-foreign-quarantine-" });
      const bundleDir = path.join(tempDir, "bundle");
      const outputDir = path.join(tempDir, "output");
      const stageDir = path.join(tempDir, ".output.bibcode-foreign-quarantine.stage");
      const quarantineDir = path.join(tempDir, ".output.bibcode-foreign-quarantine.quarantine");
      const ownerFile = ".bibcode-publication-owner";
      yield* fs.makeDirectory(bundleDir);
      yield* fs.writeFileString(path.join(bundleDir, "artifact.txt"), "ours");
      const raceCause = PlatformError.systemError({
        _tag: "AlreadyExists",
        module: "FileSystem",
        method: "rename",
        pathOrDescriptor: outputDir,
      });

      const error = yield* copyTauriBundleArtifacts(
        { bundleDir, outputDir },
        {
          transactionId: () => "foreign-quarantine",
          ownershipToken: () => "ours",
          move: (source, target) =>
            source === stageDir && target === outputDir
              ? Effect.gen(function* () {
                  yield* fs.makeDirectory(outputDir);
                  yield* fs.writeFileString(path.join(outputDir, "artifact.txt"), "foreign");
                  yield* fs.writeFileString(path.join(outputDir, ownerFile), "foreign");
                  return yield* raceCause;
                })
              : fs.rename(source, target),
          readFileString: (target) =>
            target === path.join(quarantineDir, ownerFile)
              ? fs.readFileString(target).pipe(
                  Effect.flatMap((owner) =>
                    Effect.gen(function* () {
                      yield* fs.makeDirectory(outputDir);
                      yield* fs.writeFileString(path.join(outputDir, "artifact.txt"), "newer");
                      yield* fs.writeFileString(path.join(outputDir, ownerFile), "newer");
                      return owner;
                    }),
                  ),
                )
              : fs.readFileString(target),
        },
      ).pipe(Effect.flip);

      if (!(error instanceof TauriDesktopBuildPublicationError)) throw error;
      assert.equal(yield* fs.readFileString(path.join(outputDir, "artifact.txt")), "newer");
      assert.equal(yield* fs.readFileString(path.join(quarantineDir, "artifact.txt")), "foreign");
      assert.isFalse(yield* fs.exists(stageDir));
      assert.deepStrictEqual(error.recoveryPaths, [{ kind: "quarantine", path: quarantineDir }]);
      assert.include(error.message, quarantineDir);
    }),
  );

  it.effect("preserves a competing publication when interrupted during the losing swap", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tempDir = yield* fs.makeTempDirectoryScoped({
        prefix: "bibcode-competing-publish-stop-",
      });
      const bundleDir = path.join(tempDir, "bundle");
      const outputDir = path.join(tempDir, "output");
      const stageDir = path.join(tempDir, ".output.bibcode-race-interrupt.stage");
      const backupDir = path.join(tempDir, ".output.bibcode-race-interrupt.backup");
      const rollbackCheckingOutput = yield* Deferred.make<void>();
      const releaseRollbackCheck = yield* Deferred.make<void>();
      let outputExistsChecks = 0;
      yield* fs.makeDirectory(bundleDir);
      yield* fs.writeFileString(path.join(bundleDir, "artifact.txt"), "ours");
      const raceCause = PlatformError.systemError({
        _tag: "AlreadyExists",
        module: "FileSystem",
        method: "rename",
        pathOrDescriptor: outputDir,
      });

      const publication = yield* copyTauriBundleArtifacts(
        { bundleDir, outputDir },
        {
          transactionId: () => "race-interrupt",
          ownershipToken: () => "ours",
          exists: (target) => {
            if (target !== outputDir) return fs.exists(target);
            outputExistsChecks += 1;
            return outputExistsChecks === 2
              ? Effect.gen(function* () {
                  yield* Deferred.succeed(rollbackCheckingOutput, undefined);
                  yield* Deferred.await(releaseRollbackCheck);
                  return yield* fs.exists(target);
                })
              : fs.exists(target);
          },
          move: (source, target) =>
            source === stageDir && target === outputDir
              ? Effect.gen(function* () {
                  yield* fs.makeDirectory(outputDir);
                  yield* fs.writeFileString(path.join(outputDir, "artifact.txt"), "competitor");
                  yield* fs.writeFileString(
                    path.join(outputDir, ".bibcode-publication-owner"),
                    "competitor",
                  );
                  return yield* raceCause;
                })
              : fs.rename(source, target),
        },
      ).pipe(Effect.forkChild({ startImmediately: true }));

      yield* Deferred.await(rollbackCheckingOutput);
      const interruption = yield* Fiber.interrupt(publication).pipe(
        Effect.forkChild({ startImmediately: true }),
      );
      yield* Effect.yieldNow;
      yield* Deferred.succeed(releaseRollbackCheck, undefined);
      yield* Fiber.join(interruption);

      assert.equal(yield* fs.readFileString(path.join(outputDir, "artifact.txt")), "competitor");
      assert.equal(
        yield* fs.readFileString(path.join(outputDir, ".bibcode-publication-owner")),
        "competitor",
      );
      assert.isFalse(yield* fs.exists(stageDir));
      assert.isFalse(yield* fs.exists(backupDir));
    }),
  );

  it.effect("preserves output across staging, manifest, and backup phase failures", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const failureCause = PlatformError.systemError({
        _tag: "PermissionDenied",
        module: "FileSystem",
        method: "publication",
      });

      for (const phase of [
        "stage",
        "source-manifest",
        "staged-manifest",
        "owner-marker",
        "backup",
      ] as const) {
        const tempDir = yield* fs.makeTempDirectoryScoped({ prefix: `bibcode-${phase}-failure-` });
        const bundleDir = path.join(tempDir, "bundle");
        const outputDir = path.join(tempDir, "output");
        const stageDir = path.join(tempDir, `.output.bibcode-${phase}.stage`);
        yield* fs.makeDirectory(bundleDir);
        yield* fs.makeDirectory(outputDir);
        yield* fs.writeFileString(path.join(bundleDir, "artifact.txt"), "new");
        yield* fs.writeFileString(path.join(outputDir, "artifact.txt"), "old");

        const error = yield* copyTauriBundleArtifacts(
          { bundleDir, outputDir },
          {
            transactionId: () => phase,
            makeDirectory: (target, options) =>
              phase === "stage" && target === stageDir
                ? Effect.fail(failureCause)
                : fs.makeDirectory(target, options),
            readDirectory: (target, options) => {
              if (phase === "staged-manifest" && target === stageDir) {
                return Effect.fail(failureCause);
              }
              return fs.readDirectory(target, options);
            },
            stat: (target) =>
              phase === "source-manifest" && target === path.join(bundleDir, "artifact.txt")
                ? Effect.fail(failureCause)
                : fs.stat(target),
            move: (source, target) =>
              phase === "backup" && source === outputDir
                ? Effect.fail(failureCause)
                : fs.rename(source, target),
            writeFileString: (target, value, options) =>
              phase === "owner-marker" && target.endsWith(".bibcode-publication-owner")
                ? Effect.fail(failureCause)
                : fs.writeFileString(target, value, options),
          },
        ).pipe(Effect.flip);

        assert.instanceOf(error, TauriDesktopBuildPublicationError);
        assert.equal(
          error.operation,
          phase === "stage"
            ? "copy"
            : phase === "backup" || phase === "owner-marker"
              ? "swap"
              : "validate-staging",
        );
        assert.equal(yield* fs.readFileString(path.join(outputDir, "artifact.txt")), "old");
        assert.isFalse(yield* fs.exists(stageDir));
      }
    }),
  );

  it.effect("reports a pre-existing staging directory on transaction-id collision", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tempDir = yield* fs.makeTempDirectoryScoped({ prefix: "bibcode-stage-collision-" });
      const bundleDir = path.join(tempDir, "bundle");
      const outputDir = path.join(tempDir, "output");
      const stageDir = path.join(tempDir, ".output.bibcode-collision.stage");
      yield* fs.makeDirectory(bundleDir);
      yield* fs.makeDirectory(stageDir);
      yield* fs.writeFileString(path.join(bundleDir, "artifact.txt"), "new");
      yield* fs.writeFileString(path.join(stageDir, "owner.txt"), "pre-existing");

      const error = yield* copyTauriBundleArtifacts(
        { bundleDir, outputDir },
        { transactionId: () => "collision" },
      ).pipe(Effect.flip);

      assert.instanceOf(error, TauriDesktopBuildPublicationError);
      assert.equal(yield* fs.readFileString(path.join(stageDir, "owner.txt")), "pre-existing");
      assert.deepStrictEqual(error.recoveryPaths, [{ kind: "staging", path: stageDir }]);
      assert.include(error.message, stageDir);
    }),
  );

  it.effect("preserves the publication error when initial or backup restore probes fail", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      for (const [probeName, failedProbe] of [
        ["initial", 2],
        ["backup-restore", 3],
      ] as const) {
        const tempDir = yield* fs.makeTempDirectoryScoped({
          prefix: `bibcode-${probeName}-probe-`,
        });
        const bundleDir = path.join(tempDir, "bundle");
        const outputDir = path.join(tempDir, "output");
        const stageDir = path.join(tempDir, `.output.bibcode-${probeName}.stage`);
        const backupDir = path.join(tempDir, `.output.bibcode-${probeName}.backup`);
        yield* fs.makeDirectory(bundleDir);
        yield* fs.makeDirectory(outputDir);
        yield* fs.writeFileString(path.join(bundleDir, "artifact.txt"), "new");
        yield* fs.writeFileString(path.join(outputDir, "artifact.txt"), "old");
        const publicationCause = PlatformError.systemError({
          _tag: "Unknown",
          module: "FileSystem",
          method: "post-swap",
          pathOrDescriptor: outputDir,
        });
        const probeCause = PlatformError.systemError({
          _tag: "Unknown",
          module: "FileSystem",
          method: "exists",
          pathOrDescriptor: outputDir,
        });
        let outputProbes = 0;

        const error = yield* copyTauriBundleArtifacts(
          { bundleDir, outputDir },
          {
            transactionId: () => probeName,
            exists: (target) => {
              if (target !== outputDir) return fs.exists(target);
              outputProbes += 1;
              return outputProbes === failedProbe ? Effect.fail(probeCause) : fs.exists(target);
            },
            move: (source, target) =>
              source === stageDir && target === outputDir
                ? fs
                    .rename(source, target)
                    .pipe(Effect.flatMap(() => Effect.fail(publicationCause)))
                : fs.rename(source, target),
          },
        ).pipe(Effect.flip);

        if (!(error instanceof TauriDesktopBuildPublicationError)) throw error;
        assert.strictEqual(error.cause, publicationCause);
        assert.include(
          error.rollbackFailures.map((failure) => failure.operation),
          "inspect-output",
        );
        assert.strictEqual(
          error.rollbackFailures.find((failure) => failure.operation === "inspect-output")?.cause,
          probeCause,
        );
        assert.equal(yield* fs.readFileString(path.join(backupDir, "artifact.txt")), "old");
        assert.deepStrictEqual(error.recoveryPaths, [{ kind: "backup", path: backupDir }]);
        assert.include(error.message, backupDir);
      }
    }),
  );

  it.effect("reports quarantine when the foreign restore output probe fails", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tempDir = yield* fs.makeTempDirectoryScoped({ prefix: "bibcode-foreign-probe-" });
      const bundleDir = path.join(tempDir, "bundle");
      const outputDir = path.join(tempDir, "output");
      const stageDir = path.join(tempDir, ".output.bibcode-foreign-probe.stage");
      const quarantineDir = path.join(tempDir, ".output.bibcode-foreign-probe.quarantine");
      const ownerFile = ".bibcode-publication-owner";
      yield* fs.makeDirectory(bundleDir);
      yield* fs.writeFileString(path.join(bundleDir, "artifact.txt"), "ours");
      const publicationCause = PlatformError.systemError({
        _tag: "AlreadyExists",
        module: "FileSystem",
        method: "rename",
        pathOrDescriptor: outputDir,
      });
      const probeCause = PlatformError.systemError({
        _tag: "Unknown",
        module: "FileSystem",
        method: "exists",
        pathOrDescriptor: outputDir,
      });
      let outputProbes = 0;

      const error = yield* copyTauriBundleArtifacts(
        { bundleDir, outputDir },
        {
          transactionId: () => "foreign-probe",
          ownershipToken: () => "ours",
          exists: (target) => {
            if (target !== outputDir) return fs.exists(target);
            outputProbes += 1;
            return outputProbes === 3 ? Effect.fail(probeCause) : fs.exists(target);
          },
          move: (source, target) =>
            source === stageDir && target === outputDir
              ? Effect.gen(function* () {
                  yield* fs.makeDirectory(outputDir);
                  yield* fs.writeFileString(path.join(outputDir, "artifact.txt"), "foreign");
                  yield* fs.writeFileString(path.join(outputDir, ownerFile), "foreign");
                  return yield* publicationCause;
                })
              : fs.rename(source, target),
        },
      ).pipe(Effect.flip);

      if (!(error instanceof TauriDesktopBuildPublicationError)) throw error;
      assert.strictEqual(error.cause, publicationCause);
      assert.include(
        error.rollbackFailures.map((failure) => failure.operation),
        "inspect-output",
      );
      assert.strictEqual(
        error.rollbackFailures.find((failure) => failure.operation === "inspect-output")?.cause,
        probeCause,
      );
      assert.equal(yield* fs.readFileString(path.join(quarantineDir, "artifact.txt")), "foreign");
      assert.isFalse(yield* fs.exists(stageDir));
      assert.deepStrictEqual(error.recoveryPaths, [{ kind: "quarantine", path: quarantineDir }]);
      assert.include(error.message, quarantineDir);
    }),
  );

  it.effect("fails a committed publication when retained backup cleanup fails", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tempDir = yield* fs.makeTempDirectoryScoped({ prefix: "bibcode-committed-backup-" });
      const bundleDir = path.join(tempDir, "bundle");
      const outputDir = path.join(tempDir, "output");
      const backupDir = path.join(tempDir, ".output.bibcode-committed-backup.backup");
      yield* fs.makeDirectory(bundleDir);
      yield* fs.makeDirectory(outputDir);
      yield* fs.writeFileString(path.join(bundleDir, "artifact.txt"), "published");
      yield* fs.writeFileString(path.join(outputDir, "artifact.txt"), "prior");
      const cleanupCause = PlatformError.systemError({
        _tag: "PermissionDenied",
        module: "FileSystem",
        method: "remove",
        pathOrDescriptor: backupDir,
      });

      const error = yield* copyTauriBundleArtifacts(
        { bundleDir, outputDir },
        {
          transactionId: () => "committed-backup",
          remove: (target, options) =>
            target === backupDir ? Effect.fail(cleanupCause) : fs.remove(target, options),
        },
      ).pipe(Effect.flip);

      if (!(error instanceof TauriDesktopBuildPublicationError)) throw error;
      assert.equal(error.operation, "swap");
      assert.strictEqual(error.cause, cleanupCause);
      assert.deepStrictEqual(error.rollbackFailures, [
        { operation: "cleanup-backup", path: backupDir, cause: cleanupCause },
      ]);
      assert.deepStrictEqual(error.recoveryPaths, [{ kind: "backup", path: backupDir }]);
      assert.include(error.message, backupDir);
      assert.equal(yield* fs.readFileString(path.join(outputDir, "artifact.txt")), "published");
      assert.equal(yield* fs.readFileString(path.join(backupDir, "artifact.txt")), "prior");
    }),
  );

  it.effect("preserves and reports backup when atomic restoration fails", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tempDir = yield* fs.makeTempDirectoryScoped({ prefix: "bibcode-rollback-fallback-" });
      const bundleDir = path.join(tempDir, "bundle");
      const outputDir = path.join(tempDir, "output");
      const stageDir = path.join(tempDir, ".output.bibcode-rollback-fallback.stage");
      const backupDir = path.join(tempDir, ".output.bibcode-rollback-fallback.backup");
      yield* fs.makeDirectory(bundleDir);
      yield* fs.makeDirectory(outputDir);
      yield* fs.writeFileString(path.join(bundleDir, "artifact.txt"), "new");
      yield* fs.writeFileString(path.join(outputDir, "artifact.txt"), "old");
      const swapCause = PlatformError.systemError({
        _tag: "Unknown",
        module: "FileSystem",
        method: "rename",
        pathOrDescriptor: stageDir,
      });
      const restoreCause = PlatformError.systemError({
        _tag: "PermissionDenied",
        module: "FileSystem",
        method: "rename",
        pathOrDescriptor: backupDir,
      });
      const recoveryAuditCause = PlatformError.systemError({
        _tag: "Unknown",
        module: "FileSystem",
        method: "exists",
        pathOrDescriptor: backupDir,
      });

      const error = yield* copyTauriBundleArtifacts(
        { bundleDir, outputDir },
        {
          transactionId: () => "rollback-fallback",
          copy: (source, target, options) => fs.copy(source, target, options),
          exists: (target) =>
            target === backupDir ? Effect.fail(recoveryAuditCause) : fs.exists(target),
          move: (source, target) => {
            if (source === stageDir && target === outputDir) {
              return fs.rename(source, target).pipe(Effect.flatMap(() => Effect.fail(swapCause)));
            }
            if (source === backupDir && target === outputDir) return Effect.fail(restoreCause);
            return fs.rename(source, target);
          },
        },
      ).pipe(Effect.flip);

      if (!(error instanceof TauriDesktopBuildPublicationError)) throw error;
      assert.strictEqual(error.cause, swapCause);
      assert.isFalse(yield* fs.exists(outputDir));
      assert.isFalse(yield* fs.exists(stageDir));
      assert.equal(yield* fs.readFileString(path.join(backupDir, "artifact.txt")), "old");
      assert.deepStrictEqual(error.recoveryPaths, [{ kind: "backup", path: backupDir }]);
      assert.include(error.message, backupDir);
    }),
  );

  it.effect("keeps competitor output isolated when atomic backup restoration loses a race", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tempDir = yield* fs.makeTempDirectoryScoped({ prefix: "bibcode-restore-race-" });
      const bundleDir = path.join(tempDir, "bundle");
      const outputDir = path.join(tempDir, "output");
      const stageDir = path.join(tempDir, ".output.bibcode-restore-race.stage");
      const backupDir = path.join(tempDir, ".output.bibcode-restore-race.backup");
      yield* fs.makeDirectory(bundleDir);
      yield* fs.makeDirectory(outputDir);
      yield* fs.writeFileString(path.join(bundleDir, "new-artifact.txt"), "new");
      yield* fs.writeFileString(path.join(outputDir, "prior-artifact.txt"), "prior");
      const postSwapCause = PlatformError.systemError({
        _tag: "Unknown",
        module: "FileSystem",
        method: "post-swap",
        pathOrDescriptor: outputDir,
      });
      const restoreCause = PlatformError.systemError({
        _tag: "AlreadyExists",
        module: "FileSystem",
        method: "rename",
        pathOrDescriptor: outputDir,
      });

      const error = yield* copyTauriBundleArtifacts(
        { bundleDir, outputDir },
        {
          transactionId: () => "restore-race",
          move: (source, target) => {
            if (source === stageDir && target === outputDir) {
              return fs
                .rename(source, target)
                .pipe(Effect.flatMap(() => Effect.fail(postSwapCause)));
            }
            if (source === backupDir && target === outputDir) {
              return Effect.gen(function* () {
                yield* fs.makeDirectory(outputDir);
                yield* fs.writeFileString(path.join(outputDir, "competitor.txt"), "competitor");
                return yield* restoreCause;
              });
            }
            return fs.rename(source, target);
          },
        },
      ).pipe(Effect.flip);

      if (!(error instanceof TauriDesktopBuildPublicationError)) throw error;
      assert.deepStrictEqual(yield* fs.readDirectory(outputDir), ["competitor.txt"]);
      assert.equal(yield* fs.readFileString(path.join(outputDir, "competitor.txt")), "competitor");
      assert.deepStrictEqual(yield* fs.readDirectory(backupDir), ["prior-artifact.txt"]);
      assert.equal(yield* fs.readFileString(path.join(backupDir, "prior-artifact.txt")), "prior");
      assert.deepStrictEqual(error.recoveryPaths, [{ kind: "backup", path: backupDir }]);
      assert.include(error.message, backupDir);
      assert.notInclude(
        error.rollbackFailures.map((failure) => failure.operation),
        "copy-output",
      );
    }),
  );

  it.effect("retains the valid backup when every rollback primitive fails", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      for (const removeBeforeFailure of [true, false]) {
        const tempDir = yield* fs.makeTempDirectoryScoped({
          prefix: "bibcode-rollback-exhausted-",
        });
        const bundleDir = path.join(tempDir, "bundle");
        const outputDir = path.join(tempDir, "output");
        const stageDir = path.join(tempDir, ".output.bibcode-exhausted.stage");
        const backupDir = path.join(tempDir, ".output.bibcode-exhausted.backup");
        const quarantineDir = path.join(tempDir, ".output.bibcode-exhausted.quarantine");
        yield* fs.makeDirectory(bundleDir);
        yield* fs.makeDirectory(outputDir);
        yield* fs.writeFileString(path.join(bundleDir, "artifact.txt"), "new");
        yield* fs.writeFileString(path.join(outputDir, "artifact.txt"), "old");
        const cause = PlatformError.systemError({
          _tag: "PermissionDenied",
          module: "FileSystem",
          method: "rollback",
        });

        const error = yield* copyTauriBundleArtifacts(
          { bundleDir, outputDir },
          {
            transactionId: () => "exhausted",
            move: (source, target) => {
              if (source === stageDir && target === outputDir) {
                return fs.rename(source, target).pipe(Effect.flatMap(() => Effect.fail(cause)));
              }
              if (source === backupDir && target === outputDir) {
                return Effect.fail(cause);
              }
              return fs.rename(source, target);
            },
            remove: (target, options) =>
              target === quarantineDir
                ? (removeBeforeFailure ? fs.remove(target, options) : Effect.void).pipe(
                    Effect.flatMap(() => Effect.fail(cause)),
                  )
                : fs.remove(target, options),
          },
        ).pipe(Effect.flip);

        if (!(error instanceof TauriDesktopBuildPublicationError)) {
          throw error;
        }
        assert.include(
          error.rollbackFailures.map((failure) => failure.operation),
          "remove-output",
        );
        assert.include(
          error.rollbackFailures.map((failure) => failure.operation),
          "restore-output",
        );
        assert.equal(yield* fs.readFileString(path.join(backupDir, "artifact.txt")), "old");
        assert.isFalse(yield* fs.exists(stageDir));
        assert.isFalse(yield* fs.exists(outputDir));
        assert.equal(yield* fs.exists(quarantineDir), !removeBeforeFailure);
        assert.deepStrictEqual(
          error.recoveryPaths,
          removeBeforeFailure
            ? [{ kind: "backup", path: backupDir }]
            : [
                { kind: "backup", path: backupDir },
                { kind: "quarantine", path: quarantineDir },
              ],
        );
        assert.include(error.message, backupDir);
        if (!removeBeforeFailure) assert.include(error.message, quarantineDir);
      }
    }),
  );

  it.effect("leaves output untouched when atomic rollback quarantine fails", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const tempDir = yield* fs.makeTempDirectoryScoped({ prefix: "bibcode-quarantine-failure-" });
      const bundleDir = path.join(tempDir, "bundle");
      const outputDir = path.join(tempDir, "output");
      const stageDir = path.join(tempDir, ".output.bibcode-quarantine-failure.stage");
      const backupDir = path.join(tempDir, ".output.bibcode-quarantine-failure.backup");
      const quarantineDir = path.join(tempDir, ".output.bibcode-quarantine-failure.quarantine");
      yield* fs.makeDirectory(bundleDir);
      yield* fs.makeDirectory(outputDir);
      yield* fs.writeFileString(path.join(bundleDir, "artifact.txt"), "new");
      yield* fs.writeFileString(path.join(outputDir, "artifact.txt"), "old");
      const cause = PlatformError.systemError({
        _tag: "PermissionDenied",
        module: "FileSystem",
        method: "rename",
        pathOrDescriptor: quarantineDir,
      });

      const error = yield* copyTauriBundleArtifacts(
        { bundleDir, outputDir },
        {
          transactionId: () => "quarantine-failure",
          move: (source, target) => {
            if (source === stageDir && target === outputDir) {
              return fs.rename(source, target).pipe(Effect.flatMap(() => Effect.fail(cause)));
            }
            return source === outputDir && target === quarantineDir
              ? Effect.fail(cause)
              : fs.rename(source, target);
          },
        },
      ).pipe(Effect.flip);

      if (!(error instanceof TauriDesktopBuildPublicationError)) throw error;
      assert.include(
        error.rollbackFailures.map((failure) => failure.operation),
        "quarantine-output",
      );
      assert.equal(yield* fs.readFileString(path.join(outputDir, "artifact.txt")), "new");
      assert.equal(yield* fs.readFileString(path.join(backupDir, "artifact.txt")), "old");
      assert.isFalse(yield* fs.exists(stageDir));
      assert.isFalse(yield* fs.exists(quarantineDir));
    }),
  );

  it.effect("validates platform, architecture, target, host, and boolean configuration", () =>
    Effect.gen(function* () {
      const path = yield* Path.Path;
      const repoRoot = path.resolve("X:/repo");
      const invalidInputs = [
        [{ platform: "android" }, {}, "Unsupported Tauri platform"],
        [{ platform: "win", arch: "x86" }, {}, "Unsupported Tauri arch"],
        [{ platform: "linux", target: "dmg", arch: "x64" }, {}, "Unsupported Tauri linux target"],
        [
          { platform: "win", arch: "x64" },
          { BIBCODE_TAURI_DESKTOP_VERBOSE: "maybe" },
          "must be true/false",
        ],
      ] as const;

      for (const [input, env, message] of invalidInputs) {
        const error = yield* resolveTauriBuildPlan(
          input,
          env,
          { platform: "win32", arch: "x64" },
          repoRoot,
        ).pipe(Effect.flip);
        assert.instanceOf(error, TauriDesktopBuildConfigurationError);
        assert.include(error.message, message);
      }

      const unsupportedHost = yield* resolveTauriBuildPlan(
        {},
        {},
        { platform: "freebsd", arch: "x64" },
        repoRoot,
      ).pipe(Effect.flip);
      assert.instanceOf(unsupportedHost, TauriDesktopBuildConfigurationError);
      assert.include(unsupportedHost.message, "Unsupported host platform");
    }),
  );

  it.effect("builds, copies, and reports artifacts through injected process services", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const repoRoot = yield* fs.makeTempDirectoryScoped({
        prefix: "bibcode-build-tauri-artifact-",
      });
      const plan = yield* resolveTauriBuildPlan(
        { platform: "win", arch: "x64", target: "nsis", outputDir: "out", verbose: true },
        {},
        { platform: "win32", arch: "x64" },
        repoRoot,
      );
      yield* fs.makeDirectory(plan.bundleDir, { recursive: true });
      yield* fs.writeFileString(path.join(plan.bundleDir, "installer.exe"), "binary");
      const writes: string[] = [];
      const spawnPlans: unknown[] = [];
      const spawnerLayer = Layer.succeed(
        ChildProcessSpawner.ChildProcessSpawner,
        ChildProcessSpawner.make((command) => {
          spawnPlans.push(command);
          return Effect.succeed(processHandle(0));
        }),
      );

      const artifacts = yield* buildTauriDesktopArtifact(
        { platform: "win", arch: "x64", target: "nsis", outputDir: "out", verbose: true },
        {},
        { write: (text) => writes.push(text), host: { platform: "win32", arch: "x64" }, repoRoot },
      ).pipe(Effect.provide(spawnerLayer));

      assert.equal(spawnPlans.length, 2);
      const spawned = spawnPlans[0] as {
        readonly options: { readonly stdout?: unknown; readonly stderr?: unknown };
      };
      assert.equal(spawned.options.stdout, "inherit");
      assert.equal(spawned.options.stderr, "inherit");
      assert.deepStrictEqual(artifacts, [path.join(repoRoot, "out", "installer.exe")]);
      assert.include(writes.join(""), "Building win/nsis");
      assert.include(writes.join(""), "Artifacts copied");
      assert.include(writes.join(""), "installer.exe");

      const stdout = vi.spyOn(process.stdout, "write").mockImplementation(() => true);
      try {
        yield* buildTauriDesktopArtifact(
          { platform: "win", arch: "x64", target: "nsis", outputDir: "out", skipBuild: true },
          {},
          { host: { platform: "win32", arch: "x64" }, repoRoot },
        ).pipe(Effect.provide(spawnerLayer));
        assert.equal(stdout.mock.calls.length, 1);
      } finally {
        stdout.mockRestore();
      }
    }),
  );

  it.effect("retries nonzero build exits and skips spawning when requested", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const repoRoot = yield* fs.makeTempDirectoryScoped({ prefix: "bibcode-build-exit-" });
      const plan = yield* resolveTauriBuildPlan(
        { platform: "win", arch: "x64", target: "nsis" },
        {},
        { platform: "win32", arch: "x64" },
        repoRoot,
      );
      yield* fs.makeDirectory(plan.bundleDir, { recursive: true });
      yield* fs.writeFileString(path.join(plan.bundleDir, "installer.exe"), "binary");
      let spawnCount = 0;
      const failingSpawner = Layer.succeed(
        ChildProcessSpawner.ChildProcessSpawner,
        ChildProcessSpawner.make(() => {
          spawnCount += 1;
          return Effect.succeed(processHandle(9));
        }),
      );

      const writes: string[] = [];
      const error = yield* buildTauriDesktopArtifact(
        { platform: "win", arch: "x64", target: "nsis" },
        {},
        { write: (text) => writes.push(text), host: { platform: "win32", arch: "x64" }, repoRoot },
      ).pipe(Effect.provide(failingSpawner), Effect.flip);
      assert.instanceOf(error, TauriDesktopBuildConfigurationError);
      assert.include(error.message, `after ${String(TAURI_BUILD_ATTEMPTS)} attempts`);
      assert.include(error.message, "First failure: Tauri build command exited with code 9");
      assert.equal(
        writes.filter((text) => text.includes("Build attempt")).length,
        TAURI_BUILD_ATTEMPTS,
      );
      assert.include(
        writes.join(""),
        "Build attempt 1 of 3 failed: Tauri build command exited with code 9",
      );

      const artifacts = yield* buildTauriDesktopArtifact(
        { platform: "win", arch: "x64", target: "nsis", skipBuild: true },
        {},
        { write: () => undefined, host: { platform: "win32", arch: "x64" }, repoRoot },
      ).pipe(Effect.provide(failingSpawner));
      // A non-DMG build never inspects host mounts, so only the build attempts spawn.
      assert.equal(spawnCount, TAURI_BUILD_ATTEMPTS);
      assert.equal(artifacts.length, 1);
    }),
  );

  it.effect("keeps a macOS DMG build verbose so bundle_dmg.sh diagnostics are captured", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const repoRoot = yield* fs.makeTempDirectoryScoped({ prefix: "bibcode-build-dmg-plan-" });
      const dmg = yield* resolveTauriBuildPlan(
        { platform: "mac", arch: "arm64", target: "dmg" },
        {},
        { platform: "darwin", arch: "arm64" },
        repoRoot,
      );
      assert.include(dmg.bundleCommand.args, "--verbose");
      const app = yield* resolveTauriBuildPlan(
        { platform: "mac", arch: "arm64", target: "app" },
        {},
        { platform: "darwin", arch: "arm64" },
        repoRoot,
      );
      assert.notInclude(app.bundleCommand.args, "--verbose");
    }),
  );

  it("parses hdiutil image reports and recognizes only this build's intermediate image", () => {
    const images = parseHdiutilImages(
      hdiutilInfoPlist([
        {
          path: "/repo/target/aarch64-apple-darwin/release/bundle/dmg/rw.BiBCode_0.4.2_aarch64.dmg",
          device: "/dev/disk9",
        },
        { path: "/Users/someone/Downloads/BiBCode_0.4.1_aarch64.dmg", device: "/dev/disk4" },
      ]),
    );
    assert.deepStrictEqual(
      images.map((image) => ({ imagePath: image.imagePath, device: image.devices[0] })),
      [
        {
          imagePath:
            "/repo/target/aarch64-apple-darwin/release/bundle/dmg/rw.BiBCode_0.4.2_aarch64.dmg",
          device: "/dev/disk9",
        },
        { imagePath: "/Users/someone/Downloads/BiBCode_0.4.1_aarch64.dmg", device: "/dev/disk4" },
      ],
    );
    assert.deepStrictEqual(parseHdiutilImages("not a plist"), []);
  });

  it.effect(
    "detaches only this build's leaked intermediate image after a failed DMG attempt and retries",
    () =>
      Effect.gen(function* () {
        const fs = yield* FileSystem.FileSystem;
        const path = yield* Path.Path;
        const repoRoot = yield* fs.makeTempDirectoryScoped({ prefix: "bibcode-build-dmg-leak-" });
        const plan = yield* resolveTauriBuildPlan(
          { platform: "mac", arch: "arm64", target: "dmg" },
          {},
          { platform: "darwin", arch: "arm64" },
          repoRoot,
        );
        yield* fs.makeDirectory(plan.bundleDir, { recursive: true });
        yield* fs.writeFileString(path.join(plan.bundleDir, "BiBCode_0.4.2_aarch64.dmg"), "image");
        const ownIntermediate = path.join(plan.bundleDir, "rw.BiBCode_0.4.2_aarch64.dmg");
        const foreignIntermediate = path.join(
          repoRoot,
          "other-worktree",
          "target",
          "aarch64-apple-darwin",
          "release",
          "bundle",
          "dmg",
          "rw.BiBCode_0.4.2_aarch64.dmg",
        );
        assert.isTrue(isRunOwnedIntermediateDmg(path, plan.bundleDir, ownIntermediate));
        assert.isFalse(isRunOwnedIntermediateDmg(path, plan.bundleDir, foreignIntermediate));
        assert.isFalse(
          isRunOwnedIntermediateDmg(
            path,
            plan.bundleDir,
            path.join(plan.bundleDir, "BiBCode_0.4.2_aarch64.dmg"),
          ),
        );

        const spawned: Array<{ command: string; args: ReadonlyArray<string> }> = [];
        let builds = 0;
        let bundleAttempts = 0;
        const spawnerLayer = Layer.succeed(
          ChildProcessSpawner.ChildProcessSpawner,
          ChildProcessSpawner.make((command) => {
            if (command._tag !== "StandardCommand") {
              throw new Error("The artifact wrapper never pipes commands.");
            }
            spawned.push({ command: command.command, args: command.args });
            if (command.command === "hdiutil" && command.args[0] === "info") {
              return Effect.succeed(
                processHandle(
                  0,
                  hdiutilInfoPlist([
                    { path: "/Volumes/Downloads/BiBCode_0.4.1_aarch64.dmg", device: "/dev/disk4" },
                    { path: ownIntermediate, device: "/dev/disk9" },
                    { path: foreignIntermediate, device: "/dev/disk11" },
                  ]),
                ),
              );
            }
            if (command.command === "hdiutil") {
              return Effect.succeed(processHandle(0));
            }
            if (command.args.includes("build")) {
              builds += 1;
              return Effect.succeed(processHandle(0));
            }
            bundleAttempts += 1;
            return Effect.succeed(processHandle(bundleAttempts === 1 ? 1 : 0));
          }),
        );
        const writes: string[] = [];

        const artifacts = yield* buildTauriDesktopArtifact(
          { platform: "mac", arch: "arm64", target: "dmg", outputDir: "out" },
          {},
          {
            write: (text) => writes.push(text),
            host: { platform: "darwin", arch: "arm64" },
            repoRoot,
          },
        ).pipe(Effect.provide(spawnerLayer));

        // A failed DMG attempt is retried as a bundle; the app is not compiled again.
        assert.equal(builds, 1);
        assert.equal(bundleAttempts, 2);
        const detaches = spawned.filter(
          (entry) => entry.command === "hdiutil" && entry.args[0] === "detach",
        );
        assert.deepStrictEqual(
          detaches.map((entry) => entry.args),
          [["detach", "/dev/disk9", "-force"]],
        );
        assert.include(
          writes.join(""),
          "Bundle attempt 1 of 3 failed: Tauri bundle command exited with code 1",
        );
        assert.include(
          writes.join(""),
          `Detached this build's intermediate image ${ownIntermediate} (/dev/disk9)`,
        );
        assert.notInclude(writes.join(""), "/dev/disk4");
        assert.notInclude(writes.join(""), "/dev/disk11");
        assert.equal(artifacts.length, 1);

        // An inspection failure is reported, never silently ignored.
        const report = yield* detachRunOwnedDmgMounts(plan, {}).pipe(
          Effect.provide(
            Layer.succeed(
              ChildProcessSpawner.ChildProcessSpawner,
              ChildProcessSpawner.make(() => Effect.succeed(processHandle(3))),
            ),
          ),
        );
        assert.deepStrictEqual(report.detached, []);
        assert.equal(report.failures.length, 1);
        assert.include(report.failures[0]?.detail, "hdiutil info exited with code 3");
      }),
  );

  it.effect("plans the symbol dump and the platform strip between compiling and bundling", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const repoRoot = yield* fs.makeTempDirectoryScoped({ prefix: "bibcode-build-symbols-" });
      yield* fs.makeDirectory(path.join(repoRoot, "apps", "desktop"), { recursive: true });
      yield* fs.writeFileString(
        path.join(repoRoot, "apps", "desktop", "package.json"),
        '{ "version": "1.2.3" }',
      );
      const planFor = (platform: "linux" | "mac" | "win", hostPlatform: NodeJS.Platform) =>
        resolveTauriBuildPlan(
          { platform, arch: "x64", symbols: true },
          {},
          { platform: hostPlatform, arch: "x64" },
          repoRoot,
        );
      const dumpScript = path.join(repoRoot, "scripts", "dump-symbols.ts");

      const linux = yield* planFor("linux", "linux");
      const linuxRelease = path.join(repoRoot, "target", "x86_64-unknown-linux-gnu", "release");
      const linuxArchive = path.join(
        linuxRelease,
        "bibcode-desktop-symbols-1.2.3-linux-x86_64.zip",
      );
      assert.equal(linux.symbols?.archive, linuxArchive);
      assert.deepStrictEqual(linux.symbols?.dumpCommand, {
        command: process.execPath,
        cwd: repoRoot,
        args: [
          dumpScript,
          "--binary",
          path.join(linuxRelease, "bibcode-desktop"),
          "--module-name",
          "bibcode-desktop",
          "--out",
          path.join(linuxRelease, "symbols"),
          "--zip",
          linuxArchive,
        ],
      });
      assert.equal(linux.symbols?.binary, path.join(linuxRelease, "bibcode-desktop"));
      // A stripped copy replaces the binary, which Cargo hard-links to its cached artifact.
      assert.deepStrictEqual(linux.symbols?.strip, {
        command: {
          command: "strip",
          cwd: repoRoot,
          args: [
            "--strip-all",
            "-o",
            path.join(linuxRelease, "bibcode-desktop.stripped"),
            path.join(linuxRelease, "bibcode-desktop"),
          ],
        },
        output: path.join(linuxRelease, "bibcode-desktop.stripped"),
      });

      const mac = yield* planFor("mac", "darwin");
      const macRelease = path.join(repoRoot, "target", "x86_64-apple-darwin", "release");
      assert.equal(
        mac.symbols?.archive,
        path.join(macRelease, "bibcode-desktop-symbols-1.2.3-darwin-x86_64.zip"),
      );
      assert.includeMembers(
        [...(mac.symbols?.dumpCommand.args ?? [])],
        ["--dsym", path.join(macRelease, "bibcode-desktop.dSYM")],
      );
      // The DMG bundler deletes the app it builds unless `app` is requested too, and the check
      // after bundling reads the binary in it.
      assert.include(mac.bundleCommand.args, "app,dmg");
      // Plain strip, rustc's level for executables (`-x` is for dynamic libraries).
      assert.deepStrictEqual(mac.symbols?.strip?.command.args, [
        "-o",
        path.join(macRelease, "bibcode-desktop.stripped"),
        path.join(macRelease, "bibcode-desktop"),
      ]);

      // Windows keeps the names in the PDB and ships the binary as linked.
      const win = yield* planFor("win", "win32");
      const winRelease = path.join(repoRoot, "target", "x86_64-pc-windows-msvc", "release");
      assert.includeMembers(
        [...(win.symbols?.dumpCommand.args ?? [])],
        [
          path.join(winRelease, "bibcode-desktop.exe"),
          "bibcode-desktop.exe",
          "--pdb",
          path.join(winRelease, "bibcode_desktop.pdb"),
        ],
      );
      assert.isUndefined(win.symbols?.strip);

      const error = yield* resolveTauriBuildPlan(
        { platform: "linux", arch: "x64", symbols: true, skipBuild: true },
        {},
        { platform: "linux", arch: "x64" },
        repoRoot,
      ).pipe(Effect.flip);
      assert.instanceOf(error, TauriDesktopBuildConfigurationError);
      assert.include(error.message, "--symbols");
    }),
  );

  it("compiles release symbols unstripped, with line tables, a dSYM and a GNU build-id", () => {
    assert.deepStrictEqual(
      symbolBuildEnvironment("linux", { RUSTFLAGS: "-C target-cpu=x86-64-v2" }),
      {
        CARGO_PROFILE_RELEASE_STRIP: "none",
        CARGO_PROFILE_RELEASE_DEBUG: "line-tables-only",
        RUSTFLAGS: "-C target-cpu=x86-64-v2 -C link-arg=-Wl,--build-id=sha1",
      },
    );
    assert.deepStrictEqual(symbolBuildEnvironment("mac", { KEPT: "1" }), {
      KEPT: "1",
      CARGO_PROFILE_RELEASE_STRIP: "none",
      CARGO_PROFILE_RELEASE_DEBUG: "line-tables-only",
      CARGO_PROFILE_RELEASE_SPLIT_DEBUGINFO: "packed",
    });
    assert.deepStrictEqual(symbolBuildEnvironment("win", {}), {
      CARGO_PROFILE_RELEASE_STRIP: "none",
      CARGO_PROFILE_RELEASE_DEBUG: "line-tables-only",
    });
  });

  const BUILD_ID = "5852920ede2c069ed4ba17fe44570dad84b63c1d";
  const MACHO_UUID = "0e9252582cde9e06d4ba17fe44570dad";
  const ELF = {
    unstripped: syntheticElf({ buildId: BUILD_ID, symbols: 5 }),
    stripped: syntheticElf({ buildId: BUILD_ID, symbols: 0 }),
  };

  /**
   * A `--symbols` build in a scratch repository. Cargo's binary is a hard link to its unstripped
   * deps/ artifact; the fake dump writes the archive, strip writes `stripped` to its `-o`
   * output, `onBundle` models the bundler (returning an exit code fails that attempt), and an
   * AppImage extracts `extracted`.
   */
  const symbolsBuild = Effect.fn("symbolsBuild")(function* (options: {
    readonly platform: "linux" | "mac" | "win";
    readonly target: string;
    readonly bundleFile: string;
    readonly unstripped: Buffer;
    readonly stripped: Buffer;
    readonly extracted?: Buffer;
    readonly onBundle?: (context: {
      readonly bundleDir: string;
      readonly binary: string;
      readonly cached: string;
      readonly attempt: number;
    }) => Effect.Effect<number | void, PlatformError.PlatformError>;
  }) {
    const fs = yield* FileSystem.FileSystem;
    const path = yield* Path.Path;
    const repoRoot = yield* fs.makeTempDirectoryScoped({ prefix: "bibcode-build-symbols-run-" });
    yield* fs.makeDirectory(path.join(repoRoot, "apps", "desktop"), { recursive: true });
    yield* fs.writeFileString(
      path.join(repoRoot, "apps", "desktop", "package.json"),
      '{ "version": "1.2.3" }',
    );
    const host = {
      platform: ({ linux: "linux", mac: "darwin", win: "win32" } as const)[options.platform],
      arch: "x64",
    } as const;
    const input = {
      platform: options.platform,
      target: options.target,
      arch: "x64",
      outputDir: "out",
      symbols: true,
    } as const;
    const plan = yield* resolveTauriBuildPlan(input, {}, host, repoRoot);
    // Every fixture path below derives from the plan's binary, inside the scratch repository.
    if (plan.symbols === undefined) return yield* Effect.die("a --symbols plan has symbols");
    const { archive, binary } = plan.symbols;
    yield* fs.makeDirectory(plan.bundleDir, { recursive: true });
    yield* fs.writeFileString(path.join(plan.bundleDir, options.bundleFile), "bundle");
    const cached = path.join(path.dirname(binary), "deps", "bibcode_desktop-0123");
    yield* fs.makeDirectory(path.dirname(cached), { recursive: true });
    yield* fs.writeFile(cached, options.unstripped);
    yield* fs.link(cached, binary);
    const spawned: Array<{ step: string; env: Record<string, string | undefined> }> = [];
    let bundleAttempts = 0;
    const spawnerLayer = Layer.succeed(
      ChildProcessSpawner.ChildProcessSpawner,
      ChildProcessSpawner.make((command) => {
        if (command._tag !== "StandardCommand") {
          throw new Error("The artifact wrapper never pipes commands.");
        }
        const step =
          command.command === "strip"
            ? "strip"
            : command.args[0]?.endsWith("dump-symbols.ts")
              ? "dump"
              : command.args.includes("--appimage-extract")
                ? "extract"
                : (command.args.find((arg) => arg === "build" || arg === "bundle") ?? "unknown");
        spawned.push({ step, env: { ...command.options.env } });
        const done = Effect.as(processHandle(0));
        if (step === "dump") {
          return fs.writeFileString(archive, "zip").pipe(done, Effect.orDie);
        }
        if (step === "strip") {
          const output = command.args[command.args.indexOf("-o") + 1];
          if (!command.args.includes("-o") || output === undefined) {
            return Effect.die("strip writes a copy with -o");
          }
          return fs.writeFile(output, options.stripped).pipe(done, Effect.orDie);
        }
        if (step === "extract") {
          if (command.options.cwd === undefined) return Effect.die("extraction has a cwd");
          const root = path.join(command.options.cwd, "squashfs-root", "usr", "bin");
          return fs
            .makeDirectory(root, { recursive: true })
            .pipe(
              Effect.andThen(
                fs.writeFile(
                  path.join(root, "bibcode-desktop"),
                  options.extracted ?? options.stripped,
                ),
              ),
              done,
              Effect.orDie,
            );
        }
        if (step === "bundle" && options.onBundle) {
          bundleAttempts += 1;
          return options
            .onBundle({ bundleDir: plan.bundleDir, binary, cached, attempt: bundleAttempts })
            .pipe(
              Effect.map((exitCode) => processHandle(exitCode ?? 0)),
              Effect.orDie,
            );
        }
        return Effect.succeed(processHandle(0));
      }),
    );
    const result = yield* buildTauriDesktopArtifact(
      input,
      { PATH: "/usr/bin" },
      { write: () => undefined, host, repoRoot },
    ).pipe(Effect.provide(spawnerLayer), Effect.result);
    return { result, spawned, binary, cached, repoRoot };
  });

  /** Writers for what the bundlers leave behind, bound to the test's file system. */
  const bundlerOutputs = Effect.gen(function* () {
    const fs = yield* FileSystem.FileSystem;
    const path = yield* Path.Path;
    return {
      /** The app the DMG bundler builds (and keeps when `app` is requested), with its binary. */
      app: (binary: string, image: Buffer) => {
        const macos = path.join(path.dirname(binary), "bundle", "macos", "BiBCode.app", "Contents");
        return fs
          .makeDirectory(path.join(macos, "MacOS"), { recursive: true })
          .pipe(Effect.andThen(fs.writeFile(path.join(macos, "MacOS", "bibcode-desktop"), image)));
      },
      /** What Tauri's deb bundler leaves: the .deb and, beside it, its package tree. */
      deb: (bundleDir: string, base: string, binary: Buffer) => {
        const bin = path.join(bundleDir, base, "data", "usr", "bin");
        return fs
          .makeDirectory(bin, { recursive: true })
          .pipe(
            Effect.andThen(fs.writeFile(path.join(bin, "bibcode-desktop"), binary)),
            Effect.andThen(fs.writeFileString(path.join(bundleDir, `${base}.deb`), "deb")),
          );
      },
    };
  });

  it.effect("dumps symbols and strips before bundling, then publishes the symbol archive", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const run = yield* symbolsBuild({
        platform: "linux",
        target: "appimage",
        bundleFile: "BiBCode_1.2.3_amd64.AppImage",
        ...ELF,
      });

      assert.equal(run.result._tag, "Success");
      // After bundling, the AppImage is extracted to check the binary inside it.
      assert.deepStrictEqual(
        run.spawned.map((entry) => entry.step),
        ["build", "dump", "strip", "bundle", "extract"],
      );
      const [build, , , bundle] = run.spawned;
      assert.equal(build?.env.CARGO_PROFILE_RELEASE_STRIP, "none");
      assert.equal(build?.env.CARGO_PROFILE_RELEASE_DEBUG, "line-tables-only");
      assert.equal(build?.env.RUSTFLAGS, "-C link-arg=-Wl,--build-id=sha1");
      assert.equal(build?.env.PATH, "/usr/bin");
      // Bundling sees the same environment. Whether it ran Cargo is checked afterwards, not
      // assumed: Cargo re-links its unstripped artifact over the binary even when fresh.
      assert.deepStrictEqual(bundle?.env, build?.env);
      // The bundle gets the stripped binary, and the next build still finds an unstripped one.
      assert.deepStrictEqual(
        Buffer.from(yield* fs.readFile(run.binary)).equals(ELF.stripped),
        true,
      );
      assert.deepStrictEqual(
        Buffer.from(yield* fs.readFile(run.cached)).equals(ELF.unstripped),
        true,
      );
      assert.deepStrictEqual(run.result._tag === "Success" ? run.result.success : [], [
        path.join(run.repoRoot, "out", "BiBCode_1.2.3_amd64.AppImage"),
        path.join(run.repoRoot, "out", "bibcode-desktop-symbols-1.2.3-linux-x86_64.zip"),
      ]);
    }),
  );

  it.effect("accepts bundles whose copy of the binary is the stripped one", () =>
    Effect.gen(function* () {
      const outputs = yield* bundlerOutputs;
      // The deb's package tree keeps a copy; Tauri patches its bundle-type marker.
      const deb = yield* symbolsBuild({
        platform: "linux",
        target: "deb",
        bundleFile: "BiBCode_1.2.3_amd64.deb",
        ...ELF,
        onBundle: ({ bundleDir }) => outputs.deb(bundleDir, "BiBCode_1.2.3_amd64", ELF.stripped),
      });
      assert.equal(deb.result._tag, "Success");

      // The macOS app holds a re-signed copy, compared by LC_UUID and symbol count.
      const machO = {
        unstripped: syntheticMachO({ uuid: MACHO_UUID, symbols: 900 }),
        stripped: syntheticMachO({ uuid: MACHO_UUID, symbols: 3 }),
      };
      const mac = yield* symbolsBuild({
        platform: "mac",
        target: "dmg",
        bundleFile: "BiBCode_1.2.3_x64.dmg",
        ...machO,
        onBundle: ({ binary }) => outputs.app(binary, machO.stripped),
      });
      assert.equal(mac.result._tag, "Success");

      // A Windows installer embeds the target binary itself, which nothing strips.
      const win = yield* symbolsBuild({
        platform: "win",
        target: "nsis",
        bundleFile: "BiBCode_1.2.3_x64-setup.exe",
        unstripped: Buffer.from("MZ pe image"),
        stripped: Buffer.from("MZ pe image"),
      });
      assert.equal(win.result._tag, "Success");
      assert.notInclude(
        win.spawned.map((entry) => entry.step),
        "strip",
      );
    }),
  );

  it.effect("checks a retried bundle and only this version's deb tree", () =>
    Effect.gen(function* () {
      const outputs = yield* bundlerOutputs;
      const fs = yield* FileSystem.FileSystem;
      // A failed attempt leaves Tauri's bundle-type marker patched into the binary (the bundler
      // restores it only after a successful bundle), and the retry bundles what it finds.
      const retried = yield* symbolsBuild({
        platform: "linux",
        target: "appimage",
        bundleFile: "BiBCode_1.2.3_amd64.AppImage",
        ...ELF,
        onBundle: ({ binary, attempt }) =>
          Effect.gen(function* () {
            if (attempt > 1) return 0;
            const patched = Buffer.from(yield* fs.readFile(binary));
            patched.write("APP", patched.length - 3, "latin1");
            yield* fs.writeFile(binary, patched);
            return 1;
          }),
      });
      assert.equal(retried.result._tag, "Success");
      assert.isTrue(Buffer.from(yield* fs.readFile(retried.binary)).equals(ELF.stripped));

      // A tree that an older version's .deb left beside this one is not this build's.
      const rerun = yield* symbolsBuild({
        platform: "linux",
        target: "deb",
        bundleFile: "BiBCode_1.2.3_amd64.deb",
        ...ELF,
        onBundle: ({ bundleDir }) =>
          Effect.gen(function* () {
            const older = syntheticElf({ buildId: "a".repeat(40), symbols: 0 });
            yield* outputs.deb(bundleDir, "BiBCode_1.2.2_amd64", older);
            yield* outputs.deb(bundleDir, "BiBCode_1.2.3_amd64", ELF.stripped);
          }),
      });
      assert.equal(rerun.result._tag, "Success");
    }),
  );

  it.effect("never restores the stripped bytes through a binary Cargo re-linked", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      let attempts = 0;
      // A failed attempt during which Cargo re-linked its unstripped artifact over the binary.
      const relinked = yield* symbolsBuild({
        platform: "linux",
        target: "deb",
        bundleFile: "BiBCode_1.2.3_amd64.deb",
        ...ELF,
        onBundle: ({ binary, cached, attempt }) => {
          attempts = attempt;
          return fs.remove(binary).pipe(Effect.andThen(fs.link(cached, binary)), Effect.as(1));
        },
      });
      assert.equal(relinked.result._tag, "Failure");
      assert.include(
        relinked.result._tag === "Failure"
          ? String((relinked.result as unknown as { readonly failure: Error }).failure.message)
          : "",
        "changed or replaced",
      );
      assert.equal(attempts, 1, "no retry bundles a binary that is not the stripped file");
      assert.isTrue(
        Buffer.from(yield* fs.readFile(relinked.cached)).equals(ELF.unstripped),
        "Cargo's cached artifact keeps its symbols",
      );
    }),
  );

  it.effect("fails a symbols build whose bundles may not hold the stripped binary", () =>
    Effect.gen(function* () {
      const outputs = yield* bundlerOutputs;
      const fs = yield* FileSystem.FileSystem;
      const failure = (run: { readonly result: { readonly _tag: string } }) =>
        run.result._tag === "Failure"
          ? String((run.result as unknown as { readonly failure: Error }).failure.message)
          : "";

      // Cargo running inside `tauri bundle` re-links its unstripped artifact over the binary.
      const relinked = yield* symbolsBuild({
        platform: "linux",
        target: "deb",
        bundleFile: "BiBCode_1.2.3_amd64.deb",
        ...ELF,
        onBundle: ({ binary, cached }) =>
          fs.remove(binary).pipe(Effect.andThen(fs.link(cached, binary))),
      });
      assert.include(failure(relinked), "changed or replaced");

      // The same bytes, but another file that is linked twice: no longer the stripped file.
      const sameBytes = yield* symbolsBuild({
        platform: "linux",
        target: "deb",
        bundleFile: "BiBCode_1.2.3_amd64.deb",
        ...ELF,
        onBundle: ({ bundleDir, binary }) =>
          Effect.gen(function* () {
            yield* outputs.deb(bundleDir, "BiBCode_1.2.3_amd64", ELF.stripped);
            yield* fs.copyFile(binary, `${binary}.twin`);
            yield* fs.remove(binary);
            yield* fs.link(`${binary}.twin`, binary);
          }),
      });
      assert.include(failure(sameBytes), "changed or replaced");

      // A macOS app whose binary still has its symbols.
      const machO = {
        unstripped: syntheticMachO({ uuid: MACHO_UUID, symbols: 900 }),
        stripped: syntheticMachO({ uuid: MACHO_UUID, symbols: 3 }),
      };
      const unstrippedApp = yield* symbolsBuild({
        platform: "mac",
        target: "dmg",
        bundleFile: "BiBCode_1.2.3_x64.dmg",
        ...machO,
        onBundle: ({ binary }) => outputs.app(binary, machO.unstripped),
      });
      assert.include(failure(unstrippedApp), "does not match");

      // A bundle that carries another binary than the stripped one.
      const unstrippedCopy = yield* symbolsBuild({
        platform: "linux",
        target: "appimage",
        bundleFile: "BiBCode_1.2.3_amd64.AppImage",
        ...ELF,
        extracted: ELF.unstripped,
      });
      assert.include(failure(unstrippedCopy), "does not match");

      // A bundle layout without a readable copy cannot be checked.
      const noCopy = yield* symbolsBuild({
        platform: "linux",
        target: "deb",
        bundleFile: "BiBCode_1.2.3_amd64.deb",
        ...ELF,
      });
      assert.include(failure(noCopy), "Found no copy");
    }),
  );

  it("launches only when used as the CLI entrypoint", () => {
    const launched: unknown[] = [];
    assert.equal(
      runBuildTauriDesktopArtifactMain(false, [], (effect) => launched.push(effect)),
      false,
    );
    assert.equal(
      runBuildTauriDesktopArtifactMain(true, ["--skip-build"], (effect) => launched.push(effect)),
      true,
    );
    assert.equal(launched.length, 1);
  });
});
