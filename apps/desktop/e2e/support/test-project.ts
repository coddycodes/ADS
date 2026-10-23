// @effect-diagnostics nodeBuiltinImport:off - The packaged smoke fixture owns host temp files.
import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";

import { desktopActivityFixture, desktopActivityMarkerFileName } from "./activity-events.ts";

const FIXTURE_PROJECT_NAME = "BiBCode UI Fixture";
const STREAMED_RESPONSE = "BiBCode deterministic streamed fixture response.";
const SLOW_TURN_RELEASE_FILE_NAME = ".bibcode-e2e-slow-turn-release";

const nativeActionLogFixtureSource = String.raw`
import fs from "node:fs";

const path = process.env.BIBCODE_E2E_NATIVE_ACTION_LOG;
if (!path) {
  throw new Error("BIBCODE_E2E_NATIVE_ACTION_LOG is required.");
}
fs.appendFileSync(path, JSON.stringify({
  action: "openInEditor",
  args: process.argv.slice(2),
}) + "\n");
`.trimStart();

export const composerProviderProfiles = {
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
} as const;

export interface DesktopUiTestContext {
  readonly runRoot: string;
  readonly stateRoot: string;
  readonly projectPath: string;
  readonly shimDirectory: string;
  readonly artifactDirectory: string;
  readonly fixtureUserHomePath: string;
  readonly nativeActionLogPath: string;
  readonly providerInputLogPath: string;
}

export type DesktopUiDirectoryRemover = (path: string, options: NodeFS.RmDirOptions) => void;

export interface DesktopUiExitRegistrar {
  once(event: "exit", listener: () => void): unknown;
}

export type DesktopUiTestContextCleaner = (context: DesktopUiTestContext) => void;

