import {
  scopedProjectKey,
  scopedThreadKey,
  scopeProjectRef,
  scopeThreadRef,
} from "@coddy/client-runtime/environment";
import * as Schema from "effect/Schema";
import {
  defaultInstanceIdForDriver,
  EnvironmentId,
  ProjectId,
  ProviderDriverKind,
  ProviderInstanceId,
  ThreadId,
  type ModelSelection,
  type PreviewAnnotationPayload,
  type ProviderOptionSelection,
  type RuntimeMode,
  type ServerProvider,
} from "@coddy/contracts";
import { DEFAULT_UNIFIED_SETTINGS } from "@coddy/contracts/settings";
import { createModelSelection } from "@coddy/shared/model";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

// The composer draft's `modelSelectionByProvider` and
// `stickyModelSelectionByProvider` maps are keyed by `ProviderInstanceId`
// in production; these aliases keep the legacy-key migration tests concise.
const CODEX_INSTANCE = ProviderInstanceId.make("codex");
const CODEX_SECONDARY_INSTANCE = ProviderInstanceId.make("codex_secondary");
const CLAUDE_AGENT_INSTANCE = ProviderInstanceId.make("claudeAgent");
const CURSOR_INSTANCE = ProviderInstanceId.make("cursor");
const CODEX_DRIVER = ProviderDriverKind.make("codex");
const CLAUDE_AGENT_DRIVER = ProviderDriverKind.make("claudeAgent");
const CURSOR_DRIVER = ProviderDriverKind.make("cursor");

type ProviderOptionSelectionBag = ReadonlyArray<ProviderOptionSelection>;
type ProviderOptionSelectionsByProvider = Partial<Record<string, ProviderOptionSelectionBag>>;

function toSelections(
  options: Record<string, string | boolean | undefined> | undefined,
): ReadonlyArray<ProviderOptionSelection> {
  const result: Array<ProviderOptionSelection> = [];
  if (!options) return result;
  for (const [id, value] of Object.entries(options)) {
    if (typeof value === "string" || typeof value === "boolean") {
      result.push({ id, value });
    }
  }
  return result;
}

function selectionsByProvider(
  options: Partial<Record<ProviderDriverKind, Record<string, string | boolean | undefined>>>,
): ProviderOptionSelectionsByProvider {
  const result: ProviderOptionSelectionsByProvider = {};
  for (const [provider, bag] of Object.entries(options) as Array<
    [ProviderDriverKind, Record<string, string | boolean | undefined>]
  >) {
    result[provider] = toSelections(bag);
  }
  return result;
}
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import {
  COMPOSER_DRAFT_STORAGE_KEY,
  clearComposerDraftsEnvironment,
  createEmptyThreadDraft,
  deriveEffectiveComposerModelState,
  finalizePromotedDraftThreadByRef,
  finalizePromotedDraftThreadsByRef,
  markPromotedDraftThread,
  markPromotedDraftThreadByRef,
  markPromotedDraftThreads,
  markPromotedDraftThreadsByRef,
  type ComposerAttachment,
  type ComposerImageAttachment,
  useComposerDraftStore,
  useComposerDraftModelState,
  useComposerThreadDraft,
  useEffectiveComposerModelState,
  DraftId,
  PersistedComposerAttachment,
} from "./composerDraftStore";
const isPersistedComposerAttachment = Schema.is(PersistedComposerAttachment);
import { derivePhysicalProjectKeyFromPath } from "./logicalProject";
import { type ReviewCommentContext } from "./reviewCommentContext";
import { removeLocalStorageItem, setLocalStorageItem } from "./hooks/useLocalStorage";
import {
  INLINE_TERMINAL_CONTEXT_PLACEHOLDER,
  insertInlineTerminalContextPlaceholder,
  type TerminalContextDraft,
} from "./lib/terminalContext";
import { createDebouncedStorage } from "./lib/storage";

function makeImage(input: {
  id: string;
  previewUrl?: string;
  name?: string;
  mimeType?: string;
  sizeBytes?: number;
  lastModified?: number;
}): ComposerImageAttachment {
  const name = input.name ?? "image.png";
  const mimeType = input.mimeType ?? "image/png";
  const sizeBytes = input.sizeBytes ?? 4;
  const lastModified = input.lastModified ?? 1_700_000_000_000;
  const file = new File([new Uint8Array(sizeBytes).fill(1)], name, {
    type: mimeType,
    lastModified,
  });
  return {
    type: "image",
    id: input.id,
    name,
    mimeType,
    sizeBytes: file.size,
    previewUrl: input.previewUrl ?? `blob:${input.id}`,
    file,
  };
}

function makeFile(input: {
  id: string;
  name?: string;
  mimeType?: string;
  sizeBytes?: number;
}): ComposerAttachment {
  const name = input.name ?? "notes.txt";
  const mimeType = input.mimeType ?? "text/plain";
  const file = new File([new Uint8Array(input.sizeBytes ?? 4).fill(1)], name, { type: mimeType });
  return {
    type: "file",
    id: input.id,
    name,
    mimeType,
    sizeBytes: file.size,
    file,
  };
}

function makeTerminalContext(input: {
  id: string;
  text?: string;
  terminalId?: string;
  terminalLabel?: string;
  lineStart?: number;
  lineEnd?: number;
}): TerminalContextDraft {
  return {
    id: input.id,
    threadId: ThreadId.make("thread-dedupe"),
    terminalId: input.terminalId ?? "default",
    terminalLabel: input.terminalLabel ?? "Terminal 1",
    lineStart: input.lineStart ?? 4,
    lineEnd: input.lineEnd ?? 5,
    text: input.text ?? "git status\nOn branch main",
    createdAt: "2026-03-13T12:00:00.000Z",
  };
}

function resetComposerDraftStore() {
  useComposerDraftStore.setState({
    draftsByThreadKey: {},
    draftThreadsByThreadKey: {},
    logicalProjectDraftThreadKeyByLogicalProjectKey: {},
    stickyModelSelectionByProvider: {},
    stickyActiveProvider: null,
  });
}

function modelSelection(
  provider: ProviderDriverKind,
  model: string,
  options?: Record<string, string | boolean | undefined>,
): ModelSelection {
  return createModelSelection(defaultInstanceIdForDriver(provider), model, toSelections(options));
}

function providerModelOptions(
  options: Partial<Record<string, Record<string, string | boolean | undefined>>>,
): ProviderOptionSelectionsByProvider {
  return selectionsByProvider(options);
}

const TEST_ENVIRONMENT_ID = EnvironmentId.make("environment-local");
const OTHER_TEST_ENVIRONMENT_ID = EnvironmentId.make("environment-remote");
const LEGACY_TEST_ENVIRONMENT_ID = EnvironmentId.make("__legacy__");

function threadKeyFor(
  threadId: ThreadId,
  environmentId: EnvironmentId = LEGACY_TEST_ENVIRONMENT_ID,
): string {
  if (environmentId === LEGACY_TEST_ENVIRONMENT_ID) {
    return threadId;
  }
  return scopedThreadKey(scopeThreadRef(environmentId, threadId));
}

function draftFor(threadId: ThreadId, environmentId: EnvironmentId = LEGACY_TEST_ENVIRONMENT_ID) {
  const store = useComposerDraftStore.getState().draftsByThreadKey;
  return store[threadKeyFor(threadId, environmentId)] ?? store[threadId] ?? undefined;
}

function draftByKey(key: string) {
  return useComposerDraftStore.getState().draftsByThreadKey[key] ?? undefined;
}

describe("composerDraftStore addAttachments", () => {
  it("never persists a staged upload id or ambiguous inline/staged byte sources", () => {
    const staged = {
      type: "file",
      id: "file-1",
      name: "a.txt",
      mimeType: "text/plain",
      sizeBytes: 3,
      uploadId: "stage-1",
    };
    expect(isPersistedComposerAttachment(staged)).toBe(false);
    expect(
      isPersistedComposerAttachment({ ...staged, dataUrl: "data:text/plain;base64,YWJj" }),
    ).toBe(false);
    expect(
      isPersistedComposerAttachment({
        type: "file",
        id: "file-1",
        name: "a.txt",
        mimeType: "text/plain",
        sizeBytes: 3,
        dataUrl: "data:text/plain;base64,YWJj",
      }),
    ).toBe(true);
  });
  const threadId = ThreadId.make("thread-dedupe");
  const threadRef = scopeThreadRef(TEST_ENVIRONMENT_ID, threadId);
  let originalRevokeObjectUrl: typeof URL.revokeObjectURL;
  let revokeSpy: ReturnType<typeof vi.fn<(url: string) => void>>;

  beforeEach(() => {
    resetComposerDraftStore();
    originalRevokeObjectUrl = URL.revokeObjectURL;
    revokeSpy = vi.fn();
    URL.revokeObjectURL = revokeSpy;
  });

  afterEach(() => {
    URL.revokeObjectURL = originalRevokeObjectUrl;
  });

  it("deduplicates identical images in one batch by file signature", () => {
    const first = makeImage({
      id: "img-1",
      previewUrl: "blob:first",
      name: "same.png",
      mimeType: "image/png",
      sizeBytes: 12,
      lastModified: 12345,
    });
    const duplicate = makeImage({
      id: "img-2",
      previewUrl: "blob:duplicate",
      name: "same.png",
      mimeType: "image/png",
      sizeBytes: 12,
      lastModified: 12345,
    });

    useComposerDraftStore.getState().addAttachments(threadRef, [first, duplicate]);

    const draft = draftFor(threadId, TEST_ENVIRONMENT_ID);
    expect(draft?.attachments.map((image) => image.id)).toEqual(["img-1"]);
    expect(revokeSpy).toHaveBeenCalledWith("blob:duplicate");
  });

  it("deduplicates against existing images across calls by file signature", () => {
    const first = makeImage({
      id: "img-a",
      previewUrl: "blob:a",
      name: "same.png",
      mimeType: "image/png",
      sizeBytes: 9,
      lastModified: 777,
    });
    const duplicateLater = makeImage({
      id: "img-b",
      previewUrl: "blob:b",
      name: "same.png",
      mimeType: "image/png",
      sizeBytes: 9,
      lastModified: 999,
    });

    useComposerDraftStore.getState().addAttachment(threadRef, first);
    useComposerDraftStore.getState().addAttachment(threadRef, duplicateLater);

    const draft = draftFor(threadId, TEST_ENVIRONMENT_ID);
    expect(draft?.attachments.map((image) => image.id)).toEqual(["img-a"]);
    expect(revokeSpy).toHaveBeenCalledWith("blob:b");
  });

  it("counts only deduplicated candidates when enforcing a live attachment cap", () => {
    const store = useComposerDraftStore.getState();
    const existing = Array.from({ length: 7 }, (_, index) =>
      makeImage({ id: `existing-${index}`, name: `existing-${index}.png` }),
    );
    store.addAttachments(threadRef, existing);

    const result = store.addAttachments(
      threadRef,
      [
        makeImage({ id: "duplicate", name: "existing-0.png" }),
        makeFile({ id: "new-file", name: "notes.txt" }),
      ],
      { maxAttachments: 8 },
    );

    expect(
      draftFor(threadId, TEST_ENVIRONMENT_ID)?.attachments.map((attachment) => attachment.id),
    ).toEqual([...existing.map((attachment) => attachment.id), "new-file"]);
    expect(result.rejectedCapacityAttachments).toEqual([]);
  });

  it("enforces the cap against the current store across successive handlers", () => {
    const store = useComposerDraftStore.getState();
    const existing = Array.from({ length: 7 }, (_, index) =>
      makeImage({ id: `existing-${index}`, name: `existing-${index}.png` }),
    );
    store.addAttachments(threadRef, existing);

    store.addAttachments(threadRef, [makeFile({ id: "eighth", name: "eighth.txt" })], {
      maxAttachments: 8,
    });
    const result = store.addAttachments(threadRef, [makeFile({ id: "ninth", name: "ninth.txt" })], {
      maxAttachments: 8,
    });

    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)?.attachments).toHaveLength(8);
    expect(result.rejectedCapacityAttachments.map((attachment) => attachment.name)).toEqual([
      "ninth.txt",
    ]);
  });

  it("revokes a unique image preview URL rejected by a full attachment cap", () => {
    const store = useComposerDraftStore.getState();
    store.addAttachments(
      threadRef,
      Array.from({ length: 8 }, (_, index) =>
        makeImage({
          id: `existing-${index}`,
          name: `existing-${index}.png`,
          previewUrl: `blob:existing-${index}`,
        }),
      ),
    );

    const rejected = makeImage({
      id: "rejected",
      name: "rejected.png",
      previewUrl: "blob:rejected",
    });
    const result = store.addAttachments(threadRef, [rejected], { maxAttachments: 8 });

    expect(result.rejectedCapacityAttachments).toEqual([rejected]);
    expect(revokeSpy).toHaveBeenCalledTimes(1);
    expect(revokeSpy).toHaveBeenCalledWith("blob:rejected");
  });

  it("does not revoke blob URLs that are still used by an accepted duplicate image", () => {
    const first = makeImage({
      id: "img-shared",
      previewUrl: "blob:shared",
    });
    const duplicateSameUrl = makeImage({
      id: "img-shared",
      previewUrl: "blob:shared",
    });

    useComposerDraftStore.getState().addAttachments(threadRef, [first, duplicateSameUrl]);

    const draft = draftFor(threadId, TEST_ENVIRONMENT_ID);
    expect(draft?.attachments.map((image) => image.id)).toEqual(["img-shared"]);
    expect(revokeSpy).not.toHaveBeenCalledWith("blob:shared");
  });
});

describe("composerDraftStore clearComposerContent", () => {
  const threadId = ThreadId.make("thread-clear");
  const threadRef = scopeThreadRef(TEST_ENVIRONMENT_ID, threadId);
  let originalRevokeObjectUrl: typeof URL.revokeObjectURL;
  let revokeSpy: ReturnType<typeof vi.fn<(url: string) => void>>;

  beforeEach(() => {
    resetComposerDraftStore();
    originalRevokeObjectUrl = URL.revokeObjectURL;
    revokeSpy = vi.fn();
    URL.revokeObjectURL = revokeSpy;
  });

  afterEach(() => {
    URL.revokeObjectURL = originalRevokeObjectUrl;
  });

  it("does not revoke blob preview URLs when clearing composer content", () => {
    const first = makeImage({
      id: "img-optimistic",
      previewUrl: "blob:optimistic",
    });
    useComposerDraftStore.getState().addAttachment(threadRef, first);

    useComposerDraftStore.getState().clearComposerContent(threadRef);

    const draft = draftFor(threadId, TEST_ENVIRONMENT_ID);
    expect(draft).toBeUndefined();
    expect(revokeSpy).not.toHaveBeenCalledWith("blob:optimistic");
  });

  it("revokes blob preview URLs when discarding composer content", () => {
    const image = makeImage({
      id: "img-discarded",
      previewUrl: "blob:discarded",
    });
    useComposerDraftStore.getState().addAttachment(threadRef, image);

    useComposerDraftStore.getState().discardComposerContent(threadRef);

    const draft = draftFor(threadId, TEST_ENVIRONMENT_ID);
    expect(draft).toBeUndefined();
    expect(revokeSpy).toHaveBeenCalledWith("blob:discarded");
  });
});

describe("composerDraftStore syncPersistedAttachments", () => {
  const threadId = ThreadId.make("thread-sync-persisted");
  const threadRef = scopeThreadRef(TEST_ENVIRONMENT_ID, threadId);

  beforeEach(() => {
    removeLocalStorageItem(COMPOSER_DRAFT_STORAGE_KEY);
    useComposerDraftStore.setState({
      draftsByThreadKey: {},
      draftThreadsByThreadKey: {},
      logicalProjectDraftThreadKeyByLogicalProjectKey: {},
      stickyModelSelectionByProvider: {},
      stickyActiveProvider: null,
    });
  });

  afterEach(() => {
    removeLocalStorageItem(COMPOSER_DRAFT_STORAGE_KEY);
  });

  it("treats malformed persisted draft storage as empty", async () => {
    const image = makeImage({
      id: "img-persisted",
      previewUrl: "blob:persisted",
    });
    useComposerDraftStore.getState().addAttachment(threadRef, image);
    setLocalStorageItem(
      COMPOSER_DRAFT_STORAGE_KEY,
      {
        version: 2,
        state: {
          draftsByThreadId: {
            [threadId]: {
              attachments: "not-an-array",
            },
          },
        },
      },
      Schema.Unknown,
    );

    useComposerDraftStore.getState().syncPersistedAttachments(threadRef, [
      {
        type: "image",
        id: image.id,
        name: image.name,
        mimeType: image.mimeType,
        sizeBytes: image.sizeBytes,
        dataUrl: image.previewUrl,
      },
    ]);
    await Promise.resolve();

    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)?.persistedAttachments).toEqual([]);
    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)?.nonPersistedAttachmentIds).toEqual([image.id]);
  });
});

