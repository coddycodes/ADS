import { ProviderDriverKind } from "@bibcode/contracts";
import {
  ACTIVITY_PAGE_MAX_LENGTH,
  type ActivityScopeRef,
  MessageId,
  type TurnDelivery,
  type ServerProvider,
  EnvironmentId,
  ProjectId,
  ProviderInstanceId,
  ThreadId,
  TurnId,
} from "@bibcode/contracts";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { DEFAULT_SERVER_SETTINGS } from "@bibcode/contracts/settings";
import { applyProviderInstanceSettings, deriveProviderInstanceEntries } from "../providerInstances";

import type { ChatMessage, Thread } from "../types";
import type { ComposerAttachment, ComposerImageAttachment } from "../composerDraftStore";
import type { ActivityDetailPageData, ActivityRosterPageData } from "./activity/ActivityPanel";

const atomRegistry = vi.hoisted(() => ({
  get: vi.fn(),
  subscribe: vi.fn(),
}));

vi.mock("../rpc/atomRegistry", () => ({
  appAtomRegistry: atomRegistry,
}));

import {
  MAX_HIDDEN_MOUNTED_PREVIEW_THREADS,
  buildLocalDraftThread,
  buildRevertCheckpointConfirmation,
  buildExpiredTerminalContextToastCopy,
  buildThreadTurnInterruptInput,
  cloneComposerAttachmentForRetry,
  collectUserMessageBlobPreviewUrls,
  createLocalDispatchSnapshot,
  deriveComposerSendState,
  deriveLockedProvider,
  describeUnavailableEnvironment,
  findActiveDeliveryMessage,
  deliveryOffersRetry,
  deliveryProviderLabel,
  findBlockingDelivery,
  waitsBehind,
  findLastCancellableDeliveryMessage,
  selectQueuedMessages,
  isQueuedTimelineMessage,
  deriveQueuedCardStatus,
  shouldEnqueueOnSend,
  getStartedThreadModelChangeBlockReason,
  hasServerAcknowledgedLocalDispatch,
  readFileAsDataUrl,
  reconcileBoundedActivityPages,
  resolveActivityPageLoadAction,
  reconcileRetainedMountedThreadIds,
  revokeBlobPreviewUrl,
  revokeUserMessagePreviewUrls,
  resolveCenterPanelLaunchContext,
  isAgentActivityScopeEnabled,
  resolveActivityScope,
  resolveSendEnvMode,
  shouldWriteThreadErrorToCurrentServerThread,
  threadHasStarted,
  waitForStartedServerThread,
  threadErrorAttribution,
  threadErrorCompactAction,
  COMPACT_CONVERSATION_COMMAND,
  prepareTurnAttachments,
  mergeInterruptedComposerContent,
  reusableStagedAttachmentAttempt,
} from "./ChatView.logic";
import { AsyncResult } from "effect/unstable/reactivity";
import * as Cause from "effect/Cause";
import type { AttachmentStagingResult } from "@bibcode/client-runtime/operations";

describe("staged attachment send seams", () => {
  const attachment = {
    type: "file" as const,
    id: "file-1",
    name: "a.txt",
    mimeType: "text/plain",
    sizeBytes: 3,
    file: new Blob(["abc"]),
  };
  const staged = {
    _tag: "staged" as const,
    attachments: [
      {
        type: "file" as const,
        id: "file-1",
        name: "a.txt",
        mimeType: "text/plain",
        sizeBytes: 3,
        uploadId: "stage-1",
      },
    ],
  };
  it("staged send does not read data URLs, while inline fallback does", async () => {
    const read = vi.fn().mockResolvedValue("data:text/plain;base64,YWJj");
    const result = await prepareTurnAttachments({
      sources: [attachment],
      stage: async () => AsyncResult.success(staged),
      readInline: read,
    });
    expect(result).toMatchObject({ _tag: "Success", value: staged.attachments });
    expect(read).not.toHaveBeenCalled();
    const inline = await prepareTurnAttachments({
      sources: [attachment],
      stage: async () => AsyncResult.success({ _tag: "inline" }),
      readInline: read,
    });
    expect(inline).toMatchObject({
      _tag: "Success",
      value: [
        {
          type: "file",
          id: "file-1",
          name: "a.txt",
          mimeType: "text/plain",
          sizeBytes: 3,
          dataUrl: "data:text/plain;base64,YWJj",
        },
      ],
    });
    if (inline._tag === "Success") expect(inline.value[0]).not.toHaveProperty("uploadId");
    expect(read).toHaveBeenCalledExactlyOnceWith(attachment.file);
  });
  it("preserves interruption without constructing bytes or a replacement error", async () => {
    const interrupted = AsyncResult.failure<AttachmentStagingResult>(Cause.interrupt());
    const read = vi.fn();
    const result = await prepareTurnAttachments({
      sources: [attachment],
      stage: async () => interrupted,
      readInline: read,
    });
    expect(result._tag).toBe("Failure");
    if (result._tag === "Failure") expect(result.cause).toBe(interrupted.cause);
    expect(read).not.toHaveBeenCalled();
  });
  it("restores outgoing content alongside fresh edits and deduplicates context ids", () => {
    const current = {
      prompt: "new work",
      attachments: [{ id: "new" }],
      terminalContexts: [{ id: "same", text: "new terminal" }],
      elementContexts: [{ id: "new element" }],
      previewAnnotations: [{ id: "new preview" }],
      reviewComments: [{ id: "new review" }],
    };
    const outgoing = {
      prompt: "sent work",
      attachments: [{ id: "sent" }],
      terminalContexts: [{ id: "same", text: "old terminal" }, { id: "old" }],
      elementContexts: [{ id: "sent element" }],
      previewAnnotations: [{ id: "sent preview" }],
      reviewComments: [{ id: "sent review" }],
    };
    const merged = mergeInterruptedComposerContent(current, outgoing);
    expect(merged.prompt).toBe("new work\n\nsent work");
    expect(merged.attachments.map((value) => value.id)).toEqual(["new", "sent"]);
    expect(merged.terminalContexts).toEqual([
      current.terminalContexts[0],
      outgoing.terminalContexts[1],
    ]);
    expect(merged.elementContexts.map((value) => value.id)).toEqual([
      "new element",
      "sent element",
    ]);
    expect(merged.previewAnnotations.map((value) => value.id)).toEqual([
      "new preview",
      "sent preview",
    ]);
    expect(merged.reviewComments.map((value) => value.id)).toEqual(["new review", "sent review"]);
    expect(current.prompt).toBe("new work");
  });
  it("reuses completed bytes only for unchanged files in the same thread before expiry", () => {
    const attempt = {
      threadKey: "host:thread",
      createdAt: 1000,
      sources: [attachment],
      attachments: staged.attachments,
    };
    expect(reusableStagedAttachmentAttempt(attempt, "host:thread", [attachment], 2000)).toBe(
      staged.attachments,
    );
    expect(reusableStagedAttachmentAttempt(attempt, "other:thread", [attachment], 2000)).toBeNull();
    expect(
      reusableStagedAttachmentAttempt(
        attempt,
        "host:thread",
        [{ ...attachment, file: new Blob(["xyz"]) }],
        2000,
      ),
    ).toBeNull();
    expect(
      reusableStagedAttachmentAttempt(
        attempt,
        "host:thread",
        [{ ...attachment, name: "renamed.txt" }],
        2000,
      ),
    ).toBeNull();
    expect(
      reusableStagedAttachmentAttempt(attempt, "host:thread", [attachment], 601000),
    ).toBeNull();
  });
});