const codexFixtureSource = String.raw`
import crypto from "node:crypto";
import fs from "node:fs";
import net from "node:net";
import readline from "node:readline";
import { appendProviderInput, promptTextFromParts } from "./provider-input-log-fixture.mjs";

if (process.argv.includes("--version")) {
  process.stdout.write("codex-cli 99.0.0\n");
  process.exit(0);
}
if (process.argv.includes("--help")) {
  if (process.argv.includes("app-server")) {
    process.stdout.write("Usage: codex app-server --listen unix://PATH\n");
  } else {
    process.stdout.write("Usage: codex [--remote unix://PATH]\n");
  }
  process.exit(0);
}

const send = (message) => process.stdout.write(JSON.stringify(message) + "\n");
const streamResponse = ${JSON.stringify(STREAMED_RESPONSE)};
const activity = ${JSON.stringify(desktopActivityFixture)};
const activityStartedAt = Math.floor(Date.now() / 1000);
const activityObservedAt = () => Math.max(activityStartedAt, Math.floor(Date.now() / 1000));
const activityActor = (revision) => ({
  id: activity.actor.id,
  parentThreadId: activity.rootThreadId,
  agentNickname: activity.actor.name,
  agentRole: activity.actor.role,
  createdAt: activityStartedAt,
  updatedAt: activityObservedAt(),
  status: revision > 1 ? { type: "idle" } : { type: "active", activeFlags: [] },
});
const composerActivityActor = (revision) => ({
  id: activity.actor.id + "-composer",
  parentThreadId: activity.rootThreadId,
  agentNickname: "Composer focus observer",
  agentRole: "observer",
  createdAt: activityStartedAt,
  updatedAt: activityObservedAt(),
  status: { type: "active", activeFlags: [] },
});
const fixtureProjectPath = process.env.BIBCODE_E2E_PROJECT_PATH ?? process.cwd();
const activityMarker = fixtureProjectPath + "/" + ${JSON.stringify(desktopActivityMarkerFileName)};
const activityMarkerRevision = () => {
  try {
    return Number.parseInt(fs.readFileSync(activityMarker, "utf8"), 10) || 0;
  } catch {
    return 0;
  }
};
let activityEnabled = false;
let activityRootBound = false;
let activityRevision = 0;

const activityResult = (method, params = {}) => {
  const sharedRevision = activityRootBound ? activityMarkerRevision() : 0;
  const revision = Math.max(activityRevision, sharedRevision);
  const enabled = activityEnabled || (activityRootBound && revision > 0);
  const actors = enabled
    ? [activityActor(revision), ...(revision >= 3 ? [composerActivityActor(revision)] : [])]
    : [];
  if (method === "thread/list") {
    if (typeof params.cwd === "string") {
      const now = Date.now();
      return { data: [{
        id: activity.rootThreadId,
        sessionId: activity.rootThreadId,
        forkedFromId: null,
        parentThreadId: null,
        source: "cli",
        threadSource: "user",
        cwd: params.cwd,
        createdAt: now,
        updatedAt: now,
        status: { type: "active", activeFlags: [] },
      }], nextCursor: null };
    }
    return { data: actors, nextCursor: null };
  }
  if (method === "thread/read") {
    const actor = actors.find((candidate) => candidate.id === params.threadId) ?? actors[0] ?? null;
    return { thread: actor === null ? null : { ...actor, turns: [] } };
  }
  if (method === "thread/backgroundTerminals/list") {
    return { data: enabled && revision <= 1 ? [{
      itemId: activity.backgroundTask.id,
      processId: "fixture-process",
      command: activity.backgroundTask.command,
    }] : [], nextCursor: null };
  }
  return null;
};

const listenArgumentIndex = process.argv.indexOf("--listen");
if (process.argv.includes("app-server") && listenArgumentIndex >= 0) {
  const endpoint = process.argv[listenArgumentIndex + 1] ?? "";
  const socketPath = endpoint.startsWith("unix://") ? endpoint.slice("unix://".length) : "";
  if (!socketPath) process.exit(2);
  try {
    fs.unlinkSync(socketPath);
  } catch {}
  const encodeFrame = (text) => {
    const payload = Buffer.from(text);
    if (payload.length < 126) {
      return Buffer.concat([Buffer.from([0x81, payload.length]), payload]);
    }
    const header = Buffer.alloc(4);
    header[0] = 0x81;
    header[1] = 126;
    header.writeUInt16BE(payload.length, 2);
    return Buffer.concat([header, payload]);
  };
  const decodeFrames = (state, chunk, onText) => {
    state.buffer = Buffer.concat([state.buffer, chunk]);
    while (state.buffer.length >= 2) {
      const masked = (state.buffer[1] & 0x80) !== 0;
      let length = state.buffer[1] & 0x7f;
      let offset = 2;
      if (length === 126) {
        if (state.buffer.length < 4) return;
        length = state.buffer.readUInt16BE(2);
        offset = 4;
      }
      const maskLength = masked ? 4 : 0;
      if (state.buffer.length < offset + maskLength + length) return;
      const mask = masked ? state.buffer.subarray(offset, offset + 4) : null;
      offset += maskLength;
      const payload = Buffer.from(state.buffer.subarray(offset, offset + length));
      if (mask) {
        for (let index = 0; index < payload.length; index += 1) {
          payload[index] ^= mask[index % 4];
        }
      }
      state.buffer = state.buffer.subarray(offset + length);
      onText(payload.toString("utf8"));
    }
  };
  const clients = new Set();
  const server = net.createServer((socket) => {
    let upgraded = false;
    let handshake = Buffer.alloc(0);
    const state = { buffer: Buffer.alloc(0) };
    socket.on("data", (chunk) => {
      if (!upgraded) {
        handshake = Buffer.concat([handshake, chunk]);
        const boundary = handshake.indexOf("\r\n\r\n");
        if (boundary < 0) return;
        const headers = handshake.subarray(0, boundary).toString("utf8");
        const key = headers.match(/^Sec-WebSocket-Key:\s*([^\r\n]+)$/im)?.[1]?.trim();
        if (!key) return socket.destroy();
        const accept = crypto
          .createHash("sha1")
          .update(key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11")
          .digest("base64");
        socket.write(
          "HTTP/1.1 101 Switching Protocols\r\n" +
            "Upgrade: websocket\r\nConnection: Upgrade\r\n" +
            "Sec-WebSocket-Accept: " + accept + "\r\n\r\n",
        );
        upgraded = true;
        clients.add(socket);
        const remainder = handshake.subarray(boundary + 4);
        handshake = Buffer.alloc(0);
        if (remainder.length > 0) decodeFrames(state, remainder, onText);
        return;
      }
      decodeFrames(state, chunk, onText);
    });
    const onText = (text) => {
      const message = JSON.parse(text);
      if (message.id === undefined) return;
      let result = activityResult(message.method, message.params);
      if (message.method === "initialize") {
        result = {
          userAgent: "bibcode-ui-fixture",
          codexHome: "/tmp/bibcode-ui-codex",
          platformFamily: process.platform === "win32" ? "windows" : "unix",
          platformOs: process.platform,
        };
      } else if (message.method === "thread/resume") {
        activityRootBound = message.params?.threadId === activity.rootThreadId;
        result = {
          thread: {
            id: activity.rootThreadId,
            parentThreadId: null,
            createdAt: Date.now(),
            updatedAt: Date.now(),
            status: { type: "active", activeFlags: [] },
            turns: [],
          },
        };
      }
      socket.write(encodeFrame(JSON.stringify({ id: message.id, result: result ?? {} })));
    };
    socket.on("close", () => clients.delete(socket));
  });
  server.listen(socketPath);
  let publishedRevision = activityMarkerRevision();
  const markerPoll = setInterval(() => {
    if (!activityRootBound) return;
    const revision = activityMarkerRevision();
    if (revision <= publishedRevision) return;
    publishedRevision = revision;
    const notification = encodeFrame(JSON.stringify({
      method: "turn/completed",
      params: {
        threadId: activity.actor.id,
        turn: {
          id: "bibcode-ui-reviewer-followup-turn",
          status: "completed",
          startedAt: activityStartedAt,
          completedAt: activityObservedAt(),
        },
      },
      emittedAtMs: Date.now(),
    }));
    for (const socket of clients) socket.write(notification);
  }, 25);
  markerPoll.unref();
  const shutdown = () => {
    clearInterval(markerPoll);
    server.close(() => process.exit(0));
  };
  process.on("SIGTERM", shutdown);
  process.on("SIGINT", shutdown);
} else {
const reader = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
const slowTurnReleasePath = fixtureProjectPath + "/" + ${JSON.stringify(SLOW_TURN_RELEASE_FILE_NAME)};
// The marker releases a specific turn after reload assertions; the watchdog
// prevents a failed scenario from leaving its fixture working indefinitely.
const slowTurnTimeoutMs = Number(process.env.BIBCODE_E2E_SLOW_TURN_MS ?? 60000);
if (!Number.isSafeInteger(slowTurnTimeoutMs) || slowTurnTimeoutMs <= 0) {
  throw new Error("BIBCODE_E2E_SLOW_TURN_MS must be a positive integer.");
}
let activeTurnId = null;
let slowTurnTimer;
let slowTurnPoll;
const clearSlowTurn = () => {
  clearTimeout(slowTurnTimer);
  clearInterval(slowTurnPoll);
};
const completeTurn = (status = "completed") => {
  clearSlowTurn();
  const turnId = activeTurnId;
  activeTurnId = null;
  if (turnId === null) return;
  send({ method: "turn/completed", params: {
    threadId: "bibcode-ui-provider-thread",
    turn: { id: turnId, status }
  } });
};
reader.on("close", clearSlowTurn);

reader.on("line", (line) => {
  const message = JSON.parse(line);
  const id = message.id;
  switch (message.method) {
    case "initialize":
      send({ id, result: {
        userAgent: "bibcode-ui-fixture",
        codexHome: "/tmp/bibcode-ui-codex",
        platformFamily: process.platform === "win32" ? "windows" : "unix",
        platformOs: process.platform
      } });
      break;
    case "account/read":
      send({ id, result: {
        account: { type: "chatgpt", email: "fixture@example.test", planType: "fixture" },
        requiresOpenaiAuth: false
      } });
      break;
    case "model/list":
      send({ id, result: {
        data: message.params?.cursor ? [] : [{
          model: "gpt-5.4",
          displayName: "gpt-5.4",
          supportedReasoningEfforts: [{ reasoningEffort: "medium" }],
          defaultReasoningEffort: "medium"
        }],
        nextCursor: null
      } });
      break;
    case "skills/list":
      send({ id, result: {
        data: [{
          cwd: message.params?.cwds?.[0] ?? process.cwd(),
          skills: [{
            name: "refactor",
            path: "/fixture/.codex/skills/refactor/SKILL.md",
            enabled: true,
            description: "Refactor the deterministic fixture.",
            scope: "project",
            interface: {
              displayName: "Refactor",
              shortDescription: "Refactor the deterministic fixture."
            }
          }]
        }]
      } });
      break;
    case "thread/start":
      send({ id, result: {
        cwd: message.params?.cwd ?? process.cwd(),
        model: "gpt-5.4",
        thread: { id: "bibcode-ui-provider-thread" }
      } });
      break;
    case "thread/resume":
      activityRootBound = message.params?.threadId === activity.rootThreadId;
      send({ id, result: {
        cwd: message.params?.cwd ?? process.cwd(),
        model: "gpt-5.4",
        thread: { id: "bibcode-ui-provider-thread" }
      } });
      break;
    case "thread/goal/set":
      send({ id, result: { goal: { status: "active" } } });
      break;
    case "thread/list":
      send({ id, result: activityResult(message.method, message.params) });
      break;
    case "thread/read":
      send({ id, result: activityResult(message.method, message.params) });
      break;
    case "thread/backgroundTerminals/list":
      send({ id, result: activityResult(message.method, message.params) });
      break;
    case "turn/start": {
      if (activeTurnId !== null) {
        send({ id, error: { code: -32600, message: "A fixture turn is already running." } });
        break;
      }
      const prompt = promptTextFromParts(message.params?.input);
      if (prompt === "load deterministic activity") {
        activityEnabled = true;
        activityRootBound = true;
        activityRevision = 1;
        if (activityMarker) fs.writeFileSync(activityMarker, "1");
      } else if (
        (activityEnabled || (activityRootBound && activityMarkerRevision() > 0)) &&
        prompt === "publish deterministic live activity update"
      ) {
        activityRevision = 2;
        if (activityMarker) fs.writeFileSync(activityMarker, "2");
      } else if (
        (activityEnabled || (activityRootBound && activityMarkerRevision() > 0)) &&
        prompt === "publish deterministic composer activity update"
      ) {
        activityRevision = 3;
        if (activityMarker) fs.writeFileSync(activityMarker, "3");
      }
      const turnId = "bibcode-ui-turn-" + crypto.randomUUID();
      activeTurnId = turnId;
      appendProviderInput("codex", prompt, "start", turnId);
      send({ id, result: { turn: { id: turnId } } });
      send({ method: "turn/started", params: {
        threadId: "bibcode-ui-provider-thread",
        turn: { id: turnId }
      } });
      send({ method: "item/agentMessage/delta", params: {
        threadId: "bibcode-ui-provider-thread",
        turnId,
        itemId: "bibcode-ui-message-" + turnId,
        delta: streamResponse
      } });
      if (prompt.includes("[[slow]]")) {
        slowTurnTimer = setTimeout(completeTurn, slowTurnTimeoutMs);
        slowTurnPoll = setInterval(() => {
          try {
            if (fs.readFileSync(slowTurnReleasePath, "utf8") !== turnId) return;
            fs.rmSync(slowTurnReleasePath, { force: true });
            completeTurn();
          } catch (error) {
            if (error.code !== "ENOENT") throw error;
          }
        }, 25);
      } else {
        completeTurn();
      }
      break;
    }
    case "turn/steer": {
      if (activeTurnId === null || message.params?.expectedTurnId !== activeTurnId) {
        send({ id, error: { code: -32600, message: "Steer requires the active turn ID." } });
        break;
      }
      appendProviderInput("codex", promptTextFromParts(message.params?.input), "steer", activeTurnId);
      send({ id, result: { turnId: activeTurnId } });
      break;
    }
    case "turn/interrupt":
      send({ id, result: null });
      completeTurn("interrupted");
      break;
    case "shutdown":
      clearSlowTurn();
      send({ id, result: null });
      break;
    default:
      if (id !== undefined) send({ id, result: {} });
  }
});
}
`.trimStart();

