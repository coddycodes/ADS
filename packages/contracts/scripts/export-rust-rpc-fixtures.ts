// @effect-diagnostics nodeBuiltinImport:off
import * as NodeChildProcess from "node:child_process";
import * as NodeCrypto from "node:crypto";
import * as NodeFSP from "node:fs/promises";
import * as NodePath from "node:path";

import * as DateTime from "effect/DateTime";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";
import type * as SchemaAST from "effect/SchemaAST";
import type * as RpcMessage from "effect/unstable/rpc/RpcMessage";
import * as RpcSerialization from "effect/unstable/rpc/RpcSerialization";
import * as FastCheck from "fast-check";

import {
  ActivityCancelSubtreeInput,
  ActivityRetrySubtreeCancellationInput,
} from "../src/activity.ts";
import { GitCommandError } from "../src/git.ts";
import {
  ClientOrchestrationCommand,
  OrchestrationEvent,
  ORCHESTRATION_WS_METHODS,
} from "../src/orchestration.ts";
import { RemoteUpdateSnapshot } from "../src/remoteUpdate.ts";
import { WS_METHODS, WsRpcGroup } from "../src/rpc.ts";
import { RpcResponseTooLargeError } from "../src/rpcTransport.ts";
import {
  ServerProcessDiagnosticsResult,
  ServerProcessResourceHistoryResult,
  ServerSignalProcessInput,
} from "../src/server.ts";
import { DEFAULT_SERVER_SETTINGS } from "../src/settings.ts";
import { TerminalBeginInput, TerminalOpenInput } from "../src/terminal.ts";

const StreamSchemaTypeId = "~effect/rpc/RpcSchema/StreamSchema";
const requestId = "900719925474099312345";
const compileUnknownEncoder = (
  codec: Schema.Codec<unknown, unknown, never, never>,
): ((value: unknown) => unknown) => Schema.encodeUnknownSync(codec);

const request = {
  _tag: "Request",
  id: requestId,
  tag: "server.getConfig",
  payload: {},
  headers: [
    ["authorization", "Bearer fixture"],
    ["x-bibcode-fixture", "rpc-wire"],
  ],
  traceId: "0123456789abcdef0123456789abcdef",
  spanId: "0123456789abcdef",
  sampled: true,
} satisfies RpcMessage.RequestEncoded;

const fixtures = {
  ack: {
    _tag: "Ack",
    requestId,
  } satisfies RpcMessage.AckEncoded,
  chunk: {
    _tag: "Chunk",
    requestId,
    values: [
      { _tag: "First", value: 1 },
      { _tag: "Second", value: "two" },
    ],
  } satisfies RpcMessage.ResponseChunkEncoded,
  "client-protocol-error": {
    _tag: "ClientProtocolError",
    error: {
      _tag: "RpcClientError",
      reason: {
        _tag: "RpcClientDefect",
        message: "fixture protocol error",
        cause: "fixture cause",
      },
    },
  },
  defect: {
    _tag: "Defect",
    defect: "fixture connection defect",
  } satisfies RpcMessage.ResponseDefectEncoded,
  eof: { _tag: "Eof" } satisfies RpcMessage.Eof,
  "exit-defect": {
    _tag: "Exit",
    requestId,
    exit: {
      _tag: "Failure",
      cause: [{ _tag: "Die", defect: "fixture request defect" }],
    },
  } satisfies RpcMessage.ResponseExitEncoded,
  "exit-failure": {
    _tag: "Exit",
    requestId,
    exit: {
      _tag: "Failure",
      cause: [{ _tag: "Fail", error: { _tag: "FixtureError", message: "typed failure" } }],
    },
  } satisfies RpcMessage.ResponseExitEncoded,
  "exit-interrupt": {
    _tag: "Exit",
    requestId,
    exit: {
      _tag: "Failure",
      cause: [{ _tag: "Interrupt", fiberId: undefined }],
    },
  } satisfies RpcMessage.ResponseExitEncoded,
  "exit-response-too-large": {
    _tag: "Exit",
    requestId,
    exit: {
      _tag: "Failure",
      cause: [
        {
          _tag: "Fail",
          error: Schema.encodeSync(RpcResponseTooLargeError)(
            new RpcResponseTooLargeError({
              method: "gitManager.getCommits",
              bytes: 70_000_000,
              limitBytes: 67_108_864,
            }),
          ),
        },
      ],
    },
  } satisfies RpcMessage.ResponseExitEncoded,
  "exit-stream-success": {
    _tag: "Exit",
    requestId,
    exit: { _tag: "Success", value: null },
  } satisfies RpcMessage.ResponseExitEncoded,
  "exit-success": {
    _tag: "Exit",
    requestId,
    exit: { _tag: "Success", value: { ready: true } },
  } satisfies RpcMessage.ResponseExitEncoded,
  interrupt: {
    _tag: "Interrupt",
    requestId,
  } satisfies RpcMessage.InterruptEncoded,
  ping: { _tag: "Ping" } satisfies RpcMessage.Ping,
  pong: { _tag: "Pong" } satisfies RpcMessage.Pong,
  request,
} as const;