describe("composerDraftStore terminal contexts", () => {
  const threadId = ThreadId.make("thread-dedupe");
  const threadRef = scopeThreadRef(TEST_ENVIRONMENT_ID, threadId);

  beforeEach(() => {
    useComposerDraftStore.setState({
      draftsByThreadKey: {},
      draftThreadsByThreadKey: {},
      logicalProjectDraftThreadKeyByLogicalProjectKey: {},
      stickyModelSelectionByProvider: {},
      stickyActiveProvider: null,
    });
  });

  it("deduplicates identical terminal contexts by selection signature", () => {
    const first = makeTerminalContext({ id: "ctx-1" });
    const duplicate = makeTerminalContext({ id: "ctx-2" });

    useComposerDraftStore.getState().addTerminalContexts(threadRef, [first, duplicate]);

    const draft = draftFor(threadId, TEST_ENVIRONMENT_ID);
    expect(draft?.terminalContexts.map((context) => context.id)).toEqual(["ctx-1"]);
  });

  it("clears terminal contexts when clearing composer content", () => {
    useComposerDraftStore
      .getState()
      .addTerminalContext(threadRef, makeTerminalContext({ id: "ctx-1" }));

    useComposerDraftStore.getState().clearComposerContent(threadRef);

    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)).toBeUndefined();
  });

  it("inserts terminal contexts at the requested inline prompt position", () => {
    const firstInsertion = insertInlineTerminalContextPlaceholder("alpha beta", 6);
    const secondInsertion = insertInlineTerminalContextPlaceholder(firstInsertion.prompt, 0);

    expect(
      useComposerDraftStore
        .getState()
        .insertTerminalContext(
          threadRef,
          firstInsertion.prompt,
          makeTerminalContext({ id: "ctx-1" }),
          firstInsertion.contextIndex,
        ),
    ).toBe(true);
    expect(
      useComposerDraftStore.getState().insertTerminalContext(
        threadRef,
        secondInsertion.prompt,
        makeTerminalContext({
          id: "ctx-2",
          terminalLabel: "Terminal 2",
          lineStart: 9,
          lineEnd: 10,
        }),
        secondInsertion.contextIndex,
      ),
    ).toBe(true);

    const draft = draftFor(threadId, TEST_ENVIRONMENT_ID);
    expect(draft?.prompt).toBe(
      `${INLINE_TERMINAL_CONTEXT_PLACEHOLDER} alpha ${INLINE_TERMINAL_CONTEXT_PLACEHOLDER} beta`,
    );
    expect(draft?.terminalContexts.map((context) => context.id)).toEqual(["ctx-2", "ctx-1"]);
  });

  it("omits terminal context text from persisted drafts", () => {
    useComposerDraftStore
      .getState()
      .addTerminalContext(threadRef, makeTerminalContext({ id: "ctx-persist" }));

    const persistApi = useComposerDraftStore.persist as unknown as {
      getOptions: () => {
        partialize: (state: ReturnType<typeof useComposerDraftStore.getState>) => unknown;
      };
    };
    const persistedState = persistApi.getOptions().partialize(useComposerDraftStore.getState()) as {
      draftsByThreadKey?: Record<string, { terminalContexts?: Array<Record<string, unknown>> }>;
    };

    expect(
      persistedState.draftsByThreadKey?.[threadKeyFor(threadId, TEST_ENVIRONMENT_ID)]
        ?.terminalContexts?.[0],
      "Expected terminal context metadata to be persisted.",
    ).toMatchObject({
      id: "ctx-persist",
      terminalId: "default",
      terminalLabel: "Terminal 1",
      lineStart: 4,
      lineEnd: 5,
    });
    expect(
      persistedState.draftsByThreadKey?.[threadKeyFor(threadId, TEST_ENVIRONMENT_ID)]
        ?.terminalContexts?.[0]?.text,
    ).toBeUndefined();
  });

  it("hydrates persisted terminal contexts without in-memory snapshot text", () => {
    const persistApi = useComposerDraftStore.persist as unknown as {
      getOptions: () => {
        merge: (
          persistedState: unknown,
          currentState: ReturnType<typeof useComposerDraftStore.getState>,
        ) => ReturnType<typeof useComposerDraftStore.getState>;
      };
    };
    const mergedState = persistApi.getOptions().merge(
      {
        draftsByThreadId: {
          [threadId]: {
            prompt: INLINE_TERMINAL_CONTEXT_PLACEHOLDER,
            attachments: [],
            terminalContexts: [
              {
                id: "ctx-rehydrated",
                threadId,
                createdAt: "2026-03-13T12:00:00.000Z",
                terminalId: "default",
                terminalLabel: "Terminal 1",
                lineStart: 4,
                lineEnd: 5,
              },
            ],
          },
        },
        draftThreadsByThreadId: {},
        projectDraftThreadIdByProjectKey: {},
      },
      useComposerDraftStore.getInitialState(),
    );

    expect(mergedState.draftsByThreadKey[threadKeyFor(threadId)]?.terminalContexts).toMatchObject([
      {
        id: "ctx-rehydrated",
        terminalId: "default",
        terminalLabel: "Terminal 1",
        lineStart: 4,
        lineEnd: 5,
        text: "",
      },
    ]);
  });

  it("sanitizes malformed persisted drafts during merge", () => {
    const persistApi = useComposerDraftStore.persist as unknown as {
      getOptions: () => {
        merge: (
          persistedState: unknown,
          currentState: ReturnType<typeof useComposerDraftStore.getState>,
        ) => ReturnType<typeof useComposerDraftStore.getState>;
      };
    };
    const mergedState = persistApi.getOptions().merge(
      {
        draftsByThreadId: {
          [threadId]: {
            prompt: "",
            attachments: "not-an-array",
            terminalContexts: "not-an-array",
            provider: "bogus-provider",
            modelOptions: "not-an-object",
          },
        },
        draftThreadsByThreadId: "not-an-object",
        projectDraftThreadIdByProjectKey: "not-an-object",
      },
      useComposerDraftStore.getInitialState(),
    );

    expect(mergedState.draftsByThreadKey[threadKeyFor(threadId)]).toBeUndefined();
    expect(mergedState.draftThreadsByThreadKey).toEqual({});
    expect(mergedState.logicalProjectDraftThreadKeyByLogicalProjectKey).toEqual({});
  });
});

describe("composerDraftStore element contexts", () => {
  const threadId = ThreadId.make("thread-element");
  const threadRef = scopeThreadRef(TEST_ENVIRONMENT_ID, threadId);
  const baseSelection = {
    pageUrl: "https://example.com/dashboard",
    pageTitle: "Dashboard",
    tagName: "button",
    selector: "button.submit",
    htmlPreview: "<button>Save</button>",
    componentName: "SubmitButton",
    source: {
      functionName: "SubmitButton",
      fileName: "/repo/Button.tsx",
      lineNumber: 12,
      columnNumber: 5,
    },
    styles: ".submit { color: white; }",
  } as const;

  beforeEach(() => {
    resetComposerDraftStore();
  });

  it("adds an element context and stamps id + threadId + pickedAt", () => {
    const accepted = useComposerDraftStore.getState().addElementContext(threadRef, baseSelection);
    expect(accepted).toBe(true);
    const draft = draftFor(threadId, TEST_ENVIRONMENT_ID);
    expect(draft?.elementContexts).toHaveLength(1);
    const entry = draft!.elementContexts[0]!;
    expect(entry.id.startsWith("el_")).toBe(true);
    expect(entry.threadId).toBe(threadId);
    expect(entry.pickedAt.length).toBeGreaterThan(0);
    expect(entry.componentName).toBe("SubmitButton");
  });

  it("dedupes by selector + tag + componentName + pageUrl signature", () => {
    const store = useComposerDraftStore.getState();
    expect(store.addElementContext(threadRef, baseSelection)).toBe(true);
    const second = store.addElementContext(threadRef, {
      ...baseSelection,
      htmlPreview: "<button>Save 2</button>",
    });
    expect(second).toBe(false);
    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)?.elementContexts).toHaveLength(1);
  });

  it("removeElementContext drops by id + leaves siblings intact", () => {
    const store = useComposerDraftStore.getState();
    store.addElementContext(threadRef, baseSelection);
    store.addElementContext(threadRef, { ...baseSelection, selector: "button.cancel" });
    const ids = draftFor(threadId, TEST_ENVIRONMENT_ID)!.elementContexts.map((c) => c.id);
    store.removeElementContext(threadRef, ids[0]!);
    const remaining = draftFor(threadId, TEST_ENVIRONMENT_ID)?.elementContexts;
    expect(remaining?.map((c) => c.id)).toEqual([ids[1]]);
  });

  it("setElementContexts replaces the slice and clearComposerContent wipes it", () => {
    const store = useComposerDraftStore.getState();
    store.addElementContext(threadRef, baseSelection);
    store.setElementContexts(threadRef, []);
    // Fully empty draft should be removed via shouldRemoveDraft.
    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)).toBeUndefined();

    store.addElementContext(threadRef, baseSelection);
    store.clearComposerContent(threadRef);
    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)).toBeUndefined();
  });

  it("persists element contexts via the partializer (round-trippable)", () => {
    useComposerDraftStore.getState().addElementContext(threadRef, baseSelection);
    const persistApi = useComposerDraftStore.persist as unknown as {
      getOptions: () => {
        partialize: (state: ReturnType<typeof useComposerDraftStore.getState>) => unknown;
      };
    };
    const persisted = persistApi.getOptions().partialize(useComposerDraftStore.getState()) as {
      draftsByThreadKey?: Record<string, { elementContexts?: Array<Record<string, unknown>> }>;
    };
    const entry =
      persisted.draftsByThreadKey?.[threadKeyFor(threadId, TEST_ENVIRONMENT_ID)]
        ?.elementContexts?.[0];
    expect(entry).toMatchObject({
      pageUrl: baseSelection.pageUrl,
      tagName: baseSelection.tagName,
      selector: baseSelection.selector,
      componentName: baseSelection.componentName,
    });
    // Persistence does NOT include htmlPreview / styles oversize-clamping —
    // that happens at normalization time, before the value reaches the store.
    expect(typeof entry?.htmlPreview).toBe("string");
  });
});

describe("composerDraftStore review comments", () => {
  const threadId = ThreadId.make("thread-review-comment");
  const threadRef = scopeThreadRef(TEST_ENVIRONMENT_ID, threadId);
  const comment = {
    id: "comment-1",
    sectionId: "file:src/app.ts",
    sectionTitle: "File comment",
    filePath: "src/app.ts",
    startIndex: 1,
    endIndex: 2,
    rangeLabel: "L2 to L3",
    text: "Keep this configurable.",
    diff: "@@ -2,2 +2,2 @@\n two\n three",
  } as const;

  beforeEach(() => {
    resetComposerDraftStore();
  });

  it("upserts and removes review comments by id", () => {
    const store = useComposerDraftStore.getState();
    store.addReviewComment(threadRef, comment);
    store.addReviewComment(threadRef, { ...comment, text: "Updated comment." });

    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)?.reviewComments).toEqual([
      { ...comment, text: "Updated comment." },
    ]);

    store.removeReviewComment(threadRef, comment.id);
    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)).toBeUndefined();
  });

  it("persists review comments and clears them with composer content", () => {
    const store = useComposerDraftStore.getState();
    store.addReviewComment(threadRef, comment);
    const persistApi = useComposerDraftStore.persist as unknown as {
      getOptions: () => {
        partialize: (state: ReturnType<typeof useComposerDraftStore.getState>) => unknown;
      };
    };
    const persisted = persistApi.getOptions().partialize(useComposerDraftStore.getState()) as {
      draftsByThreadKey?: Record<string, { reviewComments?: Array<Record<string, unknown>> }>;
    };

    expect(
      persisted.draftsByThreadKey?.[threadKeyFor(threadId, TEST_ENVIRONMENT_ID)]
        ?.reviewComments?.[0],
    ).toMatchObject(comment);

    store.clearComposerContent(threadRef);
    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)).toBeUndefined();
  });

  it("stores review comments against a new-thread draft id", () => {
    const draftId = DraftId.make("draft-review-comment");
    useComposerDraftStore.getState().addReviewComment(draftId, comment);

    expect(useComposerDraftStore.getState().getComposerDraft(draftId)?.reviewComments).toEqual([
      comment,
    ]);
  });
});

