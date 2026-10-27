// @effect-diagnostics nodeBuiltinImport:off - Desktop UI fixture tests inspect host temp files.
// @effect-diagnostics globalTimers:off - Native fixture protocol tests use bounded child-process watchdogs.
// @effect-diagnostics globalDate:off - Native fixture timestamp assertions bracket a real child process outside an Effect runtime.
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import * as NodeChildProcess from "node:child_process";
import * as NodeReadline from "node:readline";

import { afterEach, describe, expect, it } from "vite-plus/test";

import { desktopActivityFixture, desktopActivitySessionCommands } from "./activity-events.ts";
import { readProviderInputLog } from "./provider-input-log.ts";
import {
  archiveAndCleanupDesktopUiTestContext,
  clearDesktopActivityMarker,
  composerProviderProfiles,
  completeDesktopUiSlowTurn,
  deferDesktopUiTestContextCleanupUntilExit,
  prepareDesktopUiTestContext,
  type DesktopUiDirectoryRemover,
  type DesktopUiExitRegistrar,
  type DesktopUiTestContext,
} from "./test-project.ts";

const contexts: DesktopUiTestContext[] = [];
const hostTemporaryDirectories: string[] = [];
// oxlint-disable-next-line coddy/no-global-process-runtime -- These compatibility assertions must distinguish the native Windows host from simulated Unix targets.
const isNativeWindowsHost = process.platform === "win32";

interface FixtureProtocol {
  readonly notifications: Array<{ method: string; params: Record<string, unknown> }>;
  readonly close: () => Promise<void>;
  readonly request: (method: string, params?: Record<string, unknown>) => Promise<unknown>;
}

function startCodexFixture(fixtureScript: string, environment: NodeJS.ProcessEnv): FixtureProtocol {
  const child = NodeChildProcess.spawn(process.execPath, [fixtureScript, "app-server"], {
    env: { ...process.env, ...environment },
    stdio: ["pipe", "pipe", "pipe"],
  });
  const lines = NodeReadline.createInterface({ input: child.stdout });
  let sequence = 0;
  const notifications: FixtureProtocol["notifications"] = [];
  const pending = new Map<
    number,
    { readonly reject: (error: Error) => void; readonly resolve: (value: unknown) => void }
  >();
  lines.on("line", (line) => {
    const message = JSON.parse(line) as {
      readonly id?: number;
      readonly error?: unknown;
      readonly result?: unknown;
      readonly method?: string;
      readonly params?: Record<string, unknown>;
    };
    if (typeof message.id !== "number") {
      if (message.method)
        notifications.push({ method: message.method, params: message.params ?? {} });
      return;
    }
    const waiter = pending.get(message.id);
    if (!waiter) return;
    pending.delete(message.id);
    if (message.error !== undefined) {
      waiter.reject(new Error(JSON.stringify(message.error)));
    } else {
      waiter.resolve(message.result);
    }
  });
  child.once("exit", (code) => {
    for (const waiter of pending.values()) {
      waiter.reject(new Error(`Codex fixture exited before replying (${String(code)}).`));
    }
    pending.clear();
  });
  return {
    notifications,
    request: (method, params = {}) =>
      new Promise((resolve, reject) => {
        const id = sequence++;
        pending.set(id, { reject, resolve });
        child.stdin.write(`${JSON.stringify({ id, method, params })}\n`);
      }),
    close: async () => {
      child.stdin.end();
      if (child.exitCode !== null) return;
      await new Promise<void>((resolve) => child.once("exit", () => resolve()));
    },
  };
}

afterEach(() => {
  for (const context of contexts.splice(0)) {
    archiveAndCleanupDesktopUiTestContext(context);
  }
  for (const directory of hostTemporaryDirectories.splice(0)) {
    NodeFS.rmSync(directory, { recursive: true, force: true });
  }
});

