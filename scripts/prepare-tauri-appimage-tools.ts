#!/usr/bin/env node

// @effect-diagnostics nodeBuiltinImport:off - This standalone build adapter prepares Tauri tools.
import * as NodeHttpClient from "@effect/platform-node/NodeHttpClient";
import { Effect } from "effect";
import { HttpClient, HttpClientResponse } from "effect/http";
import * as NodeCrypto from "node:crypto";
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";
import * as NodeURL from "node:url";

export interface UpstreamGtkPluginPin {
  readonly url: string;
  readonly sha256: string;
}

export const GTK_PLUGIN_PIN = {
  url: "https://raw.githubusercontent.com/tauri-apps/linuxdeploy-plugin-gtk/b5eb8d05b4c0ed40107fe2158c5d8527f94568ef/linuxdeploy-plugin-gtk.sh",
  sha256: "cb379f9b0733e9ad9f8bd78f8c2fa038aef2478523bb7d4c8e64ff6a1ea3501a",
} as const satisfies UpstreamGtkPluginPin;

export type TauriAppImageToolPreparationBoundary = "download" | "integrity" | "filesystem";

export class TauriAppImageToolPreparationError extends Error {
  readonly _tag = "TauriAppImageToolPreparationError";
  readonly boundary: TauriAppImageToolPreparationBoundary;
  readonly targetPath: string;

  constructor(boundary: TauriAppImageToolPreparationBoundary, targetPath: string, cause?: unknown) {
    super(
      `Tauri AppImage tool preparation failed at ${boundary}: ${targetPath}`,
      cause === undefined ? undefined : { cause },
    );
    this.name = "TauriAppImageToolPreparationError";
    this.boundary = boundary;
    this.targetPath = targetPath;
  }
}

export interface PrepareTauriAppImageToolsOptions {
  readonly repositoryRoot?: string;
  readonly platform?: NodeJS.Platform;
  readonly download?: (url: string) => Promise<Uint8Array>;
  readonly plugin?: UpstreamGtkPluginPin;
}

export type PrepareTauriAppImageToolsResult =
  | {
      readonly status: "skipped";
      readonly platform: NodeJS.Platform;
    }
  | {
      readonly status: "prepared";
      readonly toolsDirectory: string;
      readonly usedCachedUpstream: boolean;
    };

function sha256(bytes: Uint8Array): string {
  return NodeCrypto.createHash("sha256").update(bytes).digest("hex");
}

function readFileIfPresent(path: string): Buffer | undefined {
  try {
    return NodeFS.readFileSync(path);
  } catch (error) {
    if (error !== null && typeof error === "object" && "code" in error && error.code === "ENOENT") {
      return undefined;
    }
    throw error;
  }
}

function publishExecutableAtomically(targetPath: string, bytes: Uint8Array): void {
  const temporaryPath = `${targetPath}.${process.pid}.${NodeCrypto.randomUUID()}.tmp`;
  try {
    NodeFS.writeFileSync(temporaryPath, bytes, { flag: "wx", mode: 0o755 });
    NodeFS.chmodSync(temporaryPath, 0o755);
    NodeFS.renameSync(temporaryPath, targetPath);
  } finally {
    NodeFS.rmSync(temporaryPath, { force: true });
  }
}

async function downloadBytes(url: string): Promise<Uint8Array> {
  return Effect.runPromise(
    HttpClient.get(url).pipe(
      Effect.flatMap(HttpClientResponse.filterStatusOk),
      Effect.flatMap((response) => response.arrayBuffer),
      Effect.map((buffer) => new Uint8Array(buffer)),
      Effect.provide(NodeHttpClient.layerNodeHttp),
    ),
  );
}

export async function prepareTauriAppImageTools(
  options: PrepareTauriAppImageToolsOptions = {},
): Promise<PrepareTauriAppImageToolsResult> {
  // oxlint-disable-next-line bibcode/no-global-process-runtime -- Standalone CLI samples the actual host platform when not injected.
  const platform = options.platform ?? process.platform;
  if (platform !== "linux") {
    return { status: "skipped", platform };
  }

  const repositoryRoot =
    options.repositoryRoot ??
    NodePath.resolve(NodePath.dirname(NodeURL.fileURLToPath(import.meta.url)), "..");
  const toolsDirectory = NodePath.join(repositoryRoot, "target/.tauri");
  const upstreamPath = NodePath.join(toolsDirectory, "bibcode-linuxdeploy-gtk-upstream.sh");
  const legacyUpstreamPath = NodePath.join(toolsDirectory, "linuxdeploy-plugin-gtk-upstream.sh");
  const wrapperPath = NodePath.join(toolsDirectory, "linuxdeploy-plugin-gtk.sh");
  const wrapperSourcePath = NodePath.join(
    repositoryRoot,
    "scripts/tauri/linuxdeploy-plugin-gtk.sh",
  );
  const plugin = options.plugin ?? GTK_PLUGIN_PIN;

  try {
    NodeFS.mkdirSync(toolsDirectory, { recursive: true });
    const cachedBytes = readFileIfPresent(upstreamPath);
    const legacyCachedBytes = readFileIfPresent(legacyUpstreamPath);
    const cachedUpstreamIsValid =
      cachedBytes !== undefined && sha256(cachedBytes) === plugin.sha256;
    const legacyUpstreamIsValid =
      legacyCachedBytes !== undefined && sha256(legacyCachedBytes) === plugin.sha256;
    const usedCachedUpstream = cachedUpstreamIsValid || legacyUpstreamIsValid;

    if (cachedUpstreamIsValid) {
      NodeFS.rmSync(legacyUpstreamPath, { force: true });
      NodeFS.chmodSync(upstreamPath, 0o755);
    } else if (legacyUpstreamIsValid) {
      publishExecutableAtomically(upstreamPath, legacyCachedBytes);
      NodeFS.rmSync(legacyUpstreamPath, { force: true });
    } else {
      NodeFS.rmSync(legacyUpstreamPath, { force: true });
      let downloadedBytes: Uint8Array;
      try {
        downloadedBytes = await (options.download ?? downloadBytes)(plugin.url);
      } catch (error) {
        throw new TauriAppImageToolPreparationError("download", plugin.url, error);
      }
      if (sha256(downloadedBytes) !== plugin.sha256) {
        throw new TauriAppImageToolPreparationError("integrity", upstreamPath);
      }
      publishExecutableAtomically(upstreamPath, downloadedBytes);
    }

    publishExecutableAtomically(wrapperPath, NodeFS.readFileSync(wrapperSourcePath));
    return { status: "prepared", toolsDirectory, usedCachedUpstream };
  } catch (error) {
    if (error instanceof TauriAppImageToolPreparationError) {
      throw error;
    }
    throw new TauriAppImageToolPreparationError("filesystem", toolsDirectory, error);
  }
}

export async function runPrepareTauriAppImageToolsMain(isMain: boolean): Promise<boolean> {
  if (!isMain) return false;
  await prepareTauriAppImageTools();
  return true;
}

if (import.meta.main) {
  await runPrepareTauriAppImageToolsMain(true);
}