describe("composerDraftStore project draft thread mapping", () => {
  const projectId = ProjectId.make("project-a");
  const otherProjectId = ProjectId.make("project-b");
  const projectRef = scopeProjectRef(TEST_ENVIRONMENT_ID, projectId);
  const otherProjectRef = scopeProjectRef(TEST_ENVIRONMENT_ID, otherProjectId);
  const remoteProjectRef = scopeProjectRef(OTHER_TEST_ENVIRONMENT_ID, projectId);
  const threadId = ThreadId.make("thread-a");
  const otherThreadId = ThreadId.make("thread-b");
  const draftId = DraftId.make("draft-a");
  const otherDraftId = DraftId.make("draft-b");
  const sharedDraftId = DraftId.make("draft-shared");
  const localDraftId = DraftId.make("draft-local");
  const remoteDraftId = DraftId.make("draft-remote");

  beforeEach(() => {
    resetComposerDraftStore();
  });

  it("clears composer data for one environment without touching another", () => {
    const store = useComposerDraftStore.getState();
    const localThreadRef = scopeThreadRef(TEST_ENVIRONMENT_ID, threadId);
    const remoteThreadRef = scopeThreadRef(OTHER_TEST_ENVIRONMENT_ID, otherThreadId);
    const originalRevokeObjectUrl = URL.revokeObjectURL;
    const revokeSpy = vi.fn<(url: string) => void>();
    URL.revokeObjectURL = revokeSpy;

    try {
      store.setProjectDraftThreadId(projectRef, localDraftId, { threadId });
      store.setProjectDraftThreadId(remoteProjectRef, remoteDraftId, {
        threadId: otherThreadId,
      });
      store.setPrompt(localDraftId, "local draft");
      store.setPrompt(remoteDraftId, "remote draft");
      store.addAttachment(
        localDraftId,
        makeImage({ id: "img-local", previewUrl: "blob:local-draft" }),
      );
      store.setPrompt(localThreadRef, "local thread draft");
      store.setPrompt(remoteThreadRef, "remote thread draft");

      clearComposerDraftsEnvironment(TEST_ENVIRONMENT_ID);

      const next = useComposerDraftStore.getState();
      expect(next.getDraftThreadByProjectRef(projectRef)).toBeNull();
      expect(next.getDraftThreadByProjectRef(remoteProjectRef)).not.toBeNull();
      expect(next.getComposerDraft(localDraftId)).toBeNull();
      expect(next.getComposerDraft(remoteDraftId)?.prompt).toBe("remote thread draft");
      expect(next.getComposerDraft(localThreadRef)).toBeNull();
      expect(next.getComposerDraft(remoteThreadRef)?.prompt).toBe("remote thread draft");
      expect(revokeSpy).toHaveBeenCalledWith("blob:local-draft");
    } finally {
      URL.revokeObjectURL = originalRevokeObjectUrl;
    }
  });

  it("stores and reads project draft thread ids via actions", () => {
    const store = useComposerDraftStore.getState();
    expect(store.getDraftThreadByProjectRef(projectRef)).toBeNull();
    expect(store.getDraftThread(draftId)).toBeNull();

    store.setProjectDraftThreadId(projectRef, draftId, {
      threadId,
      branch: "feature/test",
      worktreePath: "/tmp/worktree-test",
      createdAt: "2026-01-01T00:00:00.000Z",
    });
    expect(useComposerDraftStore.getState().getDraftThreadByProjectRef(projectRef)).toMatchObject({
      threadId,
      environmentId: TEST_ENVIRONMENT_ID,
      projectId,
      logicalProjectKey: scopedProjectKey(projectRef),
      branch: "feature/test",
      worktreePath: "/tmp/worktree-test",
      envMode: "worktree",
      runtimeMode: "full-access",
      interactionMode: "default",
      createdAt: "2026-01-01T00:00:00.000Z",
    });
    expect(useComposerDraftStore.getState().getDraftThread(draftId)).toMatchObject({
      environmentId: TEST_ENVIRONMENT_ID,
      projectId,
      logicalProjectKey: scopedProjectKey(projectRef),
      branch: "feature/test",
      worktreePath: "/tmp/worktree-test",
      envMode: "worktree",
      runtimeMode: "full-access",
      interactionMode: "default",
      createdAt: "2026-01-01T00:00:00.000Z",
    });
  });

  it("clears only matching project draft mapping entries", () => {
    const store = useComposerDraftStore.getState();
    store.setProjectDraftThreadId(projectRef, draftId, { threadId });
    store.setPrompt(draftId, "hello");

    store.clearProjectDraftThreadById(projectRef, otherDraftId);
    expect(useComposerDraftStore.getState().getDraftThreadByProjectRef(projectRef)?.threadId).toBe(
      threadId,
    );

    store.clearProjectDraftThreadById(projectRef, draftId);
    expect(useComposerDraftStore.getState().getDraftThreadByProjectRef(projectRef)).toBeNull();
    expect(useComposerDraftStore.getState().getDraftThread(draftId)).toBeNull();
    expect(draftByKey(draftId)).toBeUndefined();
  });

  it("clears project draft mapping by project id", () => {
    const store = useComposerDraftStore.getState();
    store.setProjectDraftThreadId(projectRef, draftId, { threadId });
    store.setPrompt(draftId, "hello");
    store.clearProjectDraftThreadId(projectRef);
    expect(useComposerDraftStore.getState().getDraftThreadByProjectRef(projectRef)).toBeNull();
    expect(useComposerDraftStore.getState().getDraftThread(draftId)).toBeNull();
    expect(draftByKey(draftId)).toBeUndefined();
  });

  it("revokes draft image blob URLs when clearing a project's draft thread", () => {
    const store = useComposerDraftStore.getState();
    const originalRevokeObjectUrl = URL.revokeObjectURL;
    const revokeSpy = vi.fn<(url: string) => void>();
    URL.revokeObjectURL = revokeSpy;

    try {
      store.setProjectDraftThreadId(projectRef, draftId, { threadId });
      store.addAttachment(
        draftId,
        makeImage({ id: "img-project-clear", previewUrl: "blob:clear" }),
      );

      store.clearProjectDraftThreadId(projectRef);

      expect(useComposerDraftStore.getState().getDraftThreadByProjectRef(projectRef)).toBeNull();
      expect(useComposerDraftStore.getState().getDraftThread(draftId)).toBeNull();
      expect(revokeSpy).toHaveBeenCalledWith("blob:clear");
    } finally {
      URL.revokeObjectURL = originalRevokeObjectUrl;
    }
  });

  it("revokes draft image blob URLs when clearing a matching project draft thread by id", () => {
    const store = useComposerDraftStore.getState();
    const originalRevokeObjectUrl = URL.revokeObjectURL;
    const revokeSpy = vi.fn<(url: string) => void>();
    URL.revokeObjectURL = revokeSpy;

    try {
      store.setProjectDraftThreadId(projectRef, draftId, { threadId });
      store.addAttachment(
        draftId,
        makeImage({ id: "img-project-clear-by-id", previewUrl: "blob:clear-by-id" }),
      );

      store.clearProjectDraftThreadById(projectRef, draftId);

      expect(useComposerDraftStore.getState().getDraftThreadByProjectRef(projectRef)).toBeNull();
      expect(useComposerDraftStore.getState().getDraftThread(draftId)).toBeNull();
      expect(revokeSpy).toHaveBeenCalledWith("blob:clear-by-id");
    } finally {
      URL.revokeObjectURL = originalRevokeObjectUrl;
    }
  });

  it("clears orphaned composer drafts when remapping a project to a new draft thread", () => {
    const store = useComposerDraftStore.getState();
    store.setProjectDraftThreadId(projectRef, draftId, { threadId });
    store.setPrompt(draftId, "orphan me");

    store.setProjectDraftThreadId(projectRef, otherDraftId, { threadId: otherThreadId });

    expect(useComposerDraftStore.getState().getDraftThreadByProjectRef(projectRef)?.threadId).toBe(
      otherThreadId,
    );
    expect(useComposerDraftStore.getState().getDraftThread(draftId)).toBeNull();
    expect(draftByKey(draftId)).toBeUndefined();
  });

  it("keeps composer drafts when the thread is still mapped by another project", () => {
    const store = useComposerDraftStore.getState();
    store.setProjectDraftThreadId(projectRef, draftId, { threadId });
    store.setProjectDraftThreadId(otherProjectRef, sharedDraftId, { threadId });
    store.setPrompt(sharedDraftId, "keep me");

    store.clearProjectDraftThreadId(projectRef);

    expect(useComposerDraftStore.getState().getDraftThreadByProjectRef(projectRef)).toBeNull();
    expect(
      useComposerDraftStore.getState().getDraftThreadByProjectRef(otherProjectRef)?.threadId,
    ).toBe(threadId);
    expect(draftByKey(sharedDraftId)?.prompt).toBe("keep me");
  });

  it("clears draft registration independently", () => {
    const store = useComposerDraftStore.getState();
    store.setProjectDraftThreadId(projectRef, draftId, { threadId });
    store.setPrompt(draftId, "remove me");
    store.clearDraftThread(draftId);
    expect(useComposerDraftStore.getState().getDraftThreadByProjectRef(projectRef)).toBeNull();
    expect(useComposerDraftStore.getState().getDraftThread(draftId)).toBeNull();
    expect(draftByKey(draftId)).toBeUndefined();
  });

  it("marks a promoted draft by thread id without deleting composer state", () => {
    const store = useComposerDraftStore.getState();
    store.setProjectDraftThreadId(projectRef, draftId, { threadId });
    store.setPrompt(draftId, "promote me");

    markPromotedDraftThread(threadId);

    expect(useComposerDraftStore.getState().getDraftThreadByProjectRef(projectRef)).toBeNull();
    expect(useComposerDraftStore.getState().getDraftThread(draftId)?.promotedTo).toEqual(
      scopeThreadRef(TEST_ENVIRONMENT_ID, threadId),
    );
    expect(draftByKey(draftId)?.prompt).toBe("promote me");
  });

  it("reads local draft composer state through a scoped thread ref", () => {
    const store = useComposerDraftStore.getState();
    const threadRef = scopeThreadRef(TEST_ENVIRONMENT_ID, threadId);

    store.setProjectDraftThreadId(projectRef, draftId, { threadId });
    store.setPrompt(draftId, "scoped access");

    expect(store.getComposerDraft(draftId)?.prompt).toBe("scoped access");
    expect(store.getComposerDraft(threadRef)?.prompt).toBe("scoped access");
  });

  it("does not clear composer drafts for existing server threads during promotion cleanup", () => {
    const store = useComposerDraftStore.getState();
    const threadRef = scopeThreadRef(TEST_ENVIRONMENT_ID, threadId);
    store.setPrompt(threadRef, "keep me");

    markPromotedDraftThread(threadId);

    expect(useComposerDraftStore.getState().getDraftThread(threadRef)).toBeNull();
    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)?.prompt).toBe("keep me");
  });

  it("marks promoted drafts from an iterable of server thread ids", () => {
    const store = useComposerDraftStore.getState();
    store.setProjectDraftThreadId(projectRef, draftId, { threadId });
    store.setPrompt(draftId, "promote me");
    store.setProjectDraftThreadId(otherProjectRef, otherDraftId, { threadId: otherThreadId });
    store.setPrompt(otherDraftId, "keep me");

    markPromotedDraftThreads([threadId]);

    expect(useComposerDraftStore.getState().getDraftThread(draftId)?.promotedTo).toEqual(
      scopeThreadRef(TEST_ENVIRONMENT_ID, threadId),
    );
    expect(draftByKey(draftId)?.prompt).toBe("promote me");
    expect(
      useComposerDraftStore.getState().getDraftThreadByProjectRef(otherProjectRef)?.threadId,
    ).toBe(otherThreadId);
    expect(draftByKey(otherDraftId)?.prompt).toBe("keep me");
  });

  it("marks every matching scoped draft when multiple environments share a thread id", () => {
    const store = useComposerDraftStore.getState();
    const localThreadRef = scopeThreadRef(TEST_ENVIRONMENT_ID, threadId);
    const remoteThreadRef = scopeThreadRef(OTHER_TEST_ENVIRONMENT_ID, threadId);

    store.setProjectDraftThreadId(projectRef, localDraftId, { threadId });
    store.setPrompt(localDraftId, "local draft");
    store.setProjectDraftThreadId(remoteProjectRef, remoteDraftId, { threadId });
    store.setPrompt(remoteDraftId, "remote draft");

    markPromotedDraftThread(threadId);

    expect(store.getDraftThreadByProjectRef(projectRef)).toBeNull();
    expect(store.getDraftThreadByProjectRef(remoteProjectRef)).toBeNull();
    expect(store.getDraftThreadByRef(localThreadRef)?.promotedTo).toEqual(localThreadRef);
    expect(store.getDraftThreadByRef(remoteThreadRef)?.promotedTo).toEqual(remoteThreadRef);
    expect(draftByKey(localDraftId)?.prompt).toBe("local draft");
    expect(draftByKey(remoteDraftId)?.prompt).toBe("remote draft");
  });

  it("only marks promoted drafts for the matching environment ref", () => {
    const store = useComposerDraftStore.getState();
    store.setProjectDraftThreadId(projectRef, draftId, { threadId });
    store.setPrompt(draftId, "promote me");

    markPromotedDraftThreadByRef(scopeThreadRef(OTHER_TEST_ENVIRONMENT_ID, threadId));

    expect(useComposerDraftStore.getState().getDraftThreadByProjectRef(projectRef)?.threadId).toBe(
      threadId,
    );
    expect(draftByKey(draftId)?.prompt).toBe("promote me");
  });

  it("only marks iterable promotion cleanup entries for the matching environment refs", () => {
    const store = useComposerDraftStore.getState();
    store.setProjectDraftThreadId(projectRef, draftId, { threadId });
    store.setPrompt(draftId, "promote me");

    markPromotedDraftThreadsByRef([scopeThreadRef(OTHER_TEST_ENVIRONMENT_ID, threadId)]);

    expect(useComposerDraftStore.getState().getDraftThreadByProjectRef(projectRef)?.threadId).toBe(
      threadId,
    );
    expect(draftByKey(draftId)?.prompt).toBe("promote me");
  });

  it("keeps existing server-thread composer drafts during iterable promotion cleanup", () => {
    const store = useComposerDraftStore.getState();
    const threadRef = scopeThreadRef(TEST_ENVIRONMENT_ID, threadId);
    store.setPrompt(threadRef, "keep me");

    markPromotedDraftThreads([threadId]);

    expect(useComposerDraftStore.getState().getDraftThread(threadRef)).toBeNull();
    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)?.prompt).toBe("keep me");
  });

  it("finalizes a promoted draft after the canonical thread route is active", () => {
    const store = useComposerDraftStore.getState();
    store.setProjectDraftThreadId(projectRef, draftId, { threadId });
    store.setPrompt(draftId, "promote me");
    markPromotedDraftThread(threadId);

    finalizePromotedDraftThreadByRef(scopeThreadRef(TEST_ENVIRONMENT_ID, threadId));

    expect(useComposerDraftStore.getState().getDraftThreadByProjectRef(projectRef)).toBeNull();
    expect(useComposerDraftStore.getState().getDraftThread(draftId)).toBeNull();
    expect(draftByKey(draftId)).toBeUndefined();
  });

  it("finalizes a matching materialized draft even when promotion was not pre-marked", () => {
    const store = useComposerDraftStore.getState();
    store.setProjectDraftThreadId(projectRef, draftId, { threadId });
    store.setPrompt(draftId, "promote me");

    finalizePromotedDraftThreadByRef(scopeThreadRef(TEST_ENVIRONMENT_ID, threadId));

    expect(useComposerDraftStore.getState().getDraftThreadByProjectRef(projectRef)).toBeNull();
    expect(useComposerDraftStore.getState().getDraftThread(draftId)).toBeNull();
    expect(draftByKey(draftId)).toBeUndefined();
  });

  it("updates branch context on an existing draft thread", () => {
    const store = useComposerDraftStore.getState();
    store.setProjectDraftThreadId(projectRef, draftId, {
      threadId,
      branch: "main",
      worktreePath: null,
    });
    store.setDraftThreadContext(draftId, {
      branch: "feature/next",
      worktreePath: "/tmp/feature-next",
    });
    expect(useComposerDraftStore.getState().getDraftThreadByProjectRef(projectRef)?.threadId).toBe(
      threadId,
    );
    expect(useComposerDraftStore.getState().getDraftThread(draftId)).toMatchObject({
      environmentId: TEST_ENVIRONMENT_ID,
      projectId,
      branch: "feature/next",
      worktreePath: "/tmp/feature-next",
      envMode: "worktree",
    });
  });

  it("stores the start-from-origin choice with the draft thread", () => {
    const store = useComposerDraftStore.getState();
    store.setProjectDraftThreadId(projectRef, draftId, {
      threadId,
      envMode: "worktree",
      startFromOrigin: true,
    });

    expect(useComposerDraftStore.getState().getDraftThread(draftId)?.startFromOrigin).toBe(true);

    store.setDraftThreadContext(draftId, { startFromOrigin: false });

    expect(useComposerDraftStore.getState().getDraftThread(draftId)?.startFromOrigin).toBe(false);
  });

  it("preserves existing branch and worktree when setProjectDraftThreadId receives undefined", () => {
    const store = useComposerDraftStore.getState();
    store.setProjectDraftThreadId(projectRef, draftId, {
      threadId,
      branch: "main",
      worktreePath: "/tmp/main-worktree",
    });
    const runtimeUndefinedOptions = {
      branch: undefined,
      worktreePath: undefined,
    } as unknown as {
      branch?: string | null;
      worktreePath?: string | null;
    };
    store.setProjectDraftThreadId(projectRef, draftId, runtimeUndefinedOptions);

    expect(useComposerDraftStore.getState().getDraftThread(draftId)).toMatchObject({
      environmentId: TEST_ENVIRONMENT_ID,
      projectId,
      branch: "main",
      worktreePath: "/tmp/main-worktree",
      envMode: "worktree",
    });
  });

  it("preserves worktree env mode without a worktree path", () => {
    const store = useComposerDraftStore.getState();
    store.setProjectDraftThreadId(projectRef, draftId, {
      threadId,
      branch: "feature/base",
      worktreePath: null,
      envMode: "worktree",
    });
    const runtimeUndefinedOptions = {
      branch: undefined,
      worktreePath: undefined,
      envMode: undefined,
    } as unknown as {
      branch?: string | null;
      worktreePath?: string | null;
      envMode?: "local" | "worktree";
    };
    store.setProjectDraftThreadId(projectRef, draftId, runtimeUndefinedOptions);

    expect(useComposerDraftStore.getState().getDraftThread(draftId)).toMatchObject({
      environmentId: TEST_ENVIRONMENT_ID,
      projectId,
      branch: "feature/base",
      worktreePath: null,
      envMode: "worktree",
    });
  });

  it("finds a draft stored under a project's earlier key through fallback keys", () => {
    const store = useComposerDraftStore.getState();
    const earlierKey = derivePhysicalProjectKeyFromPath(TEST_ENVIRONMENT_ID, "/work/repo");
    store.setLogicalProjectDraftThreadId(earlierKey, projectRef, draftId, { threadId });

    const current = useComposerDraftStore.getState();
    expect(current.getDraftSessionByLogicalProjectKey("github.com/acme/repo")).toBeNull();
    expect(
      current.getDraftSessionByLogicalProjectKey("github.com/acme/repo", [earlierKey])?.draftId,
    ).toBe(draftId);
  });

  it("prefers a draft under the current key over one under a fallback key", () => {
    const store = useComposerDraftStore.getState();
    const earlierKey = derivePhysicalProjectKeyFromPath(TEST_ENVIRONMENT_ID, "/work/repo");
    store.setLogicalProjectDraftThreadId(earlierKey, projectRef, draftId, { threadId });
    store.setLogicalProjectDraftThreadId("github.com/acme/repo", projectRef, otherDraftId, {
      threadId: otherThreadId,
    });

    expect(
      useComposerDraftStore
        .getState()
        .getDraftSessionByLogicalProjectKey("github.com/acme/repo", [earlierKey])?.draftId,
    ).toBe(otherDraftId);
  });

  it("ignores a fallback-key draft owned by a different project", () => {
    const store = useComposerDraftStore.getState();
    const canonicalKey = "github.com/acme/repo";
    store.setLogicalProjectDraftThreadId(canonicalKey, otherProjectRef, otherDraftId, {
      threadId: otherThreadId,
    });

    const current = useComposerDraftStore.getState();
    expect(
      current.getDraftSessionByLogicalProjectKey("separate-key", [canonicalKey], projectRef),
    ).toBeNull();
    expect(
      current.getDraftSessionByLogicalProjectKey("separate-key", [canonicalKey], otherProjectRef)
        ?.draftId,
    ).toBe(otherDraftId);
    // The current key is never ownership-filtered: group members share one draft.
    expect(current.getDraftSessionByLogicalProjectKey(canonicalKey, [], projectRef)?.draftId).toBe(
      otherDraftId,
    );
  });

  it("moves a draft to the current key so it lives under exactly one logical key", () => {
    const store = useComposerDraftStore.getState();
    const earlierKey = derivePhysicalProjectKeyFromPath(TEST_ENVIRONMENT_ID, "/work/repo");
    store.setLogicalProjectDraftThreadId(earlierKey, projectRef, draftId, { threadId });
    store.setLogicalProjectDraftThreadId("github.com/acme/repo", projectRef, draftId, {
      threadId,
    });

    const state = useComposerDraftStore.getState();
    expect(state.logicalProjectDraftThreadKeyByLogicalProjectKey).toEqual({
      "github.com/acme/repo": draftId,
    });
    expect(state.getDraftSessionByLogicalProjectKey(earlierKey)).toBeNull();
    expect(state.getDraftSessionByLogicalProjectKey("github.com/acme/repo")?.draftId).toBe(draftId);
  });

  it("clears branch and worktree context when remapping a draft to another environment", () => {
    const store = useComposerDraftStore.getState();
    store.setProjectDraftThreadId(projectRef, draftId, {
      threadId,
      branch: "feature/local-only",
      worktreePath: "/tmp/local-worktree",
      envMode: "worktree",
    });

    store.setLogicalProjectDraftThreadId(scopedProjectKey(projectRef), remoteProjectRef, draftId, {
      threadId,
    });

    expect(useComposerDraftStore.getState().getDraftThread(draftId)).toMatchObject({
      environmentId: OTHER_TEST_ENVIRONMENT_ID,
      projectId,
      branch: null,
      worktreePath: null,
      envMode: "local",
    });
  });

  it("clears branch and worktree context when changing a draft thread project ref", () => {
    const store = useComposerDraftStore.getState();
    store.setProjectDraftThreadId(projectRef, draftId, {
      threadId,
      branch: "feature/local-only",
      worktreePath: "/tmp/local-worktree",
      envMode: "worktree",
    });

    store.setDraftThreadContext(draftId, {
      projectRef: remoteProjectRef,
    });

    expect(useComposerDraftStore.getState().getDraftThread(draftId)).toMatchObject({
      environmentId: OTHER_TEST_ENVIRONMENT_ID,
      projectId,
      branch: null,
      worktreePath: null,
      envMode: "local",
    });
  });
});