const environmentId = EnvironmentId.make("environment-local");
const projectId = ProjectId.make("project-1");
const threadId = ThreadId.make("thread-1");
const now = "2026-03-29T00:00:00.000Z";

describe("activity integration helpers", () => {
  it.each([
    [{ _tag: "thread", threadId: "thread-1" }, true, false, true],
    [{ _tag: "thread", threadId: "thread-1" }, false, true, false],
    [{ _tag: "terminal", threadId: "thread-1", terminalId: "term-1" }, false, true, true],
    [{ _tag: "terminal", threadId: "thread-1", terminalId: "term-1" }, true, false, false],
  ])("selects the matching source setting", (scope, chat, terminal, expected) => {
    expect(
      isAgentActivityScopeEnabled(scope as ActivityScopeRef, {
        enableChatAgentActivity: chat,
        enableTerminalAgentActivity: terminal,
      }),
    ).toBe(expected);
  });

  it("refreshes a failed cursor page instead of reselecting the same cursor", () => {
    expect(
      resolveActivityPageLoadAction({
        currentCursor: "next",
        nextCursor: "next",
        failed: true,
      }),
    ).toBe("refresh");
    expect(
      resolveActivityPageLoadAction({
        currentCursor: null,
        nextCursor: "next",
        failed: false,
      }),
    ).toBe("select");
    expect(
      resolveActivityPageLoadAction({
        currentCursor: "next",
        nextCursor: null,
        failed: false,
      }),
    ).toBe("refresh");
  });

  it("resolves thread and terminal activity scopes from the active chat only", () => {
    const activeThreadRef = { environmentId, threadId };

    expect(resolveActivityScope(activeThreadRef, { _tag: "thread" })).toEqual({
      _tag: "thread",
      threadId,
    });
    expect(
      resolveActivityScope(activeThreadRef, {
        _tag: "terminal",
        terminalId: "terminal-1",
      }),
    ).toEqual({
      _tag: "terminal",
      threadId,
      terminalId: "terminal-1",
    });
    expect(resolveActivityScope(null, { _tag: "thread" })).toBeNull();
  });

  it("keeps the newest roster window bounded while preserving forward pagination", () => {
    const records = Array.from({ length: 150 }, (_, index) => ({
      _tag: "actor" as const,
      id: `actor-${index}`,
    }));
    const first = { records, nextCursor: "next" } as unknown as ActivityRosterPageData;
    const second = {
      records: Array.from({ length: 100 }, (_, index) => ({
        _tag: "actor" as const,
        id: `later-${index}`,
      })),
      nextCursor: null,
    } as unknown as ActivityRosterPageData;

    const initial = reconcileBoundedActivityPages([], null, first);
    const accumulated = reconcileBoundedActivityPages(initial, "next", second);
    const replaced = reconcileBoundedActivityPages(accumulated, "next", {
      ...second,
      records: second.records.slice(0, 25),
    });

    expect(accumulated.flatMap(({ page }) => page.records)).toHaveLength(ACTIVITY_PAGE_MAX_LENGTH);
    expect(accumulated[0]?.page.records).toHaveLength(100);
    expect(accumulated[1]?.page.records).toHaveLength(100);
    expect(accumulated[1]?.page.nextCursor).toBeNull();
    expect(replaced).toHaveLength(2);
    expect(replaced[1]?.page.records).toHaveLength(25);
  });

  it("preserves a full roster page cursor and advances beyond the first 200 records", () => {
    const rosterPage = (prefix: string, nextCursor: string | null) =>
      ({
        records: Array.from({ length: ACTIVITY_PAGE_MAX_LENGTH }, (_, index) => ({
          _tag: "actor" as const,
          id: `${prefix}-${index}`,
        })),
        nextCursor,
      }) as unknown as ActivityRosterPageData;

    const first = reconcileBoundedActivityPages([], null, rosterPage("first", "page-2"));
    expect(first[0]?.page.nextCursor).toBe("page-2");

    const second = reconcileBoundedActivityPages(first, "page-2", rosterPage("second", "page-3"));
    expect(second).toHaveLength(1);
    expect(second[0]?.cursor).toBe("page-2");
    expect(second[0]?.page.records[0]?.id).toBe("second-0");
    expect(second[0]?.page.nextCursor).toBe("page-3");
  });

  it("keeps the newest detail window bounded without destroying its cursor", () => {
    const detailPage = (prefix: string, length: number, nextCursor: string | null) =>
      ({
        record: { _tag: "actor", id: "actor-1" },
        entries: Array.from({ length }, (_, index) => ({ id: `${prefix}-${index}` })),
        nextCursor,
      }) as unknown as ActivityDetailPageData;

    const pages = reconcileBoundedActivityPages(
      reconcileBoundedActivityPages([], null, detailPage("first", 175, "next")),
      "next",
      detailPage("second", 50, null),
    );

    expect(pages.flatMap(({ page }) => page.entries)).toHaveLength(ACTIVITY_PAGE_MAX_LENGTH);
    expect(pages[0]?.page.entries).toHaveLength(150);
    expect(pages[1]?.page.entries).toHaveLength(50);
  });
});