const providerInputLogFixtureSource = String.raw`
import * as NodeFS from "node:fs";

export function promptTextFromParts(parts) {
  return (Array.isArray(parts) ? parts : [])
    .filter((part) => part?.type === "text" && typeof part.text === "string")
    .map((part) => part.text)
    .join("");
}

export function appendProviderInput(provider, prompt, kind = "start", turnId) {
  const path = process.env.BIBCODE_E2E_PROVIDER_INPUT_LOG;
  if (!path) {
    throw new Error("BIBCODE_E2E_PROVIDER_INPUT_LOG is required.");
  }
  NodeFS.appendFileSync(path, JSON.stringify({
    provider,
    kind,
    prompt,
    ...(turnId === undefined ? {} : { turnId }),
    recordedAt: new Date().toISOString()
  }) + "\n", "utf8");
}
`.trimStart();

const claudeFixtureSource = String.raw`
import readline from "node:readline";
import { appendProviderInput, promptTextFromParts } from "./provider-input-log-fixture.mjs";

if (process.argv.includes("--version")) {
  process.stdout.write("2.1.200 (Claude Code)\n");
  process.exit(0);
}
if (process.argv.includes("auth") && process.argv.includes("status")) {
  process.stdout.write(JSON.stringify({
    loggedIn: true,
    email: "fixture@example.test",
    authMethod: "fixture"
  }) + "\n");
  process.exit(0);
}

const send = (message) => process.stdout.write(JSON.stringify(message) + "\n");
const streamResponse = ${JSON.stringify(STREAMED_RESPONSE)};
const reader = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });

reader.on("line", (line) => {
  const message = JSON.parse(line);
  if (message.type === "control_request") {
    const request = message.request ?? {};
    const subtype = request.subtype ?? request.request?.subtype;
    const requestId = message.request_id ?? request.request_id;
    if (subtype === "initialize" && requestId) {
      send({
        type: "control_response",
        response: {
          request_id: requestId,
          subtype: "success",
          response: {
            commands: [{
              name: "compact",
              description: "Compact the deterministic fixture context."
            }, {
              name: "docs",
              description: "Use deterministic fixture documentation."
            }],
            agents: [{
              name: "claude-prose-agent",
              description: "Prose-only upstream agent without inline invocation metadata."
            }],
            models: [{
              value: "opus",
              resolvedModel: "claude-opus-5",
              displayName: "Opus",
              description: "Opus 5 · Best for everyday, complex tasks",
              supportsEffort: true,
              supportedEffortLevels: ["low", "medium", "high"],
              supportsFastMode: true
            }, {
              value: "sonnet",
              resolvedModel: "claude-sonnet-5",
              displayName: "Sonnet",
              description: "Sonnet 5 · Efficient for routine tasks",
              supportsEffort: true,
              supportedEffortLevels: ["low", "medium", "high"]
            }]
          }
        }
      });
    } else if (subtype === "reload_skills" && requestId) {
      send({
        type: "control_response",
        response: {
          request_id: requestId,
          subtype: "success",
          response: {
            skills: [{
              name: "docs",
              description: "Use deterministic fixture documentation."
            }]
          }
        }
      });
    }
    return;
  }
  if (message.type !== "user") {
    return;
  }
  const sessionId = message.session_id ?? "bibcode-ui-claude-session";
  appendProviderInput("claudeAgent", promptTextFromParts(message.message?.content));
  send(message);
  send({
    type: "stream_event",
    session_id: sessionId,
    uuid: "bibcode-ui-claude-stream",
    parent_tool_use_id: null,
    event: {
      type: "content_block_delta",
      index: 0,
      delta: { type: "text_delta", text: streamResponse }
    }
  });
  send({
    type: "result",
    subtype: "success",
    is_error: false,
    errors: [],
    stop_reason: "end_turn",
    session_id: sessionId,
    uuid: "bibcode-ui-claude-result"
  });
});
`.trimStart();

