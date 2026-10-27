/**
 * Unit tests for the ChatComposer mega-component.
 *
 * Strategy: the composer is rendered once per scenario with
 * `renderToStaticMarkup` (no DOM, per web test conventions). Heavy children
 * and UI primitives are replaced with prop-capturing mocks, the real composer
 * draft store is seeded directly, and a partial react mock instruments the
 * stateful hooks: `useState` values are seedable by ordinal index (setter
 * calls are recorded), effects and imperative handles are queued during
 * render and flushed afterwards. A jsx-runtime tap records every host
 * element's props so DOM handlers (drag/drop, focus capture, form submit,
 * collapsed-mobile buttons) can be invoked directly with fake events.
 */
import {
  ApprovalRequestId,
  EnvironmentId,
  ProjectId,
  ProviderDriverKind,
  ProviderInstanceId,
  PROVIDER_SEND_TURN_MAX_ATTACHMENTS,
  PROVIDER_SEND_TURN_MAX_IMAGE_BYTES,
  type ResolvedKeybindingsConfig,
  type ServerProvider,
  ThreadId,
} from "@coddy/contracts";
import { DEFAULT_UNIFIED_SETTINGS } from "@coddy/contracts/settings";
import { scopedThreadKey, scopeThreadRef } from "@coddy/client-runtime/environment";
import type { EnvironmentConnectionPresentation } from "@coddy/client-runtime/connection";
import { serializeComposerReference } from "@coddy/shared/composerReferences";
import * as React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

// ---------------------------------------------------------------------------
// Hoisted harness shared with every vi.mock factory.
// ---------------------------------------------------------------------------

const h = vi.hoisted(() => {
  interface Captured {
    readonly name: string;
    readonly props: Record<string, unknown>;
  }
  interface HostElement {
    readonly type: string;
    readonly props: Record<string, unknown>;
  }

  const state = {
    React: null as unknown as typeof import("react"),
    stateIndex: 0,
    stateSeeds: new Map<number, unknown>(),
    setStateCalls: [] as Array<{ index: number; value: unknown }>,
    effects: [] as Array<() => unknown>,
    executed: [] as Array<() => unknown>,
    cleanups: [] as Array<() => void>,
    captures: [] as Captured[],
    hostElements: [] as HostElement[],
    editorSnapshot: null as {
      value: string;
      cursor: number;
      expandedCursor: number;
      terminalContextIds: string[];
    } | null,
    editorHandle: {
      focus: vi.fn(),
      focusAt: vi.fn(),
      focusAtEnd: vi.fn(),
      readSnapshot: vi.fn((): unknown => state.editorSnapshot),
    },
    pathSearch: {
      entries: [] as Array<{ path: string; kind: string }>,
      error: null as string | null,
      isPending: false,
    },
    capabilityCatalogs: [] as import("@coddy/contracts").ServerProvider[],
    capabilityOverride: undefined as
      | import("@coddy/contracts").ServerProviderCapabilities
      | null
      | undefined,
    capabilityPending: false,
    capabilityError: null as string | null,
    retryCapabilities: vi.fn(),
    traitInputs: [] as unknown[],
    isMobile: false,
    terminalSurfaceOpen: false,
    toastAdd: vi.fn(),
    recordHost(type: unknown, props: unknown) {
      if (typeof type === "string" && props && typeof props === "object") {
        state.hostElements.push({ type, props: props as Record<string, unknown> });
      }
    },
    mk(name: string, tag = "div") {
      const Component = (props: Record<string, unknown>) => {
        state.captures.push({ name, props });
        const R = state.React;
        const { children, render } = props as { children?: unknown; render?: unknown };
        const passthrough: Record<string, unknown> = { "data-mock": name };
        for (const key of Object.keys(props)) {
          if (key === "children" || key === "render" || key === "ref") continue;
          const value = props[key];
          if (
            typeof value === "string" ||
            typeof value === "number" ||
            typeof value === "boolean"
          ) {
            passthrough[`data-prop-${key.toLowerCase()}`] = String(value);
          }
        }
        if (props["aria-label"] !== undefined) {
          passthrough["aria-label"] = props["aria-label"];
        }
        if (render !== undefined && R.isValidElement(render)) {
          return children === undefined
            ? R.cloneElement(render as never, passthrough as never)
            : R.cloneElement(render as never, passthrough as never, children as never);
        }
        return R.createElement(tag, passthrough, children as never);
      };
      Component.displayName = name;
      return Component;
    },
  };
  return state;
});

// ---------------------------------------------------------------------------
// Partial react mock: seedable indexed useState, queued effects + handles.
// ---------------------------------------------------------------------------

vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();

  const useState = (initial?: unknown) => {
    const index = h.stateIndex;
    h.stateIndex += 1;
    const resolved = typeof initial === "function" ? (initial as () => unknown)() : initial;
    const value = h.stateSeeds.has(index) ? h.stateSeeds.get(index) : resolved;
    const setValue = (next: unknown) => {
      const applied =
        typeof next === "function" ? (next as (current: unknown) => unknown)(value) : next;
      h.setStateCalls.push({ index, value: applied });
    };
    return [value, setValue];
  };

  const queueEffect = (effect: () => unknown) => {
    h.effects.push(effect);
  };

  const useImperativeHandle = (ref: unknown, create: () => unknown) => {
    h.effects.push(() => {
      if (typeof ref === "function") {
        (ref as (value: unknown) => void)(create());
        return;
      }
      if (ref && typeof ref === "object") {
        (ref as { current: unknown }).current = create();
      }
    });
  };

  return {
    ...actual,
    useState: useState as typeof actual.useState,
    useEffect: queueEffect as typeof actual.useEffect,
    useLayoutEffect: queueEffect as typeof actual.useLayoutEffect,
    useImperativeHandle: useImperativeHandle as typeof actual.useImperativeHandle,
  };
});

// Tap the automatic JSX runtimes so host-element props (drag handlers, form
// submit, collapsed-mobile buttons) can be located and invoked directly.
vi.mock("react/jsx-runtime", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react/jsx-runtime")>();
  return {
    ...actual,
    jsx: ((type, props, key) => {
      h.recordHost(type, props);
      return actual.jsx(type, props, key);
    }) as typeof actual.jsx,
    jsxs: ((type, props, key) => {
      h.recordHost(type, props);
      return actual.jsxs(type, props, key);
    }) as typeof actual.jsxs,
  };
});

vi.mock("react/jsx-dev-runtime", async (importOriginal) => {
  const actual = (await importOriginal<Record<string, unknown>>()) as {
    jsxDEV?: (...args: unknown[]) => unknown;
  } & Record<string, unknown>;
  if (typeof actual["jsxDEV"] !== "function") {
    return actual;
  }
  const original = actual["jsxDEV"] as (...args: unknown[]) => unknown;
  return {
    ...actual,
    jsxDEV: (...args: unknown[]) => {
      h.recordHost(args[0], args[1]);
      return original(...args);
    },
  };
});

// ---------------------------------------------------------------------------
// UI primitives and heavy children replaced with capture-mocks.
// ---------------------------------------------------------------------------

vi.mock("../ui/separator", () => ({ Separator: h.mk("Separator", "span") }));
vi.mock("../ui/button", () => ({ Button: h.mk("Button", "button") }));
vi.mock("../ui/select", () => ({
  Select: h.mk("Select"),
  SelectItem: h.mk("SelectItem"),
  SelectPopup: h.mk("SelectPopup"),
  SelectTrigger: h.mk("SelectTrigger", "button"),
  SelectValue: h.mk("SelectValue", "span"),
}));
vi.mock("../ui/tooltip", () => ({
  Tooltip: h.mk("Tooltip"),
  TooltipPopup: h.mk("TooltipPopup"),
  TooltipTrigger: h.mk("TooltipTrigger"),
}));
vi.mock("../ui/toast", () => ({ toastManager: { add: h.toastAdd } }));

vi.mock("../ComposerPromptEditor", () => ({
  ComposerPromptEditor: (props: Record<string, unknown>) => {
    h.captures.push({ name: "ComposerPromptEditor", props });
    const editorRef = props["editorRef"] as { current: unknown } | null | undefined;
    if (editorRef && typeof editorRef === "object") {
      editorRef.current = h.editorHandle;
    }
    return h.React.createElement("div", {
      "data-mock": "composer-prompt-editor",
      "data-disabled": String(props["disabled"]),
      "data-placeholder": String(props["placeholder"]),
      "data-value": String(props["value"]),
      "data-editor-class": String(props["className"] ?? ""),
    });
  },
}));

vi.mock("./ProviderModelPicker", () => ({
  ProviderModelPicker: (props: Record<string, unknown>) => {
    h.captures.push({ name: "ProviderModelPicker", props });
    return h.React.createElement("div", {
      "data-mock": "provider-model-picker",
      "data-instance": String(props["activeInstanceId"]),
      "data-model": String(props["model"]),
      "data-open": String(props["open"]),
    });
  },
}));

vi.mock("./ComposerCommandMenu", () => ({
  ComposerCommandMenu: (props: Record<string, unknown>) => {
    h.captures.push({ name: "ComposerCommandMenu", props });
    const items = props["items"] as ReadonlyArray<{ id: string }>;
    return h.React.createElement("div", {
      "data-mock": "composer-command-menu",
      "data-count": String(items.length),
      "data-active": String(props["activeItemId"]),
      "data-loading": String(props["isLoading"]),
      "data-empty-text": String(props["emptyStateText"]),
    });
  },
}));

vi.mock("./ComposerPendingApprovalActions", () => ({
  ComposerPendingApprovalActions: h.mk("ComposerPendingApprovalActions"),
}));
vi.mock("./ComposerPrimaryActions", () => ({
  ComposerPrimaryActions: h.mk("ComposerPrimaryActions"),
}));
vi.mock("./ComposerPendingApprovalPanel", () => ({
  ComposerPendingApprovalPanel: h.mk("ComposerPendingApprovalPanel"),
}));
vi.mock("./ComposerPendingUserInputPanel", () => ({
  ComposerPendingUserInputPanel: h.mk("ComposerPendingUserInputPanel"),
}));
vi.mock("./ComposerPlanFollowUpBanner", () => ({
  ComposerPlanFollowUpBanner: h.mk("ComposerPlanFollowUpBanner"),
}));
vi.mock("./ComposerPendingElementContexts", () => ({
  ComposerPendingElementContexts: h.mk("ComposerPendingElementContexts"),
}));
vi.mock("./ComposerPendingReviewComments", () => ({
  ComposerPendingReviewComments: h.mk("ComposerPendingReviewComments"),
}));
vi.mock("./ComposerPreviewAnnotationCards", () => ({
  ComposerPreviewAnnotationCards: h.mk("ComposerPreviewAnnotationCards"),
}));
vi.mock("./ContextWindowMeter", () => ({ ContextWindowMeter: h.mk("ContextWindowMeter") }));
vi.mock("./McpStatusPopover", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./McpStatusPopover")>()),
  McpStatusPopover: h.mk("McpStatusPopover"),
}));

// ---------------------------------------------------------------------------
// State hooks and heavy state modules.
// ---------------------------------------------------------------------------

vi.mock("../../state/providerCapabilities", () => ({
  useProviderCapabilities: (target: { instanceId: string }) => {
    h.captures.push({ name: "useProviderCapabilities", props: { target } });
    const catalog = h.capabilityCatalogs.find(
      (provider) => provider.instanceId === target.instanceId,
    );
    return {
      data:
        h.capabilityOverride === undefined
          ? catalog
            ? { ...catalog, issues: [] }
            : null
          : h.capabilityOverride,
      isPending: h.capabilityPending,
      error: h.capabilityError,
      refresh: h.retryCapabilities,
      revalidateIfStale: vi.fn(),
    };
  },
}));

vi.mock("../../lib/composerPathSearchState", () => ({
  useComposerPathSearch: (target: unknown) => {
    h.captures.push({ name: "useComposerPathSearch", props: { target } });
    return h.pathSearch;
  },
}));

vi.mock("../../hooks/useMediaQuery", () => ({
  useMediaQuery: () => h.isMobile,
  useIsMobile: () => h.isMobile,
}));

vi.mock("../../terminalSurfaceState", () => ({
  useThreadHasTerminalSurface: () => h.terminalSurfaceOpen,
}));

// ChatView.logic imports the atom-creating threads module at top level; give
// it an inert stub so importing the composer stays side-effect free.
vi.mock("../../state/threads", () => ({
  environmentThreadDetails: { detailAtom: () => ({}) },
}));

vi.mock("./composerProviderState", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./composerProviderState")>();
  return {
    ...actual,
    renderComposerTraitControls: (
      input: Parameters<typeof actual.renderComposerTraitControls>[0],
    ) => {
      h.traitInputs.push(input);
      return actual.renderComposerTraitControls(input);
    },
  };
});

import { type ChatComposerHandle, type ChatComposerProps, ChatComposer } from "./ChatComposer";
import {
  type ComposerFileAttachment,
  type ComposerImageAttachment,
  useComposerDraftStore,
} from "../../composerDraftStore";
import {
  INLINE_TERMINAL_CONTEXT_PLACEHOLDER,
  type TerminalContextDraft,
} from "../../lib/terminalContext";
import type { ElementContextDraft } from "../../lib/elementContext";
import type { ReviewCommentContext } from "../../reviewCommentContext";
import type { PendingApproval, PendingUserInput } from "../../session-logic";
import type { Thread } from "../../types";

h.React = React;

// ---------------------------------------------------------------------------
// useState ordinal indexes inside ChatComposer (render order).
// ---------------------------------------------------------------------------

const STATE = {
  cursor: 0,
  trigger: 1,
  highlightedItemId: 2,
  highlightedSearchKey: 3,
  dragOver: 4,
  footerCompact: 5,
  primaryActionsCompact: 6,
  modelPickerOpen: 7,
  focused: 8,
} as const;

// ---------------------------------------------------------------------------
// Globals: window / document / DOM classes / FileReader.
// ---------------------------------------------------------------------------

class FakeNode {
  readonly isFakeNode = true;
}
class FakeElement extends FakeNode {
  closestResult: unknown = null;
  closest(): unknown {
    return this.closestResult;
  }
  contains(): boolean {
    return false;
  }
}
class FakeHTMLElement extends FakeElement {
  blur = vi.fn();
  focus = vi.fn();
}

const rafCallbacks: Array<(time: number) => void> = [];
const windowStub = {
  requestAnimationFrame: (callback: (time: number) => void): number => {
    rafCallbacks.push(callback);
    return rafCallbacks.length;
  },
  cancelAnimationFrame: vi.fn(),
};
const documentStub: { activeElement: unknown } = { activeElement: null };

function runAnimationFrames(): void {
  while (rafCallbacks.length > 0) {
    const batch = rafCallbacks.splice(0, rafCallbacks.length);
    for (const callback of batch) {
      callback(0);
    }
  }
}

let fileReaderShouldFail = false;
const fileReaderDelayByName = new Map<string, number>();
class FakeFileReader {
  result: string | null = null;
  error: Error | null = null;
  private listeners: Record<string, Array<() => void>> = {};
  addEventListener(type: string, listener: () => void): void {
    (this.listeners[type] ??= []).push(listener);
  }
  readAsDataURL(file: { type?: string; name?: string }): void {
    const complete = () => {
      if (fileReaderShouldFail) {
        this.error = new Error("read failed");
        for (const listener of this.listeners["error"] ?? []) listener();
        return;
      }
      this.result = `data:${file.type ?? "application/octet-stream"};base64,${file.name ?? ""}`;
      for (const listener of this.listeners["load"] ?? []) listener();
    };
    const delay = fileReaderDelayByName.get(file.name ?? "") ?? 0;
    if (delay > 0) {
      setTimeout(complete, delay);
    } else {
      complete();
    }
  }
}

