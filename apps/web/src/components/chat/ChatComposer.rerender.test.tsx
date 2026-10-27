// @vitest-environment happy-dom

import {
  EnvironmentId,
  ProjectId,
  ProviderDriverKind,
  ProviderInstanceId,
  type ResolvedKeybindingsConfig,
  type ServerProvider,
  ThreadId,
} from "@coddy/contracts";
import { DEFAULT_UNIFIED_SETTINGS } from "@coddy/contracts/settings";
import { scopeThreadRef } from "@coddy/client-runtime/environment";
import { act, type RefObject } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const h = vi.hoisted(() => ({
  capabilityCatalogs: [] as import("@coddy/contracts").ServerProvider[],
  editorProps: null as Record<string, unknown> | null,
  menuProps: null as Record<string, unknown> | null,
  providerPickerProps: null as Record<string, unknown> | null,
  resolverInputs: [] as Array<{
    items: ReadonlyArray<{ id: string }>;
    highlightedItemId: string | null;
    currentSearchKey: string | null;
    highlightedSearchKey: string | null;
    preferredItemId?: string | null;
  }>,
  snapshot: {
    value: "",
    cursor: 0,
    expandedCursor: 0,
    terminalContextIds: [] as string[],
  },
  editorHandle: {
    focus: vi.fn(),
    focusAt: vi.fn(),
    focusAtEnd: vi.fn(),
    readSnapshot: vi.fn(() => h.snapshot),
  },
}));

function passthrough(tag: "button" | "div" | "span" = "div") {
  return function Passthrough(props: Record<string, unknown>) {
    const { children, render: _render, ...rest } = props;
    return React.createElement(tag, rest, children as React.ReactNode);
  };
}

import * as React from "react";

vi.mock("../ui/separator", () => ({ Separator: passthrough("span") }));
vi.mock("../ui/button", () => ({ Button: passthrough("button") }));
vi.mock("../ui/select", () => ({
  Select: passthrough(),
  SelectItem: passthrough(),
  SelectPopup: passthrough(),
  SelectTrigger: passthrough("button"),
  SelectValue: passthrough("span"),
}));
vi.mock("../ui/tooltip", () => ({
  Tooltip: passthrough(),
  TooltipPopup: passthrough(),
  TooltipTrigger: passthrough(),
}));
vi.mock("../ui/toast", () => ({ toastManager: { add: vi.fn() } }));

vi.mock("../ComposerPromptEditor", () => ({
  ComposerPromptEditor: (props: Record<string, unknown>) => {
    h.editorProps = props;
    const editorRef = props["editorRef"] as { current: unknown } | undefined;
    if (editorRef) {
      editorRef.current = h.editorHandle;
    }
    return React.createElement("div", {
      "data-mock": "composer-prompt-editor",
      "data-value": String(props["value"]),
      "data-cursor": String(props["cursor"]),
    });
  },
}));

vi.mock("./ComposerCommandMenu", () => ({
  ComposerCommandMenu: (props: Record<string, unknown>) => {
    h.menuProps = props;
    return React.createElement("div", {
      "data-mock": "composer-command-menu",
      "data-active": String(props["activeItemId"]),
    });
  },
}));

vi.mock("./ProviderModelPicker", () => ({
  ProviderModelPicker: (props: Record<string, unknown>) => {
    h.providerPickerProps = props;
    return React.createElement("div", {
      "data-mock": "provider-model-picker",
      "data-instance": String(props["activeInstanceId"]),
    });
  },
}));
vi.mock("./ComposerPendingApprovalActions", () => ({
  ComposerPendingApprovalActions: passthrough(),
}));
vi.mock("./ComposerPrimaryActions", () => ({
  ComposerPrimaryActions: passthrough(),
}));
vi.mock("./ComposerPendingApprovalPanel", () => ({
  ComposerPendingApprovalPanel: passthrough(),
}));
vi.mock("./ComposerPendingUserInputPanel", () => ({
  ComposerPendingUserInputPanel: passthrough(),
}));
vi.mock("./ComposerPlanFollowUpBanner", () => ({
  ComposerPlanFollowUpBanner: passthrough(),
}));
vi.mock("./ComposerPendingElementContexts", () => ({
  ComposerPendingElementContexts: passthrough(),
}));
vi.mock("./ComposerPendingReviewComments", () => ({
  ComposerPendingReviewComments: passthrough(),
}));
vi.mock("./ComposerPreviewAnnotationCards", () => ({
  ComposerPreviewAnnotationCards: passthrough(),
}));
vi.mock("./ContextWindowMeter", () => ({ ContextWindowMeter: passthrough() }));

