#!/usr/bin/env node
// @effect-diagnostics nodeBuiltinImport:off - This standalone smoke owns a native process and local HTTP boundary.
// @effect-diagnostics globalConsole:off - The CLI reports only bounded non-secret results.
// @effect-diagnostics globalTimers:off - The standalone smoke owns bounded process and HTTP timeouts.
import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodeHttp from "node:http";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import * as NodeReadline from "node:readline";
import * as NodeURL from "node:url";
import * as NodeUtil from "node:util";

export interface ServerDistributionSmokeInput {
  readonly binary: string;
  readonly expectedVersion: string;
  readonly timeoutMs?: number;
}

export interface ServerDistributionSmokeResult {
  readonly version: string;
  readonly environmentId: string;
  readonly tokenType: string;
  readonly webContainsCoddyStudio: boolean;
  readonly exitCode: number;
}

export class ServerDistributionSmokeError extends Error {
  override readonly name = "ServerDistributionSmokeError";
}

interface HttpResult {
  readonly status: number;
  readonly body: string;
}

function request(
  url: string,
  options: {
    readonly method?: string;
    readonly body?: string;
    readonly headers?: Readonly<Record<string, string>>;
  } = {},
): Promise<HttpResult> {
  return new Promise((resolve, reject) => {
    const target = new URL(url);
    const httpRequest = NodeHttp.request(
      target,
      {
        method: options.method ?? "GET",
        headers: options.headers,
      },
      (response) => {
        let body = "";
        response.setEncoding("utf8");
        response.on("data", (chunk: string) => {
          if (body.length < 1_048_576) body += chunk;
        });
        response.on("end", () => resolve({ status: response.statusCode ?? 0, body }));
      },
    );
    httpRequest.on("error", reject);
    if (options.body !== undefined) httpRequest.write(options.body);
    httpRequest.end();
  });
}

function withTimeout<A>(promise: Promise<A>, timeoutMs: number, description: string): Promise<A> {
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(
      () => reject(new ServerDistributionSmokeError(`${description} timed out.`)),
      timeoutMs,
    );
    promise.then(
      (value) => {
        clearTimeout(timeout);
        resolve(value);
      },
      (error: unknown) => {
        clearTimeout(timeout);
        reject(error);
      },
    );
  });
}

function parseJson(text: string, description: string): Record<string, unknown> {
  try {
    const value: unknown = JSON.parse(text);
    if (value !== null && typeof value === "object" && !Array.isArray(value)) {
      return value as Record<string, unknown>;
    }
  } catch {}
  throw new ServerDistributionSmokeError(`${description} was not a JSON object.`);
}

export function isExpectedServerShutdownExit(platform: NodeJS.Platform, exitCode: number): boolean {
  // Node implements SIGTERM through forced process termination on Windows,
  // where the observed child exit status is 1 rather than a graceful POSIX 0.
  return exitCode === 0 || (platform === "win32" && exitCode === 1);
}

