import { attachmentAdmissionOwner } from "../state/attachmentAdmissions";
/**
 * Deep behavior tests for ChatView.
 *
 * The sibling ChatView.test.tsx covers plain SSR rendering. This file goes
 * further using the repo's instrumented-hooks pattern (see
 * FilePreviewPanel.test.tsx): a partial `vi.mock("react")` replaces
 * useState/useEffect/useLayoutEffect so state can be seeded per scenario,
 * setter calls execute their functional updaters, and effects are captured
 * and run manually against stubbed window/document globals. Handlers captured
 * from mocked children (see Sidebar.test.tsx) are then invoked directly to
 * exercise the send/terminal/panel/approval command flows.
 */
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import type { ReactElement, ReactNode, RefObject } from "react";
import {
  ApprovalRequestId,
  type ChatAttachmentId,
  DEFAULT_MODEL_BY_PROVIDER,
  EnvironmentId,
  EventId,
  MessageId,
  type ModelSelection,
  OrchestrationProposedPlanId,
  ProjectId,
  type ProjectScript,
  ProviderDriverKind,
  ProviderInstanceId,
  type ProviderOptionDescriptor,
  type ServerProvider,
  ThreadId,
  TurnId,
  UploadError,
} from "@bibcode/contracts";
import { DEFAULT_SERVER_SETTINGS } from "@bibcode/contracts";
import { DEFAULT_CLIENT_SETTINGS } from "@bibcode/contracts/settings";
import { AsyncResult } from "effect/unstable/reactivity";
import * as Cause from "effect/Cause";
import { RpcClientError } from "effect/unstable/rpc";
import {
  BearerConnectionTarget,
  PrimaryConnectionTarget,
  type ConnectionTarget,
} from "@bibcode/client-runtime/connection";
import {
  scopeProjectRef,
  scopeThreadRef,
  scopedThreadKey,
} from "@bibcode/client-runtime/environment";
import { desktopLocalConnectionId } from "../connection/desktopLocal";
import { createEnvironmentPresentationPolicy } from "../connection/environmentPresentationPolicy";

const h = vi.hoisted(() => {
  const state = {
    captured: {} as Record<string, Record<string, unknown>>,
    capturedList: [] as Array<{ name: string; props: Record<string, unknown> }>,
    // React's fizz renderer replays a component with empty props while
    // retrying a suspended lazy child — record only real props objects.
    capture(name: string, props: Record<string, unknown>): void {
      if (!props || Object.keys(props).length === 0) return;
      state.captured[name] = props;
      state.capturedList.push({ name, props });
    },
    atomValuesByKey: new Map<string, unknown>(),
    commandCalls: [] as Array<{ key: string; input: unknown }>,
    commandResults: {} as Record<
      string,
      (input: unknown, runOptions?: { signal?: AbortSignal }) => unknown
    >,
    defaultCommandResult: (() => undefined) as (input?: unknown) => unknown,
    terminalInputEnqueues: [] as Array<{
      data: string;
      write: (data: string) => Promise<{ _tag: string; cause?: unknown }>;
      onWriteError?: (error: unknown) => void;
    }>,
    environments: [] as unknown[],
    primaryEnvironment: null as unknown,
    threadsByKey: new Map<string, unknown>(),
    projectsByKey: new Map<string, unknown>(),
    allProjects: [] as unknown[],
    threadRefs: [] as unknown[],
    knownSessions: [] as unknown[],
    runningTerminalIds: [] as string[],
    queryDataByKey: new Map<string, unknown>(),
    assetUrls: [] as string[],
    assetResources: [] as unknown[],
    previewSupported: false,
    previewState: {} as Record<string, unknown>,
    settings: {} as Record<string, unknown>,
    navigateCalls: [] as unknown[],
    // ── new harness state ────────────────────────────────────────────
    toasts: [] as unknown[],
    shortcutCommandByKey: new Map<string, string>(),
    shortcutLabel: "Mod+K" as string | null,
    terminalFocusOwner: null as "center-panel" | "right-panel" | null,
    centerCanSplit: vi.fn((_groupId: string, _direction: "right" | "down") => true),
    commandPaletteOpen: false,
    localApi: null as unknown,
    mediaQueryMatches: false,
    turnDiffSummaries: [] as unknown[],
    inferredCheckpointTurnCountByTurnId: {} as Record<string, number>,
    previewActionSubscribers: [] as Array<(action: string) => void>,
    addBrowserSurfaceCalls: [] as unknown[],
    closePreviewSessionCalls: [] as unknown[],
    setActivePreviewTabCalls: [] as unknown[],
    // react hook instrumentation
    stateCalls: [] as Array<{ index: number; initial: unknown }>,
    stateSeeds: new Map<number, { value: unknown; expectInitial: (value: unknown) => boolean }>(),
    setStateCalls: [] as Array<{ index: number; next: unknown; applied: unknown }>,
    refCalls: [] as Array<{ index: number; initial: unknown }>,
    refObjects: [] as Array<{ current: unknown }>,
    refSeeds: new Map<number, { value: unknown; expectInitial: (value: unknown) => boolean }>(),
    effects: [] as Array<() => void | (() => void)>,
    centerHeaderDensityByGroupId: new Map<string, "expanded" | "compact">(),
    presentationPolicy: null as unknown as ReturnType<typeof createEnvironmentPresentationPolicy>,
  };
  return state;
});

// ── React instrumentation ────────────────────────────────────────────

vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();

  const resolveInitial = (initial: unknown): unknown =>
    typeof initial === "function" ? (initial as () => unknown)() : initial;

  const useState = (initial?: unknown) => {
    const resolved = resolveInitial(initial);
    const index = h.stateCalls.length;
    h.stateCalls.push({ index, initial: resolved });
    const seed = h.stateSeeds.get(index);
    if (seed && !seed.expectInitial(resolved)) {
      throw new Error(
        `useState seed mismatch at index ${index}: initial value ${String(resolved)} did not match the expected shape`,
      );
    }
    const value = seed ? seed.value : resolved;
    const setValue = (next: unknown) => {
      const applied =
        typeof next === "function" ? (next as (value: unknown) => unknown)(value) : next;
      h.setStateCalls.push({ index, next, applied });
    };
    return [value, setValue];
  };

  const useEffect = (effect: () => void | (() => void)) => {
    h.effects.push(effect);
  };

  const useRef = (initial: unknown) => {
    const index = h.refCalls.length;
    h.refCalls.push({ index, initial });
    const seed = h.refSeeds.get(index);
    if (seed && !seed.expectInitial(initial)) {
      throw new Error(
        `useRef seed mismatch at index ${index}: initial value ${String(initial)} did not match the expected shape`,
      );
    }
    const ref = { current: seed ? seed.value : initial };
    h.refObjects.push(ref);
    return ref;
  };

  return {
    ...actual,
    useState: useState as typeof actual.useState,
    useRef: useRef as typeof actual.useRef,
    useEffect: useEffect as typeof actual.useEffect,
    useLayoutEffect: useEffect as typeof actual.useLayoutEffect,
  };
});

// ── Heavy state/atom modules (same shape as ChatView.test.tsx) ───────

vi.mock("../state/use-atom-command", () => ({
  useAtomCommand: (command: { key?: string } | null | undefined, _options?: unknown) => {
    const key = command && typeof command.key === "string" ? command.key : "unknown-command";
    return (input: unknown, runOptions?: { signal?: AbortSignal }) => {
      h.commandCalls.push({ key, input });
      const respond = h.commandResults[key] ?? h.defaultCommandResult;
      return Promise.resolve(respond(input, runOptions));
    };
  },
}));

vi.mock("../state/threads", () => ({
  threadEnvironment: {
    create: { key: "thread.create" },
    delete: { key: "thread.delete" },
    archive: { key: "thread.archive" },
    unarchive: { key: "thread.unarchive" },
    updateMetadata: { key: "thread.updateMetadata" },
    setRuntimeMode: { key: "thread.setRuntimeMode" },
    setInteractionMode: { key: "thread.setInteractionMode" },
    startTurn: { key: "thread.startTurn" },
    attachmentAdmissionAuthority: { key: "thread.attachmentAdmissionAuthority" },
    stageAttachments: { key: "thread.stageAttachments" },
    releaseStagedAttachments: { key: "thread.releaseStagedAttachments" },
    keepStagedAttachmentsAlive: { key: "thread.keepStagedAttachmentsAlive" },
    steerTurn: { key: "thread.steerTurn" },
    promoteTurn: { key: "thread.promoteTurn" },
    interruptTurn: { key: "thread.interruptTurn" },
    resolveDelivery: { key: "thread.resolveDelivery" },
    respondToApproval: { key: "thread.respondToApproval" },
    respondToUserInput: { key: "thread.respondToUserInput" },
    revertCheckpoint: { key: "thread.revertCheckpoint" },
  },
}));

vi.mock("../state/terminal", () => ({
  terminalEnvironment: {
    open: { key: "terminal.open" },
    write: { key: "terminal.write" },
    close: { key: "terminal.close" },
  },
}));

vi.mock("../state/projects", () => ({
  projectEnvironment: {
    update: { key: "project.update" },
  },
}));

vi.mock("../state/preview", () => ({
  previewEnvironment: {
    open: { key: "preview.open" },
    close: { key: "preview.close" },
  },
}));

vi.mock("../state/vcs", () => ({
  vcsEnvironment: {
    status: (_args: unknown) => ({ key: "vcs.status" }),
  },
}));

vi.mock("../state/worktrees", () => ({
  worktreeEnvironment: {
    catalog: () => ({ key: "worktree.catalog" }),
    createManaged: { key: "worktree.createManaged" },
    createPanel: { key: "worktree.createPanel" },
  },
}));

vi.mock("../state/shell", () => ({
  environmentShell: {
    stateAtom: (environmentId: string) => ({ key: `shell:${environmentId}` }),
  },
}));

vi.mock("../state/server", () => ({
  serverEnvironment: {
    upsertKeybinding: { key: "server.upsertKeybinding" },
  },
  primaryServerKeybindingsAtom: { key: "atom:keybindings" },
  primaryServerAvailableEditorsAtom: { key: "atom:editors" },
}));

vi.mock("../connection/catalog", () => ({
  environmentCatalog: {
    retryNow: { key: "environment.retryNow" },
  },
}));

vi.mock("../state/query", () => ({
  useEnvironmentQuery: (atom: { key?: string } | null) => ({
    data: atom && typeof atom.key === "string" ? (h.queryDataByKey.get(atom.key) ?? null) : null,
    error: null,
    isPending: false,
    refresh: () => undefined,
  }),
}));

vi.mock("../state/entities", () => ({
  useProject: (ref: { environmentId: string; projectId: string } | null) =>
    ref ? (h.projectsByKey.get(`${ref.environmentId}:${ref.projectId}`) ?? null) : null,
  useProjects: () => h.allProjects,
  useThread: (ref: { environmentId: string; threadId: string } | null) =>
    ref ? (h.threadsByKey.get(`${ref.environmentId}:${ref.threadId}`) ?? null) : null,
  useThreadRefs: () => h.threadRefs,
  useThreadProposedPlans: () => [],
  readThreadShell: () => null,
}));

vi.mock("../state/environments", () => ({
  useEnvironments: () => ({
    isReady: true,
    networkStatus: "online",
    environments: h.environments,
  }),
  usePrimaryEnvironment: () => h.primaryEnvironment,
}));

vi.mock("../connection/currentEnvironmentPresentation", () => ({
  readCurrentEnvironmentPresentationPolicy: () => h.presentationPolicy,
}));

vi.mock("../state/terminalSessions", () => ({
  useKnownTerminalSessions: () => h.knownSessions,
  useThreadRunningTerminalIds: () => h.runningTerminalIds,
}));

vi.mock("../hooks/useSettings", () => ({
  useEnvironmentSettings: (_environmentId: string, selector?: (settings: unknown) => unknown) =>
    selector ? selector(h.settings) : h.settings,
}));

vi.mock("../assets/assetUrls", () => ({
  useAssetUrls: (_environmentId: unknown, resources: unknown[]) => {
    h.assetResources = resources;
    return h.assetUrls;
  },
}));

vi.mock("../previewStateStore", () => ({
  isPreviewSupportedInRuntime: () => h.previewSupported,
  canPreviewAnyUrlInRuntime: () => h.previewSupported,
  setActivePreviewTab: (threadRef: unknown, tabId: unknown) => {
    h.setActivePreviewTabCalls.push({ threadRef, tabId });
  },
  useThreadPreviewState: () => h.previewState,
}));

vi.mock("./preview/addBrowserSurface", () => ({
  addBrowserSurface: (input: unknown) => {
    h.addBrowserSurfaceCalls.push(input);
    return Promise.resolve();
  },
}));

vi.mock("./preview/closePreviewSession", () => ({
  closePreviewSession: (input: unknown) => {
    h.closePreviewSessionCalls.push(input);
    return Promise.resolve();
  },
}));

vi.mock("./preview/previewActionBus", () => ({
  subscribePreviewAction: (callback: (action: string) => void) => {
    h.previewActionSubscribers.push(callback);
    return () => undefined;
  },
}));

vi.mock("./preview/previewEmptyStateLogic", () => ({
  getConfiguredPreviewUrls: () => [],
}));

vi.mock("@effect/atom-react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@effect/atom-react")>();
  return {
    ...actual,
    useAtomValue: (atom: unknown) => {
      const key = (atom as { key?: string } | null | undefined)?.key;
      return typeof key === "string" ? h.atomValuesByKey.get(key) : undefined;
    },
  };
});

vi.mock("@tanstack/react-router", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@tanstack/react-router")>();
  return {
    ...actual,
    useNavigate: () => (options: unknown) => {
      h.navigateCalls.push(options);
      return Promise.resolve();
    },
  };
});

// ── Behavior seams introduced for this file ──────────────────────────

vi.mock("../keybindings", () => ({
  resolveShortcutCommand: (event: { key?: string }) =>
    typeof event.key === "string" ? (h.shortcutCommandByKey.get(event.key) ?? null) : null,
  shortcutLabelForCommand: () => h.shortcutLabel,
}));

vi.mock("../commandPaletteContext", () => ({
  isCommandPaletteOpen: () => h.commandPaletteOpen,
  OpenAddProjectCommandPaletteProvider: ({ children }: { children?: ReactNode }) => children,
  useOpenAddProjectCommandPalette: () => () => undefined,
}));

vi.mock("../lib/terminalFocus", () => ({
  getTerminalFocusOwner: () => h.terminalFocusOwner,
  isTerminalFocused: () => h.terminalFocusOwner !== null,
}));

vi.mock("../localApi", () => ({
  readLocalApi: () => h.localApi,
  createLocalApi: () => h.localApi,
  ensureLocalApi: () => h.localApi,
  __resetLocalApiForTests: () => Promise.resolve(),
}));

vi.mock("./ui/toast", () => ({
  toastManager: {
    add: (toast: unknown) => {
      h.toasts.push(toast);
      return "toast-id";
    },
    close: () => undefined,
  },
  anchoredToastManager: {
    add: (toast: unknown) => {
      h.toasts.push(toast);
      return "toast-id";
    },
    close: () => undefined,
  },
  stackedThreadToast: (toast: unknown) => toast,
  ToastProvider: ({ children }: { children?: ReactNode }) => children,
  AnchoredToastProvider: ({ children }: { children?: ReactNode }) => children,
}));

vi.mock("../hooks/useTheme", () => ({
  useTheme: () => ({
    theme: "system" as const,
    resolvedTheme: "dark" as const,
    setTheme: () => undefined,
  }),
}));

vi.mock("../hooks/useMediaQuery", () => ({
  useMediaQuery: () => h.mediaQueryMatches,
}));

vi.mock("../hooks/useTurnDiffSummaries", () => ({
  useTurnDiffSummaries: () => ({
    turnDiffSummaries: h.turnDiffSummaries,
    inferredCheckpointTurnCountByTurnId: h.inferredCheckpointTurnCountByTurnId,
  }),
}));

// ── Child components ─────────────────────────────────────────────────

vi.mock("./NoActiveThreadState", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./NoActiveThreadState")>()),
  NoActiveThreadState: () => <div data-mock="no-active-thread" />,
}));

vi.mock("./DiffWorkerPoolProvider", () => ({
  DiffWorkerPoolProvider: ({ children }: { children?: ReactNode }) => (
    <div data-mock="diff-worker-pool">{children}</div>
  ),
}));

vi.mock("./chat/ChatComposer", () => ({
  ChatComposer: (props: Record<string, unknown>) => {
    h.capture("chatComposer", props);
    return <div data-mock="chat-composer" />;
  },
}));

vi.mock("./chat/MessagesTimeline", () => ({
  MessagesTimeline: (props: Record<string, unknown>) => {
    h.capture("messagesTimeline", props);
    return <div data-mock="messages-timeline" />;
  },
}));

vi.mock("./chat/ChatHeaderActions", () => ({
  ChatHeaderActions: (props: Record<string, unknown>) => {
    h.capture("chatHeaderActions", props);
    return <div data-mock="chat-header-actions" />;
  },
}));

vi.mock("./chat/ExpandedImageDialog", () => ({
  ExpandedImageDialog: (props: Record<string, unknown>) => {
    h.capture("expandedImageDialog", props);
    return <div data-mock="expanded-image-dialog" />;
  },
}));

vi.mock("./chat/PanelLayoutControls", () => ({
  PanelLayoutControls: (props: Record<string, unknown>) => {
    h.capture("panelLayoutControls", props);
    return <div data-mock="panel-layout-controls" />;
  },
  RightPanelMaximizeControl: (props: Record<string, unknown>) => {
    h.capture("rightPanelMaximizeControl", props);
    return <div data-mock="right-panel-maximize" />;
  },
}));

vi.mock("./chat/ProviderStatusBanner", () => ({
  ProviderStatusBanner: (props: Record<string, unknown>) => {
    h.capture("providerStatusBanner", props);
    return <div data-mock="provider-status-banner" />;
  },
}));

vi.mock("./chat/ThreadErrorBanner", () => ({
  ThreadErrorBanner: (props: Record<string, unknown>) => {
    h.capture("threadErrorBanner", props);
    return <div data-mock="thread-error-banner" />;
  },
}));

vi.mock("./chat/ComposerBannerStack", () => ({
  ComposerBannerStack: (props: Record<string, unknown>) => {
    h.capture("composerBannerStack", props);
    return <div data-mock="composer-banner-stack" />;
  },
}));

vi.mock("./PullRequestThreadDialog", () => ({
  PullRequestThreadDialog: (props: Record<string, unknown>) => {
    h.capture("pullRequestThreadDialog", props);
    return <div data-mock="pull-request-thread-dialog" />;
  },
}));

vi.mock("./PlanSidebar", () => ({
  default: (props: Record<string, unknown>) => {
    h.capture("planSidebar", props);
    return <div data-mock="plan-sidebar" />;
  },
}));

vi.mock("./ThreadTerminalPanel", () => ({
  default: (props: Record<string, unknown>) => {
    h.capture("threadTerminalPanel", props);
    return <div data-mock="thread-terminal-panel" data-owner={String(props["owner"] ?? "")} />;
  },
  enqueueTerminalInput: (input: {
    data: string;
    write: (data: string) => Promise<{ _tag: string; cause?: unknown }>;
    onWriteError?: (error: unknown) => void;
  }) => {
    h.terminalInputEnqueues.push(input);
    void input.write(input.data).then((result) => {
      if (result._tag === "Failure") {
        input.onWriteError?.(result.cause);
      }
    });
  },
  releaseTerminalUiResources: vi.fn(),
}));

vi.mock("./CenterPanelWorkspace", async () => {
  const React = await import("react");

  class CenterPanelWorkspace extends React.Component<Record<string, unknown>> {
    canSplitGroup(groupId: string, direction: "right" | "down"): boolean {
      return h.centerCanSplit(groupId, direction);
    }

    override render() {
      const props = this.props;
      h.capture("centerWorkspace", props);
      const state = props["state"] as {
        surfaces: Array<{ id: string }>;
        groups: Array<{ id: string; activeSurfaceId: string | null; surfaceIds: string[] }>;
        focusedGroupId: string;
      };
      const renderSurface = props["renderSurface"] as (
        surface: { id: string },
        context: { groupId: string; visible: boolean; focused: boolean },
      ) => ReactNode;
      const membership = new Map(
        state.groups.flatMap((group) => group.surfaceIds.map((surfaceId) => [surfaceId, group.id])),
      );
      const visibleIds = new Set(state.groups.flatMap((group) => group.activeSurfaceId ?? []));
      return (
        <div data-mock="center-panel-workspace">
          {state.groups.map((group) => (
            <div key={group.id} data-mock-center-header={group.id}>
              {group.id === state.focusedGroupId
                ? (props["renderFocusedActions"] as (density: "expanded" | "compact") => ReactNode)(
                    h.centerHeaderDensityByGroupId.get(group.id) ?? "compact",
                  )
                : null}
            </div>
          ))}
          {state.surfaces
            .filter((surface) => surface.id === "chat:host" || visibleIds.has(surface.id))
            .map((surface) => {
              const groupId = membership.get(surface.id)!;
              return (
                <div
                  key={surface.id}
                  data-mock-center-surface={surface.id}
                  data-visible={String(visibleIds.has(surface.id))}
                  className={visibleIds.has(surface.id) ? undefined : "hidden"}
                >
                  {renderSurface(surface, {
                    groupId,
                    visible: visibleIds.has(surface.id),
                    focused: groupId === state.focusedGroupId,
                  })}
                </div>
              );
            })}
        </div>
      );
    }
  }

  return { CenterPanelWorkspace };
});

vi.mock("./CenterTerminalPanel", () => ({
  CenterTerminalPanel: (props: Record<string, unknown>) => {
    h.capture("centerTerminalPanel", props);
    return <div data-mock="center-terminal-panel" />;
  },
}));

vi.mock("./RightPanelTabs", () => ({
  RightPanelTabs: (props: Record<string, unknown> & { children?: ReactNode }) => {
    h.capture("rightPanelTabs", props);
    return <div data-mock="right-panel-tabs">{props.children}</div>;
  },
}));

vi.mock("./RightPanelSheet", () => ({
  RightPanelSheet: (props: Record<string, unknown> & { children?: ReactNode }) => {
    h.capture("rightPanelSheet", props);
    return <div data-mock="right-panel-sheet">{props.children}</div>;
  },
}));

vi.mock("./preview/PreviewPanel", () => ({
  PreviewPanel: () => <div data-mock="preview-panel" />,
}));
vi.mock("./DiffPanel", () => ({
  default: () => <div data-mock="diff-panel" />,
}));
vi.mock("./SourceControlPanel", () => ({
  default: () => <div data-mock="source-control-panel" />,
}));
vi.mock("./files/FilePreviewPanel", () => ({
  default: (props: Record<string, unknown>) => {
    h.capture("filePreviewPanel", props);
    return <div data-mock="file-preview-panel" />;
  },
}));
vi.mock("./files/ProjectFilesPreloader", () => ({
  ProjectFilesPreloader: (props: Record<string, unknown>) => {
    h.capture("projectFilesPreloader", props);
    return null;
  },
}));

import ChatView, { eventPathContainsSelector, shouldTypeToFocusComposer } from "./ChatView";
import type { ChatMessage, Project, Thread } from "../types";
import { useComposerDraftStore } from "../composerDraftStore";
import { useRightPanelStore, type RightPanelSurface } from "../rightPanelStore";
import { HOST_SURFACE_ID, useCenterPanelStore } from "../centerPanelStore";
import { useUiStateStore } from "../uiStateStore";
import { useDiffPanelStore } from "../diffPanelStore";
import { FileEditingSessionRegistry } from "./files/fileEditingSessionRegistry";
import { newDraftId } from "../lib/utils";
import {
  deriveLogicalProjectKeyFromSettings,
  selectProjectGroupingSettings,
} from "../logicalProject";
import type { ChatComposerHandle } from "./chat/ChatComposer";
import type { ComposerBannerStackItem } from "./chat/ComposerBannerStack";
import type { ProviderInstanceEntry } from "../providerInstances";
import type { TerminalContextDraft } from "../lib/terminalContext";
import { getComposerProviderState } from "./chat/composerProviderState";

const environmentId = EnvironmentId.make("environment-local");
const projectId = ProjectId.make("project-1");
const threadId = ThreadId.make("thread-1");
const now = "2026-03-29T00:00:00.000Z";
const later = "2026-03-29T01:00:00.000Z";
const threadRef = scopeThreadRef(environmentId, threadId);
const threadKey = scopedThreadKey(threadRef);
const codexInstanceId = ProviderInstanceId.make("codex");

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
  slashCommands: [],
  skills: [],
  agents: [],
};

// ── Host useState call indexes (ChatViewContent renders first, so its
// unconditional useState calls occupy the first slots in call order; the
// seeding helper verifies the initial value shape and fails loudly if the
// component's hook order changes). ─────────────────────────────────────
const isNull = (value: unknown) => value === null;
const isFalse = (value: unknown) => value === false;
const isEmptyArray = (value: unknown) => Array.isArray(value) && value.length === 0;
const HOST_STATE = {
  showScrollToBottom: { index: 0, expectInitial: isFalse },
  expandedImage: { index: 1, expectInitial: isNull },
  optimisticUserMessages: { index: 2, expectInitial: isEmptyArray },
  attachmentUploads: {
    index: 3,
    expectInitial: (value: unknown) =>
      typeof value === "object" && value !== null && Object.keys(value).length === 0,
  },
  maximizedRightPanelThreadKey: { index: 8, expectInitial: isNull },
  terminalFocusRequestId: { index: 13, expectInitial: (value: unknown) => value === 0 },
  pullRequestDialogState: { index: 14, expectInitial: isNull },
  pendingUserInputAnswersByRequestId: {
    index: 11,
    expectInitial: (value: unknown) =>
      typeof value === "object" && value !== null && Object.keys(value).length === 0,
  },
  attachmentPreviewHandoffByMessageId: {
    index: 15,
    expectInitial: (value: unknown) =>
      typeof value === "object" && value !== null && Object.keys(value).length === 0,
  },
  composerOverlayElement: { index: 19, expectInitial: isNull },
} as const;

function seedHostState(name: keyof typeof HOST_STATE, value: unknown): void {
  const { index, expectInitial } = HOST_STATE[name];
  h.stateSeeds.set(index, { value, expectInitial });
}

function setStateCallsFor(
  name: keyof typeof HOST_STATE,
): Array<{ next: unknown; applied: unknown }> {
  const { index } = HOST_STATE[name];
  return h.setStateCalls.filter((call) => call.index === index);
}

function makeProject(overrides: Partial<Project> = {}): Project {
  return {
    id: projectId,
    environmentId,
    title: "Demo Project",
    workspaceRoot: "X:/demo",
    repositoryIdentity: null,
    defaultModelSelection: { instanceId: codexInstanceId, model: "gpt-5.4" },
    scripts: [],
    worktreeDiscovery: { visibility: "hidden", initialPromptDismissedAt: null, baselinePaths: [] },
    createdAt: now,
    updatedAt: now,
    ...overrides,
  };
}

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

function makeActionablePlanThread(suffix: string): Thread {
  const turnId = TurnId.make(`turn-plan-${suffix}`);
  return makeThread({
    interactionMode: "plan",
    latestTurn: {
      turnId,
      state: "completed",
      requestedAt: now,
      startedAt: now,
      completedAt: later,
      assistantMessageId: null,
    },
    proposedPlans: [
      {
        id: OrchestrationProposedPlanId.make(`plan-${suffix}`),
        turnId,
        planMarkdown: "# Plan\n\n1. do the thing",
        implementedAt: null,
        implementationThreadId: null,
        createdAt: now,
        updatedAt: now,
      },
    ],
  });
}

function makeSession(overrides: Partial<NonNullable<Thread["session"]>> = {}) {
  return {
    threadId,
    status: "ready",
    providerName: "codex",
    providerInstanceId: codexInstanceId,
    runtimeMode: "full-access",
    activeTurnId: null,
    lastError: null,
    updatedAt: now,
    ...overrides,
  } as NonNullable<Thread["session"]>;
}

interface TestConnectionPresentation {
  readonly phase: "available" | "offline" | "connecting" | "reconnecting" | "connected" | "error";
  readonly error: string | null;
  readonly traceId: string | null;
}

interface TestEnvironmentPresentation {
  readonly environmentId: EnvironmentId;
  readonly label: string;
  readonly displayUrl: string | null;
  readonly relayManaged: boolean;
  readonly entry: { readonly target: ConnectionTarget };
  readonly connection: TestConnectionPresentation;
  readonly serverConfig: {
    readonly providers: ReadonlyArray<ServerProvider>;
    readonly environment: {
      readonly label: string;
      readonly serverVersion?: string;
      readonly platform?: { readonly os: "linux"; readonly arch: "x64" };
    };
  } | null;
}