const cursorFixtureSource = String.raw`
import readline from "node:readline";
import { appendProviderInput, promptTextFromParts } from "./provider-input-log-fixture.mjs";

if (process.argv.includes("--version")) {
  process.stdout.write("cursor-agent 99.0.0-fixture\n");
  process.exit(0);
}
if (process.argv.includes("about")) {
  process.stdout.write(JSON.stringify({
    cliVersion: "99.0.0-fixture",
    userEmail: "fixture@example.test",
    subscriptionTier: "fixture"
  }) + "\n");
  process.exit(0);
}

const send = (id, result) => process.stdout.write(JSON.stringify({
  jsonrpc: "2.0",
  id,
  result
}) + "\n");
const reader = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });

reader.on("line", (line) => {
  const message = JSON.parse(line);
  switch (message.method) {
    case "initialize":
    case "authenticate":
    case "session/set_mode":
      send(message.id, {});
      break;
    case "cursor/list_available_models":
      send(message.id, {
        models: [{
          value: "cursor-fixture",
          name: "Cursor Fixture",
          configOptions: []
        }]
      });
      break;
    case "session/new":
    case "session/load":
      send(message.id, {
        sessionId: message.params?.sessionId ?? "bibcode-ui-cursor-session",
        configOptions: [{
          id: "model",
          name: "Model",
          category: "model",
          currentValue: "cursor-fixture",
          options: [{ value: "cursor-fixture", name: "Cursor Fixture" }]
        }],
        modes: {
          currentModeId: "ask",
          availableModes: [
            { id: "ask", name: "Ask" },
            { id: "code", name: "Agent" },
            { id: "architect", name: "Plan" }
          ]
        }
      });
      break;
    case "session/set_config_option":
      send(message.id, { configOptions: [] });
      break;
    case "session/prompt":
      appendProviderInput("cursor", promptTextFromParts(message.params?.prompt));
      send(message.id, { stopReason: "end_turn" });
      break;
    case "session/cancel":
      send(message.id, {});
      break;
    default:
      if (message.id !== undefined) {
        send(message.id, {});
      }
  }
});
`.trimStart();