describe.each([
  { platform: "mac", executableSuffix: "" },
  { platform: "linux", executableSuffix: "" },
  { platform: "win", executableSuffix: ".cmd" },
])("prepareDesktopUiTestContext on $platform", ({ platform, executableSuffix }) => {
  it("allocates its automatic run root beneath the host temporary directory", () => {
    const hostTemporaryDirectory = NodeFS.mkdtempSync(
      NodePath.join(NodeOS.tmpdir(), "coddy-studio-e2e-host-temp-"),
    );
    hostTemporaryDirectories.push(hostTemporaryDirectory);
    const environment: NodeJS.ProcessEnv = { CODDY_STUDIO_E2E_PLATFORM: platform };
    const context = prepareDesktopUiTestContext(environment, hostTemporaryDirectory);
    contexts.push(context);

    expect(NodePath.dirname(context.runRoot)).toBe(hostTemporaryDirectory);
  });

  it("pins every provider to an absolute fixture executable", () => {
    const environment: NodeJS.ProcessEnv = { CODDY_STUDIO_E2E_PLATFORM: platform };
    const context = prepareDesktopUiTestContext(environment);
    contexts.push(context);

    const settingsPath = NodePath.join(context.stateRoot, "userdata", "settings.json");
    const settings = JSON.parse(NodeFS.readFileSync(settingsPath, "utf8")) as {
      readonly providers: Record<
        string,
        { readonly enabled: boolean; readonly binaryPath: string }
      >;
    };
    const expectedExecutable = (name: string): string =>
      NodePath.join(context.shimDirectory, `${name}${executableSuffix}`);

    expect(settings.providers.codex?.binaryPath).toBe(expectedExecutable("codex"));
    expect(settings.providers.claudeAgent?.binaryPath).toBe(expectedExecutable("claude"));
    expect(settings.providers.cursor?.binaryPath).toBe(expectedExecutable("cursor-agent"));
    expect(settings.providers.grok?.binaryPath).toBe(expectedExecutable("grok"));
    expect(settings.providers.opencode?.binaryPath).toBe(expectedExecutable("opencode"));
    expect(settings.providers.codex?.enabled).toBe(true);
    expect(settings.providers.claudeAgent?.enabled).toBe(true);
    expect(settings.providers.cursor?.enabled).toBe(true);
    expect(settings.providers.grok?.enabled).toBe(true);
    expect(settings.providers.opencode?.enabled).toBe(true);
    for (const provider of Object.values(settings.providers)) {
      expect(NodePath.isAbsolute(provider.binaryPath)).toBe(true);
    }
    expect(environment.CODDY_STUDIO_E2E_SHIM_DIRECTORY).toBe(context.shimDirectory);
  });

  it("isolates provider user inventory inside the disposable run root", () => {
    const environment: NodeJS.ProcessEnv = {
      CODDY_STUDIO_E2E_PLATFORM: platform,
      APPDATA: String.raw`C:\Users\host-must-not-be-used\AppData\Roaming`,
      HOME: "/host/home-must-not-be-used",
      LOCALAPPDATA: String.raw`C:\Users\host-must-not-be-used\AppData\Local`,
      PSModuleAnalysisCachePath: String.raw`C:\Users\host-must-not-be-used\ModuleAnalysisCache`,
      USERPROFILE: String.raw`C:\Users\host-must-not-be-used`,
    };
    const context = prepareDesktopUiTestContext(environment);
    contexts.push(context);
    const expectedFixtureUserHome = NodePath.join(context.runRoot, "fixture-user-home");

    expect(context.fixtureUserHomePath).toBe(expectedFixtureUserHome);
    expect(NodePath.isAbsolute(context.fixtureUserHomePath)).toBe(true);
    expect(context.fixtureUserHomePath.startsWith(`${context.runRoot}${NodePath.sep}`)).toBe(true);
    expect(environment.CODDY_STUDIO_E2E_USER_HOME).toBe(expectedFixtureUserHome);
    for (const relativePath of [
      ".cursor/commands/review.md",
      ".cursor/skills/frontend/SKILL.md",
      ".cursor/agents/cursor-prose-agent.md",
    ]) {
      expect(
        NodeFS.readFileSync(NodePath.join(context.fixtureUserHomePath, relativePath), "utf8"),
      ).not.toBe("");
    }
    const settings = JSON.parse(
      NodeFS.readFileSync(NodePath.join(context.stateRoot, "userdata", "settings.json"), "utf8"),
    ) as {
      readonly providerInstances?: Record<
        string,
        {
          readonly driver: string;
          readonly environment: ReadonlyArray<{
            readonly name: string;
            readonly value: string;
            readonly sensitive: boolean;
          }>;
        }
      >;
    };
    expect(settings.providerInstances?.cursor).toEqual({
      driver: "cursor",
      enabled: true,
      environment: [
        {
          name: platform === "win" ? "USERPROFILE" : "HOME",
          value: expectedFixtureUserHome,
          sensitive: false,
        },
      ],
    });
    if (platform === "win") {
      const expectedAppData = NodePath.join(context.runRoot, "fixture-appdata", "Roaming");
      const expectedLocalAppData = NodePath.join(context.runRoot, "fixture-appdata", "Local");
      const expectedPowerShellModuleCache = NodePath.join(
        expectedLocalAppData,
        "Microsoft",
        "Windows",
        "PowerShell",
        "ModuleAnalysisCache",
      );

      expect(environment.USERPROFILE).toBe(expectedFixtureUserHome);
      expect(environment.HOME).toBe(expectedFixtureUserHome);
      expect(environment.APPDATA).toBe(expectedAppData);
      expect(environment.LOCALAPPDATA).toBe(expectedLocalAppData);
      expect(environment.PSModuleAnalysisCachePath).toBe(expectedPowerShellModuleCache);
      expect(NodeFS.statSync(expectedAppData).isDirectory()).toBe(true);
      expect(NodeFS.statSync(expectedLocalAppData).isDirectory()).toBe(true);
      expect(NodeFS.statSync(NodePath.dirname(expectedPowerShellModuleCache)).isDirectory()).toBe(
        true,
      );
    } else {
      expect(environment.HOME).toBe(expectedFixtureUserHome);
      expect(environment.USERPROFILE).toBe(String.raw`C:\Users\host-must-not-be-used`);
    }
  });
});