const methods = [...WsRpcGroup.requests.values()]
  .map((rpc) => ({
    name: rpc._tag,
    mode:
      StreamSchemaTypeId in (rpc.successSchema as object)
        ? ("stream" as const)
        : ("unary" as const),
  }))
  .toSorted((left, right) => left.name.localeCompare(right.name));
const activeNames = new Set<string>(methods.map(({ name }) => name));
const knownNames = new Set([
  ...Object.values(WS_METHODS),
  ...Object.values(ORCHESTRATION_WS_METHODS),
]);
const staleMethodIdentifiers = [...knownNames].filter((name) => !activeNames.has(name)).toSorted();

interface UnionLikeAst {
  readonly _tag: string;
  readonly types?: ReadonlyArray<SchemaAST.AST>;
}

const countSchemaShapes = (ast: UnionLikeAst): number => {
  if (ast._tag !== "Union") return 1;
  if (ast.types === undefined) {
    throw new Error("Union schema AST did not expose its member types.");
  }
  return ast.types.length;
};

const topLevelStreamShapeCount = [...WsRpcGroup.requests.values()]
  .filter((rpc) => StreamSchemaTypeId in (rpc.successSchema as object))
  .reduce((count, rpc) => {
    const streamSchema = rpc.successSchema as unknown as {
      readonly success: { readonly ast: UnionLikeAst };
    };
    return count + countSchemaShapes(streamSchema.success.ast);
  }, 0);
const orchestrationEventShapeCount = countSchemaShapes(OrchestrationEvent.ast);

const schemaMembers = (ast: SchemaAST.AST): ReadonlyArray<SchemaAST.AST> =>
  ast._tag === "Union" ? ast.types : [ast];

