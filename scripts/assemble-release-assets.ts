#!/usr/bin/env node
// @effect-diagnostics nodeBuiltinImport:off - This standalone assembler owns release filesystem and signing process boundaries.
// @effect-diagnostics globalConsole:off - The CLI reports bounded assembly errors and output metadata.
import * as NodeChildProcess from "node:child_process";
import * as NodeCrypto from "node:crypto";
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";
import * as NodeURL from "node:url";
import * as NodeUtil from "node:util";
import * as Schema from "effect/Schema";

import { RELEASE_TARGETS, TAURI_UPDATE_TARGETS, symbolArchiveName } from "./lib/release-targets.ts";

const UpdaterDescriptorSchema = Schema.Struct({
  target: Schema.Literals(TAURI_UPDATE_TARGETS),
  artifact: Schema.String,
  signature: Schema.String,
});
const decodeUpdaterDescriptor = Schema.decodeUnknownSync(UpdaterDescriptorSchema);

export interface ReleaseAssetAssemblyInput {
  readonly assetsDir: string;
  readonly version: string;
  readonly updater: boolean;
  readonly serverSigningKey?: string;
  readonly serverSigningPublicKey?: string;
  readonly stepSummaryPath?: string;
}

export interface ReleaseAssetAssemblyResult {
  readonly checksumsPath: string;
  readonly signed: boolean;
  readonly publishedAssets: ReadonlyArray<string>;
}

export interface ServerSigningInput {
  readonly privateKey: string | undefined;
  readonly publicKey: string | undefined;
}

export interface ServerSigningConfiguration {
  readonly privateKey: string;
  readonly publicKey: string;
}

export class ReleaseAssetAssemblyError extends Error {
  override readonly name = "ReleaseAssetAssemblyError";
}

export function expectedServerAssetNames(version: string): ReadonlyArray<string> {
  return [
    `coddy-studio-server-v${version}-darwin-aarch64.tar.gz`,
    `coddy-studio-server-v${version}-darwin-x86_64.tar.gz`,
    `coddy-studio-server-v${version}-linux-aarch64.tar.gz`,
    `coddy-studio-server-v${version}-linux-x86_64.tar.gz`,
    `coddy-studio-server-v${version}-windows-aarch64.zip`,
    `coddy-studio-server-v${version}-windows-x86_64.zip`,
    `coddy-studio-server_${version}_amd64.deb`,
    `coddy-studio-server_${version}_arm64.deb`,
    `coddy-studio-server-${version}-1.aarch64.rpm`,
    `coddy-studio-server-${version}-1.x86_64.rpm`,
  ];
}

/** The desktop and server Breakpad symbol archives, one of each per release target. */
export function expectedSymbolAssetNames(version: string): ReadonlyArray<string> {
  return (["desktop", "server"] as const).flatMap((family) =>
    RELEASE_TARGETS.map((target) => symbolArchiveName(family, version, target)),
  );
}

export function serverSigningPlan(
  input: ServerSigningInput,
): ServerSigningConfiguration | undefined {
  if (input.privateKey === undefined && input.publicKey === undefined) return undefined;
  if (input.privateKey === undefined || input.publicKey === undefined) {
    throw new ReleaseAssetAssemblyError(
      "Server signing requires both private and public keys, or neither.",
    );
  }
  return { privateKey: input.privateKey, publicKey: input.publicKey };
}