const grokFixtureSource = String.raw`
import readline from "node:readline";
import { appendProviderInput, promptTextFromParts } from "./provider-input-log-fixture.mjs";

if (process.argv.includes("--version")) {
  process.stdout.write("grok-cli 99.0.0-fixture\n");
  process.exit(0);
}

const send = (id, result) => process.stdout.write(JSON.stringify({
  jsonrpc: "2.0",
  id,
  result
}) + "\n");
const reader = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });

reader.on("line", (line) => {
  const message = JSON.parse(line);
  switch (message.method) {
    case "initialize":
    case "authenticate":
    case "session/set_mode":
    case "session/set_model":
      send(message.id, {});
      break;
    case "session/create":
    case "session/load":
      send(message.id, {
        sessionId: message.params?.sessionId ?? "bibcode-ui-grok-session",
        modes: {
          currentModeId: "code",
          availableModes: [
            { id: "code", name: "Agent" },
            { id: "ask", name: "Ask" }
          ]
        }
      });
      break;
    case "session/prompt":
      appendProviderInput("grok", promptTextFromParts(message.params?.prompt));
      send(message.id, { stopReason: "end_turn" });
      break;
    case "session/cancel":
      send(message.id, {});
      break;
    default:
      if (message.id !== undefined) {
        send(message.id, {});
      }
  }
});
`.trimStart();

const opencodeFixtureSource = String.raw`
import * as NodeHTTP from "node:http";
import { appendProviderInput, promptTextFromParts } from "./provider-input-log-fixture.mjs";

// The fixture serves OpenCode 1's HTTP API, so it reports a 1.x version: the server drives
// OpenCode by major version and treats 3.x and later as unsupported.
if (process.argv.includes("--version")) {
  process.stdout.write("opencode 1.99.0-fixture\n");
  process.exit(0);
}

const portArgument = process.argv.find((argument) => argument.startsWith("--port="));
const port = Number(portArgument?.slice("--port=".length));
if (!Number.isInteger(port) || port <= 0) {
  throw new Error("OpenCode fixture requires --port=<port>.");
}

const eventClients = new Set();
const sendJson = (response, value, status = 200) => {
  response.writeHead(status, { "content-type": "application/json" });
  response.end(JSON.stringify(value));
};
const readJson = async (request) => {
  const chunks = [];
  for await (const chunk of request) {
    chunks.push(chunk);
  }
  return chunks.length === 0
    ? {}
    : JSON.parse(Buffer.concat(chunks).toString("utf8"));
};
const broadcast = (event) => {
  const frame = "data: " + JSON.stringify(event) + "\n\n";
  for (const response of eventClients) {
    response.write(frame);
  }
};

const server = NodeHTTP.createServer(async (request, response) => {
  const pathname = new URL(request.url ?? "/", "http://127.0.0.1").pathname;
  if (request.method === "GET" && pathname === "/global/health") {
    sendJson(response, { healthy: true });
    return;
  }
  if (request.method === "GET" && pathname === "/provider") {
    sendJson(response, {
      connected: ["openai"],
      all: [{
        id: "openai",
        models: {
          "gpt-5": { name: "GPT-5 Fixture" }
        }
      }]
    });
    return;
  }
  if (request.method === "GET" && pathname === "/agent") {
    sendJson(response, [
      { name: "writer", mode: "primary", description: "Prose-only primary agent." },
      { name: "reviewer", mode: "subagent", description: "Mentionable review agent." },
      { name: "operator", mode: "all", description: "Mentionable all-mode agent." },
      { name: "secret", mode: "subagent", hidden: true, description: "Hidden agent." }
    ]);
    return;
  }
  if (request.method === "GET" && pathname === "/command") {
    sendJson(response, [{
      name: "init",
      description: "Initialize the deterministic fixture."
    }]);
    return;
  }
  if (request.method === "GET" && pathname === "/event") {
    response.writeHead(200, {
      "content-type": "text/event-stream",
      "cache-control": "no-cache",
      connection: "keep-alive"
    });
    response.write(": connected\n\n");
    eventClients.add(response);
    request.on("close", () => eventClients.delete(response));
    return;
  }
  if (request.method === "POST" && pathname === "/session") {
    await readJson(request);
    sendJson(response, { id: "bibcode-ui-opencode-session" });
    return;
  }
  if (request.method === "GET" && pathname === "/session/bibcode-ui-opencode-session") {
    sendJson(response, { id: "bibcode-ui-opencode-session" });
    return;
  }
  if (
    request.method === "POST" &&
    pathname === "/session/bibcode-ui-opencode-session/prompt_async"
  ) {
    const body = await readJson(request);
    appendProviderInput("opencode", promptTextFromParts(body.parts));
    sendJson(response, {});
    setTimeout(() => {
      broadcast({
        type: "session.status",
        properties: {
          sessionID: "bibcode-ui-opencode-session",
          status: { type: "idle" }
        }
      });
    }, 10);
    return;
  }
  if (
    request.method === "POST" &&
    pathname === "/session/bibcode-ui-opencode-session/command"
  ) {
    const body = await readJson(request);
    appendProviderInput(
      "opencode",
      "/" + String(body.command ?? "") +
        (body.arguments ? " " + String(body.arguments) : "")
    );
    sendJson(response, {});
    setTimeout(() => {
      broadcast({
        type: "session.status",
        properties: {
          sessionID: "bibcode-ui-opencode-session",
          status: { type: "idle" }
        }
      });
    }, 10);
    return;
  }
  if (request.method === "POST" && pathname.endsWith("/abort")) {
    sendJson(response, {});
    return;
  }
  if (request.method === "GET" && pathname.endsWith("/message")) {
    sendJson(response, { data: [] });
    return;
  }
  if (request.method === "POST" && pathname.endsWith("/revert")) {
    sendJson(response, {});
    return;
  }
  if (request.method === "POST" && pathname === "/bibcode-fixture/shutdown") {
    sendJson(response, { shuttingDown: true });
    setTimeout(close, 0);
    return;
  }
  sendJson(response, {});
});

server.listen(port, "127.0.0.1");
const close = () => {
  for (const response of eventClients) {
    response.end();
  }
  eventClients.clear();
  server.close(() => process.exit(0));
};
process.on("SIGINT", close);
process.on("SIGTERM", close);
`.trimStart();