const urlStatics = URL as unknown as {
  createObjectURL: ((blob: unknown) => string) | undefined;
  revokeObjectURL: ((url: string) => void) | undefined;
};
const realCreateObjectURL = urlStatics.createObjectURL;
const realRevokeObjectURL = urlStatics.revokeObjectURL;
let objectUrlCounter = 0;
const createObjectURLMock = vi.fn(() => `blob:generated-${(objectUrlCounter += 1)}`);
const revokeObjectURLMock = vi.fn();

// ---------------------------------------------------------------------------
// Effect flushing helpers.
// ---------------------------------------------------------------------------

function flushQueuedEffects(): void {
  while (h.effects.length > 0) {
    const pending = h.effects.splice(0, h.effects.length);
    for (const effect of pending) {
      h.executed.push(effect);
      const cleanup = effect();
      if (typeof cleanup === "function") {
        h.cleanups.push(cleanup as () => void);
      }
    }
  }
}

/** Re-run every executed effect: simulates a controlled re-render pass. */
function reflushExecutedEffects(): void {
  for (const effect of Array.from(h.executed)) {
    const cleanup = effect();
    if (typeof cleanup === "function") {
      h.cleanups.push(cleanup as () => void);
    }
  }
}

function runCleanups(): void {
  for (const cleanup of h.cleanups.splice(0, h.cleanups.length)) {
    cleanup();
  }
}

async function flushMicrotasks(): Promise<void> {
  await new Promise((resolve) => {
    setTimeout(resolve, 0);
  });
}

// ---------------------------------------------------------------------------
// Capture / host element lookup helpers.
// ---------------------------------------------------------------------------

function filterCaptures(name: string): Array<Record<string, unknown>> {
  return h.captures.filter((entry) => entry.name === name).map((entry) => entry.props);
}

function findCapture(
  name: string,
  predicate?: (props: Record<string, unknown>) => boolean,
): Record<string, unknown> {
  const found = h.captures.find(
    (entry) => entry.name === name && (predicate?.(entry.props) ?? true),
  )?.props;
  if (!found) throw new Error(`No captured "${name}" matched`);
  return found;
}

function lastCapture(name: string): Record<string, unknown> {
  const matches = filterCaptures(name);
  const found = matches[matches.length - 1];
  if (!found) throw new Error(`No captured "${name}"`);
  return found;
}

function captureByLabel(name: string, label: string): Record<string, unknown> {
  return findCapture(name, (props) => props["aria-label"] === label);
}

function findHost(
  predicate: (element: { type: string; props: Record<string, unknown> }) => boolean,
): { type: string; props: Record<string, unknown> } {
  const found = h.hostElements.find(predicate);
  if (!found) throw new Error("No host element matched");
  return found;
}

function hostByLabel(label: string): Record<string, unknown> {
  return findHost((element) => element.props["aria-label"] === label).props;
}

function setStateValues(index: number): unknown[] {
  return h.setStateCalls.filter((call) => call.index === index).map((call) => call.value);
}

// ---------------------------------------------------------------------------
// Fixtures.
// ---------------------------------------------------------------------------

const environmentId = EnvironmentId.make("environment-local");
const projectId = ProjectId.make("project-1");
const threadId = ThreadId.make("thread-1");
const threadRef = scopeThreadRef(environmentId, threadId);
const threadKey = scopedThreadKey(threadRef);
const codexInstanceId = ProviderInstanceId.make("codex");
const now = "2026-03-29T00:00:00.000Z";

const codexProvider: ServerProvider = {
  instanceId: codexInstanceId,
  driver: ProviderDriverKind.make("codex"),
  enabled: true,
  installed: true,
  version: "1.0.0",
  status: "ready",
  auth: { status: "authenticated" },
  checkedAt: now,
  models: [{ slug: "gpt-5.4", name: "GPT-5.4", isCustom: false, capabilities: null }],
  slashCommands: [{ name: "review", description: "Review the working tree" }],
  skills: [
    {
      name: "refactor",
      displayName: "Refactor",
      shortDescription: "Refactor code safely",
      description: "Long refactor description",
      path: "/skills/refactor",
      scope: "project",
      enabled: true,
      invocation: "dollar",
    },
    { name: "docs", path: "/skills/docs", enabled: true, invocation: "slash" },
  ],
  agents: [
    {
      name: "code-reviewer",
      description: "Review changes with a dedicated agent",
      model: "gpt-5.4",
      invocation: "mention",
    },
  ],
};

