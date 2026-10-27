/**
 * Shared harness for the Sidebar tests: hoisted capture state, every vi.mock,
 * fixtures and helpers. `Sidebar.test.tsx` (static rendering, Node) and
 * `Sidebar.browser.test.tsx` (mounted, happy-dom) import it first, so the mocks
 * are registered before the module under test loads.
 */
import * as Cause from "effect/Cause";
import { renderToStaticMarkup } from "react-dom/server";
import * as React from "react";
import { afterEach, beforeEach, describe, vi } from "vite-plus/test";

import { EnvironmentId, ProjectId, ProviderInstanceId, ThreadId } from "@coddy/contracts";
import { DEFAULT_CLIENT_SETTINGS } from "@coddy/contracts/settings";
import { createModelSelection } from "@coddy/shared/model";
import { scopedThreadKey, scopeThreadRef } from "@coddy/client-runtime/environment";
import { createEnvironmentPresentationPolicy } from "../connection/environmentPresentationPolicy";
import type {
  EnvironmentProject,
  EnvironmentThreadShell,
} from "@coddy/client-runtime/state/shell";

const browserRuntime =
  typeof document !== "undefined" && typeof document.createElement === "function";
const staticDescribe = browserRuntime ? describe.skip : describe;

// ─────────────────────────────────────────────────────────────────────────────
// Hoisted harness state shared with every vi.mock factory.
// ─────────────────────────────────────────────────────────────────────────────

const h = vi.hoisted(() => {
  interface Captured {
    readonly name: string;
    readonly props: Record<string, unknown>;
  }

  const state: any = {
    React: null,
    captures: [] as Captured[],
    // data
    projects: [],
    threads: [],
    environments: [],
    primaryEnvironmentId: null,
    activeEnvironmentId: null,
    serverConfigs: new Map(),
    remoteUpdateRuns: new Map<string, unknown>(),
    remoteUpdateSnapshots: new Map<string, unknown>(),
    remoteUpdateSnapshotQueries: [] as string[],
    requestRemoteUpdate: vi.fn(),
    clientSettings: null,
    atomValues: {},
    vcsStatusByCwd: {},
    vcsQueries: [] as Array<{ __q?: string; args?: { input?: { cwd?: string } } }>,
    worktreeCatalogs: new Map<string, unknown>(),
    discoveryCatalogSubscriptions: [] as Array<unknown>,
    discoveryFocusRefreshCalls: [] as Array<ReadonlyArray<unknown>>,
    runningTerminalIds: [],
    discoveredPortsByThreadId: {},
    desktopBootstraps: [],
    desktopUpdateState: null,
    shellSummary: {
      catalogReady: false,
      desiredEnvironmentCount: 0,
      statuses: [],
      canShowEmptyProjects: false,
      hasSnapshot: false,
      hasSynchronizingShell: false,
      hasCachedShell: false,
      hasLiveShell: false,
      firstError: null,
      latestSnapshotUpdatedAt: null,
    },
    updateBtnDisabled: false,
    updateBtnAction: "none",
    showArmWarning: false,
    isMobile: false,
    sidebarCtx: null,
    pathname: "/",
    routeParams: {},
    localApi: null,
    isDesktopHost: true,
    copyShouldFail: false,
    openDiscoveredPortResult: { _tag: "Success", value: undefined },
    commandResults: {},
    commandCalls: [],
    shortcutLabels: {},
    shortcutLabelOptions: [] as unknown[],
    showJumpHintModifiers: false,
    terminalSurfaceOpen: false,
    presentationPolicy: null as unknown,
    // mock stores
    ui: null,
    selection: null,
    meta: null,
  };

  const capture = (name: string, props: Record<string, unknown>) => {
    state.captures.push({ name, props });
  };

  const mk = (name: string, tag = "div") => {
    const Comp = (props: Record<string, unknown>) => {
      capture(name, props);
      const R = state.React as typeof import("react");
      const { children, render } = props as { children?: unknown; render?: unknown };
      const passthrough: Record<string, unknown> = {
        "data-mock": name,
      };
      const domProps = new Set([
        "aria-label",
        "autoFocus",
        "className",
        "disabled",
        "id",
        "onBlur",
        "onBlurCapture",
        "onChange",
        "onClick",
        "onContextMenu",
        "onDoubleClick",
        "onFocus",
        "onKeyDown",
        "onMouseLeave",
        "onPointerDown",
        "placeholder",
        "readOnly",
        "role",
        "tabIndex",
        "title",
        "type",
        "value",
      ]);
      for (const [key, value] of Object.entries(props)) {
        if (key.startsWith("data-") || domProps.has(key)) {
          passthrough[key] = value;
        }
      }
      if (render !== undefined && R.isValidElement(render)) {
        return children === undefined
          ? R.cloneElement(render as never, passthrough as never)
          : R.cloneElement(render as never, passthrough as never, children as never);
      }
      return R.createElement(tag, passthrough, children as never);
    };
    Comp.displayName = name;
    return Comp;
  };

  const makeStore = <T extends object>(init: () => T) => {
    let current = init();
    const hook = (selector?: (s: T) => unknown) =>
      selector ? selector(current) : (current as unknown);
    hook.getState = () => current;
    hook.setState = (partial: Partial<T>) => {
      current = { ...current, ...partial };
    };
    hook.reset = () => {
      current = init();
    };
    return hook;
  };

  const spies = {
    cardLookupPass: vi.fn(),
    navigate: vi.fn(),
    routerNavigate: vi.fn(),
    openPrLink: vi.fn(),
    openAddProject: vi.fn(),
    newThreadHandler: vi.fn(),
    updateSettings: vi.fn(),
    archiveThread: vi.fn(),
    deleteThread: vi.fn(),
    requestWorktreeRemoval: vi.fn(),
    closeWorktreeRemovalDialog: vi.fn(),
    completeWorktreeRemoval: vi.fn(),
    contextMenuShow: vi.fn(),
    dialogConfirm: vi.fn(),
    toastAdd: vi.fn(),
    toastClose: vi.fn(),
    stackedThreadToast: vi.fn((toast: unknown) => toast),
    setOpenMobile: vi.fn(),
    markRead: vi.fn(),
    markUnread: vi.fn(),
    togglePinned: vi.fn(),
    markThreadUnread: vi.fn(),
    setProjectExpanded: vi.fn(),
    reorderProjects: vi.fn(),
    toggleThread: vi.fn(),
    rangeSelectTo: vi.fn(),
    clearSelection: vi.fn(),
    removeFromSelection: vi.fn(),
    setAnchor: vi.fn(),
    getDraftThreadByProjectRef: vi.fn(),
    clearDraftThread: vi.fn(),
    clearProjectDraftThreadId: vi.fn(),
    openDiscoveredPort: vi.fn(),
    useEnvironmentThread: vi.fn(),
    autoAnimate: vi.fn(),
    pointerWithin: vi.fn(),
    closestCorners: vi.fn(),
    copyToClipboard: vi.fn(),
    windowConfirm: vi.fn(),
    openProjectDataRecovery: vi.fn(async () => undefined),
  };

  const runCommand = (command: { label?: string }, input: unknown) => {
    const label = command?.label ?? "unknown";
    state.commandCalls.push({ label, input });
    const impl = state.commandResults[label] as ((value: unknown) => unknown) | undefined;
    return Promise.resolve(impl ? impl(input) : { _tag: "Success", value: undefined });
  };

  const uiStore = makeStore(() => ({
    projectExpandedById: {} as Record<string, boolean>,
    projectOrder: [] as string[],
    threadLastVisitedAtById: {} as Record<string, string>,
    markThreadVisited: vi.fn(),
    markThreadUnread: spies.markThreadUnread,
    setThreadChangedFilesExpanded: vi.fn(),
    setDefaultAdvertisedEndpointKey: vi.fn(),
    setProjectExpanded: spies.setProjectExpanded,
    reorderProjects: spies.reorderProjects,
    sidebarView: "environments" as "environments" | "repositories",
    repositoryGroupExpandedById: {} as Record<string, boolean>,
    setSidebarView: vi.fn((view: "environments" | "repositories") =>
      uiStore.setState({ sidebarView: view }),
    ),
    setRepositoryGroupExpanded: vi.fn((key: string, expanded: boolean) =>
      uiStore.setState({
        repositoryGroupExpandedById: {
          ...(uiStore.getState() as { repositoryGroupExpandedById: Record<string, boolean> })
            .repositoryGroupExpandedById,
          [key]: expanded,
        },
      }),
    ),
  }));

  const selectionStore = makeStore(() => ({
    selectedThreadKeys: new Set<string>(),
    anchorThreadKey: null as string | null,
    toggleThread: spies.toggleThread,
    rangeSelectTo: spies.rangeSelectTo,
    clearSelection: spies.clearSelection,
    removeFromSelection: spies.removeFromSelection,
    setAnchor: spies.setAnchor,
    hasSelection: () =>
      (selectionStore.getState() as { selectedThreadKeys: Set<string> }).selectedThreadKeys.size >
      0,
    selectedProjectKey: null as string | null,
    selectedProjectRouteThreadKey: null as string | null,
    selectProject: (projectKey: string, routeThreadKey: string | null) => {
      selectionStore.setState({
        selectedThreadKeys: new Set<string>(),
        anchorThreadKey: null,
        selectedProjectKey: projectKey,
        selectedProjectRouteThreadKey: routeThreadKey,
      });
    },
    clearProjectSelection: () => {
      selectionStore.setState({ selectedProjectKey: null, selectedProjectRouteThreadKey: null });
    },
  }));

  const metaStore = makeStore(() => ({
    pinnedThreadKeys: [] as string[],
    unreadThreadKeys: [] as string[],
    togglePinned: spies.togglePinned,
    markUnread: spies.markUnread,
    markRead: spies.markRead,
  }));

  state.ui = uiStore;
  state.selection = selectionStore;
  state.meta = metaStore;
  return {
    state,
    spies,
    capture,
    mk,
    runCommand,
    uiStore,
    selectionStore,
    metaStore,
  };
});