function writeExecutable(path: string, contents: string, isWindows: boolean): void {
  NodeFS.writeFileSync(path, contents);
  if (!isWindows) {
    NodeFS.chmodSync(path, 0o755);
  }
}

function createProviderShims(shimDirectory: string, isWindows: boolean): void {
  const fixtureSources = {
    codex: codexFixtureSource,
    claude: claudeFixtureSource,
    "cursor-agent": cursorFixtureSource,
    grok: grokFixtureSource,
    opencode: opencodeFixtureSource,
  } as const;
  NodeFS.writeFileSync(
    NodePath.join(shimDirectory, "provider-input-log-fixture.mjs"),
    providerInputLogFixtureSource,
  );
  NodeFS.writeFileSync(
    NodePath.join(shimDirectory, "native-action-log-fixture.mjs"),
    nativeActionLogFixtureSource,
  );
  for (const [provider, source] of Object.entries(fixtureSources)) {
    NodeFS.writeFileSync(NodePath.join(shimDirectory, `${provider}-fixture.mjs`), source);
  }

  if (isWindows) {
    NodeFS.writeFileSync(
      NodePath.join(shimDirectory, "cursor.cmd"),
      `@"${process.execPath}" "%~dp0\\native-action-log-fixture.mjs" %*\r\n`,
    );
    for (const provider of Object.keys(fixtureSources)) {
      const probeCommands =
        provider === "codex"
          ? [
              '@if "%~1"=="--version" goto bibcode_codex_version',
              '@if "%~1"=="--help" goto bibcode_codex_help',
              '@if "%~1"=="app-server" if "%~2"=="--help" goto bibcode_codex_app_server_help',
              "@goto bibcode_codex_run",
              ":bibcode_codex_version",
              "@echo codex-cli 99.0.0",
              "@exit /b 0",
              ":bibcode_codex_help",
              "@echo Usage: codex --remote unix://PATH",
              "@exit /b 0",
              ":bibcode_codex_app_server_help",
              "@echo Usage: codex app-server --listen unix://PATH",
              "@exit /b 0",
              ":bibcode_codex_run",
            ].join("\r\n")
          : "";
      NodeFS.writeFileSync(
        NodePath.join(shimDirectory, `${provider}.cmd`),
        `${probeCommands}${probeCommands.length > 0 ? "\r\n" : ""}@node "%~dp0\\${provider}-fixture.mjs" %*\r\n`,
      );
    }
    return;
  }

  writeExecutable(
    NodePath.join(shimDirectory, "cursor"),
    '#!/bin/sh\nexec node "$(dirname "$0")/native-action-log-fixture.mjs" "$@"\n',
    false,
  );

  for (const provider of Object.keys(fixtureSources)) {
    const probeCommands =
      provider === "codex"
        ? String.raw`if [ "$1" = "--version" ]; then
  printf '%s\n' 'codex-cli 99.0.0'
  exit 0
fi
if [ "$1" = "--help" ]; then
  printf '%s\n' 'Usage: codex --remote unix://PATH'
  exit 0
fi
if [ "$1" = "app-server" ] && [ "$2" = "--help" ]; then
  printf '%s\n' 'Usage: codex app-server --listen unix://PATH'
  exit 0
fi
`
        : "";
    writeExecutable(
      NodePath.join(shimDirectory, provider),
      `#!/bin/sh\n${probeCommands}exec node "$(dirname "$0")/${provider}-fixture.mjs" "$@"\n`,
      false,
    );
  }
}