describe("composerDraftStore modelSelection", () => {
  const threadId = ThreadId.make("thread-model-options");
  const threadRef = scopeThreadRef(TEST_ENVIRONMENT_ID, threadId);

  beforeEach(() => {
    resetComposerDraftStore();
  });

  it("stores a model selection in the draft", () => {
    const store = useComposerDraftStore.getState();
    store.setModelSelection(
      threadRef,
      modelSelection(CODEX_DRIVER, "gpt-5.3-codex", {
        reasoningEffort: "xhigh",
        fastMode: true,
      }),
    );

    expect(
      draftFor(threadId, TEST_ENVIRONMENT_ID)?.modelSelectionByProvider[CODEX_INSTANCE],
    ).toEqual(
      modelSelection(CODEX_DRIVER, "gpt-5.3-codex", {
        reasoningEffort: "xhigh",
        fastMode: true,
      }),
    );
  });

  it("keeps default-only model selections on the draft", () => {
    const store = useComposerDraftStore.getState();
    store.setModelSelection(threadRef, modelSelection(CODEX_DRIVER, "gpt-5.4"));

    expect(
      draftFor(threadId, TEST_ENVIRONMENT_ID)?.modelSelectionByProvider[CODEX_INSTANCE],
    ).toEqual(modelSelection(CODEX_DRIVER, "gpt-5.4"));
  });

  it("replaces only the targeted provider options on the current model selection", () => {
    const store = useComposerDraftStore.getState();

    store.setModelSelection(
      threadRef,
      modelSelection(CLAUDE_AGENT_DRIVER, "claude-opus-4-6", {
        effort: "max",
        fastMode: true,
      }),
    );
    store.setStickyModelSelection(
      modelSelection(CLAUDE_AGENT_DRIVER, "claude-opus-4-6", {
        effort: "max",
        fastMode: true,
      }),
    );

    store.setProviderModelOptions(
      threadRef,
      CLAUDE_AGENT_DRIVER,
      toSelections({ thinking: false }),
      {
        persistSticky: true,
      },
    );

    expect(
      draftFor(threadId, TEST_ENVIRONMENT_ID)?.modelSelectionByProvider[CLAUDE_AGENT_INSTANCE],
    ).toEqual(
      modelSelection(CLAUDE_AGENT_DRIVER, "claude-opus-4-6", {
        thinking: false,
      }),
    );
    expect(
      useComposerDraftStore.getState().stickyModelSelectionByProvider[CLAUDE_AGENT_INSTANCE],
    ).toEqual(
      modelSelection(CLAUDE_AGENT_DRIVER, "claude-opus-4-6", {
        thinking: false,
      }),
    );
  });

  it("keeps explicit default-state overrides on the selection", () => {
    const store = useComposerDraftStore.getState();

    store.setModelSelection(
      threadRef,
      modelSelection(CLAUDE_AGENT_DRIVER, "claude-opus-4-6", {
        effort: "max",
      }),
    );

    store.setProviderModelOptions(threadRef, CLAUDE_AGENT_DRIVER, toSelections({ thinking: true }));

    expect(
      draftFor(threadId, TEST_ENVIRONMENT_ID)?.modelSelectionByProvider[CLAUDE_AGENT_INSTANCE],
    ).toEqual(
      modelSelection(CLAUDE_AGENT_DRIVER, "claude-opus-4-6", {
        thinking: true,
      }),
    );
    expect(useComposerDraftStore.getState().stickyModelSelectionByProvider).toEqual({});
  });

  it("keeps explicit off/default codex overrides on the selection", () => {
    const store = useComposerDraftStore.getState();

    store.setModelSelection(threadRef, modelSelection(CODEX_DRIVER, "gpt-5.4", { fastMode: true }));

    store.setProviderModelOptions(
      threadRef,
      CODEX_DRIVER,
      toSelections({ reasoningEffort: "high", fastMode: false }),
    );

    expect(
      draftFor(threadId, TEST_ENVIRONMENT_ID)?.modelSelectionByProvider[CODEX_INSTANCE],
    ).toEqual(
      modelSelection(CODEX_DRIVER, "gpt-5.4", {
        reasoningEffort: "high",
        fastMode: false,
      }),
    );
  });

  it("keeps explicit Cursor reset overrides on the selection", () => {
    const store = useComposerDraftStore.getState();

    store.setModelSelection(
      threadRef,
      modelSelection(CURSOR_DRIVER, "claude-opus-4-6", {
        reasoning: "xhigh",
        fastMode: true,
        thinking: false,
      }),
    );

    store.setProviderModelOptions(
      threadRef,
      CURSOR_DRIVER,
      toSelections({ reasoning: "medium", fastMode: false, thinking: true }),
    );

    expect(
      draftFor(threadId, TEST_ENVIRONMENT_ID)?.modelSelectionByProvider[CURSOR_INSTANCE],
    ).toEqual(
      modelSelection(CURSOR_DRIVER, "claude-opus-4-6", {
        reasoning: "medium",
        fastMode: false,
        thinking: true,
      }),
    );
  });

  it("preserves the selected Cursor model when only traits change", () => {
    const store = useComposerDraftStore.getState();

    store.setProviderModelOptions(threadRef, CURSOR_DRIVER, toSelections({ reasoning: "high" }), {
      model: "gpt-5.4",
      persistSticky: true,
    });

    expect(
      draftFor(threadId, TEST_ENVIRONMENT_ID)?.modelSelectionByProvider[CURSOR_INSTANCE],
    ).toEqual(
      modelSelection(CURSOR_DRIVER, "gpt-5.4", {
        reasoning: "high",
      }),
    );
    expect(
      useComposerDraftStore.getState().stickyModelSelectionByProvider[CURSOR_INSTANCE],
    ).toEqual(
      modelSelection(CURSOR_DRIVER, "gpt-5.4", {
        reasoning: "high",
      }),
    );
  });

  it("updates only the draft when sticky persistence is omitted", () => {
    const store = useComposerDraftStore.getState();

    store.setStickyModelSelection(
      modelSelection(CLAUDE_AGENT_DRIVER, "claude-opus-4-6", { effort: "max" }),
    );
    store.setModelSelection(
      threadRef,
      modelSelection(CLAUDE_AGENT_DRIVER, "claude-opus-4-6", { effort: "max" }),
    );

    store.setProviderModelOptions(
      threadRef,
      CLAUDE_AGENT_DRIVER,
      toSelections({ thinking: false }),
    );

    expect(
      draftFor(threadId, TEST_ENVIRONMENT_ID)?.modelSelectionByProvider[CLAUDE_AGENT_INSTANCE],
    ).toEqual(
      modelSelection(CLAUDE_AGENT_DRIVER, "claude-opus-4-6", {
        thinking: false,
      }),
    );
    expect(
      useComposerDraftStore.getState().stickyModelSelectionByProvider[CLAUDE_AGENT_INSTANCE],
    ).toEqual(modelSelection(CLAUDE_AGENT_DRIVER, "claude-opus-4-6", { effort: "max" }));
  });

  it("does not clear other provider options when setting options for a single provider", () => {
    const store = useComposerDraftStore.getState();

    // Set options for both providers
    store.setModelOptions(
      threadRef,
      providerModelOptions({
        codex: { fastMode: true },
        claudeAgent: { effort: "max" },
      }),
    );

    // Now set options for only codex — claudeAgent should be untouched
    store.setModelOptions(threadRef, providerModelOptions({ codex: { reasoningEffort: "xhigh" } }));

    const draft = draftFor(threadId, TEST_ENVIRONMENT_ID);
    expect(draft?.modelSelectionByProvider[CODEX_INSTANCE]?.options).toEqual(
      createModelSelection(CODEX_INSTANCE, "gpt-5.4", toSelections({ reasoningEffort: "xhigh" }))
        .options,
    );
    expect(draft?.modelSelectionByProvider[CLAUDE_AGENT_INSTANCE]?.options).toEqual(
      createModelSelection(
        CLAUDE_AGENT_INSTANCE,
        "claude-opus-4-6",
        toSelections({ effort: "max" }),
      ).options,
    );
  });

  it("preserves other provider options when switching the active model selection", () => {
    const store = useComposerDraftStore.getState();

    store.setModelOptions(
      threadRef,
      providerModelOptions({
        codex: { fastMode: true },
        claudeAgent: { effort: "max" },
      }),
    );

    store.setModelSelection(threadRef, modelSelection(CLAUDE_AGENT_DRIVER, "claude-opus-4-6"));

    const draft = draftFor(threadId, TEST_ENVIRONMENT_ID);
    expect(draft?.modelSelectionByProvider[CLAUDE_AGENT_INSTANCE]).toEqual(
      modelSelection(CLAUDE_AGENT_DRIVER, "claude-opus-4-6", { effort: "max" }),
    );
    expect(draft?.modelSelectionByProvider[CODEX_INSTANCE]?.options).toEqual(
      createModelSelection(CODEX_INSTANCE, "gpt-5.4", toSelections({ fastMode: true })).options,
    );
    expect(draft?.activeProvider).toBe("claudeAgent");
  });

  it("creates the first sticky snapshot from provider option changes", () => {
    const store = useComposerDraftStore.getState();

    store.setModelSelection(threadRef, modelSelection(CODEX_DRIVER, "gpt-5.4"));

    store.setProviderModelOptions(threadRef, CODEX_DRIVER, toSelections({ fastMode: true }), {
      persistSticky: true,
    });

    expect(useComposerDraftStore.getState().stickyModelSelectionByProvider[CODEX_INSTANCE]).toEqual(
      modelSelection(CODEX_DRIVER, "gpt-5.4", {
        fastMode: true,
      }),
    );
  });

  it("stores provider option changes on a selected custom instance", () => {
    const store = useComposerDraftStore.getState();

    store.setProviderModelOptions(
      threadRef,
      CODEX_DRIVER,
      toSelections({ reasoningEffort: "low" }),
      {
        instanceId: CODEX_SECONDARY_INSTANCE,
        model: "gpt-5-codex",
        persistSticky: true,
      },
    );

    expect(
      draftFor(threadId, TEST_ENVIRONMENT_ID)?.modelSelectionByProvider[CODEX_SECONDARY_INSTANCE],
    ).toEqual(
      expect.objectContaining({
        instanceId: CODEX_SECONDARY_INSTANCE,
        options: [{ id: "reasoningEffort", value: "low" }],
      }),
    );
    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)?.activeProvider).toBe(CODEX_SECONDARY_INSTANCE);
    expect(useComposerDraftStore.getState().stickyActiveProvider).toBe(CODEX_SECONDARY_INSTANCE);
    expect(useComposerDraftStore.getState().stickyModelSelectionByProvider[CODEX_INSTANCE]).toBe(
      undefined,
    );
    expect(
      useComposerDraftStore.getState().stickyModelSelectionByProvider[CODEX_SECONDARY_INSTANCE],
    ).toEqual(
      expect.objectContaining({
        instanceId: CODEX_SECONDARY_INSTANCE,
        options: [{ id: "reasoningEffort", value: "low" }],
      }),
    );
  });

  it("updates only the draft when sticky persistence is disabled", () => {
    const store = useComposerDraftStore.getState();

    store.setStickyModelSelection(
      modelSelection(CLAUDE_AGENT_DRIVER, "claude-opus-4-6", { effort: "max" }),
    );
    store.setModelSelection(
      threadRef,
      modelSelection(CLAUDE_AGENT_DRIVER, "claude-opus-4-6", { effort: "max" }),
    );

    store.setProviderModelOptions(
      threadRef,
      CLAUDE_AGENT_DRIVER,
      toSelections({ thinking: false }),
      {
        persistSticky: false,
      },
    );

    expect(
      draftFor(threadId, TEST_ENVIRONMENT_ID)?.modelSelectionByProvider[CLAUDE_AGENT_INSTANCE],
    ).toEqual(
      modelSelection(CLAUDE_AGENT_DRIVER, "claude-opus-4-6", {
        thinking: false,
      }),
    );
    expect(
      useComposerDraftStore.getState().stickyModelSelectionByProvider[CLAUDE_AGENT_INSTANCE],
    ).toEqual(modelSelection(CLAUDE_AGENT_DRIVER, "claude-opus-4-6", { effort: "max" }));
  });
});

describe("composerDraftStore setModelSelection", () => {
  const threadId = ThreadId.make("thread-model");
  const threadRef = scopeThreadRef(TEST_ENVIRONMENT_ID, threadId);

  beforeEach(() => {
    resetComposerDraftStore();
  });

  it("keeps explicit model overrides instead of coercing to null", () => {
    const store = useComposerDraftStore.getState();

    store.setModelSelection(threadRef, modelSelection(CODEX_DRIVER, "gpt-5.3-codex"));

    expect(
      draftFor(threadId, TEST_ENVIRONMENT_ID)?.modelSelectionByProvider[CODEX_INSTANCE],
    ).toEqual(modelSelection(CODEX_DRIVER, "gpt-5.3-codex"));
  });
});

describe("composerDraftStore sticky composer settings", () => {
  beforeEach(() => {
    resetComposerDraftStore();
  });

  it("stores a sticky model selection", () => {
    const store = useComposerDraftStore.getState();

    store.setStickyModelSelection(
      modelSelection(CODEX_DRIVER, "gpt-5.3-codex", {
        reasoningEffort: "medium",
        fastMode: true,
      }),
    );

    expect(useComposerDraftStore.getState().stickyModelSelectionByProvider[CODEX_INSTANCE]).toEqual(
      modelSelection(CODEX_DRIVER, "gpt-5.3-codex", {
        reasoningEffort: "medium",
        fastMode: true,
      }),
    );
    expect(useComposerDraftStore.getState().stickyActiveProvider).toBe("codex");
  });

  it("normalizes empty sticky model options by dropping selection options", () => {
    const store = useComposerDraftStore.getState();

    store.setStickyModelSelection(modelSelection(CODEX_DRIVER, "gpt-5.4"));

    expect(useComposerDraftStore.getState().stickyModelSelectionByProvider[CODEX_INSTANCE]).toEqual(
      modelSelection(CODEX_DRIVER, "gpt-5.4"),
    );
    expect(useComposerDraftStore.getState().stickyActiveProvider).toBe("codex");
  });

  it("drops empty cursor model options when normalizing sticky state", () => {
    const store = useComposerDraftStore.getState();

    store.setStickyModelSelection(
      modelSelection(CURSOR_DRIVER, "gpt-5.4", {
        reasoning: undefined,
        fastMode: undefined,
        thinking: undefined,
        contextWindow: undefined,
      }),
    );

    expect(
      useComposerDraftStore.getState().stickyModelSelectionByProvider[CURSOR_INSTANCE],
    ).toEqual(modelSelection(CURSOR_DRIVER, "gpt-5.4"));
    expect(useComposerDraftStore.getState().stickyActiveProvider).toBe("cursor");
  });

  it("applies sticky activeProvider to new drafts", () => {
    const store = useComposerDraftStore.getState();
    const threadId = ThreadId.make("thread-sticky-active-provider");
    const threadRef = scopeThreadRef(TEST_ENVIRONMENT_ID, threadId);

    store.setStickyModelSelection(modelSelection(CLAUDE_AGENT_DRIVER, "claude-opus-4-6"));
    store.applyStickyState(threadRef);

    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)).toMatchObject({
      modelSelectionByProvider: {
        claudeAgent: modelSelection(CLAUDE_AGENT_DRIVER, "claude-opus-4-6"),
      },
      activeProvider: "claudeAgent",
    });
  });
});

describe("composerDraftStore provider-scoped option updates", () => {
  const threadId = ThreadId.make("thread-provider");
  const threadRef = scopeThreadRef(TEST_ENVIRONMENT_ID, threadId);

  beforeEach(() => {
    resetComposerDraftStore();
  });

  it("retains off-provider option memory without changing the active selection", () => {
    const store = useComposerDraftStore.getState();
    store.setModelSelection(
      threadRef,
      modelSelection(CODEX_DRIVER, "gpt-5.3-codex", {
        reasoningEffort: "medium",
      }),
    );
    store.setProviderModelOptions(threadRef, CLAUDE_AGENT_DRIVER, toSelections({ effort: "max" }));
    const draft = draftFor(threadId, TEST_ENVIRONMENT_ID);
    expect(draft?.modelSelectionByProvider[CODEX_INSTANCE]).toEqual(
      modelSelection(CODEX_DRIVER, "gpt-5.3-codex", { reasoningEffort: "medium" }),
    );
    expect(draft?.modelSelectionByProvider[CLAUDE_AGENT_INSTANCE]?.options).toEqual(
      createModelSelection(
        CLAUDE_AGENT_INSTANCE,
        "claude-opus-4-6",
        toSelections({ effort: "max" }),
      ).options,
    );
    expect(draft?.activeProvider).toBe("codex");
  });
});

describe("composerDraftStore runtime and interaction settings", () => {
  const threadId = ThreadId.make("thread-settings");
  const threadRef = scopeThreadRef(TEST_ENVIRONMENT_ID, threadId);

  beforeEach(() => {
    resetComposerDraftStore();
  });

  it("stores runtime mode overrides in the composer draft", () => {
    const store = useComposerDraftStore.getState();

    store.setRuntimeMode(threadRef, "approval-required");

    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)?.runtimeMode).toBe("approval-required");
  });

  it("stores interaction mode overrides in the composer draft", () => {
    const store = useComposerDraftStore.getState();

    store.setInteractionMode(threadRef, "plan");

    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)?.interactionMode).toBe("plan");
  });

  it("removes empty settings-only drafts when overrides are cleared", () => {
    const store = useComposerDraftStore.getState();

    store.setRuntimeMode(threadRef, "approval-required");
    store.setInteractionMode(threadRef, "plan");
    store.setRuntimeMode(threadRef, null);
    store.setInteractionMode(threadRef, null);

    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)).toBeUndefined();
  });
});

// ---------------------------------------------------------------------------
// createDebouncedStorage
// ---------------------------------------------------------------------------