// ─────────────────────────────────────────────────────────────────────────────
// Module mocks
// ─────────────────────────────────────────────────────────────────────────────

vi.mock("@tanstack/react-router", () => ({
  Link: h.mk("Link", "a"),
  useNavigate: () => h.spies.navigate,
  useRouter: () => ({ navigate: h.spies.routerNavigate }),
  useLocation: (opts?: { select?: (loc: { pathname: string }) => unknown }) =>
    opts?.select ? opts.select({ pathname: h.state.pathname }) : { pathname: h.state.pathname },
  useParams: (opts?: { select?: (params: Record<string, string>) => unknown }) =>
    opts?.select ? opts.select(h.state.routeParams) : h.state.routeParams,
}));

vi.mock("@effect/atom-react", () => ({
  useAtomValue: (atom: { id?: string }) => h.state.atomValues[atom?.id ?? ""] ?? null,
  useAtomRefresh: () => () => {},
  RegistryContext: null,
}));

vi.mock("@formkit/auto-animate", () => ({
  autoAnimate: h.spies.autoAnimate,
}));

vi.mock("@dnd-kit/core", () => ({
  DndContext: h.mk("DndContext"),
  PointerSensor: function PointerSensor() {},
  useSensor: (sensor: unknown, options: unknown) => ({ sensor, options }),
  useSensors: (...sensors: unknown[]) => sensors,
  pointerWithin: h.spies.pointerWithin,
  closestCorners: h.spies.closestCorners,
}));

vi.mock("@dnd-kit/sortable", () => ({
  SortableContext: h.mk("SortableContext"),
  verticalListSortingStrategy: {},
  useSortable: () => ({
    attributes: { "data-sortable": true },
    listeners: {},
    setActivatorNodeRef: vi.fn(),
    setNodeRef: vi.fn(),
    transform: null,
    transition: undefined,
    isDragging: false,
    isOver: false,
  }),
}));