it.runIf(isNativeWindowsHost)(
  "executes the Cursor action shim through the native Windows command processor",
  () => {
    const environment: NodeJS.ProcessEnv = { CODDY_STUDIO_E2E_PLATFORM: "win" };
    const context = prepareDesktopUiTestContext(environment);
    contexts.push(context);
    const editorShimPath = NodePath.join(context.shimDirectory, "cursor.cmd");
    const editorTarget = String.raw`C:\fixture\userdata\logs`;
    const editorLaunch = NodeChildProcess.spawnSync(
      process.env.ComSpec ?? "cmd.exe",
      ["/d", "/c", `${editorShimPath} ${editorTarget}`],
      {
        env: { ...process.env, ...environment },
        encoding: "utf8",
        shell: false,
      },
    );

    expect(editorLaunch.error).toBeUndefined();
    expect(editorLaunch.status, editorLaunch.stderr).toBe(0);
    expect(JSON.parse(NodeFS.readFileSync(context.nativeActionLogPath, "utf8").trim())).toEqual({
      action: "openInEditor",
      args: [editorTarget],
    });
  },
);

describe.each(["mac", "linux"])("prepareDesktopUiTestContext on %s", (platform) => {
  it("keeps the canonical Codex helper socket below the Unix path limit", () => {
    const environment: NodeJS.ProcessEnv = { CODDY_STUDIO_E2E_PLATFORM: platform };
    const context = prepareDesktopUiTestContext(environment);
    contexts.push(context);
    const canonicalStateRoot = isNativeWindowsHost
      ? NodePath.posix.join(
          platform === "mac" ? "/private/tmp" : "/tmp",
          NodePath.basename(context.runRoot),
          "state",
        )
      : NodeFS.realpathSync(context.stateRoot);
    const canonicalSocket = NodePath.posix.join(
      canonicalStateRoot,
      "userdata",
      "runtime",
      "provider-terminal",
      "c1234567890abcdef",
      "s",
    );

    expect(new TextEncoder().encode(canonicalSocket).byteLength).toBeLessThanOrEqual(100);
  });
});