function createMockStorage() {
  const store = new Map<string, string>();
  return {
    getItem: vi.fn((name: string) => store.get(name) ?? null),
    setItem: vi.fn((name: string, value: string) => {
      store.set(name, value);
    }),
    removeItem: vi.fn((name: string) => {
      store.delete(name);
    }),
  };
}

describe("createDebouncedStorage", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("delegates getItem immediately", () => {
    const base = createMockStorage();
    base.getItem.mockReturnValueOnce("value");
    const storage = createDebouncedStorage(base);

    expect(storage.getItem("key")).toBe("value");
    expect(base.getItem).toHaveBeenCalledWith("key");
  });

  it("does not write to base storage until the debounce fires", () => {
    const base = createMockStorage();
    const storage = createDebouncedStorage(base);

    storage.setItem("key", "v1");
    expect(base.setItem).not.toHaveBeenCalled();

    vi.advanceTimersByTime(299);
    expect(base.setItem).not.toHaveBeenCalled();

    vi.advanceTimersByTime(1);
    expect(base.setItem).toHaveBeenCalledWith("key", "v1");
  });

  it("only writes the last value when setItem is called rapidly", () => {
    const base = createMockStorage();
    const storage = createDebouncedStorage(base);

    storage.setItem("key", "v1");
    storage.setItem("key", "v2");
    storage.setItem("key", "v3");

    vi.advanceTimersByTime(300);
    expect(base.setItem).toHaveBeenCalledTimes(1);
    expect(base.setItem).toHaveBeenCalledWith("key", "v3");
  });

  it("removeItem cancels a pending setItem write", () => {
    const base = createMockStorage();
    const storage = createDebouncedStorage(base);

    storage.setItem("key", "v1");
    storage.removeItem("key");

    vi.advanceTimersByTime(300);
    expect(base.setItem).not.toHaveBeenCalled();
    expect(base.removeItem).toHaveBeenCalledWith("key");
  });

  it("flush writes the pending value immediately", () => {
    const base = createMockStorage();
    const storage = createDebouncedStorage(base);

    storage.setItem("key", "v1");
    expect(base.setItem).not.toHaveBeenCalled();

    storage.flush();
    expect(base.setItem).toHaveBeenCalledWith("key", "v1");

    // Timer should be cancelled; no duplicate write.
    vi.advanceTimersByTime(300);
    expect(base.setItem).toHaveBeenCalledTimes(1);
  });

  it("flush is a no-op when nothing is pending", () => {
    const base = createMockStorage();
    const storage = createDebouncedStorage(base);

    storage.flush();
    expect(base.setItem).not.toHaveBeenCalled();
  });

  it("flush after removeItem is a no-op", () => {
    const base = createMockStorage();
    const storage = createDebouncedStorage(base);

    storage.setItem("key", "v1");
    storage.removeItem("key");
    storage.flush();

    expect(base.setItem).not.toHaveBeenCalled();
  });

  it("setItem works normally after removeItem cancels a pending write", () => {
    const base = createMockStorage();
    const storage = createDebouncedStorage(base);

    storage.setItem("key", "v1");
    storage.removeItem("key");
    storage.setItem("key", "v2");

    vi.advanceTimersByTime(300);
    expect(base.setItem).toHaveBeenCalledTimes(1);
    expect(base.setItem).toHaveBeenCalledWith("key", "v2");
  });
});

// ---------------------------------------------------------------------------
// deriveEffectiveComposerModelState
// ---------------------------------------------------------------------------

function serverProvider(input: {
  instanceId: string;
  driver?: ProviderDriverKind;
  models?: ReadonlyArray<string>;
}): ServerProvider {
  return {
    instanceId: ProviderInstanceId.make(input.instanceId),
    driver: input.driver ?? CODEX_DRIVER,
    enabled: true,
    installed: true,
    version: null,
    status: "ready",
    auth: { status: "authenticated" },
    checkedAt: "2026-01-01T00:00:00.000Z",
    models: (input.models ?? []).map((slug) => ({
      slug,
      name: slug,
      isCustom: false,
      capabilities: {},
    })),
    slashCommands: [],
    skills: [],
    agents: [],
  };
}