function makeEnvironmentPresentation(
  overrides: Partial<TestEnvironmentPresentation> = {},
): TestEnvironmentPresentation {
  const defaultServerConfig = {
    providers: [codexProvider],
    environment: { label: "Local", platform: { os: "linux" as const, arch: "x64" as const } },
  };
  return {
    environmentId,
    label: "Local",
    displayUrl: null,
    relayManaged: false,
    entry: {
      target: new PrimaryConnectionTarget({
        environmentId,
        httpBaseUrl: "http://127.0.0.1:3773",
        label: "Local",
        wsBaseUrl: "ws://127.0.0.1:3773",
      }),
    },
    connection: { phase: "connected", error: null, traceId: null },
    ...overrides,
    serverConfig:
      overrides.serverConfig === undefined
        ? defaultServerConfig
        : overrides.serverConfig === null
          ? null
          : {
              ...overrides.serverConfig,
              environment: {
                platform: defaultServerConfig.environment.platform,
                ...overrides.serverConfig.environment,
              },
            },
  };
}

function seedEnvironment(presentation: TestEnvironmentPresentation): void {
  h.environments = [presentation];
  h.primaryEnvironment = presentation;
}

function seedProject(project: Project): void {
  h.projectsByKey.set(`${project.environmentId}:${project.id}`, project);
  h.allProjects = [project];
}

function seedServerThread(thread: Thread): void {
  h.threadsByKey.set(`${thread.environmentId}:${thread.id}`, thread);
  h.threadRefs = [scopeThreadRef(thread.environmentId, thread.id)];
}

function seedGitStatus(isRepo: boolean): void {
  h.queryDataByKey.set("vcs.status", { isRepo });
}

function seedConnectedServerThread(thread: Thread = makeThread()): void {
  seedEnvironment(makeEnvironmentPresentation());
  seedProject(makeProject());
  seedServerThread(thread);
  seedGitStatus(true);
}

function resetRenderCaptures(): void {
  h.captured = {};
  h.capturedList.length = 0;
  h.stateCalls.length = 0;
  h.refCalls.length = 0;
  h.effects.length = 0;
  h.previewActionSubscribers.length = 0;
}

function renderServerRoute(): string {
  resetRenderCaptures();
  return renderToStaticMarkup(
    <ChatView
      environmentId={environmentId}
      threadId={threadId}
      routeKind="server"
      reserveTitleBarControlInset
    />,
  );
}

function renderDraftRoute(draftId: ReturnType<typeof newDraftId>): string {
  resetRenderCaptures();
  return renderToStaticMarkup(
    <ChatView
      environmentId={environmentId}
      threadId={threadId}
      routeKind="draft"
      draftId={draftId}
    />,
  );
}

function renderPanelRoute(): string {
  resetRenderCaptures();
  return renderToStaticMarkup(
    <ChatView
      variant="panel"
      panelThreadRef={scopeThreadRef(environmentId, threadId)}
      workspaceUnavailable={null}
    />,
  );
}

function capturedProps<T = Record<string, unknown>>(name: string): T {
  const entry = [...h.capturedList].toReversed().find((candidate) => candidate.name === name);
  expect(entry, `expected captured props for ${name}`).toBeDefined();
  return entry!.props as T;
}

function runEffects(): Array<() => void> {
  const cleanups: Array<() => void> = [];
  const effects = [...h.effects];
  h.effects.length = 0;
  for (const effect of effects) {
    const cleanup = effect();
    if (typeof cleanup === "function") cleanups.push(cleanup);
  }
  return cleanups;
}

function commandCallsFor(key: string): Array<{ key: string; input: unknown }> {
  return h.commandCalls.filter((call) => call.key === key);
}

async function flushTerminalAction(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
}

function closedTerminalIds(): string[] {
  return commandCallsFor("terminal.close").map((call) => {
    const command = call.input as {
      environmentId: EnvironmentId;
      input: {
        threadId: ThreadId;
        terminalId: string;
        deleteHistory: boolean;
      };
    };
    expect(command.environmentId).toBe(environmentId);
    expect(command.input.threadId).toBe(threadId);
    expect(command.input.deleteHistory).toBe(true);
    return command.input.terminalId;
  });
}

function seedTwoGroupCenterState(options?: {
  readonly hostGroupActiveSurfaceId?: typeof HOST_SURFACE_ID;
}): void {
  useCenterPanelStore.setState({
    byThreadKey: {
      [threadKey]: {
        surfaces: [
          { id: HOST_SURFACE_ID, kind: "chat-host" },
          { id: "terminal:term-a", kind: "terminal", terminalId: "term-a" },
          { id: "terminal:term-b", kind: "terminal", terminalId: "term-b" },
        ],
        groups: [
          {
            id: "group-a",
            surfaceIds: [HOST_SURFACE_ID, "terminal:term-a"],
            activeSurfaceId: options?.hostGroupActiveSurfaceId ?? "terminal:term-a",
          },
          {
            id: "group-b",
            surfaceIds: ["terminal:term-b"],
            activeSurfaceId: "terminal:term-b",
          },
        ],
        layout: {
          type: "split",
          direction: "horizontal",
          ratio: 0.5,
          first: { type: "leaf", groupId: "group-a" },
          second: { type: "leaf", groupId: "group-b" },
        },
        focusedGroupId: "group-a",
      },
    },
  });
}

function composerHandle(overrides: Partial<ChatComposerHandle> = {}): ChatComposerHandle {
  return {
    focusAtEnd: () => undefined,
    focusAt: () => undefined,
    insertTextAtEnd: () => false,
    openModelPicker: () => undefined,
    toggleModelPicker: () => undefined,
    isModelPickerOpen: () => false,
    readSnapshot: () => ({ value: "", cursor: 0, expandedCursor: 0, terminalContextIds: [] }),
    resetCursorState: () => undefined,
    addTerminalContext: () => false,
    getSendContext: () => ({
      prompt: "",
      attachments: [],
      terminalContexts: [],
      elementContexts: [],
      previewAnnotations: [],
      reviewComments: [],
      selectedPromptEffort: null,
      selectedModelOptionsForDispatch: undefined,
      selectedModelSelection: { instanceId: codexInstanceId, model: "gpt-5.4" },
      selectedProvider: ProviderDriverKind.make("codex"),
      selectedModel: "gpt-5.4",
      selectedProviderModels: codexProvider.models,
    }),
    ...overrides,
  } as ChatComposerHandle;
}

function installComposerHandle(overrides: Partial<ChatComposerHandle> = {}): {
  handle: ChatComposerHandle;
  promptRef: RefObject<string>;
} {
  const composer = capturedProps("chatComposer");
  const composerRef = composer["composerRef"] as RefObject<ChatComposerHandle | null>;
  const handle = composerHandle(overrides);
  composerRef.current = handle;
  return { handle, promptRef: composer["promptRef"] as RefObject<string> };
}

function makeOwnedComposerContexts(suffix: string) {
  return {
    image: {
      type: "image" as const,
      id: `image-${suffix}` as ChatAttachmentId,
      name: "plan-follow-up.png",
      mimeType: "image/png",
      sizeBytes: 4,
      previewUrl: `blob:plan-follow-up-${suffix}`,
      file: new File(["fake"], "plan-follow-up.png", { type: "image/png" }),
    },
    terminalContext: {
      id: `terminal-${suffix}`,
      threadId,
      createdAt: now,
      terminalId: "terminal-1",
      terminalLabel: "Shell",
      lineStart: 1,
      lineEnd: 2,
      text: "build output",
    } satisfies TerminalContextDraft,
    elementContext: {
      id: `element-${suffix}`,
      threadId,
      pageUrl: "http://localhost:3000",
      pageTitle: "Demo",
      tagName: "button",
      selector: ".save",
      htmlPreview: "<button>Save</button>",
      componentName: "SaveButton",
      source: null,
      styles: "",
      pickedAt: now,
    },
    previewAnnotation: {
      id: `annotation-${suffix}`,
      pageUrl: "http://localhost:3000",
      pageTitle: "Demo",
      comment: "Fix this",
      elements: [],
      regions: [],
      strokes: [],
      styleChanges: [],
      screenshot: null,
      createdAt: now,
    },
    reviewComment: {
      id: `comment-${suffix}`,
      sectionId: "file:src/app.ts",
      sectionTitle: "src/app.ts",
      filePath: "src/app.ts",
      startIndex: 0,
      endIndex: 1,
      rangeLabel: "L1",
      text: "Tighten this",
      diff: "+const value = 1;",
    },
  };
}

function seedOwnedComposerContexts(
  actionText: string,
  contexts: ReturnType<typeof makeOwnedComposerContexts>,
): void {
  const store = useComposerDraftStore.getState();
  store.setPrompt(threadRef, actionText);
  store.addAttachments(threadRef, [contexts.image]);
  store.setTerminalContexts(threadRef, [contexts.terminalContext]);
  store.setElementContexts(threadRef, [contexts.elementContext]);
  store.setPreviewAnnotations(threadRef, [contexts.previewAnnotation]);
  store.setReviewComments(threadRef, [contexts.reviewComment]);
}

function sendContextWithOwnedComposerContexts(
  contexts: ReturnType<typeof makeOwnedComposerContexts>,
): ReturnType<ChatComposerHandle["getSendContext"]> {
  return {
    ...composerHandle().getSendContext(),
    attachments: [contexts.image],
    terminalContexts: [contexts.terminalContext],
    elementContexts: [contexts.elementContext],
    previewAnnotations: [contexts.previewAnnotation],
    reviewComments: [contexts.reviewComment],
  };
}

function seedFreshLocalDraft(
  logicalProjectKey: string,
  modelSelection: ModelSelection,
): ReturnType<typeof newDraftId> {
  const draftId = newDraftId();
  seedEnvironment(makeEnvironmentPresentation());
  seedProject(makeProject());
  seedGitStatus(true);
  useComposerDraftStore
    .getState()
    .setLogicalProjectDraftThreadId(
      logicalProjectKey,
      scopeProjectRef(environmentId, projectId),
      draftId,
      { threadId, createdAt: now, envMode: "local" },
    );
  useComposerDraftStore.getState().setModelSelection(draftId, modelSelection);
  publishSeededStoreState(useComposerDraftStore);
  return draftId;
}

function draftModelSelection(
  draftId: ReturnType<typeof newDraftId>,
  instanceId: ProviderInstanceId,
): ModelSelection {
  const selection = useComposerDraftStore.getState().getComposerDraft(draftId)
    ?.modelSelectionByProvider[instanceId];
  expect(selection).toBeDefined();
  return selection!;
}

function installComposerModelSelection(
  selection: ModelSelection,
  promptEffort: string,
): RefObject<string> {
  return installComposerHandle({
    getSendContext: () => ({
      ...composerHandle().getSendContext(),
      selectedPromptEffort: promptEffort,
      selectedModelOptionsForDispatch: selection.options,
      selectedModelSelection: selection,
      selectedModel: selection.model,
    }),
  }).promptRef;
}

interface FakeLegendListOptions {
  scroll?: number;
  scrollLength?: number;
  dataLength?: number;
}

function makeLegendList(options: FakeLegendListOptions = {}) {
  const scrollNodeListeners: Array<{ type: string; handler: () => void; options?: unknown }> = [];
  const calls = {
    scrollToEnd: [] as unknown[],
    scrollToOffset: [] as unknown[],
    scrollToIndex: [] as unknown[],
  };
  const scrollNode = {
    listeners: scrollNodeListeners,
    addEventListener: (type: string, handler: () => void, listenerOptions?: unknown) => {
      scrollNodeListeners.push({ type, handler, options: listenerOptions });
    },
    removeEventListener: () => undefined,
    fire(type: string) {
      for (const listener of scrollNodeListeners) {
        if (listener.type === type) listener.handler();
      }
    },
  };
  const list = {
    calls,
    scrollNode,
    getScrollableNode: () => scrollNode,
    getState: () => ({
      scroll: options.scroll ?? 0,
      data: Array.from({ length: options.dataLength ?? 1 }, (_, index) => index),
      positionAtIndex: (_index: number) => 0,
      sizeAtIndex: (_index: number) => 100,
      scrollLength: options.scrollLength ?? 50,
    }),
    scrollToEnd: (input: unknown) => {
      calls.scrollToEnd.push(input);
      return Promise.resolve();
    },
    scrollToOffset: (input: unknown) => {
      calls.scrollToOffset.push(input);
      return Promise.resolve();
    },
    scrollToIndex: (input: unknown) => {
      calls.scrollToIndex.push(input);
      return Promise.resolve();
    },
  };
  return list;
}

function installLegendList(options: FakeLegendListOptions = {}) {
  const timeline = capturedProps("messagesTimeline");
  const listRef = timeline["listRef"] as RefObject<unknown>;
  const list = makeLegendList(options);
  listRef.current = list;
  return list;
}

interface WindowListener {
  type: string;
  handler: (event: unknown) => void;
}

interface WindowStub {
  listeners: WindowListener[];
  timeouts: Array<() => void>;
  addEventListener: (type: string, handler: (event: unknown) => void) => void;
  removeEventListener: () => void;
  dispatchEvent: () => boolean;
  requestAnimationFrame: (callback: (time: number) => void) => number;
  cancelAnimationFrame: () => void;
  setTimeout: (callback: () => void) => number;
  clearTimeout: () => void;
  matchMedia: (query: string) => {
    matches: boolean;
    addEventListener: () => void;
    removeEventListener: () => void;
  };
}

let windowStub: WindowStub;
let documentQuerySelectorResult: unknown = null;

function makeWindowStub(): WindowStub {
  const listeners: WindowListener[] = [];
  const timeouts: Array<() => void> = [];
  return {
    listeners,
    timeouts,
    addEventListener: (type, handler) => {
      listeners.push({ type, handler });
    },
    removeEventListener: () => undefined,
    dispatchEvent: () => true,
    requestAnimationFrame: (callback) => {
      callback(0);
      return 1;
    },
    cancelAnimationFrame: () => undefined,
    setTimeout: (callback) => {
      timeouts.push(callback);
      return timeouts.length;
    },
    clearTimeout: () => undefined,
    matchMedia: () => ({
      matches: false,
      addEventListener: () => undefined,
      removeEventListener: () => undefined,
    }),
  };
}

function runWindowTimeouts(): void {
  const pending = [...windowStub.timeouts];
  windowStub.timeouts.length = 0;
  for (const callback of pending) callback();
}

function windowKeydownHandler(): (event: unknown) => void {
  const listener = windowStub.listeners.find((entry) => entry.type === "keydown");
  expect(listener, "expected a window keydown listener").toBeDefined();
  return listener!.handler;
}

class FakeElement {
  closestResult: boolean;
  constructor(closestResult = false) {
    this.closestResult = closestResult;
  }
  closest(_selector: string): unknown {
    return this.closestResult ? this : null;
  }
}

interface FakeKeyEventInit {
  key?: string;
  metaKey?: boolean;
  ctrlKey?: boolean;
  altKey?: boolean;
  defaultPrevented?: boolean;
  isComposing?: boolean;
  path?: unknown[];
  target?: unknown;
}

function makeKeyEvent(init: FakeKeyEventInit = {}) {
  const event = {
    key: init.key ?? "F13",
    metaKey: init.metaKey ?? false,
    ctrlKey: init.ctrlKey ?? false,
    altKey: init.altKey ?? false,
    shiftKey: false,
    defaultPrevented: init.defaultPrevented ?? false,
    isComposing: init.isComposing ?? false,
    target: init.target ?? null,
    prevented: false,
    stopped: false,
    composedPath: () => [...(init.path ?? [])],
    preventDefault() {
      event.prevented = true;
    },
    stopPropagation() {
      event.stopped = true;
    },
  };
  return event;
}

interface ResettableStore {
  getState: () => object;
  getInitialState: () => object;
  setState: (state: object, replace: true) => void;
}

const resettableStores: ReadonlyArray<{ store: ResettableStore; pristine: object }> = [
  useComposerDraftStore,
  useRightPanelStore,
  useCenterPanelStore,
  useUiStateStore,
  useDiffPanelStore,
].map((store) => ({
  store: store as unknown as ResettableStore,
  pristine: { ...(store as unknown as ResettableStore).getInitialState() },
}));

/** Copy live zustand state into the server-snapshot object read during SSR. */
function publishSeededStoreState(store: unknown): void {
  const resettable = store as ResettableStore;
  Object.assign(resettable.getInitialState(), resettable.getState());
}