vi.mock("../../state/providerCapabilities", () => ({
  useProviderCapabilities: (target: { instanceId: string }) => ({
    data: {
      ...h.capabilityCatalogs.find((provider) => provider.instanceId === target.instanceId),
      issues: [],
    },
    error: null,
    isPending: false,
    refresh: vi.fn(),
    revalidateIfStale: vi.fn(),
  }),
}));

vi.mock("../../lib/composerPathSearchState", () => ({
  useComposerPathSearch: () => ({ entries: [], error: null, isPending: false }),
}));
vi.mock("../../hooks/useMediaQuery", () => ({
  useMediaQuery: () => false,
  useIsMobile: () => false,
}));
vi.mock("../../state/threads", () => ({
  environmentThreadDetails: { detailAtom: () => ({}) },
}));

vi.mock("./composerMenuHighlight", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./composerMenuHighlight")>();
  return {
    ...actual,
    resolveComposerMenuActiveItemId: (input: (typeof h.resolverInputs)[number]): string | null => {
      h.resolverInputs.push(input);
      return actual.resolveComposerMenuActiveItemId(input);
    },
  };
});

import { type ChatComposerHandle, type ChatComposerProps, ChatComposer } from "./ChatComposer";
import { useComposerDraftStore } from "../../composerDraftStore";
import type { Thread } from "../../types";

const environmentId = EnvironmentId.make("environment-local");
const projectId = ProjectId.make("project-1");
const threadId = ThreadId.make("thread-1");
const threadRef = scopeThreadRef(environmentId, threadId);
const codexInstanceId = ProviderInstanceId.make("codex");
const claudeInstanceId = ProviderInstanceId.make("claude-work");
const now = "2026-07-23T00:00:00.000Z";
const prompt = "ask $refactor then $ref";
const supportedDraftCursor = "ask ".length + 1 + " then $ref".length;
const expandedDraftCursor = prompt.length;
const emptyKeybindings = [] as unknown as ResolvedKeybindingsConfig;

const supportedProvider: ServerProvider = {
  instanceId: codexInstanceId,
  driver: ProviderDriverKind.make("codex"),
  enabled: true,
  installed: true,
  version: "1.0.0",
  status: "ready",
  auth: { status: "authenticated" },
  checkedAt: now,
  models: [{ slug: "gpt-5.4", name: "GPT-5.4", isCustom: false, capabilities: null }],
  slashCommands: [],
  skills: [
    {
      name: "refactor",
      path: "/skills/refactor",
      enabled: true,
      invocation: "dollar",
    },
  ],
  agents: [],
};

const unsupportedProvider: ServerProvider = {
  instanceId: claudeInstanceId,
  driver: ProviderDriverKind.make("claudeAgent"),
  enabled: true,
  installed: true,
  version: "1.0.0",
  status: "ready",
  auth: { status: "authenticated" },
  checkedAt: now,
  models: [
    {
      slug: "claude-sonnet-5",
      name: "Claude Sonnet 5",
      isCustom: false,
      capabilities: null,
    },
  ],
  slashCommands: [],
  skills: [],
  agents: [],
};

const planDisabledCodexProvider: ServerProvider = {
  ...supportedProvider,
  instanceId: ProviderInstanceId.make("codex-work"),
  showInteractionModeToggle: false,
};

function makeThread(): Thread {
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
  };
}