export async function smokeServerDistribution(
  input: ServerDistributionSmokeInput,
): Promise<ServerDistributionSmokeResult> {
  const binary = NodePath.resolve(input.binary);
  const timeoutMs = input.timeoutMs ?? 30_000;
  if (!NodeFS.existsSync(binary) || !NodeFS.statSync(binary).isFile()) {
    throw new ServerDistributionSmokeError(`Server binary is missing: ${binary}`);
  }
  if (!NodeFS.existsSync(NodePath.join(NodePath.dirname(binary), "web/index.html"))) {
    throw new ServerDistributionSmokeError("Staged server is missing web/index.html.");
  }

  const versionResult = NodeChildProcess.spawnSync(binary, ["--version"], {
    shell: false,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  if (versionResult.error) throw versionResult.error;
  const version = String(versionResult.stdout).trim().split(/\s+/).at(-1) ?? "";
  if (versionResult.status !== 0 || version !== input.expectedVersion) {
    throw new ServerDistributionSmokeError(
      `Expected server version ${input.expectedVersion}, received ${version || "<empty>"}.`,
    );
  }

  const stateRoot = await NodeFS.promises.mkdtemp(
    NodePath.join(NodeOS.tmpdir(), "coddy-studio-distribution-smoke-"),
  );
  const child = NodeChildProcess.spawn(
    binary,
    ["serve", "--host", "127.0.0.1", "--port", "0", "--base-dir", stateRoot],
    { shell: false, stdio: ["ignore", "pipe", "pipe"] },
  );
  let stderr = "";
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (chunk: string) => {
    if (stderr.length < 16_384) stderr += chunk;
  });

  try {
    const lines = NodeReadline.createInterface({ input: child.stdout });
    const readinessLine = await withTimeout(
      new Promise<string>((resolve, reject) => {
        lines.once("line", resolve);
        child.once("error", reject);
        child.once("exit", (code) =>
          reject(
            new ServerDistributionSmokeError(
              `Server exited before readiness with ${code ?? 1}: ${stderr.trim()}`,
            ),
          ),
        );
      }),
      timeoutMs,
      "Server readiness",
    );
    lines.close();
    const readiness = parseJson(readinessLine, "Server readiness");
    const httpBaseUrl = readiness.httpBaseUrl;
    if (typeof httpBaseUrl !== "string") {
      throw new ServerDistributionSmokeError("Server readiness omitted httpBaseUrl.");
    }

    const web = await withTimeout(request(`${httpBaseUrl}/`), timeoutMs, "Packaged web request");
    if (web.status !== 200) {
      throw new ServerDistributionSmokeError(`Packaged web request returned ${web.status}.`);
    }
    const descriptorResponse = await withTimeout(
      request(`${httpBaseUrl}/.well-known/coddy-studio/environment`),
      timeoutMs,
      "Environment descriptor request",
    );
    if (descriptorResponse.status !== 200) {
      throw new ServerDistributionSmokeError(
        `Environment descriptor returned ${descriptorResponse.status}.`,
      );
    }
    const descriptor = parseJson(descriptorResponse.body, "Environment descriptor");
    if (typeof descriptor.environmentId !== "string") {
      throw new ServerDistributionSmokeError("Environment descriptor omitted environmentId.");
    }

    const pairingResult = NodeChildProcess.spawnSync(
      binary,
      ["pairing", "issue", "--base-dir", stateRoot, "--label", "Distribution smoke", "--json"],
      { shell: false, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] },
    );
    if (pairingResult.error) throw pairingResult.error;
    if (pairingResult.status !== 0) {
      throw new ServerDistributionSmokeError("Pairing command failed.");
    }
    const pairing = parseJson(String(pairingResult.stdout).trim(), "Pairing response");
    if (typeof pairing.credential !== "string") {
      throw new ServerDistributionSmokeError("Pairing response omitted credential.");
    }
    const form = new URLSearchParams({
      grant_type: "urn:ietf:params:oauth:grant-type:token-exchange",
      subject_token: pairing.credential,
      subject_token_type: "urn:coddy-studio:params:oauth:token-type:environment-bootstrap",
      requested_token_type: "urn:ietf:params:oauth:token-type:access_token",
      client_label: "Distribution smoke",
      client_device_type: "desktop",
    }).toString();
    const tokenResponse = await withTimeout(
      request(`${httpBaseUrl}/oauth/token`, {
        method: "POST",
        body: form,
        headers: {
          "content-type": "application/x-www-form-urlencoded",
          "content-length": String(Buffer.byteLength(form)),
        },
      }),
      timeoutMs,
      "Pairing token exchange",
    );
    if (tokenResponse.status !== 200) {
      throw new ServerDistributionSmokeError(
        `Pairing token exchange returned ${tokenResponse.status}.`,
      );
    }
    const token = parseJson(tokenResponse.body, "Pairing token response");
    if (typeof token.access_token !== "string" || typeof token.token_type !== "string") {
      throw new ServerDistributionSmokeError("Pairing token response was incomplete.");
    }

    if (!child.kill("SIGTERM")) {
      throw new ServerDistributionSmokeError("Could not request server shutdown.");
    }
    const exitCode = await withTimeout(
      new Promise<number>((resolve, reject) => {
        child.once("error", reject);
        child.once("exit", (code) => resolve(code ?? 1));
      }),
      timeoutMs,
      "Server shutdown",
    );
    // oxlint-disable-next-line coddy/no-global-process-runtime -- This native smoke validates the host process it just terminated.
    if (!isExpectedServerShutdownExit(NodeOS.platform(), exitCode)) {
      throw new ServerDistributionSmokeError(`Server shutdown exited with ${exitCode}.`);
    }
    return {
      version,
      environmentId: descriptor.environmentId,
      tokenType: token.token_type,
      webContainsCoddyStudio: web.body.includes("Coddy Studio"),
      exitCode,
    };
  } finally {
    if (child.exitCode === null) child.kill("SIGKILL");
    await NodeFS.promises.rm(stateRoot, { recursive: true, force: true });
  }
}

function parseArguments(argv: ReadonlyArray<string>): ServerDistributionSmokeInput {
  const { values } = NodeUtil.parseArgs({
    args: [...argv],
    allowPositionals: false,
    strict: true,
    options: {
      binary: { type: "string" },
      "expected-version": { type: "string" },
      "timeout-ms": { type: "string" },
    },
  });
  if (typeof values.binary !== "string" || typeof values["expected-version"] !== "string") {
    throw new ServerDistributionSmokeError("--binary and --expected-version are required.");
  }
  const timeoutMs =
    typeof values["timeout-ms"] === "string" ? Number(values["timeout-ms"]) : undefined;
  if (timeoutMs !== undefined && (!Number.isSafeInteger(timeoutMs) || timeoutMs <= 0)) {
    throw new ServerDistributionSmokeError("--timeout-ms must be a positive integer.");
  }
  return {
    binary: values.binary,
    expectedVersion: values["expected-version"],
    ...(timeoutMs === undefined ? {} : { timeoutMs }),
  };
}

if (
  process.argv[1] !== undefined &&
  import.meta.url === NodeURL.pathToFileURL(process.argv[1]).href
) {
  smokeServerDistribution(parseArguments(process.argv.slice(2)))
    .then((result) => console.log(JSON.stringify(result)))
    .catch((error: unknown) => {
      console.error(error instanceof Error ? error.message : error);
      process.exitCode = 1;
    });
}
