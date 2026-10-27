#!/usr/bin/env node

import * as NodeChildProcess from "node:child_process";
import * as NodeCrypto from "node:crypto";
import * as NodeFSP from "node:fs/promises";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import * as NodeTimersPromises from "node:timers/promises";
import * as NodeURL from "node:url";

export const LOCK_STALE_MS = 10 * 60 * 1000;
export const LOCK_POLL_MS = 250;

const repoRoot = NodePath.resolve(NodePath.dirname(NodeURL.fileURLToPath(import.meta.url)), "..");
const webDirectory = NodePath.join(repoRoot, "apps", "web");
const lockDigest = NodeCrypto.createHash("sha1").update(webDirectory).digest("hex");
const lockDirectory = NodePath.join(NodeOS.tmpdir(), `coddy-studio-web-build-${lockDigest}.lock`);
const lockOwnerFile = NodePath.join(lockDirectory, "owner.txt");

export function isErrorCode(error, code) {
  return typeof error === "object" && error !== null && "code" in error && error.code === code;
}

export async function removeStaleLock(options = {}) {
  const directory = options.lockDirectory ?? lockDirectory;
  const stat = options.stat ?? NodeFSP.stat;
  const rm = options.rm ?? NodeFSP.rm;
  const now = options.now ?? Date.now;
  try {
    const stats = await stat(directory);
    if (now() - stats.mtimeMs < LOCK_STALE_MS) return;
    await rm(directory, { force: true, recursive: true });
  } catch (error) {
    if (!isErrorCode(error, "ENOENT")) throw error;
  }
}

export async function acquireBuildLock(options = {}) {
  const directory = options.lockDirectory ?? lockDirectory;
  const ownerFile = options.lockOwnerFile ?? lockOwnerFile;
  const directoryForBuild = options.webDirectory ?? webDirectory;
  const mkdir = options.mkdir ?? NodeFSP.mkdir;
  const writeFile = options.writeFile ?? NodeFSP.writeFile;
  const sleep = options.sleep ?? NodeTimersPromises.setTimeout;
  const removeStale =
    options.removeStaleLock ?? (() => removeStaleLock({ lockDirectory: directory }));
  for (;;) {
    try {
      await mkdir(directory);
      await writeFile(
        ownerFile,
        `pid=${options.pid ?? process.pid}\ncreatedAt=${(options.createdAt ?? new Date()).toISOString()}\nwebDirectory=${directoryForBuild}\n`,
      );
      return;
    } catch (error) {
      if (!isErrorCode(error, "EEXIST")) throw error;
      await removeStale();
      await sleep(LOCK_POLL_MS);
    }
  }
}

export function runWebBuild(options = {}) {
  const spawn = options.spawn ?? NodeChildProcess.spawn;
  const environment = options.env ?? process.env;
  const directory = options.webDirectory ?? webDirectory;
  return new Promise((resolve, reject) => {
    const isWindows = environment.OS === "Windows_NT" || environment.ComSpec !== undefined;
    const child = isWindows
      ? spawn("vp build", { cwd: directory, shell: true, stdio: "inherit" })
      : spawn("vp", ["build"], {
          cwd: directory,
          shell: false,
          stdio: "inherit",
        });
    child.on("error", reject);
    child.on("close", (code) => resolve(code ?? 1));
  });
}

export async function runWebBuildLocked(options = {}) {
  const directory = options.lockDirectory ?? lockDirectory;
  const rm = options.rm ?? NodeFSP.rm;
  await (options.acquireBuildLock ?? acquireBuildLock)(options);
  try {
    return await (options.runWebBuild ?? runWebBuild)(options);
  } finally {
    await rm(directory, { force: true, recursive: true });
  }
}

if (
  process.argv[1] !== undefined &&
  import.meta.url === NodeURL.pathToFileURL(process.argv[1]).href
) {
  process.exitCode = await runWebBuildLocked();
}
