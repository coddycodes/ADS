// @effect-diagnostics nodeBuiltinImport:off - Native fixture tests execute generated host shims.
// @effect-diagnostics globalDate:off - Native protocol polling runs outside an Effect runtime.
// @effect-diagnostics globalTimers:off - Native protocol polling runs outside an Effect runtime.
// @effect-diagnostics globalFetch:off - The test probes its generated local OpenCode HTTP server.
import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodeNet from "node:net";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";

import { afterEach, describe, expect, it } from "vite-plus/test";

import { readProviderInputLog } from "./provider-input-log.ts";
import {
  archiveAndCleanupDesktopUiTestContext,
  prepareDesktopUiTestContext,
  type DesktopUiTestContext,
} from "./test-project.ts";

interface PreparedProviderFixture {
  readonly context: DesktopUiTestContext;
  readonly environment: NodeJS.ProcessEnv;
  readonly hostPlatform: ReturnType<typeof NodeOS.platform>;
  readonly launchers: Record<ConfiguredProvider, string>;
}

type ConfiguredProvider = "codex" | "claudeAgent" | "cursor" | "opencode" | "grok";

type FixtureProcessTerminationPlan =
  | {
      readonly kind: "windows-tree";
      readonly command: string;
      readonly args: readonly ["/PID", string, "/T", "/F"];
    }
  | {
      readonly kind: "posix-child";
      readonly pid: number;
      readonly signals: readonly ["SIGTERM", "SIGKILL"];
    };

const preparedFixtures: PreparedProviderFixture[] = [];
const gracefulShutdownTimeoutMs = 1_000;
const forcedShutdownTimeoutMs = 5_000;

function prepareProviderFixture(): PreparedProviderFixture {
  // oxlint-disable-next-line bibcode/no-global-process-runtime -- These integration tests execute shims for the current native host.
  const hostPlatform = NodeOS.platform();
  const runRoot = NodeFS.mkdtempSync(
    NodePath.join(NodeOS.tmpdir(), "bibcode provider launcher root "),
  );
  const environment: NodeJS.ProcessEnv = {
    ...process.env,
    BIBCODE_E2E_RUN_ROOT: runRoot,
    BIBCODE_E2E_ARTIFACT_DIR: NodePath.join(runRoot, "artifacts"),
    BIBCODE_E2E_PLATFORM:
      hostPlatform === "win32" ? "win" : hostPlatform === "darwin" ? "mac" : "linux",
  };
  const context = prepareDesktopUiTestContext(environment);
  const fixture = {
    context,
    environment,
    hostPlatform,
    launchers: readConfiguredLaunchers(context),
  };
  preparedFixtures.push(fixture);
  return fixture;
}

function readConfiguredLaunchers(
  context: DesktopUiTestContext,
): Record<ConfiguredProvider, string> {
  const settings = JSON.parse(
    NodeFS.readFileSync(NodePath.join(context.stateRoot, "userdata", "settings.json"), "utf8"),
  ) as {
    readonly providers?: Partial<Record<ConfiguredProvider, { readonly binaryPath?: string }>>;
  };
  const launcher = (provider: ConfiguredProvider): string => {
    const path = settings.providers?.[provider]?.binaryPath;
    if (!path || !NodePath.isAbsolute(path) || !NodeFS.existsSync(path)) {
      throw new Error(
        `Configured ${provider} launcher is missing or invalid: ${JSON.stringify(path)}`,
      );
    }
    return path;
  };
  return {
    codex: launcher("codex"),
    claudeAgent: launcher("claudeAgent"),
    cursor: launcher("cursor"),
    opencode: launcher("opencode"),
    grok: launcher("grok"),
  };
}