function writeProviderSettings(
  stateRoot: string,
  shimDirectory: string,
  fixtureUserHomePath: string,
  isWindows: boolean,
): void {
  const executablePath = (name: string): string =>
    NodePath.join(shimDirectory, isWindows ? `${name}.cmd` : name);
  const userDataDirectory = NodePath.join(stateRoot, "userdata");
  NodeFS.mkdirSync(userDataDirectory, { recursive: true });
  NodeFS.writeFileSync(
    NodePath.join(userDataDirectory, "settings.json"),
    `${JSON.stringify(
      {
        providers: {
          codex: { enabled: true, binaryPath: executablePath("codex") },
          claudeAgent: { enabled: true, binaryPath: executablePath("claude") },
          cursor: { enabled: true, binaryPath: executablePath("cursor-agent") },
          grok: { enabled: true, binaryPath: executablePath("grok") },
          opencode: { enabled: true, binaryPath: executablePath("opencode") },
        },
        providerInstances: {
          cursor: {
            driver: "cursor",
            enabled: true,
            environment: [
              {
                name: isWindows ? "USERPROFILE" : "HOME",
                value: fixtureUserHomePath,
                sensitive: false,
              },
            ],
          },
        },
      },
      null,
      2,
    )}\n`,
  );
}

function runGit(projectPath: string, args: ReadonlyArray<string>): void {
  const result = NodeChildProcess.spawnSync(
    "git",
    [
      "-C",
      projectPath,
      "-c",
      "user.name=BiBCode UI Fixture",
      "-c",
      "user.email=fixture@example.test",
      ...args,
    ],
    { encoding: "utf8", shell: false },
  );
  if (result.status !== 0) {
    throw new Error(`UI fixture Git command failed: ${result.stderr}`);
  }
}

const cursorInventoryFiles = {
  ".cursor/commands/review.md": "# Review\n\nReview the deterministic fixture.\n",
  ".cursor/skills/frontend/SKILL.md": [
    "---",
    "name: frontend",
    "description: Exercise the deterministic frontend fixture.",
    "---",
    "",
    "# Frontend",
    "",
    "Exercise the packaged frontend fixture.",
    "",
  ].join("\n"),
  ".cursor/agents/cursor-prose-agent.md": [
    "# Cursor prose agent",
    "",
    "This upstream fixture intentionally has no inline invocation metadata.",
    "",
  ].join("\n"),
} as const;

function writeFixtureFiles(root: string, files: Readonly<Record<string, string>>): void {
  for (const [relativePath, contents] of Object.entries(files)) {
    const path = NodePath.join(root, relativePath);
    NodeFS.mkdirSync(NodePath.dirname(path), { recursive: true });
    NodeFS.writeFileSync(path, contents);
  }
}

function initializeGitProject(projectPath: string): void {
  NodeFS.mkdirSync(projectPath, { recursive: true });
  NodeFS.writeFileSync(
    NodePath.join(projectPath, "README.md"),
    "# BiBCode packaged desktop UI fixture\n",
  );
  const projectFiles = {
    ".claude/skills/docs/SKILL.md": [
      "---",
      "name: docs",
      "description: Use deterministic fixture documentation.",
      "---",
      "",
      "# Docs",
      "",
      "Use the packaged fixture documentation.",
      "",
    ].join("\n"),
    ...cursorInventoryFiles,
  } as const;
  writeFixtureFiles(projectPath, projectFiles);
  if (NodeFS.existsSync(NodePath.join(projectPath, ".git"))) return;
  runGit(projectPath, ["init", "--initial-branch=main"]);
  runGit(projectPath, ["add", "."]);
  runGit(projectPath, ["commit", "-m", "fixture"]);
}

export function clearDesktopActivityMarker(projectPath: string): void {
  NodeFS.rmSync(NodePath.join(projectPath, desktopActivityMarkerFileName), { force: true });
}

export function completeDesktopUiSlowTurn(projectPath: string, turnId: string): void {
  NodeFS.writeFileSync(NodePath.join(projectPath, SLOW_TURN_RELEASE_FILE_NAME), turnId);
}

function desktopUiHostTemporaryDirectory(): string {
  // oxlint-disable-next-line bibcode/no-global-process-runtime -- The standalone WDIO fixture selects its automatic run root from the actual host, not the simulated target.
  return NodeOS.platform() === "win32" ? NodeOS.tmpdir() : "/tmp";
}