describe("packaged provider composer fixture", () => {
  it("keeps a slow Codex turn running through steer and completes it only on its release marker", async () => {
    const environment: NodeJS.ProcessEnv = { PATH: process.env.PATH, CODDY_STUDIO_E2E_PLATFORM: "mac" };
    const context = prepareDesktopUiTestContext(environment);
    contexts.push(context);
    const protocol = startCodexFixture(
      NodePath.join(context.shimDirectory, "codex-fixture.mjs"),
      environment,
    );
    try {
      const started = (await protocol.request("turn/start", {
        input: [{ type: "text", text: "first [[slow]]" }],
      })) as { turn: { id: string } };
      await protocol.request("account/read");
      expect(protocol.notifications.map(({ method }) => method)).toEqual([
        "turn/started",
        "item/agentMessage/delta",
      ]);
      await expect(
        protocol.request("turn/steer", {
          expectedTurnId: "stale-turn",
          input: [{ type: "text", text: "wrong" }],
        }),
      ).rejects.toThrow(/active turn/);
      await expect(
        protocol.request("turn/steer", {
          expectedTurnId: started.turn.id,
          input: [{ type: "text", text: "second" }],
        }),
      ).resolves.toEqual({ turnId: started.turn.id });
      expect(protocol.notifications.filter(({ method }) => method === "turn/started")).toHaveLength(
        1,
      );
      expect(protocol.notifications.some(({ method }) => method === "turn/completed")).toBe(false);
      expect(readProviderInputLog(context.providerInputLogPath)).toMatchObject([
        { provider: "codex", kind: "start", prompt: "first [[slow]]", turnId: started.turn.id },
        { provider: "codex", kind: "steer", prompt: "second", turnId: started.turn.id },
      ]);
      completeDesktopUiSlowTurn(context.projectPath, started.turn.id);
      await expect
        .poll(() => protocol.notifications.filter(({ method }) => method === "turn/completed"))
        .toEqual([
          {
            method: "turn/completed",
            params: {
              threadId: "coddy-studio-ui-provider-thread",
              turn: { id: started.turn.id, status: "completed" },
            },
          },
        ]);
      const next = (await protocol.request("turn/start", {
        input: [{ type: "text", text: "fourth" }],
      })) as { turn: { id: string } };
      await protocol.request("account/read");
      expect(next.turn.id).not.toBe(started.turn.id);
      expect(
        protocol.notifications.filter(({ method }) => method === "turn/completed"),
      ).toHaveLength(2);
      expect(readProviderInputLog(context.providerInputLogPath).at(-1)).toMatchObject({
        kind: "start",
        prompt: "fourth",
        turnId: next.turn.id,
      });
    } finally {
      await protocol.close();
    }
  });

  it.each(["complete", "interrupt"])(
    "bounds slow turns and clears timers after %s",
    async (settle) => {
      const environment: NodeJS.ProcessEnv = {
        PATH: process.env.PATH,
        CODDY_STUDIO_E2E_PLATFORM: "mac",
        CODDY_STUDIO_E2E_SLOW_TURN_MS: "200",
      };
      const context = prepareDesktopUiTestContext(environment);
      contexts.push(context);
      const protocol = startCodexFixture(
        NodePath.join(context.shimDirectory, "codex-fixture.mjs"),
        environment,
      );
      try {
        const started = (await protocol.request("turn/start", {
          input: [{ type: "text", text: "[[slow]]" }],
        })) as { turn: { id: string } };
        await protocol.request("account/read");
        expect(protocol.notifications.some(({ method }) => method === "turn/completed")).toBe(
          false,
        );
        if (settle === "interrupt")
          await protocol.request("turn/interrupt", { turnId: started.turn.id });
        await expect
          .poll(() => protocol.notifications.filter(({ method }) => method === "turn/completed"))
          .toEqual([
            {
              method: "turn/completed",
              params: {
                threadId: "coddy-studio-ui-provider-thread",
                turn: {
                  id: started.turn.id,
                  status: settle === "interrupt" ? "interrupted" : "completed",
                },
              },
            },
          ]);
        await expect(
          protocol.request("turn/steer", {
            expectedTurnId: started.turn.id,
            input: [{ type: "text", text: "late" }],
          }),
        ).rejects.toThrow(/active turn/);
        await new Promise((resolve) => setTimeout(resolve, 250));
        expect(
          protocol.notifications.filter(({ method }) => method === "turn/completed"),
        ).toHaveLength(1);
      } finally {
        await protocol.close();
      }
    },
  );

  it("replays the Claude user message before acknowledging the completed turn", async () => {
    const environment: NodeJS.ProcessEnv = {
      PATH: process.env.PATH,
      CODDY_STUDIO_E2E_PLATFORM: "mac",
    };
    const context = prepareDesktopUiTestContext(environment);
    contexts.push(context);
    const executable = NodePath.join(context.shimDirectory, "claude-fixture.mjs");
    const child = NodeChildProcess.spawn(
      process.execPath,
      [executable, "--print", "--replay-user-messages"],
      {
        env: { ...process.env, ...environment },
        stdio: ["pipe", "pipe", "pipe"],
      },
    );
    const lines = NodeReadline.createInterface({ input: child.stdout });
    const messages: Array<Record<string, unknown>> = [];
    const completed = new Promise<void>((resolve, reject) => {
      const timeout = setTimeout(
        () => reject(new Error(`Claude fixture emitted only ${String(messages.length)} messages.`)),
        2_000,
      );
      lines.on("line", (line) => {
        messages.push(JSON.parse(line) as Record<string, unknown>);
        if (messages.length === 3) {
          clearTimeout(timeout);
          resolve();
        }
      });
      child.once("exit", (code) => {
        if (messages.length < 3) {
          clearTimeout(timeout);
          reject(new Error(`Claude fixture exited before replaying the turn (${String(code)}).`));
        }
      });
    });

    const user = {
      type: "user",
      session_id: "claude-replay-session",
      message: { role: "user", content: [{ type: "text", text: "/compact" }] },
      parent_tool_use_id: null,
    };
    child.stdin.write(`${JSON.stringify(user)}\n`);
    await completed;
    expect(messages[0]).toEqual(user);
    expect(messages[1]).toMatchObject({ type: "stream_event" });
    expect(messages[2]).toMatchObject({ type: "result", subtype: "success" });

    child.stdin.end();
    if (child.exitCode === null) {
      await new Promise<void>((resolve) => child.once("exit", () => resolve()));
    }
  });

  it("does not leak Task 4 activation into a later unrelated Codex process", async () => {
    const environment: NodeJS.ProcessEnv = {
      PATH: process.env.PATH,
      CODDY_STUDIO_E2E_PLATFORM: "mac",
    };
    const context = prepareDesktopUiTestContext(environment);
    contexts.push(context);
    const executable = NodePath.join(context.shimDirectory, "codex-fixture.mjs");

    const taskFour = startCodexFixture(executable, environment);
    try {
      await taskFour.request("thread/start", { cwd: context.projectPath });
      await taskFour.request("turn/start", {
        input: [{ type: "text", text: "load deterministic activity" }],
      });
      expect(
        (
          (await taskFour.request("thread/list")) as {
            readonly data: ReadonlyArray<unknown>;
          }
        ).data,
      ).toHaveLength(1);
    } finally {
      await taskFour.close();
      clearDesktopActivityMarker(context.projectPath);
    }

    const unrelated = startCodexFixture(executable, environment);
    try {
      await unrelated.request("thread/start", { cwd: context.projectPath });
      await unrelated.request("turn/start", {
        input: [{ type: "text", text: "later unrelated packaged test" }],
      });
      expect(await unrelated.request("thread/list")).toEqual({ data: [], nextCursor: null });
      expect(await unrelated.request("thread/backgroundTerminals/list")).toEqual({
        data: [],
        nextCursor: null,
      });
    } finally {
      await unrelated.close();
    }
  });

  it("isolates Task 4 activity from every unrelated Codex fixture session", async () => {
    const environment: NodeJS.ProcessEnv = {
      PATH: process.env.PATH,
      CODDY_STUDIO_E2E_PLATFORM: "mac",
    };
    const context = prepareDesktopUiTestContext(environment);
    contexts.push(context);
    const executable = NodePath.join(context.shimDirectory, "codex-fixture.mjs");

    const unrelated = startCodexFixture(executable, environment);
    try {
      await unrelated.request("initialize");
      await unrelated.request("thread/start", { cwd: context.projectPath });
      expect(await unrelated.request("thread/list")).toEqual({ data: [], nextCursor: null });
      await unrelated.request("turn/start", {
        input: [{ type: "text", text: "unrelated packaged test" }],
      });
      expect(await unrelated.request("thread/list")).toEqual({ data: [], nextCursor: null });
      expect(await unrelated.request("thread/backgroundTerminals/list")).toEqual({
        data: [],
        nextCursor: null,
      });
    } finally {
      await unrelated.close();
    }

    const taskFour = startCodexFixture(executable, environment);
    try {
      await taskFour.request("initialize");
      await taskFour.request("thread/start", { cwd: context.projectPath });
      await taskFour.request("turn/start", {
        input: [{ type: "text", text: "load deterministic activity" }],
      });
      const active = (await taskFour.request("thread/list")) as {
        readonly data: ReadonlyArray<{
          readonly id: string;
          readonly status: { readonly type: string };
        }>;
      };
      expect(active.data).toEqual([
        expect.objectContaining({
          id: desktopActivityFixture.actor.id,
          status: { type: "active", activeFlags: [] },
        }),
      ]);
      expect(await taskFour.request("thread/backgroundTerminals/list")).toEqual({
        data: [
          {
            itemId: desktopActivityFixture.backgroundTask.id,
            processId: "fixture-process",
            command: desktopActivityFixture.backgroundTask.command,
          },
        ],
        nextCursor: null,
      });

      await taskFour.request("turn/start", {
        input: [{ type: "text", text: "publish deterministic live activity update" }],
      });
      const updated = (await taskFour.request("thread/list")) as {
        readonly data: ReadonlyArray<{
          readonly id: string;
          readonly status: { readonly type: string };
        }>;
      };
      expect(updated.data).toEqual([
        expect.objectContaining({
          id: desktopActivityFixture.actor.id,
          status: { type: "idle" },
        }),
      ]);
      expect(await taskFour.request("thread/backgroundTerminals/list")).toEqual({
        data: [],
        nextCursor: null,
      });
    } finally {
      await taskFour.close();
    }
  });

  it("shares the live activity revision across the chat and terminal fixture processes", async () => {
    const environment: NodeJS.ProcessEnv = {
      PATH: process.env.PATH,
      CODDY_STUDIO_E2E_PLATFORM: "mac",
    };
    const context = prepareDesktopUiTestContext(environment);
    contexts.push(context);
    const executable = NodePath.join(context.shimDirectory, "codex-fixture.mjs");
    const chatProcess = startCodexFixture(executable, environment);
    try {
      await chatProcess.request("turn/start", {
        input: [{ type: "text", text: "load deterministic activity" }],
      });
    } finally {
      await chatProcess.close();
    }

    const followupProcess = startCodexFixture(executable, environment);
    try {
      await followupProcess.request("thread/resume", {
        cwd: context.projectPath,
        threadId: desktopActivityFixture.rootThreadId,
      });
      await followupProcess.request("turn/start", {
        input: [{ type: "text", text: "publish deterministic live activity update" }],
      });
      expect(await followupProcess.request("thread/list")).toEqual({
        data: [
          expect.objectContaining({
            id: desktopActivityFixture.actor.id,
            status: { type: "idle" },
          }),
        ],
        nextCursor: null,
      });
      expect(await followupProcess.request("thread/backgroundTerminals/list")).toEqual({
        data: [],
        nextCursor: null,
      });
      await followupProcess.request("turn/start", {
        input: [{ type: "text", text: "publish deterministic composer activity update" }],
      });
      expect(await followupProcess.request("thread/list")).toEqual({
        data: [
          expect.objectContaining({
            id: desktopActivityFixture.actor.id,
            status: { type: "idle" },
          }),
          expect.objectContaining({
            id: `${desktopActivityFixture.actor.id}-composer`,
            status: { type: "active", activeFlags: [] },
          }),
        ],
        nextCursor: null,
      });
    } finally {
      await followupProcess.close();
    }
  });

  it("builds the canonical project, thread, and provider-session command sequence", () => {
    expect(
      desktopActivitySessionCommands("/fixture/project", Date.parse("2026-07-29T18:00:02.000Z")),
    ).toEqual([
      {
        type: "project.create",
        commandId: "coddy-studio-ui-activity-project-create",
        projectId: "coddy-studio-ui-activity-project",
        title: "Coddy Studio UI Fixture",
        workspaceRoot: "/fixture/project",
        defaultModelSelection: {
          instanceId: "codex",
          model: "gpt-5.4",
          options: [{ id: "reasoningEffort", value: "medium" }],
        },
        createdAt: "2026-07-29T18:00:00.000Z",
      },
      {
        type: "thread.create",
        commandId: "coddy-studio-ui-activity-thread-create",
        threadId: "coddy-studio-ui-activity-thread",
        projectId: "coddy-studio-ui-activity-project",
        title: "Activity acceptance fixture",
        modelSelection: {
          instanceId: "codex",
          model: "gpt-5.4",
          options: [{ id: "reasoningEffort", value: "medium" }],
        },
        runtimeMode: "full-access",
        interactionMode: "default",
        branch: "main",
        worktreePath: null,
        createdAt: "2026-07-29T18:00:01.000Z",
      },
      {
        type: "thread.turn.start",
        commandId: "coddy-studio-ui-activity-turn-start",
        threadId: "coddy-studio-ui-activity-thread",
        message: {
          messageId: "coddy-studio-ui-activity-message",
          role: "user",
          text: "load deterministic activity",
          attachments: [],
        },
        modelSelection: {
          instanceId: "codex",
          model: "gpt-5.4",
          options: [{ id: "reasoningEffort", value: "medium" }],
        },
        runtimeMode: "full-access",
        interactionMode: "default",
        createdAt: "2026-07-29T18:00:02.000Z",
      },
    ]);
    expect(desktopActivityFixture.thread.id).toBe("coddy-studio-ui-activity-thread");
  });

  it("uses observation-time timestamps for a newly materialized activity fixture", async () => {
    const environment: NodeJS.ProcessEnv = {
      PATH: process.env.PATH,
      CODDY_STUDIO_E2E_PLATFORM: "mac",
    };
    const context = prepareDesktopUiTestContext(environment);
    contexts.push(context);
    const executable = NodePath.join(context.shimDirectory, "codex-fixture.mjs");
    const fixture = startCodexFixture(executable, environment);
    const earliestEpochSeconds = Math.floor(Date.now() / 1_000) - 1;

    try {
      await fixture.request("turn/start", {
        input: [{ type: "text", text: "load deterministic activity" }],
      });
      const active = (await fixture.request("thread/list")) as {
        readonly data: ReadonlyArray<{ readonly createdAt: number; readonly updatedAt: number }>;
      };
      const latestEpochSeconds = Math.floor(Date.now() / 1_000) + 1;
      expect(active.data).toHaveLength(1);
      expect(active.data[0]!.createdAt).toBeGreaterThanOrEqual(earliestEpochSeconds);
      expect(active.data[0]!.createdAt).toBeLessThanOrEqual(latestEpochSeconds);
      expect(active.data[0]!.updatedAt).toBeGreaterThanOrEqual(active.data[0]!.createdAt);
      expect(active.data[0]!.updatedAt).toBeLessThanOrEqual(latestEpochSeconds + 1);
    } finally {
      await fixture.close();
    }

    const earliestCommandTime = Date.now() - 2_001;
    const commands = desktopActivitySessionCommands("/fixture/project");
    const latestCommandTime = Date.now() + 2_001;
    for (const command of commands) {
      const createdAt = Date.parse(command.createdAt);
      expect(createdAt).toBeGreaterThanOrEqual(earliestCommandTime);
      expect(createdAt).toBeLessThanOrEqual(latestCommandTime);
    }
  });

  it("exports the real normalized inline capability profiles", () => {
    expect(composerProviderProfiles).toEqual({
      codex: {
        commands: ["goal"],
        slashSkills: [],
        dollarSkills: ["refactor"],
        mentionableAgents: [],
      },
      claudeAgent: {
        commands: ["compact", "goal", "loop"],
        slashSkills: ["docs"],
        dollarSkills: [],
        mentionableAgents: [],
      },
      cursor: {
        commands: [
          "review",
          "models",
          "auto-run",
          "new-chat",
          "vim",
          "help",
          "feedback",
          "resume",
          "copy-req-id",
          "rules",
          "commands",
          "mcp",
          "max-mode",
          "compress",
          "add-plugin",
          "logout",
          "quit",
        ],
        slashSkills: ["docs", "frontend"],
        dollarSkills: [],
        mentionableAgents: [],
      },
      opencode: {
        commands: ["init"],
        slashSkills: [],
        dollarSkills: [],
        mentionableAgents: ["reviewer", "operator"],
      },
      grok: {
        commands: ["loop", "agents", "skills"],
        slashSkills: [],
        dollarSkills: [],
        mentionableAgents: [],
      },
    });
  });

  it("writes provider-native workspace metadata and exports an absolute input log", () => {
    const environment: NodeJS.ProcessEnv = { CODDY_STUDIO_E2E_PLATFORM: "mac" };
    const context = prepareDesktopUiTestContext(environment);
    contexts.push(context);

    for (const relativePath of [
      ".claude/skills/docs/SKILL.md",
      ".cursor/commands/review.md",
      ".cursor/skills/frontend/SKILL.md",
      ".cursor/agents/cursor-prose-agent.md",
    ]) {
      expect(
        NodeFS.readFileSync(NodePath.join(context.projectPath, relativePath), "utf8"),
      ).not.toBe("");
    }
    const compatibleCursorSkills = new Set(
      [".claude/skills", ".cursor/skills"].flatMap((root) =>
        NodeFS.readdirSync(NodePath.join(context.projectPath, root)),
      ),
    );
    expect(composerProviderProfiles.cursor.slashSkills.toSorted()).toEqual(
      [...compatibleCursorSkills].toSorted(),
    );
    expect(NodePath.isAbsolute(context.providerInputLogPath)).toBe(true);
    expect(environment.CODDY_STUDIO_E2E_PROVIDER_INPUT_LOG).toBe(context.providerInputLogPath);
    expect(NodePath.isAbsolute(context.nativeActionLogPath)).toBe(true);
    expect(environment.CODDY_STUDIO_E2E_NATIVE_ACTION_LOG).toBe(context.nativeActionLogPath);
    expect(environment.CODDY_STUDIO_LOG).toContain("coddy-studio::desktop_e2e=info");
    expect(NodeFS.readFileSync(context.nativeActionLogPath, "utf8")).toBe("");
  });

  it("generates native protocol fixtures while keeping hidden and prose-only agents inline-inert", () => {
    const environment: NodeJS.ProcessEnv = { CODDY_STUDIO_E2E_PLATFORM: "mac" };
    const context = prepareDesktopUiTestContext(environment);
    contexts.push(context);
    const fixtureSource = (name: string): string =>
      NodeFS.readFileSync(NodePath.join(context.shimDirectory, `${name}-fixture.mjs`), "utf8");

    expect(fixtureSource("codex")).toContain('"skills/list"');
    expect(fixtureSource("codex")).toContain('"refactor"');
    expect(fixtureSource("claude")).toContain('"compact"');
    expect(fixtureSource("claude")).toContain('"docs"');
    expect(fixtureSource("claude")).toContain('"claude-prose-agent"');
    expect(fixtureSource("cursor-agent")).toContain('"cursor/list_available_models"');
    expect(fixtureSource("opencode")).toContain('"primary"');
    expect(fixtureSource("opencode")).toContain('"subagent"');
    expect(fixtureSource("opencode")).toContain('"all"');
    expect(fixtureSource("opencode")).toContain('"secret"');
    expect(fixtureSource("grok")).toContain('"session/create"');
    expect(fixtureSource("grok")).toContain('"session/prompt"');

    expect(composerProviderProfiles.claudeAgent.mentionableAgents).not.toContain(
      "claude-prose-agent",
    );
    expect(composerProviderProfiles.cursor.mentionableAgents).not.toContain("cursor-prose-agent");
    expect(composerProviderProfiles.opencode.mentionableAgents).not.toContain("secret");
    expect(composerProviderProfiles.opencode.mentionableAgents).not.toContain("writer");
  });
});