describe("deriveEffectiveComposerModelState", () => {
  const providers = [
    serverProvider({ instanceId: "codex", models: ["gpt-test-a", "gpt-test-b"] }),
    serverProvider({ instanceId: "codex_personal", models: ["gpt-test-c"] }),
  ];

  it("falls back to the default server model without any saved selection", () => {
    const state = deriveEffectiveComposerModelState({
      draft: null,
      providers,
      selectedProvider: CODEX_DRIVER,
      threadModelSelection: null,
      projectModelSelection: null,
      settings: DEFAULT_UNIFIED_SETTINGS,
    });
    expect(state.selectedModel).toBe("gpt-test-a");
    expect(state.modelOptions).toBeNull();
  });

  it("reads the draft's saved selection for the active driver bucket", () => {
    const state = deriveEffectiveComposerModelState({
      draft: {
        activeProvider: CODEX_INSTANCE,
        modelSelectionByProvider: {
          [CODEX_INSTANCE]: createModelSelection(
            CODEX_INSTANCE,
            "gpt-test-b",
            toSelections({ fastMode: true }),
          ),
        },
      },
      providers,
      selectedProvider: CODEX_DRIVER,
      threadModelSelection: null,
      projectModelSelection: null,
      settings: DEFAULT_UNIFIED_SETTINGS,
    });
    expect(state.selectedModel).toBe("gpt-test-b");
    expect(state.modelOptions).toEqual({ codex: [{ id: "fastMode", value: true }] });
  });

  it("prefers the selected custom instance's saved selection", () => {
    const personal = ProviderInstanceId.make("codex_personal");
    const state = deriveEffectiveComposerModelState({
      draft: {
        activeProvider: personal,
        modelSelectionByProvider: {
          [personal]: createModelSelection(personal, "gpt-test-c"),
          [CODEX_INSTANCE]: createModelSelection(CODEX_INSTANCE, "gpt-test-b"),
        },
      },
      providers,
      selectedProvider: CODEX_DRIVER,
      selectedInstanceId: personal,
      threadModelSelection: null,
      projectModelSelection: null,
      settings: DEFAULT_UNIFIED_SETTINGS,
    });
    expect(state.selectedModel).toBe("gpt-test-c");
  });

  it("falls back to the driver bucket when the instance has no saved selection", () => {
    const personal = ProviderInstanceId.make("codex_personal");
    const state = deriveEffectiveComposerModelState({
      draft: {
        activeProvider: CODEX_INSTANCE,
        modelSelectionByProvider: {
          [CODEX_INSTANCE]: createModelSelection(CODEX_INSTANCE, "gpt-test-b"),
        },
      },
      providers,
      selectedProvider: CODEX_DRIVER,
      selectedInstanceId: personal,
      threadModelSelection: null,
      projectModelSelection: null,
      settings: DEFAULT_UNIFIED_SETTINGS,
    });
    expect(state.selectedModel).toBe("gpt-test-b");
  });

  it("uses the thread's model selection as the base model and option source", () => {
    const state = deriveEffectiveComposerModelState({
      draft: null,
      providers,
      selectedProvider: CODEX_DRIVER,
      threadModelSelection: createModelSelection(
        CODEX_INSTANCE,
        "gpt-test-b",
        toSelections({ reasoningEffort: "high" }),
      ),
      projectModelSelection: null,
      settings: DEFAULT_UNIFIED_SETTINGS,
    });
    expect(state.selectedModel).toBe("gpt-test-b");
    expect(state.modelOptions).toEqual({ codex: [{ id: "reasoningEffort", value: "high" }] });
  });

  it("drops empty option bags from the project selection", () => {
    const state = deriveEffectiveComposerModelState({
      draft: null,
      providers,
      selectedProvider: CODEX_DRIVER,
      threadModelSelection: null,
      projectModelSelection: createModelSelection(CODEX_INSTANCE, "gpt-test-b"),
      settings: DEFAULT_UNIFIED_SETTINGS,
    });
    expect(state.selectedModel).toBe("gpt-test-b");
    expect(state.modelOptions).toBeNull();
  });

  it("ignores draft map entries whose selections carry no options", () => {
    const state = deriveEffectiveComposerModelState({
      draft: {
        activeProvider: CODEX_INSTANCE,
        modelSelectionByProvider: {
          [CODEX_INSTANCE]: createModelSelection(CODEX_INSTANCE, "gpt-test-b"),
        },
      },
      providers,
      selectedProvider: CODEX_DRIVER,
      threadModelSelection: null,
      projectModelSelection: null,
      settings: DEFAULT_UNIFIED_SETTINGS,
    });
    expect(state.modelOptions).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Persist plumbing: migrate (legacy v2 storage) and merge (v3 hydration)
// ---------------------------------------------------------------------------

interface PersistOptionsForTest {
  migrate: (persistedState: unknown, version: number) => unknown;
  merge: (
    persistedState: unknown,
    currentState: ReturnType<typeof useComposerDraftStore.getState>,
  ) => ReturnType<typeof useComposerDraftStore.getState>;
  partialize: (state: ReturnType<typeof useComposerDraftStore.getState>) => unknown;
}

function getPersistOptions(): PersistOptionsForTest {
  return (
    useComposerDraftStore.persist as unknown as { getOptions: () => PersistOptionsForTest }
  ).getOptions();
}

interface MigratedStateForTest {
  draftsByThreadKey: Record<
    string,
    {
      prompt: string;
      attachments: ReadonlyArray<{ id: string }>;
      modelSelectionByProvider?: Record<
        string,
        {
          instanceId: string;
          model: string;
          options?: ReadonlyArray<{ id: string; value: string | boolean }>;
        }
      >;
      activeProvider?: string | null;
    }
  >;
  draftThreadsByThreadKey: Record<string, Record<string, unknown>>;
  logicalProjectDraftThreadKeyByLogicalProjectKey: Record<string, string>;
  stickyModelSelectionByProvider?: Record<
    string,
    { model: string; options?: ReadonlyArray<{ id: string; value: string | boolean }> }
  >;
  stickyActiveProvider?: string | null;
}

describe("composerDraftStore legacy storage migration", () => {
  const legacyEnvironmentId = EnvironmentId.make("environment-legacy");
  const legacyProjectId = ProjectId.make("project-legacy");
  const legacyProjectKey = scopedProjectKey(scopeProjectRef(legacyEnvironmentId, legacyProjectId));
  const rawThreadId = "thread-legacy-raw";

  it("returns the empty persisted state for malformed payloads", () => {
    const migrate = getPersistOptions().migrate;
    for (const payload of [null, undefined, "not-an-object", 42]) {
      expect(migrate(payload, 2)).toEqual({
        draftsByThreadKey: {},
        draftThreadsByThreadKey: {},
        logicalProjectDraftThreadKeyByLogicalProjectKey: {},
        stickyModelSelectionByProvider: {},
        stickyActiveProvider: null,
      });
    }
  });

  it("migrates a v2 payload with legacy codex fields and record-shaped options", () => {
    const migrated = getPersistOptions().migrate(
      {
        draftsByThreadId: {
          [rawThreadId]: {
            prompt: "legacy prompt",
            attachments: [
              {
                id: "att-1",
                name: "a.png",
                mimeType: "image/png",
                sizeBytes: 3,
                dataUrl: "data:image/png;base64,QUJD",
              },
              { id: "", name: "bad", mimeType: "image/png", sizeBytes: 1, dataUrl: "x" },
              "not-an-attachment",
            ],
            provider: "codex",
            model: "gpt-5.3-codex",
            effort: "high",
            codexFastMode: true,
            modelOptions: { claudeAgent: { effort: "max" }, cursor: {} },
          },
        },
        draftThreadsByThreadId: {
          [rawThreadId]: {
            threadId: rawThreadId,
            projectId: legacyProjectId,
            createdAt: "2026-01-01T00:00:00.000Z",
            branch: "main",
            worktreePath: "/tmp/legacy",
            promotedTo: {
              environmentId: legacyEnvironmentId,
              threadId: "thread-legacy-promoted",
            },
          },
        },
        projectDraftThreadIdByProjectKey: {
          [legacyProjectKey]: rawThreadId,
        },
        stickyProvider: "codex",
        stickyModel: "gpt-5.4",
        stickyModelOptions: { codex: { reasoningEffort: "high" } },
      },
      2,
    ) as MigratedStateForTest;

    const draft = migrated.draftsByThreadKey[rawThreadId];
    expect(draft?.prompt).toBe("legacy prompt");
    expect(draft?.attachments.map((attachment) => attachment.id)).toEqual(["att-1"]);
    expect(draft?.modelSelectionByProvider?.codex).toMatchObject({
      instanceId: "codex",
      model: "gpt-5.3-codex",
      options: [
        { id: "reasoningEffort", value: "high" },
        { id: "fastMode", value: true },
      ],
    });
    expect(draft?.modelSelectionByProvider?.claudeAgent?.options).toEqual([
      { id: "effort", value: "max" },
    ]);
    expect(draft?.activeProvider).toBe("codex");

    expect(migrated.draftThreadsByThreadKey[rawThreadId]).toMatchObject({
      threadId: rawThreadId,
      environmentId: legacyEnvironmentId,
      projectId: legacyProjectId,
      branch: "main",
      worktreePath: "/tmp/legacy",
      envMode: "worktree",
      startFromOrigin: false,
      promotedTo: {
        environmentId: legacyEnvironmentId,
        threadId: "thread-legacy-promoted",
      },
    });
    expect(migrated.logicalProjectDraftThreadKeyByLogicalProjectKey[legacyProjectKey]).toBe(
      rawThreadId,
    );

    expect(migrated.stickyModelSelectionByProvider?.codex).toMatchObject({
      model: "gpt-5.4",
      options: [{ id: "reasoningEffort", value: "high" }],
    });
    expect(migrated.stickyActiveProvider).toBe("codex");
  });

  it("normalizes malformed legacy option and context entries", () => {
    const migrated = getPersistOptions().migrate(
      {
        draftsByThreadId: {
          edge: {
            prompt: "edge",
            modelSelection: {
              provider: "codex",
              model: "gpt-5.4",
              options: [
                null,
                1,
                {},
                { id: "", value: "ignored" },
                { id: "invalid", value: 1 },
                { id: "reasoningEffort", value: "medium" },
                { id: "fastMode", value: false },
              ],
            },
            modelOptions: { codex: { valid: "yes", invalid: 1 }, cursor: null },
            terminalContexts: [
              null,
              {
                id: "bad-terminal",
                threadId: "edge",
                createdAt: "2026-01-01T00:00:00.000Z",
                terminalId: 1,
                terminalLabel: null,
                lineStart: 1,
                lineEnd: 2,
              },
              {
                id: "terminal",
                threadId: "edge",
                createdAt: "2026-01-01T00:00:00.000Z",
                terminalId: " terminal ",
                terminalLabel: " Terminal ",
                lineStart: 0.5,
                lineEnd: 0.1,
              },
            ],
            elementContexts: [
              null,
              {
                id: "element",
                threadId: "edge",
                pickedAt: "2026-01-01T00:00:00.000Z",
                pageUrl: "https://example.test",
                tagName: "DIV",
                source: {
                  functionName: 1,
                  fileName: false,
                  lineNumber: Number.NaN,
                  columnNumber: "2",
                },
              },
            ],
          },
          legacyExtras: {
            prompt: "legacy extras",
            provider: "codex",
            model: "gpt-5.4",
            effort: "high",
            serviceTier: "fast",
            modelOptions: {
              codex: { reasoningEffort: "medium", fastMode: true, invalid: 1 },
            },
          },
          custom: {
            prompt: "custom",
            modelSelection: {
              provider: "custom_instance",
              model: "custom-model",
              options: [{ id: "mode", value: true }],
            },
          },
          invalidModel: { prompt: "invalid model", provider: "codex", model: "   " },
        },
        draftThreadsByThreadId: {
          "": { projectId: "ignored", environmentId: "environment" },
          primitive: "ignored",
          edge: {
            threadId: "edge",
            environmentId: "environment-edge",
            projectId: "project-edge",
            logicalProjectKey: "logical-edge",
          },
          legacyExtras: {
            threadId: "legacyExtras",
            environmentId: "environment-edge",
            projectId: "project-edge",
          },
          custom: {
            threadId: "custom",
            environmentId: "environment-edge",
            projectId: "project-edge",
          },
          invalidModel: {
            threadId: "invalidModel",
            environmentId: "environment-edge",
            projectId: "project-edge",
          },
        },
        projectDraftThreadIdByProjectKey: {
          "not-a-scoped-project": "custom",
        },
      },
      2,
    ) as unknown as {
      draftsByThreadKey: Record<
        string,
        {
          modelSelectionByProvider: Record<
            string,
            { options: ReadonlyArray<ProviderOptionSelection> }
          >;
          terminalContexts: ReadonlyArray<TerminalContextDraft>;
          elementContexts: ReadonlyArray<{
            source: {
              functionName: string | null;
              fileName: string | null;
              lineNumber: number | null;
              columnNumber: number | null;
            };
          }>;
          activeProvider?: string;
        }
      >;
      draftThreadsByThreadKey: Record<string, { logicalProjectKey?: string }>;
    };

    expect(migrated.draftsByThreadKey.edge!.modelSelectionByProvider.codex!.options).toEqual([
      { id: "reasoningEffort", value: "medium" },
      { id: "fastMode", value: false },
    ]);
    expect(migrated.draftsByThreadKey.edge!.terminalContexts).toEqual([
      expect.objectContaining({ terminalId: "terminal", lineStart: 1, lineEnd: 1 }),
    ]);
    expect(migrated.draftsByThreadKey.edge!.elementContexts[0]!.source).toEqual({
      functionName: null,
      fileName: null,
      lineNumber: null,
      columnNumber: null,
    });
    expect(
      migrated.draftsByThreadKey.legacyExtras!.modelSelectionByProvider.codex!.options,
    ).toEqual([
      { id: "reasoningEffort", value: "medium" },
      { id: "fastMode", value: true },
    ]);
    expect(migrated.draftsByThreadKey.custom!.activeProvider).toBe("custom_instance");
    expect(migrated.draftsByThreadKey.invalidModel!.activeProvider).toBeUndefined();
    expect(migrated.draftThreadsByThreadKey.edge!.logicalProjectKey).toBe("logical-edge");
  });

  it("reconciles project mappings against missing and conflicting draft threads", () => {
    const mappedEnvironmentId = EnvironmentId.make("environment-mapped");
    const mappedProjectKey = scopedProjectKey(
      scopeProjectRef(mappedEnvironmentId, ProjectId.make("project-mapped")),
    );
    const mappedThreadKey = scopedThreadKey(
      scopeThreadRef(mappedEnvironmentId, ThreadId.make("thread-mapped")),
    );
    const conflictingProjectKey = scopedProjectKey(
      scopeProjectRef(EnvironmentId.make("environment-new"), ProjectId.make("project-new")),
    );

    const migrated = getPersistOptions().migrate(
      {
        draftThreadsByThreadId: {
          "thread-conflict": {
            threadId: "thread-conflict",
            environmentId: "environment-old",
            projectId: "project-old",
            createdAt: "2026-01-01T00:00:00.000Z",
            branch: null,
            worktreePath: null,
          },
          "thread-dropped": { branch: "no-project-or-environment" },
        },
        projectDraftThreadIdByProjectKey: {
          [mappedProjectKey]: mappedThreadKey,
          [conflictingProjectKey]: "thread-conflict",
          "not-a-project-key-loose": "thread-orphan",
          "ignored-empty": "",
        },
      },
      2,
    ) as MigratedStateForTest;

    // A mapping to an unknown scoped thread key materializes a default draft thread.
    expect(migrated.draftThreadsByThreadKey[mappedThreadKey]).toMatchObject({
      threadId: "thread-mapped",
      environmentId: mappedEnvironmentId,
      projectId: "project-mapped",
      logicalProjectKey: mappedProjectKey,
      envMode: "local",
      branch: null,
      worktreePath: null,
      promotedTo: null,
    });
    // A mapping that disagrees with the stored draft thread wins.
    expect(migrated.draftThreadsByThreadKey["thread-conflict"]).toMatchObject({
      environmentId: "environment-new",
      projectId: "project-new",
      logicalProjectKey: conflictingProjectKey,
    });
    // Entries without a project or environment identity are dropped.
    expect(migrated.draftThreadsByThreadKey["thread-dropped"]).toBeUndefined();
    expect(
      migrated.logicalProjectDraftThreadKeyByLogicalProjectKey["not-a-project-key-loose"],
    ).toBe("thread-orphan");
  });
});

describe("composerDraftStore v3 hydration via merge", () => {
  const hydrateThreadId = ThreadId.make("thread-hydrate");
  const hydrateThreadKey = scopedThreadKey(scopeThreadRef(TEST_ENVIRONMENT_ID, hydrateThreadId));

  it("hydrates persisted attachments back into usable files", () => {
    const merged = getPersistOptions().merge(
      {
        draftsByThreadKey: {
          [hydrateThreadKey]: {
            prompt: "hydrate me",
            attachments: [
              {
                id: "att-b64",
                name: "pixel.png",
                mimeType: "image/png",
                sizeBytes: 3,
                dataUrl: "data:image/png;base64,QUJD",
              },
              {
                type: "file",
                id: "att-text",
                name: "note.txt",
                mimeType: "text/plain",
                sizeBytes: 11,
                dataUrl: "data:text/plain,hello%20world",
              },
              {
                type: "file",
                id: "att-charset",
                name: "charset.txt",
                mimeType: "text/plain",
                sizeBytes: 8,
                dataUrl: "data:text/markdown;charset=utf-8,hi%20there",
              },
              {
                id: "att-empty",
                name: "empty.png",
                mimeType: "image/png",
                sizeBytes: 0,
                dataUrl: "data:image/png;base64,",
              },
              {
                id: "att-broken",
                name: "broken.png",
                mimeType: "image/png",
                sizeBytes: 4,
                dataUrl: "data:image/png;base64,%%%",
              },
            ],
          },
        },
        draftThreadsByThreadKey: {},
        logicalProjectDraftThreadKeyByLogicalProjectKey: {},
      },
      useComposerDraftStore.getInitialState(),
    );

    const draft = merged.draftsByThreadKey[hydrateThreadKey];
    expect(draft?.prompt).toBe("hydrate me");
    // Undecodable payloads survive as persisted attachments but produce no runtime attachment.
    expect(draft?.persistedAttachments.map((attachment) => attachment.id)).toEqual([
      "att-b64",
      "att-text",
      "att-charset",
      "att-empty",
      "att-broken",
    ]);
    expect(draft?.attachments.map((image) => image.id)).toEqual([
      "att-b64",
      "att-text",
      "att-charset",
    ]);
    expect(draft?.attachments[0]?.file.type).toBe("image/png");
    // Non-base64 data URLs infer the mime type from the header when present.
    expect(draft?.attachments[1]?.file.type).toBe("text/plain");
    expect(draft?.attachments[2]?.file.type).toBe("text/markdown");
  });

  it("round-trips files and hydrates legacy images without creating file previews", () => {
    const createObjectURL = vi.fn(() => "blob:unexpected");
    vi.stubGlobal("URL", { createObjectURL });
    const merged = getPersistOptions().merge(
      {
        draftsByThreadKey: {
          [hydrateThreadKey]: {
            prompt: "mixed",
            attachments: [
              {
                id: "legacy-image",
                name: "legacy.png",
                mimeType: "image/png",
                sizeBytes: 3,
                dataUrl: "data:image/png;base64,QUJD",
              },
              {
                type: "file",
                id: "notes-file",
                name: "notes.txt",
                mimeType: "text/plain",
                sizeBytes: 5,
                dataUrl: "data:text/plain;base64,aGVsbG8=",
              },
            ],
          },
        },
        draftThreadsByThreadKey: {},
        logicalProjectDraftThreadKeyByLogicalProjectKey: {},
      },
      useComposerDraftStore.getInitialState(),
    );

    const attachments = merged.draftsByThreadKey[hydrateThreadKey]?.attachments ?? [];
    expect(attachments.map((attachment) => attachment.type)).toEqual(["image", "file"]);
    expect(attachments[0]).toMatchObject({ previewUrl: "data:image/png;base64,QUJD" });
    expect(attachments[1]).not.toHaveProperty("previewUrl");
    expect(attachments[0]).not.toHaveProperty("dataUrl");
    expect(attachments[1]).not.toHaveProperty("dataUrl");
    expect(createObjectURL).not.toHaveBeenCalled();
  });

  it("drops malformed persisted attachments while preserving valid image and file records", () => {
    const image = {
      type: "image",
      id: "valid-image",
      name: "pixel.png",
      mimeType: "image/png",
      sizeBytes: 3,
      dataUrl: "data:image/png;base64,QUJD",
    };
    const file = {
      type: "file",
      id: "valid-file",
      name: "notes.txt",
      mimeType: "text/plain",
      sizeBytes: 5,
      dataUrl: "data:text/plain;base64,aGVsbG8=",
    };
    const merged = getPersistOptions().merge(
      {
        draftsByThreadKey: {
          [hydrateThreadKey]: {
            prompt: "validate",
            attachments: [
              image,
              file,
              { ...image, id: "unsafe id" },
              { ...file, name: " " },
              { ...image, mimeType: "text/plain" },
              { ...file, mimeType: "image/png" },
              { ...file, mimeType: `text/${"x".repeat(100)}` },
              { ...file, sizeBytes: 1.5 },
              { ...file, sizeBytes: 10 * 1024 * 1024 + 1 },
              { ...file, dataUrl: "x".repeat(14_000_001) },
            ],
          },
        },
        draftThreadsByThreadKey: {},
        logicalProjectDraftThreadKeyByLogicalProjectKey: {},
      },
      useComposerDraftStore.getInitialState(),
    );

    expect(merged.draftsByThreadKey[hydrateThreadKey]?.persistedAttachments).toHaveLength(2);
    expect(
      merged.draftsByThreadKey[hydrateThreadKey]?.attachments.map((attachment) => attachment.id),
    ).toEqual(["valid-image", "valid-file"]);
  });

  it("hydrates element contexts, review comments, model state and modes", () => {
    const comment = {
      id: "comment-hydrate",
      sectionId: "file:src/app.ts",
      sectionTitle: "File comment",
      filePath: "src/app.ts",
      startIndex: 1,
      endIndex: 2,
      rangeLabel: "L2 to L3",
      text: "Persisted comment.",
      diff: "@@ -2,2 +2,2 @@",
    };
    const merged = getPersistOptions().merge(
      {
        draftsByThreadKey: {
          [hydrateThreadKey]: {
            prompt: "context draft",
            attachments: [],
            elementContexts: [
              {
                id: "el-1",
                threadId: hydrateThreadId,
                pickedAt: "2026-03-13T12:00:00.000Z",
                pageUrl: "https://example.com",
                pageTitle: "Example",
                tagName: "button",
                selector: "button.save",
                htmlPreview: "<button>Save</button>",
                componentName: "SaveButton",
                source: {
                  functionName: "SaveButton",
                  fileName: "/repo/Save.tsx",
                  lineNumber: 3,
                  columnNumber: 7,
                },
                styles: ".save {}",
              },
              {
                id: "el-2",
                threadId: hydrateThreadId,
                pickedAt: "2026-03-13T12:00:00.000Z",
                pageUrl: "https://example.com",
                pageTitle: null,
                tagName: "div",
                selector: null,
                htmlPreview: 42,
                componentName: null,
                source: "not-an-object",
                styles: null,
              },
              { id: "", tagName: "missing-everything" },
            ],
            reviewComments: [comment, { id: "invalid-comment" }],
            terminalContexts: [
              {
                id: "ctx-blank-terminal",
                threadId: hydrateThreadId,
                createdAt: "2026-03-13T12:00:00.000Z",
                terminalId: "   ",
                terminalLabel: "Terminal 1",
                lineStart: 1,
                lineEnd: 2,
              },
              {
                id: "ctx-bad-lines",
                threadId: hydrateThreadId,
                createdAt: "2026-03-13T12:00:00.000Z",
                terminalId: "term-1",
                terminalLabel: "Terminal 1",
                lineStart: "NaN",
                lineEnd: 2,
              },
              {
                id: "ctx-float-lines",
                threadId: hydrateThreadId,
                createdAt: "2026-03-13T12:00:00.000Z",
                terminalId: "term-1",
                terminalLabel: "Terminal 1",
                lineStart: 2.7,
                lineEnd: 1.2,
              },
            ],
            modelSelectionByProvider: {
              codex: createModelSelection(CODEX_INSTANCE, "gpt-5.3-codex"),
            },
            activeProvider: "codex",
            runtimeMode: "approval-required",
            interactionMode: "plan",
          },
        },
        draftThreadsByThreadKey: {
          "draft-hydrate": {
            threadId: hydrateThreadId,
            environmentId: TEST_ENVIRONMENT_ID,
            projectId: "project-hydrate",
            createdAt: "2026-01-01T00:00:00.000Z",
            runtimeMode: "full-access",
            interactionMode: "default",
            branch: "feature/hydrate",
            worktreePath: "/tmp/hydrate",
            envMode: "definitely-not-a-mode",
            startFromOrigin: true,
            promotedTo: {
              environmentId: TEST_ENVIRONMENT_ID,
              threadId: "thread-hydrate-promoted",
            },
          },
        },
        logicalProjectDraftThreadKeyByLogicalProjectKey: {},
        stickyModelSelectionByProvider: {
          codex: createModelSelection(CODEX_INSTANCE, "gpt-5.4"),
        },
        stickyActiveProvider: "codex",
      },
      useComposerDraftStore.getInitialState(),
    );

    const draft = merged.draftsByThreadKey[hydrateThreadKey];
    expect(draft?.elementContexts.map((context) => context.id)).toEqual(["el-1", "el-2"]);
    expect(draft?.elementContexts[0]?.source).toMatchObject({
      functionName: "SaveButton",
      lineNumber: 3,
    });
    expect(draft?.elementContexts[1]).toMatchObject({
      selector: null,
      htmlPreview: "",
      source: null,
      styles: "",
    });
    expect(draft?.reviewComments).toEqual([comment]);
    // Invalid terminal contexts are dropped; float line numbers are normalized.
    expect(draft?.terminalContexts.map((context) => context.id)).toEqual(["ctx-float-lines"]);
    expect(draft?.terminalContexts[0]).toMatchObject({ lineStart: 2, lineEnd: 2 });
    expect(draft?.modelSelectionByProvider[CODEX_INSTANCE]?.model).toBe("gpt-5.3-codex");
    expect(draft?.activeProvider).toBe("codex");
    expect(draft?.runtimeMode).toBe("approval-required");
    expect(draft?.interactionMode).toBe("plan");

    expect(merged.draftThreadsByThreadKey["draft-hydrate"]).toMatchObject({
      threadId: hydrateThreadId,
      environmentId: TEST_ENVIRONMENT_ID,
      projectId: "project-hydrate",
      branch: "feature/hydrate",
      worktreePath: "/tmp/hydrate",
      // Unknown env modes fall back based on the worktree path.
      envMode: "worktree",
      startFromOrigin: true,
      promotedTo: {
        environmentId: TEST_ENVIRONMENT_ID,
        threadId: "thread-hydrate-promoted",
      },
    });
    expect(merged.stickyModelSelectionByProvider[CODEX_INSTANCE]?.model).toBe("gpt-5.4");
    expect(merged.stickyActiveProvider).toBe("codex");
  });

  it("rescopes raw-thread-id drafts using the environment of a scoped draft thread", () => {
    const rawId = "thread-scoped-raw";
    const scopedKey = scopedThreadKey(scopeThreadRef(TEST_ENVIRONMENT_ID, ThreadId.make(rawId)));
    const merged = getPersistOptions().merge(
      {
        draftsByThreadKey: {
          [rawId]: { prompt: "raw keyed", attachments: [] },
        },
        draftThreadsByThreadKey: {
          [scopedKey]: {
            threadId: rawId,
            environmentId: TEST_ENVIRONMENT_ID,
            projectId: "project-raw",
            createdAt: "2026-01-01T00:00:00.000Z",
            runtimeMode: "full-access",
            interactionMode: "default",
            branch: null,
            worktreePath: null,
            envMode: "local",
            startFromOrigin: false,
          },
        },
        logicalProjectDraftThreadKeyByLogicalProjectKey: {},
      },
      useComposerDraftStore.getInitialState(),
    );

    expect(merged.draftsByThreadKey[scopedKey]?.prompt).toBe("raw keyed");
    expect(merged.draftsByThreadKey[rawId]).toBeUndefined();
  });
});

describe("composerDraftStore mixed attachments", () => {
  const threadId = ThreadId.make("thread-mixed-attachments");
  const threadRef = scopeThreadRef(TEST_ENVIRONMENT_ID, threadId);

  it("dedupes and removes image and file attachments without revoking files", () => {
    const revokeObjectURL = vi.fn();
    vi.stubGlobal("URL", { revokeObjectURL });
    const image = makeImage({ id: "image-1", previewUrl: "blob:image" });
    const file = makeFile({ id: "file-1" });

    useComposerDraftStore.getState().addAttachments(threadRef, [image, file]);
    useComposerDraftStore.getState().addAttachment(threadRef, makeFile({ id: "file-duplicate" }));
    expect(
      draftFor(threadId, TEST_ENVIRONMENT_ID)?.attachments.map((attachment) => attachment.id),
    ).toEqual(["image-1", "file-1"]);

    useComposerDraftStore.getState().removeAttachment(threadRef, file.id);
    expect(
      draftFor(threadId, TEST_ENVIRONMENT_ID)?.attachments.map((attachment) => attachment.id),
    ).toEqual(["image-1"]);
    expect(revokeObjectURL).not.toHaveBeenCalled();

    useComposerDraftStore.getState().removeAttachment(threadRef, image.id);
    expect(revokeObjectURL).toHaveBeenCalledWith("blob:image");
  });
});

// ---------------------------------------------------------------------------
// Store actions: images, contexts, annotations, attachments
// ---------------------------------------------------------------------------

describe("composerDraftStore removeAttachment", () => {
  const threadId = ThreadId.make("thread-remove-image");
  const threadRef = scopeThreadRef(TEST_ENVIRONMENT_ID, threadId);
  let originalRevokeObjectUrl: typeof URL.revokeObjectURL;
  let revokeSpy: ReturnType<typeof vi.fn<(url: string) => void>>;

  beforeEach(() => {
    resetComposerDraftStore();
    originalRevokeObjectUrl = URL.revokeObjectURL;
    revokeSpy = vi.fn();
    URL.revokeObjectURL = revokeSpy;
  });

  afterEach(() => {
    URL.revokeObjectURL = originalRevokeObjectUrl;
  });

  it("removes an image, revokes its preview URL and keeps the rest of the draft", () => {
    const store = useComposerDraftStore.getState();
    store.setPrompt(threadRef, "keep me");
    store.addAttachment(threadRef, makeImage({ id: "img-1", previewUrl: "blob:one" }));

    store.removeAttachment(threadRef, "img-1");

    const draft = draftFor(threadId, TEST_ENVIRONMENT_ID);
    expect(draft?.prompt).toBe("keep me");
    expect(draft?.attachments).toEqual([]);
    expect(revokeSpy).toHaveBeenCalledWith("blob:one");
  });

  it("drops the draft entirely when the last image is removed", () => {
    const store = useComposerDraftStore.getState();
    store.addAttachment(threadRef, makeImage({ id: "img-only", previewUrl: "blob:only" }));

    store.removeAttachment(threadRef, "img-only");

    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)).toBeUndefined();
  });

  it("is a no-op for unknown image ids and unknown threads", () => {
    const store = useComposerDraftStore.getState();
    store.addAttachment(threadRef, makeImage({ id: "img-keep", previewUrl: "blob:keep" }));

    store.removeAttachment(threadRef, "img-missing");
    store.removeAttachment(scopeThreadRef(OTHER_TEST_ENVIRONMENT_ID, threadId), "img-keep");

    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)?.attachments.map((image) => image.id)).toEqual([
      "img-keep",
    ]);
    expect(revokeSpy).not.toHaveBeenCalled();
  });

  it("ignores empty image batches", () => {
    const store = useComposerDraftStore.getState();
    store.addAttachments(threadRef, []);
    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)).toBeUndefined();
  });
});

