#!/usr/bin/env node
// @effect-diagnostics nodeBuiltinImport:off - This standalone tool installer verifies and publishes native release tooling.
import * as NodeChildProcess from "node:child_process";
import * as NodeCrypto from "node:crypto";
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";
import * as NodeHttpClient from "@effect/platform-node/NodeHttpClient";
import * as Effect from "effect/Effect";
import { HttpClient, HttpClientResponse } from "effect/http";

import type { ReleaseArch } from "./lib/release-targets.ts";

const VERSION = "2.47.0";
const DOWNLOAD_ROOT = `https://github.com/goreleaser/nfpm/releases/download/v${VERSION}`;

const PINS = {
  arm64: {
    asset: `nfpm_${VERSION}_Linux_arm64.tar.gz`,
    sha256: "1c0f5f2999b9a974bfb04fdb0cc3306096de530ac5dbb25d739cc5f5219c919c",
  },
  x64: {
    asset: `nfpm_${VERSION}_Linux_x86_64.tar.gz`,
    sha256: "0660ca602b2d2d2ae4781a06c692b3eeb9d437ffea05b831d76e41f4a3188783",
  },
} as const;

export const NFPM_PIN = { version: VERSION, assets: PINS } as const;

export interface NfpmInstallPlan {
  readonly version: string;
  readonly asset: string;
  readonly sha256: string;
  readonly url: string;
  readonly executablePath: string;
}

export interface NfpmInstallRuntime {
  readonly download?: (url: string) => Promise<Uint8Array>;
  readonly extract?: (archivePath: string, directory: string) => Promise<void>;
}

export class NfpmInstallationError extends Error {
  override readonly name = "NfpmInstallationError";
}

export function planNfpmInstall(
  platform: NodeJS.Platform,
  arch: ReleaseArch,
  repositoryRoot: string,
): NfpmInstallPlan {
  if (platform !== "linux") {
    throw new NfpmInstallationError(`nFPM installation requires Linux, received ${platform}.`);
  }
  const pin = PINS[arch];
  return {
    version: VERSION,
    asset: pin.asset,
    sha256: pin.sha256,
    url: `${DOWNLOAD_ROOT}/${pin.asset}`,
    executablePath: NodePath.join(repositoryRoot, "target", "tools", "nfpm", VERSION, arch, "nfpm"),
  };
}

async function download(url: string): Promise<Uint8Array> {
  return Effect.runPromise(
    Effect.gen(function* () {
      const client = (yield* HttpClient.HttpClient).pipe(HttpClient.followRedirects(5));
      const response = yield* client
        .get(url)
        .pipe(Effect.flatMap(HttpClientResponse.filterStatusOk));
      return new Uint8Array(yield* response.arrayBuffer);
    }).pipe(
      Effect.mapError((cause) => new NfpmInstallationError(`nFPM download failed: ${cause}`)),
      Effect.provide(NodeHttpClient.layerNodeHttp),
    ),
  );
}

async function extract(archivePath: string, directory: string): Promise<void> {
  const result = NodeChildProcess.spawnSync("tar", ["-xzf", archivePath, "-C", directory], {
    shell: false,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new NfpmInstallationError(`nFPM extraction failed: ${String(result.stderr).trim()}`);
  }
}

export async function installNfpm(
  plan: NfpmInstallPlan,
  runtime: NfpmInstallRuntime = {},
): Promise<string> {
  if (NodeFS.existsSync(plan.executablePath)) return plan.executablePath;

  const bytes = await (runtime.download ?? download)(plan.url);
  const checksum = NodeCrypto.createHash("sha256").update(bytes).digest("hex");
  if (checksum !== plan.sha256) {
    throw new NfpmInstallationError(
      `nFPM checksum mismatch for ${plan.asset}: expected ${plan.sha256}, received ${checksum}.`,
    );
  }

  const parent = NodePath.dirname(plan.executablePath);
  await NodeFS.promises.mkdir(parent, { recursive: true });
  const temporary = await NodeFS.promises.mkdtemp(NodePath.join(parent, ".install-"));
  try {
    const archivePath = NodePath.join(temporary, plan.asset);
    const extractionDirectory = NodePath.join(temporary, "extracted");
    await NodeFS.promises.writeFile(archivePath, bytes, { mode: 0o600 });
    await NodeFS.promises.mkdir(extractionDirectory);
    await (runtime.extract ?? extract)(archivePath, extractionDirectory);
    const extracted = NodePath.join(extractionDirectory, "nfpm");
    if (!NodeFS.existsSync(extracted) || !NodeFS.statSync(extracted).isFile()) {
      throw new NfpmInstallationError(`nFPM archive ${plan.asset} did not contain nfpm.`);
    }
    await NodeFS.promises.copyFile(extracted, plan.executablePath, NodeFS.constants.COPYFILE_EXCL);
    await NodeFS.promises.chmod(plan.executablePath, 0o755);
    return plan.executablePath;
  } finally {
    await NodeFS.promises.rm(temporary, { recursive: true, force: true });
  }
}