describe("archiveAndCleanupDesktopUiTestContext", () => {
  it("configures retries for transient Windows locks while removing the run directory", () => {
    const runRoot = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "coddy-studio-cleanup-"));
    const artifactDirectory = NodeFS.mkdtempSync(
      NodePath.join(NodeOS.tmpdir(), "coddy-studio-cleanup-artifacts-"),
    );
    const context: DesktopUiTestContext = {
      runRoot,
      stateRoot: NodePath.join(runRoot, "missing-state"),
      projectPath: NodePath.join(runRoot, "project"),
      shimDirectory: NodePath.join(runRoot, "shims"),
      artifactDirectory,
      fixtureUserHomePath: NodePath.join(runRoot, "fixture-user-home"),
      nativeActionLogPath: NodePath.join(artifactDirectory, "native-actions.jsonl"),
      providerInputLogPath: NodePath.join(runRoot, "provider-input.jsonl"),
    };
    let removalOptions: Parameters<DesktopUiDirectoryRemover>[1] | undefined;
    const removeDirectory: DesktopUiDirectoryRemover = (path, options) => {
      removalOptions = options;
      NodeFS.rmSync(path, options);
    };

    try {
      archiveAndCleanupDesktopUiTestContext(context, removeDirectory);

      expect(removalOptions).toEqual({
        recursive: true,
        force: true,
        maxRetries: 10,
        retryDelay: 100,
      });
      expect(NodeFS.existsSync(runRoot)).toBe(false);
    } finally {
      NodeFS.rmSync(runRoot, { recursive: true, force: true });
      NodeFS.rmSync(artifactDirectory, { recursive: true, force: true });
    }
  });
});

describe("deferDesktopUiTestContextCleanupUntilExit", () => {
  it("waits for launcher services to stop before cleaning the shared fixture", () => {
    const context = {} as DesktopUiTestContext;
    let exitListener: (() => void) | undefined;
    let cleanedContext: DesktopUiTestContext | undefined;
    const exitRegistrar: DesktopUiExitRegistrar = {
      once: (event, listener) => {
        expect(event).toBe("exit");
        exitListener = listener;
      },
    };

    deferDesktopUiTestContextCleanupUntilExit(context, exitRegistrar, (cleaned) => {
      cleanedContext = cleaned;
    });

    expect(cleanedContext).toBeUndefined();
    expect(exitListener).toBeTypeOf("function");
    exitListener?.();
    expect(cleanedContext).toBe(context);
  });
});
