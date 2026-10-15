import { InlineUploadChatAttachment, UploadChatAttachment } from "./orchestration.ts";
import { assert, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Schema from "effect/Schema";
import { describe, expect } from "vite-plus/test";

import {
  DEFAULT_PROVIDER_INTERACTION_MODE,
  DEFAULT_RUNTIME_MODE,
  ChatAttachment,
  ClientOrchestrationCommand,
  DispatchResult,
  ModelSelection,
  OrchestrationCommand,
  OrchestrationEvent,
  OrchestrationMessage,
  OrchestrationProject,
  OrchestrationProjectShell,
  OrchestrationThreadShell,
  OrchestrationGetFullThreadDiffInput,
  OrchestrationGetTurnDiffInput,
  OrchestrationReplayEventsInput,
  OrchestrationRpcSchemas,
  OrchestrationLatestTurn,
  ProjectCreatedPayload,
  ProjectMetaUpdatedPayload,
  OrchestrationProposedPlan,
  OrchestrationSession,
  ProjectCreateCommand,
  ThreadMetaUpdatedPayload,
  ThreadTurnStartCommand,
  ThreadCreatedPayload,
  ThreadTurnDiff,
  ThreadTurnStartRequestedPayload,
  ThreadTurnDeliveryUpdatedPayload,
  TurnDelivery,
  TurnDeliveryResolutionAction,
} from "./orchestration.ts";
import { ProviderInstanceId } from "./providerInstance.ts";
import { expectDecodeFailure } from "./test/schemaAssertions.ts";

const decodeTurnDiffInput = Schema.decodeUnknownEffect(OrchestrationGetTurnDiffInput);
const decodeFullThreadDiffInput = Schema.decodeUnknownEffect(OrchestrationGetFullThreadDiffInput);
const decodeThreadTurnDiff = Schema.decodeUnknownEffect(ThreadTurnDiff);
const decodeProjectCreateCommand = Schema.decodeUnknownEffect(ProjectCreateCommand);
const decodeProjectCreatedPayload = Schema.decodeUnknownEffect(ProjectCreatedPayload);
const decodeProjectMetaUpdatedPayload = Schema.decodeUnknownEffect(ProjectMetaUpdatedPayload);
const decodeThreadTurnStartCommand = Schema.decodeUnknownEffect(ThreadTurnStartCommand);
const decodeThreadTurnStartRequestedPayload = Schema.decodeUnknownEffect(
  ThreadTurnStartRequestedPayload,
);
const decodeOrchestrationLatestTurn = Schema.decodeUnknownEffect(OrchestrationLatestTurn);
const decodeOrchestrationProposedPlan = Schema.decodeUnknownEffect(OrchestrationProposedPlan);
const decodeOrchestrationSession = Schema.decodeUnknownEffect(OrchestrationSession);
const decodeDispatchResult = Schema.decodeUnknownEffect(DispatchResult);
const decodeOrchestrationProject = Schema.decodeUnknownEffect(OrchestrationProject);
const decodeOrchestrationProjectShell = Schema.decodeUnknownEffect(OrchestrationProjectShell);
const encodeThreadCreatedPayload = Schema.encodeEffect(ThreadCreatedPayload);
const decodeOrchestrationMessageSync = Schema.decodeUnknownSync(OrchestrationMessage);
const decodeOrchestrationCommandSync = Schema.decodeUnknownSync(OrchestrationCommand);
const decodeShell = Schema.decodeUnknownSync(OrchestrationThreadShell);
const decodeTurnDelivery = Schema.decodeUnknownSync(TurnDelivery);
const decodeThreadTurnDeliveryUpdatedPayload = Schema.decodeUnknownSync(
  ThreadTurnDeliveryUpdatedPayload,
);
const encodeTurnDelivery = Schema.encodeSync(TurnDelivery);
const decodeDeliveryResolutionAction = Schema.decodeUnknownSync(TurnDeliveryResolutionAction);
const decodeOrchestrationEventSync = Schema.decodeUnknownSync(OrchestrationEvent);
const encodeOrchestrationEventSync = Schema.encodeSync(OrchestrationEvent);
const decodeClientOrchestrationCommandSync = Schema.decodeUnknownSync(ClientOrchestrationCommand);
const decodeUploadChatAttachment = Schema.decodeUnknownSync(UploadChatAttachment);
const isUploadChatAttachment = Schema.is(UploadChatAttachment);
const isInlineUploadChatAttachment = Schema.is(InlineUploadChatAttachment);
const decodeReplayEventsInput = Schema.decodeUnknownSync(OrchestrationReplayEventsInput);
const decodeReplayEventsResult = Schema.decodeUnknownSync(
  OrchestrationRpcSchemas.replayEvents.output,
);
const decodeLegacyTurnDelivery = Schema.decodeUnknownSync(
  Schema.Struct({
    ...TurnDelivery.fields,
    reason: Schema.optionalKey(
      Schema.Literals(["modelSelectionRefused"]).pipe(
        Schema.catchDecoding(() => Effect.succeedNone),
      ),
    ),
  }),
);

describe("delivery failure reasons", () => {
  const delivery = { state: "failed", provider: "codex" };

  it("preserves a known refusal reason", () => {
    expect(decodeTurnDelivery({ ...delivery, reason: "modelSelectionRefused" })).toEqual({
      ...delivery,
      reason: "modelSelectionRefused",
    });
  });

  it("preserves the delivered notice for a new conversation", () => {
    const delivered = { state: "delivered", provider: "codex", reason: "startedNewConversation" };
    expect(decodeTurnDelivery(delivered)).toEqual(delivered);
  });

  it("lets an older client omit the new reason and keep the delivered message", () => {
    const delivered = { state: "delivered", provider: "codex" };
    expect(decodeLegacyTurnDelivery({ ...delivered, reason: "startedNewConversation" })).toEqual(
      delivered,
    );
  });

  it("decodes old deliveries without a reason", () => {
    expect(decodeTurnDelivery(delivery)).toEqual(delivery);
  });

  it("omits an unknown reason from a newer server", () => {
    const decoded = decodeTurnDelivery({ ...delivery, reason: "futureReason" });
    expect(decoded).toEqual(delivery);
    expect(Object.hasOwn(decoded, "reason")).toBe(false);
  });

  it("keeps the reason in a delivery update payload", () => {
    const payload = decodeThreadTurnDeliveryUpdatedPayload({
      threadId: "thread-1",
      messageId: "message-1",
      delivery: { ...delivery, reason: "modelSelectionRefused" },
      updatedAt: "2026-09-26T00:00:00Z",
    });
    expect(payload.delivery).toEqual({ ...delivery, reason: "modelSelectionRefused" });
  });
});

describe("delivery provider instances", () => {
  it("preserves the routed instance separately from the driver", () => {
    const delivery = {
      state: "failed",
      provider: "codex",
      providerInstanceId: "codex-personal",
    };
    expect(decodeTurnDelivery(delivery)).toEqual(delivery);
  });

  it("decodes older deliveries without an instance", () => {
    const delivery = { state: "pending", provider: "codex" };
    const decoded = decodeTurnDelivery(delivery);
    expect(decoded).toEqual(delivery);
    expect(Object.hasOwn(decoded, "providerInstanceId")).toBe(false);
  });
});

describe("replay event pagination contracts", () => {
  const event = {
    sequence: 1,
    eventId: "replay-event-1",
    type: "thread.message-sent",
    aggregateKind: "thread",
    aggregateId: "thread-1",
    occurredAt: "2026-09-26T00:00:00.000Z",
    commandId: null,
    causationEventId: null,
    correlationId: null,
    metadata: {},
    payload: {
      threadId: "thread-1",
      messageId: "message-1",
      role: "assistant",
      text: "replayed message",
      turnId: null,
      streaming: false,
      createdAt: "2026-09-26T00:00:00.000Z",
      updatedAt: "2026-09-26T00:00:00.000Z",
    },
  };

  it("decodes an unpaged replay input", () => {
    expect(decodeReplayEventsInput({ fromSequenceExclusive: 0 })).toEqual({
      fromSequenceExclusive: 0,
    });
  });

  it("preserves the paged replay opt-in", () => {
    const input = { fromSequenceExclusive: 42, paged: true };
    expect(decodeReplayEventsInput(input)).toEqual(input);
  });

  it("rejects paged false", () => {
    expect(() => decodeReplayEventsInput({ fromSequenceExclusive: 0, paged: false })).toThrow();
  });

  it.each([{}, { paged: true }])("rejects a negative replay cursor %j", (flags) => {
    expect(() => decodeReplayEventsInput({ fromSequenceExclusive: -1, ...flags })).toThrow();
  });

  it("decodes the legacy replay array", () => {
    expect(decodeReplayEventsResult([event])).toEqual([event]);
  });

  it.each([false, true])("decodes a replay page with exhausted %j", (exhausted) => {
    const page = { events: [event], exhausted };
    expect(decodeReplayEventsResult(page)).toEqual(page);
  });

  it("requires the exhausted flag on a replay page", () => {
    expect(() => decodeReplayEventsResult({ events: [event] })).toThrow();
  });
});

describe("message queue contracts", () => {
  const createdAt = "2026-09-22T12:00:00.000Z";
  const commandFields = {
    commandId: "queue-command-1",
    threadId: "thread-1",
    messageId: "message-1",
    createdAt,
  };
  const eventFields = {
    sequence: 1,
    eventId: "queue-event-1",
    aggregateKind: "thread",
    aggregateId: "thread-1",
    occurredAt: createdAt,
    commandId: null,
    causationEventId: null,
    correlationId: null,
    metadata: {},
  };

  it.each([
    { state: "queued", provider: "codex" },
    { state: "pending", provider: "codex", mode: "steer" },
    { state: "queued", provider: "codex", mode: "start", held: true },
    { state: "pending", provider: "codex", held: false },
  ])("round-trips delivery $state with optional mode and hold", (input) => {
    const decoded = decodeTurnDelivery(input);
    expect(decoded).toEqual(input);
    expect(encodeTurnDelivery(decoded)).toEqual(input);
  });

  it.each([{ mode: "queue" }, { mode: null }, { held: "true" }])(
    "rejects invalid delivery fields %j",
    (fields) => {
      expect(() =>
        decodeTurnDelivery({ state: "pending", provider: "codex", ...fields }),
      ).toThrow();
    },
  );

  it("accepts cancel and rejects unknown resolution actions", () => {
    expect(decodeDeliveryResolutionAction("cancel")).toBe("cancel");
    expect(() => decodeDeliveryResolutionAction("withdraw")).toThrow();
  });

  describe.each([
    ["client", decodeClientOrchestrationCommandSync],
    ["server", decodeOrchestrationCommandSync],
  ] as const)("%s commands", (_name, decode) => {
    const start = {
      type: "thread.turn.start",
      commandId: commandFields.commandId,
      threadId: commandFields.threadId,
      message: { messageId: "message-1", role: "user", text: "next task", attachments: [] },
      runtimeMode: "full-access",
      interactionMode: "default",
      createdAt,
    };

    it.each([{}, { queued: true }, { queued: false }])(
      "preserves optional queued flag %j",
      (flags) => {
        expect(decode({ ...start, ...flags })).toEqual({ ...start, ...flags });
      },
    );

    it.each(["true", null, 1])("rejects non-boolean queued flag %j", (queued) => {
      expect(() => decode({ ...start, queued })).toThrow();
    });

    it.each(["thread.turn.steer", "thread.turn.promote"])(
      "decodes %s with a message id",
      (type) => {
        expect(decode({ type, ...commandFields })).toEqual({ type, ...commandFields });
      },
    );

    it.each(["thread.turn.steer", "thread.turn.promote"])(
      "requires a message id for %s",
      (type) => {
        const { messageId: _messageId, ...fields } = commandFields;
        expect(() => decode({ type, ...fields })).toThrow();
      },
    );

    it("decodes cancel resolution", () => {
      const command = { type: "thread.turn-delivery.resolve", ...commandFields, action: "cancel" };
      expect(decode(command)).toEqual(command);
    });
  });

  it("round-trips a steer-requested event tied to the running turn", () => {
    const event = {
      ...eventFields,
      type: "thread.turn-steer-requested",
      payload: { threadId: "thread-1", messageId: "message-1", turnId: "turn-1", createdAt },
    };
    const decoded = decodeOrchestrationEventSync(event);
    expect(decoded).toEqual(event);
    expect(encodeOrchestrationEventSync(decoded)).toEqual(event);
  });

  it.each([
    { withdrawn: true },
    { turnId: "turn-1" },
    { held: true },
    { held: false, mode: "start" },
    { mode: "steer" },
  ])("preserves delivery update fields %j", (fields) => {
    const event = {
      ...eventFields,
      type: "thread.turn-delivery-updated",
      payload: {
        threadId: "thread-1",
        messageId: "message-1",
        delivery: { state: "dismissed", provider: "codex" },
        updatedAt: createdAt,
        ...fields,
      },
    };
    const decoded = decodeOrchestrationEventSync(event);
    expect(decoded).toEqual(event);
    expect(encodeOrchestrationEventSync(decoded)).toEqual(event);
  });
});

const baseShellFixture = {
  id: "thread-1",
  projectId: "project-1",
  title: "Thread",
  modelSelection: { provider: "codex", model: "gpt-5" },
  runtimeMode: "full-access",
  branch: null,
  worktreePath: null,
  latestTurn: null,
  createdAt: "2026-08-01T00:00:00Z",
  updatedAt: "2026-08-01T00:00:01Z",
  session: null,
  latestUserMessageAt: null,
  hasPendingApprovals: false,
  hasPendingUserInput: false,
  hasActionableProposedPlan: false,
} as const;

describe("OrchestrationConversationPreview", () => {
  it("decodes a shell without the field (older server)", () => {
    const shell = decodeShell(baseShellFixture);
    expect(shell.conversationPreview).toBeUndefined();
  });

  it("decodes a populated preview", () => {
    const shell = decodeShell({
      ...baseShellFixture,
      conversationPreview: {
        prompt: "fix the flaky test",
        tool: "Bash: vp test apps/web",
        assistantMessage: "The test is flaky because…",
      },
    });
    expect(shell.conversationPreview).toEqual({
      prompt: "fix the flaky test",
      tool: "Bash: vp test apps/web",
      assistantMessage: "The test is flaky because…",
    });
  });

  it("decodes null members and a null preview", () => {
    expect(
      decodeShell({ ...baseShellFixture, conversationPreview: null }).conversationPreview,
    ).toBeNull();
    expect(
      decodeShell({
        ...baseShellFixture,
        conversationPreview: { prompt: null, tool: null, assistantMessage: null },
      }).conversationPreview,
    ).toEqual({ prompt: null, tool: null, assistantMessage: null });
  });
});

it("decodes message delivery and delivery resolution command", () => {
  const message = decodeOrchestrationMessageSync({
    id: "message-1",
    role: "user",
    text: "ship it",
    turnId: null,
    streaming: false,
    delivery: {
      state: "uncertain",
      provider: "claudeAgent",
      detail: "connection lost after write",
    },
    createdAt: "2026-08-01T00:00:00Z",
    updatedAt: "2026-08-01T00:00:01Z",
  });
  assert.strictEqual(message.delivery?.state, "uncertain");

  const command = decodeOrchestrationCommandSync({
    type: "thread.turn-delivery.resolve",
    commandId: "resolve-1",
    threadId: "thread-1",
    messageId: "message-1",
    action: "retry",
    createdAt: "2026-08-01T00:00:02Z",
  });
  if (command.type !== "thread.turn-delivery.resolve") {
    assert.fail(`Expected delivery resolution command, received ${command.type}.`);
  }
  assert.strictEqual(command.action, "retry");
});

it.effect("decodes an authoritative project identity from project creation", () =>
  Effect.gen(function* () {
    const result = yield* decodeDispatchResult({
      sequence: 2,
      projectId: "existing-project",
    });

    assert.strictEqual(result.projectId, "existing-project");
  }),
);

it.effect("decodes the canonical default thread returned by project creation", () =>
  Effect.gen(function* () {
    const result = yield* decodeDispatchResult({
      sequence: 2,
      projectId: "project-1",
      threadId: "default-thread-1",
    });

    assert.strictEqual(result.threadId, "default-thread-1");
  }),
);

function getOptionValue(
  options: ReadonlyArray<{ id: string; value: unknown }> | undefined,
  id: string,
): unknown {
  return options?.find((option) => option.id === id)?.value;
}
const decodeThreadCreatedPayload = Schema.decodeUnknownEffect(ThreadCreatedPayload);
const decodeOrchestrationCommand = Schema.decodeUnknownEffect(OrchestrationCommand);
const decodeOrchestrationEvent = Schema.decodeUnknownEffect(OrchestrationEvent);
const decodeThreadMetaUpdatedPayload = Schema.decodeUnknownEffect(ThreadMetaUpdatedPayload);
const decodeChatAttachment = Schema.decodeUnknownEffect(ChatAttachment);
const decodeClientOrchestrationCommand = Schema.decodeUnknownEffect(ClientOrchestrationCommand);
const encodeClientOrchestrationCommand = Schema.encodeEffect(ClientOrchestrationCommand);

it.effect("client orchestration schema excludes workspace authority fields", () =>
  Effect.gen(function* () {
    const commands = [
      {
        type: "project.meta.update",
        commandId: "project-meta-1",
        projectId: "project-1",
        worktreeDiscovery: {
          visibility: "shown",
          initialPromptDismissedAt: null,
          baselinePaths: ["/caller/chosen"],
        },
      },
      {
        type: "thread.create",
        commandId: "thread-create-1",
        threadId: "thread-1",
        projectId: "project-1",
        title: "Thread",
        modelSelection: { provider: "codex", model: "gpt-5" },
        runtimeMode: "full-access",
        interactionMode: "default",
        kind: "workspace",
        branch: null,
        worktreePath: "/caller/chosen",
        createdAt: "2026-08-11T00:00:00.000Z",
      },
      {
        type: "thread.meta.update",
        commandId: "thread-meta-1",
        threadId: "thread-1",
        worktreePath: "/caller/retargeted",
      },
      {
        type: "thread.turn.start",
        commandId: "turn-start-1",
        threadId: "thread-1",
        message: {
          messageId: "message-1",
          role: "user",
          text: "hello",
          attachments: [],
        },
        runtimeMode: "full-access",
        interactionMode: "default",
        bootstrap: {
          createThread: {
            projectId: "project-1",
            title: "Thread",
            modelSelection: { provider: "codex", model: "gpt-5" },
            runtimeMode: "full-access",
            interactionMode: "default",
            branch: null,
            worktreePath: "/caller/chosen",
            createdAt: "2026-08-11T00:00:00.000Z",
          },
          prepareWorktree: {
            projectCwd: "/caller/chosen",
            baseBranch: "main",
          },
        },
        createdAt: "2026-08-11T00:00:00.000Z",
      },
    ] as const;

    const decoded = yield* Effect.all(
      commands.map((command) => decodeClientOrchestrationCommand(command)),
    );
    const projectMeta = decoded[0]!;
    const threadCreate = decoded[1]!;
    const threadMeta = decoded[2]!;
    const turnStart = decoded[3]!;

    assert.isFalse("worktreeDiscovery" in projectMeta);
    assert.isFalse("kind" in threadCreate);
    assert.isFalse("worktreePath" in threadCreate);
    assert.isFalse("worktreePath" in threadMeta);
    if (turnStart.type !== "thread.turn.start" || turnStart.bootstrap === undefined) {
      assert.fail("Expected a bootstrap turn-start command.");
    }
    assert.isFalse("worktreePath" in turnStart.bootstrap.createThread!);
    assert.isFalse("projectCwd" in turnStart.bootstrap.prepareWorktree!);
  }),
);

it.effect("decodes image and file chat attachments", () =>
  Effect.gen(function* () {
    const image = {
      type: "image" as const,
      id: "image-1",
      name: "pixel.png",
      mimeType: "image/png",
      sizeBytes: 12,
    };
    const file = {
      type: "file" as const,
      id: "notes-1",
      name: "notes.txt",
      mimeType: "text/plain",
      sizeBytes: 12,
    };

    assert.deepStrictEqual(yield* decodeChatAttachment(image), image);
    assert.deepStrictEqual(yield* decodeChatAttachment(file), file);
  }),
);

it.effect("round-trips uploaded attachment ids", () =>
  Effect.gen(function* () {
    const command = {
      type: "thread.turn.start",
      commandId: "cmd-upload-file",
      threadId: "thread-upload-file",
      message: {
        messageId: "msg-upload-file",
        role: "user",
        text: "file",
        attachments: [
          {
            type: "image" as const,
            id: "image-upload-1",
            name: "pixel.png",
            mimeType: "image/png",
            sizeBytes: 5,
            dataUrl: "data:image/png;base64,aGVsbG8=",
          },
          {
            type: "file" as const,
            id: "notes-upload-1",
            name: "notes.txt",
            mimeType: "text/plain",
            sizeBytes: 5,
            dataUrl: "data:text/plain;base64,aGVsbG8=",
          },
        ],
      },
      runtimeMode: "full-access",
      interactionMode: "default",
      createdAt: "2026-01-01T00:00:00.000Z",
    } as const;
    const parsed = yield* decodeClientOrchestrationCommand(command);

    if (parsed.type !== "thread.turn.start") {
      throw new Error("Expected a thread.turn.start command.");
    }
    assert.deepStrictEqual(parsed.message.attachments, command.message.attachments);
    assert.deepStrictEqual(yield* encodeClientOrchestrationCommand(parsed), command);
  }),
);

it.effect("rejects ambiguous, invalid, oversized, and excessive attachments", () =>
  Effect.gen(function* () {
    const image = {
      type: "image",
      id: "image-1",
      name: "pixel.png",
      mimeType: "image/png",
      sizeBytes: 12,
    };
    const file = {
      type: "file",
      id: "notes-1",
      name: "notes.txt",
      mimeType: "text/plain",
      sizeBytes: 12,
    };
    const invalidInputs = [
      { ...file, mimeType: "image/png" },
      { ...image, mimeType: "text/plain" },
      { ...file, sizeBytes: 10 * 1024 * 1024 + 1 },
    ];

    for (const input of invalidInputs) {
      assert.strictEqual((yield* Effect.exit(decodeChatAttachment(input)))._tag, "Failure");
    }
    for (const attachment of [
      {
        type: "file",
        id: "bad id",
        name: "notes.txt",
        mimeType: "text/plain",
        sizeBytes: 1,
        dataUrl: "data:text/plain;base64,eA==",
      },
      {
        type: "image",
        name: "pixel.png",
        mimeType: "image/png",
        sizeBytes: 1,
        dataUrl: "data:image/png;base64,eA==",
      },
    ]) {
      assert.strictEqual(
        (yield* Effect.exit(
          decodeClientOrchestrationCommand({
            type: "thread.turn.start",
            commandId: "cmd-invalid-upload-id",
            threadId: "thread-invalid-upload-id",
            message: {
              messageId: "msg-invalid-upload-id",
              role: "user",
              text: "invalid",
              attachments: [attachment],
            },
            runtimeMode: "full-access",
            interactionMode: "default",
            createdAt: "2026-01-01T00:00:00.000Z",
          }),
        ))._tag,
        "Failure",
      );
    }
    assert.strictEqual(
      (yield* Effect.exit(
        decodeThreadTurnStartCommand({
          type: "thread.turn.start",
          commandId: "cmd-attachments",
          threadId: "thread-attachments",
          message: {
            messageId: "msg-attachments",
            role: "user",
            text: "attachments",
            attachments: Array.from({ length: 9 }, (_, index) => ({
              ...file,
              id: `notes-${index}`,
            })),
          },
          runtimeMode: "full-access",
          interactionMode: "default",
          createdAt: "2026-01-01T00:00:00.000Z",
        }),
      ))._tag,
      "Failure",
    );
  }),
);

it.effect("parses turn diff input when fromTurnCount <= toTurnCount", () =>
  Effect.gen(function* () {
    const parsed = yield* decodeTurnDiffInput({
      threadId: "thread-1",
      fromTurnCount: 1,
      toTurnCount: 2,
    });
    assert.strictEqual(parsed.fromTurnCount, 1);
    assert.strictEqual(parsed.toTurnCount, 2);
  }),
);

it.effect("parses turn diff input with whitespace ignoring enabled", () =>
  Effect.gen(function* () {
    const parsed = yield* decodeTurnDiffInput({
      threadId: "thread-1",
      fromTurnCount: 1,
      toTurnCount: 2,
      ignoreWhitespace: true,
    });
    assert.strictEqual(parsed.ignoreWhitespace, true);
  }),
);

it.effect("parses full thread diff input with whitespace ignoring enabled", () =>
  Effect.gen(function* () {
    const parsed = yield* decodeFullThreadDiffInput({
      threadId: "thread-1",
      toTurnCount: 2,
      ignoreWhitespace: true,
    });
    assert.strictEqual(parsed.ignoreWhitespace, true);
  }),
);

it.effect("rejects turn diff input when fromTurnCount > toTurnCount", () =>
  Effect.gen(function* () {
    const result = yield* Effect.exit(
      decodeTurnDiffInput({
        threadId: "thread-1",
        fromTurnCount: 3,
        toTurnCount: 2,
      }),
    );
    assert.strictEqual(result._tag, "Failure");
  }),
);

it.effect("rejects thread turn diff when fromTurnCount > toTurnCount", () =>
  Effect.gen(function* () {
    const result = yield* Effect.exit(
      decodeThreadTurnDiff({
        threadId: "thread-1",
        fromTurnCount: 3,
        toTurnCount: 2,
        diff: "patch",
      }),
    );
    assert.strictEqual(result._tag, "Failure");
  }),
);

it.effect("trims branded ids and command string fields at decode boundaries", () =>
  Effect.gen(function* () {
    const parsed = yield* decodeProjectCreateCommand({
      type: "project.create",
      commandId: " cmd-1 ",
      projectId: " project-1 ",
      title: " Project Title ",
      workspaceRoot: " /tmp/workspace ",
      defaultModelSelection: {
        provider: "codex",
        model: " gpt-5.2 ",
      },
      createdAt: "2026-01-01T00:00:00.000Z",
    });
    assert.strictEqual(parsed.commandId, "cmd-1");
    assert.strictEqual(parsed.projectId, "project-1");
    assert.strictEqual(parsed.title, "Project Title");
    assert.strictEqual(parsed.workspaceRoot, "/tmp/workspace");
    assert.strictEqual(parsed.createWorkspaceRootIfMissing, undefined);
    assert.strictEqual(parsed.initializeGit, undefined);
    assert.deepStrictEqual(parsed.defaultModelSelection, {
      instanceId: ProviderInstanceId.make("codex"),
      model: "gpt-5.2",
    });
  }),
);

it.effect("decodes project.create with createWorkspaceRootIfMissing enabled", () =>
  Effect.gen(function* () {
    const parsed = yield* decodeProjectCreateCommand({
      type: "project.create",
      commandId: "cmd-1",
      projectId: "project-1",
      title: "Project Title",
      workspaceRoot: "/tmp/workspace",
      createWorkspaceRootIfMissing: true,
      createdAt: "2026-01-01T00:00:00.000Z",
    });

    assert.strictEqual(parsed.createWorkspaceRootIfMissing, true);
  }),
);

it.effect("decodes project.create with Git initialization enabled", () =>
  Effect.gen(function* () {
    const parsed = yield* decodeProjectCreateCommand({
      type: "project.create",
      commandId: "cmd-1",
      projectId: "project-1",
      title: "Project Title",
      workspaceRoot: "/tmp/workspace",
      createWorkspaceRootIfMissing: true,
      initializeGit: true,
      createdAt: "2026-01-01T00:00:00.000Z",
    });

    assert.strictEqual(parsed.initializeGit, true);
  }),
);

it.effect("decodes historical project.created payloads with a default provider", () =>
  Effect.gen(function* () {
    const parsed = yield* decodeProjectCreatedPayload({
      projectId: "project-1",
      title: "Project Title",
      workspaceRoot: "/tmp/workspace",
      defaultModelSelection: {
        provider: "codex",
        model: "gpt-5.4",
      },
      scripts: [],
      createdAt: "2026-01-01T00:00:00.000Z",
      updatedAt: "2026-01-01T00:00:00.000Z",
    });
    assert.strictEqual(parsed.defaultModelSelection?.instanceId, "codex");
  }),
);

it.effect("defaults missing worktree discovery policy for legacy project representations", () =>
  Effect.gen(function* () {
    const legacyProject = {
      id: "project-1",
      title: "Project Title",
      workspaceRoot: "/tmp/workspace",
      defaultModelSelection: null,
      scripts: [],
      createdAt: "2026-01-01T00:00:00.000Z",
      updatedAt: "2026-01-01T00:00:00.000Z",
      deletedAt: null,
    };
    const legacyCreated = {
      projectId: "project-1",
      title: "Project Title",
      workspaceRoot: "/tmp/workspace",
      defaultModelSelection: null,
      scripts: [],
      createdAt: "2026-01-01T00:00:00.000Z",
      updatedAt: "2026-01-01T00:00:00.000Z",
    };
    const legacyMetaUpdated = {
      projectId: "project-1",
      updatedAt: "2026-01-01T00:00:00.000Z",
    };

    const [decodedProject, decodedProjectShell, decodedCreated, metaUpdated] = yield* Effect.all([
      decodeOrchestrationProject(legacyProject),
      decodeOrchestrationProjectShell(legacyProject),
      decodeProjectCreatedPayload(legacyCreated),
      decodeProjectMetaUpdatedPayload(legacyMetaUpdated),
    ]);
    for (const project of [decodedProject, decodedProjectShell, decodedCreated]) {
      assert.deepStrictEqual(project.worktreeDiscovery, {
        visibility: "hidden",
        initialPromptDismissedAt: null,
        baselinePaths: [],
      });
    }
    assert.strictEqual(Object.hasOwn(metaUpdated, "worktreeDiscovery"), false);
  }),
);

it.effect("decodes project.meta-updated payloads with explicit default provider", () =>
  Effect.gen(function* () {
    const parsed = yield* decodeProjectMetaUpdatedPayload({
      projectId: "project-1",
      defaultModelSelection: {
        provider: "claudeAgent",
        model: "claude-opus-4-6",
      },
      updatedAt: "2026-01-01T00:00:00.000Z",
    });
    assert.strictEqual(parsed.defaultModelSelection?.instanceId, "claudeAgent");
  }),
);

it.effect("rejects command fields that become empty after trim", () =>
  Effect.gen(function* () {
    const result = yield* Effect.exit(
      decodeProjectCreateCommand({
        type: "project.create",
        commandId: "cmd-1",
        projectId: "project-1",
        title: "  ",
        workspaceRoot: "/tmp/workspace",
        createdAt: "2026-01-01T00:00:00.000Z",
      }),
    );
    assert.strictEqual(result._tag, "Failure");
  }),
);

it.effect("decodes thread.turn.start defaults for provider and runtime mode", () =>
  Effect.gen(function* () {
    const parsed = yield* decodeThreadTurnStartCommand({
      type: "thread.turn.start",
      commandId: "cmd-turn-1",
      threadId: "thread-1",
      message: {
        messageId: "msg-1",
        role: "user",
        text: "hello",
        attachments: [],
      },
      createdAt: "2026-01-01T00:00:00.000Z",
    });
    assert.strictEqual(parsed.modelSelection, undefined);
    assert.strictEqual(parsed.runtimeMode, DEFAULT_RUNTIME_MODE);
    assert.strictEqual(parsed.interactionMode, DEFAULT_PROVIDER_INTERACTION_MODE);
  }),
);

it.effect("preserves explicit provider and runtime mode in thread.turn.start", () =>
  Effect.gen(function* () {
    const parsed = yield* decodeThreadTurnStartCommand({
      type: "thread.turn.start",
      commandId: "cmd-turn-2",
      threadId: "thread-1",
      message: {
        messageId: "msg-2",
        role: "user",
        text: "hello",
        attachments: [],
      },
      modelSelection: {
        provider: "codex",
        model: "gpt-5.4",
      },
      runtimeMode: "full-access",
      createdAt: "2026-01-01T00:00:00.000Z",
    });
    assert.strictEqual(parsed.modelSelection?.instanceId, "codex");
    assert.strictEqual(parsed.runtimeMode, "full-access");
    assert.strictEqual(parsed.interactionMode, DEFAULT_PROVIDER_INTERACTION_MODE);
  }),
);

it.effect("accepts bootstrap metadata in thread.turn.start", () =>
  Effect.gen(function* () {
    const parsed = yield* decodeThreadTurnStartCommand({
      type: "thread.turn.start",
      commandId: "cmd-turn-bootstrap",
      threadId: "thread-1",
      message: {
        messageId: "msg-bootstrap",
        role: "user",
        text: "hello",
        attachments: [],
      },
      bootstrap: {
        createThread: {
          projectId: "project-1",
          title: "Bootstrap thread",
          modelSelection: {
            provider: "codex",
            model: "gpt-5.4",
          },
          runtimeMode: "full-access",
          interactionMode: "default",
          branch: null,
          worktreePath: null,
          createdAt: "2026-01-01T00:00:00.000Z",
        },
        prepareWorktree: {
          projectCwd: "/tmp/workspace",
          baseBranch: "main",
          branch: "bibcode/example",
          startFromOrigin: true,
        },
        runSetupScript: true,
      },
      createdAt: "2026-01-01T00:00:00.000Z",
    });
    assert.strictEqual(parsed.bootstrap?.createThread?.projectId, "project-1");
    assert.strictEqual(parsed.bootstrap?.prepareWorktree?.baseBranch, "main");
    assert.strictEqual(parsed.bootstrap?.prepareWorktree?.startFromOrigin, true);
    assert.strictEqual(parsed.bootstrap?.runSetupScript, true);
  }),
);

it.effect("decodes thread.created runtime mode for historical events", () =>
  Effect.gen(function* () {
    const parsed = yield* decodeThreadCreatedPayload({
      threadId: "thread-1",
      projectId: "project-1",
      title: "Thread title",
      modelSelection: {
        provider: "codex",
        model: "gpt-5.4",
      },
      interactionMode: "default",
      branch: null,
      worktreePath: null,
      createdAt: "2026-01-01T00:00:00.000Z",
      updatedAt: "2026-01-01T00:00:00.000Z",
    });

    assert.strictEqual(parsed.runtimeMode, DEFAULT_RUNTIME_MODE);
    assert.strictEqual(parsed.modelSelection.instanceId, "codex");
  }),
);

it.effect("decodes thread.meta-updated payloads with explicit provider", () =>
  Effect.gen(function* () {
    const parsed = yield* decodeThreadMetaUpdatedPayload({
      threadId: "thread-1",
      modelSelection: {
        provider: "claudeAgent",
        model: "claude-opus-4-6",
      },
      updatedAt: "2026-01-01T00:00:00.000Z",
    });
    assert.strictEqual(parsed.modelSelection?.instanceId, "claudeAgent");
  }),
);

it.effect("decodes thread archive and unarchive commands", () =>
  Effect.gen(function* () {
    const archive = yield* decodeOrchestrationCommand({
      type: "thread.archive",
      commandId: "cmd-archive-1",
      threadId: "thread-1",
    });
    const unarchive = yield* decodeOrchestrationCommand({
      type: "thread.unarchive",
      commandId: "cmd-unarchive-1",
      threadId: "thread-1",
    });

    assert.strictEqual(archive.type, "thread.archive");
    assert.strictEqual(unarchive.type, "thread.unarchive");
  }),
);

it.effect("decodes thread archived and unarchived events", () =>
  Effect.gen(function* () {
    const archived = yield* decodeOrchestrationEvent({
      sequence: 1,
      eventId: "event-archive-1",
      aggregateKind: "thread",
      aggregateId: "thread-1",
      type: "thread.archived",
      occurredAt: "2026-01-01T00:00:00.000Z",
      commandId: "cmd-archive-1",
      causationEventId: null,
      correlationId: "cmd-archive-1",
      metadata: {},
      payload: {
        threadId: "thread-1",
        archivedAt: "2026-01-01T00:00:00.000Z",
        updatedAt: "2026-01-01T00:00:00.000Z",
      },
    });
    const unarchived = yield* decodeOrchestrationEvent({
      sequence: 2,
      eventId: "event-unarchive-1",
      aggregateKind: "thread",
      aggregateId: "thread-1",
      type: "thread.unarchived",
      occurredAt: "2026-01-02T00:00:00.000Z",
      commandId: "cmd-unarchive-1",
      causationEventId: null,
      correlationId: "cmd-unarchive-1",
      metadata: {},
      payload: {
        threadId: "thread-1",
        updatedAt: "2026-01-02T00:00:00.000Z",
      },
    });

    if (archived.type !== "thread.archived") {
      assert.fail(`Expected thread.archived event, received ${archived.type}.`);
    }
    assert.strictEqual(archived.payload.archivedAt, "2026-01-01T00:00:00.000Z");
    assert.strictEqual(unarchived.type, "thread.unarchived");
  }),
);

it.effect("accepts provider-scoped model options in thread.turn.start", () =>
  Effect.gen(function* () {
    const parsed = yield* decodeThreadTurnStartCommand({
      type: "thread.turn.start",
      commandId: "cmd-turn-options",
      threadId: "thread-1",
      message: {
        messageId: "msg-options",
        role: "user",
        text: "hello",
        attachments: [],
      },
      modelSelection: {
        provider: "codex",
        model: "gpt-5.3-codex",
        options: [
          { id: "reasoningEffort", value: "high" },
          { id: "fastMode", value: true },
        ],
      },
      createdAt: "2026-01-01T00:00:00.000Z",
    });
    assert.strictEqual(parsed.modelSelection?.instanceId, "codex");
    assert.strictEqual(getOptionValue(parsed.modelSelection?.options, "reasoningEffort"), "high");
    assert.strictEqual(getOptionValue(parsed.modelSelection?.options, "fastMode"), true);
  }),
);

it.effect("normalizes legacy object-shaped modelSelection.options on decode", () =>
  Effect.gen(function* () {
    const parsed = yield* decodeThreadCreatedPayload({
      threadId: "thread-1",
      projectId: "project-1",
      title: "Legacy options thread",
      modelSelection: {
        provider: "claudeAgent",
        model: "claude-opus-4-6",
        options: {
          effort: "max",
          fastMode: true,
          // Falsy/garbage entries are dropped, matching migration 026.
          emptyStr: "   ",
          nullish: null,
          nested: { foo: 1 },
        },
      },
      branch: null,
      worktreePath: null,
      createdAt: "2026-01-01T00:00:00.000Z",
      updatedAt: "2026-01-01T00:00:00.000Z",
    });

    assert.strictEqual(parsed.modelSelection.instanceId, ProviderInstanceId.make("claudeAgent"));
    assert.deepStrictEqual(parsed.modelSelection.options, [
      { id: "effort", value: "max" },
      { id: "fastMode", value: true },
    ]);
  }),
);

it.effect("normalizes legacy object-shaped defaultModelSelection.options on decode", () =>
  Effect.gen(function* () {
    const parsed = yield* decodeProjectCreatedPayload({
      projectId: "project-1",
      title: "Legacy default project",
      workspaceRoot: "/tmp/legacy",
      defaultModelSelection: {
        provider: "codex",
        model: "gpt-5.4",
        options: { reasoningEffort: "low" },
      },
      scripts: [],
      createdAt: "2026-01-01T00:00:00.000Z",
      updatedAt: "2026-01-01T00:00:00.000Z",
    });

    assert.deepStrictEqual(parsed.defaultModelSelection?.options, [
      { id: "reasoningEffort", value: "low" },
    ]);
  }),
);

it.effect(
  "normalizes legacy object-shaped options on decode and re-encodes as canonical array",
  () =>
    Effect.gen(function* () {
      const decoded = yield* decodeThreadCreatedPayload({
        threadId: "thread-1",
        projectId: "project-1",
        title: "Round trip thread",
        modelSelection: {
          provider: "codex",
          model: "gpt-5.4",
          options: { fastMode: true },
        },
        branch: null,
        worktreePath: null,
        createdAt: "2026-01-01T00:00:00.000Z",
        updatedAt: "2026-01-01T00:00:00.000Z",
      });

      const encoded = yield* encodeThreadCreatedPayload(decoded);
      assert.deepStrictEqual(encoded.modelSelection.options, [{ id: "fastMode", value: true }]);
    }),
);

it.effect("accepts a title seed in thread.turn.start", () =>
  Effect.gen(function* () {
    const parsed = yield* decodeThreadTurnStartCommand({
      type: "thread.turn.start",
      commandId: "cmd-turn-title-seed",
      threadId: "thread-1",
      message: {
        messageId: "msg-title-seed",
        role: "user",
        text: "hello",
        attachments: [],
      },
      titleSeed: "Investigate reconnect failures",
      createdAt: "2026-01-01T00:00:00.000Z",
    });
    assert.strictEqual(parsed.titleSeed, "Investigate reconnect failures");
  }),
);

it.effect("accepts a source proposed plan reference in thread.turn.start", () =>
  Effect.gen(function* () {
    const parsed = yield* decodeThreadTurnStartCommand({
      type: "thread.turn.start",
      commandId: "cmd-turn-source-plan",
      threadId: "thread-2",
      message: {
        messageId: "msg-source-plan",
        role: "user",
        text: "implement this",
        attachments: [],
      },
      sourceProposedPlan: {
        threadId: "thread-1",
        planId: "plan-1",
      },
      createdAt: "2026-01-01T00:00:00.000Z",
    });
    assert.deepStrictEqual(parsed.sourceProposedPlan, {
      threadId: "thread-1",
      planId: "plan-1",
    });
  }),
);

it.effect(
  "decodes thread.turn-start-requested defaults for provider, runtime mode, and interaction mode",
  () =>
    Effect.gen(function* () {
      const parsed = yield* decodeThreadTurnStartRequestedPayload({
        threadId: "thread-1",
        messageId: "msg-1",
        createdAt: "2026-01-01T00:00:00.000Z",
      });
      assert.strictEqual(parsed.modelSelection, undefined);
      assert.strictEqual(parsed.runtimeMode, DEFAULT_RUNTIME_MODE);
      assert.strictEqual(parsed.interactionMode, DEFAULT_PROVIDER_INTERACTION_MODE);
      assert.strictEqual(parsed.sourceProposedPlan, undefined);
    }),
);

it.effect("decodes thread.turn-start-requested source proposed plan metadata when present", () =>
  Effect.gen(function* () {
    const parsed = yield* decodeThreadTurnStartRequestedPayload({
      threadId: "thread-2",
      messageId: "msg-2",
      sourceProposedPlan: {
        threadId: "thread-1",
        planId: "plan-1",
      },
      createdAt: "2026-01-01T00:00:00.000Z",
    });
    assert.deepStrictEqual(parsed.sourceProposedPlan, {
      threadId: "thread-1",
      planId: "plan-1",
    });
  }),
);

it.effect("decodes thread.turn-start-requested title seed when present", () =>
  Effect.gen(function* () {
    const parsed = yield* decodeThreadTurnStartRequestedPayload({
      threadId: "thread-2",
      messageId: "msg-2",
      titleSeed: "Investigate reconnect failures",
      createdAt: "2026-01-01T00:00:00.000Z",
    });
    assert.strictEqual(parsed.titleSeed, "Investigate reconnect failures");
  }),
);

it.effect("decodes latest turn source proposed plan metadata when present", () =>
  Effect.gen(function* () {
    const parsed = yield* decodeOrchestrationLatestTurn({
      turnId: "turn-2",
      state: "running",
      requestedAt: "2026-01-01T00:00:00.000Z",
      startedAt: "2026-01-01T00:00:01.000Z",
      completedAt: null,
      assistantMessageId: null,
      sourceProposedPlan: {
        threadId: "thread-1",
        planId: "plan-1",
      },
    });
    assert.deepStrictEqual(parsed.sourceProposedPlan, {
      threadId: "thread-1",
      planId: "plan-1",
    });
  }),
);

it.effect("decodes orchestration session runtime mode defaults", () =>
  Effect.gen(function* () {
    const parsed = yield* decodeOrchestrationSession({
      threadId: "thread-1",
      status: "idle",
      providerName: null,
      providerSessionId: null,
      providerThreadId: null,
      activeTurnId: null,
      lastError: null,
      updatedAt: "2026-01-01T00:00:00.000Z",
    });
    assert.strictEqual(parsed.runtimeMode, DEFAULT_RUNTIME_MODE);
  }),
);

it.effect("preserves intentional session stops in persisted snapshots", () =>
  Effect.gen(function* () {
    const parsed = yield* decodeOrchestrationSession({
      threadId: "thread-1",
      status: "error",
      providerName: "codex",
      activeTurnId: null,
      lastError: "Provider session ended when BiBCode stopped.",
      lastErrorClass: "session_stopped",
      updatedAt: "2026-01-01T00:00:00.000Z",
    });
    assert.strictEqual(parsed.lastErrorClass, "session_stopped");
  }),
);

it.effect("normalizes a newer persisted session error class", () =>
  Effect.gen(function* () {
    const parsed = yield* decodeOrchestrationSession({
      threadId: "thread-1",
      status: "error",
      providerName: "opencode",
      activeTurnId: null,
      lastError: "The provider is throttling requests.",
      lastErrorClass: "rate_limit_error",
      updatedAt: "2026-01-01T00:00:00.000Z",
    });
    assert.strictEqual(parsed.lastErrorClass, "unknown");
  }),
);

it.effect("defaults proposed plan implementation metadata for historical rows", () =>
  Effect.gen(function* () {
    const parsed = yield* decodeOrchestrationProposedPlan({
      id: "plan-1",
      turnId: "turn-1",
      planMarkdown: "# Plan",
      createdAt: "2026-01-01T00:00:00.000Z",
      updatedAt: "2026-01-01T00:00:00.000Z",
    });
    assert.strictEqual(parsed.implementedAt, null);
    assert.strictEqual(parsed.implementationThreadId, null);
  }),
);

it.effect("preserves proposed plan implementation metadata when present", () =>
  Effect.gen(function* () {
    const parsed = yield* decodeOrchestrationProposedPlan({
      id: "plan-2",
      turnId: "turn-2",
      planMarkdown: "# Plan",
      implementedAt: "2026-01-02T00:00:00.000Z",
      implementationThreadId: "thread-2",
      createdAt: "2026-01-01T00:00:00.000Z",
      updatedAt: "2026-01-02T00:00:00.000Z",
    });
    assert.strictEqual(parsed.implementedAt, "2026-01-02T00:00:00.000Z");
    assert.strictEqual(parsed.implementationThreadId, "thread-2");
  }),
);

// ── ModelSelection: instance-keyed wire shape + legacy decoder ────────
//
// `ModelSelection` is routing-keyed on `instanceId` — never a driver kind.
// Persisted and in-flight payloads from pre-instance builds carry a
// `provider` field whose value was a driver kind; those payloads are migrated
// at the wire boundary by
// promoting `provider` to the default instance id for that driver
// (built-in drivers use the driver kind slug as their default instance id, so
// the migration is a 1:1 rename).
//
// These tests pin the rollback/fork tolerance invariant: legacy payloads
// decode cleanly for fork-provided drivers, and the decoded form uses
// `instanceId` uniformly regardless of origin.

const decodeModelSelection = Schema.decodeUnknownEffect(ModelSelection);
const encodeModelSelection = Schema.encodeUnknownEffect(ModelSelection);

it.effect("ModelSelection migrates legacy `provider` field to `instanceId`", () =>
  Effect.gen(function* () {
    const parsed = yield* decodeModelSelection({
      provider: "codex",
      model: "gpt-5-codex",
      options: [{ id: "reasoningEffort", value: "high" }],
    });
    assert.strictEqual(parsed.instanceId, ProviderInstanceId.make("codex"));
    assert.strictEqual(parsed.model, "gpt-5-codex");
    assert.deepStrictEqual(parsed.options, [{ id: "reasoningEffort", value: "high" }]);
  }),
);

it.effect("ModelSelection accepts an explicit instanceId routing key", () =>
  Effect.gen(function* () {
    const parsed = yield* decodeModelSelection({
      instanceId: "codex_personal",
      model: "gpt-5-codex",
    });
    assert.strictEqual(parsed.instanceId, ProviderInstanceId.make("codex_personal"));
  }),
);

it.effect("ModelSelection prefers explicit instanceId over legacy provider", () =>
  Effect.gen(function* () {
    const parsed = yield* decodeModelSelection({
      provider: "codex",
      instanceId: "codex_personal",
      model: "gpt-5-codex",
    });
    assert.strictEqual(parsed.instanceId, ProviderInstanceId.make("codex_personal"));
  }),
);

it.effect(
  "ModelSelection decodes unknown driver kinds via legacy provider (rollback / fork invariant)",
  () =>
    Effect.gen(function* () {
      const parsed = yield* decodeModelSelection({
        provider: "ollama",
        model: "llama3:70b",
        options: [{ id: "temperature", value: "0.4" }],
      });
      assert.strictEqual(parsed.instanceId, ProviderInstanceId.make("ollama"));
      assert.strictEqual(parsed.model, "llama3:70b");
    }),
);

it.effect("ModelSelection encodes to the canonical instanceId wire form", () =>
  Effect.gen(function* () {
    const decoded = yield* decodeModelSelection({
      provider: "ollama",
      model: "llama3:70b",
      options: [{ id: "temperature", value: "0.4" }],
    });
    const encoded = yield* encodeModelSelection(decoded);
    assert.deepStrictEqual(encoded, {
      instanceId: "ollama",
      model: "llama3:70b",
      options: [{ id: "temperature", value: "0.4" }],
    });
  }),
);

it.effect("ModelSelection rejects malformed instance ids", () =>
  Effect.gen(function* () {
    const result = yield* Effect.exit(
      decodeModelSelection({
        instanceId: "1invalid", // must start with a letter
        model: "x",
      }),
    );
    assert.strictEqual(result._tag, "Failure");
  }),
);

it("ModelSelection rejects a non-string legacy provider at the routing-key path", () => {
  expectDecodeFailure(
    ModelSelection,
    { provider: 42, model: "gpt-5.4" },
    { rootTag: "Composite", paths: [["instanceId"]] },
  );
});

it("decodes staged image and file inputs and refuses two byte sources", () => {
  for (const [type, mimeType] of [
    ["image", "image/png"],
    ["file", "text/plain"],
  ]) {
    const staged = { type, id: "a-1", name: "a", mimeType, sizeBytes: 3, uploadId: "stage-1" };
    expect(decodeUploadChatAttachment(staged)).toEqual(staged);
    for (const dataUrl of [null, `data:${mimeType};base64,YWJj`]) {
      expect(isUploadChatAttachment({ ...staged, dataUrl })).toBe(false);
    }
    const { uploadId: _uploadId, ...metadata } = staged;
    expect(
      isInlineUploadChatAttachment({
        ...metadata,
        dataUrl: `data:${mimeType};base64,YWJj`,
      }),
    ).toBe(true);
    expect(isUploadChatAttachment({ ...staged, uploadId: null })).toBe(false);
  }
});