beforeEach(() => {
  atomRegistry.get.mockReset();
  atomRegistry.subscribe.mockReset();
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

function makeThread(overrides: Partial<Thread> = {}): Thread {
  return {
    id: threadId,
    environmentId,
    projectId,
    title: "Thread",
    modelSelection: {
      instanceId: ProviderInstanceId.make("codex"),
      model: "gpt-5.4",
    },
    runtimeMode: "full-access",
    interactionMode: "default",
    session: null,
    messages: [],
    proposedPlans: [],
    activities: [],
    checkpoints: [],
    createdAt: now,
    updatedAt: now,
    archivedAt: null,
    deletedAt: null,
    latestTurn: null,
    branch: null,
    worktreePath: null,
    ...overrides,
  };
}

const completedTurn = {
  turnId: TurnId.make("turn-1"),
  state: "completed" as const,
  requestedAt: now,
  startedAt: "2026-03-29T00:00:01.000Z",
  completedAt: "2026-03-29T00:00:10.000Z",
  assistantMessageId: null,
};

const readySession = {
  threadId,
  status: "ready" as const,
  providerName: "codex",
  providerInstanceId: ProviderInstanceId.make("codex"),
  runtimeMode: "full-access" as const,
  activeTurnId: null,
  lastError: null,
  updatedAt: "2026-03-29T00:00:10.000Z",
};

describe("local draft and attachment helpers", () => {
  it("keeps center panels unavailable for local drafts", () => {
    expect(
      resolveCenterPanelLaunchContext({
        hasServerThread: false,
        envMode: "local",
        projectCwd: "/repo",
        worktreePath: null,
      }),
    ).toBeNull();
  });

  it("keeps center panels unavailable while a live thread is preparing a worktree", () => {
    expect(
      resolveCenterPanelLaunchContext({
        hasServerThread: true,
        envMode: "worktree",
        projectCwd: "/repo",
        worktreePath: null,
      }),
    ).toBeNull();
  });

  it.each([
    {
      name: "explicit project-root mode",
      input: {
        hasServerThread: true,
        envMode: "local" as const,
        projectCwd: "/repo",
        worktreePath: null,
      },
      expected: { cwd: "/repo", worktreePath: null },
    },
    {
      name: "resolved worktree mode",
      input: {
        hasServerThread: true,
        envMode: "worktree" as const,
        projectCwd: "/repo",
        worktreePath: "/repo/.bibcode/worktrees/feature",
      },
      expected: {
        cwd: "/repo/.bibcode/worktrees/feature",
        worktreePath: "/repo/.bibcode/worktrees/feature",
      },
    },
  ])(
    "resolves center-panel launch context for a ready live $name thread",
    ({ input, expected }) => {
      expect(resolveCenterPanelLaunchContext(input)).toEqual(expected);
    },
  );

  it("projects draft routing metadata into a local thread", () => {
    const result = buildLocalDraftThread(
      threadId,
      {
        threadId,
        environmentId,
        projectId,
        logicalProjectKey: "local:project-1",
        createdAt: now,
        runtimeMode: "approval-required",
        interactionMode: "plan",
        branch: "feature/test",
        worktreePath: "/tmp/worktree",
        envMode: "worktree",
        startFromOrigin: true,
      },
      {
        instanceId: ProviderInstanceId.make("codex"),
        model: "gpt-5.4",
      },
    );

    expect(result).toMatchObject({
      id: threadId,
      environmentId,
      projectId,
      title: "New thread",
      runtimeMode: "approval-required",
      interactionMode: "plan",
      branch: "feature/test",
      worktreePath: "/tmp/worktree",
      createdAt: now,
      updatedAt: now,
    });
  });

  it("revokes only blob image previews and collects the same URLs", () => {
    const revokeObjectURL = vi.fn();
    vi.stubGlobal("URL", { revokeObjectURL });
    const message = {
      role: "user",
      attachments: [
        { type: "text", previewUrl: "blob:not-an-image" },
        { type: "image", previewUrl: undefined },
        { type: "image", previewUrl: "https://example.test/image.png" },
        { type: "image", previewUrl: "blob:first" },
        { type: "image", previewUrl: "blob:second" },
      ],
    } as unknown as ChatMessage;

    revokeBlobPreviewUrl(undefined);
    revokeBlobPreviewUrl("https://example.test/image.png");
    revokeUserMessagePreviewUrls({ role: "assistant" } as ChatMessage);
    revokeUserMessagePreviewUrls(message);

    expect(revokeObjectURL).toHaveBeenCalledTimes(2);
    expect(revokeObjectURL).toHaveBeenNthCalledWith(1, "blob:first");
    expect(revokeObjectURL).toHaveBeenNthCalledWith(2, "blob:second");
    expect(collectUserMessageBlobPreviewUrls({ role: "system" } as ChatMessage)).toEqual([]);
    expect(collectUserMessageBlobPreviewUrls(message)).toEqual(["blob:first", "blob:second"]);
  });

  it("leaves blob previews alone when the URL API is unavailable", () => {
    vi.stubGlobal("URL", undefined);
    expect(() => revokeBlobPreviewUrl("blob:preview")).not.toThrow();
  });

  it("clones retry blob URLs only for images and preserves files", () => {
    const file = new File(["image"], "image.png", { type: "image/png" });
    const image: ComposerImageAttachment = {
      type: "image",
      id: "image-1",
      name: "image.png",
      mimeType: "image/png",
      sizeBytes: file.size,
      previewUrl: "blob:original",
      file,
    };
    const createObjectURL = vi.fn(() => "blob:retry");
    vi.stubGlobal("URL", { createObjectURL });

    expect(cloneComposerAttachmentForRetry(image)).toEqual({ ...image, previewUrl: "blob:retry" });
    expect(createObjectURL).toHaveBeenCalledWith(file);
    expect(
      cloneComposerAttachmentForRetry({ ...image, previewUrl: "data:image/png;base64,eA==" }),
    ).toEqual({
      ...image,
      previewUrl: "data:image/png;base64,eA==",
    });

    createObjectURL.mockImplementation(() => {
      throw new Error("object URLs disabled");
    });
    expect(cloneComposerAttachmentForRetry(image)).toBe(image);

    const fileAttachment: ComposerAttachment = {
      type: "file",
      id: "file-1",
      name: "notes.txt",
      mimeType: "text/plain",
      sizeBytes: file.size,
      file,
    };
    expect(cloneComposerAttachmentForRetry(fileAttachment)).toBe(fileAttachment);
    expect(createObjectURL).toHaveBeenCalledTimes(2);

    vi.stubGlobal("URL", undefined);
    expect(cloneComposerAttachmentForRetry(image)).toBe(image);
  });
});

describe("readFileAsDataUrl", () => {
  class ScriptedFileReader {
    static outcomes: Array<{ result?: string | ArrayBuffer | null; error?: Error | null }> = [];
    result: string | ArrayBuffer | null = null;
    error: Error | null = null;
    private readonly listeners = new Map<string, () => void>();

    addEventListener(type: string, listener: () => void): void {
      this.listeners.set(type, listener);
    }

    readAsDataURL(): void {
      const outcome = ScriptedFileReader.outcomes.shift() ?? {};
      this.result = outcome.result ?? null;
      this.error = outcome.error ?? null;
      this.listeners.get("error" in outcome ? "error" : "load")?.();
    }
  }

  it("resolves string data and rejects non-string reader results", async () => {
    vi.stubGlobal("FileReader", ScriptedFileReader);
    const file = new File(["image"], "image.png", { type: "image/png" });
    ScriptedFileReader.outcomes.push(
      { result: "data:image/png;base64,aW1hZ2U=" },
      { result: new ArrayBuffer(1) },
    );

    await expect(readFileAsDataUrl(file)).resolves.toBe("data:image/png;base64,aW1hZ2U=");
    await expect(readFileAsDataUrl(file)).rejects.toThrow("Could not read attachment data.");
  });

  it("preserves reader errors and supplies a fallback error", async () => {
    vi.stubGlobal("FileReader", ScriptedFileReader);
    const file = new File(["image"], "image.png", { type: "image/png" });
    const failure = new Error("reader failed");
    ScriptedFileReader.outcomes.push({ error: failure }, { error: null });

    await expect(readFileAsDataUrl(file)).rejects.toBe(failure);
    await expect(readFileAsDataUrl(file)).rejects.toThrow("Failed to read attachment.");
  });
});

describe("thread lifecycle helpers", () => {
  it("detects every server-start signal and derives the locked provider fallback", () => {
    expect(threadHasStarted(undefined)).toBe(false);
    expect(threadHasStarted(makeThread())).toBe(false);
    expect(threadHasStarted(makeThread({ messages: [{ id: "message" } as never] }))).toBe(true);
    expect(threadHasStarted(makeThread({ latestTurn: completedTurn }))).toBe(true);
    expect(threadHasStarted(makeThread({ session: readySession }))).toBe(true);

    expect(
      deriveLockedProvider({
        thread: makeThread(),
        selectedProvider: "codex",
        threadProvider: "cursor",
      }),
    ).toBeNull();
    expect(
      deriveLockedProvider({
        thread: makeThread({ session: readySession }),
        selectedProvider: "cursor",
        threadProvider: "grok",
      }),
    ).toBe("codex");
    expect(
      deriveLockedProvider({
        thread: makeThread({
          session: { ...readySession, providerName: "" as never },
        }),
        selectedProvider: "cursor",
        threadProvider: "grok",
      }),
    ).toBe("grok");
    expect(
      deriveLockedProvider({
        thread: makeThread({ messages: [{ id: "message" } as never] }),
        selectedProvider: "cursor",
        threadProvider: "",
      }),
    ).toBe("cursor");
    expect(
      deriveLockedProvider({
        thread: makeThread({ latestTurn: completedTurn }),
        selectedProvider: "",
        threadProvider: null,
      }),
    ).toBeNull();
  });

  it("waits for subscription acknowledgement, immediate refresh, and timeout", async () => {
    const started = makeThread({ latestTurn: completedTurn });
    const unsubscribe = vi.fn();
    let listener: ((thread: Thread | undefined) => void) | undefined;
    atomRegistry.subscribe.mockImplementation((_atom, nextListener) => {
      listener = nextListener;
      return unsubscribe;
    });

    atomRegistry.get.mockReturnValueOnce(started);
    await expect(waitForStartedServerThread({ environmentId, threadId }, 50)).resolves.toBe(true);
    expect(atomRegistry.subscribe).not.toHaveBeenCalled();

    atomRegistry.get.mockReset();
    atomRegistry.get.mockReturnValue(undefined);
    const subscribed = waitForStartedServerThread({ environmentId, threadId }, 50);
    listener?.(started);
    await expect(subscribed).resolves.toBe(true);
    expect(unsubscribe).toHaveBeenCalledTimes(1);

    atomRegistry.get.mockReset();
    atomRegistry.get.mockReturnValueOnce(undefined).mockReturnValueOnce(started);
    await expect(waitForStartedServerThread({ environmentId, threadId }, 50)).resolves.toBe(true);

    vi.useFakeTimers();
    atomRegistry.get.mockReset();
    atomRegistry.get.mockReturnValue(undefined);
    const timedOut = waitForStartedServerThread({ environmentId, threadId }, 50);
    await vi.advanceTimersByTimeAsync(50);
    await expect(timedOut).resolves.toBe(false);
  });
});

describe("buildThreadTurnInterruptInput", () => {
  it("targets the session's active running turn", () => {
    const activeTurnId = TurnId.make("turn-running");

    expect(
      buildThreadTurnInterruptInput(
        makeThread({
          session: {
            ...readySession,
            status: "running",
            activeTurnId,
          },
        }),
      ),
    ).toEqual({ threadId, turnId: activeTurnId });
  });

  it("omits a turn id when the session is not running", () => {
    expect(buildThreadTurnInterruptInput(makeThread({ session: readySession }))).toEqual({
      threadId,
    });
  });
});

describe("deriveComposerSendState", () => {
  it("treats expired terminal pills as non-sendable content", () => {
    const state = deriveComposerSendState({
      prompt: "\uFFFC",
      imageCount: 0,
      terminalContexts: [
        {
          id: "ctx-expired",
          threadId,
          terminalId: "default",
          terminalLabel: "Terminal 1",
          lineStart: 4,
          lineEnd: 4,
          text: "",
          createdAt: now,
        },
      ],
    });

    expect(state.trimmedPrompt).toBe("");
    expect(state.sendableTerminalContexts).toEqual([]);
    expect(state.expiredTerminalContextCount).toBe(1);
    expect(state.hasSendableContent).toBe(false);
  });

  it("keeps text sendable while excluding expired terminal pills", () => {
    const state = deriveComposerSendState({
      prompt: `yoo \uFFFC waddup`,
      imageCount: 0,
      terminalContexts: [
        {
          id: "ctx-expired",
          threadId,
          terminalId: "default",
          terminalLabel: "Terminal 1",
          lineStart: 4,
          lineEnd: 4,
          text: "",
          createdAt: now,
        },
      ],
    });

    expect(state.trimmedPrompt).toBe("yoo  waddup");
    expect(state.expiredTerminalContextCount).toBe(1);
    expect(state.hasSendableContent).toBe(true);
  });

  it("treats element contexts as sendable content (no text, no images, no terminals)", () => {
    const state = deriveComposerSendState({
      prompt: "",
      imageCount: 0,
      terminalContexts: [],
      elementContextCount: 1,
    });

    expect(state.trimmedPrompt).toBe("");
    expect(state.expiredTerminalContextCount).toBe(0);
    expect(state.hasSendableContent).toBe(true);
  });

  it("does NOT treat zero element contexts as sendable", () => {
    expect(
      deriveComposerSendState({
        prompt: "",
        imageCount: 0,
        terminalContexts: [],
        elementContextCount: 0,
      }).hasSendableContent,
    ).toBe(false);
  });
});

describe("buildExpiredTerminalContextToastCopy", () => {
  it("formats empty and omission guidance", () => {
    expect(buildExpiredTerminalContextToastCopy(1, "empty")).toEqual({
      title: "Expired terminal context won't be sent",
      description: "Remove it or re-add it to include terminal output.",
    });
    expect(buildExpiredTerminalContextToastCopy(2, "omitted")).toEqual({
      title: "Expired terminal contexts omitted from message",
      description: "Re-add it if you want that terminal output included.",
    });
  });
});

describe("getStartedThreadModelChangeBlockReason", () => {
  const providers = [
    {
      instanceId: ProviderInstanceId.make("codex"),
    },
    {
      instanceId: ProviderInstanceId.make("grok"),
      requiresNewThreadForModelChange: true,
    },
  ];

  it("allows model changes before a provider session has started", () => {
    expect(
      getStartedThreadModelChangeBlockReason({
        providers,
        hasStartedSession: false,
        currentModelSelection: {
          instanceId: ProviderInstanceId.make("grok"),
          model: "grok-build",
        },
        nextModelSelection: {
          instanceId: ProviderInstanceId.make("grok"),
          model: "grok-other",
        },
      }),
    ).toBeNull();
  });

  it("allows unchanged model selections for restricted providers", () => {
    expect(
      getStartedThreadModelChangeBlockReason({
        providers,
        hasStartedSession: true,
        currentModelSelection: {
          instanceId: ProviderInstanceId.make("grok"),
          model: "grok-build",
        },
        nextModelSelection: {
          instanceId: ProviderInstanceId.make("grok"),
          model: "grok-build",
        },
      }),
    ).toBeNull();
  });

  it("blocks started-session model changes when either provider requires a new thread", () => {
    expect(
      getStartedThreadModelChangeBlockReason({
        providers,
        hasStartedSession: true,
        currentModelSelection: {
          instanceId: ProviderInstanceId.make("codex"),
          model: "gpt-5.4",
        },
        nextModelSelection: {
          instanceId: ProviderInstanceId.make("grok"),
          model: "grok-build",
        },
      }),
    ).toEqual({
      title: "Start a new chat to change models",
      description:
        "This provider does not allow switching models after a conversation has started.",
    });
  });

  it("blocks a restricted current provider and allows changes between unrestricted providers", () => {
    expect(
      getStartedThreadModelChangeBlockReason({
        providers,
        hasStartedSession: true,
        currentModelSelection: {
          instanceId: ProviderInstanceId.make("codex"),
          model: "stale-model",
        },
        currentProviderInstanceId: ProviderInstanceId.make("grok"),
        nextModelSelection: {
          instanceId: ProviderInstanceId.make("codex"),
          model: "gpt-5.4",
        },
      }),
    ).not.toBeNull();

    expect(
      getStartedThreadModelChangeBlockReason({
        providers,
        hasStartedSession: true,
        currentModelSelection: {
          instanceId: ProviderInstanceId.make("codex"),
          model: "gpt-5.4",
        },
        nextModelSelection: {
          instanceId: ProviderInstanceId.make("codex"),
          model: "gpt-5.5",
        },
      }),
    ).toBeNull();
  });
});

describe("resolveSendEnvMode", () => {
  it("keeps worktree mode only for git repositories", () => {
    expect(resolveSendEnvMode({ requestedEnvMode: "worktree", isGitRepo: true })).toBe("worktree");
    expect(resolveSendEnvMode({ requestedEnvMode: "worktree", isGitRepo: false })).toBe("local");
  });
});

describe("reconcileRetainedMountedThreadIds", () => {
  it("retains hidden open threads and adds the active open thread", () => {
    expect(
      reconcileRetainedMountedThreadIds({
        currentThreadIds: [ThreadId.make("thread-hidden")],
        openThreadIds: [ThreadId.make("thread-hidden")],
        activeThreadId: ThreadId.make("thread-active"),
        activeThreadOpen: true,
        maxHiddenThreadCount: MAX_HIDDEN_MOUNTED_PREVIEW_THREADS,
      }),
    ).toEqual([ThreadId.make("thread-hidden"), ThreadId.make("thread-active")]);
  });

  it("can retain the active thread as hidden when it is inactive", () => {
    expect(
      reconcileRetainedMountedThreadIds({
        currentThreadIds: [ThreadId.make("thread-active")],
        openThreadIds: [ThreadId.make("thread-active")],
        activeThreadId: ThreadId.make("thread-active"),
        activeThreadOpen: false,
        maxHiddenThreadCount: MAX_HIDDEN_MOUNTED_PREVIEW_THREADS,
        retainInactiveActiveThread: true,
      }),
    ).toEqual([ThreadId.make("thread-active")]);
  });

  it("evicts the oldest hidden threads beyond the configured cap", () => {
    const currentThreadIds = Array.from(
      { length: MAX_HIDDEN_MOUNTED_PREVIEW_THREADS + 2 },
      (_, index) => ThreadId.make(`thread-${index + 1}`),
    );

    expect(
      reconcileRetainedMountedThreadIds({
        currentThreadIds,
        openThreadIds: currentThreadIds,
        activeThreadId: null,
        activeThreadOpen: false,
        maxHiddenThreadCount: MAX_HIDDEN_MOUNTED_PREVIEW_THREADS,
      }),
    ).toEqual(currentThreadIds.slice(-MAX_HIDDEN_MOUNTED_PREVIEW_THREADS));
  });
});

describe("shouldWriteThreadErrorToCurrentServerThread", () => {
  it("requires the environment, route thread, and target thread to match", () => {
    const routeThreadRef = { environmentId, threadId };

    expect(
      shouldWriteThreadErrorToCurrentServerThread({
        serverThread: { environmentId, id: threadId },
        routeThreadRef,
        targetThreadId: threadId,
      }),
    ).toBe(true);
    expect(
      shouldWriteThreadErrorToCurrentServerThread({
        serverThread: null,
        routeThreadRef,
        targetThreadId: threadId,
      }),
    ).toBe(false);
  });
});

describe("hasServerAcknowledgedLocalDispatch", () => {
  it("does not acknowledge without a local dispatch snapshot", () => {
    expect(
      hasServerAcknowledgedLocalDispatch({
        localDispatch: null,
        phase: "ready",
        latestTurn: null,
        session: null,
        hasPendingApproval: false,
        hasPendingUserInput: false,
        threadError: null,
      }),
    ).toBe(false);
  });

  it("does not acknowledge unchanged server state", () => {
    const localDispatch = createLocalDispatchSnapshot(
      makeThread({ latestTurn: completedTurn, session: readySession }),
    );

    expect(
      hasServerAcknowledgedLocalDispatch({
        localDispatch,
        phase: "ready",
        latestTurn: completedTurn,
        session: readySession,
        hasPendingApproval: false,
        hasPendingUserInput: false,
        threadError: null,
      }),
    ).toBe(false);
  });

  it("acknowledges a settled newer turn", () => {
    const localDispatch = createLocalDispatchSnapshot(
      makeThread({ latestTurn: completedTurn, session: readySession }),
    );
    const newerTurn = {
      ...completedTurn,
      turnId: TurnId.make("turn-2"),
      requestedAt: "2026-03-29T00:01:00.000Z",
      startedAt: "2026-03-29T00:01:01.000Z",
      completedAt: "2026-03-29T00:01:30.000Z",
    };

    expect(
      hasServerAcknowledgedLocalDispatch({
        localDispatch,
        phase: "ready",
        latestTurn: newerTurn,
        session: { ...readySession, updatedAt: newerTurn.completedAt },
        hasPendingApproval: false,
        hasPendingUserInput: false,
        threadError: null,
      }),
    ).toBe(true);
  });

  it("waits for the matching running turn before acknowledging", () => {
    const localDispatch = createLocalDispatchSnapshot(
      makeThread({ latestTurn: completedTurn, session: readySession }),
    );
    const runningTurn = {
      ...completedTurn,
      turnId: TurnId.make("turn-2"),
      state: "running" as const,
      requestedAt: "2026-03-29T00:01:00.000Z",
      startedAt: "2026-03-29T00:01:01.000Z",
      completedAt: null,
    };

    expect(
      hasServerAcknowledgedLocalDispatch({
        localDispatch,
        phase: "running",
        latestTurn: runningTurn,
        session: {
          ...readySession,
          status: "running",
          activeTurnId: TurnId.make("turn-other"),
        },
        hasPendingApproval: false,
        hasPendingUserInput: false,
        threadError: null,
      }),
    ).toBe(false);
    expect(
      hasServerAcknowledgedLocalDispatch({
        localDispatch,
        phase: "running",
        latestTurn: runningTurn,
        session: {
          ...readySession,
          status: "running",
          activeTurnId: runningTurn.turnId,
        },
        hasPendingApproval: false,
        hasPendingUserInput: false,
        threadError: null,
      }),
    ).toBe(true);
  });

  it("acknowledges pending user interaction and errors immediately", () => {
    const localDispatch = createLocalDispatchSnapshot(makeThread());
    const common = {
      localDispatch,
      phase: "ready" as const,
      latestTurn: null,
      session: null,
      hasPendingApproval: false,
      hasPendingUserInput: false,
      threadError: null,
    };

    expect(hasServerAcknowledgedLocalDispatch({ ...common, hasPendingApproval: true })).toBe(true);
    expect(hasServerAcknowledgedLocalDispatch({ ...common, hasPendingUserInput: true })).toBe(true);
    expect(hasServerAcknowledgedLocalDispatch({ ...common, threadError: "failed" })).toBe(true);
  });

  it("does not mistake a pre-existing thread error for acknowledgement of a new send", () => {
    const localDispatch = createLocalDispatchSnapshot(makeThread(), {
      threadError: "Provider session ended when BiBCode stopped.",
    });
    const common = {
      localDispatch,
      phase: "ready" as const,
      latestTurn: null,
      session: null,
      hasPendingApproval: false,
      hasPendingUserInput: false,
    };

    expect(
      hasServerAcknowledgedLocalDispatch({
        ...common,
        threadError: "Provider session ended when BiBCode stopped.",
      }),
    ).toBe(false);
    expect(
      hasServerAcknowledgedLocalDispatch({
        ...common,
        threadError: "The new send failed.",
      }),
    ).toBe(true);
  });

  it("does not acknowledge when a pre-existing local error is cleared before admission", () => {
    const localDispatch = createLocalDispatchSnapshot(makeThread(), {
      threadError: "The previous send failed.",
    });

    expect(
      hasServerAcknowledgedLocalDispatch({
        localDispatch,
        phase: "ready",
        latestTurn: null,
        session: null,
        hasPendingApproval: false,
        hasPendingUserInput: false,
        threadError: null,
      }),
    ).toBe(false);
  });

  it("acknowledges a persisted pending delivery before its provider turn starts", () => {
    const localDispatch = createLocalDispatchSnapshot(makeThread());

    expect(
      hasServerAcknowledgedLocalDispatch({
        localDispatch,
        phase: "ready",
        latestTurn: null,
        session: null,
        deliveryState: "pending",
        hasPendingApproval: false,
        hasPendingUserInput: false,
        threadError: null,
      }),
    ).toBe(true);
  });

  it("acknowledges terminal delivery failure without waiting for a provider session", () => {
    const localDispatch = createLocalDispatchSnapshot(makeThread());

    expect(
      hasServerAcknowledgedLocalDispatch({
        localDispatch,
        phase: "ready",
        latestTurn: null,
        session: null,
        deliveryState: "failed",
        hasPendingApproval: false,
        hasPendingUserInput: false,
        threadError: null,
      }),
    ).toBe(true);
  });

  it("requires a started latest turn while running and detects either session field settling", () => {
    const localDispatch = createLocalDispatchSnapshot(
      makeThread({ latestTurn: completedTurn, session: readySession }),
    );
    const common = {
      localDispatch,
      hasPendingApproval: false,
      hasPendingUserInput: false,
      threadError: null,
    };

    expect(
      hasServerAcknowledgedLocalDispatch({
        ...common,
        phase: "running",
        latestTurn: null,
        session: null,
      }),
    ).toBe(false);
    expect(
      hasServerAcknowledgedLocalDispatch({
        ...common,
        phase: "ready",
        latestTurn: completedTurn,
        session: { ...readySession, status: "running" },
      }),
    ).toBe(true);
    expect(
      hasServerAcknowledgedLocalDispatch({
        ...common,
        phase: "ready",
        latestTurn: completedTurn,
        session: { ...readySession, updatedAt: "2026-03-29T02:00:00.000Z" },
      }),
    ).toBe(true);
  });
});

describe("findLastCancellableDeliveryMessage", () => {
  it("keeps the latest pending or sending delivery cancellable after reload", () => {
    const message = (id: string, state: "delivered" | "pending" | "sending") =>
      ({ id, delivery: { state, provider: "opencode" } }) as ChatMessage;

    expect(
      findLastCancellableDeliveryMessage([
        message("delivered", "delivered"),
        message("pending", "pending"),
        message("sending", "sending"),
      ])?.id,
    ).toBe("sending");
  });
});

describe("findActiveDeliveryMessage", () => {
  const message = (
    id: string,
    state: "delivered" | "dismissed" | "failed" | "uncertain" | "pending" | "sending",
  ) =>
    ({
      id,
      delivery: { state, provider: "cursor" },
    }) as ChatMessage;

  it("returns the oldest claimable delivery after terminal predecessors", () => {
    expect(
      findActiveDeliveryMessage([
        message("delivered", "delivered"),
        message("dismissed", "dismissed"),
        message("pending", "pending"),
        message("sending", "sending"),
      ])?.id,
    ).toBe("pending");
  });

  it.each(["uncertain", "failed"] as const)(
    "does not report a queued successor as active behind an unresolved %s delivery",
    (state) => {
      expect(
        findActiveDeliveryMessage([message("blocker", state), message("queued", "pending")]),
      ).toBeNull();
    },
  );
});

describe.each([undefined, "start", "steer"] as const)("deliveryOffersRetry (mode=%s)", (mode) => {
  it.each([
    ["queued", true],
    ["pending", true],
    ["sending", true],
    ["delivered", true],
    ["uncertain", true],
    ["failed", false],
    ["dismissed", true],
  ] as const)("only suppresses Retry for a failed refusal (%s)", (state, refusedOffersRetry) => {
    const delivery: TurnDelivery = { state, provider: ProviderDriverKind.make("codex"), mode };
    expect(deliveryOffersRetry(delivery)).toBe(true);
    expect(deliveryOffersRetry({ ...delivery, reason: "modelSelectionRefused" })).toBe(
      refusedOffersRetry,
    );
  });
});

describe("findBlockingDelivery", () => {
  const message = (
    id: string,
    state: TurnDelivery["state"],
    mode: TurnDelivery["mode"] = "start",
  ): ChatMessage => ({
    id: MessageId.make(id),
    role: "user",
    text: id,
    createdAt: now,
    updatedAt: now,
    streaming: false,
    turnId: null,
    delivery: { state, mode, provider: ProviderDriverKind.make("codex") },
  });
  const nonBlocking = [
    message("queued", "queued"),
    message("pending-steer", "pending", "steer"),
    message("sending-steer", "sending", "steer"),
    message("pending", "pending"),
    message("sending", "sending"),
    message("delivered", "delivered"),
    message("dismissed", "dismissed"),
    { ...message("legacy", "delivered"), delivery: undefined },
  ];

  it("ignores queued, active, resolved, and legacy messages", () => {
    expect(findBlockingDelivery(nonBlocking)).toBeNull();
    expect(findBlockingDelivery([])).toBeNull();
  });

  it.each([
    ["failed", "start"],
    ["uncertain", "steer"],
    ["failed", "steer"],
    ["uncertain", "start"],
  ] as const)("finds the first unresolved %s %s", (state, mode) => {
    const blocker = message("blocker", state, mode);
    expect(
      findBlockingDelivery([...nonBlocking, blocker, message("later-failure", "failed")]),
    ).toBe(blocker);
  });
});

describe("waitsBehind", () => {
  const blocker = { id: MessageId.make("blocker"), createdAt: now };

  it.each([
    ["2026-03-29T00:00:01.000Z", true],
    ["2026-03-28T23:59:59.000Z", false],
    [now, false],
    ["2026-03-28T21:00:00.000-03:00", false],
    ["invalid time", false],
  ])("uses strictly later parsed timestamps (%s)", (createdAt, expected) => {
    expect(waitsBehind({ id: MessageId.make("message"), createdAt }, blocker)).toBe(expected);
  });

  it("does not block itself even if its timestamp is later", () => {
    expect(waitsBehind({ ...blocker, createdAt: "2026-03-29T00:00:01.000Z" }, blocker)).toBe(false);
  });

  it("stays permissive when the blocker timestamp is invalid or the blocker is absent", () => {
    const message = { id: MessageId.make("message"), createdAt: now };
    expect(waitsBehind(message, { ...blocker, createdAt: "invalid time" })).toBe(false);
    expect(waitsBehind(message, null)).toBe(false);
  });
});

describe("deliveryProviderLabel", () => {
  const instanceId = ProviderInstanceId.make("codex-personal");
  const provider = ProviderDriverKind.make("codex");
  const snapshot: ServerProvider = {
    instanceId,
    driver: provider,
    displayName: "Codex Personal",
    enabled: true,
    installed: true,
    version: null,
    status: "ready",
    auth: { status: "authenticated" },
    checkedAt: now,
    models: [],
    slashCommands: [],
    skills: [],
    agents: [],
  };

  function instanceLabels(displayName?: string): ReadonlyMap<ProviderInstanceId, string> {
    const settings = {
      ...DEFAULT_SERVER_SETTINGS,
      providerInstances: {
        [instanceId]: { driver: provider, ...(displayName ? { displayName } : {}) },
      },
    };
    const entries = applyProviderInstanceSettings(
      deriveProviderInstanceEntries([snapshot]),
      settings,
    );
    return new Map(entries.map((entry) => [entry.instanceId, entry.displayName]));
  }

  it("prefers the settings-configured name over the snapshot name", () => {
    expect(
      deliveryProviderLabel(
        { provider, providerInstanceId: instanceId },
        instanceLabels("My Codex"),
      ),
    ).toBe("My Codex");
  });

  it("uses the snapshot displayName without a configured name", () => {
    expect(
      deliveryProviderLabel({ provider, providerInstanceId: instanceId }, instanceLabels()),
    ).toBe("Codex Personal");
  });

  it("falls back to the driver when the instance is missing", () => {
    expect(
      deliveryProviderLabel(
        { provider, providerInstanceId: ProviderInstanceId.make("deleted-instance") },
        instanceLabels(),
      ),
    ).toBe("Codex");
  });

  it("falls back to the driver for an old server with no instance id", () => {
    expect(deliveryProviderLabel({ provider }, instanceLabels())).toBe("Codex");
  });

  it("formats an unknown driver when no display name is registered", () => {
    expect(
      deliveryProviderLabel(
        { provider: ProviderDriverKind.make("custom-agent") },
        instanceLabels(),
      ),
    ).toBe("Custom Agent");
  });
});

describe("threadErrorAttribution", () => {
  it("names the provider that reported the failure", () => {
    expect(
      threadErrorAttribution({
        isBiBCodeAction: false,
        errorClass: "provider_error",
        providerName: "claudeAgent",
      }),
    ).toBe("Claude reported an error");
  });

  it("attributes a failed BiBCode command to BiBCode without blaming it", () => {
    // The command is BiBCode's, but it may have failed because the provider
    // refused it, so the wording says what BiBCode could not do, not whose
    // fault it was.
    expect(
      threadErrorAttribution({
        isBiBCodeAction: true,
        errorClass: "provider_error",
        providerName: "codex",
      }),
    ).toBe("BiBCode could not complete this action");
  });

  it("distinguishes a lost connection from a provider failure", () => {
    expect(
      threadErrorAttribution({
        isBiBCodeAction: false,
        errorClass: "transport_error",
        providerName: "codex",
      }),
    ).toBe("BiBCode lost its connection to Codex");
  });

  it.each(["codex", "claudeAgent", null])(
    "describes an intentional session stop without claiming a lost connection (%s)",
    (providerName) => {
      expect(
        threadErrorAttribution({
          isBiBCodeAction: false,
          errorClass: "session_stopped",
          providerName,
        }),
      ).toBe("BiBCode stopped this session");
    },
  );

  it("separates credential rejection from a generic provider error", () => {
    expect(
      threadErrorAttribution({
        isBiBCodeAction: false,
        errorClass: "permission_error",
        providerName: "opencode",
      }),
    ).toBe("OpenCode rejected the credentials");
  });

  it("says when the conversation no longer fits the provider's context window", () => {
    expect(
      threadErrorAttribution({
        isBiBCodeAction: false,
        errorClass: "context_limit",
        providerName: "claudeAgent",
      }),
    ).toBe("Claude ran out of context");
  });

  it("offers to compact only a provider's context-limit failure", () => {
    expect(
      threadErrorCompactAction({
        isBiBCodeAction: false,
        errorClass: "context_limit",
        busy: false,
      }),
    ).toEqual({ disabledReason: null });
    for (const errorClass of ["provider_error", "unknown", null, undefined]) {
      expect(
        threadErrorCompactAction({ isBiBCodeAction: false, errorClass, busy: false }),
      ).toBeNull();
    }
    // A failed BiBCode command shown over a context-limit session is not the provider's failure.
    expect(
      threadErrorCompactAction({ isBiBCodeAction: true, errorClass: "context_limit", busy: false }),
    ).toBeNull();
  });

  it("is disabled with its reason while the thread is working, such as compacting", () => {
    expect(
      threadErrorCompactAction({ isBiBCodeAction: false, errorClass: "context_limit", busy: true }),
    ).toEqual({ disabledReason: "Wait for the current work to finish, then compact." });
    expect(COMPACT_CONVERSATION_COMMAND).toBe("/compact");
  });

  it("says nothing rather than guessing when the class is absent or unknown", () => {
    for (const errorClass of [null, undefined, "unknown", "a_class_from_a_newer_server"]) {
      expect(
        threadErrorAttribution({
          isBiBCodeAction: false,
          errorClass,
          providerName: "claudeAgent",
        }),
      ).toBeNull();
    }
  });

  it("falls back to a generic agent noun when the provider is unknown", () => {
    expect(
      threadErrorAttribution({
        isBiBCodeAction: false,
        errorClass: "provider_error",
        providerName: null,
      }),
    ).toBe("The agent reported an error");
  });
});

describe("queued messages", () => {
  const message = (id: string, delivery: TurnDelivery, createdAt = now): ChatMessage => ({
    id: MessageId.make(id),
    role: "user",
    text: id,
    createdAt,
    updatedAt: createdAt,
    streaming: false,
    turnId: null,
    delivery,
  });
  const queued: TurnDelivery = {
    state: "queued",
    mode: "start",
    provider: ProviderDriverKind.make("codex"),
  };
  const base = {
    index: 0,
    phase: "running" as const,
    sessionStatus: "running" as const,
    supportsTurnSteer: true,
    delivery: queued,
    hasPendingApproval: false,
    hasPendingUserInput: false,
    waitingBehind: null,
  };

  it("keeps durable array order and includes both in-flight steer states only once", () => {
    const first = message("first", queued, "2026-09-22T02:00:00Z");
    const second = message(
      "second",
      { ...queued, state: "pending", mode: "steer" },
      "2026-09-22T01:00:00Z",
    );
    const third = message("third", { ...queued, state: "sending", mode: "steer" });
    const delivered = message("delivered", { ...queued, state: "delivered", mode: "steer" });
    const start = message("start", { ...queued, state: "pending" });
    expect(selectQueuedMessages([first, second, third, delivered, start])).toEqual([
      first,
      second,
      third,
    ]);
    expect([first, second, third, delivered, start].map(isQueuedTimelineMessage)).toEqual([
      true,
      true,
      true,
      false,
      false,
    ]);
  });

  it.each(["queued", "pending", "sending"] as const)(
    "does not count %s queue/steer delivery as busy",
    (state) => {
      const messages = [
        message("queued", { ...queued, state, mode: state === "queued" ? "start" : "steer" }),
      ];
      expect(findLastCancellableDeliveryMessage(messages)).toBeNull();
      expect(findActiveDeliveryMessage(messages)).toBeNull();
    },
  );

  it("offers steer for a capable running head", () => {
    expect(deriveQueuedCardStatus(base)).toMatchObject({
      label: "Sends when the turn ends. Steer to send it now.",
      canSteer: true,
      steerDisabledReason: null,
      canCancel: true,
      steering: false,
      primaryAction: "steer",
    });
  });

  it.each([
    [true, "Retry or dismiss the earlier message first"],
    [false, "Dismiss the earlier message first"],
  ] as const)("blocks the head when the earlier message offersRetry=%s", (offersRetry, reason) => {
    for (const phase of ["running", "ready"] as const) {
      expect(
        deriveQueuedCardStatus({
          ...base,
          phase,
          waitingBehind: { offersRetry },
          hasPendingApproval: true,
          hasPendingUserInput: true,
        }),
      ).toMatchObject({
        label: "Waiting for an earlier message",
        canSteer: false,
        canSendNow: false,
        steerDisabledReason: reason,
        sendNowDisabledReason: reason,
        canCancel: true,
        primaryAction: phase === "running" ? "steer" : "send-now",
      });
    }
  });

  it.each(["pending", "sending"] as const)("keeps %s steering ahead of the blocker", (state) => {
    expect(
      deriveQueuedCardStatus({
        ...base,
        delivery: { ...queued, state, mode: "steer" },
        waitingBehind: { offersRetry: false },
      }),
    ).toMatchObject({
      label: "Steering…",
      steerDisabledReason: "Steering…",
      sendNowDisabledReason: "Steering…",
      canSteer: false,
      canSendNow: false,
      canCancel: false,
    });
  });

  it("keeps later cards waiting behind their queue predecessors", () => {
    expect(
      deriveQueuedCardStatus({ ...base, index: 1, waitingBehind: { offersRetry: false } }),
    ).toMatchObject({
      label: "Sends after the messages above.",
      steerDisabledReason: "Send the messages above first",
      sendNowDisabledReason: "Send the messages above first",
      canSteer: false,
      canSendNow: false,
      primaryAction: null,
    });
  });

  it("explains unsupported steering", () => {
    expect(deriveQueuedCardStatus({ ...base, supportsTurnSteer: false })).toMatchObject({
      label: "Sends when the turn ends.",
      canSteer: false,
      steerDisabledReason: "This provider cannot steer a running turn",
      primaryAction: null,
    });
  });

  it.each(["ready", "error", "stopped", "idle"] as const)(
    "offers Send now when the session is %s",
    (sessionStatus) => {
      expect(deriveQueuedCardStatus({ ...base, phase: "ready", sessionStatus })).toMatchObject({
        label: "Waiting for you",
        canSteer: false,
        canSendNow: true,
        canCancel: true,
        primaryAction: "send-now",
      });
    },
  );

  it("keeps a held head waiting for Send now even when another turn is running", () => {
    expect(deriveQueuedCardStatus({ ...base, delivery: { ...queued, held: true } })).toMatchObject({
      label: "Waiting for you",
      primaryAction: "send-now",
      canSteer: false,
      canSendNow: false,
      sendNowDisabledReason: "Wait for the running turn to end",
    });
  });

  it("keeps the tail behind the head", () => {
    expect(deriveQueuedCardStatus({ ...base, index: 1 })).toMatchObject({
      label: "Sends after the messages above.",
      canSteer: false,
      canSendNow: false,
      canCancel: true,
    });
  });

  it.each(["pending", "sending"] as const)("disables both actions during %s steer", (state) => {
    expect(
      deriveQueuedCardStatus({ ...base, delivery: { ...queued, state, mode: "steer" } }),
    ).toMatchObject({
      label: "Steering…",
      canSteer: false,
      canSendNow: false,
      canCancel: false,
      steering: true,
    });
  });

  it.each([
    ["hasPendingApproval", "Respond to the pending approval first"],
    ["hasPendingUserInput", "Answer the pending question first"],
  ] as const)("blocks steer and promote for %s", (key, reason) => {
    expect(deriveQueuedCardStatus({ ...base, [key]: true })).toMatchObject({
      canSteer: false,
      steerDisabledReason: reason,
    });
    expect(
      deriveQueuedCardStatus({ ...base, phase: "ready", sessionStatus: "ready", [key]: true }),
    ).toMatchObject({ canSendNow: false, sendNowDisabledReason: reason });
  });

  it.each(["disconnected", "connecting", "ready"] as const)(
    "enqueues behind an unsettled initial delivery while phase is %s",
    (phase) => {
      expect(shouldEnqueueOnSend({ phase, sessionStatus: null, hasPendingDelivery: true })).toBe(
        true,
      );
      expect(shouldEnqueueOnSend({ phase, sessionStatus: null, hasPendingDelivery: false })).toBe(
        false,
      );
    },
  );

  it.each([
    ["running", "running", true],
    ["connecting", "starting", true],
    ["ready", "ready", false],
    ["disconnected", null, false],
  ] as const)("enqueues for phase %s / session %s", (phase, sessionStatus, expected) => {
    expect(shouldEnqueueOnSend({ phase, sessionStatus, hasPendingDelivery: false })).toBe(expected);
  });
});

describe("describeUnavailableEnvironment", () => {
  for (const [phase, title] of [
    ["connecting", "devbox: Connecting…"],
    ["reconnecting", "devbox: Reconnecting…"],
  ] as const) {
    it(`keeps slow settings progress in the body while ${phase}`, () => {
      const notice = "Receiving settings from devbox over a slow connection…";
      expect(
        describeUnavailableEnvironment({
          label: "devbox",
          connection: { phase, error: null, traceId: null, notice },
        }),
      ).toEqual({ title, description: notice });
    });
  }

  it("keeps the title short and states the reason once, in the body", () => {
    const reason =
      "devbox rejected a new pairing credential. Connect again; if it keeps failing, remove the environment and add it again.";
    const copy = describeUnavailableEnvironment({
      label: "devbox",
      connection: { phase: "error", error: reason, traceId: null },
    });

    expect(copy).toEqual({ title: "devbox: Connection failed", description: reason });
    expect(`${copy.title} ${copy.description}`.split(reason)).toHaveLength(2);
  });

  it("keeps a retrying failure's reason out of the title", () => {
    const reason =
      "The remote host did not issue a pairing credential within 30 seconds. Check the connection; BiBCode keeps trying.";
    const copy = describeUnavailableEnvironment({
      label: "devbox",
      connection: { phase: "reconnecting", error: reason, traceId: null },
    });

    expect(copy).toEqual({ title: "devbox: Reconnecting…", description: reason });
  });

  it("explains what is blocked when there is no reason", () => {
    expect(
      describeUnavailableEnvironment({
        label: "Laptop",
        connection: { phase: "offline", error: null, traceId: null },
      }),
    ).toEqual({
      title: "Laptop: Offline",
      description: "Reconnect this environment before sending messages or running actions.",
    });
  });
});

describe("buildRevertCheckpointConfirmation", () => {
  it("warns that a full revert discards the newer messages", () => {
    expect(
      buildRevertCheckpointConfirmation({
        turnCount: 2,
        provider: { driver: ProviderDriverKind.make("codex"), supportsConversationRewind: true },
      }),
    ).toBe(
      "Revert this thread to checkpoint 2?\nThis will discard newer messages and turn diffs in this thread.\nThis action cannot be undone.",
    );
  });

  it("says a provider that cannot rewind keeps the newer messages", () => {
    expect(
      buildRevertCheckpointConfirmation({
        turnCount: 2,
        provider: {
          driver: ProviderDriverKind.make("claudeAgent"),
          displayName: "Work Claude",
          supportsConversationRewind: false,
        },
      }),
    ).toBe(
      "Restore files to checkpoint 2?\nWork Claude can't rewind its conversation, so the newer messages stay.\nThis action cannot be undone.",
    );
  });

  it("keeps the full-revert warning when the capability is unknown", () => {
    expect(buildRevertCheckpointConfirmation({ turnCount: 1, provider: null })).toContain(
      "Revert this thread to checkpoint 1?",
    );
    // An older server omits the flag even for Codex, which still discards the newer messages.
    expect(
      buildRevertCheckpointConfirmation({
        turnCount: 1,
        provider: { driver: ProviderDriverKind.make("codex") },
      }),
    ).toContain("This will discard newer messages");
  });
});