const fixtureEnvironmentDescriptor = {
  environmentId: "fixture",
  label: "Fixture",
  platform: { os: "windows", arch: "x64" },
  serverVersion: "0.1.1",
  storageInstanceId: "00000000-0000-4000-8000-000000000002",
  bootId: "00000000-0000-4000-8000-000000000003",
  capabilities: {
    repositoryIdentity: true,
    worktreeCatalog: true,
    worktreeCatalogRefreshReason: true,
    vcsStatusSummary: true,
    activityProtocolVersion: 2,
  },
} as const;
const fixtureServerConfig = {
  environment: fixtureEnvironmentDescriptor,
  auth: {
    policy: "loopback-browser",
    bootstrapMethods: ["one-time-token"],
    sessionMethods: ["browser-session-cookie", "bearer-access-token"],
    sessionCookieName: "bibcode_session",
  },
  cwd: "C:\\fixture",
  keybindingsConfigPath: "C:\\fixture\\keybindings.json",
  keybindings: [],
  issues: [],
  providers: [],
  availableEditors: [],
  observability: {
    logsDirectoryPath: "C:\\fixture\\logs",
    localTracingEnabled: false,
    otlpTracesEnabled: false,
    otlpMetricsEnabled: false,
  },
  settings: DEFAULT_SERVER_SETTINGS,
} as const;
const fixtureDate = Option.getOrThrow(DateTime.make(1_767_225_600_000));
const fixtureProcessAttribution = {
  processKey: "42:100",
  scope: "external",
  kind: "provider",
  label: "Codex",
  confidence: "exact",
} as const;
const fixtureProcessTotals = {
  combined: { cpuPercent: 3, rssBytes: 30, processCount: 2 },
  core: { cpuPercent: 1, rssBytes: 10, processCount: 1 },
  external: { cpuPercent: 2, rssBytes: 20, processCount: 1 },
} as const;
const fixtureProcessDiagnostics = {
  serverPid: 1,
  readAt: fixtureDate,
  totals: fixtureProcessTotals,
  uiCoverage: { status: "notApplicable", message: Option.none<string>() },
  processes: [
    {
      pid: 42,
      ppid: 1,
      pgid: Option.none<number>(),
      status: "Run",
      cpuPercent: 2,
      rssBytes: 20,
      elapsed: "00:00:01",
      command: "codex",
      depth: 0,
      childPids: [],
      ...fixtureProcessAttribution,
    },
  ],
  error: Option.none<{ readonly message: string }>(),
} as const;
const fixtureProcessHistory = {
  readAt: fixtureDate,
  windowMs: 60_000,
  bucketMs: 1_000,
  sampleIntervalMs: 500,
  retainedSampleCount: 2,
  cpuSecondsApprox: { combined: 1.5, core: 1, external: 0.5 },
  uiCoverage: { status: "partial", message: Option.some("UI coverage is partial.") },
  buckets: [
    {
      startedAt: fixtureDate,
      endedAt: fixtureDate,
      cpuPercent: {
        average: { combined: 3, core: 1, external: 2 },
        peak: { combined: 6, core: 2, external: 4 },
      },
      rssBytes: {
        average: { combined: 30, core: 10, external: 20 },
        peak: { combined: 60, core: 20, external: 40 },
      },
      maxProcessCount: { combined: 2, core: 1, external: 1 },
    },
  ],
  processes: [
    {
      pid: 42,
      ppid: 1,
      command: "codex",
      depth: 0,
      ...fixtureProcessAttribution,
      firstSeenAt: fixtureDate,
      lastSeenAt: fixtureDate,
      currentCpuPercent: 2,
      avgCpuPercent: 2,
      maxCpuPercent: 4,
      cpuSecondsApprox: 0.5,
      currentRssBytes: 20,
      maxRssBytes: 40,
      sampleCount: 2,
    },
  ],
  error: Option.none<{
    readonly failureTag:
      | "ProcessDiagnosticsQueryTimeoutError"
      | "ProcessDiagnosticsQueryFailedError"
      | "ProcessDiagnosticsServerProcessSignalError"
      | "ProcessDiagnosticsNotDescendantError"
      | "ProcessDiagnosticsSignalFailedError";
    readonly message: string;
  }>(),
} as const;
const fixtureSignalProcessInput = {
  pid: 42,
  processKey: "42:100",
  signal: "SIGINT",
} as const;
const fixturePairingLink = {
  id: "fixture-pairing-link",
  credential: "23456789ABCD",
  scopes: ["orchestration:read"],
  subject: "fixture",
  createdAt: fixtureDate,
  expiresAt: fixtureDate,
} as const;
const fixtureClientSession = {
  sessionId: "00000000-0000-4000-8000-000000000001",
  subject: "fixture",
  scopes: ["orchestration:read"],
  method: "bearer-access-token",
  client: { deviceType: "desktop" },
  issuedAt: fixtureDate,
  expiresAt: fixtureDate,
  lastConnectedAt: null,
  connected: false,
  current: false,
} as const;
const fixtureActivityActor = {
  _tag: "actor",
  id: "actor:child-1",
  parentActorId: null,
  name: "Explore provider events",
  role: "explorer",
  providerType: "worker",
  status: "running",
  summary: "Reading App Server schemas",
  startedAt: "2026-07-22T12:00:00Z",
  updatedAt: "2026-07-22T12:00:01Z",
  terminalAt: null,
} as const;
const fixtureActivitySnapshot = {
  protocolVersion: 2,
  scopeId: "thread:thread-1",
  scope: { _tag: "thread", threadId: "thread-1" },
  revision: 3,
  provider: "codex",
  providerInstanceId: "codex",
  capabilities: {
    actors: true,
    attributedActivity: true,
    backgroundWork: true,
    historyRecovery: "full",
    terminalObservation: false,
    targetedActorCancellation: true,
  },
  observationState: "live",
  sections: {
    subagents: { state: "live", message: null, retryable: false },
    backgroundTasks: { state: "live", message: null, retryable: false },
  },
  counts: {
    subagents: { active: 1, done: 0 },
    backgroundTasks: { active: 0, done: 0 },
  },
  actors: [fixtureActivityActor],
  workItems: [],
  actorsHasMore: false,
  workItemsHasMore: false,
  control: {
    scopeId: "thread:thread-1",
    revision: 8,
    actors: [
      {
        actorId: "actor:child-1",
        state: "available",
        controlRevision: 3,
        activeDescendantCount: 2,
      },
    ],
    operations: [],
  },
  updatedAt: "2026-07-22T12:00:01Z",
} as const;