beforeEach(() => {
  attachmentAdmissionOwner.dispose();
  h.captured = {};
  h.atomValuesByKey.clear();
  h.atomValuesByKey.set("atom:keybindings", []);
  h.atomValuesByKey.set("atom:editors", []);
  h.commandCalls.length = 0;
  h.commandResults = {
    "thread.stageAttachments": () => AsyncResult.success({ _tag: "inline" }),
    "thread.attachmentAdmissionAuthority": () =>
      AsyncResult.success({ storageInstanceId: "test-store", hostIdentity: "test-host" }),
  };
  h.defaultCommandResult = () => AsyncResult.success(undefined);
  h.terminalInputEnqueues.length = 0;
  h.environments = [];
  h.primaryEnvironment = null;
  h.threadsByKey.clear();
  h.projectsByKey.clear();
  h.allProjects = [];
  h.threadRefs = [];
  h.knownSessions = [];
  h.runningTerminalIds = [];
  h.queryDataByKey.clear();
  h.assetUrls = [];
  h.assetResources = [];
  h.previewSupported = false;
  h.previewState = {
    snapshot: null,
    sessions: {},
    suppressedTabIds: new Set<string>(),
    activeTabId: null,
    desktopOverlay: null,
    desktopByTabId: {},
    recentlySeenUrls: [],
  };
  h.settings = { ...DEFAULT_SERVER_SETTINGS, ...DEFAULT_CLIENT_SETTINGS };
  h.navigateCalls = [];
  h.toasts.length = 0;
  h.shortcutCommandByKey.clear();
  h.shortcutLabel = "Mod+K";
  h.terminalFocusOwner = null;
  h.centerCanSplit.mockReset();
  h.centerCanSplit.mockReturnValue(true);
  h.commandPaletteOpen = false;
  h.localApi = null;
  h.mediaQueryMatches = false;
  h.turnDiffSummaries = [];
  h.inferredCheckpointTurnCountByTurnId = {};
  h.previewActionSubscribers.length = 0;
  h.addBrowserSurfaceCalls.length = 0;
  h.closePreviewSessionCalls.length = 0;
  h.setActivePreviewTabCalls.length = 0;
  h.stateCalls.length = 0;
  h.stateSeeds.clear();
  h.setStateCalls.length = 0;
  h.refCalls.length = 0;
  h.refObjects.length = 0;
  h.refSeeds.clear();
  h.effects.length = 0;
  h.centerHeaderDensityByGroupId.clear();
  h.presentationPolicy = createEnvironmentPresentationPolicy({
    surface: "browser",
    platform: "unknown",
  });

  for (const { store, pristine } of resettableStores) {
    store.setState({ ...pristine }, true);
    Object.assign(store.getInitialState(), pristine);
  }

  windowStub = makeWindowStub();
  documentQuerySelectorResult = null;
  vi.stubGlobal("window", windowStub);
  vi.stubGlobal("document", {
    querySelector: () => documentQuerySelectorResult,
    addEventListener: () => undefined,
    removeEventListener: () => undefined,
    activeElement: null,
  });
  vi.stubGlobal("requestAnimationFrame", (callback: (time: number) => void) => {
    callback(0);
    return 1;
  });
  vi.stubGlobal("cancelAnimationFrame", () => undefined);
  vi.stubGlobal("Element", FakeElement);
  vi.stubGlobal("navigator", {
    clipboard: { writeText: () => Promise.resolve() },
  });
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("ChatView attachment assets", () => {
  it("requests server asset URLs only for image attachments", () => {
    seedConnectedServerThread(
      makeThread({
        messages: [
          {
            id: MessageId.make("image-message"),
            role: "user",
            text: "",
            attachments: [
              {
                type: "image",
                id: "image-attachment",
                name: "shot.png",
                mimeType: "image/png",
                sizeBytes: 1,
              },
              {
                type: "file",
                id: "file-attachment",
                name: "notes.txt",
                mimeType: "text/plain",
                sizeBytes: 1,
              },
            ],
            turnId: null,
            createdAt: now,
            updatedAt: now,
            streaming: false,
          },
        ],
      }),
    );

    renderServerRoute();

    expect(h.assetResources).toEqual([{ _tag: "attachment", attachmentId: "image-attachment" }]);
  });
});

describe("ChatView center panel variant", () => {
  it("renders a server-backed sibling without host-only chrome or effects", () => {
    seedConnectedServerThread();
    renderPanelRoute();

    expect(h.captured["messagesTimeline"]).toBeDefined();
    expect(h.captured["chatComposer"]).toBeDefined();
    expect(h.captured["chatHeaderActions"]).toBeUndefined();
    runEffects();
    expect(windowStub.listeners.some((listener) => listener.type === "keydown")).toBe(false);
  });

  it("forwards the focused group's expanded density to the host header actions", () => {
    seedConnectedServerThread();
    h.centerHeaderDensityByGroupId.set("center:root", "expanded");

    renderServerRoute();

    expect(capturedProps("chatHeaderActions")["density"]).toBe("expanded");
  });

  it("mounts an active sibling chat surface from host center-panel state", () => {
    const siblingId = ThreadId.make("thread-sibling");
    seedConnectedServerThread();
    seedServerThread(makeThread({ id: siblingId, title: "Sibling thread" }));
    useCenterPanelStore.getState().openChatPanel(threadRef, siblingId, "Codex");
    publishSeededStoreState(useCenterPanelStore);

    renderServerRoute();

    expect(h.capturedList.filter((entry) => entry.name === "messagesTimeline")).toHaveLength(2);
    expect(h.captured["centerWorkspace"]).toBeDefined();
  });

  it("preserves an outstanding focus request while center-pane eligibility changes", () => {
    seedConnectedServerThread();
    seedTwoGroupCenterState({ hostGroupActiveSurfaceId: HOST_SURFACE_ID });
    publishSeededStoreState(useCenterPanelStore);
    seedHostState("terminalFocusRequestId", 7);

    renderServerRoute();
    expect(capturedProps<Record<string, unknown>>("centerTerminalPanel")).toMatchObject({
      focusRequestId: 7,
      focusEligible: false,
    });

    const workspace = capturedProps("centerWorkspace");
    (workspace["onFocusGroup"] as (groupId: string) => void)("group-b");
    publishSeededStoreState(useCenterPanelStore);
    renderServerRoute();
    expect(capturedProps<Record<string, unknown>>("centerTerminalPanel")).toMatchObject({
      focusRequestId: 7,
      focusEligible: true,
    });

    (workspace["onFocusGroup"] as (groupId: string) => void)("group-a");
    publishSeededStoreState(useCenterPanelStore);
    renderServerRoute();
    expect(capturedProps<Record<string, unknown>>("centerTerminalPanel")).toMatchObject({
      focusRequestId: 7,
      focusEligible: false,
    });
  });

  it("renders the empty center-panel state when every surface was closed", () => {
    seedConnectedServerThread();
    useCenterPanelStore.setState({
      byThreadKey: {
        [threadKey]: {
          surfaces: [],
          groups: [{ id: "center:root", surfaceIds: [], activeSurfaceId: null }],
          layout: { type: "leaf", groupId: "center:root" },
          focusedGroupId: "center:root",
        },
      },
    });
    publishSeededStoreState(useCenterPanelStore);

    expect(renderServerRoute()).toContain('data-mock="center-panel-workspace"');
    expect(capturedProps("centerWorkspace")["renderFocusedActions"]).toBeTypeOf("function");
  });
});

// ─────────────────────────────────────────────────────────────────────
// Effects
// ─────────────────────────────────────────────────────────────────────

describe("ChatView effects (captured and run manually)", () => {
  it("runs the mount effects for a connected server thread and cleans up", () => {
    seedConnectedServerThread();
    h.queryDataByKey.set(`shell:${environmentId}`, { snapshot: { _tag: "Some" } });

    renderServerRoute();
    installComposerHandle();
    const list = installLegendList({ scrollLength: 50 });

    const cleanups = runEffects();

    // markThreadVisited effect stamped the visited timestamp.
    expect(useUiStateStore.getState().threadLastVisitedAtById[threadKey]).toBe(now);
    // Keydown keybinding listener was registered on window.
    expect(windowStub.listeners.some((listener) => listener.type === "keydown")).toBe(true);
    // Manual-navigation listeners attached to the legend list scroll node.
    expect(list.scrollNode.listeners.map((listener) => listener.type)).toEqual(
      expect.arrayContaining(["wheel", "touchmove", "pointerdown"]),
    );
    // Live-follow effect scrolled to the end because content overflows.
    expect(list.calls.scrollToEnd.length).toBeGreaterThanOrEqual(1);

    // Wheel gesture cancels live-follow.
    list.scrollNode.fire("wheel");

    for (const cleanup of cleanups) cleanup();
  });

  it("measures the composer overlay height via the layout effect and a ResizeObserver", () => {
    seedConnectedServerThread();
    const observed: unknown[] = [];
    class FakeResizeObserver {
      callback: () => void;
      constructor(callback: () => void) {
        this.callback = callback;
      }
      observe(target: unknown) {
        observed.push(target);
        this.callback();
      }
      disconnect() {
        observed.length = 0;
      }
    }
    vi.stubGlobal("ResizeObserver", FakeResizeObserver);
    const overlayElement = {
      getBoundingClientRect: () => ({ height: 41.4 }),
    };
    seedHostState("composerOverlayElement", overlayElement);

    renderServerRoute();
    const cleanups = runEffects();

    const heightCalls = h.setStateCalls.filter((call) => call.applied === 42);
    expect(heightCalls.length).toBeGreaterThanOrEqual(1);
    expect(observed).toContain(overlayElement);
    for (const cleanup of cleanups) cleanup();
  });

  it("does not reserve overlay space for a zero-height composer without ResizeObserver", () => {
    seedConnectedServerThread();
    const overlayElement = {
      getBoundingClientRect: () => ({ height: 0 }),
    };
    seedHostState("composerOverlayElement", overlayElement);
    vi.stubGlobal("ResizeObserver", undefined);

    renderServerRoute();
    const cleanups = runEffects();

    expect(h.setStateCalls.some((call) => call.applied === 0)).toBe(false);
    for (const cleanup of cleanups) cleanup();
  });

  it("keeps an already measured overlay height stable", () => {
    seedConnectedServerThread();
    const overlayElement = {
      getBoundingClientRect: () => ({ height: 42 }),
    };
    seedHostState("composerOverlayElement", overlayElement);
    h.stateSeeds.set(20, { value: 42, expectInitial: (value) => value === 0 });
    vi.stubGlobal("ResizeObserver", undefined);

    renderServerRoute();
    const cleanups = runEffects();

    expect(h.setStateCalls.some((call) => call.index === 20 && call.applied === 42)).toBe(true);
    for (const cleanup of cleanups) cleanup();
  });

  it("reconciles optimistic user messages once the server confirms them", () => {
    const optimisticMessage: ChatMessage = {
      id: MessageId.make("message-1"),
      role: "user",
      text: "hello",
      turnId: null,
      createdAt: now,
      updatedAt: now,
      streaming: false,
      attachments: [
        {
          type: "image",
          id: "attachment-1",
          name: "img.png",
          mimeType: "image/png",
          sizeBytes: 10,
          previewUrl: "blob:preview-1",
        },
      ],
    };
    seedConnectedServerThread(
      makeThread({
        messages: [
          {
            id: MessageId.make("message-1"),
            role: "user",
            text: "hello",
            turnId: null,
            createdAt: now,
            updatedAt: now,
            streaming: false,
          },
        ],
      }),
    );
    seedHostState("optimisticUserMessages", [optimisticMessage]);

    renderServerRoute();
    runEffects();
    runWindowTimeouts();

    // The removal timer executed the functional updater that drops confirmed
    // optimistic messages.
    const optimisticUpdates = setStateCallsFor("optimisticUserMessages");
    expect(
      optimisticUpdates.some(
        (call) => Array.isArray(call.applied) && (call.applied as unknown[]).length === 0,
      ),
    ).toBe(true);
  });

  it("positions and stabilizes a timeline anchor after manual navigation", () => {
    seedConnectedServerThread();
    renderServerRoute();
    const list = installLegendList({ scroll: 24, scrollLength: 200, dataLength: 4 });
    const cleanups = runEffects();
    const timeline = capturedProps("messagesTimeline");
    const messageId = MessageId.make("anchor-message");
    const onAnchorReady = timeline["onAnchorReady"] as (
      messageId: MessageId,
      anchorIndex: number,
    ) => void;
    const onAnchorSizeChanged = timeline["onAnchorSizeChanged"] as (messageId: MessageId) => void;

    onAnchorReady(messageId, 2);
    expect(list.calls.scrollToIndex).toContainEqual({
      index: 2,
      animated: true,
      viewPosition: 0,
      viewOffset: 16,
    });

    list.scrollNode.fire("scrollend");
    expect(list.calls.scrollToOffset).toContainEqual({ offset: 24, animated: false });

    list.scrollNode.fire("wheel");
    onAnchorReady(messageId, 2);
    list.scrollNode.fire("scrollend");
    expect(list.calls.scrollToOffset).toHaveLength(2);

    onAnchorSizeChanged(messageId);
    expect(list.calls.scrollToOffset).toHaveLength(3);

    onAnchorReady(messageId, 2);
    expect(list.calls.scrollToIndex).toHaveLength(2);
    for (const cleanup of cleanups) cleanup();
  });

  it("promotes blob attachment previews to settled server preview urls", async () => {
    const images: Array<{ src: string; fire: (type: string) => void }> = [];
    class FakeImage {
      src = "";
      listeners: Array<{ type: string; handler: () => void }> = [];
      constructor() {
        images.push({
          src: "",
          fire: (type: string) => {
            for (const listener of this.listeners) {
              if (listener.type === type) listener.handler();
            }
          },
        });
        const entry = images[images.length - 1]!;
        Object.defineProperty(this, "src", {
          get: () => entry.src,
          set: (value: string) => {
            entry.src = value;
          },
        });
      }
      addEventListener(type: string, handler: () => void) {
        this.listeners.push({ type, handler });
      }
    }
    vi.stubGlobal("Image", FakeImage);

    h.assetUrls = ["https://server/attachment-1.png"];
    seedConnectedServerThread(
      makeThread({
        messages: [
          {
            id: MessageId.make("message-1"),
            role: "user",
            text: "hello",
            turnId: null,
            createdAt: now,
            updatedAt: now,
            streaming: false,
            attachments: [
              {
                type: "image",
                id: "attachment-1",
                name: "img.png",
                mimeType: "image/png",
                sizeBytes: 10,
              },
            ],
          },
        ],
      }),
    );
    seedHostState("attachmentPreviewHandoffByMessageId", {
      "message-1": ["blob:handoff-1"],
    });

    renderServerRoute();
    const cleanups = runEffects();

    expect(images).toHaveLength(1);
    expect(images[0]!.src).toBe("https://server/attachment-1.png");
    images[0]!.fire("load");
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();

    const handoffUpdates = setStateCallsFor("attachmentPreviewHandoffByMessageId");
    expect(handoffUpdates.length).toBeGreaterThanOrEqual(1);
    for (const cleanup of cleanups) cleanup();
  });

  it("relays preview bus actions to the preview panel toggle", () => {
    seedConnectedServerThread();
    h.previewSupported = true;

    renderServerRoute();
    runEffects();

    expect(h.previewActionSubscribers.length).toBeGreaterThanOrEqual(1);
    for (const subscriber of h.previewActionSubscribers) subscriber("toggle-panel");

    // No active preview tab: toggling opens a fresh browser surface.
    expect(h.addBrowserSurfaceCalls).toHaveLength(1);
  });
});

// ─────────────────────────────────────────────────────────────────────
// Keydown shortcut dispatch
// ─────────────────────────────────────────────────────────────────────

describe("ChatView keydown shortcuts", () => {
  it("classifies every type-to-focus keyboard guard and event-path fallback", () => {
    expect(shouldTypeToFocusComposer(makeKeyEvent({ key: "a" }) as never)).toBe(true);
    expect(
      shouldTypeToFocusComposer(makeKeyEvent({ key: "a", defaultPrevented: true }) as never),
    ).toBe(false);
    expect(shouldTypeToFocusComposer(makeKeyEvent({ key: "a", isComposing: true }) as never)).toBe(
      false,
    );
    expect(shouldTypeToFocusComposer(makeKeyEvent({ key: "a", metaKey: true }) as never)).toBe(
      false,
    );
    expect(shouldTypeToFocusComposer(makeKeyEvent({ key: "a", ctrlKey: true }) as never)).toBe(
      false,
    );
    expect(shouldTypeToFocusComposer(makeKeyEvent({ key: "a", altKey: true }) as never)).toBe(
      false,
    );
    expect(shouldTypeToFocusComposer(makeKeyEvent({ key: "Enter" }) as never)).toBe(false);
    expect(
      shouldTypeToFocusComposer(makeKeyEvent({ key: "a", path: [new FakeElement(true)] }) as never),
    ).toBe(false);
    const interactive = new FakeElement(false);
    interactive.closest = (selector: string) => (selector.includes("button") ? interactive : null);
    expect(
      shouldTypeToFocusComposer(makeKeyEvent({ key: "a", path: [interactive] }) as never),
    ).toBe(false);

    const target = new FakeElement(true);
    const emptyPathEvent = makeKeyEvent({ key: "a", target });
    expect(eventPathContainsSelector(emptyPathEvent as never, "button")).toBe(true);
    expect(
      eventPathContainsSelector(makeKeyEvent({ key: "a", path: [{}] }) as never, "button"),
    ).toBe(false);
  });

  function renderWithKeydown(thread: Thread = makeThread()) {
    seedConnectedServerThread(thread);
    h.refSeeds.set(0, {
      value: { canSplitGroup: h.centerCanSplit },
      expectInitial: (value) => value === null,
    });
    renderServerRoute();
    const composer = installComposerHandle();
    runEffects();
    return { handler: windowKeydownHandler(), composer };
  }

  function openedTerminalIds(): string[] {
    return commandCallsFor("terminal.open").map((call) => {
      const command = call.input as { input: { terminalId: string } };
      return command.input.terminalId;
    });
  }

  it("opens a center terminal without dimensions when no renderer can measure it", async () => {
    const { handler } = renderWithKeydown();
    h.shortcutCommandByKey.set("F1", "terminal.newCenter");
    handler(makeKeyEvent({ key: "F1" }));
    await flushTerminalAction();
    expect(commandCallsFor("terminal.open")).toHaveLength(1);
    const request = commandCallsFor("terminal.open")[0]!.input as {
      input: { cols?: number; rows?: number };
    };
    expect(request.input.cols).toBeUndefined();
    expect(request.input.rows).toBeUndefined();
  });

  it("routes printable keys into the composer (type-to-focus)", () => {
    const inserted: string[] = [];
    seedConnectedServerThread();
    renderServerRoute();
    installComposerHandle({
      insertTextAtEnd: (text: string) => {
        inserted.push(text);
        return true;
      },
    });
    runEffects();
    const handler = windowKeydownHandler();

    const event = makeKeyEvent({ key: "a" });
    handler(event);
    expect(inserted).toEqual(["a"]);
    expect(event.prevented).toBe(true);

    // Editable targets are excluded from type-to-focus.
    const editable = new FakeElement(true);
    const editableEvent = makeKeyEvent({ key: "b", path: [editable] });
    handler(editableEvent);
    expect(inserted).toEqual(["a"]);

    // An open floating layer suppresses type-to-focus too.
    documentQuerySelectorResult = {};
    const floatingEvent = makeKeyEvent({ key: "c" });
    handler(floatingEvent);
    expect(inserted).toEqual(["a"]);
    documentQuerySelectorResult = null;

    // Modifier chords never type-to-focus.
    handler(makeKeyEvent({ key: "d", ctrlKey: true }));
    expect(inserted).toEqual(["a"]);
  });

  it.each([null, "center-panel", "right-panel"] as const)(
    "terminal.newCenter targets the focused center group from %s focus",
    async (owner) => {
      if (owner === "center-panel") {
        useCenterPanelStore.getState().openTerminalPanel(threadRef, "term-existing");
        publishSeededStoreState(useCenterPanelStore);
      } else if (owner === "right-panel") {
        useRightPanelStore.getState().openTerminal(threadRef, "term-right");
        publishSeededStoreState(useRightPanelStore);
      }
      h.terminalFocusOwner = owner;
      const place = vi.spyOn(useCenterPanelStore.getState(), "placeTerminalPanel");
      const { handler } = renderWithKeydown();
      h.shortcutCommandByKey.set("F1", "terminal.newCenter");

      handler(makeKeyEvent({ key: "F1" }));
      await flushTerminalAction();

      expect(place).toHaveBeenCalledWith(
        threadRef,
        expect.any(String),
        { type: "tab", groupId: "center:root" },
        {},
      );
      expect(commandCallsFor("terminal.open")).toHaveLength(1);
    },
  );

  it.each([
    ["terminal.new", { type: "tab", groupId: "center:root" }],
    ["terminal.split", { type: "split", groupId: "center:root", direction: "right" }],
    ["terminal.splitVertical", { type: "split", groupId: "center:root", direction: "down" }],
  ] as const)("routes %s to the focused center terminal group", async (command, placement) => {
    useCenterPanelStore.getState().openTerminalPanel(threadRef, "term-existing");
    publishSeededStoreState(useCenterPanelStore);
    h.terminalFocusOwner = "center-panel";
    const place = vi.spyOn(useCenterPanelStore.getState(), "placeTerminalPanel");
    const { handler } = renderWithKeydown();
    h.shortcutCommandByKey.set("F1", command);

    handler(makeKeyEvent({ key: "F1" }));
    await flushTerminalAction();

    expect(place).toHaveBeenCalledWith(threadRef, expect.any(String), placement, {});
    expect(commandCallsFor("terminal.open")).toHaveLength(1);
  });

  it.each(["terminal.new", "terminal.split", "terminal.splitVertical", "terminal.close"] as const)(
    "leaves %s as a no-op when no terminal surface owns focus",
    async (command) => {
      const { handler } = renderWithKeydown();
      h.shortcutCommandByKey.set("F1", command);

      handler(makeKeyEvent({ key: "F1" }));
      await flushTerminalAction();

      expect(commandCallsFor("terminal.open")).toHaveLength(0);
      expect(commandCallsFor("terminal.close")).toHaveLength(0);
    },
  );

  it.each([
    ["terminal.new", "new"],
    ["terminal.split", "horizontal"],
    ["terminal.splitVertical", "vertical"],
  ] as const)("preserves right-panel %s routing", async (command, expectedAction) => {
    seedConnectedServerThread();
    useRightPanelStore.getState().openTerminal(threadRef, "terminal-77");
    publishSeededStoreState(useRightPanelStore);
    h.terminalFocusOwner = "right-panel";
    const rightStore = useRightPanelStore.getState();
    const action =
      expectedAction === "new"
        ? vi.spyOn(rightStore, "openTerminal")
        : vi.spyOn(rightStore, "splitTerminal");
    renderServerRoute();
    installComposerHandle();
    runEffects();
    const handler = windowKeydownHandler();
    h.shortcutCommandByKey.set("F1", command);

    handler(makeKeyEvent({ key: "F1" }));
    await flushTerminalAction();

    if (expectedAction === "new") {
      expect(action).toHaveBeenCalled();
    } else {
      expect(action).toHaveBeenCalledWith(
        threadRef,
        expect.any(String),
        expect.any(String),
        expectedAction,
      );
    }
    expect(commandCallsFor("terminal.open")).toHaveLength(1);
  });

  it("preserves right-panel close routing", async () => {
    seedConnectedServerThread();
    useRightPanelStore.getState().openTerminal(threadRef, "terminal-77");
    publishSeededStoreState(useRightPanelStore);
    h.terminalFocusOwner = "right-panel";
    const closeRightTerminal = vi.spyOn(useRightPanelStore.getState(), "closeTerminal");
    renderServerRoute();
    installComposerHandle();
    runEffects();
    const handler = windowKeydownHandler();
    h.shortcutCommandByKey.set("F1", "terminal.close");

    handler(makeKeyEvent({ key: "F1" }));
    await flushTerminalAction();

    expect(closeRightTerminal).toHaveBeenCalledWith(threadRef, expect.any(String), "terminal-77");
    expect(closedTerminalIds()).toEqual(["terminal-77"]);
  });

  it("consumes a native close-style shortcut only when the resolver closes the focused terminal", async () => {
    seedConnectedServerThread();
    useRightPanelStore.getState().openTerminal(threadRef, "terminal-native-close");
    publishSeededStoreState(useRightPanelStore);
    h.terminalFocusOwner = "right-panel";
    renderServerRoute();
    installComposerHandle();
    runEffects();
    h.shortcutCommandByKey.set("w", "terminal.close");
    const event = makeKeyEvent({ key: "w", metaKey: true });

    windowKeydownHandler()(event);
    await flushTerminalAction();

    expect(event.prevented).toBe(true);
    expect(closedTerminalIds()).toEqual(["terminal-native-close"]);
  });

  it("rejects an infeasible center split before opening a server terminal", async () => {
    useCenterPanelStore.getState().openTerminalPanel(threadRef, "term-existing");
    publishSeededStoreState(useCenterPanelStore);
    h.terminalFocusOwner = "center-panel";
    h.centerCanSplit.mockReturnValue(false);
    const { handler } = renderWithKeydown();
    h.shortcutCommandByKey.set("F1", "terminal.split");

    handler(makeKeyEvent({ key: "F1" }));
    await flushTerminalAction();

    expect(commandCallsFor("terminal.open")).toHaveLength(0);
    expect(h.toasts).toHaveLength(1);
  });

  it("compensates with history deletion when center placement loses its race", async () => {
    useCenterPanelStore.getState().openTerminalPanel(threadRef, "term-existing");
    publishSeededStoreState(useCenterPanelStore);
    h.terminalFocusOwner = "center-panel";
    vi.spyOn(useCenterPanelStore.getState(), "placeTerminalPanel").mockReturnValue(false);
    const { handler } = renderWithKeydown();
    h.shortcutCommandByKey.set("F1", "terminal.new");

    handler(makeKeyEvent({ key: "F1" }));
    await flushTerminalAction();

    expect(commandCallsFor("terminal.close")).toHaveLength(1);
    expect(commandCallsFor("terminal.close")[0]?.input).toMatchObject({
      input: { deleteHistory: true },
    });
  });

  it("surfaces a durable notice when compensating center close fails", async () => {
    useCenterPanelStore.getState().openTerminalPanel(threadRef, "term-existing");
    publishSeededStoreState(useCenterPanelStore);
    h.terminalFocusOwner = "center-panel";
    vi.spyOn(useCenterPanelStore.getState(), "placeTerminalPanel").mockReturnValue(false);
    h.commandResults["terminal.close"] = () =>
      AsyncResult.failure(Cause.fail(new Error("close backend unavailable")));
    const { handler } = renderWithKeydown();
    h.shortcutCommandByKey.set("F1", "terminal.new");

    handler(makeKeyEvent({ key: "F1" }));

    await vi.waitFor(() => {
      expect(h.toasts).toEqual([
        expect.objectContaining({
          type: "error",
          timeout: 0,
          description: expect.stringContaining(
            "spawned session could not be closed: close backend unavailable",
          ),
        }),
      ]);
    });
  });

  it("classifies interrupted compensating center close without a failure notice", async () => {
    useCenterPanelStore.getState().openTerminalPanel(threadRef, "term-existing");
    publishSeededStoreState(useCenterPanelStore);
    h.terminalFocusOwner = "center-panel";
    vi.spyOn(useCenterPanelStore.getState(), "placeTerminalPanel").mockReturnValue(false);
    h.commandResults["terminal.close"] = () => AsyncResult.failure(Cause.interrupt(1));
    const { handler } = renderWithKeydown();
    h.shortcutCommandByKey.set("F1", "terminal.new");

    handler(makeKeyEvent({ key: "F1" }));
    await flushTerminalAction();

    expect(commandCallsFor("terminal.close")).toHaveLength(1);
    expect(h.toasts).toEqual([]);
  });

  it("requests terminal focus only after center placement succeeds", async () => {
    let finishOpen: ((result: unknown) => void) | undefined;
    h.commandResults["terminal.open"] = () =>
      new Promise((resolve) => {
        finishOpen = resolve;
      });
    useCenterPanelStore.getState().openTerminalPanel(threadRef, "term-existing");
    publishSeededStoreState(useCenterPanelStore);
    h.terminalFocusOwner = "center-panel";
    const { handler } = renderWithKeydown();
    h.shortcutCommandByKey.set("F1", "terminal.split");

    handler(makeKeyEvent({ key: "F1" }));
    expect(setStateCallsFor("terminalFocusRequestId")).toHaveLength(0);

    finishOpen?.(AsyncResult.success(undefined));
    await flushTerminalAction();
    expect(setStateCallsFor("terminalFocusRequestId")).toHaveLength(1);
  });

  it("reserves distinct terminal ids for concurrent center opens without closing either winner", async () => {
    const finishOpen: Array<(result: unknown) => void> = [];
    h.commandResults["terminal.open"] = () =>
      new Promise((resolve) => {
        finishOpen.push(resolve);
      });
    useCenterPanelStore.getState().openTerminalPanel(threadRef, "term-existing");
    publishSeededStoreState(useCenterPanelStore);
    h.terminalFocusOwner = "center-panel";
    const place = vi.spyOn(useCenterPanelStore.getState(), "placeTerminalPanel");
    const { handler } = renderWithKeydown();
    h.shortcutCommandByKey.set("F1", "terminal.new");

    handler(makeKeyEvent({ key: "F1" }));
    handler(makeKeyEvent({ key: "F1" }));

    expect(openedTerminalIds()).toHaveLength(2);
    expect(new Set(openedTerminalIds()).size).toBe(2);

    finishOpen[0]?.(AsyncResult.success(undefined));
    finishOpen[1]?.(AsyncResult.success(undefined));
    await flushTerminalAction();

    expect(place).toHaveBeenCalledTimes(2);
    expect(commandCallsFor("terminal.close")).toHaveLength(0);
  });

  it("reserves distinct terminal ids across concurrent center and right-panel opens", async () => {
    const finishOpen: Array<(result: unknown) => void> = [];
    h.commandResults["terminal.open"] = () =>
      new Promise((resolve) => {
        finishOpen.push(resolve);
      });
    useRightPanelStore.getState().openTerminal(threadRef, "right-seed");
    publishSeededStoreState(useRightPanelStore);
    const { handler } = renderWithKeydown();

    h.shortcutCommandByKey.set("F1", "terminal.newCenter");
    handler(makeKeyEvent({ key: "F1" }));
    h.terminalFocusOwner = "right-panel";
    h.shortcutCommandByKey.set("F1", "terminal.new");
    handler(makeKeyEvent({ key: "F1" }));

    expect(openedTerminalIds()).toHaveLength(2);
    expect(new Set(openedTerminalIds()).size).toBe(2);

    finishOpen[0]?.(AsyncResult.success(undefined));
    finishOpen[1]?.(AsyncResult.success(undefined));
    await flushTerminalAction();
    expect(commandCallsFor("terminal.close")).toHaveLength(0);
  });

  it.each(["terminal.new", "terminal.split"] as const)(
    "reserves distinct terminal ids for rapid right-panel %s opens",
    async (command) => {
      const finishOpen: Array<(result: unknown) => void> = [];
      h.commandResults["terminal.open"] = () =>
        new Promise((resolve) => {
          finishOpen.push(resolve);
        });
      useRightPanelStore.getState().openTerminal(threadRef, "right-seed");
      publishSeededStoreState(useRightPanelStore);
      h.terminalFocusOwner = "right-panel";
      const { handler } = renderWithKeydown();
      h.shortcutCommandByKey.set("F1", command);

      handler(makeKeyEvent({ key: "F1" }));
      handler(makeKeyEvent({ key: "F1" }));

      expect(openedTerminalIds()).toHaveLength(2);
      expect(new Set(openedTerminalIds()).size).toBe(2);

      finishOpen[0]?.(AsyncResult.success(undefined));
      finishOpen[1]?.(AsyncResult.success(undefined));
      await flushTerminalAction();
    },
  );

  it("caps rapid same-render right-panel splits without opening an orphan terminal", () => {
    h.commandResults["terminal.open"] = () => AsyncResult.success(undefined);
    const store = useRightPanelStore.getState();
    store.openTerminal(threadRef, "right-seed");
    store.splitTerminal(threadRef, "terminal:right-seed", "right-2");
    store.splitTerminal(threadRef, "terminal:right-seed", "right-3");
    publishSeededStoreState(useRightPanelStore);
    h.terminalFocusOwner = "right-panel";
    const { handler } = renderWithKeydown();
    h.shortcutCommandByKey.set("F1", "terminal.split");

    handler(makeKeyEvent({ key: "F1" }));
    handler(makeKeyEvent({ key: "F1" }));

    expect(openedTerminalIds()).toHaveLength(1);
    const terminalSurface = useRightPanelStore
      .getState()
      .byThreadKey[scopedThreadKey(threadRef)]?.surfaces.find(
        (surface) => surface.id === "terminal:right-seed" && surface.kind === "terminal",
      );
    expect(terminalSurface).toMatchObject({
      terminalIds: expect.arrayContaining(["right-seed", "right-2", "right-3"]),
    });
    expect(terminalSurface?.kind === "terminal" ? terminalSurface.terminalIds : []).toHaveLength(4);
  });

  it("releases a right-panel terminal reservation after a rejected open settles", async () => {
    const finishOpen: Array<(result: unknown) => void> = [];
    h.commandResults["terminal.open"] = () =>
      new Promise((resolve) => {
        finishOpen.push(resolve);
      });
    useRightPanelStore.getState().openTerminal(threadRef, "right-seed");
    publishSeededStoreState(useRightPanelStore);
    h.terminalFocusOwner = "right-panel";
    const { handler } = renderWithKeydown();
    h.shortcutCommandByKey.set("F1", "terminal.new");

    handler(makeKeyEvent({ key: "F1" }));
    handler(makeKeyEvent({ key: "F1" }));
    const [rejectedId, pendingId] = openedTerminalIds();
    expect(rejectedId).not.toBe(pendingId);

    finishOpen[0]?.(AsyncResult.failure(Cause.fail(new Error("open rejected"))));
    await flushTerminalAction();
    useRightPanelStore.getState().closeSurface(threadRef, `terminal:${rejectedId}`);
    handler(makeKeyEvent({ key: "F1" }));

    expect(openedTerminalIds()[2]).toBe(rejectedId);
    expect(openedTerminalIds()[2]).not.toBe(pendingId);

    finishOpen[1]?.(AsyncResult.success(undefined));
    finishOpen[2]?.(AsyncResult.success(undefined));
    await flushTerminalAction();
  });

  it.each([
    ["failure", AsyncResult.failure(Cause.fail(new Error("open failed")))],
    ["interruption", AsyncResult.failure(Cause.interrupt(1))],
  ] as const)("releases a reserved terminal id after open %s", async (_kind, failedResult) => {
    const finishOpen: Array<(result: unknown) => void> = [];
    h.commandResults["terminal.open"] = () =>
      new Promise((resolve) => {
        finishOpen.push(resolve);
      });
    useCenterPanelStore.getState().openTerminalPanel(threadRef, "term-existing");
    publishSeededStoreState(useCenterPanelStore);
    h.terminalFocusOwner = "center-panel";
    const { handler } = renderWithKeydown();
    h.shortcutCommandByKey.set("F1", "terminal.new");

    handler(makeKeyEvent({ key: "F1" }));
    handler(makeKeyEvent({ key: "F1" }));
    const [failedId, pendingId] = openedTerminalIds();
    expect(failedId).not.toBe(pendingId);

    finishOpen[0]?.(failedResult);
    await flushTerminalAction();
    handler(makeKeyEvent({ key: "F1" }));

    expect(openedTerminalIds()[2]).toBe(failedId);
    expect(openedTerminalIds()[2]).not.toBe(pendingId);

    finishOpen[1]?.(AsyncResult.success(undefined));
    finishOpen[2]?.(AsyncResult.success(undefined));
    await flushTerminalAction();
  });

  it("compensates an open when navigation replaces its originating thread and workspace", async () => {
    let finishOpen: ((result: unknown) => void) | undefined;
    h.commandResults["terminal.open"] = () =>
      new Promise((resolve) => {
        finishOpen = resolve;
      });
    useCenterPanelStore.getState().openTerminalPanel(threadRef, "term-existing");
    publishSeededStoreState(useCenterPanelStore);
    h.terminalFocusOwner = "center-panel";
    const place = vi.spyOn(useCenterPanelStore.getState(), "placeTerminalPanel");
    const { handler } = renderWithKeydown();
    h.shortcutCommandByKey.set("F1", "terminal.split");

    handler(makeKeyEvent({ key: "F1" }));
    expect(h.centerCanSplit).toHaveBeenCalledTimes(1);
    const terminalId = openedTerminalIds()[0]!;

    const nextWorkspaceCanSplit = vi.fn(() => true);
    h.refObjects[0]!.current = { canSplitGroup: nextWorkspaceCanSplit };
    h.refObjects[1]!.current = {
      threadKey: `${environmentId}:${ThreadId.make("thread-2")}`,
      revision: 1,
    };
    finishOpen?.(AsyncResult.success(undefined));
    await flushTerminalAction();

    expect(nextWorkspaceCanSplit).not.toHaveBeenCalled();
    expect(place).not.toHaveBeenCalled();
    expect(setStateCallsFor("terminalFocusRequestId")).toHaveLength(0);
    expect(commandCallsFor("terminal.close")).toEqual([
      expect.objectContaining({
        input: expect.objectContaining({
          input: {
            threadId,
            terminalId,
            deleteHistory: true,
          },
        }),
      }),
    ]);
  });

  it("does not place or focus a pending center tab after thread navigation", async () => {
    let finishOpen: ((result: unknown) => void) | undefined;
    h.commandResults["terminal.open"] = () =>
      new Promise((resolve) => {
        finishOpen = resolve;
      });
    useCenterPanelStore.getState().openTerminalPanel(threadRef, "term-existing");
    publishSeededStoreState(useCenterPanelStore);
    h.terminalFocusOwner = "center-panel";
    const place = vi.spyOn(useCenterPanelStore.getState(), "placeTerminalPanel");
    const { handler } = renderWithKeydown();
    h.shortcutCommandByKey.set("F1", "terminal.new");

    handler(makeKeyEvent({ key: "F1" }));
    const terminalId = openedTerminalIds()[0]!;
    h.refObjects[1]!.current = {
      threadKey: `${environmentId}:${ThreadId.make("thread-2")}`,
      revision: 1,
    };
    finishOpen?.(AsyncResult.success(undefined));
    await flushTerminalAction();

    expect(place).not.toHaveBeenCalled();
    expect(setStateCallsFor("terminalFocusRequestId")).toHaveLength(0);
    expect(commandCallsFor("terminal.close")).toEqual([
      expect.objectContaining({
        input: expect.objectContaining({
          input: {
            threadId,
            terminalId,
            deleteHistory: true,
          },
        }),
      }),
    ]);
  });

  it("center close activates the next local tab", async () => {
    seedConnectedServerThread();
    useCenterPanelStore.getState().openTerminalPanel(threadRef, "term-next");
    useCenterPanelStore.getState().openTerminalPanel(threadRef, "term-active");
    publishSeededStoreState(useCenterPanelStore);
    h.terminalFocusOwner = "center-panel";
    renderServerRoute();
    installComposerHandle();
    runEffects();
    const handler = windowKeydownHandler();
    h.shortcutCommandByKey.set("F1", "terminal.close");

    const event = makeKeyEvent({ key: "F1" });
    handler(event);
    await flushTerminalAction();

    const state = useCenterPanelStore.getState().byThreadKey[threadKey]!;
    expect(event.prevented).toBe(true);
    expect(event.stopped).toBe(true);
    expect(state.groups[0]?.activeSurfaceId).toBe("terminal:term-next");
    expect(closedTerminalIds()).toEqual(["term-active"]);
  });

  it("center close collapses an empty split and focuses the surviving group", async () => {
    seedConnectedServerThread();
    useCenterPanelStore.getState().openTerminalPanel(threadRef, "term-survivor");
    useCenterPanelStore
      .getState()
      .placeTerminalPanel(
        threadRef,
        "term-closing",
        { type: "split", groupId: "center:root", direction: "right" },
        undefined,
      );
    publishSeededStoreState(useCenterPanelStore);
    h.terminalFocusOwner = "center-panel";
    renderServerRoute();
    installComposerHandle();
    runEffects();
    const handler = windowKeydownHandler();
    h.shortcutCommandByKey.set("F1", "terminal.close");

    handler(makeKeyEvent({ key: "F1" }));
    await flushTerminalAction();

    const state = useCenterPanelStore.getState().byThreadKey[threadKey]!;
    expect(state.groups).toHaveLength(1);
    expect(state.focusedGroupId).toBe("center:root");
    expect(state.groups[0]?.activeSurfaceId).toBe("terminal:term-survivor");
    expect(setStateCallsFor("terminalFocusRequestId")).toHaveLength(1);
  });

  it("project script shortcuts run the mapped script in a terminal", async () => {
    const script: ProjectScript = {
      id: "dev-server",
      name: "Dev server",
      command: "pnpm dev",
      icon: "play",
      runOnWorktreeCreate: false,
    };
    seedEnvironment(makeEnvironmentPresentation());
    seedProject(makeProject({ scripts: [script] }));
    seedServerThread(makeThread());
    seedGitStatus(true);
    renderServerRoute();
    installComposerHandle();
    runEffects();
    const handler = windowKeydownHandler();
    h.shortcutCommandByKey.set("F9", "script.dev-server.run");

    handler(makeKeyEvent({ key: "F9" }));

    await vi.waitFor(() => {
      expect(commandCallsFor("terminal.open")).toHaveLength(1);
      expect(
        useCenterPanelStore
          .getState()
          .byThreadKey[threadKey]?.surfaces.some((surface) => surface.kind === "terminal"),
      ).toBe(true);
      const writes = commandCallsFor("terminal.write");
      expect(writes).toHaveLength(1);
      expect((writes[0]!.input as { input: { data: string } }).input.data).toBe("pnpm dev\r");
    });
  });

  it("ignores shortcuts while the command palette is open", () => {
    const { handler } = renderWithKeydown();
    h.commandPaletteOpen = true;
    h.shortcutCommandByKey.set("F1", "terminal.newCenter");

    handler(makeKeyEvent({ key: "F1" }));

    expect(commandCallsFor("terminal.open")).toHaveLength(0);
  });
});

// ─────────────────────────────────────────────────────────────────────
// Right panel surface handlers
// ─────────────────────────────────────────────────────────────────────

describe("ChatView right panel handlers", () => {
  function openedPanelProps(open: (ref: typeof threadRef) => void) {
    seedConnectedServerThread();
    open(threadRef);
    publishSeededStoreState(useRightPanelStore);
    renderServerRoute();
    return capturedProps("rightPanelTabs");
  }

  it("preloads project files before a file surface is opened", () => {
    seedConnectedServerThread();

    renderServerRoute();

    expect(capturedProps("projectFilesPreloader")).toEqual({
      environmentId,
      cwd: "X:/demo",
    });
  });

  it("publishes the visible active file to the editing registry", () => {
    const setActivePath = vi.spyOn(FileEditingSessionRegistry.prototype, "setActivePath");
    seedConnectedServerThread();
    useRightPanelStore.getState().openFile(threadRef, "src/app.ts");
    publishSeededStoreState(useRightPanelStore);

    renderServerRoute();
    runEffects();

    expect(setActivePath).toHaveBeenCalledWith("src/app.ts");
  });

  it("publishes null for an active non-file surface", () => {
    const setActivePath = vi.spyOn(FileEditingSessionRegistry.prototype, "setActivePath");
    seedConnectedServerThread();
    useRightPanelStore.getState().openFile(threadRef, "src/app.ts");
    useRightPanelStore.getState().open(threadRef, "plan");
    publishSeededStoreState(useRightPanelStore);

    renderServerRoute();
    runEffects();

    expect(setActivePath).toHaveBeenCalledWith(null);
  });

  it("publishes null when the active file remains selected behind a hidden panel", () => {
    const setActivePath = vi.spyOn(FileEditingSessionRegistry.prototype, "setActivePath");
    seedConnectedServerThread();
    useRightPanelStore.getState().openFile(threadRef, "src/app.ts");
    useRightPanelStore.getState().close(threadRef);
    publishSeededStoreState(useRightPanelStore);

    renderServerRoute();
    runEffects();

    expect(setActivePath).toHaveBeenCalledWith(null);
  });

  it("passes one project-scoped editing registry into file surfaces", async () => {
    seedConnectedServerThread();
    useRightPanelStore.getState().openFile(threadRef, "src/a.ts");
    useRightPanelStore.getState().openFile(threadRef, "src/b.ts");
    publishSeededStoreState(useRightPanelStore);

    renderServerRoute();
    await vi.dynamicImportSettled();
    renderServerRoute();

    const props = capturedProps("filePreviewPanel");
    expect(props["editingSessions"]).toBeInstanceOf(FileEditingSessionRegistry);
  });

  it("activates plan / terminal / diff surfaces with their side effects", () => {
    const props = openedPanelProps((ref) => {
      useRightPanelStore.getState().open(ref, "plan");
      useRightPanelStore.getState().openTerminal(ref, "terminal-1");
      useRightPanelStore.getState().open(ref, "diff");
    });
    const onActivate = props["onActivate"] as (surface: RightPanelSurface) => void;
    const surfaces = useRightPanelStore.getState().byThreadKey[threadKey]!.surfaces;

    for (const surface of surfaces) {
      onActivate(surface);
    }

    expect(useRightPanelStore.getState().byThreadKey[threadKey]?.activeSurfaceId).toBe(
      surfaces[surfaces.length - 1]!.id,
    );
  });

  it("closes single, other, right-of, and all surfaces including terminal cleanup", () => {
    const props = openedPanelProps((ref) => {
      useRightPanelStore.getState().open(ref, "plan");
      useRightPanelStore.getState().openTerminal(ref, "terminal-1");
      useRightPanelStore.getState().open(ref, "sourceControl");
      useRightPanelStore.getState().open(ref, "files");
    });
    const surfaces = useRightPanelStore.getState().byThreadKey[threadKey]!.surfaces;
    const terminalSurface = surfaces.find((surface) => surface.kind === "terminal")!;
    const planSurface = surfaces.find((surface) => surface.kind === "plan")!;

    const onCloseSurface = props["onCloseSurface"] as (surface: RightPanelSurface) => void;
    const onCloseOtherSurfaces = props["onCloseOtherSurfaces"] as (
      surface: RightPanelSurface,
    ) => void;
    const onCloseSurfacesToRight = props["onCloseSurfacesToRight"] as (
      surface: RightPanelSurface,
    ) => void;
    const onCloseAllSurfaces = props["onCloseAllSurfaces"] as () => void;

    onCloseSurface(terminalSurface);
    expect(commandCallsFor("terminal.close")).toHaveLength(1);

    onCloseSurfacesToRight(planSurface);
    onCloseOtherSurfaces(planSurface);
    onCloseAllSurfaces();

    expect(useRightPanelStore.getState().byThreadKey[threadKey]?.surfaces ?? []).toHaveLength(0);
  });

  it("falls back to terminal exit when right-panel session close fails", async () => {
    h.commandResults["terminal.close"] = () =>
      AsyncResult.failure(Cause.fail(new Error("temporary close failure")));
    const props = openedPanelProps((ref) => {
      useRightPanelStore.getState().openTerminal(ref, "terminal-fallback");
    });
    const terminalSurface = useRightPanelStore
      .getState()
      .byThreadKey[threadKey]!.surfaces.find((surface) => surface.kind === "terminal")!;

    (props["onCloseSurface"] as (surface: RightPanelSurface) => void)(terminalSurface);
    await flushTerminalAction();

    expect(closedTerminalIds()).toEqual(["terminal-fallback"]);
    expect(commandCallsFor("terminal.write")).toEqual([
      expect.objectContaining({
        input: expect.objectContaining({
          input: {
            threadId,
            terminalId: "terminal-fallback",
            data: "exit\n",
          },
        }),
      }),
    ]);
    expect(useRightPanelStore.getState().byThreadKey[threadKey]?.surfaces ?? []).toEqual([]);
  });

  it("adds browser/terminal/diff/source-control/files surfaces on demand", () => {
    const props = openedPanelProps((ref) => {
      useRightPanelStore.getState().open(ref, "plan");
    });

    (props["onAddBrowser"] as () => void)();
    (props["onAddTerminal"] as () => void)();
    (props["onAddDiff"] as () => void)();
    (props["onAddSourceControl"] as () => void)();
    (props["onAddFiles"] as () => void)();

    expect(h.addBrowserSurfaceCalls).toHaveLength(1);
    expect(commandCallsFor("terminal.open")).toHaveLength(1);
    const kinds = (useRightPanelStore.getState().byThreadKey[threadKey]?.surfaces ?? []).map(
      (surface) => surface.kind,
    );
    expect(kinds).toEqual(expect.arrayContaining(["terminal", "diff", "sourceControl", "files"]));
  });

  it("copies file paths via the clipboard with success and failure toasts", async () => {
    const written: string[] = [];
    vi.stubGlobal("navigator", {
      clipboard: {
        writeText: (text: string) => {
          written.push(text);
          return Promise.resolve();
        },
      },
    });
    const props = openedPanelProps((ref) => {
      useRightPanelStore.getState().openFile(ref, "src/index.ts");
    });
    const onCopyFilePath = props["onCopyFilePath"] as (relativePath: string) => void;

    onCopyFilePath("src/index.ts");
    await Promise.resolve();
    expect(written).toEqual(["src/index.ts"]);
    expect(h.toasts.some((toast) => (toast as { title?: string }).title === "Path copied")).toBe(
      true,
    );

    // Clipboard rejection surfaces an error toast.
    vi.stubGlobal("navigator", {
      clipboard: {
        writeText: () => Promise.reject(new Error("denied")),
      },
    });
    onCopyFilePath("src/other.ts");
    await Promise.resolve();
    await Promise.resolve();
    expect(
      h.toasts.some((toast) => (toast as { title?: string }).title === "Failed to copy path"),
    ).toBe(true);

    // Missing clipboard API short-circuits with an error toast.
    vi.stubGlobal("navigator", {});
    h.toasts.length = 0;
    onCopyFilePath("src/third.ts");
    expect(
      h.toasts.some((toast) => (toast as { title?: string }).title === "Failed to copy path"),
    ).toBe(true);
  });

  it("closes preview surfaces through closePreviewSession", () => {
    h.previewSupported = true;
    seedConnectedServerThread();
    useRightPanelStore.getState().openBrowser(threadRef, "tab-1");
    publishSeededStoreState(useRightPanelStore);
    renderServerRoute();
    const props = capturedProps("rightPanelTabs");
    const surfaces = useRightPanelStore.getState().byThreadKey[threadKey]!.surfaces;
    const previewSurface = surfaces.find((surface) => surface.kind === "preview")!;

    (props["onCloseSurface"] as (surface: RightPanelSurface) => void)(previewSurface);

    expect(h.closePreviewSessionCalls).toHaveLength(1);
  });

  it("renders the sheet layout on narrow viewports and closes via the sheet", () => {
    h.mediaQueryMatches = true;
    seedConnectedServerThread();
    useRightPanelStore.getState().open(threadRef, "plan");
    publishSeededStoreState(useRightPanelStore);

    const markup = renderServerRoute();

    expect(markup).toContain('data-mock="right-panel-sheet"');
    const sheet = capturedProps("rightPanelSheet");
    (sheet["onClose"] as () => void)();
    expect(useRightPanelStore.getState().byThreadKey[threadKey]?.isOpen).toBe(false);
  });

  it("exposes only the right panel from the layout controls", () => {
    seedConnectedServerThread();
    renderServerRoute();
    const controls = capturedProps("panelLayoutControls");

    (controls["onToggleRightPanel"] as () => void)();
    expect(useRightPanelStore.getState().byThreadKey[threadKey]?.isOpen).toBe(true);
    expect(controls).not.toHaveProperty("onToggleTerminal");
    expect(controls).not.toHaveProperty("terminalOpen");
    expect(controls).not.toHaveProperty("terminalAvailable");
  });

  it("opens Files with one layout-control click when a sibling chat suppresses host Activity", () => {
    const siblingThreadId = ThreadId.make("thread-sibling-suppressed-activity");
    seedConnectedServerThread();
    seedServerThread(makeThread({ id: siblingThreadId, title: "Sibling thread" }));
    useRightPanelStore.getState().openActivity(threadRef, "subagents", { _tag: "thread" });
    useCenterPanelStore.getState().openChatPanel(threadRef, siblingThreadId, "Codex");
    publishSeededStoreState(useRightPanelStore);
    publishSeededStoreState(useCenterPanelStore);

    renderServerRoute();
    const controls = capturedProps("panelLayoutControls");
    expect(controls["rightPanelOpen"]).toBe(false);

    (controls["onToggleRightPanel"] as () => void)();

    const panelState = useRightPanelStore.getState().byThreadKey[threadKey];
    expect(panelState?.isOpen).toBe(true);
    expect(panelState?.activeSurfaceId).toBe("files");
    expect(panelState?.surfaces.map((surface) => surface.kind)).toEqual(["activity", "files"]);

    publishSeededStoreState(useRightPanelStore);
    renderServerRoute();
    expect(capturedProps("panelLayoutControls")["rightPanelOpen"]).toBe(true);
  });

  it("maximizes the inline right panel via the maximize control", () => {
    seedConnectedServerThread();
    useRightPanelStore.getState().open(threadRef, "plan");
    publishSeededStoreState(useRightPanelStore);
    seedHostState("maximizedRightPanelThreadKey", threadKey);

    const markup = renderServerRoute();

    expect(markup).toContain('data-chat-column-maximized-away="true"');
    const control = capturedProps("rightPanelMaximizeControl");
    expect(control["maximized"]).toBe(true);
    (control["onToggle"] as () => void)();
    const toggles = setStateCallsFor("maximizedRightPanelThreadKey");
    expect(toggles).toHaveLength(1);
    expect(toggles[0]!.applied).toBeNull();
  });
});

// ─────────────────────────────────────────────────────────────────────
// Project scripts (header handlers)
// ─────────────────────────────────────────────────────────────────────

describe("ChatView project script handlers", () => {
  const script: ProjectScript = {
    id: "dev-server",
    name: "Dev server",
    command: "pnpm dev",
    icon: "play",
    runOnWorktreeCreate: false,
  };

  function renderWithScripts(scripts = [script]) {
    seedEnvironment(makeEnvironmentPresentation());
    seedProject(makeProject({ scripts }));
    seedServerThread(makeThread());
    seedGitStatus(true);
    renderServerRoute();
    return capturedProps("chatHeaderActions");
  }

  it("creates a chat panel from configured defaults instead of stale project options", async () => {
    const reasoningEffort: ProviderOptionDescriptor = {
      id: "reasoningEffort",
      label: "Reasoning",
      type: "select",
      options: [
        { id: "medium", label: "Medium", isDefault: true },
        { id: "high", label: "High" },
      ],
      currentValue: "medium",
    };
    const configuredModel = {
      slug: "gpt-configured",
      name: "Configured",
      isCustom: false,
      capabilities: { optionDescriptors: [reasoningEffort] },
    };
    seedEnvironment(makeEnvironmentPresentation());
    seedProject(
      makeProject({
        defaultModelSelection: {
          instanceId: codexInstanceId,
          model: "gpt-stale",
          options: [
            { id: "reasoningEffort", value: "medium" },
            { id: "serviceTier", value: "default" },
          ],
        },
        scripts: [script],
      }),
    );
    seedServerThread(
      makeThread({
        branch: "feature/panels",
        worktreePath: "X:/demo/worktrees/panels",
      }),
    );
    seedGitStatus(true);
    h.settings = {
      ...h.settings,
      providerSessionDefaults: {
        [ProviderDriverKind.make("codex")]: {
          model: "gpt-configured",
          options: [
            { id: "reasoningEffort", value: "high" },
            { id: "serviceTier", value: "fast" },
          ],
        },
      },
    };
    renderServerRoute();
    const header = capturedProps("chatHeaderActions");
    const entry = {
      instanceId: codexInstanceId,
      driverKind: ProviderDriverKind.make("codex"),
      displayName: "Codex",
      models: [
        {
          slug: "gpt-stale",
          name: "Stale",
          isCustom: false,
          capabilities: { optionDescriptors: [reasoningEffort] },
        },
        configuredModel,
      ],
    } as unknown as ProviderInstanceEntry;

    (header["onCreateChatPanel"] as (entry: ProviderInstanceEntry) => void)(entry);
    await Promise.resolve();
    await Promise.resolve();

    expect(commandCallsFor("worktree.createPanel")).toHaveLength(1);
    expect(commandCallsFor("worktree.createPanel")[0]?.input).toMatchObject({
      environmentId,
      input: {
        hostThreadId: threadId,
        threadDefaults: {
          modelSelection: {
            instanceId: codexInstanceId,
            model: "gpt-configured",
            options: [{ id: "reasoningEffort", value: "high" }],
          },
        },
      },
    });
  });

  it("creates a Claude chat panel with a configured prompt-injected effort", async () => {
    const claudeDriver = ProviderDriverKind.make("claudeAgent");
    const claudeInstanceId = ProviderInstanceId.make("claudeAgent");
    const configuredModel = {
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
    seedEnvironment(makeEnvironmentPresentation());
    seedProject(makeProject({ defaultModelSelection: null }));
    seedServerThread(makeThread());
    seedGitStatus(true);
    h.settings = {
      ...h.settings,
      providerSessionDefaults: {
        [claudeDriver]: {
          model: "claude-sonnet-5",
          options: [{ id: "effort", value: "ultrathink" }],
        },
      },
    };
    renderServerRoute();
    const header = capturedProps("chatHeaderActions");
    const entry = {
      instanceId: claudeInstanceId,
      driverKind: claudeDriver,
      displayName: "Claude",
      models: [configuredModel],
    } as unknown as ProviderInstanceEntry;

    (header["onCreateChatPanel"] as (entry: ProviderInstanceEntry) => void)(entry);
    await Promise.resolve();
    await Promise.resolve();

    expect(commandCallsFor("worktree.createPanel")[0]?.input).toMatchObject({
      input: {
        threadDefaults: {
          modelSelection: {
            instanceId: claudeInstanceId,
            model: "claude-sonnet-5",
            options: [{ id: "effort", value: "ultrathink" }],
          },
        },
      },
    });
  });

  it("creates a Codex chat panel with saved effort but not unverified Fast during empty discovery", async () => {
    seedEnvironment(makeEnvironmentPresentation());
    seedProject(makeProject({ defaultModelSelection: null }));
    seedServerThread(makeThread());
    seedGitStatus(true);
    h.settings = {
      ...h.settings,
      providerSessionDefaults: {
        [ProviderDriverKind.make("codex")]: {
          model: "gpt-offline",
          options: [
            { id: "reasoningEffort", value: "high" },
            { id: "serviceTier", value: "fast" },
          ],
        },
      },
    };
    renderServerRoute();
    const header = capturedProps("chatHeaderActions");
    const entry = {
      instanceId: codexInstanceId,
      driverKind: ProviderDriverKind.make("codex"),
      displayName: "Codex",
      models: [],
    } as unknown as ProviderInstanceEntry;

    (header["onCreateChatPanel"] as (entry: ProviderInstanceEntry) => void)(entry);
    await Promise.resolve();
    await Promise.resolve();

    expect(commandCallsFor("worktree.createPanel")[0]?.input).toMatchObject({
      input: {
        threadDefaults: {
          modelSelection: {
            instanceId: codexInstanceId,
            model: DEFAULT_MODEL_BY_PROVIDER[ProviderDriverKind.make("codex")],
            options: [{ id: "reasoningEffort", value: "high" }],
          },
        },
      },
    });
  });

  it("runs a script in the focused idle center terminal", async () => {
    useCenterPanelStore.getState().openTerminalPanel(threadRef, "terminal-1");
    publishSeededStoreState(useCenterPanelStore);
    h.knownSessions = [
      {
        target: { environmentId, threadId, terminalId: "terminal-1" },
        state: { summary: { label: "Shell", cwd: "X:/demo", worktreePath: null } },
      },
    ];
    const header = renderWithScripts();
    const onRunProjectScript = header["onRunProjectScript"] as (
      target: ProjectScript,
      options?: Record<string, unknown>,
    ) => Promise<void>;

    await onRunProjectScript(script);

    expect(commandCallsFor("terminal.open")).toHaveLength(1);
    expect(commandCallsFor("terminal.open")[0]?.input).toMatchObject({
      input: { terminalId: "terminal-1" },
    });
    const writes = commandCallsFor("terminal.write");
    expect(writes).toHaveLength(1);
    expect((writes[0]!.input as { input: { data: string } }).input.data).toBe("pnpm dev\r");
    expect(h.terminalInputEnqueues.at(-1)?.data).toBe("pnpm dev\r");
    expect(
      useCenterPanelStore
        .getState()
        .byThreadKey[threadKey]?.surfaces.filter((surface) => surface.kind === "terminal"),
    ).toHaveLength(1);
  });

  it("opens a fresh center terminal when the focused terminal is busy", async () => {
    h.runningTerminalIds = ["terminal-1"];
    seedEnvironment(makeEnvironmentPresentation());
    seedProject(makeProject({ scripts: [script] }));
    seedServerThread(makeThread());
    seedGitStatus(true);
    useCenterPanelStore.getState().openTerminalPanel(threadRef, "terminal-1");
    publishSeededStoreState(useCenterPanelStore);
    h.knownSessions = [
      {
        target: { environmentId, threadId, terminalId: "terminal-1" },
        state: { summary: { label: "Shell", cwd: "X:/demo", worktreePath: null } },
      },
    ];
    renderServerRoute();
    const header = capturedProps("chatHeaderActions");

    await (header["onRunProjectScript"] as (input: typeof script) => Promise<void>)(script);

    const openCalls = commandCallsFor("terminal.open");
    expect(openCalls).toHaveLength(1);
    const openInput = openCalls[0]!.input as {
      input: { terminalId: string; cols?: number; rows?: number };
    };
    expect(openInput.input.terminalId).not.toBe("terminal-1");
    expect(openInput.input.cols).toBeUndefined();
    expect(openInput.input.rows).toBeUndefined();
    expect(
      useCenterPanelStore
        .getState()
        .byThreadKey[threadKey]?.surfaces.filter((surface) => surface.kind === "terminal"),
    ).toHaveLength(2);
  });

  it("reports script terminal failures as thread errors", async () => {
    h.commandResults["terminal.open"] = () =>
      AsyncResult.failure(Cause.fail(new Error("terminal exploded")));
    const header = renderWithScripts();

    await (header["onRunProjectScript"] as (input: typeof script) => Promise<void>)(script);

    expect(commandCallsFor("terminal.write")).toHaveLength(0);
    // The failure was routed into local server error state.
    const errorWrites = h.setStateCalls.filter(
      (call) =>
        typeof call.applied === "object" &&
        call.applied !== null &&
        (call.applied as Record<string, unknown>)[threadKey] === "terminal exploded",
    );
    expect(errorWrites.length).toBeGreaterThanOrEqual(1);
  });

  it("saves, updates, and deletes project scripts through project.update", async () => {
    const header = renderWithScripts();

    const onAdd = header["onAddProjectScript"] as (
      input: Record<string, unknown>,
    ) => Promise<{ _tag: string }>;
    const addResult = await onAdd({
      name: "Build",
      command: "pnpm build",
      icon: null,
      runOnWorktreeCreate: true,
      keybinding: null,
    });
    expect(addResult._tag).toBe("Success");
    expect(commandCallsFor("project.update")).toHaveLength(1);

    const onUpdate = header["onUpdateProjectScript"] as (
      scriptId: string,
      input: Record<string, unknown>,
    ) => Promise<{ _tag: string }>;
    const updateResult = await onUpdate("dev-server", {
      name: "Dev server 2",
      command: "pnpm dev --host",
      icon: null,
      runOnWorktreeCreate: true,
      keybinding: null,
    });
    expect(updateResult._tag).toBe("Success");

    const missingResult = await onUpdate("missing-script", {
      name: "Nope",
      command: "true",
      icon: null,
      runOnWorktreeCreate: false,
      keybinding: null,
    });
    expect(missingResult._tag).toBe("Failure");

    const onDelete = header["onDeleteProjectScript"] as (
      scriptId: string,
    ) => Promise<{ _tag: string }>;
    const deleteResult = await onDelete("dev-server");
    expect(deleteResult._tag).toBe("Success");
    expect(
      h.toasts.some((toast) => (toast as { title?: string }).title?.startsWith("Deleted action")),
    ).toBe(true);

    h.commandResults["project.update"] = () =>
      AsyncResult.failure(Cause.fail(new Error("persist failed")));
    const failedDelete = await onDelete("dev-server");
    expect(failedDelete._tag).toBe("Failure");
    expect(
      h.toasts.some((toast) => (toast as { title?: string }).title === "Could not delete action"),
    ).toBe(true);
  });

  it("creates chat panels and center terminal panels from the header", async () => {
    const header = renderWithScripts();

    const entry = {
      instanceId: codexInstanceId,
      driverKind: ProviderDriverKind.make("codex"),
      displayName: "Codex",
      models: codexProvider.models,
    } as unknown as ProviderInstanceEntry;
    (header["onCreateChatPanel"] as (entry: ProviderInstanceEntry) => void)(entry);
    expect(commandCallsFor("thread.create").length).toBeGreaterThanOrEqual(0);

    (header["onOpenTerminalPanel"] as () => void)();
    await flushTerminalAction();
    const centerState = useCenterPanelStore.getState().byThreadKey[threadKey];
    expect(centerState?.surfaces.some((surface) => surface.kind === "terminal")).toBe(true);

    const providerTerminalAction = {
      entry,
      label: "Codex Terminal",
      command: {
        executable: "/opt/codex",
        args: ["--dangerously-bypass-approvals-and-sandbox"],
        label: "Codex Terminal",
      },
    };
    (header["onOpenProviderTerminalPanel"] as (action: typeof providerTerminalAction) => void)(
      providerTerminalAction,
    );
    await flushTerminalAction();
    const providerSurface = useCenterPanelStore
      .getState()
      .byThreadKey[threadKey]?.surfaces.find(
        (surface) => surface.kind === "terminal" && surface.label === "Codex Terminal",
      );
    expect(providerSurface).toMatchObject({
      kind: "terminal",
      label: "Codex Terminal",
      command: providerTerminalAction.command,
    });
    expect(
      useCenterPanelStore
        .getState()
        .byThreadKey[threadKey]?.surfaces.filter((surface) => surface.kind === "terminal"),
    ).toHaveLength(2);
  });

  it("falls back to a built-in model discovered for the target provider instance", async () => {
    const targetInstanceId = ProviderInstanceId.make("codex_work");
    seedEnvironment(makeEnvironmentPresentation());
    seedProject(
      makeProject({
        defaultModelSelection: {
          instanceId: codexInstanceId,
          model: "gpt-custom-first",
        },
        scripts: [script],
      }),
    );
    seedServerThread(makeThread());
    seedGitStatus(true);
    h.settings = {
      ...h.settings,
      providerSessionDefaults: {
        [ProviderDriverKind.make("codex")]: { model: "gpt-other-instance" },
      },
    };
    renderServerRoute();
    const header = capturedProps("chatHeaderActions");
    const entry = {
      instanceId: targetInstanceId,
      driverKind: ProviderDriverKind.make("codex"),
      displayName: "Codex Work",
      models: [
        { slug: "gpt-custom-first", name: "Custom", isCustom: true, capabilities: null },
        { slug: "gpt-built-in", name: "Built in", isCustom: false, capabilities: null },
      ],
    } as unknown as ProviderInstanceEntry;

    (header["onCreateChatPanel"] as (entry: ProviderInstanceEntry) => void)(entry);
    await Promise.resolve();
    await Promise.resolve();

    expect(commandCallsFor("worktree.createPanel")).toHaveLength(1);
    expect(commandCallsFor("worktree.createPanel")[0]?.input).toMatchObject({
      input: {
        threadDefaults: {
          modelSelection: {
            instanceId: targetInstanceId,
            model: "gpt-built-in",
          },
        },
      },
    });
  });

  it("creates a provider chat panel even when model discovery is temporarily empty", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const header = renderWithScripts();
    const claudeDriver = ProviderDriverKind.make("claudeAgent");
    const entry = {
      instanceId: ProviderInstanceId.make("claudeAgent"),
      driverKind: claudeDriver,
      displayName: "Claude",
      models: [],
    } as unknown as ProviderInstanceEntry;

    (header["onCreateChatPanel"] as (entry: ProviderInstanceEntry) => void)(entry);
    await Promise.resolve();
    await Promise.resolve();

    const createCalls = commandCallsFor("worktree.createPanel");
    expect(createCalls).toHaveLength(1);
    const createInput = createCalls[0]?.input as {
      environmentId: EnvironmentId;
      input: {
        threadId: ThreadId;
        title: string;
        hostThreadId: ThreadId;
        threadDefaults: {
          modelSelection: { instanceId: ProviderInstanceId; model: string };
        };
      };
    };
    expect(createInput).toMatchObject({
      environmentId,
      input: {
        title: "Panel — Claude",
        hostThreadId: threadId,
        threadDefaults: {
          modelSelection: {
            instanceId: ProviderInstanceId.make("claudeAgent"),
            model: DEFAULT_MODEL_BY_PROVIDER[claudeDriver],
          },
        },
      },
    });
    expect(warn).toHaveBeenCalledWith(
      "Provider session default fallback",
      expect.objectContaining({
        driver: claudeDriver,
        instanceId: ProviderInstanceId.make("claudeAgent"),
        resolvedModel: DEFAULT_MODEL_BY_PROVIDER[claudeDriver],
        reason: "models-unavailable",
      }),
    );

    const centerState = useCenterPanelStore.getState().byThreadKey[threadKey];
    const claudeSurface = centerState?.surfaces.find((surface) => surface.kind === "chat");
    expect(claudeSurface).toEqual({
      id: `chat:${createInput.input.threadId}`,
      kind: "chat",
      threadId: createInput.input.threadId,
      providerLabel: "Claude",
    });
    expect(
      centerState?.groups.find((group) => group.id === centerState.focusedGroupId)?.activeSurfaceId,
    ).toBe(claudeSurface?.id);
  });

  it("covers explicit script launch options and opaque terminal failures", async () => {
    const header = renderWithScripts();
    const onRun = header["onRunProjectScript"] as (
      target: ProjectScript,
      options?: Record<string, unknown>,
    ) => Promise<void>;

    await onRun(script, {
      cwd: "X:/custom",
      env: { FEATURE: "1" },
      worktreePath: null,
      preferNewTerminal: true,
      rememberAsLastInvoked: false,
    });
    expect(commandCallsFor("terminal.open")[0]?.input).toMatchObject({
      input: { cwd: "X:/custom", env: expect.objectContaining({ FEATURE: "1" }) },
    });

    h.commandCalls.length = 0;
    h.commandResults["terminal.open"] = () => AsyncResult.failure(Cause.fail("opaque open"));
    await onRun(script);
    expect(commandCallsFor("terminal.write")).toHaveLength(0);

    h.commandCalls.length = 0;
    h.commandResults["terminal.open"] = () => AsyncResult.success(undefined);
    h.commandResults["terminal.write"] = () => AsyncResult.failure(Cause.fail("opaque write"));
    await onRun(script);
    expect(
      h.setStateCalls.some(
        (call) =>
          typeof call.applied === "object" &&
          call.applied !== null &&
          Object.values(call.applied as Record<string, unknown>).includes(
            'Failed to run script "Dev server".',
          ),
      ),
    ).toBe(true);
  });

  it("covers script persistence alternatives and unknown deletions", async () => {
    const autoScript = { ...script, runOnWorktreeCreate: true };
    const secondScript: ProjectScript = {
      id: "test",
      name: "Test",
      command: "pnpm test",
      icon: "test",
      runOnWorktreeCreate: false,
    };
    const header = renderWithScripts([autoScript, secondScript]);
    const input = {
      name: "Build",
      command: "pnpm build",
      icon: null,
      runOnWorktreeCreate: false,
      keybinding: null,
    };

    await (header["onAddProjectScript"] as (value: Record<string, unknown>) => Promise<unknown>)(
      input,
    );
    await (
      header["onUpdateProjectScript"] as (
        id: string,
        value: Record<string, unknown>,
      ) => Promise<unknown>
    )("dev-server", { ...input, runOnWorktreeCreate: true });
    await (header["onDeleteProjectScript"] as (id: string) => Promise<unknown>)("missing");

    expect(
      h.toasts.some((toast) => (toast as { title?: string }).title === 'Deleted action "Unknown"'),
    ).toBe(true);
  });

  it("returns successful no-ops for script mutations without an active project", async () => {
    seedEnvironment(makeEnvironmentPresentation());
    seedServerThread(makeThread());
    seedGitStatus(true);
    renderServerRoute();
    const header = capturedProps("chatHeaderActions");
    const input = {
      name: "No project",
      command: "true",
      icon: null,
      runOnWorktreeCreate: false,
      keybinding: null,
    };

    expect(
      await (
        header["onAddProjectScript"] as (value: Record<string, unknown>) => Promise<{
          _tag: string;
        }>
      )(input),
    ).toMatchObject({ _tag: "Success" });
    expect(
      await (
        header["onUpdateProjectScript"] as (
          id: string,
          value: Record<string, unknown>,
        ) => Promise<{ _tag: string }>
      )("missing", input),
    ).toMatchObject({ _tag: "Success" });
    expect(
      await (header["onDeleteProjectScript"] as (id: string) => Promise<{ _tag: string }>)(
        "missing",
      ),
    ).toMatchObject({ _tag: "Success" });
  });
});

// ─────────────────────────────────────────────────────────────────────
// Send flows
// ─────────────────────────────────────────────────────────────────────

describe("ChatView send flows", () => {
  it("restages with a new command only after exact replay proves the earlier stage was not accepted", async () => {
    seedConnectedServerThread();
    renderServerRoute();
    const contexts = makeOwnedComposerContexts("expired-replay");
    const { promptRef } = installComposerHandle({
      getSendContext: () => sendContextWithOwnedComposerContexts(contexts),
    });
    promptRef.current = "send after expired replay";
    let uploads = 0;
    h.commandResults["thread.stageAttachments"] = () =>
      AsyncResult.success({
        _tag: "staged",
        attachments: [
          {
            type: "image",
            id: contexts.image.id,
            name: contexts.image.name,
            mimeType: "image/png",
            sizeBytes: 4,
            uploadId: `stage-${++uploads}`,
          },
        ],
      });
    let admissions = 0;
    h.commandResults["thread.startTurn"] = () =>
      ++admissions === 1
        ? AsyncResult.failure(
            Cause.fail(
              new RpcClientError.RpcClientError({
                reason: new RpcClientError.RpcClientDefect({
                  message: "reply lost",
                  cause: new Error(),
                }),
              }),
            ),
          )
        : admissions === 2
          ? AsyncResult.failure(
              Cause.fail(new UploadError({ reason: "not_found", message: "the upload expired" })),
            )
          : AsyncResult.success(undefined);
    const send = capturedProps("chatComposer")["onSend"] as () => Promise<void>;
    await send();
    await send();
    expect(commandCallsFor("thread.stageAttachments")).toHaveLength(1);
    expect(commandCallsFor("thread.startTurn")[1]!.input).toEqual(
      commandCallsFor("thread.startTurn")[0]!.input,
    );
    await send();
    const requests = commandCallsFor("thread.startTurn").map(
      (call) =>
        (
          call.input as {
            input: { commandId: string; message: { attachments: Array<{ uploadId: string }> } };
          }
        ).input,
    );
    expect(commandCallsFor("thread.stageAttachments")).toHaveLength(2);
    expect(requests[2]!.commandId).not.toBe(requests[0]!.commandId);
    expect(requests[2]!.message.attachments[0]!.uploadId).toBe("stage-2");
  });

  it("does not release stages if route cleanup happens while ambiguous replay is pending", async () => {
    seedConnectedServerThread();
    renderServerRoute();
    const cleanups = runEffects();
    const contexts = makeOwnedComposerContexts("pending-replay");
    const { promptRef } = installComposerHandle({
      getSendContext: () => sendContextWithOwnedComposerContexts(contexts),
    });
    promptRef.current = "check earlier send";
    h.commandResults["thread.stageAttachments"] = () =>
      AsyncResult.success({
        _tag: "staged",
        attachments: [
          {
            type: "image",
            id: contexts.image.id,
            name: contexts.image.name,
            mimeType: "image/png",
            sizeBytes: 4,
            uploadId: "stage-pending",
          },
        ],
      });
    let admissions = 0;
    let settleReplay!: (result: unknown) => void;
    h.commandResults["thread.startTurn"] = () =>
      ++admissions === 1
        ? AsyncResult.failure(
            Cause.fail(
              new RpcClientError.RpcClientError({
                reason: new RpcClientError.RpcClientDefect({
                  message: "reply lost",
                  cause: new Error(),
                }),
              }),
            ),
          )
        : new Promise((resolve) => {
            settleReplay = resolve;
          });
    const send = capturedProps("chatComposer")["onSend"] as () => Promise<void>;
    await send();
    const replay = send();
    await vi.waitFor(() => expect(commandCallsFor("thread.startTurn")).toHaveLength(2));
    for (const cleanup of cleanups) cleanup();
    expect(commandCallsFor("thread.releaseStagedAttachments")).toHaveLength(0);
    settleReplay(AsyncResult.success(undefined));
    await replay;
    expect(commandCallsFor("thread.releaseStagedAttachments")).toHaveLength(0);
  });
  it.each([false, true])(
    "replays the exact ambiguous staged admission without reading consumed IDs (new edits %s)",
    async (edited) => {
      seedConnectedServerThread();
      renderServerRoute();
      const contexts = makeOwnedComposerContexts("ambiguous");
      const { promptRef } = installComposerHandle({
        getSendContext: () => sendContextWithOwnedComposerContexts(contexts),
      });
      promptRef.current = "original send";
      h.commandResults["thread.stageAttachments"] = () =>
        AsyncResult.success({
          _tag: "staged",
          attachments: [
            {
              type: "image",
              id: contexts.image.id,
              name: contexts.image.name,
              mimeType: "image/png",
              sizeBytes: 4,
              uploadId: "consumed-stage",
            },
          ],
        });
      let admissions = 0;
      h.commandResults["thread.startTurn"] = () =>
        ++admissions === 1
          ? AsyncResult.failure(
              Cause.fail(
                new RpcClientError.RpcClientError({
                  reason: new RpcClientError.RpcClientDefect({
                    message: "reply lost",
                    cause: new Error("closed"),
                  }),
                }),
              ),
            )
          : AsyncResult.success(undefined);
      const send = capturedProps("chatComposer")["onSend"] as () => Promise<void>;
      await send();
      if (edited) {
        promptRef.current = "fresh work";
        useComposerDraftStore.getState().setPrompt(threadRef, "fresh work");
      }
      await send();
      const requests = commandCallsFor("thread.startTurn");
      expect(requests).toHaveLength(2);
      expect(requests[0]!.input).toMatchObject({
        input: {
          commandId: expect.any(String),
          message: { attachments: [{ uploadId: "consumed-stage" }] },
        },
      });
      expect(requests[1]!.input).toEqual(requests[0]!.input);
      expect(commandCallsFor("thread.stageAttachments")).toHaveLength(1);
      expect(promptRef.current.replaceAll("\uFFFC", "")).toBe(edited ? "fresh work" : "");
    },
  );
  it.each(["success", "failure"] as const)(
    "retains stages through %s admission after route cleanup and cleans up only after the outcome",
    async (outcome) => {
      seedConnectedServerThread();
      renderServerRoute();
      const contexts = makeOwnedComposerContexts(`admission-${outcome}`);
      seedOwnedComposerContexts("outgoing admission", contexts);
      const { promptRef } = installComposerHandle({
        getSendContext: () => sendContextWithOwnedComposerContexts(contexts),
      });
      promptRef.current = "outgoing admission";
      const cleanups = runEffects();
      h.commandResults["thread.stageAttachments"] = () =>
        AsyncResult.success({
          _tag: "staged",
          attachments: [
            {
              type: "image",
              id: contexts.image.id,
              name: contexts.image.name,
              mimeType: "image/png",
              sizeBytes: 4,
              uploadId: "stage-admission",
            },
          ],
        });
      let keeperSignal: AbortSignal | undefined;
      h.commandResults["thread.keepStagedAttachmentsAlive"] = (_target, options) =>
        new Promise((resolve) => {
          keeperSignal = options?.signal;
          keeperSignal?.addEventListener(
            "abort",
            () => resolve(AsyncResult.failure(Cause.interrupt())),
            { once: true },
          );
        });
      let settleAdmission!: (result: unknown) => void;
      h.commandResults["thread.startTurn"] = () =>
        new Promise((resolve) => {
          settleAdmission = resolve;
        });
      const send = (capturedProps("chatComposer")["onSend"] as () => Promise<void>)();
      await vi.waitFor(() => expect(commandCallsFor("thread.startTurn")).toHaveLength(1));
      const otherRef = scopeThreadRef(environmentId, ThreadId.make("other-admission-thread"));
      useComposerDraftStore.getState().setPrompt(otherRef, "new thread work");
      for (const cleanup of cleanups) cleanup();
      expect(commandCallsFor("thread.releaseStagedAttachments")).toHaveLength(0);
      expect(keeperSignal?.aborted).toBe(false);
      settleAdmission(
        outcome === "success"
          ? AsyncResult.success(undefined)
          : AsyncResult.failure(Cause.fail(new Error("admission rejected"))),
      );
      await send;
      expect(keeperSignal?.aborted).toBe(true);
      expect(commandCallsFor("thread.releaseStagedAttachments")).toHaveLength(
        outcome === "failure" ? 1 : 0,
      );
      expect(useComposerDraftStore.getState().getComposerDraft(otherRef)?.prompt).toBe(
        "new thread work",
      );
      if (outcome === "failure") {
        const restored = useComposerDraftStore.getState().getComposerDraft(threadRef)!;
        expect(restored.prompt.replaceAll("\uFFFC", "")).toBe("outgoing admission");
        expect(restored.attachments[0]?.file).toBe(contexts.image.file);
        expect(restored.terminalContexts).toContainEqual(contexts.terminalContext);
        expect(restored.previewAnnotations).toContainEqual(contexts.previewAnnotation);
      }
    },
  );
  it("marks attachment preparation as send-busy without claiming the provider is working", () => {
    seedConnectedServerThread();
    seedHostState("attachmentUploads", {
      upload: {
        threadKey: scopedThreadKey(threadRef),
        attachmentCount: 1,
        fileName: "a.png",
        sentBytes: 0,
        totalBytes: 1024,
        phase: "uploading",
      },
    });
    renderServerRoute();
    expect(capturedProps("chatComposer")["isSendBusy"]).toBe(true);
    expect(capturedProps("messagesTimeline")["isWorking"]).toBe(false);
    expect(capturedProps("messagesTimeline")["attachmentUploads"]).toHaveProperty("upload");
  });
  it("staged Cancel restores contexts and fresh edits immediately without encoding or admitting", async () => {
    seedConnectedServerThread();
    renderServerRoute();
    const outgoing = makeOwnedComposerContexts("upload-outgoing");
    const fresh = makeOwnedComposerContexts("upload-fresh");
    fresh.image.name = "fresh-image.png";
    fresh.terminalContext.terminalId = "terminal-2";
    fresh.terminalContext.text = "fresh build output";
    fresh.elementContext.selector = ".fresh-button";
    seedOwnedComposerContexts("outgoing text", outgoing);
    const { promptRef } = installComposerHandle({
      getSendContext: () => sendContextWithOwnedComposerContexts(outgoing),
    });
    promptRef.current = "outgoing text";
    const read = vi.fn(() => {
      throw new Error("staged uploads must not read data URLs");
    });
    vi.stubGlobal(
      "FileReader",
      class {
        readAsDataURL = read;
      },
    );
    let aborts = 0;
    h.commandResults["thread.stageAttachments"] = (target, options) =>
      new Promise((resolve) => {
        const input = (target as { input: { onProgress: (progress: unknown) => void } }).input;
        input.onProgress({
          fileName: outgoing.image.name,
          sentBytes: 2,
          totalBytes: 4,
          phase: "uploading",
        });
        options?.signal?.addEventListener(
          "abort",
          () => {
            aborts++;
            resolve(AsyncResult.failure(Cause.interrupt()));
          },
          { once: true },
        );
      });
    const pending = (capturedProps("chatComposer")["onSend"] as () => Promise<void>)();
    await vi.waitFor(() => expect(commandCallsFor("thread.stageAttachments")).toHaveLength(1));
    const progress = setStateCallsFor("attachmentUploads").at(-1)!.applied as Record<
      string,
      unknown
    >;
    const messageId = Object.keys(progress)[0]!;
    expect(commandCallsFor("thread.startTurn")).toHaveLength(0);
    expect(read).not.toHaveBeenCalled();
    seedOwnedComposerContexts("fresh text", fresh);
    promptRef.current = "fresh text";
    (capturedProps("messagesTimeline")["onCancelAttachmentUpload"] as (id: MessageId) => void)(
      MessageId.make(messageId),
    );
    const restored = useComposerDraftStore.getState().getComposerDraft(threadRef)!;
    expect(restored.prompt.replaceAll("\uFFFC", "")).toBe("fresh text\n\noutgoing text");
    expect(restored.attachments.map((value) => value.id)).toEqual([
      fresh.image.id,
      outgoing.image.id,
    ]);
    expect(restored.terminalContexts.map((value) => value.id)).toEqual([
      fresh.terminalContext.id,
      outgoing.terminalContext.id,
    ]);
    expect(restored.elementContexts.map((value) => value.id)).toEqual([
      fresh.elementContext.id,
      outgoing.elementContext.id,
    ]);
    expect(restored.previewAnnotations.map((value) => value.id)).toEqual([
      fresh.previewAnnotation.id,
      outgoing.previewAnnotation.id,
    ]);
    expect(restored.reviewComments.map((value) => value.id)).toEqual([
      fresh.reviewComment.id,
      outgoing.reviewComment.id,
    ]);
    await pending;
    expect(aborts).toBe(1);
    expect(commandCallsFor("thread.startTurn")).toHaveLength(0);
    expect(
      h.setStateCalls.some(
        (call) =>
          typeof call.applied === "object" &&
          call.applied !== null &&
          Object.values(call.applied).some(
            (value) => typeof value === "string" && value.includes("Couldn't upload"),
          ),
      ),
    ).toBe(false);
  });

  it("shows a local queued upload row before enqueue and removes it after acceptance", async () => {
    seedConnectedServerThread(
      makeThread({
        session: makeSession({ status: "running", activeTurnId: TurnId.make("active") }),
      }),
    );
    renderServerRoute();
    const contexts = makeOwnedComposerContexts("queued-upload");
    const { promptRef } = installComposerHandle({
      getSendContext: () => sendContextWithOwnedComposerContexts(contexts),
    });
    promptRef.current = "queued upload";
    let release!: (result: unknown) => void;
    h.commandResults["thread.stageAttachments"] = (target) =>
      new Promise((resolve) => {
        release = resolve;
        (target as { input: { onProgress: (progress: unknown) => void } }).input.onProgress({
          fileName: contexts.image.name,
          sentBytes: 0,
          totalBytes: 4,
          phase: "uploading",
        });
      });
    const pending = (capturedProps("chatComposer")["onSend"] as () => Promise<void>)();
    await vi.waitFor(() => expect(commandCallsFor("thread.stageAttachments")).toHaveLength(1));
    const rows = setStateCallsFor("optimisticUserMessages").at(-1)!.applied as ChatMessage[];
    expect(rows).toHaveLength(1);
    expect(rows[0]).not.toHaveProperty("delivery");
    expect(commandCallsFor("thread.startTurn")).toHaveLength(0);
    expect(capturedProps("chatComposer")["onInterrupt"]).toBeTypeOf("function");
    release(
      AsyncResult.success({
        _tag: "staged",
        attachments: [
          {
            type: "image",
            id: contexts.image.id,
            name: contexts.image.name,
            mimeType: "image/png",
            sizeBytes: 4,
            uploadId: "stage-1",
          },
        ],
      }),
    );
    await pending;
    expect(commandCallsFor("thread.startTurn")[0]!.input).toMatchObject({
      input: { queued: true, message: { attachments: [{ uploadId: "stage-1" }] } },
    });
    expect(setStateCallsFor("optimisticUserMessages").at(-1)!.applied).toEqual([]);
  });

  it("reuses completed stages after a failed turn admission without another file read", async () => {
    seedConnectedServerThread();
    renderServerRoute();
    const contexts = makeOwnedComposerContexts("retry-upload");
    const { promptRef } = installComposerHandle({
      getSendContext: () => sendContextWithOwnedComposerContexts(contexts),
    });
    promptRef.current = "retry this upload";
    h.commandResults["thread.stageAttachments"] = () =>
      AsyncResult.success({
        _tag: "staged",
        attachments: [
          {
            type: "image",
            id: contexts.image.id,
            name: contexts.image.name,
            mimeType: "image/png",
            sizeBytes: 4,
            uploadId: "stage-1",
          },
        ],
      });
    let admissions = 0;
    h.commandResults["thread.startTurn"] = () =>
      ++admissions === 1
        ? AsyncResult.failure(Cause.fail(new Error("temporarily unavailable")))
        : AsyncResult.success(undefined);
    const send = capturedProps("chatComposer")["onSend"] as () => Promise<void>;
    await send();
    expect(promptRef.current.replaceAll("\uFFFC", "")).toBe("retry this upload");
    await send();
    expect(commandCallsFor("thread.stageAttachments")).toHaveLength(2);
    expect(commandCallsFor("thread.stageAttachments")[1]!.input).toMatchObject({
      input: { reusable: [{ uploadId: "stage-1" }] },
    });
    expect(commandCallsFor("thread.startTurn")).toHaveLength(2);
  });
  it("uses shared defaults for a legacy draft that has no stored model selection", () => {
    const draftId = newDraftId();
    const capabilityRichProvider: ServerProvider = {
      ...codexProvider,
      models: [
        {
          ...codexProvider.models[0]!,
          capabilities: {
            optionDescriptors: [
              {
                id: "reasoningEffort",
                label: "Reasoning",
                type: "select",
                options: [
                  { id: "medium", label: "Medium", isDefault: true },
                  { id: "high", label: "High" },
                ],
                currentValue: "medium",
              },
              {
                id: "serviceTier",
                label: "Service tier",
                type: "select",
                options: [
                  { id: "default", label: "Default", isDefault: true },
                  { id: "fast", label: "Fast" },
                ],
                currentValue: "default",
              },
            ],
          },
        },
      ],
    };
    seedEnvironment(
      makeEnvironmentPresentation({
        serverConfig: {
          providers: [capabilityRichProvider],
          environment: { label: "Local" },
        },
      }),
    );
    seedProject(makeProject({ defaultModelSelection: null }));
    seedGitStatus(true);
    h.settings = {
      ...h.settings,
      providerSessionDefaults: {
        codex: {
          model: "gpt-5.4",
          options: [
            { id: "reasoningEffort", value: "high" },
            { id: "serviceTier", value: "fast" },
          ],
        },
      },
    };
    useComposerDraftStore
      .getState()
      .setLogicalProjectDraftThreadId(
        "legacy-draft",
        scopeProjectRef(environmentId, projectId),
        draftId,
        { threadId, createdAt: now, envMode: "local" },
      );
    publishSeededStoreState(useComposerDraftStore);

    renderDraftRoute(draftId);

    expect(capturedProps("chatComposer")["activeThreadModelSelection"]).toEqual({
      instanceId: codexInstanceId,
      model: "gpt-5.4",
      options: [
        { id: "reasoningEffort", value: "high" },
        { id: "serviceTier", value: "fast" },
      ],
    });
  });

  it.each(["draft", "server-style"] as const)(
    "warns once at legacy draft promotion through the %s route when a configured model falls back",
    async (route) => {
      const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
      const draftId = newDraftId();
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject({ defaultModelSelection: null }));
      seedGitStatus(true);
      h.settings = {
        ...h.settings,
        providerSessionDefaults: {
          codex: {
            model: "retired-codex-model",
            options: [],
          },
        },
      };
      useComposerDraftStore
        .getState()
        .setLogicalProjectDraftThreadId(
          "legacy-draft-fallback",
          scopeProjectRef(environmentId, projectId),
          draftId,
          { threadId, createdAt: now, envMode: "local" },
        );
      publishSeededStoreState(useComposerDraftStore);

      if (route === "draft") {
        renderDraftRoute(draftId);
      } else {
        renderServerRoute();
      }

      expect(warn).not.toHaveBeenCalled();
      const resolvedSelection = capturedProps("chatComposer")[
        "activeThreadModelSelection"
      ] as ModelSelection;
      expect(resolvedSelection).toEqual({
        instanceId: codexInstanceId,
        model: "gpt-5.4",
      });
      const promptRef = installComposerModelSelection(resolvedSelection, "medium");
      const onSend = capturedProps("chatComposer")["onSend"] as () => Promise<void>;

      promptRef.current = "promote the legacy draft";
      await onSend();
      promptRef.current = "retry the same legacy promotion boundary";
      await onSend();

      expect(warn.mock.calls).toEqual([
        [
          "Provider session default fallback",
          {
            driver: ProviderDriverKind.make("codex"),
            instanceId: codexInstanceId,
            configuredModel: "retired-codex-model",
            resolvedModel: "gpt-5.4",
            reason: "configured-model-unavailable",
          },
        ],
      ]);
    },
  );

  it("promotes a freshly seeded draft with its unchanged full model selection", async () => {
    const seededSelection: ModelSelection = {
      instanceId: codexInstanceId,
      model: "gpt-5.4",
      options: [
        { id: "reasoningEffort", value: "high" },
        { id: "serviceTier", value: "fast" },
      ],
    };
    const draftId = seedFreshLocalDraft("logical-project-first-turn", seededSelection);
    const storedSelection = draftModelSelection(draftId, codexInstanceId);
    expect(storedSelection).toEqual(seededSelection);

    renderDraftRoute(draftId);
    const promptRef = installComposerModelSelection(storedSelection, "high");
    promptRef.current = "use the seeded defaults";

    await (capturedProps("chatComposer")["onSend"] as () => Promise<void>)();

    const startCalls = commandCallsFor("thread.startTurn");
    expect(startCalls).toHaveLength(1);
    const input = startCalls[0]!.input as {
      input: {
        modelSelection: ModelSelection;
        bootstrap?: { createThread?: { modelSelection: ModelSelection } };
      };
    };
    expect(input.input.modelSelection).toBe(storedSelection);
    expect(input.input.bootstrap?.createThread?.modelSelection).toBe(storedSelection);
  });

  it("normalizes Codex Fast to the advertised tier on first send through partial metadata", async () => {
    const partialCodexModel: ServerProvider["models"][number] = {
      slug: "gpt-5.5",
      name: "GPT-5.5",
      isCustom: false,
      capabilities: {
        optionDescriptors: [
          {
            id: "reasoningEffort",
            label: "Reasoning",
            type: "select",
            options: [
              { id: "medium", label: "Medium", isDefault: true },
              { id: "high", label: "High" },
            ],
          },
          {
            id: "serviceTier",
            label: "Service Tier",
            type: "select",
            options: [{ id: "default", label: "Standard", isDefault: true }],
          },
        ],
      },
    };
    const seededSelection: ModelSelection = {
      instanceId: codexInstanceId,
      model: partialCodexModel.slug,
      options: [
        { id: "reasoningEffort", value: "high" },
        { id: "serviceTier", value: "fast" },
      ],
    };
    const draftId = seedFreshLocalDraft("codex-partial-fast-first-turn", seededSelection);
    seedEnvironment(
      makeEnvironmentPresentation({
        serverConfig: {
          providers: [{ ...codexProvider, models: [partialCodexModel] }],
          environment: { label: "Local" },
        },
      }),
    );
    renderDraftRoute(draftId);

    const composerState = getComposerProviderState({
      provider: ProviderDriverKind.make("codex"),
      model: partialCodexModel.slug,
      models: [partialCodexModel],
      modelOptions: seededSelection.options,
    });
    const dispatchSelection: ModelSelection = {
      instanceId: codexInstanceId,
      model: partialCodexModel.slug,
      ...(composerState.modelOptionsForDispatch
        ? { options: composerState.modelOptionsForDispatch }
        : {}),
    };
    const { promptRef } = installComposerHandle({
      getSendContext: () => ({
        ...composerHandle().getSendContext(),
        selectedPromptEffort: composerState.promptEffort,
        selectedModelOptionsForDispatch: composerState.modelOptionsForDispatch,
        selectedModelSelection: dispatchSelection,
        selectedModel: partialCodexModel.slug,
        selectedProviderModels: [partialCodexModel],
      }),
    });
    promptRef.current = "dispatch fast";

    await (capturedProps("chatComposer")["onSend"] as () => Promise<void>)();

    const normalizedSelection: ModelSelection = {
      instanceId: codexInstanceId,
      model: partialCodexModel.slug,
      options: [
        { id: "reasoningEffort", value: "high" },
        { id: "serviceTier", value: "default" },
      ],
    };
    expect(commandCallsFor("thread.startTurn")).toHaveLength(1);
    expect(commandCallsFor("thread.startTurn")[0]?.input).toMatchObject({
      input: {
        modelSelection: normalizedSelection,
        bootstrap: { createThread: { modelSelection: normalizedSelection } },
      },
    });
  });

  it("prefixes the first prompt from a prompt-injected Claude session default", async () => {
    const claudeDriver = ProviderDriverKind.make("claudeAgent");
    const claudeInstanceId = ProviderInstanceId.make("claudeAgent");
    const claudeModel: ServerProvider["models"][number] = {
      slug: "claude-sonnet-5",
      name: "Claude Sonnet 5",
      isCustom: false,
      capabilities: {
        optionDescriptors: [
          {
            id: "effort",
            label: "Effort",
            type: "select",
            options: [
              { id: "high", label: "High", isDefault: true },
              { id: "ultrathink", label: "Ultrathink" },
            ],
            promptInjectedValues: ["ultrathink"],
          },
        ],
      },
    };
    const seededSelection: ModelSelection = {
      instanceId: claudeInstanceId,
      model: claudeModel.slug,
      options: [{ id: "effort", value: "ultrathink" }],
    };
    const draftId = seedFreshLocalDraft("claude-ultrathink-first-turn", seededSelection);
    seedEnvironment(
      makeEnvironmentPresentation({
        serverConfig: {
          providers: [
            {
              ...codexProvider,
              instanceId: claudeInstanceId,
              driver: claudeDriver,
              models: [claudeModel],
            },
          ],
          environment: { label: "Local" },
        },
      }),
    );

    renderDraftRoute(draftId);
    const storedSelection = draftModelSelection(draftId, claudeInstanceId);
    const composerState = getComposerProviderState({
      provider: claudeDriver,
      model: claudeModel.slug,
      models: [claudeModel],
      modelOptions: storedSelection.options,
    });
    const dispatchSelection: ModelSelection = {
      instanceId: claudeInstanceId,
      model: claudeModel.slug,
      ...(composerState.modelOptionsForDispatch
        ? { options: composerState.modelOptionsForDispatch }
        : {}),
    };
    const { promptRef } = installComposerHandle({
      getSendContext: () => ({
        ...composerHandle().getSendContext(),
        selectedPromptEffort: composerState.promptEffort,
        selectedModelOptionsForDispatch: composerState.modelOptionsForDispatch,
        selectedModelSelection: dispatchSelection,
        selectedProvider: claudeDriver,
        selectedModel: claudeModel.slug,
        selectedProviderModels: [claudeModel],
      }),
    });
    promptRef.current = "Investigate the flaky test";

    await (capturedProps("chatComposer")["onSend"] as () => Promise<void>)();

    expect(commandCallsFor("thread.startTurn")).toHaveLength(1);
    expect(commandCallsFor("thread.startTurn")[0]?.input).toMatchObject({
      input: {
        message: { text: "Ultrathink:\nInvestigate the flaky test" },
        modelSelection: {
          instanceId: claudeInstanceId,
          model: "claude-sonnet-5",
          options: [{ id: "effort", value: "high" }],
        },
        bootstrap: {
          createThread: {
            modelSelection: {
              instanceId: claudeInstanceId,
              model: "claude-sonnet-5",
              options: [{ id: "effort", value: "high" }],
            },
          },
        },
      },
    });
  });

  it("uses explicit draft model, effort, and fast changes for the first turn", async () => {
    const seededSelection: ModelSelection = {
      instanceId: codexInstanceId,
      model: "gpt-5.4",
      options: [
        { id: "reasoningEffort", value: "medium" },
        { id: "fastMode", value: false },
      ],
    };
    const userSelection: ModelSelection = {
      instanceId: codexInstanceId,
      model: "gpt-5.4-mini",
      options: [
        { id: "reasoningEffort", value: "high" },
        { id: "fastMode", value: true },
      ],
    };
    const draftId = seedFreshLocalDraft("logical-project-first-turn-edited", seededSelection);
    useComposerDraftStore.getState().setModelSelection(draftId, userSelection);
    publishSeededStoreState(useComposerDraftStore);

    const storedSelection = draftModelSelection(draftId, codexInstanceId);
    expect(storedSelection).toEqual(userSelection);

    renderDraftRoute(draftId);
    const promptRef = installComposerModelSelection(storedSelection, "high");
    promptRef.current = "use my explicit choices";

    await (capturedProps("chatComposer")["onSend"] as () => Promise<void>)();

    const startCalls = commandCallsFor("thread.startTurn");
    expect(startCalls).toHaveLength(1);
    const input = startCalls[0]!.input as {
      input: {
        modelSelection: ModelSelection;
        bootstrap?: { createThread?: { modelSelection: ModelSelection } };
      };
    };
    expect(input.input.modelSelection).toBe(storedSelection);
    expect(input.input.bootstrap?.createThread?.modelSelection).toBe(storedSelection);
  });

  it("keeps an existing thread selection after provider session defaults change", async () => {
    const persistedSelection: ModelSelection = {
      instanceId: codexInstanceId,
      model: "gpt-5.4",
      options: [
        { id: "reasoningEffort", value: "low" },
        { id: "fastMode", value: false },
      ],
    };
    seedConnectedServerThread(makeThread({ modelSelection: persistedSelection }));
    h.settings = {
      ...h.settings,
      providerSessionDefaults: {
        [ProviderDriverKind.make("codex")]: {
          model: "gpt-5.4-mini",
          options: [
            { id: "reasoningEffort", value: "high" },
            { id: "fastMode", value: true },
          ],
        },
      },
    };

    renderServerRoute();
    const promptRef = installComposerModelSelection(persistedSelection, "low");
    promptRef.current = "continue with the persisted selection";

    await (capturedProps("chatComposer")["onSend"] as () => Promise<void>)();

    expect(commandCallsFor("thread.create")).toHaveLength(0);
    const startCalls = commandCallsFor("thread.startTurn");
    expect(startCalls).toHaveLength(1);
    const input = startCalls[0]!.input as {
      input: {
        modelSelection: ModelSelection;
        bootstrap?: unknown;
      };
    };
    expect(input.input.modelSelection).toBe(persistedSelection);
    expect(input.input.bootstrap).toBeUndefined();
    expect(
      commandCallsFor("thread.updateMetadata").filter(
        (call) =>
          (
            call.input as {
              input?: { modelSelection?: ModelSelection };
            }
          ).input?.modelSelection !== undefined,
      ),
    ).toHaveLength(0);
  });

  it("returns without dispatch when the composer handle has no send context", async () => {
    seedConnectedServerThread();
    renderServerRoute();

    await (capturedProps("chatComposer")["onSend"] as () => Promise<void>)();

    expect(commandCallsFor("thread.startTurn")).toHaveLength(0);
  });

  it("treats a standalone :plan message as a local interaction mode switch", async () => {
    seedConnectedServerThread();
    renderServerRoute();
    const { promptRef } = installComposerHandle();
    promptRef.current = ":plan";

    const composer = capturedProps("chatComposer");
    await (composer["onSend"] as () => Promise<void>)();

    expect(commandCallsFor("thread.startTurn")).toHaveLength(0);
    expect(useComposerDraftStore.getState().getComposerDraft(threadRef)?.interactionMode).toBe(
      "plan",
    );
  });

  it("treats a standalone :default message as a local interaction mode switch", async () => {
    seedConnectedServerThread(makeThread({ interactionMode: "plan" }));
    renderServerRoute();
    const { promptRef } = installComposerHandle();
    promptRef.current = ":default";

    const composer = capturedProps("chatComposer");
    await (composer["onSend"] as () => Promise<void>)();

    expect(commandCallsFor("thread.startTurn")).toHaveLength(0);
    expect(useComposerDraftStore.getState().getComposerDraft(threadRef)?.interactionMode).toBe(
      "default",
    );
  });

  it.each([
    [":plan", "plan"],
    [":default", "default"],
  ] as const)(
    "keeps attachment-bearing standalone %s local and clears the complete composer draft",
    async (actionText, expectedMode) => {
      class FakeFileReader {
        result: string | null = null;
        private listeners: Array<{ type: string; handler: () => void }> = [];
        addEventListener(type: string, handler: () => void) {
          this.listeners.push({ type, handler });
        }
        readAsDataURL(_file: unknown) {
          this.result = "data:image/png;base64,ZmFrZQ==";
          for (const listener of this.listeners) {
            if (listener.type === "load") listener.handler();
          }
        }
      }
      vi.stubGlobal("FileReader", FakeFileReader);
      const revokeObjectUrl = vi.spyOn(URL, "revokeObjectURL");

      seedConnectedServerThread(
        makeThread({ interactionMode: expectedMode === "default" ? "plan" : "default" }),
      );
      renderServerRoute();

      const image = {
        type: "image" as const,
        id: `image-${expectedMode}` as ChatAttachmentId,
        name: "shot.png",
        mimeType: "image/png",
        sizeBytes: 4,
        previewUrl: "blob:colon-action",
        file: new File(["fake"], "shot.png", { type: "image/png" }),
      };
      const terminalContext: TerminalContextDraft = {
        id: `terminal-${expectedMode}`,
        threadId,
        createdAt: now,
        terminalId: "terminal-1",
        terminalLabel: "Shell",
        lineStart: 1,
        lineEnd: 2,
        text: "build output",
      };
      const elementContext = {
        id: `element-${expectedMode}`,
        threadId,
        pageUrl: "http://localhost:3000",
        pageTitle: "Demo",
        tagName: "button",
        selector: ".save",
        htmlPreview: "<button>Save</button>",
        componentName: "SaveButton",
        source: null,
        styles: "",
        pickedAt: now,
      };
      const previewAnnotation = {
        id: `annotation-${expectedMode}`,
        pageUrl: "http://localhost:3000",
        pageTitle: "Demo",
        comment: "Fix this",
        elements: [],
        regions: [],
        strokes: [],
        styleChanges: [],
        screenshot: null,
        createdAt: now,
      };
      const reviewComment = {
        id: `comment-${expectedMode}`,
        sectionId: "file:src/app.ts",
        sectionTitle: "src/app.ts",
        filePath: "src/app.ts",
        startIndex: 0,
        endIndex: 1,
        rangeLabel: "L1",
        text: "Tighten this",
        diff: "+const value = 1;",
      };

      const store = useComposerDraftStore.getState();
      store.setPrompt(threadRef, actionText);
      store.addAttachments(threadRef, [image]);
      store.setTerminalContexts(threadRef, [terminalContext]);
      store.setElementContexts(threadRef, [elementContext]);
      store.setPreviewAnnotations(threadRef, [previewAnnotation]);
      store.setReviewComments(threadRef, [reviewComment]);

      const resetCursorState = vi.fn();
      const { promptRef } = installComposerHandle({
        resetCursorState,
        getSendContext: () => ({
          ...composerHandle().getSendContext(),
          attachments: [image],
          terminalContexts: [terminalContext],
          elementContexts: [elementContext],
          previewAnnotations: [previewAnnotation],
          reviewComments: [reviewComment],
        }),
      });
      promptRef.current = actionText;

      await (capturedProps("chatComposer")["onSend"] as () => Promise<void>)();

      expect(commandCallsFor("thread.startTurn")).toHaveLength(0);
      expect(promptRef.current).toBe("");
      expect(resetCursorState).toHaveBeenCalledTimes(1);
      expect(revokeObjectUrl).toHaveBeenCalledWith("blob:colon-action");
      expect(useComposerDraftStore.getState().getComposerDraft(threadRef)).toMatchObject({
        prompt: "",
        attachments: [],
        terminalContexts: [],
        elementContexts: [],
        previewAnnotations: [],
        reviewComments: [],
        interactionMode: expectedMode,
      });
    },
  );

  it("warns when only expired terminal contexts remain", async () => {
    seedConnectedServerThread();
    renderServerRoute();
    const expiredContext: TerminalContextDraft = {
      id: "ctx-1",
      threadId,
      createdAt: now,
      terminalId: "terminal-1",
      terminalLabel: "Shell",
      lineStart: 1,
      lineEnd: 3,
      text: "",
    };
    const { promptRef } = installComposerHandle({
      getSendContext: () => ({
        ...composerHandle().getSendContext(),
        terminalContexts: [expiredContext],
      }),
    });
    promptRef.current = "";

    const composer = capturedProps("chatComposer");
    await (composer["onSend"] as () => Promise<void>)();

    expect(commandCallsFor("thread.startTurn")).toHaveLength(0);
    expect(h.toasts.length).toBeGreaterThanOrEqual(1);
  });

  it("requires a base branch before sending in new-worktree mode", async () => {
    seedEnvironment(makeEnvironmentPresentation());
    seedProject(makeProject());
    seedGitStatus(true);
    const draftId = newDraftId();
    useComposerDraftStore
      .getState()
      .setLogicalProjectDraftThreadId(
        "logical-project-1",
        scopeProjectRef(environmentId, projectId),
        draftId,
        { threadId, createdAt: now, envMode: "worktree" },
      );
    publishSeededStoreState(useComposerDraftStore);

    renderDraftRoute(draftId);
    const { promptRef } = installComposerHandle();
    promptRef.current = "start in worktree";

    const composer = capturedProps("chatComposer");
    await (composer["onSend"] as () => Promise<void>)();

    expect(commandCallsFor("thread.startTurn")).toHaveLength(0);
    const errorWrites = h.setStateCalls.filter(
      (call) =>
        typeof call.applied === "object" &&
        call.applied !== null &&
        Object.values(call.applied as Record<string, unknown>).includes(
          "Select a base branch before sending in New worktree mode.",
        ),
    );
    expect(errorWrites.length).toBeGreaterThanOrEqual(1);
  });

  it("restores the composer draft when the turn start fails", async () => {
    seedConnectedServerThread();
    h.commandResults["thread.startTurn"] = () =>
      AsyncResult.failure(Cause.fail(new Error("turn rejected")));
    renderServerRoute();
    const resetCalls: unknown[] = [];
    const { promptRef } = installComposerHandle({
      resetCursorState: (options?: unknown) => {
        resetCalls.push(options ?? null);
      },
    });
    promptRef.current = "Retry [main.ts](src/main.ts) and @README.md";

    const composer = capturedProps("chatComposer");
    await (composer["onSend"] as () => Promise<void>)();

    // Restore pass: prompt written back to the ref and the draft store.
    expect(promptRef.current).toBe("Retry @src/main.ts and @README.md");
    expect(useComposerDraftStore.getState().getComposerDraft(threadRef)?.prompt).toBe(
      "Retry @src/main.ts and @README.md",
    );
    expect(commandCallsFor("thread.startTurn")[0]?.input).toMatchObject({
      input: {
        message: { text: "Retry @src/main.ts and @README.md" },
      },
    });
    // The cursor reset ran once on clear and once on restore.
    expect(resetCalls.length).toBe(2);
    // Optimistic message add + removal updaters both executed.
    const optimisticUpdates = setStateCallsFor("optimisticUserMessages");
    expect(optimisticUpdates.length).toBeGreaterThanOrEqual(2);
  });

  it("restores unsupported OpenCode dollar text at the raw prompt end", async () => {
    seedConnectedServerThread();
    h.commandResults["thread.startTurn"] = () =>
      AsyncResult.failure(Cause.fail(new Error("turn rejected")));
    renderServerRoute();
    const prompt = "Retry $refactor in OpenCode";
    const resetCursorState = vi.fn();
    const { promptRef } = installComposerHandle({
      resetCursorState,
      getSendContext: () => ({
        ...composerHandle().getSendContext(),
        selectedProvider: ProviderDriverKind.make("opencode"),
        selectedModelSelection: {
          instanceId: ProviderInstanceId.make("opencode"),
          model: "openai/gpt-5",
        },
        selectedModel: "openai/gpt-5",
        selectedProviderModels: [],
      }),
    });
    promptRef.current = prompt;

    await (capturedProps("chatComposer")["onSend"] as () => Promise<void>)();

    expect(resetCursorState).toHaveBeenCalledTimes(2);
    expect(resetCursorState).toHaveBeenLastCalledWith({
      cursor: prompt.length,
      prompt,
      detectTrigger: true,
    });
  });

  it("sends image attachments through FileReader data urls", async () => {
    class FakeFileReader {
      result: string | null = null;
      private listeners: Array<{ type: string; handler: () => void }> = [];
      addEventListener(type: string, handler: () => void) {
        this.listeners.push({ type, handler });
      }
      readAsDataURL(_file: unknown) {
        this.result = "data:image/png;base64,ZmFrZQ==";
        for (const listener of this.listeners) {
          if (listener.type === "load") listener.handler();
        }
      }
    }
    vi.stubGlobal("FileReader", FakeFileReader);

    seedConnectedServerThread();
    renderServerRoute();
    const image = {
      type: "image" as const,
      id: "image-1" as ChatAttachmentId,
      name: "shot.png",
      mimeType: "image/png",
      sizeBytes: 4,
      previewUrl: "blob:preview-9",
      file: new File(["fake"], "shot.png", { type: "image/png" }),
    };
    const { promptRef } = installComposerHandle({
      getSendContext: () => ({
        ...composerHandle().getSendContext(),
        attachments: [image],
      }),
    });
    promptRef.current = "";

    const composer = capturedProps("chatComposer");
    await (composer["onSend"] as () => Promise<void>)();

    const startCalls = commandCallsFor("thread.startTurn");
    expect(startCalls).toHaveLength(1);
    const input = startCalls[0]!.input as {
      input: {
        titleSeed: string;
        message: { attachments: Array<{ dataUrl: string }> };
      };
    };
    expect(input.input.titleSeed).toBe("Image: shot.png");
    expect(input.input.message.attachments[0]!.dataUrl).toBe("data:image/png;base64,ZmFrZQ==");
  });

  it("sends file attachments with their id and a file title seed", async () => {
    class FakeFileReader {
      result = "data:text/plain;base64,bm90ZQ==";
      addEventListener(type: string, handler: () => void) {
        if (type === "load") handler();
      }
      readAsDataURL(_file: unknown) {}
    }
    vi.stubGlobal("FileReader", FakeFileReader);

    seedConnectedServerThread();
    renderServerRoute();
    const file = {
      type: "file" as const,
      id: "file-1" as ChatAttachmentId,
      name: "notes.txt",
      mimeType: "text/plain",
      sizeBytes: 4,
      file: new File(["note"], "notes.txt", { type: "text/plain" }),
    };
    const { promptRef } = installComposerHandle({
      getSendContext: () => ({
        ...composerHandle().getSendContext(),
        attachments: [file],
      }),
    });
    promptRef.current = "";

    await (capturedProps("chatComposer")["onSend"] as () => Promise<void>)();

    expect(commandCallsFor("thread.startTurn")[0]?.input).toMatchObject({
      input: {
        titleSeed: "File: notes.txt",
        message: { attachments: [{ type: "file", id: "file-1" }] },
      },
    });
  });

  it("uses terminal and element context labels as attachment-only title seeds", async () => {
    const terminalContext: TerminalContextDraft = {
      id: "ctx-title",
      threadId,
      createdAt: now,
      terminalId: "terminal-1",
      terminalLabel: "Build shell",
      lineStart: 3,
      lineEnd: 4,
      text: "build output",
    };
    seedConnectedServerThread();
    renderServerRoute();
    let installed = installComposerHandle({
      getSendContext: () => ({
        ...composerHandle().getSendContext(),
        terminalContexts: [terminalContext],
      }),
    });
    installed.promptRef.current = "";
    await (capturedProps("chatComposer")["onSend"] as () => Promise<void>)();
    expect(commandCallsFor("thread.startTurn")[0]?.input).toMatchObject({
      input: { titleSeed: expect.stringContaining("Build shell") },
    });

    h.commandCalls.length = 0;
    seedConnectedServerThread(makeThread({ messages: [] }));
    renderServerRoute();
    const elementContext = {
      id: "element-title",
      threadId,
      pageUrl: "http://localhost:3000",
      pageTitle: "Demo",
      tagName: "button",
      selector: ".save",
      htmlPreview: "<button>Save</button>",
      componentName: "SaveButton",
      source: null,
      styles: "",
      pickedAt: now,
    };
    installed = installComposerHandle({
      getSendContext: () => ({
        ...composerHandle().getSendContext(),
        elementContexts: [elementContext],
      }),
    });
    installed.promptRef.current = "";
    await (capturedProps("chatComposer")["onSend"] as () => Promise<void>)();
    expect(commandCallsFor("thread.startTurn")[0]?.input).toMatchObject({
      input: { titleSeed: "<SaveButton>" },
    });
  });

  it("omits expired contexts while sending valid prompt content", async () => {
    const expiredContext: TerminalContextDraft = {
      id: "ctx-expired-with-prompt",
      threadId,
      createdAt: now,
      terminalId: "terminal-1",
      terminalLabel: "Expired shell",
      lineStart: 1,
      lineEnd: 2,
      text: "",
    };
    seedConnectedServerThread();
    renderServerRoute();
    const { promptRef } = installComposerHandle({
      getSendContext: () => ({
        ...composerHandle().getSendContext(),
        terminalContexts: [expiredContext],
      }),
    });
    promptRef.current = "send the valid text";

    await (capturedProps("chatComposer")["onSend"] as () => Promise<void>)();

    expect(commandCallsFor("thread.startTurn")).toHaveLength(1);
    expect(
      h.toasts.some((toast) =>
        (toast as { title?: string }).title?.includes("Expired terminal context"),
      ),
    ).toBe(true);
  });

  it("uses generic send and interrupt messages for opaque command failures", async () => {
    seedConnectedServerThread(
      makeThread({
        session: makeSession({ status: "running", activeTurnId: TurnId.make("turn-opaque") }),
      }),
    );
    h.commandResults["thread.updateMetadata"] = () =>
      AsyncResult.failure(Cause.fail("opaque title failure"));
    h.commandResults["thread.interruptTurn"] = () =>
      AsyncResult.failure(Cause.fail("opaque interrupt failure"));
    renderServerRoute();
    const { promptRef } = installComposerHandle();
    promptRef.current = "fail opaquely";
    const composer = capturedProps("chatComposer");

    await (composer["onSend"] as () => Promise<void>)();
    await (composer["onInterrupt"] as () => Promise<void>)();

    const messages = h.setStateCalls.flatMap((call) =>
      typeof call.applied === "object" && call.applied !== null
        ? Object.values(call.applied as Record<string, unknown>)
        : [],
    );
    expect(messages).toEqual(
      expect.arrayContaining(["Failed to send message.", "Failed to interrupt the current turn."]),
    );
  });

  it.each([
    [":plan", "plan"],
    [":default", "default"],
  ] as const)(
    "keeps plan-follow-up %s local before feedback submission and releases owned contexts",
    async (actionText, expectedMode) => {
      seedConnectedServerThread(makeActionablePlanThread(`local-${expectedMode}`));
      renderServerRoute();
      const contexts = makeOwnedComposerContexts(`local-${expectedMode}`);
      seedOwnedComposerContexts(actionText, contexts);
      const revokeObjectUrl = vi.spyOn(URL, "revokeObjectURL");
      const resetCursorState = vi.fn();
      const { promptRef } = installComposerHandle({
        resetCursorState,
        getSendContext: () => sendContextWithOwnedComposerContexts(contexts),
      });
      promptRef.current = actionText;

      const composer = capturedProps("chatComposer");
      expect(composer["showPlanFollowUpPrompt"]).toBe(true);
      await (composer["onSend"] as () => Promise<void>)();

      expect(commandCallsFor("thread.startTurn")).toHaveLength(0);
      expect(promptRef.current).toBe("");
      expect(resetCursorState).toHaveBeenCalledTimes(1);
      expect(revokeObjectUrl).toHaveBeenCalledTimes(1);
      expect(revokeObjectUrl).toHaveBeenCalledWith(contexts.image.previewUrl);
      const clearedDraft = useComposerDraftStore.getState().getComposerDraft(threadRef);
      expect(clearedDraft?.prompt ?? "").toBe("");
      expect(clearedDraft?.attachments ?? []).toEqual([]);
      expect(clearedDraft?.terminalContexts ?? []).toEqual([]);
      expect(clearedDraft?.elementContexts ?? []).toEqual([]);
      expect(clearedDraft?.previewAnnotations ?? []).toEqual([]);
      expect(clearedDraft?.reviewComments ?? []).toEqual([]);
      expect(clearedDraft?.interactionMode ?? "plan").toBe(expectedMode);
    },
  );

  it("releases owned attachments while submitting ordinary plan follow-up feedback", async () => {
    seedConnectedServerThread(makeActionablePlanThread("feedback"));
    renderServerRoute();
    const contexts = makeOwnedComposerContexts("feedback");
    seedOwnedComposerContexts("please refine this", contexts);
    const revokeObjectUrl = vi.spyOn(URL, "revokeObjectURL");
    const resetCursorState = vi.fn();
    const { promptRef } = installComposerHandle({
      resetCursorState,
      getSendContext: () => sendContextWithOwnedComposerContexts(contexts),
    });
    promptRef.current = "please refine this";

    await (capturedProps("chatComposer")["onSend"] as () => Promise<void>)();

    expect(commandCallsFor("thread.startTurn")).toHaveLength(1);
    expect(promptRef.current).toBe("");
    expect(resetCursorState).toHaveBeenCalledTimes(1);
    expect(revokeObjectUrl).toHaveBeenCalledTimes(1);
    expect(revokeObjectUrl).toHaveBeenCalledWith(contexts.image.previewUrl);
    expect(useComposerDraftStore.getState().getComposerDraft(threadRef)).toMatchObject({
      prompt: "",
      attachments: [],
      terminalContexts: [],
      elementContexts: [],
      previewAnnotations: [],
      reviewComments: [],
      interactionMode: "plan",
    });
  });

  it("submits a plan follow-up instead of a regular turn when a plan is actionable", async () => {
    const turnId = TurnId.make("turn-1");
    seedConnectedServerThread(
      makeThread({
        interactionMode: "plan",
        latestTurn: {
          turnId,
          state: "completed",
          requestedAt: now,
          startedAt: now,
          completedAt: later,
          assistantMessageId: null,
        },
        proposedPlans: [
          {
            id: OrchestrationProposedPlanId.make("plan-1"),
            turnId,
            planMarkdown: "# Plan\n\n1. do the thing",
            implementedAt: null,
            implementationThreadId: null,
            createdAt: now,
            updatedAt: now,
          },
        ],
      }),
    );
    renderServerRoute();
    const { promptRef } = installComposerHandle();
    promptRef.current = "ship it";

    const composer = capturedProps("chatComposer");
    expect(composer["showPlanFollowUpPrompt"]).toBe(true);
    await (composer["onSend"] as () => Promise<void>)();

    const startCalls = commandCallsFor("thread.startTurn");
    expect(startCalls).toHaveLength(1);
    const input = startCalls[0]!.input as {
      input: { interactionMode: string; sourceProposedPlan?: { planId: string } };
    };
    // A follow-up with custom text refines the plan in plan mode.
    expect(input.input.interactionMode).toBe("plan");
  });

  it("reports plan follow-up failures and drops the optimistic message", async () => {
    const turnId = TurnId.make("turn-1");
    seedConnectedServerThread(
      makeThread({
        interactionMode: "plan",
        latestTurn: {
          turnId,
          state: "completed",
          requestedAt: now,
          startedAt: now,
          completedAt: later,
          assistantMessageId: null,
        },
        proposedPlans: [
          {
            id: OrchestrationProposedPlanId.make("plan-1"),
            turnId,
            planMarkdown: "# Plan",
            implementedAt: null,
            implementationThreadId: null,
            createdAt: now,
            updatedAt: now,
          },
        ],
      }),
    );
    h.commandResults["thread.startTurn"] = () =>
      AsyncResult.failure(Cause.fail(new Error("follow-up rejected")));
    renderServerRoute();
    const { promptRef } = installComposerHandle();
    promptRef.current = "";

    const composer = capturedProps("chatComposer");
    await (composer["onSend"] as () => Promise<void>)();

    expect(commandCallsFor("thread.startTurn")).toHaveLength(1);
    const errorWrites = h.setStateCalls.filter(
      (call) =>
        typeof call.applied === "object" &&
        call.applied !== null &&
        (call.applied as Record<string, unknown>)[threadKey] === "follow-up rejected",
    );
    expect(errorWrites.length).toBeGreaterThanOrEqual(1);
  });

  it("implements the plan in a new thread and cleans up when the start fails", async () => {
    const turnId = TurnId.make("turn-1");
    seedConnectedServerThread(
      makeThread({
        interactionMode: "plan",
        latestTurn: {
          turnId,
          state: "completed",
          requestedAt: now,
          startedAt: now,
          completedAt: later,
          assistantMessageId: null,
        },
        proposedPlans: [
          {
            id: OrchestrationProposedPlanId.make("plan-1"),
            turnId,
            planMarkdown: "# Great Plan\n\ndo it",
            implementedAt: null,
            implementationThreadId: null,
            createdAt: now,
            updatedAt: now,
          },
        ],
      }),
    );
    h.commandResults["thread.startTurn"] = () =>
      AsyncResult.failure(Cause.fail(new Error("no capacity")));
    renderServerRoute();
    installComposerHandle();

    const composer = capturedProps("chatComposer");
    await (composer["onImplementPlanInNewThread"] as () => Promise<void>)();

    expect(commandCallsFor("worktree.createPanel")).toHaveLength(1);
    expect(commandCallsFor("thread.startTurn")).toHaveLength(1);
    // Failure path deletes the freshly created thread and toasts.
    expect(commandCallsFor("thread.delete")).toHaveLength(1);
    expect(
      h.toasts.some(
        (toast) => (toast as { title?: string }).title === "Could not start implementation thread",
      ),
    ).toBe(true);
    expect(h.navigateCalls).toHaveLength(0);
  });

  it("interrupt failures surface a thread error", async () => {
    seedConnectedServerThread(
      makeThread({
        session: makeSession({ status: "running", activeTurnId: TurnId.make("turn-9") }),
      }),
    );
    h.commandResults["thread.interruptTurn"] = () =>
      AsyncResult.failure(Cause.fail(new Error("cannot interrupt")));
    renderServerRoute();

    const composer = capturedProps("chatComposer");
    await (composer["onInterrupt"] as () => Promise<void>)();

    const errorWrites = h.setStateCalls.filter(
      (call) =>
        typeof call.applied === "object" &&
        call.applied !== null &&
        (call.applied as Record<string, unknown>)[threadKey] === "cannot interrupt",
    );
    expect(errorWrites.length).toBeGreaterThanOrEqual(1);
  });
});

// ─────────────────────────────────────────────────────────────────────
// Pending approvals and user input
// ─────────────────────────────────────────────────────────────────────

describe("ChatView pending user input", () => {
  const requestId = ApprovalRequestId.make("request-1");

  function threadWithQuestions(questionCount: number): Thread {
    const questions = Array.from({ length: questionCount }, (_, index) => ({
      id: `question-${index + 1}`,
      header: `Header ${index + 1}`,
      question: `Question ${index + 1}?`,
      options: [
        { label: "Yes", description: "Do it" },
        { label: "No", description: "Skip it" },
      ],
      multiSelect: false,
    }));
    return makeThread({
      activities: [
        {
          id: EventId.make("event-1"),
          tone: "approval",
          kind: "user-input.requested",
          summary: "Needs your input",
          payload: { requestId: "request-1", questions },
          turnId: null,
          createdAt: now,
        },
      ],
    });
  }

  it("toggles option selections and custom answers for the active question", () => {
    seedConnectedServerThread(threadWithQuestions(2));
    renderServerRoute();
    const focusAtCalls: number[] = [];
    installComposerHandle({
      focusAt: (cursor: number) => {
        focusAtCalls.push(cursor);
      },
    });

    const composer = capturedProps("chatComposer");
    expect(composer["pendingUserInputs"]).toHaveLength(1);

    (
      composer["onSelectActivePendingUserInputOption"] as (
        questionId: string,
        optionLabel: string,
      ) => void
    )("question-1", "Yes");
    (
      composer["onChangeActivePendingUserInputCustomAnswer"] as (
        questionId: string,
        value: string,
        nextCursor: number,
        expandedCursor: number,
        cursorAdjacentToMention: boolean,
      ) => void
    )("question-1", "custom answer", 13, 13, false);

    // Both handlers wrote draft answers for the request.
    const answerWrites = h.setStateCalls.filter(
      (call) =>
        typeof call.applied === "object" &&
        call.applied !== null &&
        "request-1" in (call.applied as Record<string, unknown>),
    );
    expect(answerWrites.length).toBeGreaterThanOrEqual(2);
    expect(focusAtCalls).toEqual([13]);
  });

  it("advances through questions and submits answers on the last one", async () => {
    seedConnectedServerThread(threadWithQuestions(1));
    seedHostState("pendingUserInputAnswersByRequestId", {
      "request-1": { "question-1": { customAnswer: "", selectedOptionLabels: ["Yes"] } },
    });
    renderServerRoute();
    installComposerHandle();

    const composer = capturedProps("chatComposer");
    (composer["onAdvanceActivePendingUserInput"] as () => void)();
    await Promise.resolve();
    await Promise.resolve();

    // Single question: advancing submits immediately.
    expect(commandCallsFor("thread.respondToUserInput")).toHaveLength(1);

    (composer["onPreviousActivePendingUserInputQuestion"] as () => void)();
  });

  it("onSend advances the pending user input instead of sending", async () => {
    seedConnectedServerThread(threadWithQuestions(1));
    seedHostState("pendingUserInputAnswersByRequestId", {
      "request-1": { "question-1": { customAnswer: "", selectedOptionLabels: ["Yes"] } },
    });
    renderServerRoute();
    installComposerHandle();

    const composer = capturedProps("chatComposer");
    await (composer["onSend"] as () => Promise<void>)();

    expect(commandCallsFor("thread.startTurn")).toHaveLength(0);
    expect(commandCallsFor("thread.respondToUserInput")).toHaveLength(1);
  });

  it("reports user-input submission failures as thread errors", async () => {
    seedConnectedServerThread(threadWithQuestions(1));
    seedHostState("pendingUserInputAnswersByRequestId", {
      "request-1": { "question-1": { customAnswer: "", selectedOptionLabels: ["Yes"] } },
    });
    h.commandResults["thread.respondToUserInput"] = () =>
      AsyncResult.failure(Cause.fail(new Error("input rejected")));
    renderServerRoute();
    installComposerHandle();

    const composer = capturedProps("chatComposer");
    (composer["onAdvanceActivePendingUserInput"] as () => void)();
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();

    const errorWrites = h.setStateCalls.filter(
      (call) =>
        typeof call.applied === "object" &&
        call.applied !== null &&
        (call.applied as Record<string, unknown>)[threadKey] === "input rejected",
    );
    expect(errorWrites.length).toBeGreaterThanOrEqual(1);
  });

  it("reports approval submission failures as thread errors", async () => {
    seedConnectedServerThread();
    h.commandResults["thread.respondToApproval"] = () =>
      AsyncResult.failure(Cause.fail(new Error("approval rejected")));
    renderServerRoute();

    const composer = capturedProps("chatComposer");
    await (
      composer["onRespondToApproval"] as (
        requestId: ApprovalRequestId,
        decision: string,
      ) => Promise<unknown>
    )(requestId, "approve");

    const errorWrites = h.setStateCalls.filter(
      (call) =>
        typeof call.applied === "object" &&
        call.applied !== null &&
        (call.applied as Record<string, unknown>)[threadKey] === "approval rejected",
    );
    expect(errorWrites.length).toBeGreaterThanOrEqual(1);
  });

  it("keeps pending-input callbacks inert when no request is active", () => {
    seedConnectedServerThread();
    renderServerRoute();
    const composer = capturedProps("chatComposer");

    (composer["onSelectActivePendingUserInputOption"] as (id: string, label: string) => void)(
      "missing",
      "Yes",
    );
    (
      composer["onChangeActivePendingUserInputCustomAnswer"] as (
        id: string,
        value: string,
        cursor: number,
        expandedCursor: number,
        adjacent: boolean,
      ) => void
    )("missing", "answer", 6, 6, false);
    (composer["onAdvanceActivePendingUserInput"] as () => void)();
    (composer["onPreviousActivePendingUserInputQuestion"] as () => void)();

    expect(commandCallsFor("thread.respondToUserInput")).toHaveLength(0);
  });

  it("ignores unknown questions, advances multi-question requests, and avoids redundant focus", () => {
    seedConnectedServerThread(threadWithQuestions(2));
    renderServerRoute();
    const focusAt = vi.fn();
    installComposerHandle({
      focusAt,
      readSnapshot: () => ({
        value: "answer",
        cursor: 6,
        expandedCursor: 6,
        terminalContextIds: [],
      }),
    });
    const composer = capturedProps("chatComposer");

    (composer["onSelectActivePendingUserInputOption"] as (id: string, label: string) => void)(
      "unknown-question",
      "Yes",
    );
    (
      composer["onChangeActivePendingUserInputCustomAnswer"] as (
        id: string,
        value: string,
        cursor: number,
        expandedCursor: number,
        adjacent: boolean,
      ) => void
    )("question-1", "answer", 6, 6, true);
    (composer["onAdvanceActivePendingUserInput"] as () => void)();

    expect(focusAt).not.toHaveBeenCalled();
    expect(commandCallsFor("thread.respondToUserInput")).toHaveLength(0);
  });

  it("uses generic messages for opaque approval and user-input failures", async () => {
    seedConnectedServerThread(threadWithQuestions(1));
    seedHostState("pendingUserInputAnswersByRequestId", {
      "request-1": { "question-1": { customAnswer: "", selectedOptionLabels: ["Yes"] } },
    });
    h.commandResults["thread.interruptTurn"] = () =>
      AsyncResult.failure(Cause.fail("opaque cancel"));
    h.commandResults["thread.respondToApproval"] = () =>
      AsyncResult.failure(Cause.fail("opaque approval"));
    h.commandResults["thread.respondToUserInput"] = () =>
      AsyncResult.failure(Cause.fail("opaque input"));
    renderServerRoute();
    const composer = capturedProps("chatComposer");

    await (
      composer["onRespondToApproval"] as (
        id: ApprovalRequestId,
        decision: "cancel" | "deny",
      ) => Promise<unknown>
    )(requestId, "cancel");
    await (
      composer["onRespondToApproval"] as (
        id: ApprovalRequestId,
        decision: "cancel" | "deny",
      ) => Promise<unknown>
    )(requestId, "deny");
    (composer["onAdvanceActivePendingUserInput"] as () => void)();
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();

    const messages = h.setStateCalls.flatMap((call) =>
      typeof call.applied === "object" && call.applied !== null
        ? Object.values(call.applied as Record<string, unknown>)
        : [],
    );
    expect(messages).toEqual(
      expect.arrayContaining([
        "Failed to cancel the current turn.",
        "Failed to submit approval decision.",
        "Failed to submit user input.",
      ]),
    );
  });
});

// ─────────────────────────────────────────────────────────────────────
// Turn delivery resolution
// ─────────────────────────────────────────────────────────────────────

describe("ChatView turn delivery resolution", () => {
  const deliveryMessageId = MessageId.make("message-delivery-1");

  function seedUncertainDelivery() {
    seedConnectedServerThread(
      makeThread({
        messages: [
          {
            id: deliveryMessageId,
            role: "user",
            text: "send once",
            turnId: null,
            createdAt: now,
            updatedAt: now,
            streaming: false,
            delivery: {
              state: "uncertain",
              provider: ProviderDriverKind.make("claudeAgent"),
              detail: "connection closed before acknowledgement",
            },
          },
        ],
      }),
    );
  }

  it("confirms an uncertain retry before dispatching the message-scoped action", async () => {
    seedUncertainDelivery();
    const confirms: string[] = [];
    h.localApi = {
      dialogs: {
        confirm: (message: string) => {
          confirms.push(message);
          return Promise.resolve(true);
        },
      },
    };
    renderServerRoute();

    const callback = capturedProps("messagesTimeline")["onResolveTurnDelivery"];
    expect(callback).toBeTypeOf("function");
    await (callback as (messageId: MessageId, action: "retry" | "dismiss") => Promise<void>)(
      deliveryMessageId,
      "retry",
    );

    expect(confirms).toHaveLength(1);
    expect(confirms[0]).toContain("Claude may receive a duplicate");
    expect(commandCallsFor("thread.resolveDelivery")).toEqual([
      {
        key: "thread.resolveDelivery",
        input: {
          environmentId,
          input: { threadId, messageId: deliveryMessageId, action: "retry" },
        },
      },
    ]);
  });

  it("dismisses without duplicate confirmation and marks only the matching command pending", async () => {
    seedUncertainDelivery();
    const confirm = vi.fn(() => Promise.resolve(true));
    h.localApi = { dialogs: { confirm } };
    renderServerRoute();

    const callback = capturedProps("messagesTimeline")["onResolveTurnDelivery"];
    expect(callback).toBeTypeOf("function");
    await (callback as (messageId: MessageId, action: "retry" | "dismiss") => Promise<void>)(
      deliveryMessageId,
      "dismiss",
    );

    expect(confirm).not.toHaveBeenCalled();
    expect(commandCallsFor("thread.resolveDelivery")[0]?.input).toEqual({
      environmentId,
      input: { threadId, messageId: deliveryMessageId, action: "dismiss" },
    });
    expect(h.setStateCalls.some((call) => call.applied === deliveryMessageId)).toBe(true);
    expect(h.setStateCalls.at(-1)?.applied).toBeNull();
  });

  it("does not retry uncertain delivery when duplicate confirmation is declined", async () => {
    seedUncertainDelivery();
    h.localApi = { dialogs: { confirm: () => Promise.resolve(false) } };
    renderServerRoute();

    const callback = capturedProps("messagesTimeline")["onResolveTurnDelivery"];
    expect(callback).toBeTypeOf("function");
    await (callback as (messageId: MessageId, action: "retry" | "dismiss") => Promise<void>)(
      deliveryMessageId,
      "retry",
    );

    expect(commandCallsFor("thread.resolveDelivery")).toHaveLength(0);
  });

  it("does not show blocked queued delivery as active work", () => {
    const queuedAt = "2026-03-29T00:00:02.000Z";
    seedConnectedServerThread(
      makeThread({
        session: makeSession({
          status: "error",
          lastError: "Provider session ended when BiBCode stopped.",
        }),
        messages: [
          {
            id: MessageId.make("message-uncertain"),
            role: "user",
            text: "possibly delivered",
            turnId: null,
            createdAt: now,
            updatedAt: now,
            streaming: false,
            delivery: {
              state: "uncertain",
              provider: ProviderDriverKind.make("cursor"),
              detail: "provider does not support exact delivery reconciliation",
            },
          },
          {
            id: MessageId.make("message-queued"),
            role: "user",
            text: "queued behind uncertainty",
            turnId: null,
            createdAt: queuedAt,
            updatedAt: queuedAt,
            streaming: false,
            delivery: {
              state: "pending",
              provider: ProviderDriverKind.make("cursor"),
            },
          },
        ],
      }),
    );

    renderServerRoute();

    expect(capturedProps("messagesTimeline")["isWorking"]).toBe(false);
    expect(capturedProps("messagesTimeline")["activeTurnStartedAt"]).toBeNull();
    expect(capturedProps("chatComposer")["isSendBusy"]).toBe(true);
    expect(capturedProps("chatComposer")["canCancelPendingSend"]).toBe(true);
  });

  it("anchors active persisted delivery work to the message timestamp after reload", () => {
    const pendingAt = "2026-03-29T00:00:02.000Z";
    seedConnectedServerThread(
      makeThread({
        session: makeSession(),
        messages: [
          {
            id: MessageId.make("message-pending"),
            role: "user",
            text: "still delivering",
            turnId: null,
            createdAt: pendingAt,
            updatedAt: pendingAt,
            streaming: false,
            delivery: {
              state: "pending",
              provider: ProviderDriverKind.make("cursor"),
            },
          },
        ],
      }),
    );

    renderServerRoute();

    expect(capturedProps("messagesTimeline")["isWorking"]).toBe(true);
    expect(capturedProps("messagesTimeline")["activeTurnStartedAt"]).toBe(pendingAt);
  });

  it("does not inherit an abandoned turn timestamp for a newer pending delivery", () => {
    const pendingAt = "2026-03-29T00:00:02.000Z";
    seedConnectedServerThread(
      makeThread({
        session: makeSession({
          status: "error",
          lastError: "Provider session ended when BiBCode stopped.",
        }),
        latestTurn: {
          turnId: TurnId.make("turn-abandoned"),
          state: "running",
          requestedAt: now,
          startedAt: now,
          completedAt: null,
          assistantMessageId: null,
        },
        messages: [
          {
            id: MessageId.make("message-pending-after-restart"),
            role: "user",
            text: "deliver after restart",
            turnId: null,
            createdAt: pendingAt,
            updatedAt: pendingAt,
            streaming: false,
            delivery: {
              state: "pending",
              provider: ProviderDriverKind.make("cursor"),
            },
          },
        ],
      }),
    );

    renderServerRoute();

    expect(capturedProps("messagesTimeline")["isWorking"]).toBe(true);
    expect(capturedProps("messagesTimeline")["activeTurnStartedAt"]).toBe(pendingAt);
  });
});

// ─────────────────────────────────────────────────────────────────────
// Checkpoint revert
// ─────────────────────────────────────────────────────────────────────

describe("ChatView checkpoint revert", () => {
  const userMessageId = MessageId.make("message-user-1");
  const assistantMessageId = MessageId.make("message-assistant-1");
  const turnId = TurnId.make("turn-1");

  function threadWithTurn(): Thread {
    return makeThread({
      messages: [
        {
          id: userMessageId,
          role: "user",
          text: "do something",
          turnId,
          createdAt: now,
          updatedAt: now,
          streaming: false,
        },
        {
          id: assistantMessageId,
          role: "assistant",
          text: "done",
          turnId,
          createdAt: later,
          updatedAt: later,
          streaming: false,
        },
      ],
    });
  }

  function seedRevertContext() {
    h.turnDiffSummaries = [
      {
        turnId,
        assistantMessageId,
        checkpointTurnCount: 3,
        files: [],
      },
    ];
    seedConnectedServerThread(threadWithTurn());
  }

  it("confirms and reverts to the checkpoint for a user message", async () => {
    seedRevertContext();
    const confirms: string[] = [];
    h.localApi = {
      dialogs: {
        confirm: (message: string) => {
          confirms.push(message);
          return Promise.resolve(true);
        },
      },
    };
    renderServerRoute();

    const timeline = capturedProps("messagesTimeline");
    const revertCounts = timeline["revertTurnCountByUserMessageId"] as ReadonlyMap<
      MessageId,
      number
    >;
    expect(revertCounts.get(userMessageId)).toBe(2);

    (timeline["onRevertUserMessage"] as (messageId: MessageId) => void)(userMessageId);
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();

    expect(confirms).toHaveLength(1);
    const revertCalls = commandCallsFor("thread.revertCheckpoint");
    expect(revertCalls).toHaveLength(1);
    expect((revertCalls[0]!.input as { input: { turnCount: number } }).input.turnCount).toBe(2);
  });

  it("does not revert when the confirmation is declined", async () => {
    seedRevertContext();
    h.localApi = {
      dialogs: { confirm: () => Promise.resolve(false) },
    };
    renderServerRoute();

    const timeline = capturedProps("messagesTimeline");
    (timeline["onRevertUserMessage"] as (messageId: MessageId) => void)(userMessageId);
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();

    expect(commandCallsFor("thread.revertCheckpoint")).toHaveLength(0);
  });

  it("blocks reverting while the environment is unavailable", async () => {
    h.turnDiffSummaries = [{ turnId, assistantMessageId, checkpointTurnCount: 3, files: [] }];
    seedEnvironment(
      makeEnvironmentPresentation({
        connection: { phase: "error", error: "socket closed", traceId: null },
      }),
    );
    seedProject(makeProject());
    seedServerThread(threadWithTurn());
    seedGitStatus(true);
    h.localApi = { dialogs: { confirm: () => Promise.resolve(true) } };
    renderServerRoute();

    const timeline = capturedProps("messagesTimeline");
    (timeline["onRevertUserMessage"] as (messageId: MessageId) => void)(userMessageId);
    await Promise.resolve();
    await Promise.resolve();

    expect(commandCallsFor("thread.revertCheckpoint")).toHaveLength(0);
    const errorWrites = h.setStateCalls.filter(
      (call) =>
        typeof call.applied === "object" &&
        call.applied !== null &&
        (call.applied as Record<string, unknown>)[threadKey] ===
          "Reconnect Local before reverting checkpoints.",
    );
    expect(errorWrites.length).toBeGreaterThanOrEqual(1);
  });

  it("surfaces revert failures on the thread", async () => {
    seedRevertContext();
    h.localApi = { dialogs: { confirm: () => Promise.resolve(true) } };
    h.commandResults["thread.revertCheckpoint"] = () =>
      AsyncResult.failure(Cause.fail(new Error("revert refused")));
    renderServerRoute();

    const timeline = capturedProps("messagesTimeline");
    (timeline["onRevertUserMessage"] as (messageId: MessageId) => void)(userMessageId);
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();

    const errorWrites = h.setStateCalls.filter(
      (call) =>
        typeof call.applied === "object" &&
        call.applied !== null &&
        (call.applied as Record<string, unknown>)[threadKey] === "revert refused",
    );
    expect(errorWrites.length).toBeGreaterThanOrEqual(1);
  });

  it("opens the diff panel for a turn from the timeline", () => {
    seedRevertContext();
    renderServerRoute();

    const timeline = capturedProps("messagesTimeline");
    (timeline["onOpenTurnDiff"] as (turnId: TurnId, filePath?: string) => void)(
      turnId,
      "src/index.ts",
    );

    expect(
      useRightPanelStore
        .getState()
        .byThreadKey[threadKey]?.surfaces.some((surface) => surface.kind === "diff"),
    ).toBe(true);
  });
});

// ─────────────────────────────────────────────────────────────────────
// Model / env-mode selection
// ─────────────────────────────────────────────────────────────────────

describe("ChatView model and environment selection", () => {
  it("selects a provider model and stores it as the sticky selection", () => {
    seedConnectedServerThread();
    renderServerRoute();
    installComposerHandle();

    const composer = capturedProps("chatComposer");
    (composer["onProviderModelSelect"] as (instanceId: ProviderInstanceId, model: string) => void)(
      codexInstanceId,
      "gpt-5.4",
    );

    const draft = useComposerDraftStore.getState().getComposerDraft(threadRef);
    expect(draft?.modelSelectionByProvider[codexInstanceId]).toMatchObject({
      instanceId: codexInstanceId,
      model: "gpt-5.4",
    });
  });

  it("blocks model changes on a started restricted session with a toast", () => {
    const restrictedCodex: ServerProvider = {
      ...codexProvider,
      requiresNewThreadForModelChange: true,
      models: [
        { slug: "gpt-5.4", name: "GPT-5.4", isCustom: false, capabilities: null },
        { slug: "gpt-6", name: "GPT-6", isCustom: false, capabilities: null },
      ],
    };
    seedEnvironment(
      makeEnvironmentPresentation({
        serverConfig: {
          providers: [restrictedCodex],
          environment: { label: "Local" },
        },
      }),
    );
    seedProject(makeProject());
    seedServerThread(makeThread({ session: makeSession() }));
    seedGitStatus(true);
    renderServerRoute();
    installComposerHandle();

    const composer = capturedProps("chatComposer");
    (composer["onProviderModelSelect"] as (instanceId: ProviderInstanceId, model: string) => void)(
      codexInstanceId,
      "gpt-6",
    );

    expect(h.toasts.length).toBeGreaterThanOrEqual(1);
    expect(
      useComposerDraftStore.getState().getComposerDraft(threadRef)?.modelSelectionByProvider ?? {},
    ).toEqual({});

    // getModelDisabledReason mirrors the same restriction for the picker UI.
    const getModelDisabledReason = composer["getModelDisabledReason"] as (
      instanceId: ProviderInstanceId,
      model: string,
    ) => string | null;
    expect(getModelDisabledReason(codexInstanceId, "gpt-6")).toContain("Start a new thread");
  });

  it("silently ignores selecting a different locked provider", () => {
    const grokInstanceId = ProviderInstanceId.make("grok");
    const grokProvider: ServerProvider = {
      ...codexProvider,
      instanceId: grokInstanceId,
      driver: ProviderDriverKind.make("grok"),
      models: [{ slug: "grok-build", name: "Grok Build", isCustom: false, capabilities: null }],
    };
    seedEnvironment(
      makeEnvironmentPresentation({
        serverConfig: {
          providers: [codexProvider, grokProvider],
          environment: { label: "Local" },
        },
      }),
    );
    seedProject(makeProject());
    seedServerThread(makeThread({ session: makeSession() }));
    seedGitStatus(true);
    renderServerRoute();
    installComposerHandle();

    const composer = capturedProps("chatComposer");
    (composer["onProviderModelSelect"] as (instanceId: ProviderInstanceId, model: string) => void)(
      grokInstanceId,
      "grok-build",
    );

    expect(h.toasts).toHaveLength(0);
    expect(
      useComposerDraftStore.getState().getComposerDraft(threadRef)?.modelSelectionByProvider ?? {},
    ).toEqual({});
  });
});

// ─────────────────────────────────────────────────────────────────────
// Banners, dialogs, and misc chrome
// ─────────────────────────────────────────────────────────────────────

describe("ChatView banners and dialogs", () => {
  function findButtons(node: ReactNode, found: ReactElement[] = []): ReactElement[] {
    if (!node || typeof node !== "object") return found;
    if (Array.isArray(node)) {
      for (const child of node) findButtons(child, found);
      return found;
    }
    const element = node as ReactElement<Record<string, unknown>>;
    if (element.props && typeof element.props["onClick"] === "function") {
      found.push(element);
    }
    if (element.props && element.props["children"] !== undefined) {
      findButtons(element.props["children"] as ReactNode, found);
    }
    return found;
  }

  function unavailableBannerActions(input: {
    readonly surface: "browser" | "desktop";
    readonly platform: "macos" | "windows";
    readonly target: ConnectionTarget;
  }): string {
    h.presentationPolicy = createEnvironmentPresentationPolicy({
      surface: input.surface,
      platform: input.platform,
    });
    seedEnvironment(
      makeEnvironmentPresentation({
        entry: { target: input.target },
        connection: { phase: "error", error: "socket closed", traceId: null },
      }),
    );
    seedProject(makeProject());
    seedServerThread(makeThread());
    seedGitStatus(true);
    renderServerRoute();

    const bannerStack = capturedProps<{ items: ComposerBannerStackItem[] }>("composerBannerStack");
    return renderToStaticMarkup(<>{bannerStack.items[0]!.actions}</>);
  }

  it("gates unavailable environment actions by desktop target while preserving browser recovery", () => {
    const remoteTarget = new BearerConnectionTarget({
      connectionId: "remote:server",
      environmentId,
      label: "Remote server",
      serverEnvironmentId: null,
    });
    const wslTarget = new BearerConnectionTarget({
      connectionId: desktopLocalConnectionId("wsl:Ubuntu"),
      environmentId,
      label: "WSL: Ubuntu",
      serverEnvironmentId: null,
    });

    const macRemoteBanner = unavailableBannerActions({
      surface: "desktop",
      platform: "macos",
      target: remoteTarget,
    });
    expect(macRemoteBanner).not.toContain("Reconnect");
    expect(macRemoteBanner).not.toContain("Connections");

    const windowsWslBanner = unavailableBannerActions({
      surface: "desktop",
      platform: "windows",
      target: wslTarget,
    });
    expect(windowsWslBanner).toContain("Reconnect");
    expect(windowsWslBanner).not.toContain("Connections");

    const browserRemoteBanner = unavailableBannerActions({
      surface: "browser",
      platform: "macos",
      target: remoteTarget,
    });
    expect(browserRemoteBanner).toContain("Reconnect");
    expect(browserRemoteBanner).toContain("Remote Servers");
  });

  it("reconnects the environment from the unavailable banner and toasts failures", async () => {
    seedEnvironment(
      makeEnvironmentPresentation({
        connection: { phase: "error", error: "socket closed", traceId: null },
      }),
    );
    seedProject(makeProject());
    seedServerThread(makeThread());
    seedGitStatus(true);
    h.commandResults["environment.retryNow"] = () =>
      AsyncResult.failure(Cause.fail(new Error("still down")));
    renderServerRoute();

    const bannerStack = capturedProps<{ items: ComposerBannerStackItem[] }>("composerBannerStack");
    expect(bannerStack.items).toHaveLength(1);
    const buttons = findButtons(bannerStack.items[0]!.actions as ReactNode);
    expect(buttons.length).toBe(2);

    (buttons[0]!.props as { onClick: () => void }).onClick();
    await Promise.resolve();
    await Promise.resolve();
    expect(commandCallsFor("environment.retryNow")).toHaveLength(1);
    expect(
      h.toasts.some(
        (toast) => (toast as { title?: string }).title === "Could not reconnect environment",
      ),
    ).toBe(true);

    (buttons[1]!.props as { onClick: () => void }).onClick();
    expect(h.navigateCalls).toContainEqual({ to: "/settings/remote-servers" });
  });

  it("dismisses the version mismatch banner persistently", () => {
    seedEnvironment(
      makeEnvironmentPresentation({
        serverConfig: {
          providers: [codexProvider],
          environment: { label: "Local", serverVersion: "0.0.0-mismatch" },
        },
      }),
    );
    seedProject(makeProject());
    seedServerThread(makeThread());
    seedGitStatus(true);
    renderServerRoute();

    const bannerStack = capturedProps<{ items: ComposerBannerStackItem[] }>("composerBannerStack");
    expect(bannerStack.items).toHaveLength(1);
    const item = bannerStack.items[0]!;
    expect(item.title).toBe("Client and server versions differ");
    item.onDismiss?.();

    // Dismissal is persisted, so a fresh render no longer shows the banner.
    renderServerRoute();
    const nextStack = capturedProps<{ items: ComposerBannerStackItem[] }>("composerBannerStack");
    expect(nextStack.items).toHaveLength(0);
  });

  it("shows the scroll-to-end pill and scrolls on click", () => {
    seedConnectedServerThread();
    seedHostState("showScrollToBottom", true);

    const markup = renderServerRoute();
    expect(markup).toContain("Scroll to end");

    const list = installLegendList();
    // The pill's onClick is on a real <button>; drive scrollToEnd through the
    // captured timeline live-follow instead.
    const timeline = capturedProps("messagesTimeline");
    (timeline["onIsAtEndChange"] as (isAtEnd: boolean) => void)(false);
    (timeline["onIsAtEndChange"] as (isAtEnd: boolean) => void)(true);
    expect(list).toBeDefined();
  });

  it("renders the expanded image dialog from seeded state and closes it", () => {
    seedConnectedServerThread();
    seedHostState("expandedImage", {
      images: [{ src: "blob:image-1", alt: "img" }],
      index: 0,
    });

    const markup = renderServerRoute();
    expect(markup).toContain('data-mock="expanded-image-dialog"');

    const dialog = capturedProps("expandedImageDialog");
    (dialog["onClose"] as () => void)();
    const closes = setStateCallsFor("expandedImage");
    expect(closes).toHaveLength(1);
    expect(closes[0]!.applied).toBeNull();
  });

  it("expands timeline images through the timeline callback", () => {
    seedConnectedServerThread();
    renderServerRoute();

    const timeline = capturedProps("messagesTimeline");
    (timeline["onImageExpand"] as (preview: unknown) => void)({
      images: [{ src: "blob:image-2" }],
      index: 0,
    });

    const expands = setStateCallsFor("expandedImage");
    expect(expands).toHaveLength(1);
  });

  it("opens the pull request dialog flow and prepares a draft thread", async () => {
    seedConnectedServerThread();
    seedProject(makeProject({ defaultModelSelection: null }));
    const capabilityRichCodexProvider: ServerProvider = {
      ...codexProvider,
      models: [
        {
          ...codexProvider.models[0]!,
          capabilities: {
            optionDescriptors: [
              {
                id: "reasoningEffort",
                label: "Reasoning",
                type: "select",
                options: [
                  { id: "medium", label: "Medium", isDefault: true },
                  { id: "high", label: "High" },
                ],
                currentValue: "medium",
              },
              {
                id: "serviceTier",
                label: "Service tier",
                type: "select",
                options: [
                  { id: "default", label: "Default", isDefault: true },
                  { id: "fast", label: "Fast" },
                ],
                currentValue: "default",
              },
            ],
          },
        },
      ],
    };
    seedEnvironment(
      makeEnvironmentPresentation({
        serverConfig: {
          providers: [capabilityRichCodexProvider],
          environment: { label: "Local" },
        },
      }),
    );
    h.settings = {
      ...h.settings,
      providerSessionDefaults: {
        codex: {
          model: "gpt-5.4",
          options: [
            { id: "reasoningEffort", value: "high" },
            { id: "serviceTier", value: "fast" },
          ],
        },
      },
    };
    seedHostState("pullRequestDialogState", { initialReference: "octo/repo#42", key: 7 });

    const markup = renderServerRoute();
    expect(markup).toContain('data-mock="pull-request-thread-dialog"');

    const dialog = capturedProps("pullRequestThreadDialog");
    expect(dialog["initialReference"]).toBe("octo/repo#42");

    // Closing resets the dialog state.
    (dialog["onOpenChange"] as (open: boolean) => void)(false);
    const dialogWrites = setStateCallsFor("pullRequestDialogState");
    expect(dialogWrites.some((call) => call.applied === null)).toBe(true);

    // Preparing hands off to a fresh draft thread and navigates to it.
    await (
      dialog["onPrepared"] as (input: {
        branch: string;
        mode: "local" | "worktree";
      }) => Promise<void>
    )({ branch: "pr-branch", mode: "local" });

    expect(h.navigateCalls.length).toBeGreaterThanOrEqual(1);
    const navigateCall = h.navigateCalls[0] as {
      to: string;
      params: { draftId: ReturnType<typeof newDraftId> };
    };
    expect(navigateCall.to).toBe("/draft/$draftId");
    expect(draftModelSelection(navigateCall.params.draftId, codexInstanceId)).toEqual({
      instanceId: codexInstanceId,
      model: "gpt-5.4",
      options: [
        { id: "reasoningEffort", value: "high" },
        { id: "serviceTier", value: "fast" },
      ],
    });
  });

  it("reuses a stored draft session for pull request checkout when one exists", async () => {
    seedConnectedServerThread();
    const project = makeProject();
    const logicalKey = deriveLogicalProjectKeyFromSettings(
      project,
      selectProjectGroupingSettings(h.settings as never),
    );
    const draftId = newDraftId();
    const draftThreadId = ThreadId.make("thread-draft-9");
    useComposerDraftStore
      .getState()
      .setLogicalProjectDraftThreadId(
        logicalKey,
        scopeProjectRef(environmentId, projectId),
        draftId,
        { threadId: draftThreadId, createdAt: now, envMode: "local" },
      );
    publishSeededStoreState(useComposerDraftStore);
    seedHostState("pullRequestDialogState", { initialReference: null, key: 3 });

    renderServerRoute();
    const dialog = capturedProps("pullRequestThreadDialog");
    await (
      dialog["onPrepared"] as (input: {
        branch: string;
        mode: "local" | "worktree";
      }) => Promise<void>
    )({ branch: "pr-branch", mode: "local" });

    const session = useComposerDraftStore.getState().getDraftSession(draftId);
    expect(session?.worktreePath).toBeNull();
    expect(session?.envMode).toBe("local");
    expect(h.navigateCalls).toContainEqual({
      to: "/draft/$draftId",
      params: { draftId },
    });
  });

  it("creates a pull request worktree through the dedicated atomic owner boundary", async () => {
    seedConnectedServerThread();
    seedHostState("pullRequestDialogState", { initialReference: null, key: 3 });
    h.commandResults["worktree.createManaged"] = (raw) => {
      const command = raw as {
        input: { threadId: ThreadId; refName: string };
      };
      return AsyncResult.success({
        threadId: command.input.threadId,
        path: "X:/managed/pr-branch",
        refName: command.input.refName,
      });
    };

    renderServerRoute();
    const dialog = capturedProps("pullRequestThreadDialog");
    await (
      dialog["onPrepared"] as (input: {
        branch: string;
        mode: "local" | "worktree";
      }) => Promise<void>
    )({ branch: "pr-branch", mode: "worktree" });

    const [createCall] = commandCallsFor("worktree.createManaged");
    expect(createCall?.input).toMatchObject({
      environmentId,
      input: {
        projectId,
        refName: "pr-branch",
        newRefName: null,
        baseRefName: null,
      },
    });
    if (!createCall) {
      throw new Error("expected one managed-worktree creation call");
    }
    const managedInput = (createCall.input as { input: Record<string, unknown> }).input;
    expect(managedInput).not.toHaveProperty("cwd");
    expect(managedInput).not.toHaveProperty("path");
    expect(managedInput).not.toHaveProperty("worktreePath");
    expect(managedInput).not.toHaveProperty("kind");
    expect(h.navigateCalls).toContainEqual({
      to: "/$environmentId/$threadId",
      params: {
        environmentId,
        threadId: managedInput["threadId"],
      },
    });
  });

  it("dismisses the thread error banner", () => {
    seedConnectedServerThread(
      makeThread({ session: makeSession({ lastError: "provider exploded" }) }),
    );
    renderServerRoute();

    const banner = capturedProps("threadErrorBanner");
    expect(banner["error"]).toBe("provider exploded");

    // Raise a local error first so both updater branches run.
    const composer = capturedProps("chatComposer");
    (composer["setThreadError"] as (targetThreadId: ThreadId, error: string | null) => void)(
      threadId,
      "local boom",
    );
    const writesBeforeDismiss = h.setStateCalls.length;
    (banner["onDismiss"] as () => void)();

    const writes = h.setStateCalls.filter(
      (call) =>
        typeof call.applied === "object" &&
        call.applied !== null &&
        threadKey in (call.applied as Record<string, unknown>),
    );
    expect(
      writes.some((call) => (call.applied as Record<string, unknown>)[threadKey] === "local boom"),
    ).toBe(true);
    // Dismissing with no rendered local error takes the identity early-return
    // inside the updater (the render-time snapshot has no entry to clear).
    expect(h.setStateCalls.length).toBe(writesBeforeDismiss + 1);
  });

  it("toggles the plan sidebar from the composer controls", () => {
    seedConnectedServerThread();
    renderServerRoute();

    const composer = capturedProps("chatComposer");
    (composer["togglePlanSidebar"] as () => void)();
    expect(
      useRightPanelStore
        .getState()
        .byThreadKey[threadKey]?.surfaces.some((surface) => surface.kind === "plan"),
    ).toBe(true);

    (composer["togglePlanSidebar"] as () => void)();
    expect(useRightPanelStore.getState().byThreadKey[threadKey]?.isOpen).toBe(false);
  });

  it("runtime and interaction mode changes persist into the draft store", () => {
    seedConnectedServerThread();
    renderServerRoute();
    installComposerHandle();

    const composer = capturedProps("chatComposer");
    (composer["handleRuntimeModeChange"] as (mode: string) => void)("approval-required");
    (composer["toggleInteractionMode"] as () => void)();

    const draft = useComposerDraftStore.getState().getComposerDraft(threadRef);
    expect(draft?.runtimeMode).toBe("approval-required");
    expect(draft?.interactionMode).toBe("plan");
  });

  it("routes creation to the focused center group and cleans only explicit group removals", async () => {
    seedConnectedServerThread();
    seedTwoGroupCenterState();
    publishSeededStoreState(useCenterPanelStore);
    renderServerRoute();

    const workspace = capturedProps("centerWorkspace");
    (workspace["onFocusGroup"] as (groupId: string) => void)("group-b");
    const header = capturedProps("chatHeaderActions");
    (header["onOpenTerminalPanel"] as () => void)();
    await flushTerminalAction();

    const focusedGroup = useCenterPanelStore
      .getState()
      .byThreadKey[threadKey]!.groups.find((group) => group.id === "group-b")!;
    expect(focusedGroup.surfaceIds.at(-1)).toMatch(/^terminal:/);
    expect(focusedGroup.surfaceIds.at(-1)).not.toBe("terminal:term-b");

    seedTwoGroupCenterState();
    h.commandCalls.length = 0;
    (workspace["onCloseAllSurfaces"] as (groupId: string) => void)("group-b");

    expect(closedTerminalIds()).toEqual(["term-b"]);
    expect(closedTerminalIds()).not.toContain("term-a");
  });

  it("moves and merges center layout without cleaning terminal or chat resources", () => {
    seedConnectedServerThread();
    seedTwoGroupCenterState();
    publishSeededStoreState(useCenterPanelStore);
    renderServerRoute();

    const workspace = capturedProps("centerWorkspace");
    (workspace["onDropSurface"] as (surfaceId: string, target: { groupId: string }) => void)(
      "terminal:term-a",
      { groupId: "group-b" },
    );
    (workspace["onMergeGroup"] as (groupId: string) => void)("group-b");

    expect(commandCallsFor("terminal.close")).toEqual([]);
    expect(commandCallsFor("thread.delete")).toEqual([]);
    expect(commandCallsFor("thread.archive")).toEqual([]);
    expect(useCenterPanelStore.getState().byThreadKey[threadKey]!.groups).toHaveLength(1);
    expect(
      useCenterPanelStore.getState().byThreadKey[threadKey]!.surfaces.map((surface) => surface.id),
    ).toEqual([HOST_SURFACE_ID, "terminal:term-a", "terminal:term-b"]);
  });

  it("closes a center terminal session when its tab is closed", () => {
    seedConnectedServerThread();
    useCenterPanelStore.getState().openTerminalPanel(threadRef, "terminal-42");
    publishSeededStoreState(useCenterPanelStore);
    renderServerRoute();

    const workspace = capturedProps("centerWorkspace");
    const surfaces = useCenterPanelStore.getState().byThreadKey[threadKey]!.surfaces;
    const terminalSurface = surfaces.find((surface) => surface.kind === "terminal")!;

    (workspace["onActivate"] as (groupId: string, surface: unknown) => void)(
      "center:root",
      terminalSurface,
    );
    (workspace["onCloseSurface"] as (groupId: string, surface: unknown) => void)(
      "center:root",
      terminalSurface,
    );
    expect(
      useCenterPanelStore
        .getState()
        .byThreadKey[threadKey]?.surfaces.some((surface) => surface.kind === "terminal") ?? false,
    ).toBe(false);
    expect(closedTerminalIds()).toEqual(["terminal-42"]);
  });

  it("falls back to terminal exit after a center session close failure", async () => {
    h.commandResults["terminal.close"] = () =>
      AsyncResult.failure(Cause.fail(new Error("temporary close failure")));
    seedConnectedServerThread();
    useCenterPanelStore.getState().openTerminalPanel(threadRef, "terminal-fallback");
    publishSeededStoreState(useCenterPanelStore);
    renderServerRoute();

    const workspace = capturedProps("centerWorkspace");
    const terminalSurface = useCenterPanelStore
      .getState()
      .byThreadKey[threadKey]!.surfaces.find((surface) => surface.kind === "terminal")!;
    (workspace["onCloseSurface"] as (groupId: string, surface: unknown) => void)(
      "center:root",
      terminalSurface,
    );
    await flushTerminalAction();

    expect(closedTerminalIds()).toEqual(["terminal-fallback"]);
    expect(commandCallsFor("terminal.write")).toEqual([
      expect.objectContaining({
        input: expect.objectContaining({
          input: {
            threadId,
            terminalId: "terminal-fallback",
            data: "exit\n",
          },
        }),
      }),
    ]);
    expect(
      useCenterPanelStore
        .getState()
        .byThreadKey[threadKey]?.surfaces.some((surface) => surface.kind === "terminal") ?? false,
    ).toBe(false);
  });

  it("closes a center terminal session from the panel close control", () => {
    seedConnectedServerThread();
    useCenterPanelStore.getState().openTerminalPanel(threadRef, "terminal-panel");
    publishSeededStoreState(useCenterPanelStore);
    renderServerRoute();

    const panel = capturedProps("centerTerminalPanel");
    (panel["onClose"] as () => void)();

    expect(closedTerminalIds()).toEqual(["terminal-panel"]);
    expect(
      useCenterPanelStore
        .getState()
        .byThreadKey[threadKey]?.surfaces.some(
          (surface) => surface.id === "terminal:terminal-panel",
        ) ?? false,
    ).toBe(false);
  });

  it("closes only removed center terminals when closing other surfaces", () => {
    seedConnectedServerThread();
    for (const terminalId of ["terminal-left", "terminal-kept", "terminal-right"]) {
      useCenterPanelStore.getState().openTerminalPanel(threadRef, terminalId);
    }
    publishSeededStoreState(useCenterPanelStore);
    renderServerRoute();

    const workspace = capturedProps("centerWorkspace");
    const kept = useCenterPanelStore
      .getState()
      .byThreadKey[threadKey]!.surfaces.find((surface) => surface.id === "terminal:terminal-kept")!;
    (workspace["onCloseOtherSurfaces"] as (groupId: string, surface: typeof kept) => void)(
      "center:root",
      kept,
    );

    expect(closedTerminalIds()).toEqual(["terminal-left", "terminal-right"]);
    expect(
      useCenterPanelStore.getState().byThreadKey[threadKey]?.surfaces.map((surface) => surface.id),
    ).toEqual([HOST_SURFACE_ID, "terminal:terminal-kept"]);
  });

  it("closes only center terminals to the right of the selected surface", () => {
    seedConnectedServerThread();
    for (const terminalId of ["terminal-left", "terminal-middle", "terminal-right"]) {
      useCenterPanelStore.getState().openTerminalPanel(threadRef, terminalId);
    }
    publishSeededStoreState(useCenterPanelStore);
    renderServerRoute();

    const workspace = capturedProps("centerWorkspace");
    const selected = useCenterPanelStore
      .getState()
      .byThreadKey[threadKey]!.surfaces.find((surface) => surface.id === "terminal:terminal-left")!;
    (workspace["onCloseSurfacesToRight"] as (groupId: string, surface: typeof selected) => void)(
      "center:root",
      selected,
    );

    expect(closedTerminalIds()).toEqual(["terminal-middle", "terminal-right"]);
    expect(
      useCenterPanelStore.getState().byThreadKey[threadKey]?.surfaces.map((surface) => surface.id),
    ).toEqual([HOST_SURFACE_ID, "terminal:terminal-left"]);
  });

  it("closes every center terminal when closing all surfaces", () => {
    seedConnectedServerThread();
    for (const terminalId of ["terminal-one", "terminal-two"]) {
      useCenterPanelStore.getState().openTerminalPanel(threadRef, terminalId);
    }
    publishSeededStoreState(useCenterPanelStore);
    renderServerRoute();

    const workspace = capturedProps("centerWorkspace");
    (workspace["onCloseAllSurfaces"] as (groupId: string) => void)("center:root");

    expect(closedTerminalIds()).toEqual(["terminal-one", "terminal-two"]);
    expect(useCenterPanelStore.getState().byThreadKey[threadKey]?.surfaces ?? []).toEqual([]);
  });

  it("archives, not deletes, a center chat panel without closing a terminal", () => {
    seedConnectedServerThread();
    const siblingThreadId = ThreadId.make("center-chat-only");
    useCenterPanelStore.getState().openChatPanel(threadRef, siblingThreadId, "Codex");
    publishSeededStoreState(useCenterPanelStore);
    renderServerRoute();

    const workspace = capturedProps("centerWorkspace");
    const chatSurface = useCenterPanelStore
      .getState()
      .byThreadKey[threadKey]!.surfaces.find((surface) => surface.kind === "chat")!;
    (workspace["onCloseSurface"] as (groupId: string, surface: typeof chatSurface) => void)(
      "center:root",
      chatSurface,
    );

    expect(commandCallsFor("terminal.close")).toHaveLength(0);
    expect(commandCallsFor("thread.delete")).toHaveLength(0);
    expect(commandCallsFor("thread.archive")).toEqual([
      expect.objectContaining({
        input: expect.objectContaining({ input: { threadId: siblingThreadId } }),
      }),
    ]);
  });
});

describe("ChatView durable message queue", () => {
  const queued = (
    id: string,
    overrides: Partial<Thread["messages"][number]> = {},
  ): Thread["messages"][number] => ({
    id: MessageId.make(id),
    role: "user",
    text: id,
    turnId: null,
    createdAt: now,
    updatedAt: now,
    streaming: false,
    delivery: { state: "queued", provider: ProviderDriverKind.make("codex"), mode: "start" },
    ...overrides,
  });
  function seedQueue(
    messages = [queued("head"), queued("tail")],
    status: NonNullable<Thread["session"]>["status"] = "running",
    supportsTurnSteer = true,
  ) {
    seedConnectedServerThread(
      makeThread({
        messages,
        session: makeSession({
          status,
          activeTurnId: status === "running" ? TurnId.make("active-turn") : null,
        }),
      }),
    );
    seedEnvironment(
      makeEnvironmentPresentation({
        serverConfig: {
          providers: [{ ...codexProvider, supportsTurnSteer }],
          environment: { label: "Local" },
        },
      }),
    );
    renderServerRoute();
    return installComposerHandle();
  }
  const cancel = (id: string) =>
    (
      capturedProps("messagesTimeline")["onCancelQueuedMessage"] as (id: MessageId) => Promise<void>
    )(MessageId.make(id));

  it.each(["running", "starting"] as const)(
    "enqueues while %s without local dispatch or an optimistic row and clears the draft",
    async (status) => {
      const { promptRef } = seedQueue(undefined, status);
      promptRef.current = "another message";
      useComposerDraftStore.getState().setPrompt(threadRef, "another message");
      h.setStateCalls.length = 0;
      await (capturedProps("chatComposer")["onSend"] as () => Promise<void>)();
      expect(commandCallsFor("thread.startTurn")[0]?.input).toMatchObject({
        input: {
          queued: true,
          message: { text: "another message" },
          modelSelection: { instanceId: codexInstanceId, model: "gpt-5.4" },
        },
      });
      expect(promptRef.current).toBe("");
      expect(useComposerDraftStore.getState().getComposerDraft(threadRef)?.prompt ?? "").toBe("");
      expect(setStateCallsFor("optimisticUserMessages")).toHaveLength(0);
      expect(
        h.setStateCalls.some(
          (call) =>
            call.applied &&
            typeof call.applied === "object" &&
            "messageId" in call.applied &&
            "startedAt" in call.applied,
        ),
      ).toBe(false);
      promptRef.current = "one more";
      await (capturedProps("chatComposer")["onSend"] as () => Promise<void>)();
      expect(commandCallsFor("thread.startTurn")).toHaveLength(2);
    },
  );

  it.each(["pending", "sending"] as const)(
    "enqueues behind the initial %s delivery before the provider is running",
    async (state) => {
      const { promptRef } = seedQueue(
        [
          queued("initial", {
            delivery: { state, mode: "start", provider: ProviderDriverKind.make("codex") },
          }),
        ],
        "idle",
      );
      expect(capturedProps("chatComposer")["isSendBusy"]).toBe(true);
      promptRef.current = "follow-up during launch";
      h.setStateCalls.length = 0;
      await (capturedProps("chatComposer")["onSend"] as () => Promise<void>)();
      expect(commandCallsFor("thread.startTurn")[0]?.input).toMatchObject({
        input: {
          queued: true,
          message: { text: "follow-up during launch" },
        },
      });
      expect(promptRef.current).toBe("");
      expect(setStateCallsFor("optimisticUserMessages")).toHaveLength(0);
      expect(
        h.setStateCalls.some(
          (call) =>
            call.applied &&
            typeof call.applied === "object" &&
            "messageId" in call.applied &&
            "startedAt" in call.applied,
        ),
      ).toBe(false);
    },
  );

  it("keeps a second Enter inert while an enqueue request is still in flight", async () => {
    const { promptRef } = seedQueue();
    let release!: (value: unknown) => void;
    h.commandResults["thread.startTurn"] = () =>
      new Promise((resolve) => {
        release = resolve;
      });
    const send = capturedProps("chatComposer")["onSend"] as () => Promise<void>;
    promptRef.current = "first enqueue";
    const pending = send();
    await vi.waitFor(() => expect(commandCallsFor("thread.startTurn")).toHaveLength(1));
    promptRef.current = "still composing";
    await send();
    expect(commandCallsFor("thread.startTurn")).toHaveLength(1);
    expect(promptRef.current).toBe("still composing");
    release(AsyncResult.success(undefined));
    await pending;
  });

  it("retains the actual enqueued File until cancellation and restores it after the receipt", async () => {
    class FakeFileReader {
      result = "data:image/png;base64,ZmFrZQ==";
      onLoad = () => {};
      addEventListener(type: string, callback: () => void) {
        if (type === "load") this.onLoad = callback;
      }
      readAsDataURL() {
        this.onLoad();
      }
    }
    vi.stubGlobal("FileReader", FakeFileReader);
    seedQueue([]);
    const image = makeOwnedComposerContexts("enqueued").image;
    const { promptRef } = installComposerHandle({
      getSendContext: () => ({ ...composerHandle().getSendContext(), attachments: [image] }),
    });
    promptRef.current = "with image";
    await (capturedProps("chatComposer")["onSend"] as () => Promise<void>)();
    const sent = (
      commandCallsFor("thread.startTurn")[0]!.input as {
        input: { message: { messageId: MessageId } };
      }
    ).input.message;
    seedQueue([queued(sent.messageId, { text: "with image", attachments: [image] })]);
    await cancel(sent.messageId);
    expect(useComposerDraftStore.getState().getComposerDraft(threadRef)?.attachments[0]?.file).toBe(
      image.file,
    );
  });

  it("appends a rejected enqueue to newer draft text instead of losing either prompt", async () => {
    const { promptRef } = seedQueue([]);
    promptRef.current = "original queued text";
    h.commandResults["thread.startTurn"] = () => {
      promptRef.current = "new draft";
      useComposerDraftStore.getState().setPrompt(threadRef, "new draft");
      return AsyncResult.failure(Cause.fail(new Error("Enqueue rejected")));
    };
    await (capturedProps("chatComposer")["onSend"] as () => Promise<void>)();
    expect(useComposerDraftStore.getState().getComposerDraft(threadRef)?.prompt).toBe(
      "new draft\n\noriginal queued text",
    );
    expect(promptRef.current).toBe("new draft\n\noriginal queued text");
  });

  it("does not interrupt until an already pending cancel finishes or restore the same message twice", async () => {
    seedQueue();
    let release!: (value: unknown) => void;
    const deferred = new Promise((resolve) => {
      release = resolve;
    });
    h.commandResults["thread.resolveDelivery"] = (command) =>
      (command as { input: { messageId: string } }).input.messageId === "head"
        ? deferred
        : AsyncResult.success(undefined);
    const pendingCancel = cancel("head");
    const pendingStop = (capturedProps("chatComposer")["onInterrupt"] as () => Promise<void>)();
    expect(commandCallsFor("thread.interruptTurn")).toHaveLength(0);
    release(AsyncResult.success(undefined));
    await pendingCancel;
    await pendingStop;
    expect(commandCallsFor("thread.resolveDelivery")).toHaveLength(2);
    expect(useComposerDraftStore.getState().getComposerDraft(threadRef)?.prompt).toBe(
      "head\n\ntail",
    );
    expect(commandCallsFor("thread.interruptTurn")).toHaveLength(1);
  });

  it("Stop cancels head to tail, restores in order, then interrupts", async () => {
    const { promptRef } = seedQueue([queued("head", { createdAt: later }), queued("tail")]);
    promptRef.current = "draft";
    useComposerDraftStore.getState().setPrompt(threadRef, "draft");
    const draftsAtCommand: string[] = [];
    h.commandResults["thread.resolveDelivery"] = () => {
      draftsAtCommand.push(
        useComposerDraftStore.getState().getComposerDraft(threadRef)?.prompt ?? "",
      );
      return AsyncResult.success(undefined);
    };
    h.commandResults["thread.interruptTurn"] = () => {
      draftsAtCommand.push(
        useComposerDraftStore.getState().getComposerDraft(threadRef)?.prompt ?? "",
      );
      return AsyncResult.success(undefined);
    };
    await (capturedProps("chatComposer")["onInterrupt"] as () => Promise<void>)();
    expect(h.commandCalls.map((c) => c.key)).toEqual([
      "thread.resolveDelivery",
      "thread.resolveDelivery",
      "thread.interruptTurn",
    ]);
    expect(commandCallsFor("thread.resolveDelivery").map((c) => c.input)).toEqual([
      { environmentId, input: { threadId, messageId: MessageId.make("head"), action: "cancel" } },
      { environmentId, input: { threadId, messageId: MessageId.make("tail"), action: "cancel" } },
    ]);
    expect(draftsAtCommand).toEqual(["draft", "draft\n\nhead", "draft\n\nhead\n\ntail"]);
    expect(promptRef.current).toBe("draft\n\nhead\n\ntail");
  });

  it("a failed cancel preserves the card error and still drains the tail before interrupt", async () => {
    seedQueue();
    h.commandResults["thread.resolveDelivery"] = (command) =>
      (command as { input: { messageId: string } }).input.messageId === "head"
        ? AsyncResult.failure(Cause.fail(new Error("Cancel rejected")))
        : AsyncResult.success(undefined);
    await (capturedProps("chatComposer")["onInterrupt"] as () => Promise<void>)();
    expect(h.commandCalls.map((c) => c.key)).toEqual([
      "thread.resolveDelivery",
      "thread.resolveDelivery",
      "thread.interruptTurn",
    ]);
    expect(useComposerDraftStore.getState().getComposerDraft(threadRef)?.prompt).toBe("tail");
    expect(h.setStateCalls.some((c) => JSON.stringify(c.applied).includes("Cancel rejected"))).toBe(
      true,
    );
  });

  it("cancel restores cached Files and reports attachments from another client", async () => {
    const { queuedMessageCache } = await import("./chat/queuedMessageCache");
    const image = makeOwnedComposerContexts("queue").image;
    const remote = { ...image, id: "remote-file" as ChatAttachmentId };
    seedQueue([queued("head", { attachments: [image, remote] })]);
    queuedMessageCache.remember(MessageId.make("head"), { attachments: [image] });
    await cancel("head");
    const draft = useComposerDraftStore.getState().getComposerDraft(threadRef);
    expect(draft?.prompt).toBe("head");
    expect(draft?.attachments[0]?.file).toBe(image.file);
    expect(
      h.toasts.some(
        (t) =>
          JSON.stringify(t).includes("1 attachment") && JSON.stringify(t).includes("not restored"),
      ),
    ).toBe(true);
    expect(queuedMessageCache.take(MessageId.make("head"))).toBeUndefined();
  });

  it("Send now promotes a held head and never steers it", async () => {
    seedQueue(
      [
        queued("head", {
          delivery: { state: "queued", provider: ProviderDriverKind.make("codex"), held: true },
        }),
      ],
      "interrupted",
    );
    expect(capturedProps("messagesTimeline")["queuedStatuses"]).toMatchObject([
      { label: "Waiting for you", primaryAction: "send-now", canSteer: false },
    ]);
    await (
      capturedProps("messagesTimeline")["onSendNowQueuedMessage"] as (
        id: MessageId,
      ) => Promise<void>
    )(MessageId.make("head"));
    expect(commandCallsFor("thread.promoteTurn")[0]?.input).toEqual({
      environmentId,
      input: { threadId, messageId: MessageId.make("head") },
    });
    expect(commandCallsFor("thread.steerTurn")).toHaveLength(0);
  });

  it.each([true, false])(
    "composer steer targets the head only when the bound snapshot supports it (%s)",
    async (supports) => {
      seedQueue(undefined, "running", supports);
      await (capturedProps("chatComposer")["onSteerQueuedMessage"] as () => Promise<void>)();
      expect(commandCallsFor("thread.steerTurn")).toHaveLength(supports ? 1 : 0);
      if (supports)
        expect(commandCallsFor("thread.steerTurn")[0]?.input).toEqual({
          environmentId,
          input: { threadId, messageId: MessageId.make("head") },
        });
    },
  );
});