vi.mock("@dnd-kit/modifiers", () => ({
  restrictToFirstScrollableAncestor: () => ({}),
  restrictToVerticalAxis: () => ({}),
}));

vi.mock("@dnd-kit/utilities", () => ({
  CSS: { Translate: { toString: () => "" } },
}));

vi.mock("../env", () => ({
  get isDesktopHost() {
    return h.state.isDesktopHost as boolean;
  },
}));

vi.mock("../connection/currentEnvironmentPresentation", () => ({
  readCurrentEnvironmentPresentationPolicy: () => h.state.presentationPolicy,
}));

vi.mock("../state/entities", () => ({
  useProjects: () => h.state.projects,
  useThreadShells: () => h.state.threads,
  useActiveEnvironmentId: () => h.state.activeEnvironmentId,
  setActiveEnvironmentId: (environmentId: unknown) => {
    h.state.activeEnvironmentId = environmentId;
  },
  useThreadShellsForProjectRefs: (
    refs: ReadonlyArray<{ environmentId: string; projectId: string }>,
  ) =>
    (h.state.threads as Array<{ environmentId: string; projectId: string }>).filter((thread) =>
      refs.some(
        (ref) => ref.environmentId === thread.environmentId && ref.projectId === thread.projectId,
      ),
    ),
  useProject: (ref: { environmentId: string; projectId: string } | null) =>
    ref
      ? ((h.state.projects as Array<{ environmentId: string; id: string }>).find(
          (project) => project.environmentId === ref.environmentId && project.id === ref.projectId,
        ) ?? null)
      : null,
  useServerConfigs: () => h.state.serverConfigs,
  readThreadShell: () => null,
}));

vi.mock("../state/environments", () => ({
  useEnvironments: () => ({ environments: h.state.environments }),
  usePrimaryEnvironmentId: () => h.state.primaryEnvironmentId,
  useEnvironment: (environmentId: string) =>
    (h.state.environments as Array<{ environmentId: string }>).find(
      (environment) => environment.environmentId === environmentId,
    ) ?? null,
}));

vi.mock("../state/query", () => ({
  useEnvironmentQuery: (
    atom: {
      __q?: string;
      args?: { environmentId?: string; input?: { cwd?: string; projectId?: string } };
    } | null,
  ) => {
    if (atom?.__q?.startsWith("vcs.")) {
      h.state.vcsQueries.push(atom);
    }
    if (atom?.__q === "remoteUpdate.snapshot")
      h.state.remoteUpdateSnapshotQueries.push(atom.args?.environmentId);
    return {
      data:
        atom?.__q === "remoteUpdate.snapshot"
          ? (h.state.remoteUpdateSnapshots.get(atom.args?.environmentId) ?? null)
          : atom?.__q === "worktree.catalog"
            ? (h.state.worktreeCatalogs.get(
                `${atom.args?.environmentId}:${atom.args?.input?.projectId}`,
              ) ?? null)
            : atom
              ? (h.state.vcsStatusByCwd[atom.args?.input?.cwd ?? ""] ?? null)
              : null,
      error: null,
      emission: { _tag: "Success" },
      isPending: false,
      refresh: () => {},
    };
  },
}));

vi.mock("../state/remoteUpdates", () => ({
  remoteUpdateEnvironment: {
    snapshot: (args: unknown) => ({ __q: "remoteUpdate.snapshot", args }),
    check: { label: "remote-update.check" },
  },
  useRemoteUpdateRun: (id: string | null) =>
    id === null ? null : (h.state.remoteUpdateRuns.get(id) ?? null),
  useRemoteUpdateCheckState: () => ({ inFlight: false, failure: null }),
  useRequestRemoteUpdateConfirmation: () => h.state.requestRemoteUpdate,
}));

vi.mock("../state/terminalSessions", () => ({
  useThreadRunningTerminalIds: () => h.state.runningTerminalIds,
}));

vi.mock("../portDiscoveryState", () => ({
  useThreadDiscoveredPorts: (ref: { threadId: string }) =>
    h.state.discoveredPortsByThreadId[ref.threadId] ?? [],
}));

vi.mock("./preview/openDiscoveredPort", () => ({
  openDiscoveredPort: h.spies.openDiscoveredPort,
}));

vi.mock("../state/use-atom-command", () => ({
  useAtomCommand: (command: { label?: string }) => (input: unknown) => h.runCommand(command, input),
}));

vi.mock("../state/preview", () => ({
  previewEnvironment: { open: { label: "preview.open" } },
}));

vi.mock("../state/projects", () => ({
  projectEnvironment: {
    delete: { label: "project.delete" },
    update: { label: "project.update" },
  },
}));

vi.mock("../state/shell", () => ({
  shellEnvironment: { openInEditor: { label: "shell.openInEditor" } },
  environmentAvailabilityCommands: {
    retry: { label: "environment.retry" },
    adoptStorage: { label: "environment.adoptStorage" },
  },
  useEnvironmentShellSummary: () => h.state.shellSummary,
}));

vi.mock("../state/threads", () => ({
  threadEnvironment: {
    updateMetadata: { label: "thread.updateMetadata" },
    create: { label: "thread.create" },
  },
  useEnvironmentThread: h.spies.useEnvironmentThread,
}));

vi.mock("../state/vcs", () => ({
  vcsEnvironment: {
    status: (args: unknown) => ({ __q: "vcs.status", args }),
    summary: (args: unknown) => ({ __q: "vcs.summary", args }),
    pull: { label: "vcs.pull" },
    refreshStatus: { label: "vcs.refreshStatus" },
  },
}));

vi.mock("../state/server", () => ({
  primaryServerConfigAtom: { id: "primaryServerConfig" },
  primaryServerKeybindingsAtom: { id: "primaryServerKeybindings" },
}));