const manualStreamSamples = new Map<string, unknown>([
  ["subscribeActivity:0", { kind: "snapshot", snapshot: fixtureActivitySnapshot }],
  [
    "subscribeActivity:1",
    {
      kind: "delta",
      delta: {
        scopeId: fixtureActivitySnapshot.scopeId,
        previousRevision: 3,
        revision: 4,
        changes: [{ kind: "actor-upserted", actor: fixtureActivityActor }],
        updatedAt: "2026-07-22T12:00:02Z",
      },
    },
  ],
  ["subscribeServerConfig:0", { version: 1, type: "snapshot", config: fixtureServerConfig }],
  [
    "subscribeServerConfig:1",
    {
      version: 1,
      type: "keybindingsUpdated",
      payload: { keybindings: [], issues: [] },
    },
  ],
  [
    "subscribeServerConfig:3",
    {
      version: 1,
      type: "settingsUpdated",
      payload: { settings: DEFAULT_SERVER_SETTINGS },
    },
  ],
  [
    "subscribeServerLifecycle:0",
    {
      version: 1,
      sequence: 1,
      type: "welcome",
      payload: {
        environment: fixtureEnvironmentDescriptor,
        cwd: "C:\\fixture",
        projectName: "Fixture",
      },
    },
  ],
  [
    "subscribeServerLifecycle:1",
    {
      version: 1,
      sequence: 2,
      type: "ready",
      payload: {
        at: "2026-01-01T00:00:00.000Z",
        environment: fixtureEnvironmentDescriptor,
      },
    },
  ],
  [
    "subscribeAuthAccess:0",
    {
      version: 1,
      revision: 1,
      type: "snapshot",
      payload: { pairingLinks: [], clientSessions: [] },
    },
  ],
  [
    "subscribeAuthAccess:1",
    {
      version: 1,
      revision: 2,
      type: "pairingLinkUpserted",
      payload: fixturePairingLink,
    },
  ],
  [
    "subscribeAuthAccess:2",
    {
      version: 1,
      revision: 3,
      type: "pairingLinkRemoved",
      payload: { id: fixturePairingLink.id },
    },
  ],
  [
    "subscribeAuthAccess:3",
    {
      version: 1,
      revision: 4,
      type: "clientUpserted",
      payload: fixtureClientSession,
    },
  ],
  [
    "subscribeAuthAccess:4",
    {
      version: 1,
      revision: 5,
      type: "clientRemoved",
      payload: { sessionId: fixtureClientSession.sessionId },
    },
  ],
  [
    "subscribeVcsStatusSummary:2",
    {
      isRepo: true,
      refName: "feature/fixture-summary",
      detachedHead: null,
      hasWorkingTreeChanges: true,
      sourceControlProvider: {
        kind: "github",
        name: "GitHub",
        baseUrl: "https://github.com",
      },
      pr: {
        provider: "github",
        number: 42,
        title: "Fixture summary pull request",
        url: "https://github.com/bibcode/fixture/pull/42",
        baseRefName: "main",
        headRefName: "feature/fixture-summary",
        state: "open",
        updatedAt: Option.none(),
      },
      observedAt: "2026-08-21T12:00:00.000Z",
      stale: false,
    },
  ],
]);

const stringSeed = (value: string): number => {
  let seed = 0x811c9dc5;
  for (const character of value) {
    seed ^= character.codePointAt(0) ?? 0;
    seed = Math.imul(seed, 0x01000193);
  }
  return seed | 0;
};

const encodeSchemaSample = (
  ast: SchemaAST.AST,
  fixtureKey: string,
  manualValue?: unknown,
): unknown => {
  const codec = Schema.make(ast) as Schema.Codec<unknown, unknown, never, never>;
  const encode = compileUnknownEncoder(codec);
  const encodeAndValidate = (value: unknown): unknown => {
    const encoded = encode(value);
    const jsonRoundTrip = JSON.parse(
      JSON.stringify(encoded, (_key, item: unknown) => {
        if (typeof item === "number" && !Number.isFinite(item)) return 0;
        if (typeof item === "string" && /^-?\d{5,}-\d{2}-\d{2}T/.test(item)) {
          return "2026-01-01T00:00:00.000Z";
        }
        return item;
      }),
    ) as unknown;
    return jsonRoundTrip;
  };
  if (manualValue !== undefined) {
    return encodeAndValidate(manualValue);
  }
  const arbitrary = Schema.toArbitrary(codec)(FastCheck);
  let lastError: unknown;
  for (let attempt = 0; attempt < 100; attempt += 1) {
    const [sample] = FastCheck.sample(arbitrary, {
      numRuns: 1,
      seed: stringSeed(`${fixtureKey}:${attempt}`),
    });
    try {
      return encodeAndValidate(sample);
    } catch (error) {
      lastError = error;
    }
  }
  throw new Error(`Could not generate schema fixture ${fixtureKey}.`, { cause: lastError });
};

