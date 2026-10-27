#!/usr/bin/env node
import * as NodeOS from "node:os";
import * as NodeProcess from "node:process";
import * as NodeURL from "node:url";

import { runMsvc } from "./run-msvc.mjs";

export function tauriBuildEnvironment(platform, env) {
  if (platform !== "linux") {
    return env;
  }
  return { ...env, NO_STRIP: "1" };
}

export function runTauriBuild(options = {}) {
  // oxlint-disable-next-line coddy/no-global-process-runtime -- Standalone CLI samples the host platform once and still accepts an injected platform in tests.
  const platform = options.platform ?? NodeOS.platform();
  const env = options.env ?? NodeProcess.env;
  const args = options.args ?? NodeProcess.argv.slice(2);
  const runWithToolchain = options.runMsvc ?? runMsvc;
  // `bundle` packages what an earlier `tauri build --no-bundle` compiled; linuxdeploy runs here.
  const command = args[0] === "bundle" ? args : ["build", ...args];
  return runWithToolchain(["pnpm", "exec", "tauri", ...command], {
    env: tauriBuildEnvironment(platform, env),
  });
}

if (
  NodeProcess.argv[1] !== undefined &&
  import.meta.url === NodeURL.pathToFileURL(NodeProcess.argv[1]).href
) {
  NodeProcess.exit(runTauriBuild());
}