function windowsCommandToken(value: string): string {
  if (/["\r\n]/u.test(value)) {
    throw new Error(`Unsupported Windows fixture launcher token: ${JSON.stringify(value)}`);
  }
  return `"${value.replaceAll("%", "%%")}"`;
}

function configuredLauncherInvocation(
  fixture: PreparedProviderFixture,
  provider: ConfiguredProvider,
  args: readonly string[],
): { readonly command: string; readonly args: string[] } {
  const launcher = fixture.launchers[provider];
  if (fixture.hostPlatform !== "win32") {
    return { command: launcher, args: [...args] };
  }
  const commandLine = `"${[launcher, ...args].map(windowsCommandToken).join(" ")}"`;
  return {
    command: fixture.environment.ComSpec ?? "cmd.exe",
    args: ["/e:ON", "/v:OFF", "/d", "/s", "/c", commandLine],
  };
}

function spawnConfiguredLauncherSync(
  fixture: PreparedProviderFixture,
  provider: ConfiguredProvider,
  args: readonly string[],
  input?: string,
) {
  const launch = configuredLauncherInvocation(fixture, provider, args);
  return NodeChildProcess.spawnSync(launch.command, launch.args, {
    encoding: "utf8",
    env: fixture.environment,
    windowsVerbatimArguments: fixture.hostPlatform === "win32",
    ...(input === undefined ? {} : { input }),
  });
}

function spawnConfiguredLauncher(
  fixture: PreparedProviderFixture,
  provider: ConfiguredProvider,
  args: readonly string[],
): NodeChildProcess.ChildProcess {
  const launch = configuredLauncherInvocation(fixture, provider, args);
  return NodeChildProcess.spawn(launch.command, launch.args, {
    env: fixture.environment,
    stdio: ["ignore", "pipe", "pipe"],
    windowsVerbatimArguments: fixture.hostPlatform === "win32",
  });
}

function exchangeJsonLines(
  fixture: PreparedProviderFixture,
  provider: ConfiguredProvider,
  args: readonly string[],
  messages: ReadonlyArray<unknown>,
): Array<Record<string, unknown>> {
  const result = spawnConfiguredLauncherSync(
    fixture,
    provider,
    args,
    `${messages.map((message) => JSON.stringify(message)).join("\n")}\n`,
  );
  if (result.status !== 0) {
    throw new Error(`${provider} fixture failed (${result.status}): ${result.stderr}`);
  }
  return result.stdout
    .split(/\r?\n/u)
    .filter((line) => line.length > 0)
    .map((line) => JSON.parse(line) as Record<string, unknown>);
}

function responseWithId(
  messages: ReadonlyArray<Record<string, unknown>>,
  id: number,
): Record<string, unknown> {
  const response = messages.find((message) => message.id === id);
  if (!response) {
    throw new Error(`Fixture response ${id} was not emitted: ${JSON.stringify(messages)}`);
  }
  return response;
}

function reserveLocalPort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const reservation = NodeNet.createServer();
    reservation.once("error", reject);
    reservation.listen(0, "127.0.0.1", () => {
      const address = reservation.address();
      const port = typeof address === "object" && address !== null ? address.port : null;
      reservation.close((error) => {
        if (error) {
          reject(error);
        } else if (port === null) {
          reject(new Error("The OpenCode fixture did not reserve a TCP port."));
        } else {
          resolve(port);
        }
      });
    });
  });
}

async function waitForHealthyEndpoint(endpoint: string): Promise<void> {
  const deadline = Date.now() + 5_000;
  let lastError: unknown = null;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(`${endpoint}/global/health`);
      if (response.ok) {
        return;
      }
      lastError = new Error(`HTTP ${response.status}`);
    } catch (error) {
      lastError = error;
    }
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  throw new Error(`OpenCode fixture did not become healthy: ${String(lastError)}`);
}

async function readServerSentEvent(
  reader: ReadableStreamDefaultReader<Uint8Array>,
  eventType: string,
): Promise<Record<string, unknown>> {
  const decoder = new TextDecoder();
  const deadline = Date.now() + 5_000;
  let buffer = "";
  while (Date.now() < deadline) {
    const remaining = deadline - Date.now();
    const chunk = await withTimeout(
      reader.read(),
      remaining,
      `Timed out waiting for SSE event ${eventType}.`,
    );
    if (chunk.done) {
      break;
    }
    buffer += decoder.decode(chunk.value, { stream: true });
    for (const frame of buffer.split("\n\n")) {
      const data = frame
        .split(/\r?\n/u)
        .find((line) => line.startsWith("data: "))
        ?.slice("data: ".length);
      if (!data) {
        continue;
      }
      const event = JSON.parse(data) as Record<string, unknown>;
      if (event.type === eventType) {
        return event;
      }
    }
    const lastFrameBoundary = buffer.lastIndexOf("\n\n");
    if (lastFrameBoundary >= 0) {
      buffer = buffer.slice(lastFrameBoundary + 2);
    }
  }
  throw new Error(`SSE event ${eventType} was not emitted. Buffered data: ${buffer}`);
}