function escapeRegex(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function requireOne(entries: ReadonlyArray<string>, pattern: RegExp, label: string): string {
  const matches = entries.filter((entry) => pattern.test(entry));
  if (matches.length !== 1) {
    throw new ReleaseAssetAssemblyError(`Expected exactly one ${label}, found ${matches.length}.`);
  }
  return matches[0]!;
}

function expectedDesktopAssets(entries: ReadonlyArray<string>, version: string): Set<string> {
  const escaped = escapeRegex(version);
  return new Set([
    requireOne(
      entries,
      new RegExp(`^CoddyStudio_${escaped}_(?:aarch64|arm64)\\.dmg$`),
      "macOS ARM64 DMG",
    ),
    requireOne(entries, new RegExp(`^CoddyStudio_${escaped}_(?:x64|x86_64)\\.dmg$`), "macOS x64 DMG"),
    requireOne(
      entries,
      new RegExp(`^CoddyStudio_${escaped}_(?:aarch64|arm64)\\.AppImage$`),
      "Linux ARM64 AppImage",
    ),
    requireOne(
      entries,
      new RegExp(`^CoddyStudio_${escaped}_(?:amd64|x64|x86_64)\\.AppImage$`),
      "Linux x64 AppImage",
    ),
    requireOne(entries, new RegExp(`^CoddyStudio_${escaped}_arm64-setup\\.exe$`), "Windows ARM64 NSIS"),
    requireOne(entries, new RegExp(`^CoddyStudio_${escaped}_x64-setup\\.exe$`), "Windows x64 NSIS"),
  ]);
}

async function sha256(path: string): Promise<string> {
  return new Promise((resolve, reject) => {
    const hash = NodeCrypto.createHash("sha256");
    const stream = NodeFS.createReadStream(path);
    stream.on("error", reject);
    stream.on("data", (chunk) => hash.update(chunk));
    stream.on("end", () => resolve(hash.digest("hex")));
  });
}

function runMinisign(configuration: ServerSigningConfiguration, assetPath: string): void {
  const signaturePath = `${assetPath}.minisig`;
  const sign = NodeChildProcess.spawnSync(
    "minisign",
    ["-S", "-s", configuration.privateKey, "-m", assetPath, "-x", signaturePath],
    { shell: false, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] },
  );
  if (sign.error) throw sign.error;
  if (sign.status !== 0) {
    throw new ReleaseAssetAssemblyError(
      `Minisign failed for ${NodePath.basename(assetPath)}: ${String(sign.stderr).trim()}`,
    );
  }
  const verify = NodeChildProcess.spawnSync(
    "minisign",
    ["-V", "-p", configuration.publicKey, "-m", assetPath, "-x", signaturePath],
    { shell: false, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] },
  );
  if (verify.error) throw verify.error;
  if (verify.status !== 0) {
    throw new ReleaseAssetAssemblyError(
      `Minisign verification failed for ${NodePath.basename(assetPath)}: ${String(verify.stderr).trim()}`,
    );
  }
}