describe("composerDraftStore terminal context editing", () => {
  const threadId = ThreadId.make("thread-context-editing");
  const threadRef = scopeThreadRef(TEST_ENVIRONMENT_ID, threadId);

  beforeEach(() => {
    resetComposerDraftStore();
  });

  it("setTerminalContexts normalizes, dedupes and drops invalid contexts", () => {
    const store = useComposerDraftStore.getState();
    store.setTerminalContexts(threadRef, [
      makeTerminalContext({ id: "ctx-valid", lineStart: 3, lineEnd: 1 }),
      // Same id → dropped even with a different signature.
      makeTerminalContext({ id: "ctx-valid", lineStart: 9, lineEnd: 12 }),
      // Blank terminal id → invalid.
      makeTerminalContext({ id: "ctx-invalid", terminalId: "   " }),
    ]);

    const draft = draftFor(threadId, TEST_ENVIRONMENT_ID);
    expect(draft?.terminalContexts.map((context) => context.id)).toEqual(["ctx-valid"]);
    // lineEnd is clamped to at least lineStart.
    expect(draft?.terminalContexts[0]).toMatchObject({ lineStart: 3, lineEnd: 3 });
  });

  it("setTerminalContexts with an empty list clears the context slice", () => {
    const store = useComposerDraftStore.getState();
    store.setTerminalContexts(threadRef, [makeTerminalContext({ id: "ctx-1" })]);
    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)?.terminalContexts).toHaveLength(1);

    store.setTerminalContexts(threadRef, []);
    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)?.terminalContexts).toEqual([]);
  });

  it("removeTerminalContext removes one context and keeps siblings", () => {
    const store = useComposerDraftStore.getState();
    store.addTerminalContexts(threadRef, [
      makeTerminalContext({ id: "ctx-a" }),
      makeTerminalContext({ id: "ctx-b", lineStart: 20, lineEnd: 21 }),
    ]);

    store.removeTerminalContext(threadRef, "ctx-a");
    expect(
      draftFor(threadId, TEST_ENVIRONMENT_ID)?.terminalContexts.map((context) => context.id),
    ).toEqual(["ctx-b"]);

    // Blank context id is a no-op.
    store.removeTerminalContext(threadRef, "");
    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)?.terminalContexts).toHaveLength(1);
  });

  it("clearTerminalContexts wipes the slice and removes empty drafts", () => {
    const store = useComposerDraftStore.getState();
    store.setPrompt(threadRef, "hold the draft");
    store.addTerminalContext(threadRef, makeTerminalContext({ id: "ctx-clear" }));

    store.clearTerminalContexts(threadRef);
    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)?.terminalContexts).toEqual([]);

    // Clearing again is a no-op.
    store.clearTerminalContexts(threadRef);
    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)?.prompt).toContain("hold the draft");
  });

  it("insertTerminalContext rejects invalid and duplicate contexts", () => {
    const store = useComposerDraftStore.getState();
    expect(
      store.insertTerminalContext(
        threadRef,
        "prompt",
        makeTerminalContext({ id: "ctx-bad", terminalLabel: "  " }),
        0,
      ),
    ).toBe(false);

    store.addTerminalContext(threadRef, makeTerminalContext({ id: "ctx-first" }));
    expect(
      store.insertTerminalContext(
        threadRef,
        "prompt",
        makeTerminalContext({ id: "ctx-duplicate-signature" }),
        0,
      ),
    ).toBe(false);
  });

  it("does not create contexts for unresolvable draft targets", () => {
    const store = useComposerDraftStore.getState();
    const orphanDraftId = DraftId.make("draft-without-session");
    store.addTerminalContext(orphanDraftId, makeTerminalContext({ id: "ctx-orphan" }));
    expect(store.getComposerDraft(orphanDraftId)).toBeNull();
    expect(
      store.addElementContext(orphanDraftId, {
        pageUrl: "https://example.com",
        pageTitle: null,
        tagName: "div",
        selector: null,
        htmlPreview: "<div />",
        componentName: null,
        source: null,
        styles: "",
      }),
    ).toBe(false);
  });
});

describe("composerDraftStore element context clearing", () => {
  const threadId = ThreadId.make("thread-element-clear");
  const threadRef = scopeThreadRef(TEST_ENVIRONMENT_ID, threadId);
  const selection = {
    pageUrl: "https://example.com/settings",
    pageTitle: "Settings",
    tagName: "input",
    selector: "input.token",
    htmlPreview: "<input />",
    componentName: null,
    source: null,
    styles: "",
  } as const;

  beforeEach(() => {
    resetComposerDraftStore();
  });

  it("clearElementContexts wipes the slice and removes empty drafts", () => {
    const store = useComposerDraftStore.getState();
    store.addElementContext(threadRef, selection);
    store.clearElementContexts(threadRef);
    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)).toBeUndefined();

    // Clearing an absent draft is a no-op.
    store.clearElementContexts(threadRef);
    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)).toBeUndefined();
  });

  it("keeps non-empty drafts when clearing element contexts", () => {
    const store = useComposerDraftStore.getState();
    store.setPrompt(threadRef, "still here");
    store.addElementContext(threadRef, selection);
    store.clearElementContexts(threadRef);
    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)).toMatchObject({
      prompt: "still here",
      elementContexts: [],
    });
  });

  it("removeElementContext ignores unknown ids", () => {
    const store = useComposerDraftStore.getState();
    store.addElementContext(threadRef, selection);
    store.removeElementContext(threadRef, "el-missing");
    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)?.elementContexts).toHaveLength(1);
  });
});

function makePreviewAnnotation(input: {
  id: string;
  withScreenshot?: boolean;
}): PreviewAnnotationPayload {
  return {
    id: input.id,
    pageUrl: "https://example.com/page",
    pageTitle: "Page",
    comment: "Fix the header spacing",
    elements: [],
    regions: [],
    strokes: [],
    styleChanges: [],
    screenshot: input.withScreenshot
      ? {
          dataUrl: "data:image/png;base64,QUJD",
          width: 100,
          height: 50,
          cropRect: { x: 0, y: 0, width: 100, height: 50 },
        }
      : null,
    createdAt: "2026-03-13T12:00:00.000Z",
  };
}

describe("composerDraftStore preview annotations", () => {
  const threadId = ThreadId.make("thread-preview-annotations");
  const threadRef = scopeThreadRef(TEST_ENVIRONMENT_ID, threadId);

  beforeEach(() => {
    resetComposerDraftStore();
  });

  it("adds annotations and strips screenshot payload data", () => {
    const store = useComposerDraftStore.getState();
    store.addPreviewAnnotation(
      threadRef,
      makePreviewAnnotation({ id: "ann-1", withScreenshot: true }),
    );

    const draft = draftFor(threadId, TEST_ENVIRONMENT_ID);
    expect(draft?.previewAnnotations).toHaveLength(1);
    expect(draft?.previewAnnotations[0]?.screenshot).toMatchObject({ dataUrl: "", width: 100 });
  });

  it("replaces annotations with the same id instead of duplicating them", () => {
    const store = useComposerDraftStore.getState();
    store.addPreviewAnnotation(threadRef, makePreviewAnnotation({ id: "ann-1" }));
    store.addPreviewAnnotation(threadRef, {
      ...makePreviewAnnotation({ id: "ann-1" }),
      comment: "Updated annotation",
    });

    const draft = draftFor(threadId, TEST_ENVIRONMENT_ID);
    expect(draft?.previewAnnotations).toHaveLength(1);
    expect(draft?.previewAnnotations[0]?.comment).toBe("Updated annotation");
    expect(draft?.previewAnnotations[0]?.screenshot).toBeNull();
  });

  it("setPreviewAnnotations replaces the whole slice", () => {
    const store = useComposerDraftStore.getState();
    store.addPreviewAnnotation(threadRef, makePreviewAnnotation({ id: "ann-old" }));
    store.setPreviewAnnotations(threadRef, [
      makePreviewAnnotation({ id: "ann-new-1" }),
      makePreviewAnnotation({ id: "ann-new-2" }),
    ]);

    expect(
      draftFor(threadId, TEST_ENVIRONMENT_ID)?.previewAnnotations.map((entry) => entry.id),
    ).toEqual(["ann-new-1", "ann-new-2"]);
  });

  it("removePreviewAnnotation drops the annotation and its staged screenshot image", () => {
    const store = useComposerDraftStore.getState();
    const originalRevokeObjectUrl = URL.revokeObjectURL;
    const revokeSpy = vi.fn<(url: string) => void>();
    URL.revokeObjectURL = revokeSpy;
    try {
      store.addPreviewAnnotation(threadRef, makePreviewAnnotation({ id: "ann-img" }));
      store.addAttachment(threadRef, makeImage({ id: "ann-img", previewUrl: "blob:annotation" }));
      store.removePreviewAnnotation(threadRef, "ann-img");

      expect(draftFor(threadId, TEST_ENVIRONMENT_ID)).toBeUndefined();
      expect(revokeSpy).toHaveBeenCalledTimes(1);
      expect(revokeSpy).toHaveBeenCalledWith("blob:annotation");

      store.addPreviewAnnotation(threadRef, makePreviewAnnotation({ id: "ann-file" }));
      store.addAttachment(threadRef, makeFile({ id: "ann-file" }));
      store.removePreviewAnnotation(threadRef, "ann-file");

      expect(revokeSpy).toHaveBeenCalledTimes(1);
    } finally {
      URL.revokeObjectURL = originalRevokeObjectUrl;
    }
  });

  it("removePreviewAnnotation ignores unknown ids and empty drafts", () => {
    const store = useComposerDraftStore.getState();
    store.removePreviewAnnotation(threadRef, "ann-none");
    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)).toBeUndefined();

    store.addPreviewAnnotation(threadRef, makePreviewAnnotation({ id: "ann-kept" }));
    store.removePreviewAnnotation(threadRef, "ann-unknown");
    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)?.previewAnnotations).toHaveLength(1);
  });
});

describe("composerDraftStore setReviewComments", () => {
  const threadId = ThreadId.make("thread-set-review-comments");
  const threadRef = scopeThreadRef(TEST_ENVIRONMENT_ID, threadId);
  const comment: ReviewCommentContext = {
    id: "comment-replace",
    sectionId: "file:src/index.ts",
    sectionTitle: "File comment",
    filePath: "src/index.ts",
    startIndex: 0,
    endIndex: 1,
    rangeLabel: "L1 to L2",
    text: "Replace me.",
    diff: "@@ -1,1 +1,1 @@",
  };

  beforeEach(() => {
    resetComposerDraftStore();
  });

  it("replaces the slice and filters malformed comments", () => {
    const store = useComposerDraftStore.getState();
    store.addReviewComment(threadRef, comment);
    store.setReviewComments(threadRef, [
      { ...comment, id: "comment-next", text: "Next comment." },
      { id: "malformed" } as unknown as ReviewCommentContext,
    ]);

    expect(
      draftFor(threadId, TEST_ENVIRONMENT_ID)?.reviewComments.map((entry) => entry.id),
    ).toEqual(["comment-next"]);
  });

  it("removes a comments-only draft when the slice is emptied", () => {
    const store = useComposerDraftStore.getState();
    store.addReviewComment(threadRef, comment);
    store.setReviewComments(threadRef, []);
    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)).toBeUndefined();
  });

  it("removeReviewComment ignores unknown comment ids", () => {
    const store = useComposerDraftStore.getState();
    store.addReviewComment(threadRef, comment);
    store.removeReviewComment(threadRef, "comment-unknown");
    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)?.reviewComments).toHaveLength(1);
  });
});

describe("composerDraftStore persisted attachment lifecycle", () => {
  const threadId = ThreadId.make("thread-attachment-lifecycle");
  const threadRef = scopeThreadRef(TEST_ENVIRONMENT_ID, threadId);
  const threadKey = threadKeyFor(threadId, TEST_ENVIRONMENT_ID);

  beforeEach(() => {
    removeLocalStorageItem(COMPOSER_DRAFT_STORAGE_KEY);
    resetComposerDraftStore();
  });

  afterEach(() => {
    removeLocalStorageItem(COMPOSER_DRAFT_STORAGE_KEY);
  });

  it("confirms persisted attachments when storage has flushed them", async () => {
    const image = makeImage({ id: "img-persist-ok", previewUrl: "blob:persist-ok" });
    const attachment = {
      type: "image" as const,
      id: image.id,
      name: image.name,
      mimeType: image.mimeType,
      sizeBytes: image.sizeBytes,
      dataUrl: "data:image/png;base64,QUJD",
    };
    setLocalStorageItem(
      COMPOSER_DRAFT_STORAGE_KEY,
      {
        version: 8,
        state: {
          draftsByThreadKey: {
            [threadKey]: { prompt: "", attachments: [attachment] },
          },
          draftThreadsByThreadKey: {},
          logicalProjectDraftThreadKeyByLogicalProjectKey: {},
        },
      },
      Schema.Unknown,
    );

    useComposerDraftStore.getState().addAttachment(threadRef, image);
    useComposerDraftStore.getState().syncPersistedAttachments(threadRef, [attachment]);
    await Promise.resolve();

    const draft = draftFor(threadId, TEST_ENVIRONMENT_ID);
    expect(draft?.persistedAttachments).toEqual([attachment]);
    expect(draft?.nonPersistedAttachmentIds).toEqual([]);
  });

  it("removes a draft that only staged attachments no image still references", async () => {
    const attachment = {
      type: "image" as const,
      id: "img-ghost",
      name: "ghost.png",
      mimeType: "image/png",
      sizeBytes: 3,
      dataUrl: "data:image/png;base64,QUJD",
    };
    useComposerDraftStore.setState((state) => ({
      draftsByThreadKey: {
        ...state.draftsByThreadKey,
        [threadKey]: { ...createEmptyThreadDraft(), persistedAttachments: [attachment] },
      },
    }));

    useComposerDraftStore.getState().syncPersistedAttachments(threadRef, [attachment]);
    await Promise.resolve();

    expect(draftByKey(threadKey)).toBeUndefined();
  });

  it("clearPersistedAttachments resets attachment bookkeeping but keeps content", () => {
    const attachment = {
      type: "image" as const,
      id: "img-clear",
      name: "clear.png",
      mimeType: "image/png",
      sizeBytes: 3,
      dataUrl: "data:image/png;base64,QUJD",
    };
    useComposerDraftStore.setState((state) => ({
      draftsByThreadKey: {
        ...state.draftsByThreadKey,
        [threadKey]: {
          ...createEmptyThreadDraft(),
          prompt: "keep",
          persistedAttachments: [attachment],
          nonPersistedAttachmentIds: ["img-clear"],
        },
      },
    }));

    useComposerDraftStore.getState().clearPersistedAttachments(threadRef);

    expect(draftByKey(threadKey)).toMatchObject({
      prompt: "keep",
      persistedAttachments: [],
      nonPersistedAttachmentIds: [],
    });
  });

  it("clearPersistedAttachments removes attachment-only drafts and skips missing ones", () => {
    const attachment = {
      type: "image" as const,
      id: "img-clear-only",
      name: "only.png",
      mimeType: "image/png",
      sizeBytes: 3,
      dataUrl: "data:image/png;base64,QUJD",
    };
    useComposerDraftStore.setState((state) => ({
      draftsByThreadKey: {
        ...state.draftsByThreadKey,
        [threadKey]: { ...createEmptyThreadDraft(), persistedAttachments: [attachment] },
      },
    }));

    useComposerDraftStore.getState().clearPersistedAttachments(threadRef);
    expect(draftByKey(threadKey)).toBeUndefined();

    // No draft → no-op.
    useComposerDraftStore.getState().clearPersistedAttachments(threadRef);
    expect(draftByKey(threadKey)).toBeUndefined();
  });
});

// ---------------------------------------------------------------------------
// Draft session getters and promotion edge cases
// ---------------------------------------------------------------------------