const jsonParser = RpcSerialization.json.makeUnsafe();
const serializeWireFixture = (message: unknown): unknown => {
  const encoded = jsonParser.encode(message);
  if (typeof encoded !== "string") {
    throw new Error("JSON RPC serializer did not produce a string.");
  }
  return JSON.parse(encoded) as unknown;
};

const stripEffectOptionIds = (value: unknown): unknown =>
  JSON.parse(
    JSON.stringify(value, (key, item: unknown) => {
      if (key === "_id" && item === "Option") return undefined;
      return item;
    }),
  ) as unknown;

const dynamicFixtures = new Map<string, unknown>();
for (const [kind, detail] of [
  ["live", "The Git status stream fell behind. Subscribe again."],
  ["setup", "The Git status subscription fell behind while it was being set up. Subscribe again."],
] as const) {
  dynamicFixtures.set(
    `contract-shapes/subscribeVcsStatus__fell-behind-${kind}-failure.json`,
    serializeWireFixture({
      _tag: "Exit",
      requestId,
      exit: {
        _tag: "Failure",
        cause: [
          {
            _tag: "Fail",
            error: Schema.encodeSync(GitCommandError)(
              new GitCommandError({
                operation: "GitStatusBroadcaster.fellBehind",
                command: "git",
                cwd: "/repo",
                detail,
              }),
            ),
          },
        ],
      },
    } satisfies RpcMessage.ResponseExitEncoded),
  );
}
const fixtureRemoteUpdateSnapshot = {
  serverVersion: "0.6.2",
  latestVersion: "0.6.4",
  state: "downloading",
  error: null,
  support: { installMode: "interactive", reason: "available", installKind: "unknown" },
  downloadPercent: 42,
  targetVersion: "0.6.4",
  installStage: null,
} satisfies typeof RemoteUpdateSnapshot.Type;
dynamicFixtures.set(
  "contract-shapes/updater__status-success.json",
  stripEffectOptionIds(
    serializeWireFixture({
      _tag: "Exit",
      requestId,
      exit: {
        _tag: "Success",
        value: compileUnknownEncoder(RemoteUpdateSnapshot)(fixtureRemoteUpdateSnapshot),
      },
    } satisfies RpcMessage.ResponseExitEncoded),
  ),
);
dynamicFixtures.set(
  "contract-shapes/server__getProcessDiagnostics-success.json",
  stripEffectOptionIds(
    serializeWireFixture({
      _tag: "Exit",
      requestId,
      exit: {
        _tag: "Success",
        value: compileUnknownEncoder(ServerProcessDiagnosticsResult)(fixtureProcessDiagnostics),
      },
    } satisfies RpcMessage.ResponseExitEncoded),
  ),
);
dynamicFixtures.set(
  "contract-shapes/server__getProcessResourceHistory-success.json",
  stripEffectOptionIds(
    serializeWireFixture({
      _tag: "Exit",
      requestId,
      exit: {
        _tag: "Success",
        value: compileUnknownEncoder(ServerProcessResourceHistoryResult)(fixtureProcessHistory),
      },
    } satisfies RpcMessage.ResponseExitEncoded),
  ),
);
dynamicFixtures.set(
  "contract-shapes/terminal__beginInput-request.json",
  serializeWireFixture({
    _tag: "Request",
    id: requestId,
    tag: WS_METHODS.terminalBeginInput,
    payload: compileUnknownEncoder(TerminalBeginInput)({
      threadId: "thread-1",
      terminalId: "terminal-1",
      attachmentSequence: 0,
    }),
    headers: [],
  } satisfies RpcMessage.RequestEncoded),
);
dynamicFixtures.set(
  "contract-shapes/server__signalProcess-request.json",
  serializeWireFixture({
    _tag: "Request",
    id: requestId,
    tag: "server.signalProcess",
    payload: compileUnknownEncoder(ServerSignalProcessInput)(fixtureSignalProcessInput),
    headers: [],
  } satisfies RpcMessage.RequestEncoded),
);
dynamicFixtures.set(
  "contract-shapes/activity__cancelSubtree-request.json",
  serializeWireFixture({
    _tag: "Request",
    id: requestId,
    tag: "activity.cancelSubtree",
    payload: compileUnknownEncoder(ActivityCancelSubtreeInput)({
      scope: { _tag: "thread", threadId: "thread-1" },
      scopeId: "thread:thread-1",
      actorId: "actor:child-1",
      expectedControlRevision: 3,
    }),
    headers: [],
  } satisfies RpcMessage.RequestEncoded),
);
dynamicFixtures.set(
  "contract-shapes/activity__retrySubtreeCancellation-request.json",
  serializeWireFixture({
    _tag: "Request",
    id: requestId,
    tag: "activity.retrySubtreeCancellation",
    payload: compileUnknownEncoder(ActivityRetrySubtreeCancellationInput)({
      scope: { _tag: "thread", threadId: "thread-1" },
      scopeId: "thread:thread-1",
      rootActorId: "actor:child-1",
      expectedOperationRevision: 4,
    }),
    headers: [],
  } satisfies RpcMessage.RequestEncoded),
);
dynamicFixtures.set(
  "contract-shapes/terminal__open-provider-activity-request.json",
  serializeWireFixture({
    _tag: "Request",
    id: requestId,
    tag: "terminal.open",
    payload: compileUnknownEncoder(TerminalOpenInput)({
      threadId: "thread-1",
      terminalId: "term-1",
      cwd: "C:/repo",
      env: {
        COMMAND_ONLY: "command",
        RUNTIME_ONLY: "runtime",
        SHARED: "runtime",
      },
      command: {
        executable: "codex",
        args: ["--model", "gpt-5"],
        env: {
          COMMAND_ONLY: "command",
          SHARED: "command",
        },
        activity: {
          driverKind: "codex",
          providerInstanceId: "codex_personal",
        },
      },
    }),
    headers: [],
  } satisfies RpcMessage.RequestEncoded),
);
dynamicFixtures.set(
  "contract-shapes/terminal__open-without-provider-activity-request.json",
  serializeWireFixture({
    _tag: "Request",
    id: requestId,
    tag: "terminal.open",
    payload: compileUnknownEncoder(TerminalOpenInput)({
      threadId: "thread-1",
      terminalId: "term-2",
      cwd: "C:/repo",
      command: {
        executable: "codex",
        args: [],
      },
    }),
    headers: [],
  } satisfies RpcMessage.RequestEncoded),
);
const queueCommandFields = {
  commandId: "queue-command-1",
  threadId: "thread-1",
  createdAt: "2026-09-22T12:00:00.000Z",
};
const queueCommands = [
  {
    ...queueCommandFields,
    type: "thread.turn.start",
    queued: true,
    message: { messageId: "message-1", role: "user", text: "next task", attachments: [] },
    runtimeMode: "full-access",
    interactionMode: "default",
  },
  { ...queueCommandFields, type: "thread.turn.steer", messageId: "message-1" },
  { ...queueCommandFields, type: "thread.turn.promote", messageId: "message-1" },
  {
    ...queueCommandFields,
    type: "thread.turn-delivery.resolve",
    messageId: "message-1",
    action: "cancel",
  },
];
for (const command of queueCommands) {
  dynamicFixtures.set(
    `contract-shapes/orchestration__${command.type.replaceAll(".", "__")}-request.json`,
    serializeWireFixture({
      _tag: "Request",
      id: requestId,
      tag: ORCHESTRATION_WS_METHODS.dispatchCommand,
      payload: compileUnknownEncoder(ClientOrchestrationCommand)(command),
      headers: [],
    } satisfies RpcMessage.RequestEncoded),
  );
}
const queueEventPayload = {
  threadId: "thread-1",
  messageId: "message-1",
  updatedAt: queueCommandFields.createdAt,
};
const queueEvents = {
  "session-stopped": {
    type: "thread.session-set",
    payload: {
      threadId: "thread-1",
      session: {
        threadId: "thread-1",
        status: "error",
        providerName: "codex",
        providerInstanceId: "codex",
        runtimeMode: "full-access",
        activeTurnId: null,
        lastError:
          "Provider session ended when BiBCode stopped. Review delivery status before continuing.",
        lastErrorClass: "session_stopped",
        updatedAt: queueCommandFields.createdAt,
      },
    },
  },
  "steer-requested": {
    type: "thread.turn-steer-requested",
    payload: {
      threadId: "thread-1",
      messageId: "message-1",
      turnId: "turn-1",
      createdAt: queueCommandFields.createdAt,
    },
  },
  "delivery-held": {
    type: "thread.turn-delivery-updated",
    payload: {
      ...queueEventPayload,
      delivery: { state: "queued", provider: "codex", mode: "start", held: true },
      held: true,
      mode: "start",
    },
  },
  "delivery-steered": {
    type: "thread.turn-delivery-updated",
    payload: {
      ...queueEventPayload,
      delivery: { state: "delivered", provider: "codex", mode: "steer", held: false },
      turnId: "turn-1",
      held: false,
      mode: "steer",
    },
  },
  "delivery-new-conversation": {
    type: "thread.turn-delivery-updated",
    payload: {
      ...queueEventPayload,
      delivery: {
        state: "delivered",
        provider: "claudeAgent",
        mode: "start",
        reason: "startedNewConversation",
      },
      mode: "start",
    },
  },
  "delivery-withdrawn": {
    type: "thread.turn-delivery-updated",
    payload: {
      ...queueEventPayload,
      delivery: { state: "dismissed", provider: "codex" },
      withdrawn: true,
    },
  },
};
for (const [name, event] of Object.entries(queueEvents)) {
  dynamicFixtures.set(
    `contract-shapes/orchestration__${name}-event.json`,
    serializeWireFixture({
      _tag: "Exit",
      requestId,
      exit: {
        _tag: "Success",
        value: [
          compileUnknownEncoder(OrchestrationEvent)({
            sequence: 1,
            eventId: "queue-event-1",
            aggregateKind: "thread",
            aggregateId: "thread-1",
            occurredAt: queueCommandFields.createdAt,
            commandId: queueCommandFields.commandId,
            causationEventId: null,
            correlationId: null,
            metadata: {},
            ...event,
          }),
        ],
      },
    } satisfies RpcMessage.ResponseExitEncoded),
  );
}
const schemaFingerprints: Record<string, string> = {};
const streamShapeFixtures: Array<string> = [];
const typedFailureFixtures: Array<string> = [];
for (const rpc of [...WsRpcGroup.requests.values()].toSorted((left, right) =>
  left._tag.localeCompare(right._tag),
)) {
  const safeMethodName = rpc._tag.replaceAll(".", "__");
  if (StreamSchemaTypeId in (rpc.successSchema as object)) {
    const streamSchema = rpc.successSchema as unknown as {
      readonly success: { readonly ast: SchemaAST.AST };
    };
    for (const [index, member] of schemaMembers(streamSchema.success.ast).entries()) {
      const fixtureKey = `${rpc._tag}:${index}`;
      const relativePath = `stream-shapes/${safeMethodName}-${String(index).padStart(2, "0")}.json`;
      const value = encodeSchemaSample(member, fixtureKey, manualStreamSamples.get(fixtureKey));
      dynamicFixtures.set(
        relativePath,
        serializeWireFixture({
          _tag: "Chunk",
          requestId,
          values: [value],
        } satisfies RpcMessage.ResponseChunkEncoded),
      );
      schemaFingerprints[relativePath] = NodeCrypto.createHash("sha256")
        .update(JSON.stringify(member))
        .digest("hex");
      streamShapeFixtures.push(relativePath);
    }
  }

  for (const [index, member] of schemaMembers(rpc.errorSchema.ast).entries()) {
    if (member._tag === "Never") continue;
    const fixtureKey = `${rpc._tag}:error:${index}`;
    const relativePath = `typed-failures/${safeMethodName}-${String(index).padStart(2, "0")}.json`;
    let error: unknown;
    try {
      error = encodeSchemaSample(member, fixtureKey);
    } catch (cause) {
      if (cause instanceof Error && cause.message.includes("Unsupported AST Never")) {
        continue;
      }
      throw cause;
    }
    dynamicFixtures.set(
      relativePath,
      serializeWireFixture({
        _tag: "Exit",
        requestId,
        exit: {
          _tag: "Failure",
          cause: [{ _tag: "Fail", error }],
        },
      } satisfies RpcMessage.ResponseExitEncoded),
    );
    schemaFingerprints[relativePath] = NodeCrypto.createHash("sha256")
      .update(JSON.stringify(member))
      .digest("hex");
    typedFailureFixtures.push(relativePath);
  }
}

