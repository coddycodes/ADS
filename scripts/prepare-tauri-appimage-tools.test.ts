// @effect-diagnostics nodeBuiltinImport:off - Tests use real temporary filesystem behavior.
import * as NodeFS from "node:fs";
import * as NodeHttp from "node:http";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";

import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import {
  GTK_PLUGIN_PIN,
  prepareTauriAppImageTools,
  type UpstreamGtkPluginPin,
} from "./prepare-tauri-appimage-tools.ts";

vi.mock("node:fs", async (importOriginal) => {
  const actual = await importOriginal<typeof import("node:fs")>();
  return { ...actual, renameSync: vi.fn(actual.renameSync) };
});

const REPOSITORY_ROOT = NodePath.resolve(import.meta.dirname, "..");
const FIXTURE_PLUGIN = new TextEncoder().encode("#!/usr/bin/env bash\nexit 0\n");
const LEGACY_UPSTREAM_FILENAME = "linuxdeploy-plugin-gtk-upstream.sh";
const UPSTREAM_FILENAME = "coddy-studio-linuxdeploy-gtk-upstream.sh";
const FIXTURE_PIN = {
  url: "https://example.invalid/linuxdeploy-plugin-gtk.sh",
  sha256: "fb99eae951f1adc14d1a4a9a186c21930db2786b3208c94c7d9af382bd1048e5",
} as const satisfies UpstreamGtkPluginPin;
const temporaryDirectories: Array<string> = [];

function makeRepositoryRoot(): string {
  const root = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "coddy-studio-appimage-tools-"));
  temporaryDirectories.push(root);
  const wrapperDirectory = NodePath.join(root, "scripts/tauri");
  NodeFS.mkdirSync(wrapperDirectory, { recursive: true });
  NodeFS.copyFileSync(
    NodePath.join(REPOSITORY_ROOT, "scripts/tauri/linuxdeploy-plugin-gtk.sh"),
    NodePath.join(wrapperDirectory, "linuxdeploy-plugin-gtk.sh"),
  );
  return root;
}

function toolPath(root: string, name: string): string {
  return NodePath.join(root, "target/.tauri", name);
}

afterEach(() => {
  for (const directory of temporaryDirectories.splice(0)) {
    NodeFS.rmSync(directory, { recursive: true, force: true });
  }
});