async function withTimeout<T>(
  operation: Promise<T>,
  timeoutMs: number,
  message: string,
): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      operation,
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new Error(message)), timeoutMs);
      }),
    ]);
  } finally {
    if (timer !== undefined) {
      clearTimeout(timer);
    }
  }
}

function fixtureProcessTerminationPlan(
  pid: number,
  hostPlatform: ReturnType<typeof NodeOS.platform>,
  environment: NodeJS.ProcessEnv,
): FixtureProcessTerminationPlan {
  if (!Number.isSafeInteger(pid) || pid <= 0) {
    throw new Error(`Fixture process has no valid test-owned PID: ${JSON.stringify(pid)}`);
  }
  if (hostPlatform !== "win32") {
    return {
      kind: "posix-child",
      pid,
      signals: ["SIGTERM", "SIGKILL"],
    };
  }
  const systemRoot = Object.entries(environment).find(
    ([name, value]) => name.toLowerCase() === "systemroot" && value,
  )?.[1];
  return {
    kind: "windows-tree",
    command: systemRoot
      ? NodePath.win32.join(systemRoot, "System32", "taskkill.exe")
      : "taskkill.exe",
    args: ["/PID", String(pid), "/T", "/F"],
  };
}

function fixtureProcessHasExited(child: NodeChildProcess.ChildProcess): boolean {
  return child.exitCode !== null || child.signalCode !== null;
}

async function requestGracefulOpenCodeShutdown(endpoint: string): Promise<void> {
  const controller = new AbortController();
  try {
    const response = await withTimeout(
      fetch(`${endpoint}/bibcode-fixture/shutdown`, {
        method: "POST",
        signal: controller.signal,
      }),
      gracefulShutdownTimeoutMs,
      "Timed out requesting graceful OpenCode fixture shutdown.",
    );
    if (!response.ok) {
      throw new Error(`OpenCode fixture shutdown returned HTTP ${response.status}.`);
    }
  } finally {
    controller.abort();
  }
}

async function cleanupOpenCodeFixture(
  endpoint: string,
  child: NodeChildProcess.ChildProcess,
  hostPlatform: ReturnType<typeof NodeOS.platform>,
  environment: NodeJS.ProcessEnv,
): Promise<void> {
  if (fixtureProcessHasExited(child)) {
    return;
  }
  try {
    await requestGracefulOpenCodeShutdown(endpoint);
    await waitForFixtureProcessExit(
      child,
      gracefulShutdownTimeoutMs,
      "OpenCode fixture did not exit after graceful shutdown.",
    );
    return;
  } catch {
    if (fixtureProcessHasExited(child)) {
      return;
    }
  }
  await terminateFixtureProcess(child, hostPlatform, environment);
}

async function terminateFixtureProcess(
  child: NodeChildProcess.ChildProcess,
  hostPlatform: ReturnType<typeof NodeOS.platform>,
  environment: NodeJS.ProcessEnv,
): Promise<void> {
  if (fixtureProcessHasExited(child)) {
    return;
  }
  const pid = child.pid;
  if (pid === undefined) {
    throw new Error("Cannot terminate fixture launcher without its test-owned PID.");
  }
  const plan = fixtureProcessTerminationPlan(pid, hostPlatform, environment);
  if (plan.kind === "windows-tree") {
    const result = NodeChildProcess.spawnSync(plan.command, plan.args, {
      encoding: "utf8",
      env: environment,
      timeout: forcedShutdownTimeoutMs,
      windowsHide: true,
    });
    try {
      await waitForFixtureProcessExit(
        child,
        forcedShutdownTimeoutMs,
        `Windows fixture process tree ${pid} did not exit after taskkill.`,
      );
    } catch (error) {
      const outcome = result.error
        ? String(result.error)
        : `status=${String(result.status)} signal=${String(result.signal)} stderr=${result.stderr.trim()}`;
      throw new Error(
        `Failed to terminate Windows fixture process tree ${pid}: ${outcome}; ${String(error)}`,
        { cause: error },
      );
    }
    return;
  }
  for (const signal of plan.signals) {
    child.kill(signal);
    try {
      await waitForFixtureProcessExit(
        child,
        signal === "SIGTERM" ? gracefulShutdownTimeoutMs : forcedShutdownTimeoutMs,
        `POSIX fixture process ${plan.pid} did not exit after ${signal}.`,
      );
      return;
    } catch (error) {
      if (signal === "SIGKILL") {
        throw error;
      }
    }
  }
}