if (methods.length !== 154) {
  throw new Error(`Expected 154 active RPC methods, found ${methods.length}.`);
}
const streamMethodCount = methods.filter(({ mode }) => mode === "stream").length;
if (streamMethodCount !== 23) {
  throw new Error(`Expected 23 streaming RPC methods, found ${streamMethodCount}.`);
}
if (topLevelStreamShapeCount !== 76) {
  throw new Error(
    `Expected 76 top-level streaming item shapes, found ${topLevelStreamShapeCount}.`,
  );
}
if (streamShapeFixtures.length !== topLevelStreamShapeCount) {
  throw new Error(
    `Exported ${streamShapeFixtures.length} stream shape fixtures, expected ${topLevelStreamShapeCount}.`,
  );
}
if (typedFailureFixtures.length !== 329) {
  throw new Error(`Expected 329 typed failure fixtures, found ${typedFailureFixtures.length}.`);
}
if (orchestrationEventShapeCount !== 24) {
  throw new Error(`Expected 24 orchestration event shapes, found ${orchestrationEventShapeCount}.`);
}
const expectedStale = ["projects.add", "projects.list", "projects.remove"];
if (JSON.stringify(staleMethodIdentifiers) !== JSON.stringify(expectedStale)) {
  throw new Error(`Unexpected stale RPC identifiers: ${staleMethodIdentifiers.join(", ")}`);
}