export function prepareDesktopUiTestContext(
  environment: NodeJS.ProcessEnv = process.env,
  hostTemporaryDirectory: string = desktopUiHostTemporaryDirectory(),
): DesktopUiTestContext {
  // oxlint-disable-next-line bibcode/no-global-process-runtime -- The standalone WDIO fixture injects the detected host into its adapters.
  const hostPlatform = environment.BIBCODE_E2E_PLATFORM ?? process.platform;
  const isWindows = hostPlatform === "win" || hostPlatform === "win32";
  const runRoot =
    environment.BIBCODE_E2E_RUN_ROOT ??
    NodeFS.mkdtempSync(NodePath.join(hostTemporaryDirectory, "t4ui-"));
  const artifactDirectory =
    environment.BIBCODE_E2E_ARTIFACT_DIR ??
    NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "bibcode-desktop-ui-artifacts-"));
  const stateRoot = NodePath.join(runRoot, "state");
  const projectPath = NodePath.join(runRoot, "projects with spaces", FIXTURE_PROJECT_NAME);
  const shimDirectory = NodePath.join(runRoot, "provider-shims");
  const fixtureUserHomePath = NodePath.join(runRoot, "fixture-user-home");
  const fixtureAppDataPath = NodePath.join(runRoot, "fixture-appdata", "Roaming");
  const fixtureLocalAppDataPath = NodePath.join(runRoot, "fixture-appdata", "Local");
  const powerShellModuleAnalysisCachePath = NodePath.join(
    fixtureLocalAppDataPath,
    "Microsoft",
    "Windows",
    "PowerShell",
    "ModuleAnalysisCache",
  );
  const nativeActionLogPath = NodePath.join(artifactDirectory, "native-actions.jsonl");
  const providerInputLogPath = NodePath.join(runRoot, "provider-input.jsonl");

  NodeFS.mkdirSync(stateRoot, { recursive: true });
  NodeFS.mkdirSync(shimDirectory, { recursive: true });
  NodeFS.mkdirSync(fixtureUserHomePath, { recursive: true });
  if (isWindows) {
    NodeFS.mkdirSync(fixtureAppDataPath, { recursive: true });
    NodeFS.mkdirSync(NodePath.dirname(powerShellModuleAnalysisCachePath), { recursive: true });
  }
  NodeFS.mkdirSync(artifactDirectory, { recursive: true });
  if (!NodeFS.existsSync(nativeActionLogPath)) {
    NodeFS.writeFileSync(nativeActionLogPath, "");
  }
  NodeFS.writeFileSync(providerInputLogPath, "");
  initializeGitProject(projectPath);
  writeFixtureFiles(fixtureUserHomePath, cursorInventoryFiles);
  createProviderShims(shimDirectory, isWindows);
  writeProviderSettings(stateRoot, shimDirectory, fixtureUserHomePath, isWindows);

  environment.BIBCODE_E2E_RUN_ROOT = runRoot;
  environment.BIBCODE_E2E_ARTIFACT_DIR = artifactDirectory;
  environment.BIBCODE_E2E_PROJECT_PATH = projectPath;
  environment.BIBCODE_E2E_SHIM_DIRECTORY = shimDirectory;
  environment.BIBCODE_E2E_USER_HOME = fixtureUserHomePath;
  environment.BIBCODE_E2E_NATIVE_ACTION_LOG = nativeActionLogPath;
  environment.BIBCODE_E2E_PROVIDER_INPUT_LOG = providerInputLogPath;
  environment.BIBCODE_HOME = stateRoot;
  if (isWindows) {
    environment.APPDATA = fixtureAppDataPath;
    environment.LOCALAPPDATA = fixtureLocalAppDataPath;
    environment.PSModuleAnalysisCachePath = powerShellModuleAnalysisCachePath;
    environment.USERPROFILE = fixtureUserHomePath;
    environment.HOME = fixtureUserHomePath;
  } else {
    environment.HOME = fixtureUserHomePath;
  }
  environment.PATH = `${shimDirectory}${NodePath.delimiter}${environment.PATH ?? ""}`;
  environment.RUST_LOG ??= "bibcode=debug";
  environment.BIBCODE_LOG = `${environment.BIBCODE_LOG ?? environment.RUST_LOG},bibcode::desktop_e2e=info`;

  return {
    runRoot,
    stateRoot,
    projectPath,
    shimDirectory,
    artifactDirectory,
    fixtureUserHomePath,
    nativeActionLogPath,
    providerInputLogPath,
  };
}

export function archiveAndCleanupDesktopUiTestContext(
  context: DesktopUiTestContext,
  removeDirectory: DesktopUiDirectoryRemover = NodeFS.rmSync,
): void {
  clearDesktopActivityMarker(context.projectPath);
  const archivedState = NodePath.join(context.artifactDirectory, "state");
  if (NodeFS.existsSync(context.stateRoot)) {
    NodeFS.cpSync(context.stateRoot, archivedState, { recursive: true, force: true });
  }
  if (NodeFS.existsSync(context.providerInputLogPath)) {
    NodeFS.copyFileSync(
      context.providerInputLogPath,
      NodePath.join(context.artifactDirectory, "provider-input.jsonl"),
    );
  }
  removeDirectory(context.runRoot, {
    recursive: true,
    force: true,
    maxRetries: 10,
    retryDelay: 100,
  });
}

export function deferDesktopUiTestContextCleanupUntilExit(
  context: DesktopUiTestContext,
  exitRegistrar: DesktopUiExitRegistrar,
  cleanContext: DesktopUiTestContextCleaner = archiveAndCleanupDesktopUiTestContext,
): void {
  exitRegistrar.once("exit", () => {
    cleanContext(context);
  });
}

export const desktopUiFixture = {
  projectName: FIXTURE_PROJECT_NAME,
  streamedResponse: STREAMED_RESPONSE,
} as const;