function makeProps(
  providerStatuses: ServerProvider[],
  composerRef: RefObject<ChatComposerHandle | null>,
  promptRef: RefObject<string>,
  selectedProviderStatus: ServerProvider,
): ChatComposerProps {
  h.capabilityCatalogs = providerStatuses;
  return {
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
    providerBindingInstanceId: selectedProviderStatus.instanceId,
    lockProviderPickerToActiveInstance: false,
    providerBindingConflictReason: null,
    providerStatuses,
    activeProjectDefaultModelSelection: {
      instanceId: selectedProviderStatus.instanceId,
      model: selectedProviderStatus.models[0]!.slug,
    },
    activeThreadModelSelection: null,
    activeThreadActivities: [],
    resolvedTheme: "dark",
    settings: DEFAULT_UNIFIED_SETTINGS,
    keybindings: emptyKeybindings,
    gitCwd: "/repo",
    promptRef,
    composerAttachmentsRef: { current: [] },
    composerTerminalContextsRef: { current: [] },
    composerElementContextsRef: { current: [] },
    composerRef,
    onSend: vi.fn(),
    onInterrupt: vi.fn(),
    onImplementPlanInNewThread: vi.fn(),
    onRespondToApproval: vi.fn(async () => undefined),
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

interface ResettableStore {
  getInitialState: () => object;
  setState: (state: object, replace: true) => void;
}

const resettableComposerStore = useComposerDraftStore as unknown as ResettableStore;
const pristineComposerState = { ...resettableComposerStore.getInitialState() };

let root: Root;
let container: HTMLDivElement;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  resettableComposerStore.setState({ ...pristineComposerState }, true);
  h.editorProps = null;
  h.menuProps = null;
  h.providerPickerProps = null;
  h.resolverInputs.length = 0;
  h.snapshot = {
    value: prompt,
    cursor: supportedDraftCursor,
    expandedCursor: expandedDraftCursor,
    terminalContextIds: [],
  };
  vi.clearAllMocks();
  useComposerDraftStore.getState().setPrompt(threadRef, prompt);
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

describe("ChatComposer provider selection rerenders", () => {
  it("recomputes provider state and closes a live menu across a real provider switch", async () => {
    const composerRef = React.createRef<ChatComposerHandle>();
    const promptRef = { current: prompt };

    await act(async () => {
      root.render(
        <ChatComposer
          {...makeProps(
            [supportedProvider, unsupportedProvider],
            composerRef,
            promptRef,
            supportedProvider,
          )}
        />,
      );
    });
    await act(async () => {
      const onChange = h.editorProps?.["onChange"];
      expect(onChange).toBeTypeOf("function");
      (
        onChange as (
          value: string,
          cursor: number,
          expandedCursor: number,
          cursorAdjacentToMention: boolean,
          terminalContextIds: string[],
        ) => void
      )(prompt, supportedDraftCursor, expandedDraftCursor, false, []);
    });

    expect(container.querySelector('[data-mock="composer-command-menu"]')).not.toBeNull();
    expect(h.menuProps?.["activeItemId"]).toBe("provider-skill:codex:dollar:refactor");
    expect(h.editorProps?.["cursor"]).toBe(supportedDraftCursor);
    expect(h.providerPickerProps?.["activeInstanceId"]).toBe(codexInstanceId);
    expect(composerRef.current?.getSendContext()).toMatchObject({
      selectedProvider: ProviderDriverKind.make("codex"),
      selectedModelSelection: { instanceId: codexInstanceId },
    });

    h.resolverInputs.length = 0;
    await act(async () => {
      root.render(
        <ChatComposer
          {...makeProps(
            [supportedProvider, unsupportedProvider],
            composerRef,
            promptRef,
            unsupportedProvider,
          )}
        />,
      );
    });

    expect(container.querySelector('[data-mock="composer-command-menu"]')).toBeNull();
    expect(h.resolverInputs.at(-1)).toMatchObject({
      highlightedItemId: null,
      highlightedSearchKey: null,
      currentSearchKey: null,
    });
    expect(useComposerDraftStore.getState().getComposerDraft(threadRef)?.prompt).toBe(prompt);
    expect(promptRef.current).toBe(prompt);
    expect(h.editorProps?.["value"]).toBe(prompt);
    expect(h.editorProps?.["cursor"]).toBe(expandedDraftCursor);
    expect(h.providerPickerProps?.["activeInstanceId"]).toBe(claudeInstanceId);
    expect(composerRef.current?.getSendContext()).toMatchObject({
      selectedProvider: ProviderDriverKind.make("claudeAgent"),
      selectedModelSelection: { instanceId: claudeInstanceId },
    });
  });

  it("recomputes Plan availability when switching between instances of one driver", async () => {
    const composerRef = React.createRef<ChatComposerHandle>();
    const promptRef = { current: prompt };
    const statuses = [supportedProvider, planDisabledCodexProvider];

    await act(async () => {
      root.render(
        <ChatComposer {...makeProps(statuses, composerRef, promptRef, supportedProvider)} />,
      );
    });
    expect(container.textContent).not.toContain("Plan mode is not supported");

    await act(async () => {
      root.render(
        <ChatComposer
          {...makeProps(statuses, composerRef, promptRef, planDisabledCodexProvider)}
        />,
      );
    });

    expect(container.textContent).toContain("Plan mode is not supported");
  });
});