const outputDirectory = NodePath.resolve(import.meta.dirname, "../fixtures/rpc-wire");
await NodeFSP.rm(outputDirectory, { force: true, recursive: true });
await NodeFSP.mkdir(outputDirectory, { recursive: true });
for (const [name, fixture] of Object.entries(fixtures)) {
  const outputPath = NodePath.join(outputDirectory, `${name}.json`);
  await NodeFSP.writeFile(outputPath, `${JSON.stringify(fixture, null, 2)}\n`);
}
for (const [relativePath, fixture] of dynamicFixtures) {
  const outputPath = NodePath.join(outputDirectory, relativePath);
  await NodeFSP.mkdir(NodePath.dirname(outputPath), { recursive: true });
  await NodeFSP.writeFile(outputPath, `${JSON.stringify(fixture, null, 2)}\n`);
}
const manifestPath = NodePath.join(outputDirectory, "manifest.json");
await NodeFSP.writeFile(
  manifestPath,
  `${JSON.stringify(
    {
      protocolVersion: "effect-4.0.0-beta.107",
      methods,
      streamMethodCount,
      expectedTopLevelStreamShapes: topLevelStreamShapeCount,
      expectedOrchestrationEventShapes: orchestrationEventShapeCount,
      streamShapeFixtures,
      typedFailureFixtures,
      schemaFingerprints,
      staleMethodIdentifiers,
      fixtures: [
        ...Object.keys(fixtures).map((name) => `${name}.json`),
        ...dynamicFixtures.keys(),
      ].toSorted(),
    },
    null,
    2,
  )}\n`,
);