vi.mock("../state/desktopUpdate", () => ({
  useDesktopUpdateState: () => h.state.desktopUpdateState,
}));

vi.mock("../state/worktrees", () => ({
  worktreeEnvironment: {
    catalog: (args: unknown) => {
      h.state.discoveryCatalogSubscriptions.push(args);
      return { __q: "worktree.catalog", args };
    },
    refresh: { label: "worktree.refresh" },
    updatePolicy: { label: "worktree.updatePolicy" },
    addOne: { label: "worktree.addOne" },
    addAll: { label: "worktree.addAll" },
  },
  useWorktreeCatalogFocusRefresh: (projects: ReadonlyArray<unknown>) => {
    h.state.discoveryFocusRefreshCalls.push([...projects]);
  },
}));

vi.mock("../state/projectDataSafety", () => ({
  projectDataSafetyStore: {
    open: h.spies.openProjectDataRecovery,
  },
}));

vi.mock("./desktopUpdate.logic", () => ({
  isDesktopUpdateButtonDisabled: () => h.state.updateBtnDisabled,
  resolveDesktopUpdateButtonAction: () => h.state.updateBtnAction,
  shouldShowArm64IntelBuildWarning: () => h.state.showArmWarning,
  getArm64IntelBuildWarningDescription: () => "Running the Intel build on Apple Silicon.",
  getDesktopUpdateInstallConfirmationMessage: () => "Install the update now?",
  getDesktopUpdateActionError: (result: { error?: string } | null | undefined) =>
    result?.error ?? null,
  shouldToastDesktopUpdateActionResult: (result: { toast?: boolean } | null | undefined) =>
    result?.toast === true,
}));

vi.mock("../hooks/useThreadActions", () => ({
  useThreadActions: () => ({
    archiveThread: h.spies.archiveThread,
    deleteThread: h.spies.deleteThread,
    worktreeRemovalTarget: null,
    requestWorktreeRemoval: h.spies.requestWorktreeRemoval,
    closeWorktreeRemovalDialog: h.spies.closeWorktreeRemovalDialog,
    completeWorktreeRemoval: h.spies.completeWorktreeRemoval,
  }),
}));

vi.mock("./WorktreeAvailabilityWarning", () => ({
  WorktreeAvailabilityWarning: ({ status, onRetry, onRemove }: any) => {
    h.capture("WorktreeAvailabilityWarning", { status, onRetry, onRemove });
    return (
      <div data-testid={`availability-warning-${status.threadId}`}>
        <span>{status.availability}</span>
        <span>{status.path}</span>
        <button type="button" onClick={onRetry}>
          Retry detection
        </button>
        <button type="button" onClick={onRemove}>
          Remove from Coddy Studio
        </button>
      </div>
    );
  },
}));

vi.mock("./WorktreeRemovalDialog", () => ({
  WorktreeRemovalDialog: (props: Record<string, unknown>) => {
    h.capture("WorktreeRemovalDialog", props);
    return null;
  },
}));

vi.mock("./ImportCliSessionsDialog", () => ({
  ImportCliSessionsDialog: (props: Record<string, unknown>) => {
    h.capture("ImportCliSessionsDialog", props);
    return null;
  },
}));

vi.mock("../hooks/useHandleNewThread", () => ({
  useNewThreadHandler: () => h.spies.newThreadHandler,
}));

vi.mock("~/hooks/useSettings", () => ({
  usePrimarySettings: (selector: (settings: unknown) => unknown) =>
    selector({ ...h.state.clientSettings }),
  useClientSettings: (selector: (settings: unknown) => unknown) => selector(h.state.clientSettings),
  useUpdateClientSettings: () => h.spies.updateSettings,
}));

vi.mock("~/hooks/useCopyToClipboard", () => ({
  useCopyToClipboard: (opts?: {
    onCopy?: (ctx: unknown) => void;
    onError?: (error: Error, ctx: unknown) => void;
  }) => ({
    copyToClipboard: vi.fn((value: string, ctx: unknown) => {
      h.spies.copyToClipboard(value, ctx);
      if (h.state.copyShouldFail) {
        opts?.onError?.(new Error("copy failed"), ctx);
      } else {
        opts?.onCopy?.(ctx);
      }
    }),
  }),
}));

vi.mock("~/hooks/useMediaQuery", () => ({
  useIsMobile: () => h.state.isMobile,
}));

vi.mock("../connection/useDesktopLocalBootstraps", () => ({
  useDesktopLocalBootstraps: () => h.state.desktopBootstraps,
}));

vi.mock("../lib/openPullRequestLink", () => ({
  useOpenPrLink: () => h.spies.openPrLink,
}));

vi.mock("../commandPaletteContext", () => ({
  useOpenAddProjectCommandPalette: () => h.spies.openAddProject,
}));

vi.mock("../localApi", () => ({
  readLocalApi: () => h.state.localApi,
}));

vi.mock("../keybindings", () => ({
  resolveShortcutCommand: () => null,
  shortcutLabelForCommand: (_config: unknown, command: string, options: unknown) => {
    h.state.shortcutLabelOptions.push(options);
    const labels = h.state.shortcutLabels as Record<string, string | null>;
    return command in labels ? labels[command] : "Mod+K";
  },
  shouldShowThreadJumpHintsForModifiers: () => h.state.showJumpHintModifiers,
  threadJumpCommandForIndex: (index: number) => (index < 9 ? `thread.jump.${index + 1}` : null),
  threadJumpIndexFromCommand: (command: string) => {
    const match = /^thread\.jump\.([1-9])$/u.exec(command);
    return match ? Number(match[1]) - 1 : null;
  },
  threadTraversalDirectionFromCommand: (command: string | null) =>
    command === "thread.previous" ? "previous" : command === "thread.next" ? "next" : null,
}));

vi.mock("../uiStateStore", async (importOriginal) => {
  const actual = await importOriginal<Record<string, unknown>>();
  return { ...actual, useUiStateStore: h.uiStore };
});