function makeThread(overrides: Partial<Thread> = {}): Thread {
  return {
    id: threadId,
    environmentId,
    projectId,
    title: "Demo Thread",
    modelSelection: { instanceId: codexInstanceId, model: "gpt-5.4" },
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

const proposedPlan: Thread["proposedPlans"][number] = {
  id: "plan-1",
  turnId: null,
  planMarkdown: "# Improve tests\n\n1. Write them",
  implementedAt: null,
  implementationThreadId: null,
  createdAt: now,
  updatedAt: now,
};

const pendingApproval: PendingApproval = {
  requestId: ApprovalRequestId.make("approval-1"),
  requestKind: "command",
  createdAt: now,
  detail: "Run pnpm test",
};

function makePendingUserInput(): PendingUserInput {
  return {
    requestId: ApprovalRequestId.make("input-1"),
    createdAt: now,
    questions: [
      {
        id: "q1",
        header: "Choose",
        question: "Pick one",
        options: [{ label: "A", description: "Option A" }],
        multiSelect: false,
      },
    ],
  };
}

function makePendingProgress(
  overrides: Partial<NonNullable<ChatComposerProps["activePendingProgress"]>> = {},
): NonNullable<ChatComposerProps["activePendingProgress"]> {
  return {
    questionIndex: 0,
    isLastQuestion: true,
    canAdvance: true,
    customAnswer: "",
    activeQuestion: { id: "q1", multiSelect: false },
    ...overrides,
  };
}

let imageCounter = 0;
function makeImage(overrides: Partial<ComposerImageAttachment> = {}): ComposerImageAttachment {
  imageCounter += 1;
  return {
    type: "image",
    id: `img-${imageCounter}`,
    name: "shot.png",
    mimeType: "image/png",
    sizeBytes: 4,
    previewUrl: `blob:existing-${imageCounter}`,
    file: new File([new Uint8Array([1, 2, 3, 4])], "shot.png", { type: "image/png" }),
    ...overrides,
  };
}

function makeFile(overrides: Partial<ComposerFileAttachment> = {}): ComposerFileAttachment {
  return {
    type: "file",
    id: "file-1",
    name: "notes.txt",
    mimeType: "text/plain",
    sizeBytes: 4,
    file: new File(["note"], "notes.txt", { type: "text/plain" }),
    ...overrides,
  };
}

function makeTerminalContext(id: string, text = "npm test output"): TerminalContextDraft {
  return {
    id,
    threadId,
    createdAt: now,
    terminalId: "term-1",
    terminalLabel: "Terminal 1",
    lineStart: 1,
    lineEnd: 3,
    text,
  };
}

function makeElementContext(id: string): ElementContextDraft {
  return {
    id,
    threadId,
    pickedAt: now,
    pageUrl: "http://localhost:3000/",
    pageTitle: "App",
    tagName: "button",
    selector: "#save",
    htmlPreview: '<button id="save">Save</button>',
    componentName: "SaveButton",
    source: null,
    styles: "",
  };
}

function makeReviewComment(id: string): ReviewCommentContext {
  return {
    id,
    sectionId: "section-1",
    sectionTitle: "src/app.ts",
    filePath: "src/app.ts",
    startIndex: 0,
    endIndex: 10,
    rangeLabel: "L1-L2",
    text: "Tighten this",
    diff: "+const a = 1;",
  };
}

const emptyKeybindings = [] as unknown as ResolvedKeybindingsConfig;

const draftStore = () => useComposerDraftStore.getState();
const draftOf = (ref: typeof threadRef) => draftStore().getComposerDraft(ref);

interface ResettableStore {
  getState: () => object;
  getInitialState: () => object;
  setState: (state: object, replace: true) => void;
}

const resettableComposerStore = useComposerDraftStore as unknown as ResettableStore;
const pristineComposerState = { ...resettableComposerStore.getInitialState() };

/**
 * renderToStaticMarkup reads zustand state through `getInitialState()` (the
 * server snapshot), so seeded state written with regular actions must be
 * copied into the initial-state object before rendering.
 */
function publishSeededStoreState(): void {
  Object.assign(resettableComposerStore.getInitialState(), resettableComposerStore.getState());
}

// ---------------------------------------------------------------------------
// Render harness.
// ---------------------------------------------------------------------------

function makeSpies() {
  return {
    onSend: vi.fn(),
    onInterrupt: vi.fn(),
    onImplementPlanInNewThread: vi.fn(),
    onRespondToApproval: vi.fn(() => Promise.resolve(undefined)),
    onSelectActivePendingUserInputOption: vi.fn(),
    onAdvanceActivePendingUserInput: vi.fn(),
    onPreviousActivePendingUserInputQuestion: vi.fn(),
    onChangeActivePendingUserInputCustomAnswer: vi.fn(),
    onProviderModelSelect: vi.fn(),
    getModelDisabledReason: vi.fn(() => null),
    toggleInteractionMode: vi.fn(),
    handleRuntimeModeChange: vi.fn(),
    handleInteractionModeChange: vi.fn(),
    togglePlanSidebar: vi.fn(),
    focusComposer: vi.fn(),
    scheduleComposerFocus: vi.fn(),
    setThreadError: vi.fn(),
    onExpandImage: vi.fn(),
  };
}

interface RenderResult {
  markup: string;
  props: ChatComposerProps;
  spies: ReturnType<typeof makeSpies>;
  composerRef: React.RefObject<ChatComposerHandle | null>;
  handle: () => ChatComposerHandle;
}

function renderComposer(overrides: Partial<ChatComposerProps> = {}): RenderResult {
  const spies = makeSpies();
  const composerRef: React.RefObject<ChatComposerHandle | null> = { current: null };
  const props: ChatComposerProps = {
    composerDraftTarget: threadRef,
    environmentId,
    routeKind: "server",
    routeThreadRef: threadRef,
    draftId: null,
    activeThreadId: threadId,
    activeThreadEnvironmentId: environmentId,
    activeThread: makeThread(),
    isServerThread: true,
    isLocalDraftThread: false,
    phase: "ready",
    isConnecting: false,
    isSendBusy: false,
    isPreparingWorktree: false,
    environmentUnavailable: null,
    activePendingApproval: null,
    pendingApprovals: [],
    pendingUserInputs: [],
    activePendingProgress: null,
    activePendingResolvedAnswers: null,
    activePendingIsResponding: false,
    activePendingDraftAnswers: {},
    activePendingQuestionIndex: 0,
    respondingRequestIds: [],
    showPlanFollowUpPrompt: false,
    activeProposedPlan: null,
    activePlan: null,
    sidebarProposedPlan: null,
    planSidebarLabel: "Plan",
    planSidebarOpen: false,
    runtimeMode: "approval-required",
    interactionMode: "default",
    lockedProvider: null,
    providerBindingInstanceId: codexInstanceId,
    lockProviderPickerToActiveInstance: false,
    providerBindingConflictReason: null,
    providerStatuses: [codexProvider],
    activeProjectDefaultModelSelection: { instanceId: codexInstanceId, model: "gpt-5.4" },
    activeThreadModelSelection: null,
    activeThreadActivities: [],
    resolvedTheme: "dark",
    settings: DEFAULT_UNIFIED_SETTINGS,
    keybindings: emptyKeybindings,
    gitCwd: "/repo",
    promptRef: { current: "" },
    composerAttachmentsRef: { current: [] },
    composerTerminalContextsRef: { current: [] },
    composerElementContextsRef: { current: [] },
    composerRef,
    onSend: spies.onSend,
    onInterrupt: spies.onInterrupt,
    onImplementPlanInNewThread: spies.onImplementPlanInNewThread,
    onRespondToApproval: spies.onRespondToApproval,
    onSelectActivePendingUserInputOption: spies.onSelectActivePendingUserInputOption,
    onAdvanceActivePendingUserInput: spies.onAdvanceActivePendingUserInput,
    onPreviousActivePendingUserInputQuestion: spies.onPreviousActivePendingUserInputQuestion,
    onChangeActivePendingUserInputCustomAnswer: spies.onChangeActivePendingUserInputCustomAnswer,
    onProviderModelSelect: spies.onProviderModelSelect,
    getModelDisabledReason: spies.getModelDisabledReason,
    toggleInteractionMode: spies.toggleInteractionMode,
    handleRuntimeModeChange: spies.handleRuntimeModeChange,
    handleInteractionModeChange: spies.handleInteractionModeChange,
    togglePlanSidebar: spies.togglePlanSidebar,
    focusComposer: spies.focusComposer,
    scheduleComposerFocus: spies.scheduleComposerFocus,
    setThreadError: spies.setThreadError,
    onExpandImage: spies.onExpandImage,
    ...overrides,
  };
  h.stateIndex = 0;
  h.captures.length = 0;
  h.hostElements.length = 0;
  h.traitInputs.length = 0;
  publishSeededStoreState();
  h.capabilityCatalogs = props.providerStatuses as ServerProvider[];
  const markup = renderToStaticMarkup(<ChatComposer {...props} />);
  flushQueuedEffects();
  return {
    markup,
    props,
    spies,
    composerRef,
    handle: () => {
      if (!composerRef.current) throw new Error("composer handle not attached");
      return composerRef.current;
    },
  };
}

function editorProps(): Record<string, unknown> {
  return lastCapture("ComposerPromptEditor");
}

type PromptChange = (
  nextPrompt: string,
  nextCursor: number,
  expandedCursor: number,
  cursorAdjacentToMention: boolean,
  terminalContextIds: string[],
) => void;

type CommandKey = (key: "ArrowDown" | "ArrowUp" | "Enter" | "Tab", event: KeyboardEvent) => boolean;

function keyEvent(overrides: Partial<{ shiftKey: boolean }> = {}): KeyboardEvent {
  return { shiftKey: false, ...overrides } as unknown as KeyboardEvent;
}

function setEditorSnapshot(value: string, cursor: number, terminalContextIds: string[] = []): void {
  h.editorSnapshot = { value, cursor, expandedCursor: cursor, terminalContextIds };
}

function seedPrompt(prompt: string): void {
  draftStore().setPrompt(threadRef, prompt);
}

interface FakeDragEventInit {
  types?: string[];
  files?: File[];
  relatedTarget?: unknown;
  containsRelated?: boolean;
}

function dragEvent(init: FakeDragEventInit = {}) {
  const dataTransfer = {
    types: init.types ?? ["Files"],
    files: init.files ?? [],
    dropEffect: "",
  };
  return {
    dataTransfer,
    preventDefault: vi.fn(),
    relatedTarget: init.relatedTarget ?? null,
    currentTarget: { contains: () => init.containsRelated ?? false },
  };
}

function pasteEvent(files: File[]) {
  return {
    clipboardData: { files },
    preventDefault: vi.fn(),
  } as unknown as React.ClipboardEvent<HTMLElement>;
}

// ---------------------------------------------------------------------------
// Lifecycle.
// ---------------------------------------------------------------------------

beforeEach(() => {
  vi.clearAllMocks();
  h.stateIndex = 0;
  h.stateSeeds.clear();
  h.setStateCalls.length = 0;
  h.effects.length = 0;
  h.executed.length = 0;
  h.cleanups.length = 0;
  h.captures.length = 0;
  h.hostElements.length = 0;
  h.editorSnapshot = null;
  h.capabilityOverride = undefined;
  h.capabilityPending = false;
  h.capabilityError = null;
  h.pathSearch = { entries: [], error: null, isPending: false };
  h.isMobile = false;
  h.terminalSurfaceOpen = false;
  rafCallbacks.length = 0;
  documentStub.activeElement = null;
  fileReaderShouldFail = false;
  fileReaderDelayByName.clear();
  resettableComposerStore.setState({ ...pristineComposerState }, true);
  Object.assign(resettableComposerStore.getInitialState(), pristineComposerState);
  vi.stubGlobal("window", windowStub);
  vi.stubGlobal("document", documentStub);
  vi.stubGlobal("Node", FakeNode);
  vi.stubGlobal("Element", FakeElement);
  vi.stubGlobal("HTMLElement", FakeHTMLElement);
  vi.stubGlobal("FileReader", FakeFileReader);
  urlStatics.createObjectURL = createObjectURLMock;
  urlStatics.revokeObjectURL = revokeObjectURLMock;
});

afterEach(() => {
  vi.unstubAllGlobals();
  urlStatics.createObjectURL = realCreateObjectURL;
  urlStatics.revokeObjectURL = realRevokeObjectURL;
});

// ---------------------------------------------------------------------------
// Rendering scenarios
// ---------------------------------------------------------------------------

describe("ChatComposer rendering", () => {
  it("keeps the composer border neutral for focused descendants", () => {
    renderComposer();
    const surface = findHost(
      (element) => element.props["data-chat-composer-mobile-collapsed"] !== undefined,
    ).props;
    const className = String(surface["className"]);

    expect(className).toContain("border-border");
    expect(className).not.toContain("has-focus-visible:border-ring");
    expect(surface["onFocusCapture"]).toBeTypeOf("function");
  });

  it("derives model-picker terminal context from supported terminal surfaces", () => {
    h.terminalSurfaceOpen = true;

    renderComposer();

    expect(findCapture("ProviderModelPicker")["terminalOpen"]).toBe(true);
  });

  it("renders the idle composer with editor, model picker, and runtime mode", () => {
    const { markup } = renderComposer();

    expect(markup).toContain('data-chat-composer-form="true"');
    expect(markup).toContain(
      'data-placeholder="Ask anything, @ files, : Coddy Studio actions, or a provider-native command"',
    );
    expect(markup).toContain('data-disabled="false"');
    expect(markup).toContain('data-instance="codex"');
    expect(markup).toContain('data-model="gpt-5.4"');
    expect(markup).toContain("Supervised");
    expect(markup).toContain("Auto-approve edits, ask before other actions.");
    expect(markup).not.toContain('data-mock="composer-command-menu"');
    expect(markup).toContain('data-mock="ContextWindowMeter"');
    expect(findCapture("ContextWindowMeter")).toMatchObject({ supported: false, usage: null });

    const select = findCapture("Select");
    expect(select["value"]).toBe("approval-required");
    const picker = findCapture("ProviderModelPicker");
    expect(picker["lockedProvider"]).toBeNull();
    expect(picker["lockToActiveInstance"]).toBe(false);

    // Path search targets nothing while no path trigger is active.
    const pathSearch = findCapture("useComposerPathSearch")["target"] as Record<string, unknown>;
    expect(pathSearch["cwd"]).toBeNull();
    expect(pathSearch["query"]).toBeNull();
  });

  it("shows MCP status for the selected provider instance and disables unsupported providers", () => {
    const activeThreadActivities = [
      {
        id: "activity-mcp" as Thread["activities"][number]["id"],
        tone: "info",
        kind: "provider-event",
        summary: "mcp.status.updated",
        payload: {
          providerInstanceId: "codex",
          servers: [{ name: "context7", state: "connected" }],
        },
        turnId: null,
        createdAt: now,
      },
    ] as Thread["activities"];

    renderComposer({
      providerStatuses: [{ ...codexProvider, supportsMcpStatus: true }],
      activeThreadActivities,
    });

    expect(findCapture("McpStatusPopover")).toMatchObject({
      supported: true,
      snapshot: {
        servers: [{ name: "context7", state: "connected", detail: null }],
      },
    });

    h.captures.length = 0;
    const personalInstanceId = ProviderInstanceId.make("codex_personal");
    renderComposer({
      providerStatuses: [
        { ...codexProvider, supportsMcpStatus: true },
        { ...codexProvider, instanceId: personalInstanceId, supportsMcpStatus: true },
      ],
      providerBindingInstanceId: personalInstanceId,
      activeProjectDefaultModelSelection: { instanceId: personalInstanceId, model: "gpt-5.4" },
      activeThreadActivities: [
        ...activeThreadActivities,
        {
          ...activeThreadActivities[0]!,
          id: "activity-mcp-personal" as Thread["activities"][number]["id"],
          payload: {
            providerInstanceId: "codex_personal",
            servers: [{ name: "personal-server", state: "starting" }],
          },
        },
      ],
    });
    expect(findCapture("McpStatusPopover")["snapshot"]).toEqual({
      servers: [{ name: "personal-server", state: "starting", detail: null }],
    });

    h.captures.length = 0;
    renderComposer({ providerStatuses: [codexProvider], activeThreadActivities });
    expect(filterCaptures("McpStatusPopover")).toHaveLength(1);
    expect(findCapture("McpStatusPopover")["supported"]).toBe(false);
  });

  it("uses a provider-family lock without fixing the active instance", () => {
    renderComposer({ lockedProvider: ProviderDriverKind.make("codex") });

    const picker = findCapture("ProviderModelPicker");
    expect(picker["lockedProvider"]).toBe("codex");
    expect(picker["lockToActiveInstance"]).toBe(false);
  });

  it("presents a raw Claude Ultrathink session default before the first send", () => {
    const claudeInstanceId = ProviderInstanceId.make("claudeAgent");
    const claudeModel = {
      slug: "claude-sonnet-5",
      name: "Claude Sonnet 5",
      isCustom: false,
      capabilities: {
        optionDescriptors: [
          {
            id: "effort",
            label: "Effort",
            type: "select" as const,
            options: [
              { id: "high", label: "High", isDefault: true },
              { id: "ultrathink", label: "Ultrathink" },
            ],
            promptInjectedValues: ["ultrathink"],
          },
        ],
      },
    };
    const claudeProvider: ServerProvider = {
      ...codexProvider,
      instanceId: claudeInstanceId,
      driver: ProviderDriverKind.make("claudeAgent"),
      models: [claudeModel],
    };
    const rawDefault = {
      instanceId: claudeInstanceId,
      model: claudeModel.slug,
      options: [{ id: "effort", value: "ultrathink" }],
    };

    const { markup, handle } = renderComposer({
      providerStatuses: [claudeProvider],
      providerBindingInstanceId: claudeInstanceId,
      activeProjectDefaultModelSelection: null,
      activeThreadModelSelection: rawDefault,
    });

    expect(markup).toContain("ultrathink-frame");
    expect(markup).toContain("shadow-[0_0_0_1px_rgba(255,255,255,0.07)_inset]");
    expect(findCapture("ProviderModelPicker")["activeProviderIconClassName"]).toBe(
      "ultrathink-chroma",
    );
    expect(handle().getSendContext()).toMatchObject({
      selectedPromptEffort: "ultrathink",
      selectedModelOptionsForDispatch: [{ id: "effort", value: "high" }],
    });
  });

  it("disables the editor while connecting and when the environment is unavailable", () => {
    renderComposer({ isConnecting: true });
    expect(editorProps()["disabled"]).toBe(true);

    const connection: EnvironmentConnectionPresentation = {
      phase: "offline",
      error: null,
      traceId: null,
    };
    const { markup } = renderComposer({
      environmentUnavailable: { label: "Laptop", connection },
    });
    expect(markup).toContain('data-placeholder="Laptop is not connected"');
    expect(markup).toContain("opacity-75");
    expect(editorProps()["disabled"]).toBe(true);
  });

  it("names the unavailable environment without repeating the banner's reason", () => {
    const reason =
      "devbox rejected a new pairing credential. Connect again; if it keeps failing, remove the environment and add it again.";
    const { markup } = renderComposer({
      environmentUnavailable: {
        label: "devbox",
        connection: { phase: "error", error: reason, traceId: null },
      },
    });

    expect(markup).toContain('data-placeholder="devbox is not connected"');
    expect(markup).not.toContain("rejected a new pairing credential");
  });

  it("shows the disconnected placeholder", () => {
    const { markup } = renderComposer({ phase: "disconnected" });
    expect(markup).toContain('data-placeholder="Ask for follow-up changes or attach files"');
  });

  it("renders the approval header, empties the editor, and swaps the footer", () => {
    seedPrompt("hidden while approving");
    const { markup } = renderComposer({
      activePendingApproval: pendingApproval,
      pendingApprovals: [pendingApproval],
      respondingRequestIds: [pendingApproval.requestId],
    });

    expect(markup).toContain('data-mock="ComposerPendingApprovalPanel"');
    expect(markup).toContain('data-mock="ComposerPendingApprovalActions"');
    expect(markup).not.toContain('data-mock="provider-model-picker"');
    expect(editorProps()["value"]).toBe("");
    expect(editorProps()["placeholder"]).toBe("Run pnpm test");

    const actions = findCapture("ComposerPendingApprovalActions");
    expect(actions["isResponding"]).toBe(true);
    expect(captureByLabel("Button", "Attach files")["disabled"]).toBe(true);
    const picker = findHost(
      (element) => element.type === "input" && element.props["type"] === "file",
    ).props;
    expect(picker["hidden"]).toBe(true);
    expect(picker["disabled"]).toBe(true);
  });

  it("falls back to the generic approval placeholder without a detail", () => {
    const { detail: _detail, ...approvalWithoutDetail } = pendingApproval;
    renderComposer({
      activePendingApproval: approvalWithoutDetail,
      pendingApprovals: [pendingApproval],
    });
    expect(editorProps()["placeholder"]).toBe("Resolve this approval request to continue");
  });

  it("renders the pending user input panel and custom answer editor", () => {
    const { markup } = renderComposer({
      pendingUserInputs: [makePendingUserInput()],
      activePendingProgress: makePendingProgress({ customAnswer: "my answer" }),
    });

    expect(markup).toContain('data-mock="ComposerPendingUserInputPanel"');
    expect(markup).not.toContain('data-mock="ContextWindowMeter"');
    expect(editorProps()["value"]).toBe("my answer");
    expect(editorProps()["placeholder"]).toBe(
      "Type your own answer, or leave this blank to use the selected option",
    );
    // Terminal contexts are suppressed while questions are pending.
    expect(editorProps()["terminalContexts"]).toEqual([]);
    expect(captureByLabel("Button", "Attach files")["disabled"]).toBe(true);
  });

  it("renders the plan follow-up banner with the extracted plan title", () => {
    const { markup } = renderComposer({
      showPlanFollowUpPrompt: true,
      activeProposedPlan: proposedPlan,
    });

    expect(markup).toContain('data-mock="ComposerPlanFollowUpBanner"');
    const banner = findCapture("ComposerPlanFollowUpBanner");
    expect(banner["planTitle"]).toBe("Improve tests");
    expect(editorProps()["placeholder"]).toBe(
      "Add feedback to refine the plan, or leave this blank to implement it",
    );
  });

  it("shows the plan sidebar toggle and forwards toggle clicks", () => {
    const { markup, spies } = renderComposer({ planSidebarOpen: true });
    expect(markup).toContain("Plan");

    const toggle = captureByLabel("Button", "Hide plan sidebar");
    expect(toggle["variant"]).toBe("ghost");
    expect(toggle["className"]).toContain("bg-foreground/10");
    expect(toggle["className"]).toContain("dark:bg-foreground/14");
    expect(toggle["className"]).toContain("text-foreground");
    (toggle["onClick"] as () => void)();
    expect(spies.togglePlanSidebar).toHaveBeenCalledTimes(1);
  });

  it("renders the folded-map Plan toggle without Build UI", () => {
    const { markup, spies } = renderComposer();

    expect(markup).not.toContain(">Build<");
    const toggle = captureByLabel("Button", "Enable plan mode");
    (toggle["onClick"] as () => void)();
    expect(spies.toggleInteractionMode).toHaveBeenCalledOnce();

    renderComposer({ interactionMode: "plan" });
    const activePlan = captureByLabel("Button", "Disable plan mode");
    expect(activePlan["aria-pressed"]).toBe(true);
    expect(activePlan["variant"]).toBe("ghost");
    expect(activePlan["className"]).toContain("bg-foreground/10");
    expect(activePlan["className"]).toContain("dark:bg-foreground/14");
    expect(activePlan["className"]).toContain("text-foreground");
  });

  it("keeps runtime controls icon-only at compact and regular widths", () => {
    const { spies } = renderComposer({ runtimeMode: "auto-accept-edits" });

    const runtimeTrigger = captureByLabel("SelectTrigger", "Auto-accept edits");
    expect(runtimeTrigger["className"]).not.toContain("bg-primary");
    expect(runtimeTrigger["className"]).not.toContain("bg-foreground/");
    expect(runtimeTrigger["className"]).toContain("text-foreground/80");
    expect(filterCaptures("SelectValue")).toHaveLength(0);

    const select = findCapture("Select");
    (select["onValueChange"] as (value: string) => void)("full-access");
    expect(spies.handleRuntimeModeChange).toHaveBeenCalledWith("full-access");

    const hidden = renderComposer({
      providerStatuses: [{ ...codexProvider, showInteractionModeToggle: false }],
    });
    expect(hidden.markup).not.toContain("Enable plan mode");
    const unavailablePlan = findCapture("Button", (props) =>
      String(props["aria-label"]).startsWith("Plan mode is not supported"),
    );
    expect(unavailablePlan["aria-disabled"]).toBe(true);
    (unavailablePlan["onClick"] as () => void)();
    expect(hidden.spies.toggleInteractionMode).not.toHaveBeenCalled();
  });

  it("uses destructive icon treatment only while Full access is selected", () => {
    const fullAccess = renderComposer({ runtimeMode: "full-access" });
    const fullAccessTrigger = captureByLabel("SelectTrigger", "Full access");
    const fullAccessItem = findCapture("SelectItem", (props) => props["value"] === "full-access");

    expect(String(fullAccessTrigger["className"])).toContain("[&_svg]:text-destructive");
    expect(filterCaptures("SelectValue")).toHaveLength(0);
    expect(String(fullAccessItem["className"])).not.toContain("text-destructive");
    expect(fullAccess.markup).toContain("text-destructive");
    expect(fullAccess.markup).toContain(
      "inline-flex items-center gap-1.5 font-medium text-foreground",
    );
    expect(fullAccess.markup).toContain("Allow commands and edits without prompts.");

    const supervised = renderComposer({ runtimeMode: "approval-required" });
    const supervisedTrigger = captureByLabel("SelectTrigger", "Supervised");
    expect(String(supervisedTrigger["className"])).not.toContain("[&_svg]:text-destructive");
    expect(supervised.markup).not.toContain("text-destructive");
  });

  it("keeps mode controls left and send actions fixed when the footer is compact", () => {
    h.stateSeeds.set(STATE.footerCompact, true);
    h.stateSeeds.set(STATE.primaryActionsCompact, true);
    const { markup, spies } = renderComposer({ planSidebarOpen: true });

    expect(markup).toContain('data-chat-composer-footer-compact="true"');
    expect(markup).toContain('data-chat-composer-actions="right"');
    const toggle = captureByLabel("Button", "Enable plan mode");
    (toggle["onClick"] as () => void)();
    expect(spies.toggleInteractionMode).toHaveBeenCalledTimes(1);

    const primary = findCapture("ComposerPrimaryActions");
    expect(primary["compact"]).toBe(true);
  });

  it("passes measured context capability and orders MCP before context and primary actions", () => {
    const activities = [
      {
        id: "activity-context" as Thread["activities"][number]["id"],
        tone: "info",
        kind: "context-window.updated",
        summary: "context.window.updated",
        payload: { usedTokens: 50, maxTokens: 100 },
        turnId: null,
        createdAt: now,
      },
      {
        id: "activity-mcp" as Thread["activities"][number]["id"],
        tone: "info",
        kind: "provider-event",
        summary: "mcp.status.updated",
        payload: { providerInstanceId: "codex", servers: [] },
        turnId: null,
        createdAt: now,
      },
    ] as Thread["activities"];

    renderComposer({
      providerStatuses: [
        { ...codexProvider, supportsMcpStatus: true, supportsContextWindowUsage: true },
      ],
      activeThreadActivities: activities,
    });

    expect(findCapture("ContextWindowMeter")["supported"]).toBe(true);
    expect(findCapture("ContextWindowMeter")["usage"]).toMatchObject({
      usedTokens: 50,
      maxTokens: 100,
      usedPercentage: 50,
    });
    const attachmentIndex = h.captures.findIndex(
      (capture) => capture.name === "Button" && capture.props["aria-label"] === "Attach files",
    );
    const contextIndex = h.captures.findIndex((capture) => capture.name === "ContextWindowMeter");
    const mcpIndex = h.captures.findIndex((capture) => capture.name === "McpStatusPopover");
    const primaryIndex = h.captures.findIndex(
      (capture) => capture.name === "ComposerPrimaryActions",
    );

    expect(attachmentIndex).toBeGreaterThanOrEqual(0);
    expect(attachmentIndex).toBeLessThan(contextIndex);
    expect(mcpIndex).toBeLessThan(contextIndex);
    expect(contextIndex).toBeLessThan(primaryIndex);
  });

  it("passes the thread instance's picker label to the context meter", () => {
    const instanceId = ProviderInstanceId.make("codex_personal");
    const modelSelection = { instanceId, model: "gpt-5.4" };
    renderComposer({
      activeThread: makeThread({ modelSelection }),
      activeThreadModelSelection: modelSelection,
      providerBindingInstanceId: instanceId,
      providerStatuses: [
        { ...codexProvider, displayName: "Default Codex" },
        { ...codexProvider, instanceId, displayName: "Stale Personal Name" },
      ],
      settings: {
        ...DEFAULT_UNIFIED_SETTINGS,
        providerInstances: {
          [instanceId]: { driver: codexProvider.driver, displayName: "Codex Personal" },
        },
      },
    });

    expect(findCapture("ContextWindowMeter")["providerDisplayName"]).toBe("Codex Personal");
  });

  it.each([
    ["Cursor", "cursor"],
    ["Grok", "grok"],
    ["OpenCode", "opencode"],
  ])("passes an unavailable context meter for %s", (_displayName, driver) => {
    const instanceId = ProviderInstanceId.make(driver);
    renderComposer({
      providerBindingInstanceId: instanceId,
      providerStatuses: [
        {
          ...codexProvider,
          instanceId,
          driver: ProviderDriverKind.make(driver),
        },
      ],
      activeProjectDefaultModelSelection: { instanceId, model: "gpt-5.4" },
    });

    expect(filterCaptures("ContextWindowMeter")).toHaveLength(1);
    expect(findCapture("ContextWindowMeter")).toMatchObject({ supported: false, usage: null });
    expect(filterCaptures("McpStatusPopover")).toHaveLength(1);
    expect(findCapture("McpStatusPopover")["supported"]).toBe(false);
  });

  it.each([
    ["Codex", "codex"],
    ["Claude", "claudeAgent"],
  ])("passes an awaiting context meter for %s", (_displayName, driver) => {
    const instanceId = ProviderInstanceId.make(driver);
    renderComposer({
      providerBindingInstanceId: instanceId,
      providerStatuses: [
        {
          ...codexProvider,
          instanceId,
          driver: ProviderDriverKind.make(driver),
          supportsContextWindowUsage: true,
          supportsMcpStatus: true,
        },
      ],
      activeProjectDefaultModelSelection: { instanceId, model: "gpt-5.4" },
    });

    expect(filterCaptures("ContextWindowMeter")).toHaveLength(1);
    expect(findCapture("ContextWindowMeter")).toMatchObject({ supported: true, usage: null });
    expect(filterCaptures("McpStatusPopover")).toHaveLength(1);
    expect(findCapture("McpStatusPopover")["supported"]).toBe(true);
  });

  it("keeps context usage unavailable when an unsupported selected provider has stale activity", () => {
    const cursorInstanceId = ProviderInstanceId.make("cursor");
    renderComposer({
      providerBindingInstanceId: cursorInstanceId,
      providerStatuses: [
        {
          ...codexProvider,
          instanceId: cursorInstanceId,
          driver: ProviderDriverKind.make("cursor"),
        },
      ],
      activeProjectDefaultModelSelection: { instanceId: cursorInstanceId, model: "gpt-5.4" },
      activeThreadActivities: [
        {
          id: "stale-context" as Thread["activities"][number]["id"],
          tone: "info",
          kind: "context-window.updated",
          summary: "context.window.updated",
          payload: { usedTokens: 50, maxTokens: 100 },
          turnId: null,
          createdAt: now,
        },
      ],
    });

    expect(findCapture("ContextWindowMeter")["supported"]).toBe(false);
    expect(findCapture("ContextWindowMeter")["usage"]).toMatchObject({ usedTokens: 50 });
  });

  it("shows the preparing worktree hint", () => {
    const { markup } = renderComposer({ isPreparingWorktree: true });
    expect(markup).toContain("Preparing worktree...");
  });

  it("forwards interrupt and plan implementation primary actions", () => {
    const { spies } = renderComposer({ phase: "running", showPlanFollowUpPrompt: true });
    const actions = lastCapture("ComposerPrimaryActions");

    (actions["onInterrupt"] as () => void)();
    (actions["onImplementPlanInNewThread"] as () => void)();

    expect(spies.onInterrupt).toHaveBeenCalledOnce();
    expect(spies.onImplementPlanInNewThread).toHaveBeenCalledOnce();
  });
});

// ---------------------------------------------------------------------------
// Attachments
// ---------------------------------------------------------------------------

describe("ChatComposer attachments", () => {
  it("selects mixed attachments through the paperclip picker", () => {
    const { markup } = renderComposer();
    const picker = captureByLabel("Button", "Attach files");
    const input = findHost(
      (element) => element.type === "input" && element.props["type"] === "file",
    ).props;
    const image = new File(["png"], "shot.png", { type: "image/png" });
    const file = new File(["note"], "notes.txt", { type: "text/plain" });

    (picker["onClick"] as () => void)();
    (input["onChange"] as (event: unknown) => void)({
      currentTarget: { files: [image, file], value: "picked" },
    });

    expect(draftOf(threadRef)?.attachments.map((attachment) => attachment.type)).toEqual([
      "image",
      "file",
    ]);
    expect(markup).toContain('aria-label="Attach files"');
  });

  it("renders a file chip without an image preview", () => {
    draftStore().addAttachment(threadRef, makeFile());
    const { markup } = renderComposer();

    expect(markup).toContain("notes.txt");
    expect(markup).toContain("4 B");
    expect(markup).toContain('aria-label="Remove notes.txt"');
    expect(markup).not.toContain('aria-label="Preview notes.txt"');
  });

  it("renders image previews, remove buttons, and non-persisted warnings", async () => {
    const withPreview = makeImage({ id: "img-a", name: "shot.png" });
    const withoutPreview = makeImage({ id: "img-b", name: "plain.png", previewUrl: "" });
    draftStore().addAttachments(threadRef, [withPreview, withoutPreview]);
    useComposerDraftStore.setState((state) => ({
      draftsByThreadKey: {
        ...state.draftsByThreadKey,
        [threadKey]: {
          ...state.draftsByThreadKey[threadKey]!,
          nonPersistedAttachmentIds: ["img-a"],
        },
      },
    }));

    const { markup, spies } = renderComposer();
    await flushMicrotasks();

    expect(markup).toContain('aria-label="Preview shot.png"');
    expect(markup).toContain("plain.png");
    expect(markup).toContain("Draft attachment may not persist");

    // Preview click resolves the expanded preview from previewable images.
    const previewButton = hostByLabel("Preview shot.png");
    (previewButton["onClick"] as () => void)();
    expect(spies.onExpandImage).toHaveBeenCalledWith({
      images: [{ src: withPreview.previewUrl, name: "shot.png" }],
      index: 0,
    });

    // Remove click deletes the image from the draft store.
    const removeButton = captureByLabel("Button", "Remove shot.png");
    (removeButton["onClick"] as () => void)();
    expect(draftOf(threadRef)?.attachments.map((attachment) => attachment.id)).toEqual(["img-b"]);

    // The persist effect staged data urls through the FileReader stub; the
    // store's verification pass then strips them again because nothing ever
    // reaches localStorage in this environment, marking images non-persisted.
    expect(draftOf(threadRef)?.persistedAttachments).toEqual([]);
    expect(draftOf(threadRef)?.nonPersistedAttachmentIds).toEqual(["img-b"]);
  });

  it("restages existing persisted attachments when reading a file fails", async () => {
    const image = makeImage({ id: "img-keep" });
    draftStore().addAttachments(threadRef, [image]);
    draftStore().syncPersistedAttachments(threadRef, [
      {
        type: "image",
        id: "img-keep",
        name: image.name,
        mimeType: image.mimeType,
        sizeBytes: image.sizeBytes,
        dataUrl: "data:image/png;base64,old",
      },
    ]);
    fileReaderShouldFail = true;

    renderComposer();
    await flushMicrotasks();

    // The read failure falls back to the previously staged attachment; the
    // storage verification pass then reports it as non-persisted (no real
    // localStorage here), so the image survives while the staging is cleared.
    expect(draftOf(threadRef)?.attachments.map((entry) => entry.id)).toEqual(["img-keep"]);
    expect(draftOf(threadRef)?.persistedAttachments).toEqual([]);
    expect(draftOf(threadRef)?.nonPersistedAttachmentIds).toEqual(["img-keep"]);
  });

  it("persists mixed attachments in draft order when reads resolve out of order", async () => {
    const image = makeImage({ id: "slow-image", name: "slow.png" });
    const file = makeFile({ id: "fast-file", name: "fast.txt" });
    draftStore().addAttachments(threadRef, [image, file]);
    fileReaderDelayByName.set("slow.png", 10);
    const syncPersistedAttachments = vi.spyOn(draftStore(), "syncPersistedAttachments");

    renderComposer();
    await new Promise((resolve) => setTimeout(resolve, 20));

    expect(syncPersistedAttachments).toHaveBeenCalledWith(
      threadRef,
      expect.arrayContaining([
        expect.objectContaining({ name: "slow.png" }),
        expect.objectContaining({ name: "fast.txt" }),
      ]),
    );
    expect(
      syncPersistedAttachments.mock.calls.at(-1)?.[1]?.map((attachment) => attachment.name),
    ).toEqual(["slow.png", "fast.txt"]);
  });

  it("renders element contexts, review comments, and preview annotations with working removal", () => {
    draftStore().setElementContexts(threadRef, [makeElementContext("el-1")]);
    draftStore().setReviewComments(threadRef, [makeReviewComment("rc-1")]);
    draftStore().setPreviewAnnotations(threadRef, [
      {
        id: "ann-1",
        pageUrl: "http://localhost:3000/",
        pageTitle: null,
        comment: "Make it blue",
        elements: [],
        regions: [],
        strokes: [],
        styleChanges: [],
        screenshot: null,
        createdAt: now,
      },
    ]);

    const { markup, spies } = renderComposer();

    expect(markup).toContain('data-mock="ComposerPendingElementContexts"');
    expect(markup).toContain('data-mock="ComposerPendingReviewComments"');
    expect(markup).toContain('data-mock="ComposerPreviewAnnotationCards"');

    (findCapture("ComposerPendingElementContexts")["onRemove"] as (id: string) => void)("el-1");
    expect(draftOf(threadRef)?.elementContexts).toEqual([]);

    (findCapture("ComposerPendingReviewComments")["onRemove"] as (id: string) => void)("rc-1");
    expect(draftOf(threadRef)?.reviewComments).toEqual([]);

    const annotationCards = findCapture("ComposerPreviewAnnotationCards");
    (annotationCards["onExpandImage"] as (id: string) => void)("missing-image");
    expect(spies.onExpandImage).not.toHaveBeenCalled();
    (annotationCards["onRemove"] as (id: string) => void)("ann-1");
    // Removing the last annotation empties the draft, which the store drops.
    expect(draftOf(threadRef)?.previewAnnotations ?? []).toEqual([]);
  });

  it("preserves existing contexts and attachments while a provider conflict is active", () => {
    draftStore().addAttachments(threadRef, [makeFile()]);
    draftStore().setTerminalContexts(threadRef, [makeTerminalContext("ctx-locked")]);
    draftStore().setElementContexts(threadRef, [makeElementContext("el-locked")]);
    seedPrompt(`${INLINE_TERMINAL_CONTEXT_PLACEHOLDER} keep this`);
    renderComposer({
      providerBindingConflictReason: "Provider metadata conflicts with the active session.",
    });

    (editorProps()["onChange"] as PromptChange)("replace this", 12, 12, false, []);
    (editorProps()["onRemoveTerminalContext"] as (id: string) => void)("ctx-locked");
    (findCapture("ComposerPendingElementContexts")["onRemove"] as (id: string) => void)(
      "el-locked",
    );
    const attachmentButton = captureByLabel("Button", "Remove notes.txt");
    (attachmentButton["onClick"] as () => void)();

    const draft = draftOf(threadRef);
    expect(draft?.prompt).toBe(`${INLINE_TERMINAL_CONTEXT_PLACEHOLDER} keep this`);
    expect(draft?.terminalContexts.map((context) => context.id)).toEqual(["ctx-locked"]);
    expect(draft?.elementContexts.map((context) => context.id)).toEqual(["el-locked"]);
    expect(draft?.attachments.map((attachment) => attachment.name)).toEqual(["notes.txt"]);
  });

  it("expands the image attached to a preview annotation", () => {
    draftStore().addAttachments(threadRef, [makeImage({ id: "ann-image" })]);
    draftStore().setPreviewAnnotations(threadRef, [
      {
        id: "ann-image",
        pageUrl: "http://localhost:3000/",
        pageTitle: "Preview",
        comment: "Inspect this",
        elements: [],
        regions: [],
        strokes: [],
        styleChanges: [],
        screenshot: null,
        createdAt: now,
      },
    ]);

    const { spies } = renderComposer();
    const annotationCards = findCapture("ComposerPreviewAnnotationCards");
    (annotationCards["onExpandImage"] as (id: string) => void)("ann-image");

    expect(spies.onExpandImage).toHaveBeenCalledWith(
      expect.objectContaining({ index: 0, images: expect.any(Array) }),
    );
  });
});

// ---------------------------------------------------------------------------
// Command menu
// ---------------------------------------------------------------------------

describe("ChatComposer command menu", () => {
  it("uses the workspace catalog for both user skill suggestions and inline metadata", () => {
    const personal = {
      name: "personal",
      path: "/home/me/.agents/skills/personal/SKILL.md",
      enabled: true,
      invocation: "dollar" as const,
      scope: "user",
    };
    h.capabilityOverride = { slashCommands: [], agents: [], issues: [], skills: [personal] };
    seedPrompt("$");
    renderComposer({ gitCwd: "/another-worktree" });
    expect(findCapture("useProviderCapabilities")["target"]).toMatchObject({
      environmentId,
      instanceId: codexInstanceId,
      cwd: "/another-worktree",
    });
    const items = findCapture("ComposerCommandMenu")["items"] as Array<Record<string, unknown>>;
    expect(items.map((item) => item["label"])).toEqual(["$personal"]);
    expect(editorProps()["skills"]).toEqual([personal]);
  });

  it("keeps the skill menu and draft available during loading and failed discovery", () => {
    h.capabilityOverride = null;
    h.capabilityPending = true;
    seedPrompt("$personal");
    renderComposer();
    expect(findCapture("ComposerCommandMenu")).toMatchObject({
      isLoading: true,
      loadingText: "Loading provider skills...",
    });
    h.capabilityPending = false;
    h.capabilityError = "Could not load provider skills.";
    renderComposer();
    const menu = findCapture("ComposerCommandMenu");
    expect(menu["issueText"]).toBe(h.capabilityError);
    (menu["onRetry"] as () => void)();
    expect(h.retryCapabilities).toHaveBeenCalledOnce();
    expect(draftOf(threadRef)?.prompt).toBe("$personal");
    expect(editorProps()["skills"]).toEqual([]);
  });

  it("builds native file items from workspace entries while a reference trigger is active", () => {
    seedPrompt("hello @src");
    h.pathSearch = {
      entries: [
        { path: "src/app/main.ts", kind: "file" },
        { path: "src/app", kind: "directory" },
      ],
      error: null,
      isPending: true,
    };

    const { markup } = renderComposer();

    expect(markup).toContain('data-mock="composer-command-menu"');
    const menu = findCapture("ComposerCommandMenu");
    const items = menu["items"] as Array<Record<string, unknown>>;
    expect(items).toHaveLength(2);
    expect(
      items.find((item) => item["type"] === "file-reference" && item["pathKind"] === "file"),
    ).toMatchObject({
      id: "file-reference:file:src/app/main.ts",
      type: "file-reference",
      label: "main.ts",
      description: "src/app",
      replacement: "@src/app/main.ts ",
    });
    expect(menu["isLoading"]).toBe(true);
    expect(menu["emptyStateText"]).toBe("No matching files or agents.");

    // The path search hook received the trigger query and git cwd.
    const target = findCapture("useComposerPathSearch")["target"] as Record<string, unknown>;
    expect(target["cwd"]).toBe("/repo");
    expect(target["query"]).toBe("src");

    // The highlight sync effect resolved the first item.
    expect(setStateValues(STATE.highlightedItemId)).toContain("file-reference:directory:src/app");
    expect(setStateValues(STATE.highlightedSearchKey)).toContain("provider-reference:src");
  });

  it("lists only Coddy Studio actions for colon", () => {
    seedPrompt(":");
    renderComposer();

    const menu = findCapture("ComposerCommandMenu");
    const items = menu["items"] as Array<Record<string, unknown>>;
    expect(items.map((item) => item["label"])).toEqual([":model", ":plan", ":default"]);
    expect(items.every((item) => item["type"] === "coddy-studio-action")).toBe(true);
    expect(menu["emptyStateText"]).toBe("No matching Coddy Studio action.");
  });

  it("keeps provider commands and slash skills under slash", () => {
    seedPrompt("/");
    renderComposer();

    const menu = findCapture("ComposerCommandMenu");
    const items = menu["items"] as Array<Record<string, unknown>>;
    expect(items.map((item) => item["label"])).toEqual(["/review", "/docs"]);
    expect(items.map((item) => item["label"])).not.toContain(":plan");
    expect(items.some((item) => item["type"] === "agent-reference")).toBe(false);
    expect(menu["emptyStateText"]).toBe("No matching provider command or skill.");
  });

  it("keeps an empty native skill menu reachable without changing dollar text", () => {
    const unsupportedProvider = {
      ...codexProvider,
      slashCommands: [],
      skills: [],
      agents: [],
    };
    seedPrompt("$ordinary");

    renderComposer({ providerStatuses: [unsupportedProvider] });

    expect(findCapture("ComposerCommandMenu")["items"]).toEqual([]);
    expect(draftOf(threadRef)?.prompt).toBe("$ordinary");
  });

  it("filters slash skills under slash without leaking dollar skills", () => {
    seedPrompt("/doc");
    renderComposer();

    const items = findCapture("ComposerCommandMenu")["items"] as Array<Record<string, unknown>>;
    expect(items.map((item) => item["label"])).toEqual(["/docs"]);
    expect(items.map((item) => item["label"])).not.toContain("$refactor");
  });

  it("filters dollar skills without leaking slash skills", () => {
    seedPrompt("$ref");
    renderComposer();

    const menu = findCapture("ComposerCommandMenu");
    const items = menu["items"] as Array<Record<string, unknown>>;
    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({
      id: "provider-skill:codex:dollar:refactor",
      type: "provider-skill",
      label: "$refactor",
      description: "Refactor code safely",
      replacement: "$refactor ",
    });
    expect(items.map((item) => item["label"])).not.toContain("/docs");
    expect(menu["emptyStateText"]).toBe("No matching provider skill.");
  });

  it("passes only mentionable agents to the prompt editor", () => {
    renderComposer();

    expect((editorProps()["agents"] as Array<{ name: string }>).map((agent) => agent.name)).toEqual(
      ["code-reviewer"],
    );
  });

  it("closes a stale menu after provider capabilities change without changing draft text", () => {
    const providerWithoutDollarSkills: ServerProvider = {
      ...codexProvider,
      instanceId: ProviderInstanceId.make("claude"),
      driver: ProviderDriverKind.make("claudeAgent"),
      slashCommands: [],
      skills: [],
      agents: [],
    };
    seedPrompt("$ordinary");
    h.stateSeeds.set(STATE.trigger, {
      kind: "provider-dollar-skill",
      query: "ordinary",
      rangeStart: 0,
      rangeEnd: 9,
    });

    const overrides = {
      providerStatuses: [providerWithoutDollarSkills],
      activeProjectDefaultModelSelection: {
        instanceId: providerWithoutDollarSkills.instanceId,
        model: "gpt-5.4",
      },
    };
    renderComposer(overrides);

    expect(setStateValues(STATE.trigger)).toContain(null);
    expect(draftOf(threadRef)?.prompt).toBe("$ordinary");

    // The next render sees the cleared trigger, so the closed menu drops its stale highlight.
    h.stateSeeds.set(STATE.trigger, null);
    h.stateSeeds.set(STATE.highlightedItemId, "stale-item");
    h.stateSeeds.set(STATE.highlightedSearchKey, "provider-dollar-skill:ordinary");
    renderComposer(overrides);

    expect(setStateValues(STATE.highlightedItemId)).toContain(null);
    expect(setStateValues(STATE.highlightedSearchKey)).toContain(null);
    expect(draftOf(threadRef)?.prompt).toBe("$ordinary");
  });

  it("hides the menu while an approval is pending", () => {
    seedPrompt("/");
    const { markup } = renderComposer({
      activePendingApproval: pendingApproval,
      pendingApprovals: [pendingApproval],
    });
    expect(markup).not.toContain('data-mock="composer-command-menu"');
  });

  it("resets highlight state when the menu is closed", () => {
    seedPrompt("plain prompt");
    h.stateSeeds.set(STATE.highlightedItemId, "stale-item");
    h.stateSeeds.set(STATE.highlightedSearchKey, "provider-slash:stale");
    renderComposer();

    expect(setStateValues(STATE.highlightedItemId)).toContain(null);
    expect(setStateValues(STATE.highlightedSearchKey)).toContain(null);
  });
});

// ---------------------------------------------------------------------------
// Menu selection
// ---------------------------------------------------------------------------

describe("ChatComposer menu selection", () => {
  function renderPathMenu() {
    seedPrompt("hello @src");
    h.pathSearch = {
      entries: [{ path: "src/app/main.ts", kind: "file" }],
      error: null,
      isPending: false,
    };
    const rendered = renderComposer();
    setEditorSnapshot("hello @src", 10);
    const onSelect = findCapture("ComposerCommandMenu")["onSelect"] as (
      item: Record<string, unknown>,
    ) => void;
    const items = findCapture("ComposerCommandMenu")["items"] as Array<Record<string, unknown>>;
    return { ...rendered, onSelect, items };
  }

  it("replaces a path trigger with a native file reference", () => {
    const { onSelect, items } = renderPathMenu();

    onSelect(items[0]!);

    expect(draftOf(threadRef)?.prompt).toBe(
      `hello ${serializeComposerReference("src/app/main.ts")} `,
    );
    // Focus is scheduled on the next animation frame.
    runAnimationFrames();
    expect(h.editorHandle.focusAt).toHaveBeenCalled();
  });

  it("locks re-entrant selection until the next animation frame", () => {
    const { onSelect, items } = renderPathMenu();

    onSelect(items[0]!);
    const afterFirst = draftOf(threadRef)?.prompt;
    onSelect(items[0]!);
    expect(draftOf(threadRef)?.prompt).toBe(afterFirst);

    runAnimationFrames();
  });

  it("consumes a trailing space after the replaced range", () => {
    seedPrompt("see @src x");
    h.pathSearch = {
      entries: [{ path: "src/a.ts", kind: "file" }],
      error: null,
      isPending: false,
    };
    // The initial cursor sits at the end of the prompt, so the menu only
    // opens through a seeded trigger for the mid-prompt "@src" token.
    h.stateSeeds.set(STATE.trigger, {
      kind: "provider-reference",
      query: "src",
      rangeStart: 4,
      rangeEnd: 8,
    });
    renderComposer();
    // Cursor right after "@src" (before the existing space).
    setEditorSnapshot("see @src x", 8);
    const onSelect = findCapture("ComposerCommandMenu")["onSelect"] as (
      item: Record<string, unknown>,
    ) => void;
    const items = findCapture("ComposerCommandMenu")["items"] as Array<Record<string, unknown>>;

    onSelect(items[0]!);

    expect(draftOf(threadRef)?.prompt).toBe(`see ${serializeComposerReference("src/a.ts")} x`);
  });

  it("aborts when the prompt changed under the trigger", () => {
    const { onSelect, items } = renderPathMenu();
    // Snapshot no longer matches the store-backed promptRef contents.
    setEditorSnapshot("hello @other", 12);

    onSelect(items[0]!);

    expect(draftOf(threadRef)?.prompt).toBe("hello @src");
  });

  it("ignores selection without an active trigger", () => {
    const { onSelect, items } = renderPathMenu();
    setEditorSnapshot("plain text", 5);

    onSelect(items[0]!);

    expect(draftOf(threadRef)?.prompt).toBe("hello @src");
  });

  it("ignores a stale item that is no longer in the current menu", () => {
    const { onSelect, items } = renderPathMenu();

    onSelect({ ...items[0]!, id: "stale-provider-item" });

    expect(draftOf(threadRef)?.prompt).toBe("hello @src");
  });

  function renderCommandMenu(prompt: string) {
    seedPrompt(prompt);
    const rendered = renderComposer();
    setEditorSnapshot(prompt, prompt.length);
    const onSelect = findCapture("ComposerCommandMenu")["onSelect"] as (
      item: Record<string, unknown>,
    ) => void;
    const items = findCapture("ComposerCommandMenu")["items"] as Array<Record<string, unknown>>;
    return { ...rendered, onSelect, items };
  }

  it("opens the model picker from :model and clears the prompt", () => {
    const { onSelect, items } = renderCommandMenu(":mod");

    onSelect(items.find((item) => item["id"] === "coddy-studio-action:model")!);

    // Clearing the prompt empties the draft entirely, so the store drops it.
    expect(draftOf(threadRef)?.prompt ?? "").toBe("");
    expect(setStateValues(STATE.modelPickerOpen)).toContain(true);
  });

  it("executes :plan and :default locally", () => {
    const first = renderCommandMenu(":plan");
    first.onSelect(first.items.find((item) => item["id"] === "coddy-studio-action:plan")!);
    expect(first.spies.handleInteractionModeChange).toHaveBeenCalledWith("plan");
    expect(first.spies.onSend).not.toHaveBeenCalled();
    expect(draftOf(threadRef)?.prompt ?? "").toBe("");

    const second = renderCommandMenu(":default");
    second.onSelect(second.items.find((item) => item["id"] === "coddy-studio-action:default")!);
    expect(second.spies.handleInteractionModeChange).toHaveBeenCalledWith("default");
    expect(second.spies.onSend).not.toHaveBeenCalled();
  });

  it("inserts provider slash commands with a trailing space", () => {
    const { onSelect, items } = renderCommandMenu("/rev");

    onSelect(items.find((item) => item["id"] === "provider-command:codex:review")!);

    expect(draftOf(threadRef)?.prompt).toBe("/review ");
  });

  it("inserts skill references with a trailing space", () => {
    seedPrompt("$ref");
    renderComposer();
    setEditorSnapshot("$ref", 4);
    const onSelect = findCapture("ComposerCommandMenu")["onSelect"] as (
      item: Record<string, unknown>,
    ) => void;
    const items = findCapture("ComposerCommandMenu")["items"] as Array<Record<string, unknown>>;

    onSelect(items[0]!);

    expect(draftOf(threadRef)?.prompt).toBe("$refactor ");
  });

  it("uses provider-native slash invocation for slash skills", () => {
    seedPrompt("/doc");
    renderComposer();
    setEditorSnapshot("/doc", 4);
    const menu = findCapture("ComposerCommandMenu");
    const onSelect = menu["onSelect"] as (item: Record<string, unknown>) => void;
    const items = menu["items"] as Array<Record<string, unknown>>;

    onSelect(items[0]!);

    expect(draftOf(threadRef)?.prompt).toBe("/docs ");
  });

  it("inserts an explicit provider-agent instruction", () => {
    const { onSelect, items } = renderCommandMenu("@code");

    onSelect(items.find((item) => item["id"] === "agent-reference:codex:code-reviewer")!);

    expect(draftOf(threadRef)?.prompt).toBe("@code-reviewer ");
  });

  it("routes custom answers through the pending input callback instead of the store", () => {
    seedPrompt("$ref");
    const { spies } = renderComposer({
      pendingUserInputs: [makePendingUserInput()],
      activePendingProgress: makePendingProgress({ customAnswer: "$ref" }),
    });
    setEditorSnapshot("$ref", 4);
    const onSelect = findCapture("ComposerCommandMenu")["onSelect"] as (
      item: Record<string, unknown>,
    ) => void;
    const items = findCapture("ComposerCommandMenu")["items"] as Array<Record<string, unknown>>;

    onSelect(items[0]!);

    expect(spies.onChangeActivePendingUserInputCustomAnswer).toHaveBeenCalledWith(
      "q1",
      "$refactor ",
      expect.any(Number),
      expect.any(Number),
      false,
    );
    expect(draftOf(threadRef)?.prompt).toBe("$ref");
  });

  it("records menu highlight changes with the current search key", () => {
    const { items } = renderPathMenu();
    const onHighlight = findCapture("ComposerCommandMenu")["onHighlightedItemChange"] as (
      id: string | null,
    ) => void;

    onHighlight(String(items[0]!["id"]));

    expect(setStateValues(STATE.highlightedItemId)).toContain(
      "file-reference:file:src/app/main.ts",
    );
    expect(setStateValues(STATE.highlightedSearchKey)).toContain("provider-reference:src");
  });
});

// ---------------------------------------------------------------------------
// Command keys
// ---------------------------------------------------------------------------

describe("ChatComposer command keys", () => {
  it("toggles interaction mode on Shift+Tab", () => {
    const { spies } = renderComposer();
    const onKey = editorProps()["onCommandKeyDown"] as CommandKey;

    expect(onKey("Tab", keyEvent({ shiftKey: true }))).toBe(true);
    expect(spies.toggleInteractionMode).toHaveBeenCalledTimes(1);
  });

  it("navigates and selects menu items from the keyboard", () => {
    seedPrompt("hello @src");
    h.pathSearch = {
      entries: [
        { path: "src/a.ts", kind: "file" },
        { path: "src/b.ts", kind: "file" },
      ],
      error: null,
      isPending: false,
    };
    renderComposer();
    setEditorSnapshot("hello @src", 10);
    const onKey = editorProps()["onCommandKeyDown"] as CommandKey;

    expect(onKey("ArrowDown", keyEvent())).toBe(true);
    expect(setStateValues(STATE.highlightedItemId)).toContain("file-reference:file:src/a.ts");
    expect(onKey("ArrowUp", keyEvent())).toBe(true);

    expect(onKey("Enter", keyEvent())).toBe(true);
    expect(draftOf(threadRef)?.prompt).toBe(`hello ${serializeComposerReference("src/a.ts")} `);
  });

  it("preserves the exact-agent preference through highlight sync and rerender before Enter", () => {
    seedPrompt("@code-reviewer");
    h.pathSearch = {
      entries: [{ path: "code-reviewer.ts", kind: "file" }],
      error: null,
      isPending: false,
    };
    renderComposer();

    const preferredAgentId = "agent-reference:codex:code-reviewer";
    const syncedHighlight = setStateValues(STATE.highlightedItemId).findLast(
      (value) => value !== null,
    );
    const syncedSearchKey = setStateValues(STATE.highlightedSearchKey).findLast(
      (value) => value !== null,
    );
    expect(syncedHighlight).toBe(preferredAgentId);

    h.stateSeeds.set(STATE.highlightedItemId, syncedHighlight);
    h.stateSeeds.set(STATE.highlightedSearchKey, syncedSearchKey);
    renderComposer();
    setEditorSnapshot("@code-reviewer", 14);

    const onKey = editorProps()["onCommandKeyDown"] as CommandKey;
    expect(onKey("Enter", keyEvent())).toBe(true);
    expect(draftOf(threadRef)?.prompt).toBe("@code-reviewer ");
  });

  it("executes a standalone :model through the keyboard Enter path", () => {
    seedPrompt(":model");
    const { spies } = renderComposer();
    setEditorSnapshot(":model", 6);
    const onKey = editorProps()["onCommandKeyDown"] as CommandKey;

    expect(onKey("Enter", keyEvent())).toBe(true);
    expect(spies.onSend).not.toHaveBeenCalled();
    expect(draftOf(threadRef)?.prompt ?? "").toBe("");
    expect(setStateValues(STATE.trigger)).toContain(null);
    expect(setStateValues(STATE.modelPickerOpen)).toContain(true);
  });

  it.each([
    [":model", null],
    [":plan", "plan"],
    [":default", "default"],
  ] as const)(
    "executes a pending standalone %s through Enter without advancing the answer",
    (actionText, expectedInteractionMode) => {
      seedPrompt(actionText);
      const { spies } = renderComposer({
        pendingUserInputs: [makePendingUserInput()],
        activePendingProgress: makePendingProgress({ customAnswer: actionText }),
      });
      setEditorSnapshot(actionText, actionText.length);
      const onKey = editorProps()["onCommandKeyDown"] as CommandKey;

      expect(onKey("Enter", keyEvent())).toBe(true);
      expect(spies.onSend).not.toHaveBeenCalled();
      expect(spies.onChangeActivePendingUserInputCustomAnswer).toHaveBeenCalledWith(
        "q1",
        "",
        0,
        0,
        false,
      );
      if (expectedInteractionMode === null) {
        expect(setStateValues(STATE.modelPickerOpen)).toContain(true);
        expect(spies.handleInteractionModeChange).not.toHaveBeenCalled();
      } else {
        expect(setStateValues(STATE.modelPickerOpen)).not.toContain(true);
        expect(spies.handleInteractionModeChange).toHaveBeenCalledWith(expectedInteractionMode);
      }
    },
  );

  it("submits on Enter without an active menu", () => {
    seedPrompt("send me");
    const { spies } = renderComposer();
    setEditorSnapshot("send me", 7);
    const onKey = editorProps()["onCommandKeyDown"] as CommandKey;

    expect(onKey("Enter", keyEvent())).toBe(true);
    expect(spies.onSend).toHaveBeenCalledTimes(1);
  });

  it("lets Shift+Enter fall through for a newline", () => {
    seedPrompt("send me");
    const { spies } = renderComposer();
    setEditorSnapshot("send me", 7);
    const onKey = editorProps()["onCommandKeyDown"] as CommandKey;

    expect(onKey("Enter", keyEvent({ shiftKey: true }))).toBe(false);
    expect(spies.onSend).not.toHaveBeenCalled();
  });
});

// ---------------------------------------------------------------------------
// Prompt changes from the editor
// ---------------------------------------------------------------------------

describe("ChatComposer prompt changes", () => {
  it("stores the new prompt and re-detects the trigger", () => {
    seedPrompt("old");
    renderComposer();
    const onChange = editorProps()["onChange"] as PromptChange;

    onChange("new @q", 6, 6, false, []);

    expect(draftOf(threadRef)?.prompt).toBe("new @q");
    expect(setStateValues(STATE.cursor)).toContain(6);
    const triggers = setStateValues(STATE.trigger) as Array<{ kind?: string } | null>;
    expect(triggers.at(-1)?.kind).toBe("provider-reference");
  });

  it("suppresses the trigger when the cursor touches a mention", () => {
    seedPrompt("old");
    renderComposer();
    const onChange = editorProps()["onChange"] as PromptChange;

    onChange("new @q", 6, 6, true, []);

    expect(setStateValues(STATE.trigger).at(-1)).toBeNull();
  });

  it("synchronizes terminal contexts removed inside the editor", () => {
    const context = makeTerminalContext("ctx-1");
    draftStore().setTerminalContexts(threadRef, [context]);
    seedPrompt(`${INLINE_TERMINAL_CONTEXT_PLACEHOLDER} tail`);
    renderComposer();
    const onChange = editorProps()["onChange"] as PromptChange;

    // Same ids: no sync required.
    onChange(`${INLINE_TERMINAL_CONTEXT_PLACEHOLDER} tai`, 4, 4, false, ["ctx-1"]);
    expect(draftOf(threadRef)?.terminalContexts.map((entry) => entry.id)).toEqual(["ctx-1"]);

    // Unknown editor ids are discarded while known contexts retain editor order.
    onChange(`${INLINE_TERMINAL_CONTEXT_PLACEHOLDER} tai`, 4, 4, false, ["missing", "ctx-1"]);
    expect(draftOf(threadRef)?.terminalContexts.map((entry) => entry.id)).toEqual(["ctx-1"]);

    // Editor dropped the placeholder: the store follows.
    onChange("tail", 4, 4, false, []);
    expect(draftOf(threadRef)?.terminalContexts).toEqual([]);
  });

  it("removes terminal chips by id and ignores stale removal requests", () => {
    const first = makeTerminalContext("ctx-first");
    const second = { ...makeTerminalContext("ctx-second"), terminalId: "term-2" };
    draftStore().setTerminalContexts(threadRef, [first, second]);
    seedPrompt(
      `${INLINE_TERMINAL_CONTEXT_PLACEHOLDER} ${INLINE_TERMINAL_CONTEXT_PLACEHOLDER} tail`,
    );
    renderComposer();
    const remove = editorProps()["onRemoveTerminalContext"] as (id: string) => void;

    remove("missing");
    expect(draftOf(threadRef)?.terminalContexts.map((entry) => entry.id)).toEqual([
      "ctx-first",
      "ctx-second",
    ]);

    remove("ctx-second");
    expect(draftOf(threadRef)?.terminalContexts.map((entry) => entry.id)).toEqual(["ctx-first"]);
  });

  it("routes edits to the pending input callback while a question is active", () => {
    const { spies } = renderComposer({
      pendingUserInputs: [makePendingUserInput()],
      activePendingProgress: makePendingProgress(),
    });
    const onChange = editorProps()["onChange"] as PromptChange;

    onChange("typed", 5, 5, false, []);

    expect(spies.onChangeActivePendingUserInputCustomAnswer).toHaveBeenCalledWith(
      "q1",
      "typed",
      5,
      5,
      false,
    );
    expect(draftOf(threadRef)?.prompt ?? "").toBe("");
  });
});

// ---------------------------------------------------------------------------
// Paste and drag/drop
// ---------------------------------------------------------------------------

describe("ChatComposer paste and drag", () => {
  function imageFile(name = "shot.png"): File {
    return new File([new Uint8Array([1, 2, 3, 4])], name, { type: "image/png" });
  }

  it("adds a single pasted image and clears the thread error", () => {
    const { spies } = renderComposer();
    const onPaste = editorProps()["onPaste"] as (event: unknown) => void;
    const event = pasteEvent([imageFile()]);

    onPaste(event);

    expect(
      (event as unknown as { preventDefault: ReturnType<typeof vi.fn> }).preventDefault,
    ).toHaveBeenCalled();
    expect(draftOf(threadRef)?.attachments).toHaveLength(1);
    expect(draftOf(threadRef)?.attachments[0]).toMatchObject({
      previewUrl: expect.stringContaining("blob:generated-"),
    });
    expect(spies.setThreadError).toHaveBeenCalledWith(threadId, null);
  });

  it("adds multiple pasted images at once", () => {
    renderComposer();
    const onPaste = editorProps()["onPaste"] as (event: unknown) => void;

    onPaste(pasteEvent([imageFile("a.png"), imageFile("b.png")]));

    expect(draftOf(threadRef)?.attachments.map((attachment) => attachment.name)).toEqual([
      "a.png",
      "b.png",
    ]);
  });

  it("adds non-image files from paste", () => {
    renderComposer();
    const onPaste = editorProps()["onPaste"] as (event: unknown) => void;

    const empty = pasteEvent([]);
    onPaste(empty);
    const textOnly = pasteEvent([new File(["x"], "notes.txt", { type: "text/plain" })]);
    onPaste(textOnly);

    expect(draftOf(threadRef)?.attachments.map((attachment) => attachment.name)).toEqual([
      "notes.txt",
    ]);
    expect(
      (textOnly as unknown as { preventDefault: ReturnType<typeof vi.fn> }).preventDefault,
    ).toHaveBeenCalled();
  });

  it("blocks paste, file input, and drag attachment ingress during a provider conflict", () => {
    const conflictReason = "Provider metadata conflicts with the active session.";
    const { spies } = renderComposer({ providerBindingConflictReason: conflictReason });
    const pasted = pasteEvent([imageFile("pasted.png")]);
    const onPaste = editorProps()["onPaste"] as (event: unknown) => void;
    const fileInput = findHost(
      (element) => element.type === "input" && element.props["type"] === "file",
    );
    const inputTarget = {
      files: [imageFile("selected.png")],
      value: "/fake/selected.png",
    };
    const dragHost = findHost((element) => typeof element.props["onDrop"] === "function");
    const entered = dragEvent({ files: [imageFile("entered.png")] });
    const dropped = dragEvent({ files: [imageFile("dropped.png")] });

    onPaste(pasted);
    (fileInput.props["onChange"] as (event: unknown) => void)({ currentTarget: inputTarget });
    (dragHost.props["onDragEnter"] as (event: unknown) => void)(entered);
    (dragHost.props["onDrop"] as (event: unknown) => void)(dropped);

    expect(pasted.preventDefault).toHaveBeenCalled();
    expect(inputTarget.value).toBe("");
    expect(entered.preventDefault).toHaveBeenCalled();
    expect(dropped.preventDefault).toHaveBeenCalled();
    expect(setStateValues(STATE.dragOver)).not.toContain(true);
    expect(draftOf(threadRef)?.attachments ?? []).toEqual([]);
    expect(spies.setThreadError).not.toHaveBeenCalled();
    expect(spies.focusComposer).not.toHaveBeenCalled();
  });

  it("rejects attachments while plan questions are pending", () => {
    renderComposer({
      pendingUserInputs: [makePendingUserInput()],
      activePendingProgress: makePendingProgress(),
    });
    const onPaste = editorProps()["onPaste"] as (event: unknown) => void;

    onPaste(pasteEvent([imageFile()]));

    expect(h.toastAdd).toHaveBeenCalledWith({
      type: "error",
      title: "Attach files after answering plan questions.",
    });
    expect(draftOf(threadRef)?.attachments ?? []).toEqual([]);
  });

  it("does nothing without an active thread", () => {
    renderComposer({ activeThreadId: null });
    const onPaste = editorProps()["onPaste"] as (event: unknown) => void;

    onPaste(pasteEvent([imageFile()]));

    expect(draftOf(threadRef)?.attachments ?? []).toEqual([]);
  });

  it("reports empty, oversized, and excess attachments on drop", () => {
    const preloaded = Array.from({ length: PROVIDER_SEND_TURN_MAX_ATTACHMENTS }, (_, index) =>
      makeImage({ id: `preloaded-${index}`, name: `preloaded-${index}.png` }),
    );
    const { spies } = renderComposer({
      composerAttachmentsRef: { current: [] },
    });
    const dropHost = findHost((element) => typeof element.props["onDrop"] === "function");
    const onDrop = dropHost.props["onDrop"] as (event: unknown) => void;

    // Empty file.
    onDrop(dragEvent({ files: [new File([], "empty.txt", { type: "text/plain" })] }));
    expect(spies.setThreadError).toHaveBeenLastCalledWith(
      threadId,
      "'empty.txt' is empty and cannot be attached.",
    );

    // Oversized image.
    const oversized = {
      name: "big.png",
      type: "image/png",
      size: PROVIDER_SEND_TURN_MAX_IMAGE_BYTES + 1,
    } as unknown as File;
    onDrop(dragEvent({ files: [oversized] }));
    expect(String(spies.setThreadError.mock.calls.at(-1)?.[1])).toContain("exceeds the");

    // Attachment cap.
    draftStore().addAttachments(threadRef, preloaded);
    onDrop(dragEvent({ files: [imageFile("over.png")] }));
    expect(spies.setThreadError).toHaveBeenLastCalledWith(
      threadId,
      "'over.png' cannot be attached: you can attach up to 8 files per message.",
    );

    expect(spies.focusComposer).toHaveBeenCalledTimes(3);
  });

  it("keeps the first validation error from a mixed invalid drop", () => {
    const { spies } = renderComposer();
    const onDrop = findHost((element) => typeof element.props["onDrop"] === "function").props[
      "onDrop"
    ] as (event: unknown) => void;
    const oversized = {
      name: "large.bin",
      type: "application/octet-stream",
      size: PROVIDER_SEND_TURN_MAX_IMAGE_BYTES + 1,
    } as unknown as File;

    onDrop(
      dragEvent({
        files: [new File([], "empty.txt", { type: "text/plain" }), oversized],
      }),
    );

    expect(spies.setThreadError).toHaveBeenLastCalledWith(
      threadId,
      "'empty.txt' is empty and cannot be attached.",
    );
  });

  it("reports the earliest rejected input across capacity and validation failures", () => {
    draftStore().addAttachments(
      threadRef,
      Array.from({ length: PROVIDER_SEND_TURN_MAX_ATTACHMENTS }, (_, index) =>
        makeImage({ id: `preloaded-${index}`, name: `preloaded-${index}.png` }),
      ),
    );
    const { spies } = renderComposer();
    const onDrop = findHost((element) => typeof element.props["onDrop"] === "function").props[
      "onDrop"
    ] as (event: unknown) => void;

    onDrop(
      dragEvent({
        files: [imageFile("valid.png"), new File([], "empty.txt", { type: "text/plain" })],
      }),
    );

    expect(spies.setThreadError).toHaveBeenLastCalledWith(
      threadId,
      "'valid.png' cannot be attached: you can attach up to 8 files per message.",
    );
  });

  it("tracks drag enter, over, leave, and drop", () => {
    renderComposer();
    const dragHost = findHost((element) => typeof element.props["onDragEnter"] === "function");
    const onDragEnter = dragHost.props["onDragEnter"] as (event: unknown) => void;
    const onDragOver = dragHost.props["onDragOver"] as (event: unknown) => void;
    const onDragLeave = dragHost.props["onDragLeave"] as (event: unknown) => void;
    const onDrop = dragHost.props["onDrop"] as (event: unknown) => void;

    // Non-file drags are ignored entirely.
    const nonFile = dragEvent({ types: ["text/plain"] });
    onDragEnter(nonFile);
    onDragOver(nonFile);
    onDragLeave(nonFile);
    onDrop(nonFile);
    expect((nonFile.preventDefault as ReturnType<typeof vi.fn>).mock.calls).toHaveLength(0);

    const falseResets = () =>
      setStateValues(STATE.dragOver).filter((value) => value === false).length;
    const baselineFalse = falseResets();

    const enter = dragEvent();
    onDragEnter(enter);
    expect(enter.preventDefault).toHaveBeenCalled();
    expect(setStateValues(STATE.dragOver)).toContain(true);

    const over = dragEvent();
    onDragOver(over);
    expect(over.dataTransfer.dropEffect).toBe("copy");

    // Leaving toward a child node keeps the overlay active.
    const inside = dragEvent({ relatedTarget: new FakeHTMLElement(), containsRelated: true });
    onDragLeave(inside);
    expect(falseResets()).toBe(baselineFalse);

    // Leaving the surface clears it once the depth returns to zero.
    const outside = dragEvent({ relatedTarget: new FakeHTMLElement(), containsRelated: false });
    onDragLeave(outside);
    expect(falseResets()).toBe(baselineFalse + 1);

    const drop = dragEvent({ files: [imageFile("dropped.png")] });
    onDrop(drop);
    expect(draftOf(threadRef)?.attachments.map((attachment) => attachment.name)).toEqual([
      "dropped.png",
    ]);
  });

  it("renders the drag-over styling when a drag is active", () => {
    h.stateSeeds.set(STATE.dragOver, true);
    const { markup } = renderComposer();
    expect(markup).toContain("border-primary/70");
  });
});

// ---------------------------------------------------------------------------
// Form submit
// ---------------------------------------------------------------------------

describe("ChatComposer submit", () => {
  it("submits the form through onSend", () => {
    const { spies } = renderComposer();
    const form = findHost((element) => element.type === "form");
    const event = { preventDefault: vi.fn() };

    (form.props["onSubmit"] as (event: unknown) => void)(event);

    expect(spies.onSend).toHaveBeenCalledWith(event);
  });

  it.each([
    [":model", null],
    [":plan", "plan"],
    [":default", "default"],
  ] as const)(
    "executes a pending standalone %s from the submit button without advancing",
    (actionText, expectedInteractionMode) => {
      seedPrompt(actionText);
      const { spies } = renderComposer({
        pendingUserInputs: [makePendingUserInput()],
        activePendingProgress: makePendingProgress({ customAnswer: actionText }),
      });
      const form = findHost((element) => element.type === "form");
      const event = { preventDefault: vi.fn() };

      (form.props["onSubmit"] as (event: unknown) => void)(event);

      expect(event.preventDefault).toHaveBeenCalledTimes(1);
      expect(spies.onSend).not.toHaveBeenCalled();
      expect(spies.onChangeActivePendingUserInputCustomAnswer).toHaveBeenCalledWith(
        "q1",
        "",
        0,
        0,
        false,
      );
      if (expectedInteractionMode === null) {
        expect(setStateValues(STATE.modelPickerOpen)).toContain(true);
        expect(spies.handleInteractionModeChange).not.toHaveBeenCalled();
      } else {
        expect(setStateValues(STATE.modelPickerOpen)).not.toContain(true);
        expect(spies.handleInteractionModeChange).toHaveBeenCalledWith(expectedInteractionMode);
      }
    },
  );

  it("keeps an ordinary pending custom answer in the pending-answer flow", () => {
    seedPrompt("ordinary answer");
    const { spies } = renderComposer({
      pendingUserInputs: [makePendingUserInput()],
      activePendingProgress: makePendingProgress({ customAnswer: "ordinary answer" }),
    });
    const form = findHost((element) => element.type === "form");
    const event = { preventDefault: vi.fn() };

    (form.props["onSubmit"] as (event: unknown) => void)(event);

    expect(spies.onSend).toHaveBeenCalledWith(event);
    expect(spies.onChangeActivePendingUserInputCustomAnswer).not.toHaveBeenCalled();
    expect(setStateValues(STATE.modelPickerOpen)).not.toContain(true);
  });
});

// ---------------------------------------------------------------------------
// Imperative handle
// ---------------------------------------------------------------------------

describe("ChatComposer imperative handle", () => {
  it("forwards focus helpers to the prompt editor", () => {
    const { handle } = renderComposer();

    handle().focusAtEnd();
    expect(h.editorHandle.focusAtEnd).toHaveBeenCalledTimes(1);
    handle().focusAt(3);
    expect(h.editorHandle.focusAt).toHaveBeenCalledWith(3);
  });

  it("inserts text at the end of the prompt", () => {
    seedPrompt("hello");
    const { handle } = renderComposer();

    expect(handle().insertTextAtEnd(" world")).toBe(true);
    expect(draftOf(threadRef)?.prompt).toBe("hello world");
    runAnimationFrames();
    expect(h.editorHandle.focusAt).toHaveBeenCalled();
  });

  it("refuses insertion when blocked", () => {
    seedPrompt("hello");
    const blockedStates: Array<Partial<ChatComposerProps>> = [
      { isConnecting: true },
      { activePendingApproval: pendingApproval, pendingApprovals: [pendingApproval] },
      { pendingUserInputs: [makePendingUserInput()] },
      {
        environmentUnavailable: {
          label: "Laptop",
          connection: { phase: "offline", error: null, traceId: null },
        },
      },
    ];
    for (const overrides of blockedStates) {
      const { handle } = renderComposer(overrides);
      expect(handle().insertTextAtEnd(" world")).toBe(false);
    }
    const { handle } = renderComposer();
    expect(handle().insertTextAtEnd("")).toBe(false);
    expect(draftOf(threadRef)?.prompt).toBe("hello");
  });

  it("controls the model picker", () => {
    h.stateSeeds.set(STATE.modelPickerOpen, true);
    const { handle } = renderComposer();

    expect(handle().isModelPickerOpen()).toBe(true);
    handle().openModelPicker();
    expect(setStateValues(STATE.modelPickerOpen)).toContain(true);
    handle().toggleModelPicker();
    expect(setStateValues(STATE.modelPickerOpen)).toContain(false);

    // The picker mock receives the seeded open flag and reports open changes.
    const picker = findCapture("ProviderModelPicker");
    expect(picker["open"]).toBe(true);
    (picker["onOpenChange"] as (open: boolean) => void)(false);
    expect(setStateValues(STATE.modelPickerOpen)).toContain(false);
  });

  it("reads snapshots from the editor and falls back to local state", () => {
    seedPrompt("fallback text");
    h.stateSeeds.set(STATE.cursor, 4);
    const { handle } = renderComposer();

    setEditorSnapshot("editor text", 2, ["ctx-9"]);
    expect(handle().readSnapshot()).toEqual({
      value: "editor text",
      cursor: 2,
      expandedCursor: 2,
      terminalContextIds: ["ctx-9"],
    });

    h.editorSnapshot = null;
    expect(handle().readSnapshot()).toEqual({
      value: "fallback text",
      cursor: 4,
      expandedCursor: 4,
      terminalContextIds: [],
    });
  });

  it("resets cursor state with and without trigger detection", () => {
    // "@qu" without a trailing space is not yet an inline token, so the
    // collapsed and expanded cursors coincide and the trigger stays live.
    seedPrompt("hi @qu");
    const { handle } = renderComposer();

    handle().resetCursorState({ cursor: 6, detectTrigger: true });
    expect(setStateValues(STATE.cursor)).toContain(6);
    const triggers = setStateValues(STATE.trigger) as Array<{ kind?: string } | null>;
    expect(triggers.at(-1)?.kind).toBe("provider-reference");

    handle().resetCursorState({ prompt: "clean", cursor: 2 });
    expect(setStateValues(STATE.trigger).at(-1)).toBeNull();
  });

  it("inserts terminal contexts at the editor cursor", () => {
    seedPrompt("hello world");
    const { handle } = renderComposer();
    setEditorSnapshot("hello world", 5);

    handle().addTerminalContext({
      terminalId: "term-9",
      terminalLabel: "Terminal 9",
      lineStart: 10,
      lineEnd: 12,
      text: "compile ok",
    });

    const draft = draftOf(threadRef);
    expect(draft?.terminalContexts).toHaveLength(1);
    expect(draft?.terminalContexts[0]).toMatchObject({
      terminalId: "term-9",
      threadId,
      text: "compile ok",
    });
    expect(draft?.prompt).toContain(INLINE_TERMINAL_CONTEXT_PLACEHOLDER);
    runAnimationFrames();
    expect(h.editorHandle.focusAt).toHaveBeenCalled();
  });

  it("returns false without mutating when terminal context is added during a provider conflict", () => {
    seedPrompt("hello");
    const { handle } = renderComposer({
      providerBindingConflictReason: "Provider metadata conflicts with the active session.",
    });

    const inserted = handle().addTerminalContext({
      terminalId: "term-9",
      terminalLabel: "Terminal 9",
      lineStart: 10,
      lineEnd: 12,
      text: "compile ok",
    });

    expect(inserted).toBe(false);
    expect(draftOf(threadRef)?.prompt).toBe("hello");
    expect(draftOf(threadRef)?.terminalContexts ?? []).toEqual([]);
  });

  it("skips terminal context insertion without an active thread", () => {
    seedPrompt("hello");
    const { handle } = renderComposer({ activeThread: undefined });

    handle().addTerminalContext({
      terminalId: "term-9",
      terminalLabel: "Terminal 9",
      lineStart: 1,
      lineEnd: 2,
      text: "ignored",
    });

    expect(draftOf(threadRef)?.terminalContexts ?? []).toEqual([]);
  });

  it("exposes the full send context", () => {
    seedPrompt("send me");
    draftStore().setReviewComments(threadRef, [makeReviewComment("rc-ctx")]);
    const { handle } = renderComposer();

    const context = handle().getSendContext();

    expect(context.prompt).toBe("send me");
    expect(context.selectedProvider).toBe("codex");
    expect(context.selectedModel).toBe("gpt-5.4");
    expect(context.selectedModelSelection.instanceId).toBe(codexInstanceId);
    expect(context.reviewComments.map((comment) => comment.id)).toEqual(["rc-ctx"]);
    expect(context.attachments).toEqual([]);
    expect(context.selectedProviderModels.map((model) => model.slug)).toEqual(["gpt-5.4"]);
  });
});

// ---------------------------------------------------------------------------
// Provider / model selection
// ---------------------------------------------------------------------------

describe("ChatComposer provider selection", () => {
  it("disables composing and submission for a provider binding conflict", () => {
    const conflictReason =
      'Provider instance "codex_personal" reports driver "claude", but the active session expects "codex". Sending is blocked until provider metadata agrees.';
    const conflictOverrides = {
      providerBindingConflictReason: conflictReason,
      providerBindingInstanceId: ProviderInstanceId.make("codex_personal"),
      lockProviderPickerToActiveInstance: true,
    } as Partial<ChatComposerProps> & { providerBindingConflictReason: string };
    seedPrompt("do not send");

    const { spies } = renderComposer(conflictOverrides);

    expect(editorProps()["disabled"]).toBe(true);
    expect(editorProps()["placeholder"]).toBe(conflictReason);
    expect(findCapture("ProviderModelPicker")["disabled"]).toBe(true);
    expect(lastCapture("ComposerPrimaryActions")["sendBlockedReason"]).toBe(conflictReason);

    setEditorSnapshot("do not send", 11);
    const onKey = editorProps()["onCommandKeyDown"] as CommandKey;
    onKey("Enter", keyEvent());
    expect(spies.onSend).not.toHaveBeenCalled();
  });

  it("ignores provider trait prompt and model-option callbacks during a binding conflict", async () => {
    seedPrompt("keep this prompt");
    const onCommitModelSelection = vi.fn(async () => undefined);
    renderComposer({
      providerBindingConflictReason: "Provider metadata conflicts with the active session.",
      onCommitModelSelection,
    });
    const traitInput = h.traitInputs.at(-1) as {
      onPromptChange: (nextPrompt: string) => void;
      onModelOptionsChange?: (
        nextOptions: ReadonlyArray<{ id: string; value: string | boolean }> | undefined,
      ) => void | Promise<void>;
    };

    traitInput.onPromptChange("replace this prompt");
    await traitInput.onModelOptionsChange?.([{ id: "effort", value: "high" }]);

    const draft = draftOf(threadRef);
    expect(draft?.prompt).toBe("keep this prompt");
    expect(draft?.modelSelectionByProvider ?? {}).toEqual({});
    expect(onCommitModelSelection).not.toHaveBeenCalled();
  });

  it("falls back to codex when no providers are configured", () => {
    renderComposer({ providerStatuses: [], activeProjectDefaultModelSelection: null });
    const picker = findCapture("ProviderModelPicker");
    expect(picker["activeInstanceId"]).toBe("codex");
  });

  it("keeps an explicitly selected instance even when it has no live entry", () => {
    renderComposer({
      providerStatuses: [],
      providerBindingInstanceId: ProviderInstanceId.make("codex_personal"),
      activeProjectDefaultModelSelection: null,
      activeThreadModelSelection: {
        instanceId: ProviderInstanceId.make("codex_personal"),
        model: "gpt-5.4",
      },
    });
    const picker = findCapture("ProviderModelPicker");
    expect(picker["activeInstanceId"]).toBe("codex_personal");
  });

  it("keeps an exact missing-instance lock ahead of a colliding stale selection", () => {
    const boundInstanceId = ProviderInstanceId.make("codex_personal");
    const staleInstanceId = ProviderInstanceId.make("stale_selection");
    const collidingProvider: ServerProvider = {
      ...codexProvider,
      instanceId: staleInstanceId,
      driver: ProviderDriverKind.make("codex_personal"),
      models: [
        {
          slug: "collision-model",
          name: "Collision Model",
          isCustom: false,
          capabilities: null,
        },
      ],
    };
    draftStore().setModelSelection(threadRef, {
      instanceId: staleInstanceId,
      model: "collision-model",
    });
    publishSeededStoreState();

    renderComposer({
      lockedProvider: null,
      providerBindingInstanceId: boundInstanceId,
      lockProviderPickerToActiveInstance: true,
      providerStatuses: [collidingProvider],
      activeProjectDefaultModelSelection: null,
      activeThreadModelSelection: { instanceId: boundInstanceId, model: "gpt-5.4" },
    });

    const picker = findCapture("ProviderModelPicker");
    expect(picker["activeInstanceId"]).toBe("codex_personal");
    expect(picker["lockToActiveInstance"]).toBe(true);
    expect(picker["lockedProvider"]).toBeNull();
  });

  it("routes a legacy partial session through its authoritative binding", () => {
    const claudeInstanceId = ProviderInstanceId.make("claude");
    const claudeProvider: ServerProvider = {
      ...codexProvider,
      instanceId: claudeInstanceId,
      driver: ProviderDriverKind.make("claude"),
      models: [
        {
          slug: "claude-sonnet",
          name: "Claude Sonnet",
          isCustom: false,
          capabilities: null,
        },
      ],
    };
    draftStore().setModelSelection(threadRef, {
      instanceId: claudeInstanceId,
      model: "claude-sonnet",
    });
    publishSeededStoreState();

    const { handle } = renderComposer({
      activeThread: makeThread({
        modelSelection: { instanceId: claudeInstanceId, model: "claude-sonnet" },
        session: {
          threadId,
          status: "ready",
          providerName: "codex",
          runtimeMode: "full-access",
          activeTurnId: null,
          lastError: null,
          updatedAt: now,
        },
      }),
      lockedProvider: ProviderDriverKind.make("codex"),
      providerBindingInstanceId: codexInstanceId,
      providerStatuses: [codexProvider, claudeProvider],
      activeProjectDefaultModelSelection: null,
      activeThreadModelSelection: {
        instanceId: claudeInstanceId,
        model: "claude-sonnet",
      },
    });

    const picker = findCapture("ProviderModelPicker");
    expect(picker["activeInstanceId"]).toBe("codex");
    expect(picker["model"]).toBe("gpt-5.4");
    expect(handle().getSendContext()).toMatchObject({
      selectedProvider: "codex",
      selectedModelSelection: { instanceId: "codex", model: "gpt-5.4" },
    });
  });

  it("routes an exact session account while only another account has status", () => {
    const customInstanceId = ProviderInstanceId.make("codex_personal");
    const { handle } = renderComposer({
      activeThread: makeThread({
        modelSelection: { instanceId: customInstanceId, model: "gpt-5.4" },
        session: {
          threadId,
          status: "ready",
          providerName: "codex",
          providerInstanceId: customInstanceId,
          runtimeMode: "full-access",
          activeTurnId: null,
          lastError: null,
          updatedAt: now,
        },
      }),
      lockedProvider: ProviderDriverKind.make("codex"),
      providerBindingInstanceId: customInstanceId,
      lockProviderPickerToActiveInstance: true,
      providerStatuses: [codexProvider],
      activeProjectDefaultModelSelection: {
        instanceId: codexInstanceId,
        model: "gpt-5.4",
      },
      activeThreadModelSelection: {
        instanceId: customInstanceId,
        model: "gpt-5.4",
      },
    });

    expect(findCapture("ProviderModelPicker")).toMatchObject({
      activeInstanceId: "codex_personal",
      lockedProvider: "codex",
      lockToActiveInstance: true,
    });
    expect(handle().getSendContext()).toMatchObject({
      selectedProvider: "codex",
      selectedModelSelection: { instanceId: "codex_personal", model: "gpt-5.4" },
    });
  });

  it("locks the provider and derives the continuation group", () => {
    renderComposer({
      lockedProvider: ProviderDriverKind.make("codex"),
      activeThreadModelSelection: { instanceId: codexInstanceId, model: "gpt-5.4" },
    });
    const picker = findCapture("ProviderModelPicker");
    expect(picker["activeInstanceId"]).toBe("codex");
    expect(picker["lockedProvider"]).toBe("codex");

    (picker["onInstanceModelChange"] as (instance: string, model: string) => void)(
      "codex",
      "gpt-5.4",
    );
  });

  it("skips persisted selections from a different driver kind while locked", () => {
    renderComposer({
      lockedProvider: ProviderDriverKind.make("claudeAgent"),
      activeThreadModelSelection: { instanceId: codexInstanceId, model: "gpt-5.4" },
    });
    const picker = findCapture("ProviderModelPicker");
    // The codex selection is rejected; the explicit instance id wins instead.
    expect(picker["activeInstanceId"]).toBe("codex");
  });
});

// ---------------------------------------------------------------------------
// Mobile behaviors
// ---------------------------------------------------------------------------

describe("ChatComposer mobile", () => {
  it("collapses on mobile with an expandable prompt row", () => {
    h.isMobile = true;
    seedPrompt("draft text");
    const { markup } = renderComposer();

    expect(markup).toContain('data-chat-composer-mobile-collapsed="true"');
    expect(markup).toContain("draft text");

    const expand = hostByLabel("Expand composer");
    (expand["onClick"] as () => void)();
    expect(setStateValues(STATE.focused)).toContain(true);
    runAnimationFrames();
    expect(h.editorHandle.focusAtEnd).toHaveBeenCalledTimes(1);

    const pointerDown = expand["onPointerDown"] as (event: { preventDefault: () => void }) => void;
    const pointerEvent = { preventDefault: vi.fn() };
    pointerDown(pointerEvent);
    expect(pointerEvent.preventDefault).toHaveBeenCalled();
  });

  it("shows the collapsed placeholder text when the prompt is empty", () => {
    h.isMobile = true;
    const { markup } = renderComposer();
    expect(markup).toContain("Ask anything...");
  });

  it("sends from the collapsed row and blurs the active element", () => {
    h.isMobile = true;
    seedPrompt("ready to send");
    const active = new FakeHTMLElement();
    documentStub.activeElement = active;
    const { spies } = renderComposer();

    const send = hostByLabel("Send message");
    const clickEvent = { stopPropagation: vi.fn() };
    (send["onClick"] as (event: unknown) => void)(clickEvent);

    expect(clickEvent.stopPropagation).toHaveBeenCalled();
    expect(spies.onSend).toHaveBeenCalledTimes(1);
    expect(active.blur).toHaveBeenCalledTimes(1);
    expect(setStateValues(STATE.focused)).toContain(false);
  });

  it("keeps focus when the turn is still running", () => {
    h.isMobile = true;
    seedPrompt("ready to send");
    const active = new FakeHTMLElement();
    documentStub.activeElement = active;
    renderComposer({ phase: "running" });

    const send = hostByLabel("Send message");
    (send["onClick"] as (event: unknown) => void)({ stopPropagation: vi.fn() });

    expect(active.blur).not.toHaveBeenCalled();
  });

  it("disables the collapsed send button without sendable content", () => {
    h.isMobile = true;
    const { markup } = renderComposer();
    const send = hostByLabel("Send message");
    expect(send["disabled"]).toBe(true);
    expect(markup).toContain("disabled");
  });

  it("renders the collapsed approval controls", () => {
    h.isMobile = true;
    const { markup } = renderComposer({
      activePendingApproval: pendingApproval,
      pendingApprovals: [pendingApproval],
    });
    expect(markup).toContain('data-chat-composer-collapsed-controls="true"');
    expect(markup).toContain('data-mock="ComposerPendingApprovalActions"');
  });

  it("renders the collapsed pending question controls with a custom answer button", () => {
    h.isMobile = true;
    const { markup } = renderComposer({
      pendingUserInputs: [makePendingUserInput()],
      activePendingProgress: makePendingProgress({
        customAnswer: "typed answer",
        activeQuestion: { id: "q1", multiSelect: true },
      }),
    });

    expect(markup).toContain('data-chat-composer-mobile-pending-compact="true"');
    expect(markup).toContain("typed answer");
    expect(markup).toContain('data-mock="ComposerPrimaryActions"');

    const write = hostByLabel("Write custom answer");
    (write["onClick"] as () => void)();
    expect(setStateValues(STATE.focused)).toContain(true);
  });

  it("shows floating pending answer actions while expanded on mobile", () => {
    h.isMobile = true;
    h.stateSeeds.set(STATE.focused, true);
    const { markup } = renderComposer({
      pendingUserInputs: [makePendingUserInput()],
      activePendingProgress: makePendingProgress(),
    });

    expect(markup).toContain('data-chat-composer-mobile-pending-actions="true"');
    expect(editorProps()["className"]).toBe("max-sm:pb-11");
  });

  it("expands on focus capture and collapses after blur", () => {
    h.isMobile = true;
    renderComposer();
    const surface = findHost(
      (element) => element.props["data-chat-composer-mobile-collapsed"] !== undefined,
    ).props;

    // Focus from the collapsed inline controls is ignored.
    const controlsTarget = new FakeHTMLElement();
    controlsTarget.closestResult = {};
    (surface["onFocusCapture"] as (event: unknown) => void)({ target: controlsTarget });
    expect(setStateValues(STATE.focused)).not.toContain(true);

    // Any other focus expands the composer.
    const target = new FakeHTMLElement();
    (surface["onFocusCapture"] as (event: unknown) => void)({ target });
    expect(setStateValues(STATE.focused)).toContain(true);

    // Blur schedules a collapse check on the next frame.
    documentStub.activeElement = null;
    (surface["onBlurCapture"] as () => void)();
    runAnimationFrames();
    expect(setStateValues(STATE.focused)).toContain(false);
  });

  it("keeps the composer expanded while focus sits in a floating layer", () => {
    h.isMobile = true;
    renderComposer();
    const surface = findHost(
      (element) => element.props["data-chat-composer-mobile-collapsed"] !== undefined,
    ).props;

    const floating = new FakeElement();
    floating.closestResult = {};
    documentStub.activeElement = floating;
    (surface["onBlurCapture"] as () => void)();
    runAnimationFrames();

    expect(setStateValues(STATE.focused)).not.toContain(false);
  });

  it("skips collapse checks entirely on desktop", () => {
    renderComposer();
    const surface = findHost(
      (element) => element.props["data-chat-composer-mobile-collapsed"] !== undefined,
    ).props;

    (surface["onBlurCapture"] as () => void)();
    expect(rafCallbacks).toHaveLength(0);
  });

  it("cancels queued animation frames on unmount", () => {
    h.isMobile = true;
    renderComposer();
    const expand = hostByLabel("Expand composer");
    (expand["onClick"] as () => void)();

    runCleanups();
    expect(windowStub.cancelAnimationFrame).toHaveBeenCalled();
  });
});

// ---------------------------------------------------------------------------
// Effects
// ---------------------------------------------------------------------------

describe("ChatComposer effects", () => {
  it("synchronizes parent refs after render", () => {
    const context = makeTerminalContext("ctx-sync");
    draftStore().setTerminalContexts(threadRef, [context]);
    seedPrompt(`${INLINE_TERMINAL_CONTEXT_PLACEHOLDER} sync me`);
    const element = makeElementContext("el-sync");
    draftStore().setElementContexts(threadRef, [element]);

    const { props } = renderComposer();

    expect(props.promptRef.current).toBe(`${INLINE_TERMINAL_CONTEXT_PLACEHOLDER} sync me`);
    expect(props.composerTerminalContextsRef.current.map((entry) => entry.id)).toEqual([
      "ctx-sync",
    ]);
    expect(props.composerElementContextsRef.current.map((entry) => entry.id)).toEqual(["el-sync"]);
  });

  it("adopts the pending custom answer and skips redundant re-syncs", () => {
    const { props } = renderComposer({
      pendingUserInputs: [makePendingUserInput()],
      activePendingProgress: makePendingProgress({ customAnswer: "draft answer" }),
    });

    expect(props.promptRef.current).toBe("draft answer");
    expect(setStateValues(STATE.highlightedItemId)).toContain(null);

    const callsBefore = h.setStateCalls.length;
    reflushExecutedEffects();
    // The second pass hits the "nothing changed" early return for the pending
    // input sync (other effects may still re-fire their setters).
    expect(props.promptRef.current).toBe("draft answer");
    expect(h.setStateCalls.length).toBeGreaterThanOrEqual(callsBefore);
  });

  it("clears persisted attachments when the draft has no images", async () => {
    draftStore().syncPersistedAttachments(threadRef, [
      {
        type: "image",
        id: "stale",
        name: "stale.png",
        mimeType: "image/png",
        sizeBytes: 1,
        dataUrl: "data:image/png;base64,x",
      },
    ]);

    renderComposer();
    await flushMicrotasks();

    expect(draftOf(threadRef)?.persistedAttachments ?? []).toEqual([]);
  });

  it("measures footer compactness when the form element is attached", () => {
    renderComposer();
    // Re-run the layout effect with an attached form element.
    const form = findHost((element) => element.type === "form");
    const formRef = form.props["ref"] as { current: unknown } | undefined;
    expect(formRef).toBeDefined();

    const observed: Array<() => void> = [];
    class FakeResizeObserver {
      private readonly callback: () => void;
      observe = vi.fn();
      disconnect = vi.fn();
      constructor(callback: () => void) {
        this.callback = callback;
        observed.push(() => this.callback());
      }
    }
    vi.stubGlobal("ResizeObserver", FakeResizeObserver);
    formRef!.current = { clientWidth: 200 };
    reflushExecutedEffects();

    expect(setStateValues(STATE.footerCompact)).toContain(true);
    // The observer re-measures on resize.
    expect(observed.length).toBeGreaterThan(0);
    for (const trigger of observed) trigger();
    runCleanups();
  });
});

describe("queued steer shortcut", () => {
  it.each(
    ["MacIntel", "Win32"].flatMap((platform) => [false, true].map((menu) => ({ platform, menu }))),
  )(
    "consumes Mod+Shift+Enter before menu selection or newline on $platform (menu $menu)",
    ({ platform, menu }) => {
      const platformSpy = vi.spyOn(navigator, "platform", "get").mockReturnValue(platform);
      try {
        seedPrompt(menu ? ":model" : "draft unchanged");
        const onSteerQueuedMessage = vi.fn();
        const keybindings: ResolvedKeybindingsConfig = [
          {
            command: "thread.steerQueuedMessage",
            shortcut: {
              key: "enter",
              modKey: true,
              shiftKey: true,
              metaKey: false,
              ctrlKey: false,
              altKey: false,
            },
            whenAst: { type: "identifier", name: "editableFocus" },
          },
        ];
        const { spies } = renderComposer({ onSteerQueuedMessage, keybindings });
        const onKey = editorProps()["onCommandKeyDown"] as CommandKey;
        const event = {
          key: "Enter",
          shiftKey: true,
          ctrlKey: platform === "Win32",
          metaKey: platform === "MacIntel",
          altKey: false,
          preventDefault: vi.fn(),
        } as unknown as KeyboardEvent;
        expect(onKey("Enter", event)).toBe(true);
        expect(event.preventDefault).toHaveBeenCalled();
        expect(onSteerQueuedMessage).toHaveBeenCalledOnce();
        expect(spies.onSend).not.toHaveBeenCalled();
        expect(draftOf(threadRef)?.prompt).toBe(menu ? ":model" : "draft unchanged");
      } finally {
        platformSpy.mockRestore();
      }
    },
  );
});