// Format through the checkout-local Vite+ launcher: a bare `vp` cannot be
// spawned without a shell on Windows (`vp.CMD`), and a global `vp` would load
// a second runtime.
const repositoryRoot = NodePath.resolve(import.meta.dirname, "../../..");
const formatResult = NodeChildProcess.spawnSync(
  process.execPath,
  [NodePath.join(repositoryRoot, "scripts", "run-local-vp.mjs"), "fmt", "--write", outputDirectory],
  {
    cwd: repositoryRoot,
    stdio: "inherit",
  },
);
if (formatResult.error !== undefined) {
  throw new Error(`Failed to start the RPC fixture formatter: ${formatResult.error.message}`);
}
if (formatResult.status !== 0) {
  throw new Error(`Failed to format RPC fixtures (exit ${formatResult.status ?? "unknown"}).`);
}

process.stdout.write(
  JSON.stringify(
    {
      rpcMethods: methods.length,
      streamMethods: streamMethodCount,
      topLevelStreamShapes: topLevelStreamShapeCount,
      orchestrationEventShapes: orchestrationEventShapeCount,
      streamShapeFixtures: streamShapeFixtures.length,
      typedFailureFixtures: typedFailureFixtures.length,
      contractShapeFixtures: [...dynamicFixtures.keys()].filter((path) =>
        path.startsWith("contract-shapes/"),
      ).length,
      fixtures: Object.keys(fixtures).length + dynamicFixtures.size,
      schemaFingerprints: Object.keys(schemaFingerprints).length,
      staleMethodIdentifiers: staleMethodIdentifiers.length,
    },
    null,
    2,
  ) + "\n",
);