vi.mock("../threadSelectionStore", () => ({
  useThreadSelectionStore: h.selectionStore,
}));

vi.mock("../sidebarWorkspaceMetaStore", async (importOriginal) => {
  const actual = await importOriginal<Record<string, unknown>>();
  return { ...actual, useSidebarWorkspaceMetaStore: h.metaStore };
});

vi.mock("../terminalSurfaceState", () => ({
  useThreadHasTerminalSurface: () => h.state.terminalSurfaceOpen,
}));

vi.mock("../composerDraftStore", () => ({
  DraftId: { make: (value: string) => value },
  useComposerDraftStore: {
    getState: () => ({
      getDraftThreadByProjectRef: h.spies.getDraftThreadByProjectRef,
      clearDraftThread: h.spies.clearDraftThread,
      clearProjectDraftThreadId: h.spies.clearProjectDraftThreadId,
    }),
  },
}));

vi.mock("./ThreadStatusIndicators", () => ({
  ChangeRequestStatusIcon: h.mk("ChangeRequestStatusIcon", "span"),
  resolveThreadPr: (branch: string | null, data: { pr?: unknown; refName?: string } | null) =>
    branch !== null && data?.refName === branch && data.pr ? data.pr : null,
  prStatusIndicator: (pr: { url: string } | null) =>
    pr ? { url: pr.url, tooltip: "PR open", colorClass: "pr-color", numberLabel: "#1" } : null,
  terminalStatusFromRunningIds: (ids: readonly string[]) =>
    ids.length > 0
      ? { label: `${ids.length} terminal running`, colorClass: "term-color", pulse: true }
      : null,
}));

vi.mock("./ProjectFavicon", () => ({ ProjectFavicon: h.mk("ProjectFavicon", "span") }));
vi.mock("./CreateWorktreeDialog", () => ({
  CreateWorktreeDialog: h.mk("CreateWorktreeDialog"),
}));
vi.mock("./settings/SettingsSidebarNav", () => ({
  SettingsSidebarNav: h.mk("SettingsSidebarNav"),
}));
vi.mock("./sidebar/EnvironmentContextCard", () => ({
  EnvironmentContextCard: h.mk("EnvironmentContextCard"),
}));
vi.mock("./settings/ManualUpdateStepsDialog", () => ({
  ManualUpdateStepsDialog: h.mk("ManualUpdateStepsDialog"),
}));
vi.mock("./sidebar/SidebarUpdatePill", () => ({
  SidebarUpdatePill: h.mk("SidebarUpdatePill", "span"),
}));
vi.mock("./sidebar/SidebarProviderUpdatePill", () => ({
  SidebarProviderUpdatePill: h.mk("SidebarProviderUpdatePill", "span"),
}));

vi.mock("./ui/toast", () => ({
  toastManager: {
    add: h.spies.toastAdd,
    close: h.spies.toastClose,
  },
  stackedThreadToast: h.spies.stackedThreadToast,
}));

vi.mock("./ui/alert", () => ({
  Alert: h.mk("Alert"),
  AlertAction: h.mk("AlertAction"),
  AlertDescription: h.mk("AlertDescription"),
  AlertTitle: h.mk("AlertTitle"),
}));

vi.mock("./ui/button", () => ({ Button: h.mk("Button", "button") }));
vi.mock("./ui/input", () => ({ Input: h.mk("Input", "input") }));
vi.mock("./ui/kbd", () => ({ Kbd: h.mk("Kbd", "kbd") }));
vi.mock("./ui/command", () => ({ CommandDialogTrigger: h.mk("CommandDialogTrigger") }));

vi.mock("./ui/dialog", () => ({
  Dialog: h.mk("Dialog"),
  DialogDescription: h.mk("DialogDescription"),
  DialogFooter: h.mk("DialogFooter"),
  DialogHeader: h.mk("DialogHeader"),
  DialogPanel: h.mk("DialogPanel"),
  DialogPopup: h.mk("DialogPopup"),
  DialogTitle: h.mk("DialogTitle"),
}));

vi.mock("./ui/menu", () => ({
  Menu: h.mk("Menu"),
  MenuGroup: h.mk("MenuGroup"),
  MenuPopup: h.mk("MenuPopup"),
  MenuRadioGroup: h.mk("MenuRadioGroup"),
  MenuRadioItem: h.mk("MenuRadioItem"),
  MenuSeparator: h.mk("MenuSeparator"),
  MenuTrigger: h.mk("MenuTrigger", "button"),
}));

vi.mock("./ui/number-field", () => ({
  NumberField: h.mk("NumberField"),
  NumberFieldDecrement: h.mk("NumberFieldDecrement", "button"),
  NumberFieldGroup: h.mk("NumberFieldGroup"),
  NumberFieldIncrement: h.mk("NumberFieldIncrement", "button"),
  NumberFieldInput: h.mk("NumberFieldInput", "input"),
}));

vi.mock("./ui/select", () => ({
  Select: h.mk("Select"),
  SelectItem: h.mk("SelectItem"),
  SelectPopup: h.mk("SelectPopup"),
  SelectTrigger: h.mk("SelectTrigger", "button"),
  SelectValue: h.mk("SelectValue", "span"),
}));

vi.mock("./ui/tooltip", () => ({
  Tooltip: h.mk("Tooltip", "span"),
  TooltipPopup: h.mk("TooltipPopup", "span"),
  TooltipTrigger: h.mk("TooltipTrigger", "span"),
}));

