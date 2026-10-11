import * as NodeCrypto from "node:crypto";

import * as NodeServices from "@effect/platform-node/NodeServices";
import { assert, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Path from "effect/Path";
import * as Schema from "effect/Schema";

import { decodeRgbaPng, type DecodedRgbaPng } from "./lib/png-rgba.ts";

const TauriConfiguration = Schema.fromJsonString(
  Schema.Struct({
    identifier: Schema.String,
    build: Schema.Struct({ beforeBuildCommand: Schema.String }),
    app: Schema.Struct({
      withGlobalTauri: Schema.Boolean,
      security: Schema.Struct({
        csp: Schema.NullOr(Schema.String),
        devCsp: Schema.optionalKey(Schema.NullOr(Schema.String)),
      }),
    }),
    bundle: Schema.Struct({
      useLocalToolsDir: Schema.optionalKey(Schema.Boolean),
      icon: Schema.Array(Schema.String),
      macOS: Schema.Struct({
        minimumSystemVersion: Schema.String,
        signingIdentity: Schema.optionalKey(Schema.String),
      }),
      resources: Schema.optionalKey(Schema.Record(Schema.String, Schema.String)),
    }),
  }),
);
const CapabilityConfiguration = Schema.fromJsonString(
  Schema.Struct({ permissions: Schema.Array(Schema.String) }),
);
const PlatformToolsTauriConfiguration = Schema.fromJsonString(
  Schema.Struct({
    bundle: Schema.Struct({
      useLocalToolsDir: Schema.Boolean,
      windows: Schema.optionalKey(
        Schema.Struct({
          nsis: Schema.Struct({ installerHooks: Schema.String }),
        }),
      ),
    }),
  }),
);
const DesktopPackageConfiguration = Schema.fromJsonString(
  Schema.Struct({ scripts: Schema.Struct({ build: Schema.String }) }),
);
const UpdaterConfiguration = Schema.Struct({
  pubkey: Schema.String,
  endpoints: Schema.Array(Schema.String),
  windows: Schema.optionalKey(Schema.Struct({ installMode: Schema.String })),
});
const BaseUpdaterConfiguration = Schema.fromJsonString(
  Schema.Struct({
    bundle: Schema.Struct({
      createUpdaterArtifacts: Schema.optionalKey(Schema.Boolean),
    }),
    plugins: Schema.Struct({ updater: UpdaterConfiguration }),
  }),
);
const ReleaseUpdaterConfiguration = Schema.fromJsonString(
  Schema.Struct({
    bundle: Schema.Struct({ createUpdaterArtifacts: Schema.Boolean }),
    plugins: Schema.Struct({ updater: UpdaterConfiguration }),
  }),
);
const decodeTauriConfiguration = Schema.decodeUnknownEffect(TauriConfiguration);
const decodePlatformToolsTauriConfiguration = Schema.decodeUnknownEffect(
  PlatformToolsTauriConfiguration,
);
const decodeCapabilityConfiguration = Schema.decodeUnknownEffect(CapabilityConfiguration);
const decodeDesktopPackageConfiguration = Schema.decodeUnknownEffect(DesktopPackageConfiguration);
const decodeBaseUpdaterConfiguration = Schema.decodeUnknownEffect(BaseUpdaterConfiguration);
const decodeReleaseUpdaterConfiguration = Schema.decodeUnknownEffect(ReleaseUpdaterConfiguration);

function topRowOpaqueBounds(image: DecodedRgbaPng): readonly [number, number] {
  const opaqueColumns: Array<number> = [];
  for (let x = 0; x < image.width; x++) {
    if (image.pixels[x * 4 + 3]! >= 128) opaqueColumns.push(x);
  }
  assert.ok(opaqueColumns.length > 0, "macOS icon top row must contain opaque pixels");
  return [opaqueColumns[0]!, opaqueColumns.at(-1)!];
}

function rgbChannelSha256(image: DecodedRgbaPng): string {
  const rgb = Buffer.alloc(image.width * image.height * 3);
  for (let sourceOffset = 0, rgbOffset = 0; sourceOffset < image.pixels.length;) {
    rgb[rgbOffset++] = image.pixels[sourceOffset++]!;
    rgb[rgbOffset++] = image.pixels[sourceOffset++]!;
    rgb[rgbOffset++] = image.pixels[sourceOffset++]!;
    sourceOffset++;
  }
  return NodeCrypto.createHash("sha256").update(rgb).digest("hex");
}

function readIcnsChunks(icns: Uint8Array): ReadonlyMap<string, Uint8Array> {
  const bytes = Buffer.from(icns.buffer, icns.byteOffset, icns.byteLength);
  assert.equal(bytes.toString("ascii", 0, 4), "icns");
  assert.equal(bytes.readUInt32BE(4), bytes.length);
  const chunks = new Map<string, Uint8Array>();
  for (let offset = 8; offset < bytes.length;) {
    const type = bytes.toString("ascii", offset, offset + 4);
    const size = bytes.readUInt32BE(offset + 4);
    assert.ok(size >= 8 && offset + size <= bytes.length, `Invalid ICNS ${type} chunk`);
    chunks.set(type, bytes.subarray(offset + 8, offset + size));
    offset += size;
  }
  return chunks;
}

it.layer(NodeServices.layer)("Tauri production hardening", (it) => {
  it.effect("uses the proven full-black macOS enclosure geometry", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const repoRoot = yield* path.fromFileUrl(new URL("..", import.meta.url));
      const source = decodeRgbaPng(
        yield* fs.readFile(path.join(repoRoot, "assets/prod/black-macos-1024.png")),
      );

      assert.equal(source.width, 1024);
      assert.equal(source.height, 1024);
      assert.deepEqual(topRowOpaqueBounds(source), [171, 852]);
      assert.equal(
        rgbChannelSha256(source),
        "8b6e2020b1bf741409862203340d693ad2926bba7e26020f7bf8c239de1eb42b",
        "macOS icon RGB values must preserve the pre-mask BiBCode artwork",
      );
      assert.equal(source.pixels[(512 * source.width + 512) * 4 + 3], 255);
      assert.ok(
        Array.from(
          { length: source.width * source.height },
          (_, index) => source.pixels[index * 4 + 3]!,
        ).filter((alpha) => alpha === 0).length >= 27_000,
        "macOS icon must retain the proven transparent corner area",
      );
      assert.ok(
        Array.from(
          { length: source.width * source.height },
          (_, index) => source.pixels[index * 4 + 3]!,
        ).some((alpha) => alpha > 0 && alpha < 255),
        "macOS icon corners must retain antialiasing",
      );

      const chunks = readIcnsChunks(
        yield* fs.readFile(path.join(repoRoot, "assets/prod/bibcode-black-macos.icns")),
      );
      for (const type of ["ic11", "ic12", "ic13", "ic07", "ic08", "ic14", "ic09", "ic10"]) {
        assert.equal(chunks.has(type), true, `ICNS must contain ${type}`);
      }
      const largest = decodeRgbaPng(chunks.get("ic10")!);
      assert.equal(largest.width, 1024);
      assert.equal(largest.height, 1024);
      assert.deepEqual(topRowOpaqueBounds(largest), [171, 852]);
      assert.equal(
        rgbChannelSha256(largest),
        "8b6e2020b1bf741409862203340d693ad2926bba7e26020f7bf8c239de1eb42b",
        "ICNS must preserve the pre-mask BiBCode RGB artwork",
      );
    }),
  );

  it.effect("caches bundler tools inside the repository target for Linux and Windows builds", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const repoRoot = yield* path.fromFileUrl(new URL("..", import.meta.url));
      const tauri = yield* decodeTauriConfiguration(
        yield* fs.readFileString(path.join(repoRoot, "apps/desktop/src-tauri/tauri.conf.json")),
      );
      const linux = yield* decodePlatformToolsTauriConfiguration(
        yield* fs.readFileString(
          path.join(repoRoot, "apps/desktop/src-tauri/tauri.linux.conf.json"),
        ),
      );
      const windows = yield* decodePlatformToolsTauriConfiguration(
        yield* fs.readFileString(
          path.join(repoRoot, "apps/desktop/src-tauri/tauri.windows.conf.json"),
        ),
      );

      assert.equal(tauri.bundle.useLocalToolsDir, undefined);
      assert.equal(linux.bundle.useLocalToolsDir, true);
      // Windows keeps NSIS under target/.tauri instead of %LOCALAPPDATA%, so an
      // ARM64 build never depends on a per-account cache that x86 filesystem
      // redirection can make unreachable for the NSIS bootstrapper.
      assert.equal(windows.bundle.useLocalToolsDir, true);
      assert.equal(windows.bundle.windows?.nsis.installerHooks, "./windows/installer-hooks.nsh");
      const installerHook = yield* fs.readFileString(
        path.join(repoRoot, "apps/desktop/src-tauri/windows/installer-hooks.nsh"),
      );
      assert.match(installerHook, /!macro NSIS_HOOK_PREINSTALL/);
      assert.match(
        installerHook,
        /\$PassiveMode = 1[\s\S]*\$\{Silent\}[\s\S]*\$\{FileExists\} "\$INSTDIR\\\$\{MAINBINARYNAME\}\.exe"[\s\S]*KillProcessCurrentUser "\$\{MAINBINARYNAME\}\.exe"/,
      );
      assert.match(installerHook, /Delete "\$INSTDIR\\\$\{MAINBINARYNAME\}\.exe"/);
      assert.equal(installerHook.includes("MessageBox"), false);
      assert.equal(installerHook.includes("Abort"), false);
      assert.equal(
        tauri.build.beforeBuildCommand,
        "node ../../scripts/prepare-tauri-appimage-tools.ts && vp run --filter @bibcode/web build && node ../../scripts/apply-web-brand-assets.ts production apps/web/dist",
      );
      assert.equal(
        yield* fs.exists(path.join(repoRoot, "scripts/prepare-tauri-appimage-tools.ts")),
        true,
      );
    }),
  );

  it.effect("keeps updater I/O release-only", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const repoRoot = yield* path.fromFileUrl(new URL("..", import.meta.url));
      const base = yield* decodeBaseUpdaterConfiguration(
        yield* fs.readFileString(path.join(repoRoot, "apps/desktop/src-tauri/tauri.conf.json")),
      );
      const release = yield* decodeReleaseUpdaterConfiguration(
        yield* fs.readFileString(
          path.join(repoRoot, "apps/desktop/src-tauri/tauri.release.conf.json"),
        ),
      );

      assert.equal(base.bundle.createUpdaterArtifacts, undefined);
      assert.equal(base.plugins.updater.pubkey, "");
      assert.deepEqual(base.plugins.updater.endpoints, []);
      assert.equal(release.bundle.createUpdaterArtifacts, true);
      assert.match(
        Buffer.from(release.plugins.updater.pubkey, "base64").toString("utf8"),
        /minisign public key/,
      );
      assert.deepEqual(release.plugins.updater.endpoints, [
        "https://github.com/mubeda/BibCode/releases/latest/download/latest.json",
      ]);
      assert.equal(release.plugins.updater.windows?.installMode, "passive");
    }),
  );

  it.effect("bundles only canonical black desktop icons", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const repoRoot = yield* path.fromFileUrl(new URL("..", import.meta.url));
      const tauri = yield* decodeTauriConfiguration(
        yield* fs.readFileString(path.join(repoRoot, "apps/desktop/src-tauri/tauri.conf.json")),
      );
      const expectedIcons = [
        "../../../assets/prod/black-universal-1024.png",
        "../../../assets/prod/bibcode-black-windows.ico",
        "../../../assets/prod/bibcode-black-macos.icns",
      ];

      assert.deepEqual(tauri.bundle.icon, expectedIcons);
      assert.equal(tauri.bundle.macOS.minimumSystemVersion, "11.0");
      for (const iconPath of [
        "assets/prod/black-universal-1024.png",
        "assets/prod/bibcode-black-windows.ico",
        "assets/prod/bibcode-black-macos.icns",
      ]) {
        assert.equal(yield* fs.exists(path.join(repoRoot, iconPath)), true, iconPath);
      }
      const linuxIcon = yield* fs.readFile(
        path.join(repoRoot, "assets/prod/black-universal-1024.png"),
      );
      assert.equal(linuxIcon[25], 6, "Linux desktop icon must use the RGBA PNG color type");
      assert.equal(yield* fs.exists(path.join(repoRoot, "apps/desktop/resources")), false);
    }),
  );

  it.effect("ad-hoc signs macOS application bundles without an Apple identity", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const repoRoot = yield* path.fromFileUrl(new URL("..", import.meta.url));
      const tauri = yield* decodeTauriConfiguration(
        yield* fs.readFileString(path.join(repoRoot, "apps/desktop/src-tauri/tauri.conf.json")),
      );

      assert.equal(tauri.bundle.macOS.signingIdentity, "-");
    }),
  );

  it.effect("applies the production black web icons before bundling the desktop app", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const repoRoot = yield* path.fromFileUrl(new URL("..", import.meta.url));
      const tauri = yield* decodeTauriConfiguration(
        yield* fs.readFileString(path.join(repoRoot, "apps/desktop/src-tauri/tauri.conf.json")),
      );

      assert.match(
        tauri.build.beforeBuildCommand,
        /apply-web-brand-assets\.ts production apps\/web\/dist/,
      );
      assert.equal(
        yield* fs.exists(path.join(repoRoot, "assets/prod/bibcode-black-web-apple-touch-180.png")),
        true,
      );
    }),
  );

  it.effect("keeps only canonical black product-icon assets", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const repoRoot = yield* path.fromFileUrl(new URL("..", import.meta.url));

      for (const legacyPath of ["assets/dev", "assets/nightly"]) {
        assert.equal(
          yield* fs.exists(path.join(repoRoot, legacyPath)),
          false,
          `${legacyPath} must be absent`,
        );
      }

      const publicCopies = [
        [
          "assets/prod/bibcode-black-web-favicon.ico",
          "apps/web/public/favicon.ico",
          "apps/marketing/public/favicon.ico",
        ],
        [
          "assets/prod/bibcode-black-web-favicon-16x16.png",
          "apps/web/public/favicon-16x16.png",
          "apps/marketing/public/favicon-16x16.png",
        ],
        [
          "assets/prod/bibcode-black-web-favicon-32x32.png",
          "apps/web/public/favicon-32x32.png",
          "apps/marketing/public/favicon-32x32.png",
        ],
        [
          "assets/prod/bibcode-black-web-apple-touch-180.png",
          "apps/web/public/apple-touch-icon.png",
          "apps/marketing/public/apple-touch-icon.png",
        ],
      ] as const;

      for (const [sourcePath, ...copyPaths] of publicCopies) {
        const source = yield* fs.readFile(path.join(repoRoot, sourcePath));
        for (const copyPath of copyPaths) {
          assert.deepEqual(
            yield* fs.readFile(path.join(repoRoot, copyPath)),
            source,
            `${copyPath} must match ${sourcePath}`,
          );
        }
      }
    }),
  );

  it.effect("restricts the main WebView and disables production source maps by default", () =>
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const repoRoot = yield* path.fromFileUrl(new URL("..", import.meta.url));
      const tauri = yield* decodeTauriConfiguration(
        yield* fs.readFileString(path.join(repoRoot, "apps/desktop/src-tauri/tauri.conf.json")),
      );
      const capability = yield* decodeCapabilityConfiguration(
        yield* fs.readFileString(
          path.join(repoRoot, "apps/desktop/src-tauri/capabilities/default.json"),
        ),
      );
      const viteConfig = yield* fs.readFileString(path.join(repoRoot, "apps/web/vite.config.ts"));
      const rootPackage = yield* fs.readFileString(path.join(repoRoot, "package.json"));
      const workspace = yield* fs.readFileString(path.join(repoRoot, "pnpm-workspace.yaml"));
      const desktopPackage = yield* fs.readFileString(
        path.join(repoRoot, "apps/desktop/package.json"),
      );
      const desktopLib = yield* fs.readFileString(
        path.join(repoRoot, "apps/desktop/src-tauri/src/lib.rs"),
      );

      for (const obsoletePath of [
        "apps/server-rust",
        "apps/desktop-tauri",
        "packages/effect-acp",
        "packages/effect-codex-app-server",
        "packages/native-command-runner",
        "packages/native-process-diagnostics",
        "packages/ssh",
        "packages/tailscale",
        "scripts/prepare-tauri-node-runtime.ts",
      ]) {
        assert.equal(
          yield* fs.exists(path.join(repoRoot, obsoletePath)),
          false,
          `${obsoletePath} must be absent`,
        );
      }

      assert.equal(tauri.app.withGlobalTauri, false);
      assert.notMatch(tauri.identifier, /\.app$/i);
      assert.equal(
        /prepare-tauri-node-runtime|server\//.test(tauri.build.beforeBuildCommand),
        false,
      );
      assert.equal(tauri.bundle.resources, undefined);
      assert.notEqual(tauri.app.security.csp, null);
      assert.match(tauri.app.security.csp ?? "", /default-src 'self'/);
      assert.match(tauri.app.security.csp ?? "", /object-src 'none'/);
      assert.match(tauri.app.security.csp ?? "", /frame-ancestors 'none'/);
      assert.notEqual(tauri.app.security.devCsp, null);
      // Remote servers are user-chosen plain-HTTP endpoints on a LAN or
      // tailnet, reached from the webview with fetch and a ws:// encrypted
      // channel. A connect-src that admits only loopback and https/wss makes
      // every Add Server attempt fail as "Server unreachable" before a packet
      // leaves the machine.
      for (const policy of [tauri.app.security.csp, tauri.app.security.devCsp]) {
        const connectSource = /connect-src ([^;]*)/.exec(policy ?? "")?.[1] ?? "";
        assert.match(connectSource, /(^|\s)http:(\s|$)/, "connect-src must allow off-host http:");
        assert.match(connectSource, /(^|\s)ws:(\s|$)/, "connect-src must allow off-host ws:");
      }
      assert.match(tauri.app.security.csp ?? "", /script-src 'self';/);
      // macOS App Transport Security refuses plain-HTTP loads from web content
      // unless the bundle declares the exception. Remote servers are
      // user-chosen plain-HTTP endpoints reached from the webview, so the
      // desktop bundle must carry the web-content exemption (and a Local
      // Network usage description for the macOS permission prompt), or every
      // Add Server attempt on macOS fails as "Server unreachable".
      const macPlist = yield* fs.readFileString(
        path.join(repoRoot, "apps/desktop/src-tauri/Info.plist"),
      );
      assert.match(
        macPlist,
        /<key>NSAppTransportSecurity<\/key>\s*<dict>[\s\S]*?<key>NSAllowsArbitraryLoadsInWebContent<\/key>\s*<true\/>/,
      );
      assert.match(macPlist, /<key>NSAllowsLocalNetworking<\/key>\s*<true\/>/);
      assert.match(
        macPlist,
        /<key>NSLocalNetworkUsageDescription<\/key>\s*<string>[^<]+<\/string>/,
      );
      assert.notMatch(macPlist, /NSAllowsArbitraryLoads<\/key>\s*<true/);
      assert.deepEqual(capability.permissions, [
        "allow-desktop-bridge",
        "allow-desktop-preview",
        "core:default",
        "deep-link:default",
      ]);
      assert.match(viteConfig, /tanstackRouter\(\{[\s\S]*?autoCodeSplitting: true,/);
      assert.match(viteConfig, /chunkSizeWarningLimit: 1536,/);
      assert.match(
        viteConfig,
        /const buildSourcemap:[\s\S]*?sourcemapEnv === "hidden"[\s\S]*?sourcemapEnv === "true";/,
      );
      assert.equal(/electron|electron-builder|@clerk\/electron/i.test(rootPackage), false);
      assert.equal(
        /effect-acp|effect-codex-app-server|node-pty|ffi-rs|fff-node/i.test(workspace),
        false,
      );
      assert.equal(
        /resources\/node|server-node_modules|dist\/bin\.mjs/i.test(desktopPackage),
        false,
      );
      const desktopPackageJson = yield* decodeDesktopPackageConfiguration(desktopPackage);
      assert.equal(desktopPackageJson.scripts.build, "node ../../scripts/run-tauri-build.mjs");
      assert.equal(yield* fs.exists(path.join(repoRoot, "scripts/run-tauri-build.mjs")), true);
      assert.notMatch(desktopPackage, /pnpm dlx/);
      assert.notMatch(desktopLib, /if\s*!cfg!\(debug_assertions\)[\s\S]*?backend\.start_default/);
      assert.match(
        desktopLib,
        /\.run_exclusive\(backend\.start_default\(app_handle\)\)[\s\S]*?\.await/,
      );
    }),
  );
});