describe("prepare Tauri AppImage tools", () => {
  it("pins the official plugin to an immutable URL and expected digest", () => {
    expect(GTK_PLUGIN_PIN).toEqual({
      url: "https://raw.githubusercontent.com/tauri-apps/linuxdeploy-plugin-gtk/b5eb8d05b4c0ed40107fe2158c5d8527f94568ef/linuxdeploy-plugin-gtk.sh",
      sha256: "cb379f9b0733e9ad9f8bd78f8c2fa038aef2478523bb7d4c8e64ff6a1ea3501a",
    });
  });

  it("does no filesystem or network work on non-Linux platforms", async () => {
    const repositoryRoot = makeRepositoryRoot();
    let downloads = 0;

    const result = await prepareTauriAppImageTools({
      repositoryRoot,
      platform: "darwin",
      plugin: FIXTURE_PIN,
      download: async () => {
        downloads += 1;
        return FIXTURE_PLUGIN;
      },
    });

    expect(result).toEqual({ status: "skipped", platform: "darwin" });
    expect(downloads).toBe(0);
    expect(NodeFS.existsSync(NodePath.join(repositoryRoot, "target"))).toBe(false);
  });

  it("downloads, verifies, and publishes both executable tools", async () => {
    const repositoryRoot = makeRepositoryRoot();
    let downloads = 0;

    const result = await prepareTauriAppImageTools({
      repositoryRoot,
      platform: "linux",
      plugin: FIXTURE_PIN,
      download: async (url) => {
        expect(url).toBe(FIXTURE_PIN.url);
        downloads += 1;
        return FIXTURE_PLUGIN;
      },
    });

    const upstreamPath = toolPath(repositoryRoot, UPSTREAM_FILENAME);
    const wrapperPath = toolPath(repositoryRoot, "linuxdeploy-plugin-gtk.sh");
    expect(result).toEqual({
      status: "prepared",
      toolsDirectory: NodePath.dirname(upstreamPath),
      usedCachedUpstream: false,
    });
    expect(downloads).toBe(1);
    expect(NodeFS.readFileSync(upstreamPath)).toEqual(Buffer.from(FIXTURE_PLUGIN));
    expect(NodeFS.readFileSync(wrapperPath)).toEqual(
      NodeFS.readFileSync(NodePath.join(repositoryRoot, "scripts/tauri/linuxdeploy-plugin-gtk.sh")),
    );
    expect(
      NodeFS.readdirSync(NodePath.dirname(upstreamPath)).filter((name) =>
        name.startsWith("linuxdeploy-plugin-"),
      ),
    ).toEqual(["linuxdeploy-plugin-gtk.sh"]);
    // oxlint-disable-next-line coddy/no-global-process-runtime -- These filesystem integration tests assert executable modes on the host platform.
    if (process.platform !== "win32") {
      expect(NodeFS.statSync(upstreamPath).mode & 0o111).not.toBe(0);
      expect(NodeFS.statSync(wrapperPath).mode & 0o111).not.toBe(0);
    }
  });

  it("uses the Node HTTP client for the default download adapter", async () => {
    const repositoryRoot = makeRepositoryRoot();
    const server = NodeHttp.createServer((_request, response) => {
      response.writeHead(200, { "content-type": "application/octet-stream" });
      response.end(FIXTURE_PLUGIN);
    });
    await new Promise<void>((resolve, reject) => {
      server.once("error", reject);
      server.listen(0, "127.0.0.1", () => {
        server.off("error", reject);
        resolve();
      });
    });
    const address = server.address();
    if (address === null || typeof address === "string") {
      throw new Error("test HTTP server did not expose a TCP address");
    }
    const originalFetch = globalThis.fetch;
    globalThis.fetch = async () => {
      throw new Error("global fetch must not be used");
    };

    try {
      const result = await prepareTauriAppImageTools({
        repositoryRoot,
        platform: "linux",
        plugin: {
          url: `http://127.0.0.1:${address.port}/linuxdeploy-plugin-gtk.sh`,
          sha256: FIXTURE_PIN.sha256,
        },
      });

      expect(result).toMatchObject({ status: "prepared", usedCachedUpstream: false });
      expect(NodeFS.readFileSync(toolPath(repositoryRoot, UPSTREAM_FILENAME))).toEqual(
        Buffer.from(FIXTURE_PLUGIN),
      );
    } finally {
      globalThis.fetch = originalFetch;
      await new Promise<void>((resolve, reject) => {
        server.close((error) => {
          if (error === undefined) resolve();
          else reject(error);
        });
      });
    }
  });

  it("rehashes and reuses a valid cached upstream plugin without downloading", async () => {
    const repositoryRoot = makeRepositoryRoot();
    const upstreamPath = toolPath(repositoryRoot, UPSTREAM_FILENAME);
    NodeFS.mkdirSync(NodePath.dirname(upstreamPath), { recursive: true });
    NodeFS.writeFileSync(upstreamPath, FIXTURE_PLUGIN, { mode: 0o644 });

    const result = await prepareTauriAppImageTools({
      repositoryRoot,
      platform: "linux",
      plugin: FIXTURE_PIN,
      download: async () => {
        throw new Error("download must not run for a valid cache");
      },
    });

    expect(result).toMatchObject({ status: "prepared", usedCachedUpstream: true });
    expect(NodeFS.readFileSync(upstreamPath)).toEqual(Buffer.from(FIXTURE_PLUGIN));
    // oxlint-disable-next-line coddy/no-global-process-runtime -- These filesystem integration tests assert executable modes on the host platform.
    if (process.platform !== "win32") {
      expect(NodeFS.statSync(upstreamPath).mode & 0o111).not.toBe(0);
    }
  });

  it("migrates a valid legacy cache offline and removes its discoverable plugin path", async () => {
    const repositoryRoot = makeRepositoryRoot();
    const legacyPath = toolPath(repositoryRoot, LEGACY_UPSTREAM_FILENAME);
    const upstreamPath = toolPath(repositoryRoot, UPSTREAM_FILENAME);
    NodeFS.mkdirSync(NodePath.dirname(legacyPath), { recursive: true });
    NodeFS.writeFileSync(legacyPath, FIXTURE_PLUGIN, { mode: 0o755 });

    const result = await prepareTauriAppImageTools({
      repositoryRoot,
      platform: "linux",
      plugin: FIXTURE_PIN,
      download: async () => {
        throw new Error("download must not run for a valid legacy cache");
      },
    });

    expect(result).toMatchObject({ status: "prepared", usedCachedUpstream: true });
    expect(NodeFS.existsSync(legacyPath)).toBe(false);
    expect(NodeFS.readFileSync(upstreamPath)).toEqual(Buffer.from(FIXTURE_PLUGIN));
    expect(
      NodeFS.readdirSync(NodePath.dirname(upstreamPath)).filter((name) =>
        name.startsWith("linuxdeploy-plugin-"),
      ),
    ).toEqual(["linuxdeploy-plugin-gtk.sh"]);
  });

  it("keeps a valid legacy cache when atomic migration publication fails", async () => {
    const repositoryRoot = makeRepositoryRoot();
    const toolsDirectory = NodePath.join(repositoryRoot, "target/.tauri");
    const legacyPath = toolPath(repositoryRoot, LEGACY_UPSTREAM_FILENAME);
    const upstreamPath = toolPath(repositoryRoot, UPSTREAM_FILENAME);
    NodeFS.mkdirSync(toolsDirectory, { recursive: true });
    NodeFS.writeFileSync(legacyPath, FIXTURE_PLUGIN, { mode: 0o755 });
    const rename = vi.mocked(NodeFS.renameSync);
    rename.mockImplementationOnce(() => {
      throw new Error("forced migration publication failure");
    });

    try {
      await expect(
        prepareTauriAppImageTools({
          repositoryRoot,
          platform: "linux",
          plugin: FIXTURE_PIN,
          download: async () => {
            throw new Error("download must not run for a valid legacy cache");
          },
        }),
      ).rejects.toMatchObject({
        boundary: "filesystem",
        targetPath: toolsDirectory,
      });
      expect(NodeFS.readFileSync(legacyPath)).toEqual(Buffer.from(FIXTURE_PLUGIN));
      expect(NodeFS.existsSync(upstreamPath)).toBe(false);
    } finally {
      rename.mockRestore();
    }
  });

  it("rejects corrupt downloaded bytes without publishing them", async () => {
    const repositoryRoot = makeRepositoryRoot();
    const upstreamPath = toolPath(repositoryRoot, UPSTREAM_FILENAME);

    await expect(
      prepareTauriAppImageTools({
        repositoryRoot,
        platform: "linux",
        plugin: FIXTURE_PIN,
        download: async () => new TextEncoder().encode("corrupt"),
      }),
    ).rejects.toMatchObject({
      boundary: "integrity",
      targetPath: upstreamPath,
    });
    expect(NodeFS.existsSync(upstreamPath)).toBe(false);
  });

  it("removes the legacy plugin before rejecting a corrupt replacement", async () => {
    const repositoryRoot = makeRepositoryRoot();
    const legacyPath = toolPath(repositoryRoot, LEGACY_UPSTREAM_FILENAME);
    const upstreamPath = toolPath(repositoryRoot, UPSTREAM_FILENAME);
    NodeFS.mkdirSync(NodePath.dirname(legacyPath), { recursive: true });
    NodeFS.writeFileSync(legacyPath, "invalid legacy bytes", { mode: 0o755 });

    await expect(
      prepareTauriAppImageTools({
        repositoryRoot,
        platform: "linux",
        plugin: FIXTURE_PIN,
        download: async () => new TextEncoder().encode("corrupt"),
      }),
    ).rejects.toMatchObject({
      boundary: "integrity",
      targetPath: upstreamPath,
    });
    expect(NodeFS.existsSync(legacyPath)).toBe(false);
    expect(NodeFS.existsSync(upstreamPath)).toBe(false);
  });

  it("reports download failures at the network boundary", async () => {
    const repositoryRoot = makeRepositoryRoot();

    await expect(
      prepareTauriAppImageTools({
        repositoryRoot,
        platform: "linux",
        plugin: FIXTURE_PIN,
        download: async () => {
          throw new Error("offline");
        },
      }),
    ).rejects.toMatchObject({
      boundary: "download",
      targetPath: FIXTURE_PIN.url,
    });
  });

  it("reports local cache publication failures at the filesystem boundary", async () => {
    const repositoryRoot = makeRepositoryRoot();
    const toolsDirectory = NodePath.join(repositoryRoot, "target/.tauri");
    NodeFS.mkdirSync(NodePath.dirname(toolsDirectory), { recursive: true });
    NodeFS.writeFileSync(toolsDirectory, "blocks directory creation");

    await expect(
      prepareTauriAppImageTools({
        repositoryRoot,
        platform: "linux",
        plugin: FIXTURE_PIN,
        download: async () => FIXTURE_PLUGIN,
      }),
    ).rejects.toMatchObject({
      boundary: "filesystem",
      targetPath: toolsDirectory,
    });
  });

  it("replaces an invalid cached plugin only after a valid download", async () => {
    const repositoryRoot = makeRepositoryRoot();
    const upstreamPath = toolPath(repositoryRoot, UPSTREAM_FILENAME);
    NodeFS.mkdirSync(NodePath.dirname(upstreamPath), { recursive: true });
    NodeFS.writeFileSync(upstreamPath, "invalid cached bytes");

    const result = await prepareTauriAppImageTools({
      repositoryRoot,
      platform: "linux",
      plugin: FIXTURE_PIN,
      download: async () => FIXTURE_PLUGIN,
    });

    expect(result).toMatchObject({ status: "prepared", usedCachedUpstream: false });
    expect(NodeFS.readFileSync(upstreamPath)).toEqual(Buffer.from(FIXTURE_PLUGIN));
  });
});