async function waitForFixtureProcessExit(
  child: NodeChildProcess.ChildProcess,
  timeoutMs = forcedShutdownTimeoutMs,
  message = "Fixture launcher process did not exit.",
): Promise<void> {
  if (fixtureProcessHasExited(child)) {
    return;
  }
  await withTimeout(
    new Promise<void>((resolve) => child.once("exit", () => resolve())),
    timeoutMs,
    message,
  );
}

afterEach(() => {
  for (const fixture of preparedFixtures.splice(0)) {
    archiveAndCleanupDesktopUiTestContext(fixture.context);
  }
});

describe("fixture process cleanup", () => {
  it("targets the entire Windows test-owned process tree by explicit PID", () => {
    expect(
      fixtureProcessTerminationPlan(4_242, "win32", {
        SystemRoot: "C:\\Windows",
      }),
    ).toEqual({
      kind: "windows-tree",
      command: "C:\\Windows\\System32\\taskkill.exe",
      args: ["/PID", "4242", "/T", "/F"],
    });
  });

  it("targets only the test-owned POSIX child by explicit PID", () => {
    expect(fixtureProcessTerminationPlan(4_242, "linux", {})).toEqual({
      kind: "posix-child",
      pid: 4_242,
      signals: ["SIGTERM", "SIGKILL"],
    });
  });
});