vi.mock("./ui/sidebar", () => ({
  SidebarContent: h.mk("SidebarContent"),
  SidebarFooter: h.mk("SidebarFooter"),
  SidebarGroup: h.mk("SidebarGroup"),
  SidebarHeader: h.mk("SidebarHeader"),
  SidebarMenu: h.mk("SidebarMenu", "ul"),
  SidebarMenuButton: h.mk("SidebarMenuButton", "button"),
  SidebarMenuItem: h.mk("SidebarMenuItem", "li"),
  SidebarMenuSub: h.mk("SidebarMenuSub", "ul"),
  SidebarMenuSubButton: h.mk("SidebarMenuSubButton", "button"),
  SidebarMenuSubItem: h.mk("SidebarMenuSubItem", "li"),
  SidebarSeparator: h.mk("SidebarSeparator", "hr"),
  SidebarTrigger: h.mk("SidebarTrigger", "button"),
  useSidebar: () => h.state.sidebarCtx,
}));

// Test files import "./Sidebar" after this harness, so these mocks are in place
// before the module under test loads.

// ─────────────────────────────────────────────────────────────────────────────
// Fixtures
// ─────────────────────────────────────────────────────────────────────────────

const ENV_MAIN = EnvironmentId.make("env-main");
const ENV_REMOTE = EnvironmentId.make("env-remote");
const ENV_WSL = EnvironmentId.make("env-wsl");

const NOW = Date.parse("2026-07-06T12:00:00.000Z");
const iso = (minutesAgo: number) => new Date(NOW - minutesAgo * 60_000).toISOString();

function makeProject(
  id: string,
  overrides: Partial<Omit<EnvironmentProject, "id">> = {},
): EnvironmentProject {
  return {
    id: ProjectId.make(id),
    title: "Repo A",
    workspaceRoot: "C:/repo-a",
    repositoryIdentity: null,
    defaultModelSelection: null,
    scripts: [],
    worktreeDiscovery: { visibility: "hidden", initialPromptDismissedAt: null, baselinePaths: [] },
    createdAt: iso(600),
    updatedAt: iso(60),
    environmentId: ENV_MAIN,
    ...overrides,
  };
}

function makeThread(
  id: string,
  overrides: Partial<Omit<EnvironmentThreadShell, "id">> = {},
): EnvironmentThreadShell {
  return {
    id: ThreadId.make(id),
    projectId: ProjectId.make("project-a"),
    title: `Thread ${id}`,
    modelSelection: createModelSelection(ProviderInstanceId.make("codex"), "gpt-5-codex"),
    runtimeMode: "full-access",
    interactionMode: "default",
    branch: null,
    worktreePath: null,
    latestTurn: null,
    createdAt: iso(500),
    updatedAt: iso(50),
    archivedAt: null,
    session: null,
    latestUserMessageAt: iso(40),
    hasPendingApprovals: false,
    hasPendingUserInput: false,
    hasActionableProposedPlan: false,
    environmentId: ENV_MAIN,
    ...overrides,
  };
}

function environmentFixture(overrides: {
  environmentId: EnvironmentId;
  label?: string | null;
  connectionId?: string;
  primary?: boolean;
  displayUrl?: string | null;
  phase?: string;
  error?: string | null;
  serverConfig?: unknown;
}) {
  return {
    environmentId: overrides.environmentId,
    label: overrides.label ?? null,
    entry: {
      target: overrides.primary
        ? { _tag: "PrimaryConnectionTarget" }
        : {
            _tag: "BearerConnectionTarget",
            connectionId: overrides.connectionId ?? "plain",
          },
    },
    displayUrl: overrides.displayUrl ?? null,
    connection: { phase: overrides.phase ?? "connected", error: overrides.error ?? null },
    serverConfig: overrides.serverConfig ?? null,
  };
}

const threadKeyOf = (thread: EnvironmentThreadShell) =>
  scopedThreadKey(scopeThreadRef(thread.environmentId, thread.id));

function cardDataProps(thread: EnvironmentThreadShell) {
  const key = threadKeyOf(thread);
  const catalog = h.state.worktreeCatalogs.get(`${thread.environmentId}:${thread.projectId}`) as
    | { adoptedWorkspaces: Array<{ threadId: string }> }
    | undefined;
  return {
    isPinned: h.metaStore.getState().pinnedThreadKeys.includes(key),
    isUnread: h.metaStore.getState().unreadThreadKeys.includes(key),
    worktreeStatus: (catalog?.adoptedWorkspaces.find((row) => row.threadId === thread.id) ??
      null) as import("@coddy/contracts").VcsAdoptedWorktreeStatus | null,
    runningTerminalIds: h.state.runningTerminalIds as readonly string[],
    discoveredPorts: (h.state.discoveredPortsByThreadId[thread.id] ??
      []) as readonly import("@coddy/contracts").DiscoveredLocalServer[],
  };
}

vi.mock("./sidebar/useWorkspaceCardLookups", async () => {
  const indexes = await import("./sidebar/workspaceCardLookups");
  const { selectWorktreeCatalogCapabilityPolicy: policy } =
    await import("@coddy/client-runtime/state/worktrees");
  return {
    useWorkspaceCardLookups: (input: {
      project: import("../sidebarProjectGrouping").SidebarProjectSnapshot;
      serverConfigs: ReadonlyMap<
        import("@coddy/contracts").EnvironmentId,
        import("@coddy/contracts").ServerConfig
      >;
      active: boolean;
      threads: readonly EnvironmentThreadShell[];
      pinnedKeys: readonly string[];
      unreadKeys: readonly string[];
    }) => {
      h.spies.cardLookupPass(input);
      const adopted = input.active
        ? input.project.memberProjects.flatMap((member) => {
            const config = input.serverConfigs.get(member.environmentId);
            if (!config || policy(config.environment).catalogRpc !== "enabled") return [];
            h.state.discoveryCatalogSubscriptions.push({
              environmentId: member.environmentId,
              input: { projectId: member.id },
            });
            const catalog = h.state.worktreeCatalogs.get(`${member.environmentId}:${member.id}`);
            return [
              { environmentId: member.environmentId, rows: catalog?.adoptedWorkspaces ?? [] },
            ];
          })
        : [];
      return {
        keys: indexes.buildWorkspaceCardKeys(input.threads, input.pinnedKeys, input.unreadKeys),
        adopted: indexes.buildAdoptedWorkspaceMap(adopted),
        terminals: new Map(
          input.threads.map((thread) => [
            threadKeyOf(thread),
            cardDataProps(thread).runningTerminalIds,
          ]),
        ),
        ports: new Map(
          input.threads.map((thread) => [
            threadKeyOf(thread),
            cardDataProps(thread).discoveredPorts,
          ]),
        ),
      };
    },
  };
});