export async function assembleReleaseAssets(
  input: ReleaseAssetAssemblyInput,
): Promise<ReleaseAssetAssemblyResult> {
  const assetsDir = NodePath.resolve(input.assetsDir);
  const initialEntries = NodeFS.readdirSync(assetsDir).toSorted();
  const serverAssets = expectedServerAssetNames(input.version);
  for (const asset of serverAssets) {
    if (!initialEntries.includes(asset)) {
      throw new ReleaseAssetAssemblyError(`Missing server release asset ${asset}.`);
    }
  }
  const symbolAssets = expectedSymbolAssetNames(input.version);
  for (const asset of symbolAssets) {
    if (!initialEntries.includes(asset)) {
      throw new ReleaseAssetAssemblyError(`Missing symbol release asset ${asset}.`);
    }
  }
  const desktopAssets = expectedDesktopAssets(initialEntries, input.version);
  const descriptorNames = new Set(TAURI_UPDATE_TARGETS.map((target) => `updater-${target}.json`));
  const updaterArtifacts = new Set<string>();
  if (input.updater) {
    if (!initialEntries.includes("latest.json")) {
      throw new ReleaseAssetAssemblyError("Stable updater assembly requires latest.json.");
    }
    for (const descriptor of descriptorNames) {
      if (!initialEntries.includes(descriptor)) {
        throw new ReleaseAssetAssemblyError(`Missing internal updater descriptor ${descriptor}.`);
      }
      const decoded = decodeUpdaterDescriptor(
        JSON.parse(NodeFS.readFileSync(NodePath.join(assetsDir, descriptor), "utf8")),
      );
      if (`updater-${decoded.target}.json` !== descriptor) {
        throw new ReleaseAssetAssemblyError(`Updater descriptor target mismatch in ${descriptor}.`);
      }
      if (decoded.signature !== `${decoded.artifact}.sig`) {
        throw new ReleaseAssetAssemblyError(`Updater signature mismatch in ${descriptor}.`);
      }
      for (const asset of [decoded.artifact, decoded.signature]) {
        if (!initialEntries.includes(asset)) {
          throw new ReleaseAssetAssemblyError(`Missing updater asset ${asset}.`);
        }
        updaterArtifacts.add(asset);
      }
    }
  }

  const allowed = new Set([...serverAssets, ...symbolAssets, ...desktopAssets]);
  for (const updaterArtifact of updaterArtifacts) allowed.add(updaterArtifact);
  if (input.updater) allowed.add("latest.json");
  for (const descriptor of descriptorNames) {
    if (input.updater) allowed.add(descriptor);
  }
  for (const entry of initialEntries) {
    if (allowed.has(entry)) {
      continue;
    }
    throw new ReleaseAssetAssemblyError(`Unexpected release asset ${entry}.`);
  }

  const checksumLines: string[] = [];
  for (const asset of [...serverAssets].toSorted()) {
    checksumLines.push(`${await sha256(NodePath.join(assetsDir, asset))}  ${asset}`);
  }
  const checksumsPath = NodePath.join(assetsDir, "coddy-studio-server-SHA256SUMS");
  const temporaryChecksums = `${checksumsPath}.${process.pid}.tmp`;
  await NodeFS.promises.writeFile(temporaryChecksums, `${checksumLines.join("\n")}\n`, {
    flag: "wx",
    mode: 0o644,
  });
  await NodeFS.promises.rename(temporaryChecksums, checksumsPath);

  const signing = serverSigningPlan({
    privateKey: input.serverSigningKey,
    publicKey: input.serverSigningPublicKey,
  });
  if (signing !== undefined) {
    for (const asset of [...serverAssets, "coddy-studio-server-SHA256SUMS"]) {
      runMinisign(signing, NodePath.join(assetsDir, asset));
    }
  } else if (input.stepSummaryPath !== undefined) {
    await NodeFS.promises.appendFile(
      input.stepSummaryPath,
      "Standalone server assets are unsigned; SHA-256 checksums were generated.\n",
    );
  }

  if (input.updater) {
    for (const descriptor of descriptorNames) {
      await NodeFS.promises.rm(NodePath.join(assetsDir, descriptor));
    }
  }
  return {
    checksumsPath,
    signed: signing !== undefined,
    publishedAssets: NodeFS.readdirSync(assetsDir).toSorted(),
  };
}

function parseArguments(argv: ReadonlyArray<string>): ReleaseAssetAssemblyInput {
  const { values } = NodeUtil.parseArgs({
    args: [...argv],
    allowPositionals: false,
    strict: true,
    options: {
      "assets-dir": { type: "string" },
      version: { type: "string" },
      updater: { type: "boolean" },
      "server-signing-key": { type: "string" },
      "server-signing-public-key": { type: "string" },
      "step-summary": { type: "string" },
    },
  });
  if (typeof values["assets-dir"] !== "string" || typeof values.version !== "string") {
    throw new ReleaseAssetAssemblyError("--assets-dir and --version are required.");
  }
  return {
    assetsDir: values["assets-dir"],
    version: values.version,
    updater: values.updater === true,
    ...(typeof values["server-signing-key"] === "string"
      ? { serverSigningKey: values["server-signing-key"] }
      : {}),
    ...(typeof values["server-signing-public-key"] === "string"
      ? { serverSigningPublicKey: values["server-signing-public-key"] }
      : {}),
    ...(typeof values["step-summary"] === "string"
      ? { stepSummaryPath: values["step-summary"] }
      : {}),
  };
}

if (
  process.argv[1] !== undefined &&
  import.meta.url === NodeURL.pathToFileURL(process.argv[1]).href
) {
  assembleReleaseAssets(parseArguments(process.argv.slice(2)))
    .then((result) => console.log(JSON.stringify(result)))
    .catch((error: unknown) => {
      console.error(error instanceof Error ? error.message : error);
      process.exitCode = 1;
    });
}