describe("generated provider shims", () => {
  it("executes configured host launchers from paths with spaces and forwards arguments", () => {
    const fixture = prepareProviderFixture();
    expect(fixture.context.runRoot).toContain(" ");
    for (const [provider, expectedVersion] of [
      ["codex", "codex-cli 99.0.0"],
      ["claudeAgent", "2.1.200 (Claude Code)"],
      ["cursor", "cursor-agent 99.0.0-fixture"],
      ["opencode", "opencode 1.99.0-fixture"],
      ["grok", "grok-cli 99.0.0-fixture"],
    ] as const) {
      const result = spawnConfiguredLauncherSync(fixture, provider, ["--version"]);
      expect(result.status, `${provider}: ${result.stderr}`).toBe(0);
      expect(result.stdout.trim()).toBe(expectedVersion);
    }
  });

  it("reports an OpenCode 1.x version, matching the V1 HTTP API the fixture serves", () => {
    // The server drives OpenCode by major version and reports 3.x and later as unsupported,
    // which leaves OpenCode unavailable and its New panel entry disabled in packaged UI tests.
    const fixture = prepareProviderFixture();
    const result = spawnConfiguredLauncherSync(fixture, "opencode", ["--version"]);
    expect(result.status, result.stderr).toBe(0);
    const major = /\bv?(\d+)\./.exec(result.stdout)?.[1];
    expect(major).toBe("1");
  });

  it("answers Codex capability probes without starting the Node fixture runtime", () => {
    const fixture = prepareProviderFixture();

    const rootHelp = spawnConfiguredLauncherSync(fixture, "codex", ["--help"]);
    const appServerHelp = spawnConfiguredLauncherSync(fixture, "codex", ["app-server", "--help"]);

    expect(rootHelp.status, rootHelp.stderr).toBe(0);
    expect(rootHelp.stdout).toContain("--remote");
    expect(rootHelp.stdout).toContain("unix://");
    expect(appServerHelp.status, appServerHelp.stderr).toBe(0);
    expect(appServerHelp.stdout).toContain("--listen");
    expect(appServerHelp.stdout).toContain("unix://");
    expect(readProviderInputLog(fixture.context.providerInputLogPath)).toEqual([]);
  });

  it("speaks Codex app-server inventory and turn protocols", () => {
    const fixture = prepareProviderFixture();
    const responses = exchangeJsonLines(
      fixture,
      "codex",
      ["app-server"],
      [
        { id: 1, method: "initialize" },
        { id: 2, method: "skills/list", params: { cwds: [fixture.context.projectPath] } },
        {
          id: 3,
          method: "turn/start",
          params: { input: [{ type: "text", text: "$refactor" }] },
        },
      ],
    );

    expect(responseWithId(responses, 2)).toMatchObject({
      result: {
        data: [
          {
            cwd: fixture.context.projectPath,
            skills: [{ name: "refactor", enabled: true }],
          },
        ],
      },
    });
    expect(responses).toContainEqual(
      expect.objectContaining({
        method: "turn/completed",
        params: expect.objectContaining({ turn: expect.objectContaining({ status: "completed" }) }),
      }),
    );
    expect(readProviderInputLog(fixture.context.providerInputLogPath)).toMatchObject([
      { provider: "codex", prompt: "$refactor" },
    ]);
  });

  it("speaks Claude control, skill reload, stream, and result protocols", () => {
    const fixture = prepareProviderFixture();
    const responses = exchangeJsonLines(
      fixture,
      "claudeAgent",
      ["--print", "--input-format", "stream-json", "--output-format", "stream-json"],
      [
        {
          type: "control_request",
          request_id: "initialize",
          request: { subtype: "initialize" },
        },
        {
          type: "control_request",
          request_id: "skills",
          request: { subtype: "reload_skills" },
        },
        {
          type: "user",
          session_id: "claude-fixture",
          message: { content: [{ type: "text", text: "/compact" }] },
        },
      ],
    );

    expect(responses).toContainEqual(
      expect.objectContaining({
        type: "control_response",
        response: expect.objectContaining({
          request_id: "initialize",
          response: expect.objectContaining({
            commands: [
              expect.objectContaining({ name: "compact" }),
              expect.objectContaining({ name: "docs" }),
            ],
          }),
        }),
      }),
    );
    expect(responses).toContainEqual(
      expect.objectContaining({
        type: "control_response",
        response: expect.objectContaining({
          request_id: "skills",
          response: { skills: [expect.objectContaining({ name: "docs" })] },
        }),
      }),
    );
    expect(responses).toContainEqual(expect.objectContaining({ type: "stream_event" }));
    expect(responses).toContainEqual(
      expect.objectContaining({ type: "result", subtype: "success", is_error: false }),
    );
    expect(readProviderInputLog(fixture.context.providerInputLogPath)).toMatchObject([
      { provider: "claudeAgent", prompt: "/compact" },
    ]);
  });

  it("speaks Cursor ACP session and prompt protocols", () => {
    const fixture = prepareProviderFixture();
    const responses = exchangeJsonLines(
      fixture,
      "cursor",
      ["acp"],
      [
        { jsonrpc: "2.0", id: 1, method: "initialize" },
        { jsonrpc: "2.0", id: 2, method: "authenticate" },
        { jsonrpc: "2.0", id: 3, method: "session/new", params: {} },
        {
          jsonrpc: "2.0",
          id: 4,
          method: "session/prompt",
          params: { prompt: [{ type: "text", text: "/review" }] },
        },
      ],
    );

    expect(responseWithId(responses, 3)).toMatchObject({
      jsonrpc: "2.0",
      result: {
        sessionId: "bibcode-ui-cursor-session",
        modes: { currentModeId: "ask" },
      },
    });
    expect(responseWithId(responses, 4)).toMatchObject({
      jsonrpc: "2.0",
      result: { stopReason: "end_turn" },
    });
    expect(readProviderInputLog(fixture.context.providerInputLogPath)).toMatchObject([
      { provider: "cursor", prompt: "/review" },
    ]);
  });

  it("serves OpenCode inventory, session, prompt, and SSE protocols", async () => {
    const fixture = prepareProviderFixture();
    const port = await reserveLocalPort();
    const endpoint = `http://127.0.0.1:${port}`;
    const child = spawnConfiguredLauncher(fixture, "opencode", [
      "serve",
      "--hostname=127.0.0.1",
      `--port=${port}`,
    ]);
    const abort = new AbortController();

    try {
      await waitForHealthyEndpoint(endpoint);
      expect(await (await fetch(`${endpoint}/provider`)).json()).toMatchObject({
        connected: ["openai"],
      });
      expect(await (await fetch(`${endpoint}/agent`)).json()).toEqual([
        expect.objectContaining({ name: "writer", mode: "primary" }),
        expect.objectContaining({ name: "reviewer", mode: "subagent" }),
        expect.objectContaining({ name: "operator", mode: "all" }),
        expect.objectContaining({ name: "secret", mode: "subagent", hidden: true }),
      ]);
      expect(await (await fetch(`${endpoint}/command`)).json()).toEqual([
        expect.objectContaining({ name: "init" }),
      ]);

      const eventResponse = await fetch(`${endpoint}/event`, { signal: abort.signal });
      expect(eventResponse.headers.get("content-type")).toContain("text/event-stream");
      const reader = eventResponse.body?.getReader();
      if (!reader) {
        throw new Error("OpenCode fixture did not return an SSE response body.");
      }

      expect(
        await (
          await fetch(`${endpoint}/session`, {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: "{}",
          })
        ).json(),
      ).toEqual({ id: "bibcode-ui-opencode-session" });
      await fetch(`${endpoint}/session/bibcode-ui-opencode-session/prompt_async`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ parts: [{ type: "text", text: "@reviewer" }] }),
      });

      expect(await readServerSentEvent(reader, "session.status")).toMatchObject({
        properties: {
          sessionID: "bibcode-ui-opencode-session",
          status: { type: "idle" },
        },
      });
      expect(readProviderInputLog(fixture.context.providerInputLogPath)).toMatchObject([
        { provider: "opencode", prompt: "@reviewer" },
      ]);
      await reader.cancel();
    } finally {
      abort.abort();
      await cleanupOpenCodeFixture(endpoint, child, fixture.hostPlatform, fixture.environment);
    }
  });

  it("answers OpenCode message lookups the way the server's delivery check expects", async () => {
    // Before sending, the server asks whether the delivery's message already exists. A missing
    // message is 404; one the fixture received is a user message of that session.
    const fixture = prepareProviderFixture();
    const port = await reserveLocalPort();
    const endpoint = `http://127.0.0.1:${port}`;
    const child = spawnConfiguredLauncher(fixture, "opencode", [
      "serve",
      "--hostname=127.0.0.1",
      `--port=${port}`,
    ]);
    const messageUrl = `${endpoint}/session/bibcode-ui-opencode-session/message/msg_fixture1`;

    try {
      await waitForHealthyEndpoint(endpoint);
      expect((await fetch(messageUrl)).status).toBe(404);
      await fetch(`${endpoint}/session/bibcode-ui-opencode-session/prompt_async`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          messageID: "msg_fixture1",
          parts: [{ type: "text", text: "@reviewer" }],
        }),
      });
      const found = await fetch(messageUrl);
      expect(found.status).toBe(200);
      expect(await found.json()).toEqual({
        info: { id: "msg_fixture1", sessionID: "bibcode-ui-opencode-session", role: "user" },
        parts: [{ type: "text", text: "@reviewer" }],
      });
    } finally {
      await cleanupOpenCodeFixture(endpoint, child, fixture.hostPlatform, fixture.environment);
    }
  });

  it("speaks Grok ACP session and prompt protocols", () => {
    const fixture = prepareProviderFixture();
    const responses = exchangeJsonLines(
      fixture,
      "grok",
      ["agent", "stdio"],
      [
        { jsonrpc: "2.0", id: 1, method: "initialize" },
        { jsonrpc: "2.0", id: 2, method: "authenticate" },
        { jsonrpc: "2.0", id: 3, method: "session/create", params: {} },
        {
          jsonrpc: "2.0",
          id: 4,
          method: "session/prompt",
          params: { prompt: [{ type: "text", text: "/skills" }] },
        },
      ],
    );

    expect(responseWithId(responses, 3)).toMatchObject({
      jsonrpc: "2.0",
      result: {
        sessionId: "bibcode-ui-grok-session",
        modes: { currentModeId: "code" },
      },
    });
    expect(responseWithId(responses, 4)).toMatchObject({
      jsonrpc: "2.0",
      result: { stopReason: "end_turn" },
    });
    expect(readProviderInputLog(fixture.context.providerInputLogPath)).toMatchObject([
      { provider: "grok", prompt: "/skills" },
    ]);
  });
});