// ─────────────────────────────────────────────────────────────────────────────
// Render + capture helpers
// ─────────────────────────────────────────────────────────────────────────────

function render(element: React.ReactElement): string {
  h.state.captures.length = 0;
  return renderToStaticMarkup(element);
}

interface Captured {
  readonly name: string;
  readonly props: Record<string, unknown>;
}

function captured(name: string): Captured[] {
  return (h.state.captures as Captured[]).filter((entry) => entry.name === name);
}

function collectElements(node: unknown, out: React.ReactElement[]): void {
  if (node === null || node === undefined) return;
  if (typeof node !== "object") return;
  if (Array.isArray(node)) {
    for (const child of node) collectElements(child, out);
    return;
  }
  if (React.isValidElement(node)) {
    out.push(node);
    const props = node.props as Record<string, unknown>;
    // Element-valued props too: cards pass slots such as `trailing`,
    // `renameInput` and `footer` to presentational components.
    for (const value of Object.values(props)) {
      collectElements(value, out);
    }
  }
}

type PropsPredicate = (props: Record<string, unknown>) => boolean;

function findProps(predicate: PropsPredicate): Record<string, unknown> | null {
  for (const entry of h.state.captures as Captured[]) {
    if (predicate(entry.props)) return entry.props;
    const elements: React.ReactElement[] = [];
    collectElements(entry.props["children"], elements);
    collectElements(entry.props["render"], elements);
    for (const element of elements) {
      const props = element.props as Record<string, unknown>;
      if (predicate(props)) return props;
    }
  }
  return null;
}

function mustFindProps(predicate: PropsPredicate, label: string): Record<string, unknown> {
  const props = findProps(predicate);
  if (!props) throw new Error(`Could not find captured element: ${label}`);
  return props;
}

const byTestId =
  (id: string): PropsPredicate =>
  (props) =>
    props["data-testid"] === id;
const byAriaLabel =
  (label: string): PropsPredicate =>
  (props) =>
    props["aria-label"] === label;

function invoke<TEvent>(props: Record<string, unknown>, handler: string, event: TEvent): void {
  const fn = props[handler];
  if (typeof fn !== "function") {
    throw new Error(`Captured element has no ${handler} handler`);
  }
  fn(event);
}

function mouseEvent(overrides: Record<string, unknown> = {}): React.MouseEvent<HTMLButtonElement> {
  return {
    preventDefault: vi.fn(),
    stopPropagation: vi.fn(),
    metaKey: false,
    ctrlKey: false,
    shiftKey: false,
    altKey: false,
    detail: 1,
    button: 0,
    clientX: 11,
    clientY: 22,
    target: { closest: () => null },
    currentTarget: { contains: () => false },
    ...overrides,
  } as unknown as React.MouseEvent<HTMLButtonElement>;
}

function keyboardEvent(
  key: string,
  overrides: Record<string, unknown> = {},
): React.KeyboardEvent<HTMLButtonElement> {
  return {
    key,
    preventDefault: vi.fn(),
    stopPropagation: vi.fn(),
    ...overrides,
  } as unknown as React.KeyboardEvent<HTMLButtonElement>;
}

const flush = async () => {
  for (let i = 0; i < 6; i += 1) {
    await new Promise<void>((resolve) => {
      setTimeout(resolve, 0);
    });
  }
};

function failureResult(message: string) {
  return { _tag: "Failure", cause: Cause.fail(new Error(message)) };
}

// ─────────────────────────────────────────────────────────────────────────────
// Scenario setup
// ─────────────────────────────────────────────────────────────────────────────

const projectA = makeProject("project-a");

const threadDefault = makeThread("thread-default", {
  kind: "default",
  title: "Repo A",
});
const threadActive = makeThread("thread-active", {
  title: "Active worktree",
  branch: "feat/x",
  worktreePath: "C:/wt/x",
  session: {
    threadId: ThreadId.make("thread-active"),
    status: "running",
    providerName: "Claude Code",
    activeTurnId: null,
    lastError: null,
    updatedAt: iso(90),
    runtimeMode: "full-access",
  },
  latestUserMessageAt: iso(5),
});
const threadIdle = makeThread("thread-idle", { title: "Idle thread" });
const threadPanel = makeThread("thread-panel", { kind: "panel", title: "Panel thread" });
const threadArchived = makeThread("thread-archived", {
  archivedAt: iso(10),
  title: "Archived thread",
});

function baseScenario() {
  h.state.projects = [projectA];
  h.state.threads = [threadDefault, threadActive, threadIdle, threadPanel, threadArchived];
  h.state.environments = [
    environmentFixture({ environmentId: ENV_MAIN, label: "Main", connectionId: "primary" }),
  ];
  h.state.primaryEnvironmentId = ENV_MAIN;
  h.state.routeParams = { environmentId: ENV_MAIN, threadId: "thread-active" };
  h.state.vcsStatusByCwd = {
    "C:/repo-a": { refName: "main" },
    "C:/wt/x": { refName: "feat/x", pr: { url: "https://example.com/pr/1" } },
  };
}

const groupedRepoIdentity = {
  canonicalKey: "github.com/acme/repo-a",
  locator: {
    source: "git-remote" as const,
    remoteName: "origin",
    remoteUrl: "https://github.com/acme/repo-a.git",
  },
  rootPath: "C:/repo-a",
  displayName: "Repo A",
  name: "repo-a",
};