describe("composerDraftStore draft session getters", () => {
  const projectId = ProjectId.make("project-getters");
  const projectRef = scopeProjectRef(TEST_ENVIRONMENT_ID, projectId);
  const threadId = ThreadId.make("thread-getters");
  const draftId = DraftId.make("draft-getters");

  beforeEach(() => {
    resetComposerDraftStore();
  });

  it("looks up draft sessions by logical project key", () => {
    const store = useComposerDraftStore.getState();
    store.setProjectDraftThreadId(projectRef, draftId, { threadId });

    const session = store.getDraftThreadByLogicalProjectKey(scopedProjectKey(projectRef));
    expect(session).toMatchObject({ draftId, threadId, projectId });

    expect(store.getDraftSessionByLogicalProjectKey("   ")).toBeNull();
    expect(store.getDraftSessionByLogicalProjectKey("missing-key")).toBeNull();
  });

  it("hides promoting draft sessions from logical project lookups", () => {
    const store = useComposerDraftStore.getState();
    store.setProjectDraftThreadId(projectRef, draftId, { threadId });
    markPromotedDraftThread(threadId);

    expect(
      useComposerDraftStore
        .getState()
        .getDraftThreadByLogicalProjectKey(scopedProjectKey(projectRef)),
    ).toBeNull();
  });

  it("lists scoped draft thread keys and reports environment membership", () => {
    const store = useComposerDraftStore.getState();
    store.setProjectDraftThreadId(projectRef, draftId, { threadId });

    expect(useComposerDraftStore.getState().listDraftThreadKeys()).toEqual([
      scopedThreadKey(scopeThreadRef(TEST_ENVIRONMENT_ID, threadId)),
    ]);
    expect(useComposerDraftStore.getState().hasDraftThreadsInEnvironment(TEST_ENVIRONMENT_ID)).toBe(
      true,
    );
    expect(
      useComposerDraftStore.getState().hasDraftThreadsInEnvironment(OTHER_TEST_ENVIRONMENT_ID),
    ).toBe(false);
  });

  it("returns null for unknown or blank composer targets", () => {
    const store = useComposerDraftStore.getState();
    expect(store.getComposerDraft(DraftId.make("   "))).toBeNull();
    expect(store.getDraftSession(DraftId.make("draft-unknown"))).toBeNull();
  });

  it("ignores promotion marks for unresolvable and unknown targets", () => {
    const store = useComposerDraftStore.getState();
    const before = useComposerDraftStore.getState().draftThreadsByThreadKey;

    store.markDraftThreadPromoting(DraftId.make("   "));
    store.markDraftThreadPromoting(DraftId.make("draft-not-there"));
    store.finalizePromotedDraftThread(DraftId.make("draft-not-there"));
    store.clearDraftThread(DraftId.make("draft-not-there"));

    expect(useComposerDraftStore.getState().draftThreadsByThreadKey).toBe(before);
  });

  it("re-marking an already promoting draft keeps state stable", () => {
    const store = useComposerDraftStore.getState();
    store.setProjectDraftThreadId(projectRef, draftId, { threadId });

    store.markDraftThreadPromoting(draftId);
    const afterFirst = useComposerDraftStore.getState().draftThreadsByThreadKey;
    store.markDraftThreadPromoting(draftId);

    expect(useComposerDraftStore.getState().draftThreadsByThreadKey).toBe(afterFirst);
  });

  it("does not finalize a draft that was never marked as promoting", () => {
    const store = useComposerDraftStore.getState();
    store.setProjectDraftThreadId(projectRef, draftId, { threadId });

    store.finalizePromotedDraftThread(draftId);

    expect(useComposerDraftStore.getState().getDraftThread(draftId)).not.toBeNull();
  });

  it("marks and finalizes promoted drafts through the scoped-ref helpers", () => {
    const store = useComposerDraftStore.getState();
    const threadRef = scopeThreadRef(TEST_ENVIRONMENT_ID, threadId);
    store.setProjectDraftThreadId(projectRef, draftId, { threadId });

    markPromotedDraftThreadByRef(threadRef);
    expect(useComposerDraftStore.getState().getDraftThread(draftId)?.promotedTo).toEqual(threadRef);

    finalizePromotedDraftThreadsByRef([threadRef]);
    expect(useComposerDraftStore.getState().getDraftThread(draftId)).toBeNull();
  });

  it("setDraftThreadContext handles unknown targets and empty updates", () => {
    const store = useComposerDraftStore.getState();
    store.setDraftThreadContext(DraftId.make("draft-not-registered"), { branch: "main" });
    expect(useComposerDraftStore.getState().draftThreadsByThreadKey).toEqual({});

    store.setProjectDraftThreadId(projectRef, draftId, { threadId });
    const before = useComposerDraftStore.getState().draftThreadsByThreadKey;
    store.setDraftThreadContext(draftId, {});
    expect(useComposerDraftStore.getState().draftThreadsByThreadKey).toBe(before);
  });

  it("setDraftThreadContext updates modes and preserves createdAt for empty overrides", () => {
    const store = useComposerDraftStore.getState();
    store.setProjectDraftThreadId(projectRef, draftId, {
      threadId,
      createdAt: "2026-01-01T00:00:00.000Z",
    });

    store.setDraftThreadContext(draftId, {
      createdAt: "",
      runtimeMode: "approval-required",
      interactionMode: "plan",
    });

    expect(useComposerDraftStore.getState().getDraftThread(draftId)).toMatchObject({
      createdAt: "2026-01-01T00:00:00.000Z",
      runtimeMode: "approval-required",
      interactionMode: "plan",
    });
  });

  it("setDraftThreadContext rejects project refs without identity", () => {
    const store = useComposerDraftStore.getState();
    store.setProjectDraftThreadId(projectRef, draftId, { threadId });

    store.setDraftThreadContext(draftId, {
      projectRef: scopeProjectRef("" as EnvironmentId, "" as ProjectId),
    });

    expect(useComposerDraftStore.getState().getDraftThread(draftId)).toMatchObject({
      environmentId: TEST_ENVIRONMENT_ID,
      projectId,
    });
  });
});

// ---------------------------------------------------------------------------
// Setter edge cases
// ---------------------------------------------------------------------------

describe("composerDraftStore setter edge cases", () => {
  const threadId = ThreadId.make("thread-setter-edges");
  const threadRef = scopeThreadRef(TEST_ENVIRONMENT_ID, threadId);

  beforeEach(() => {
    resetComposerDraftStore();
  });

  it("setPrompt ignores blank draft targets", () => {
    useComposerDraftStore.getState().setPrompt(DraftId.make("   "), "never stored");
    expect(useComposerDraftStore.getState().draftsByThreadKey).toEqual({});
  });

  it("setModelSelection with a null selection never creates a draft", () => {
    useComposerDraftStore.getState().setModelSelection(threadRef, null);
    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)).toBeUndefined();
  });

  it("setModelSelection with an invalid selection keeps the draft unchanged", () => {
    const store = useComposerDraftStore.getState();
    store.setPrompt(threadRef, "existing");
    store.setModelSelection(threadRef, undefined);
    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)).toMatchObject({
      prompt: "existing",
      activeProvider: null,
    });
  });

  it("setModelOptions is a no-op without a draft and empty options", () => {
    useComposerDraftStore.getState().setModelOptions(threadRef, null);
    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)).toBeUndefined();
  });

  it("setModelOptions clears a provider's options when given an empty bag", () => {
    const store = useComposerDraftStore.getState();
    store.setModelOptions(threadRef, providerModelOptions({ codex: { fastMode: true } }));
    store.setModelOptions(threadRef, { codex: [] });

    const selection = draftFor(threadId, TEST_ENVIRONMENT_ID)?.modelSelectionByProvider[
      CODEX_INSTANCE
    ];
    expect(selection?.options).toBeUndefined();
    expect(selection?.model.length).toBeGreaterThan(0);
  });

  it("setProviderModelOptions rejects non-string driver kinds", () => {
    const store = useComposerDraftStore.getState();
    store.setProviderModelOptions(
      threadRef,
      42 as unknown as ProviderDriverKind,
      toSelections({ fastMode: true }),
    );
    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)).toBeUndefined();
  });

  it("setProviderModelOptions clears draft and sticky options with a null bag", () => {
    const store = useComposerDraftStore.getState();
    store.setModelSelection(threadRef, modelSelection(CODEX_DRIVER, "gpt-5.4", { fastMode: true }));
    store.setStickyModelSelection(modelSelection(CODEX_DRIVER, "gpt-5.4", { fastMode: true }));

    store.setProviderModelOptions(threadRef, CODEX_DRIVER, null, { persistSticky: true });

    expect(
      draftFor(threadId, TEST_ENVIRONMENT_ID)?.modelSelectionByProvider[CODEX_INSTANCE]?.options,
    ).toBeUndefined();
    expect(
      useComposerDraftStore.getState().stickyModelSelectionByProvider[CODEX_INSTANCE]?.options,
    ).toBeUndefined();
  });

  it("setRuntimeMode ignores invalid values and repeated assignments", () => {
    const store = useComposerDraftStore.getState();
    store.setRuntimeMode(threadRef, "bogus-mode" as RuntimeMode);
    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)).toBeUndefined();

    store.setRuntimeMode(threadRef, "approval-required");
    const before = useComposerDraftStore.getState().draftsByThreadKey;
    store.setRuntimeMode(threadRef, "approval-required");
    expect(useComposerDraftStore.getState().draftsByThreadKey).toBe(before);
  });

  it("setInteractionMode ignores repeated assignments", () => {
    const store = useComposerDraftStore.getState();
    store.setInteractionMode(threadRef, "invalid" as never);
    expect(draftFor(threadId, TEST_ENVIRONMENT_ID)).toBeUndefined();

    store.setInteractionMode(threadRef, "plan");
    const before = useComposerDraftStore.getState().draftsByThreadKey;
    store.setInteractionMode(threadRef, "plan");
    expect(useComposerDraftStore.getState().draftsByThreadKey).toBe(before);
  });

  it("applyStickyState without sticky data leaves the store untouched", () => {
    const store = useComposerDraftStore.getState();
    store.applyStickyState(threadRef);
    expect(useComposerDraftStore.getState().draftsByThreadKey).toEqual({});
  });

  it("applyStickyState keeps the draft's chosen model over the sticky model", () => {
    const store = useComposerDraftStore.getState();
    store.setModelSelection(threadRef, modelSelection(CODEX_DRIVER, "gpt-5.3-codex"));
    store.setStickyModelSelection(modelSelection(CODEX_DRIVER, "gpt-5.4", { fastMode: true }));

    store.applyStickyState(threadRef);

    expect(
      draftFor(threadId, TEST_ENVIRONMENT_ID)?.modelSelectionByProvider[CODEX_INSTANCE],
    ).toMatchObject({
      model: "gpt-5.3-codex",
      options: [{ id: "fastMode", value: true }],
    });
  });

  it("setStickyModelSelection ignores unparseable selections", () => {
    const store = useComposerDraftStore.getState();
    store.setStickyModelSelection(null);
    store.setStickyModelSelection({
      instanceId: "!!bad!!",
      model: "x",
    } as unknown as ModelSelection);
    expect(useComposerDraftStore.getState().stickyModelSelectionByProvider).toEqual({});
    expect(useComposerDraftStore.getState().stickyActiveProvider).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// React hooks (static server render probes)
// ---------------------------------------------------------------------------

function renderHookProbe<T>(hook: () => T): T {
  let captured: T | undefined;
  function Probe(): null {
    captured = hook();
    return null;
  }
  renderToStaticMarkup(createElement(Probe));
  return captured as T;
}

describe("composerDraftStore hooks", () => {
  const threadId = ThreadId.make("thread-hooks");
  const threadRef = scopeThreadRef(TEST_ENVIRONMENT_ID, threadId);
  const providers = [serverProvider({ instanceId: "codex", models: ["gpt-test-a"] })];

  beforeEach(() => {
    resetComposerDraftStore();
  });

  it("useComposerThreadDraft returns the empty draft for unknown threads", () => {
    const draft = renderHookProbe(() => useComposerThreadDraft(threadRef));
    expect(draft).toMatchObject({
      prompt: "",
      attachments: [],
      terminalContexts: [],
      activeProvider: null,
      runtimeMode: null,
    });
  });

  it("useComposerDraftModelState returns the empty model state for unknown drafts", () => {
    const modelState = renderHookProbe(() =>
      useComposerDraftModelState(DraftId.make("draft-hook-probe")),
    );
    expect(modelState).toEqual({ activeProvider: null, modelSelectionByProvider: {} });
  });

  it("useEffectiveComposerModelState derives defaults without any selection", () => {
    const state = renderHookProbe(() =>
      useEffectiveComposerModelState({
        providers,
        selectedProvider: CODEX_DRIVER,
        threadModelSelection: null,
        projectModelSelection: null,
        settings: DEFAULT_UNIFIED_SETTINGS,
      }),
    );
    expect(state).toEqual({ selectedModel: "gpt-test-a", modelOptions: null });
  });

  it("useEffectiveComposerModelState accepts a draft id target", () => {
    const state = renderHookProbe(() =>
      useEffectiveComposerModelState({
        draftId: DraftId.make("draft-hook-probe"),
        providers,
        selectedProvider: CODEX_DRIVER,
        threadModelSelection: createModelSelection(
          CODEX_INSTANCE,
          "gpt-test-a",
          toSelections({ fastMode: true }),
        ),
        projectModelSelection: null,
        settings: DEFAULT_UNIFIED_SETTINGS,
      }),
    );
    expect(state.selectedModel).toBe("gpt-test-a");
    expect(state.modelOptions).toEqual({ codex: [{ id: "fastMode", value: true }] });
  });
});

describe("composerDraftStore invalid draft targets", () => {
  const emptyDraftId = DraftId.make("");

  function expectInvalidTargetDidNotMutate(): void {
    expect(useComposerDraftStore.getState()).toMatchObject({
      draftsByThreadKey: {},
      draftThreadsByThreadKey: {},
      logicalProjectDraftThreadKeyByLogicalProjectKey: {},
      stickyModelSelectionByProvider: {},
      stickyActiveProvider: null,
    });
  }

  beforeEach(() => {
    resetComposerDraftStore();
  });

  it("leaves model and execution state unchanged for an empty draft identity", () => {
    const store = useComposerDraftStore.getState();

    store.setPrompt(emptyDraftId, "ignored");
    expectInvalidTargetDidNotMutate();
    store.setModelSelection(emptyDraftId, modelSelection(CODEX_DRIVER, "gpt-5.4"));
    expectInvalidTargetDidNotMutate();
    store.setModelOptions(emptyDraftId, providerModelOptions({ codex: { fastMode: true } }));
    expectInvalidTargetDidNotMutate();
    store.applyStickyState(emptyDraftId);
    expectInvalidTargetDidNotMutate();
    store.setProviderModelOptions(emptyDraftId, CODEX_DRIVER, [{ id: "fastMode", value: true }]);
    expectInvalidTargetDidNotMutate();
    store.setRuntimeMode(emptyDraftId, "full-access");
    expectInvalidTargetDidNotMutate();
    store.setInteractionMode(emptyDraftId, "plan");
    expectInvalidTargetDidNotMutate();
    store.setDraftThreadContext(emptyDraftId, { branch: "ignored" });
    expectInvalidTargetDidNotMutate();
  });

  it("ignores attachment and context operations for an empty draft identity", () => {
    const store = useComposerDraftStore.getState();
    const image = makeImage({ id: "ignored-image", previewUrl: "blob:ignored" });
    const terminalContext = makeTerminalContext({ id: "ignored-terminal" });

    store.addAttachment(emptyDraftId, image);
    expectInvalidTargetDidNotMutate();
    store.addAttachments(emptyDraftId, [image]);
    expectInvalidTargetDidNotMutate();
    store.removeAttachment(emptyDraftId, image.id);
    expectInvalidTargetDidNotMutate();
    expect(store.insertTerminalContext(emptyDraftId, "prompt", terminalContext, 0)).toBe(false);
    expectInvalidTargetDidNotMutate();
    store.addTerminalContext(emptyDraftId, terminalContext);
    expectInvalidTargetDidNotMutate();
    store.addTerminalContexts(emptyDraftId, [terminalContext]);
    expectInvalidTargetDidNotMutate();
    store.removeTerminalContext(emptyDraftId, terminalContext.id);
    expectInvalidTargetDidNotMutate();
    store.clearTerminalContexts(emptyDraftId);
    expectInvalidTargetDidNotMutate();
    expect(store.addElementContext(emptyDraftId, {} as never)).toBe(false);
    expectInvalidTargetDidNotMutate();
    store.setElementContexts(emptyDraftId, []);
    expectInvalidTargetDidNotMutate();
    store.removeElementContext(emptyDraftId, "element");
    expectInvalidTargetDidNotMutate();
    store.clearElementContexts(emptyDraftId);
    expectInvalidTargetDidNotMutate();
  });

  it("ignores annotation, review, persistence, and cleanup operations for an empty draft identity", () => {
    const store = useComposerDraftStore.getState();
    const annotation = makePreviewAnnotation({ id: "ignored-annotation" });

    store.addPreviewAnnotation(emptyDraftId, annotation);
    expectInvalidTargetDidNotMutate();
    store.setPreviewAnnotations(emptyDraftId, [annotation]);
    expectInvalidTargetDidNotMutate();
    store.removePreviewAnnotation(emptyDraftId, annotation.id);
    expectInvalidTargetDidNotMutate();
    store.addReviewComment(emptyDraftId, {} as never);
    expectInvalidTargetDidNotMutate();
    store.setReviewComments(emptyDraftId, []);
    expectInvalidTargetDidNotMutate();
    store.removeReviewComment(emptyDraftId, "comment");
    expectInvalidTargetDidNotMutate();
    store.clearPersistedAttachments(emptyDraftId);
    expectInvalidTargetDidNotMutate();
    store.syncPersistedAttachments(emptyDraftId, []);
    expectInvalidTargetDidNotMutate();
    store.clearComposerContent(emptyDraftId);
    expectInvalidTargetDidNotMutate();
    store.markDraftThreadPromoting(emptyDraftId);
    expectInvalidTargetDidNotMutate();
    store.finalizePromotedDraftThread(emptyDraftId);
    expectInvalidTargetDidNotMutate();
    store.clearDraftThread(emptyDraftId);
    expectInvalidTargetDidNotMutate();
  });
});
