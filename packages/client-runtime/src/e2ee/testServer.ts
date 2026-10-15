// Test-only: runs a real `bibcode serve` for the opt-in cross-language suites. Absent from the
// package exports; set BIBCODE_E2EE_SERVER_BIN to a freshly built server binary to enable them.
// @effect-diagnostics nodeBuiltinImport:off
// @effect-diagnostics globalTimers:off - The process harness owns bounded watchdogs.
import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import * as NodeReadline from "node:readline";

import { expect } from "@effect/vitest";

export const serverBinary = process.env["BIBCODE_E2EE_SERVER_BIN"];
export interface RunningServer {
  process: NodeChildProcess.ChildProcess;
  httpBaseUrl: string;
  token: string;
  dataRoot: string;
  adminAccessToken?: string;
}

export async function startServer(): Promise<RunningServer> {
  const dataRoot = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "bibcode-e2ee-"));
  const child = NodeChildProcess.spawn(serverBinary!, [
    "serve",
    "--host",
    "127.0.0.1",
    "--port",
    "0",
    "--base-dir",
    dataRoot,
    "--no-browser",
  ]);
  const stderr: string[] = [];
  child.stderr?.on("data", (chunk) => stderr.push(String(chunk)));
  const startup = await new Promise<{ httpBaseUrl: string; token: string }>((resolve, reject) => {
    const timer = setTimeout(
      () => reject(new Error(`server did not report readiness: ${stderr.join("")}`)),
      30_000,
    );
    let ready = false;
    NodeReadline.createInterface({ input: child.stdout! }).on("line", (line) => {
      try {
        const parsed = JSON.parse(line) as { httpBaseUrl?: string; token?: string };
        if (parsed.httpBaseUrl && parsed.token) {
          ready = true;
          clearTimeout(timer);
          resolve({ httpBaseUrl: parsed.httpBaseUrl, token: parsed.token });
        }
      } catch {
        // Ignore non-JSON log lines.
      }
    });
    child.on("exit", (code) => {
      if (!ready) {
        clearTimeout(timer);
        reject(new Error(`server exited early (${String(code)}): ${stderr.join("")}`));
      }
    });
  });
  return { process: child, dataRoot, ...startup };
}

export async function stopServer(server: RunningServer): Promise<void> {
  if (server.process.exitCode === null) {
    server.process.kill();
    await Promise.race([
      new Promise<void>((resolve) => server.process.once("exit", () => resolve())),
      new Promise<void>((resolve) => setTimeout(resolve, 5_000)),
    ]);
  }
  if (server.process.exitCode === null) server.process.kill("SIGKILL");
  NodeFS.rmSync(server.dataRoot, { recursive: true, force: true });
}

export function readHostPublicKey(dataRoot: string): Uint8Array {
  const record = NodeFS.readFileSync(
    NodePath.join(dataRoot, "userdata", "secrets", "host-identity-x25519.bin"),
  );
  expect(record).toHaveLength(64);
  return Uint8Array.from(record.subarray(32));
}