function groupedScenario() {
  const localMember = makeProject("project-a", {
    repositoryIdentity: groupedRepoIdentity,
  });
  const remoteMember = makeProject("project-a-remote", {
    workspaceRoot: "C:/remote/repo-a",
    repositoryIdentity: { ...groupedRepoIdentity, rootPath: "C:/remote/repo-a" },
    environmentId: ENV_REMOTE,
  });
  const remoteThread = makeThread("thread-remote", {
    projectId: ProjectId.make("project-a-remote"),
    environmentId: ENV_REMOTE,
    title: "Remote thread",
  });
  h.state.projects = [localMember, remoteMember];
  h.state.threads = [threadDefault, threadIdle, remoteThread];
  h.state.environments = [
    environmentFixture({ environmentId: ENV_MAIN, label: "Main", connectionId: "primary" }),
    environmentFixture({
      environmentId: ENV_REMOTE,
      label: "Remote Box",
      connectionId: "remote",
    }),
  ];
  return { remoteThread };
}

function fakeLocalApi() {
  const api = {
    contextMenu: { show: h.spies.contextMenuShow },
    dialogs: { confirm: h.spies.dialogConfirm },
  };
  h.state.localApi = api;
  return api;
}

beforeEach(() => {
  vi.clearAllMocks();
  h.state.React = React;
  h.uiStore.reset();
  h.selectionStore.reset();
  h.metaStore.reset();
  h.state.captures.length = 0;
  h.state.projects = [];
  h.state.threads = [];
  h.state.environments = [];
  h.state.remoteUpdateRuns.clear();
  h.state.remoteUpdateSnapshots.clear();
  h.state.remoteUpdateSnapshotQueries = [];
  h.state.requestRemoteUpdate.mockReset();
  h.state.primaryEnvironmentId = ENV_MAIN;
  h.state.activeEnvironmentId = null;
  h.state.serverConfigs = new Map();
  h.state.clientSettings = { ...DEFAULT_CLIENT_SETTINGS };
  h.state.atomValues = {
    primaryServerConfig: {
      availableEditors: ["vscode"],
      environment: { serverVersion: "0.1.0" },
    },
    primaryServerKeybindings: {},
  };
  h.state.vcsStatusByCwd = {};
  h.state.vcsQueries = [];
  h.state.worktreeCatalogs = new Map();
  h.state.discoveryCatalogSubscriptions = [];
  h.state.discoveryFocusRefreshCalls = [];
  h.state.runningTerminalIds = [];
  h.state.discoveredPortsByThreadId = {};
  h.state.desktopBootstraps = [];
  h.state.desktopUpdateState = null;
  h.state.shellSummary = {
    catalogReady: false,
    desiredEnvironmentCount: 0,
    statuses: [],
    canShowEmptyProjects: false,
    hasSnapshot: false,
    hasSynchronizingShell: false,
    hasCachedShell: false,
    hasLiveShell: false,
    firstError: null,
    latestSnapshotUpdatedAt: null,
  };
  h.state.updateBtnDisabled = false;
  h.state.updateBtnAction = "none";
  h.state.showArmWarning = false;
  h.state.isMobile = false;
  h.state.sidebarCtx = { isMobile: false, setOpenMobile: h.spies.setOpenMobile };
  h.state.pathname = "/";
  h.state.routeParams = {};
  h.state.localApi = null;
  h.state.isDesktopHost = true;
  h.state.copyShouldFail = false;
  h.state.openDiscoveredPortResult = { _tag: "Success", value: undefined };
  h.state.commandResults = {};
  h.state.commandCalls = [];
  h.state.shortcutLabels = {};
  h.state.shortcutLabelOptions = [];
  h.state.showJumpHintModifiers = false;
  h.state.terminalSurfaceOpen = false;
  h.state.presentationPolicy = createEnvironmentPresentationPolicy({
    surface: "browser",
    platform: "unknown",
  });
  h.spies.contextMenuShow.mockResolvedValue(null);
  h.spies.dialogConfirm.mockResolvedValue(true);
  h.spies.toastAdd.mockReturnValue("toast-1");
  h.spies.archiveThread.mockResolvedValue({ _tag: "Success", value: undefined });
  h.spies.deleteThread.mockResolvedValue({ _tag: "Success", value: undefined });
  h.spies.completeWorktreeRemoval.mockResolvedValue({ _tag: "Success", value: undefined });
  h.spies.getDraftThreadByProjectRef.mockReturnValue(null);
  h.spies.openDiscoveredPort.mockImplementation(async () => h.state.openDiscoveredPortResult);
  h.spies.pointerWithin.mockReturnValue([]);
  h.spies.closestCorners.mockReturnValue([]);
  h.spies.windowConfirm.mockReturnValue(true);

  if (!browserRuntime) {
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
      callback(0);
      return 0;
    });
    vi.stubGlobal("document", { activeElement: null, querySelector: () => null });
    vi.stubGlobal("window", {
      setTimeout: (callback: () => void) => {
        callback();
        return 0;
      },
      clearTimeout: () => {},
      confirm: h.spies.windowConfirm,
      desktopBridge: undefined,
      addEventListener: () => {},
      removeEventListener: () => {},
    });
  }
});

afterEach(() => {
  vi.unstubAllGlobals();
});

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

export {
  ENV_MAIN,
  ENV_REMOTE,
  ENV_WSL,
  NOW,
  baseScenario,
  browserRuntime,
  byAriaLabel,
  byTestId,
  captured,
  cardDataProps,
  collectElements,
  environmentFixture,
  failureResult,
  fakeLocalApi,
  findProps,
  flush,
  groupedRepoIdentity,
  groupedScenario,
  h,
  invoke,
  iso,
  keyboardEvent,
  makeProject,
  makeThread,
  mouseEvent,
  mustFindProps,
  projectA,
  render,
  staticDescribe,
  threadActive,
  threadArchived,
  threadDefault,
  threadIdle,
  threadKeyOf,
  threadPanel,
};
export type { Captured, PropsPredicate };
