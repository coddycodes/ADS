import { attachmentAdmissionOwner } from "../state/attachmentAdmissions";
// @vitest-environment happy-dom

/**
 * Render-level tests for ChatView.
 *
 * ChatView is a very large route component; these tests render it through
 * `renderToStaticMarkup` (no DOM, per web test conventions) with the heavy
 * state/atom modules and child components replaced by prop-capturing mocks.
 * Real zustand stores (composer drafts and right/center panels) are
 * seeded directly so the component's derivation pipeline runs against
 * realistic state. Handler props captured from mocked children are then
 * invoked to exercise the send/interrupt/approval command flows.
 */
import {
  act,
  isValidElement,
  StrictMode,
  type ComponentProps,
  type ReactElement,
  type ReactNode,
  type RefObject,
  useEffect,
  useState,
  useSyncExternalStore,
} from "react";
import {
  type ConnectionTarget,
  PrimaryConnectionTarget,
  SshConnectionTarget,
} from "@bibcode/client-runtime/connection";
import { createRoot, type Root } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import {
  ApprovalRequestId,
  ActivityError,
  type ActivityActorSummary,
  type ActivityEntry,
  ActivityRecordId,
  type ActivityScopeRef,
  type ActivitySnapshot,
  EnvironmentId,
  MessageId,
  type ModelSelection,
  ProjectId,
  ProviderDriverKind,
  ProviderInstanceId,
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
import * as Option from "effect/Option";
import {
  scopedThreadKey,
  scopeProjectRef,
  scopeThreadRef,
} from "@bibcode/client-runtime/environment";

const h = vi.hoisted(() => {
  return {
    captured: {} as Record<string, unknown>,
    atomValuesByKey: new Map<string, unknown>(),
    commandCalls: [] as Array<{ key: string; input: unknown }>,
    commandHandlers: new Map<string, (input: unknown) => Promise<unknown>>(),
    commandOptions: [] as Array<{ key: string; options: unknown }>,
    commandResults: {} as Record<string, (input: unknown) => unknown>,
    defaultCommandResult: (() => undefined) as (input?: unknown) => unknown,
    environments: [] as unknown[],
    primaryEnvironment: null as unknown,
    threadsByKey: new Map<string, unknown>(),
    projectsByKey: new Map<string, unknown>(),
    allProjects: [] as unknown[],
    threadRefs: [] as unknown[],
    knownSessions: [] as unknown[],
    runningTerminalIds: [] as string[],
    queryDataByKey: new Map<string, unknown>(),
    queryEmissionsByKey: new Map<string, unknown>(),
    queryRefreshCalls: [] as string[],
    queryRevalidationCalls: [] as string[],
    queryRetryCalls: [] as string[],
    queryAwaitingRetry: new Set<string>(),
    querySubscriptionStarts: [] as string[],
    querySubscriptionStops: [] as string[],
    activeQuerySubscriptions: new Map<string, number>(),
    chatComposerProps: [] as Array<Record<string, unknown>>,
    assetUrls: [] as string[],
    previewSupported: false,
    previewState: {} as Record<string, unknown>,
    settings: {} as Record<string, unknown>,
    navigateCalls: [] as unknown[],
    presentationSurface: {
      surface: "browser" as "browser" | "desktop",
      platform: "linux" as "macos" | "windows" | "linux" | "unknown",
    },
    releasedTerminalInputs: [] as Array<{
      environmentId: string;
      threadId: string;
      terminalId: string;
    }>,
    filePreviewRevealEvents: [] as Array<{
      relativePath: unknown;
      revealRequestId: unknown;
    }>,
    filePreviewCommentActions: [] as Array<{
      kind: "submit" | "remove";
      composerDraftTarget: unknown;
      entryId: string;
    }>,
    compactDesktop: false,
    compactActivityDock: false,
    activityStateTargets: [] as unknown[],
    activityAtomTargets: [] as Array<{ kind: string; target: unknown }>,
    atomRefreshCalls: [] as string[],
    scopedProjectKeyCalls: [] as unknown[],
    activityDockRenderProps: [] as unknown[],
    activityPanelRenderProps: [] as unknown[],
    environmentSettingsById: new Map<string, Record<string, unknown>>(),
    settingsListeners: new Set<() => void>(),
    centerHeaderDensityByGroupId: new Map<string, "expanded" | "compact">(),
    chatHeaderActionsInstanceCount: 0,
  };
});

// ── Heavy state/atom modules ─────────────────────────────────────────

vi.mock("../connection/currentEnvironmentPresentation", async () => {
  const { createEnvironmentPresentationPolicy } =
    await import("../connection/environmentPresentationPolicy");
  return {
    readCurrentEnvironmentPresentationPolicy: () =>
      createEnvironmentPresentationPolicy(h.presentationSurface),
  };
});

vi.mock("../state/use-atom-command", () => ({
  useAtomCommand: (command: { key?: string } | null | undefined, options?: unknown) => {
    const key = command && typeof command.key === "string" ? command.key : "unknown-command";
    h.commandOptions.push({ key, options });
    let handler = h.commandHandlers.get(key);
    if (!handler) {
      handler = (input: unknown) => {
        h.commandCalls.push({ key, input });
        const respond = h.commandResults[key] ?? h.defaultCommandResult;
        return Promise.resolve(respond(input));
      };
      h.commandHandlers.set(key, handler);
    }
    return handler;
  },
}));

vi.mock("../state/threads", () => ({
  threadEnvironment: {
    create: { key: "thread.create" },
    delete: { key: "thread.delete" },
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

vi.mock("../state/query", async () => {
  const AsyncResultModule = await import("effect/unstable/reactivity");
  const CauseModule = await import("effect/Cause");
  const ReactModule = await import("react");
  return {
    useEnvironmentQuery: (atom: { key?: string } | null) => {
      const key = atom && typeof atom.key === "string" ? atom.key : null;
      ReactModule.useEffect(() => {
        if (key === null) return;
        h.querySubscriptionStarts.push(key);
        h.activeQuerySubscriptions.set(key, (h.activeQuerySubscriptions.get(key) ?? 0) + 1);
        return () => {
          h.querySubscriptionStops.push(key);
          const remaining = (h.activeQuerySubscriptions.get(key) ?? 1) - 1;
          if (remaining === 0) h.activeQuerySubscriptions.delete(key);
          else h.activeQuerySubscriptions.set(key, remaining);
        };
      }, [key]);
      const emission = key === null ? undefined : h.queryEmissionsByKey.get(key);
      const result = emission as
        | {
            _tag?: string;
            value?: unknown;
            cause?: unknown;
          }
        | undefined;
      const data =
        result?._tag === "Success"
          ? result.value
          : key === null
            ? null
            : (h.queryDataByKey.get(key) ?? null);
      const error =
        result?._tag === "Failure" && result.cause
          ? String(CauseModule.squash(result.cause as Cause.Cause<unknown>))
          : null;
      return {
        data,
        emission: emission ?? AsyncResultModule.AsyncResult.initial(false),
        error,
        isPending: result?._tag === "Initial",
        refresh: () => {
          if (key === null) return;
          h.queryRetryCalls.push(key);
          h.queryAwaitingRetry.delete(key);
          h.queryRefreshCalls.push(key);
        },
        revalidate: () => {
          if (key === null) return;
          h.queryRevalidationCalls.push(key);
          h.queryRefreshCalls.push(key);
        },
        requiresRetry: key !== null && h.queryAwaitingRetry.has(key),
      };
    },
  };
});

vi.mock("../state/activity", () => {
  const key = (kind: string, target: unknown) => `${kind}:${JSON.stringify(target)}`;
  const atom = (kind: string, target: unknown) => {
    h.activityAtomTargets.push({ kind, target });
    return { key: key(kind, target) };
  };
  return {
    environmentActivity: {
      stateValueAtom: (target: unknown) => {
        h.activityStateTargets.push(target);
        return atom("activity-state", target);
      },
      stateAtom: (target: unknown) => atom("activity-state-source", target),
      roster: (target: unknown) => atom("activity-roster", target),
      detail: (target: unknown) => atom("activity-detail", target),
      cancelSubtree: { key: "activity.cancelSubtree" },
      retrySubtreeCancellation: { key: "activity.retrySubtreeCancellation" },
    },
  };
});

vi.mock("../state/entities", () => ({
  useProject: (ref: { environmentId: string; projectId: string } | null) =>
    ref ? (h.projectsByKey.get(`${ref.environmentId}:${ref.projectId}`) ?? null) : null,
  useProjects: () => h.allProjects,
  useThread: (ref: { environmentId: string; threadId: string } | null) =>
    ref ? (h.threadsByKey.get(`${ref.environmentId}:${ref.threadId}`) ?? null) : null,
  useThreadRefs: () => h.threadRefs,
  useThreadProposedPlans: () => [],
}));

vi.mock("../state/environments", () => ({
  useEnvironments: () => ({
    isReady: true,
    networkStatus: "online",
    environments: h.environments,
  }),
  usePrimaryEnvironment: () => h.primaryEnvironment,
}));

vi.mock("../state/terminalSessions", () => ({
  useKnownTerminalSessions: () => h.knownSessions,
  useThreadRunningTerminalIds: () => h.runningTerminalIds,
}));

vi.mock("../hooks/useSettings", () => ({
  useEnvironmentSettings: (
    environmentId: string,
    selector?: (settings: Record<string, unknown>) => unknown,
  ) => {
    const settings = useSyncExternalStore(
      (listener) => {
        h.settingsListeners.add(listener);
        return () => h.settingsListeners.delete(listener);
      },
      () => h.environmentSettingsById.get(environmentId) ?? h.settings,
      () => h.environmentSettingsById.get(environmentId) ?? h.settings,
    ) as Record<string, unknown>;
    return selector ? selector(settings) : settings;
  },
}));

vi.mock("../assets/assetUrls", () => ({
  useAssetUrls: () => h.assetUrls,
}));

vi.mock("../hooks/useTheme", () => ({
  useTheme: () => ({
    theme: "system" as const,
    resolvedTheme: "dark" as const,
    setTheme: () => undefined,
  }),
}));

vi.mock("../hooks/useMediaQuery", () => ({
  useMediaQuery: (query: string) =>
    query === "(max-width: 1199px)" ? h.compactActivityDock : h.compactDesktop,
  usePrefersReducedMotion: () => false,
}));

vi.mock("~/hooks/useLocalStorage", () => ({
  useLocalStorage: (_key: string, initialValue: unknown) => [initialValue, () => undefined],
}));

vi.mock("../previewStateStore", () => ({
  isPreviewSupportedInRuntime: () => h.previewSupported,
  canPreviewAnyUrlInRuntime: () => h.previewSupported,
  setActivePreviewTab: () => undefined,
  useThreadPreviewState: () => h.previewState,
}));

vi.mock("./preview/addBrowserSurface", () => ({
  addBrowserSurface: () => undefined,
}));

vi.mock("./preview/closePreviewSession", () => ({
  closePreviewSession: () => Promise.resolve(),
}));

vi.mock("./preview/previewActionBus", () => ({
  subscribePreviewAction: () => () => undefined,
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
    useAtomRefresh: (atom: unknown) => () => {
      const key = (atom as { key?: string } | null | undefined)?.key;
      if (typeof key === "string") h.atomRefreshCalls.push(key);
    },
  };
});

vi.mock("@bibcode/client-runtime/environment", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@bibcode/client-runtime/environment")>();
  return {
    ...actual,
    scopedProjectKey: (ref: unknown) => {
      h.scopedProjectKeyCalls.push(ref);
      return actual.scopedProjectKey(ref as Parameters<typeof actual.scopedProjectKey>[0]);
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
    h.captured["chatComposer"] = props;
    h.chatComposerProps.push(props);
    return <div data-mock="chat-composer" />;
  },
}));

vi.mock("./chat/MessagesTimeline", () => ({
  MessagesTimeline: (props: Record<string, unknown>) => {
    h.captured["messagesTimeline"] = props;
    return (
      <div
        data-mock="messages-timeline"
        data-entry-count={String((props["timelineEntries"] as readonly unknown[]).length)}
      />
    );
  },
}));

vi.mock("./chat/ChatHeaderActions", () => ({
  ChatHeaderActions: (props: Record<string, unknown>) => {
    const [instanceId] = useState(() => ++h.chatHeaderActionsInstanceCount);
    h.captured["chatHeaderActions"] = props;
    return (
      <div
        data-mock="chat-header-actions"
        data-density={String(props["density"])}
        data-instance-id={instanceId}
      />
    );
  },
}));

vi.mock("./chat/ExpandedImageDialog", () => ({
  ExpandedImageDialog: () => <div data-mock="expanded-image-dialog" />,
}));

vi.mock("./chat/PanelLayoutControls", () => ({
  PanelLayoutControls: (props: Record<string, unknown>) => {
    h.captured["panelLayoutControls"] = props;
    return <div data-mock="panel-layout-controls" />;
  },
  RightPanelMaximizeControl: (props: Record<string, unknown>) => {
    h.captured["rightPanelMaximizeControl"] = props;
    return <div data-mock="right-panel-maximize" />;
  },
}));

vi.mock("./chat/ProviderStatusBanner", () => ({
  ProviderStatusBanner: (props: Record<string, unknown>) => {
    h.captured["providerStatusBanner"] = props;
    return <div data-mock="provider-status-banner" />;
  },
}));

vi.mock("./chat/ThreadErrorBanner", () => ({
  ThreadErrorBanner: (props: Record<string, unknown>) => {
    h.captured["threadErrorBanner"] = props;
    return (
      <div data-mock="thread-error-banner">
        {typeof props["error"] === "string" ? props["error"] : ""}
      </div>
    );
  },
}));

vi.mock("./chat/ComposerBannerStack", () => ({
  ComposerBannerStack: (props: Record<string, unknown>) => {
    h.captured["composerBannerStack"] = props;
    return <div data-mock="composer-banner-stack" />;
  },
}));

vi.mock("./PullRequestThreadDialog", () => ({
  PullRequestThreadDialog: (props: Record<string, unknown>) => {
    h.captured["pullRequestThreadDialog"] = props;
    return <div data-mock="pull-request-thread-dialog" />;
  },
}));

vi.mock("./PlanSidebar", () => ({
  default: (props: Record<string, unknown>) => {
    h.captured["planSidebar"] = props;
    return <div data-mock="plan-sidebar" />;
  },
}));

vi.mock("./ThreadTerminalPanel", () => ({
  default: (props: Record<string, unknown>) => {
    h.captured["threadTerminalPanel"] = props;
    return <div data-mock="thread-terminal-panel" data-owner={String(props["owner"] ?? "")} />;
  },
  enqueueTerminalInput: (input: {
    data: string;
    write: (data: string) => Promise<{ _tag: string; cause?: unknown }>;
    onWriteError?: (error: unknown) => void;
  }) => {
    void input.write(input.data).then((result) => {
      if (result._tag === "Failure") {
        input.onWriteError?.(result.cause);
      }
    });
  },
  releaseTerminalUiResources: (environmentId: string, threadId: string, terminalId: string) => {
    h.releasedTerminalInputs.push({ environmentId, threadId, terminalId });
  },
}));

vi.mock("./CenterPanelWorkspace", () => ({
  CenterPanelWorkspace: (props: Record<string, unknown>) => {
    h.captured["centerWorkspace"] = props;
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
  },
}));

vi.mock("./CenterTerminalPanel", () => ({
  CenterTerminalPanel: (props: Record<string, unknown>) => {
    h.captured["centerTerminalPanel"] = props;
    return <div data-mock="center-terminal-panel" />;
  },
}));

vi.mock("./RightPanelTabs", () => ({
  RightPanelTabs: (props: Record<string, unknown> & { children?: ReactNode }) => {
    h.captured["rightPanelTabs"] = props;
    return <div data-mock="right-panel-tabs">{props.children}</div>;
  },
}));

vi.mock("../browser/DesktopPreviewTabHosts", () => ({
  DesktopPreviewTabHosts: (props: Record<string, unknown>) => {
    h.captured["desktopPreviewTabHosts"] = props;
    return <div data-mock="desktop-preview-tab-hosts" />;
  },
}));

vi.mock("./RightPanelSheet", () => ({
  RightPanelSheet: (props: Record<string, unknown> & { children?: ReactNode }) => {
    h.captured["rightPanelSheet"] = props;
    return <div data-mock="right-panel-sheet">{props.children}</div>;
  },
}));

// Lazy-loaded panels: keep the imports trivial so Suspense fallbacks stay inert.
vi.mock("./preview/PreviewPanel", () => ({
  PreviewPanel: () => <div data-mock="preview-panel" />,
}));
vi.mock("./DiffPanel", () => ({
  default: () => <div data-mock="diff-panel" />,
}));
vi.mock("./SourceControlPanel", () => ({
  default: () => <div data-mock="source-control-panel" />,
}));
vi.mock("./files/FilePreviewPanel", () => {
  let nextInstanceId = 0;
  return {
    default: (props: Record<string, unknown>) => {
      const [instanceId] = useState(() => {
        nextInstanceId += 1;
        return nextInstanceId;
      });
      const [viewState, setViewState] = useState<{
        annotationEntryIds: string[];
        selectedRange: { start: number; end: number } | null;
      }>(() => ({ annotationEntryIds: [], selectedRange: null }));

      useEffect(() => {
        h.filePreviewRevealEvents.push({
          relativePath: props["relativePath"],
          revealRequestId: props["revealRequestId"],
        });
      }, [props["relativePath"], props["revealRequestId"]]);

      const recordCommentAction = (kind: "submit" | "remove", entryId: string): void => {
        if (!viewState.annotationEntryIds.includes(entryId)) return;
        h.filePreviewCommentActions.push({
          kind,
          composerDraftTarget: props["composerDraftTarget"],
          entryId,
        });
      };

      h.captured["filePreviewPanel"] = {
        ...props,
        mockView: {
          instanceId,
          viewState,
          setViewState,
          submitAnnotation: (entryId: string) => recordCommentAction("submit", entryId),
          removeAnnotation: (entryId: string) => recordCommentAction("remove", entryId),
        },
      };
      return <div data-mock="file-preview-panel" />;
    },
  };
});
vi.mock("./files/ProjectFilesPreloader", () => ({ ProjectFilesPreloader: () => null }));

vi.mock("./activity/ActivityDock", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./activity/ActivityDock")>();
  return {
    ...actual,
    ActivityDock: (props: ComponentProps<typeof actual.ActivityDock>) => {
      h.activityDockRenderProps.push(props);
      return <actual.ActivityDock {...props} />;
    },
  };
});

vi.mock("./activity/ActivityPanel", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./activity/ActivityPanel")>();
  return {
    ...actual,
    ActivityPanel: (props: ComponentProps<typeof actual.ActivityPanel>) => {
      h.activityPanelRenderProps.push(props);
      return <actual.ActivityPanel {...props} />;
    },
  };
});

import ChatView from "./ChatView";
import type { Project, Thread } from "../types";
import { useComposerDraftStore } from "../composerDraftStore";
import { useRightPanelStore } from "../rightPanelStore";
import { HOST_SURFACE_ID, useCenterPanelStore } from "../centerPanelStore";
import { useUiStateStore } from "../uiStateStore";
import { useDiffPanelStore } from "../diffPanelStore";
import { useActivityDockStore } from "../activityDockStore";
import { newDraftId } from "../lib/utils";
import type { ProviderInstanceEntry } from "../providerInstances";
import type { ChatComposerHandle } from "./chat/ChatComposer";
import type { ComposerBannerStackItem } from "./chat/ComposerBannerStack";
import { FileEditingSessionRegistry } from "./files/fileEditingSessionRegistry";
import type {
  ActivityDetailPageData,
  ActivityPanelProps,
  ActivityRosterPageData,
} from "./activity/ActivityPanel";

const environmentId = EnvironmentId.make("environment-local");
const projectId = ProjectId.make("project-1");
const threadId = ThreadId.make("thread-1");
const now = "2026-03-29T00:00:00.000Z";
const threadRef = scopeThreadRef(environmentId, threadId);
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

interface TestConnectionPresentation {
  readonly phase: "available" | "offline" | "connecting" | "reconnecting" | "connected" | "error";
  readonly error: string | null;
  readonly traceId: string | null;
}

interface TestEnvironmentPresentation {
  readonly environmentId: EnvironmentId;
  readonly label: string;
  readonly entry?: { readonly target: ConnectionTarget };
  readonly displayUrl: string | null;
  readonly relayManaged: boolean;
  readonly connection: TestConnectionPresentation;
  readonly serverConfig: {
    readonly providers: ReadonlyArray<ServerProvider>;
    readonly environment: {
      readonly label: string;
      readonly serverVersion?: string;
      readonly platform?: { readonly os: "linux"; readonly arch: "x64" };
      readonly capabilities?: {
        readonly activityProtocolVersion: 2 | null;
        readonly worktreeCatalog?: boolean;
      };
    };
  } | null;
}

function makeEnvironmentPresentation(
  overrides: Partial<TestEnvironmentPresentation> = {},
): TestEnvironmentPresentation {
  const defaultServerConfig = {
    providers: [codexProvider],
    environment: {
      label: "Local",
      platform: { os: "linux" as const, arch: "x64" as const },
      capabilities: { activityProtocolVersion: 2 as const, worktreeCatalog: true },
    },
  };
  return {
    environmentId,
    label: "Local",
    displayUrl: null,
    relayManaged: false,
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

/** The first element in `node`'s tree whose children are exactly `text`. */
function findElementByText(node: ReactNode, text: string): ReactElement | null {
  if (Array.isArray(node)) {
    for (const child of node) {
      const found = findElementByText(child, text);
      if (found) return found;
    }
    return null;
  }
  if (!isValidElement(node)) return null;
  const children = (node.props as { children?: ReactNode }).children;
  if (children === text) return node;
  return findElementByText(children, text);
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

function renderServerRoute(): string {
  return renderToStaticMarkup(
    <ChatView
      environmentId={environmentId}
      threadId={threadId}
      routeKind="server"
      reserveTitleBarControlInset
    />,
  );
}

function capturedProps<T>(name: string): T {
  const props = h.captured[name];
  expect(props, `expected captured props for ${name}`).toBeDefined();
  return props as T;
}

function deferredResult<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

function fakeEditingSession(relativePath: string) {
  return {
    relativePath,
    editor: { history: [] as string[] },
    flush: vi.fn(async () => "saved" as const),
    settle: vi.fn<() => Promise<"saved" | "failed">>(async () => "saved"),
    setAutosaveEnabled: vi.fn(),
    pauseSaving: vi.fn(),
    resumeSaving: vi.fn(),
    discardPendingSave: vi.fn(),
    rename: vi.fn(function rename(this: { relativePath: string }, next: string) {
      this.relativePath = next;
    }),
    dispose: vi.fn(),
  };
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
  useActivityDockStore,
].map((store) => ({
  store: store as unknown as ResettableStore,
  pristine: { ...(store as unknown as ResettableStore).getInitialState() },
}));

/**
 * renderToStaticMarkup reads zustand state through `getInitialState()` (the
 * server snapshot), so seeded state written with regular actions must be
 * copied into the initial-state object before rendering.
 */
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
  h.commandHandlers.clear();
  h.commandOptions.length = 0;
  h.commandResults = {
    "thread.stageAttachments": () => AsyncResult.success({ _tag: "inline" }),
    "thread.attachmentAdmissionAuthority": () =>
      AsyncResult.success({ storageInstanceId: "test-store", hostIdentity: "test-host" }),
  };
  h.defaultCommandResult = () => AsyncResult.success(undefined);
  h.environments = [];
  h.primaryEnvironment = null;
  h.threadsByKey.clear();
  h.projectsByKey.clear();
  h.allProjects = [];
  h.threadRefs = [];
  h.knownSessions = [];
  h.runningTerminalIds = [];
  h.queryDataByKey.clear();
  h.queryEmissionsByKey.clear();
  h.queryRefreshCalls = [];
  h.queryRevalidationCalls = [];
  h.queryRetryCalls = [];
  h.queryAwaitingRetry.clear();
  h.querySubscriptionStarts = [];
  h.querySubscriptionStops = [];
  h.activeQuerySubscriptions.clear();
  h.chatComposerProps = [];
  h.assetUrls = [];
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
  h.presentationSurface = { surface: "browser", platform: "linux" };
  h.releasedTerminalInputs = [];
  h.filePreviewRevealEvents = [];
  h.filePreviewCommentActions = [];
  h.compactDesktop = false;
  h.compactActivityDock = false;
  h.activityStateTargets = [];
  h.activityAtomTargets = [];
  h.atomRefreshCalls = [];
  h.scopedProjectKeyCalls = [];
  h.activityDockRenderProps = [];
  h.activityPanelRenderProps = [];
  h.environmentSettingsById.clear();
  h.settingsListeners.clear();
  h.centerHeaderDensityByGroupId.clear();
  h.chatHeaderActionsInstanceCount = 0;

  for (const { store, pristine } of resettableStores) {
    store.setState({ ...pristine }, true);
    Object.assign(store.getInitialState(), pristine);
  }

  vi.stubGlobal("window", {
    requestAnimationFrame: (callback: (time: number) => void) => {
      callback(0);
      return 0;
    },
    addEventListener: () => undefined,
    removeEventListener: () => undefined,
    dispatchEvent: () => true,
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
  delete (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT;
});

describe("ChatView", () => {
  describe("when: current-chat activity is available", () => {
    const actor = (
      id: string,
      name = id,
      overrides: Partial<ActivityActorSummary> = {},
    ): ActivityActorSummary =>
      ({
        _tag: "actor",
        id,
        name,
        status: "running",
        summary: `Summary for ${name}`,
        startedAt: "2026-07-22T20:00:00.000Z",
        updatedAt: "2026-07-22T20:10:00.000Z",
        terminalAt: null,
        parentActorId: null,
        role: "reviewer",
        providerType: "codex",
        ...overrides,
      }) as unknown as ActivityActorSummary;

    const activitySnapshot = (
      scope: ActivityScopeRef,
      actors: readonly ActivityActorSummary[],
      overrides: Partial<ActivitySnapshot> = {},
    ): ActivitySnapshot =>
      ({
        protocolVersion: 2,
        scopeId: `scope-${scope.threadId}`,
        scope,
        revision: 1,
        provider: "codex",
        providerInstanceId: null,
        capabilities: {
          actors: true,
          attributedActivity: true,
          backgroundWork: true,
          historyRecovery: "full",
          terminalObservation: false,
          targetedActorCancellation: false,
        },
        observationState: "live",
        sections: {
          subagents: { state: "live", message: null, retryable: false },
          backgroundTasks: { state: "live", message: null, retryable: false },
        },
        counts: {
          subagents: { active: actors.length, done: 0 },
          backgroundTasks: { active: 0, done: 0 },
        },
        actors,
        workItems: [],
        actorsHasMore: false,
        workItemsHasMore: false,
        control: {
          scopeId: `scope-${scope.threadId}`,
          revision: 0,
          actors: [],
          operations: [],
        },
        updatedAt: "2026-07-22T20:10:00.000Z",
        ...overrides,
      }) as unknown as ActivitySnapshot;

    const activityKey = (kind: string, target: unknown): string =>
      `${kind}:${JSON.stringify(target)}`;

    const seedActivityState = (
      nextEnvironmentId: EnvironmentId,
      scope: ActivityScopeRef,
      snapshot: ActivitySnapshot | null,
      status: "empty" | "synchronizing" | "live" | "stale" = "live",
      error: string | null = null,
      recentEntries: ReadonlyMap<ActivityRecordId, ReadonlyArray<ActivityEntry>> = new Map(),
    ): void => {
      h.atomValuesByKey.set(
        activityKey("activity-state", { environmentId: nextEnvironmentId, input: scope }),
        {
          snapshot: snapshot === null ? Option.none() : Option.some(snapshot),
          status,
          error: error === null ? Option.none() : Option.some(error),
          recentEntries,
        },
      );
    };

    const seedRoster = (
      nextEnvironmentId: EnvironmentId,
      snapshot: ActivitySnapshot,
      section: "subagents" | "backgroundTasks",
      bucket: "active" | "done",
      records: ActivityRosterPageData["records"],
      nextCursor: string | null = null,
      cursor?: string,
      actorControls: ActivityRosterPageData["actorControls"] = [],
    ): void => {
      h.queryDataByKey.set(
        activityKey("activity-roster", {
          environmentId: nextEnvironmentId,
          input: {
            scope: snapshot.scope,
            scopeId: snapshot.scopeId,
            section,
            bucket,
            ...(cursor === undefined ? {} : { cursor }),
            limit: 200,
          },
        }),
        { records, actorControls, nextCursor } satisfies ActivityRosterPageData,
      );
    };

    const seedDetail = (
      nextEnvironmentId: EnvironmentId,
      snapshot: ActivitySnapshot,
      record: ActivityActorSummary,
      nextCursor: string | null = null,
      cursor?: string,
    ): void => {
      h.queryDataByKey.set(
        activityKey("activity-detail", {
          environmentId: nextEnvironmentId,
          input: {
            scope: snapshot.scope,
            scopeId: snapshot.scopeId,
            recordKind: "actor",
            recordId: record.id,
            ...(cursor === undefined ? {} : { cursor }),
            limit: 200,
          },
        }),
        {
          record,
          entries: [
            {
              id: `entry-${record.id}`,
              ownerKind: "actor",
              ownerId: record.id,
              kind: "commentary",
              title: `Detail for ${record.name}`,
              detail: "Working independently.",
              tone: "info",
              createdAt: "2026-07-22T20:05:00.000Z",
            },
          ],
          nextCursor,
        } as unknown as ActivityDetailPageData,
      );
    };

    const seedActivityQueries = (
      nextEnvironmentId: EnvironmentId,
      snapshot: ActivitySnapshot,
      actors: readonly ActivityActorSummary[],
    ): void => {
      seedRoster(nextEnvironmentId, snapshot, "subagents", "active", actors);
      seedRoster(nextEnvironmentId, snapshot, "subagents", "done", []);
      for (const currentActor of actors) {
        seedDetail(nextEnvironmentId, snapshot, currentActor);
      }
    };

    const click = async (target: Element): Promise<void> => {
      await act(async () => {
        target.dispatchEvent(new MouseEvent("click", { bubbles: true }));
        await Promise.resolve();
      });
    };

    const publishSettingsUpdated = async (
      nextEnvironmentId: EnvironmentId,
      patch: {
        readonly enableChatAgentActivity?: boolean;
        readonly enableTerminalAgentActivity?: boolean;
      },
    ): Promise<void> => {
      const current =
        h.environmentSettingsById.get(nextEnvironmentId) ?? (h.settings as Record<string, unknown>);
      h.environmentSettingsById.set(nextEnvironmentId, {
        ...current,
        ...patch,
      });
      await act(async () => {
        for (const listener of h.settingsListeners) listener();
        await Promise.resolve();
      });
    };

    const mountActivityRoute = async (
      nextThreadId: ThreadId = threadId,
    ): Promise<{ container: HTMLDivElement; root: Root }> => {
      vi.unstubAllGlobals();
      Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
      Object.defineProperty(Element.prototype, "getAnimations", {
        configurable: true,
        value: () => [],
      });
      const container = document.createElement("div");
      document.body.append(container);
      const root = createRoot(container);
      await act(async () => {
        root.render(
          <ChatView
            environmentId={environmentId}
            threadId={nextThreadId}
            routeKind="server"
            reserveTitleBarControlInset
          />,
        );
        await Promise.resolve();
      });
      return { container, root };
    };

    const mountActivityPanel = async (
      panelThreadRef: ReturnType<typeof scopeThreadRef> = threadRef,
    ): Promise<{ container: HTMLDivElement; root: Root }> => {
      vi.unstubAllGlobals();
      Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
      Object.defineProperty(Element.prototype, "getAnimations", {
        configurable: true,
        value: () => [],
      });
      const container = document.createElement("div");
      document.body.append(container);
      const root = createRoot(container);
      await act(async () => {
        root.render(
          <ChatView variant="panel" panelThreadRef={panelThreadRef} workspaceUnavailable={null} />,
        );
        await Promise.resolve();
      });
      return { container, root };
    };

    const openSubagents = async (container: HTMLDivElement): Promise<void> => {
      const expand = container.querySelector<HTMLButtonElement>(
        'button[aria-label^="Expand activity summary"]',
      );
      expect(expand).not.toBeNull();
      await click(expand!);
      const subagents = container.querySelector<HTMLButtonElement>(
        '[data-activity-section="subagents"]',
      );
      expect(subagents).not.toBeNull();
      await click(subagents!);
      await vi.waitFor(() => {
        expect(container.querySelector("[data-activity-panel]")).not.toBeNull();
      });
    };

    const latestActivityPanelProps = (): ActivityPanelProps => {
      const props = h.activityPanelRenderProps.at(-1);
      expect(props).toBeDefined();
      return props as ActivityPanelProps;
    };

    it("binds server-fenced cancel and residual retry commands without reporting raw failures", async () => {
      const child = actor("actor-cancellable", "Cancellable reviewer");
      const snapshot = activitySnapshot({ _tag: "thread", threadId }, [child], {
        capabilities: {
          actors: true,
          attributedActivity: true,
          backgroundWork: true,
          historyRecovery: "full",
          terminalObservation: false,
          targetedActorCancellation: true,
        },
        control: {
          scopeId: `scope-${threadId}` as ActivitySnapshot["scopeId"],
          revision: 19,
          actors: [
            {
              actorId: child.id,
              state: "available",
              controlRevision: 11,
              activeDescendantCount: 2,
            },
          ],
          operations: [
            {
              rootActorId: child.id,
              state: "partial",
              residualCount: 2,
              message: "Some agents are still running.",
              operationRevision: 17,
            },
          ],
        },
      });
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      seedActivityState(environmentId, snapshot.scope, snapshot);
      seedRoster(environmentId, snapshot, "subagents", "active", [child], null, undefined, [
        snapshot.control.actors[0]!,
      ]);
      seedRoster(environmentId, snapshot, "subagents", "done", []);

      const { container, root } = await mountActivityRoute();
      try {
        await openSubagents(container);
        const panel = latestActivityPanelProps();
        await act(async () => panel.onCancelActor?.(child.id, 11));
        await act(async () => panel.onRetryCancellation?.(child.id, 17));

        expect(h.commandCalls.filter((call) => call.key.startsWith("activity."))).toEqual([
          {
            key: "activity.cancelSubtree",
            input: {
              environmentId,
              input: {
                scope: snapshot.scope,
                scopeId: snapshot.scopeId,
                actorId: child.id,
                expectedControlRevision: 11,
              },
            },
          },
          {
            key: "activity.retrySubtreeCancellation",
            input: {
              environmentId,
              input: {
                scope: snapshot.scope,
                scopeId: snapshot.scopeId,
                rootActorId: child.id,
                expectedOperationRevision: 17,
              },
            },
          },
        ]);
        expect(h.commandOptions.filter((entry) => entry.key.startsWith("activity."))).toEqual(
          expect.arrayContaining([
            { key: "activity.cancelSubtree", options: { reportFailure: false } },
            {
              key: "activity.retrySubtreeCancellation",
              options: { reportFailure: false },
            },
          ]),
        );
      } finally {
        await act(async () => root.unmount());
        container.remove();
      }
    });

    it("maps typed cancellation failures to bounded copy without leaking native payloads", async () => {
      const child = actor("actor-provider-down", "Provider-down reviewer");
      const snapshot = activitySnapshot({ _tag: "thread", threadId }, [child], {
        capabilities: {
          actors: true,
          attributedActivity: true,
          backgroundWork: true,
          historyRecovery: "full",
          terminalObservation: false,
          targetedActorCancellation: true,
        },
      });
      const nativePayload = "claude-task-id=secret-native-42 provider socket exploded";
      h.commandResults["activity.cancelSubtree"] = () =>
        AsyncResult.failure(
          Cause.fail(new ActivityError({ reason: "providerUnavailable", message: nativePayload })),
        );
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      seedActivityState(environmentId, snapshot.scope, snapshot);
      seedActivityQueries(environmentId, snapshot, [child]);

      const { container, root } = await mountActivityRoute();
      try {
        await openSubagents(container);
        await act(async () => latestActivityPanelProps().onCancelActor?.(child.id, 5));
        await vi.waitFor(() => {
          expect(latestActivityPanelProps().cancellationError).toBe(
            "The provider is unavailable. Try again when it reconnects.",
          );
        });
        expect(container.textContent).not.toContain(nativePayload);
        expect(container.textContent).not.toContain("secret-native-42");
      } finally {
        await act(async () => root.unmount());
        container.remove();
      }
    });

    it("ignores a late cancellation failure after the Activity target changes environments", async () => {
      const child = actor("actor-old-target", "Old target reviewer");
      const oldSnapshot = activitySnapshot({ _tag: "thread", threadId }, [child], {
        scopeId: "colliding-scope" as ActivitySnapshot["scopeId"],
        capabilities: {
          actors: true,
          attributedActivity: true,
          backgroundWork: true,
          historyRecovery: "full",
          terminalObservation: false,
          targetedActorCancellation: true,
        },
      });
      const oldFailure = deferredResult<ReturnType<typeof AsyncResult.failure>>();
      h.commandResults["activity.cancelSubtree"] = () => oldFailure.promise;
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      seedActivityState(environmentId, oldSnapshot.scope, oldSnapshot);
      seedActivityQueries(environmentId, oldSnapshot, [child]);

      const { container, root } = await mountActivityRoute();
      try {
        await openSubagents(container);
        void latestActivityPanelProps().onCancelActor?.(child.id, 5);

        const nextEnvironmentId = EnvironmentId.make("environment-remote");
        const nextThreadRef = scopeThreadRef(nextEnvironmentId, threadId);
        const nextChild = actor("actor-new-target", "New target reviewer");
        const nextSnapshot = activitySnapshot({ _tag: "thread", threadId }, [nextChild], {
          scopeId: "colliding-scope" as ActivitySnapshot["scopeId"],
          capabilities: oldSnapshot.capabilities,
        });
        seedEnvironment(makeEnvironmentPresentation({ environmentId: nextEnvironmentId }));
        seedProject(makeProject({ environmentId: nextEnvironmentId }));
        seedServerThread(makeThread({ environmentId: nextEnvironmentId }));
        seedActivityState(nextEnvironmentId, nextSnapshot.scope, nextSnapshot);
        seedActivityQueries(nextEnvironmentId, nextSnapshot, [nextChild]);
        useRightPanelStore.getState().openActivity(nextThreadRef, "subagents", nextSnapshot.scope);
        await act(async () => {
          root.render(
            <ChatView
              environmentId={nextEnvironmentId}
              threadId={threadId}
              routeKind="server"
              reserveTitleBarControlInset
            />,
          );
          await Promise.resolve();
        });
        await vi.waitFor(() =>
          expect((latestActivityPanelProps().snapshot as ActivitySnapshot).scopeId).toBe(
            nextSnapshot.scopeId,
          ),
        );

        await act(async () => {
          oldFailure.resolve(
            AsyncResult.failure(
              Cause.fail(
                new ActivityError({
                  reason: "providerUnavailable",
                  message: "secret old environment payload",
                }),
              ),
            ),
          );
          await Promise.resolve();
        });
        expect(latestActivityPanelProps().cancellationError).toBeNull();
        expect(container.textContent).not.toContain("secret old environment payload");
      } finally {
        await act(async () => root.unmount());
        container.remove();
      }
    });

    it("does not resurrect a rendered cancellation failure after leaving and returning to its target", async () => {
      const child = actor("actor-return-target", "Returning reviewer");
      const snapshot = activitySnapshot({ _tag: "thread", threadId }, [child], {
        scopeId: "reused-scope" as ActivitySnapshot["scopeId"],
        capabilities: {
          actors: true,
          attributedActivity: true,
          backgroundWork: true,
          historyRecovery: "full",
          terminalObservation: false,
          targetedActorCancellation: true,
        },
      });
      h.commandResults["activity.cancelSubtree"] = () =>
        AsyncResult.failure(
          Cause.fail(
            new ActivityError({
              reason: "providerUnavailable",
              message: "secret returning-target payload",
            }),
          ),
        );
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      seedActivityState(environmentId, snapshot.scope, snapshot);
      seedActivityQueries(environmentId, snapshot, [child]);

      const { container, root } = await mountActivityRoute();
      try {
        await openSubagents(container);
        await act(async () => latestActivityPanelProps().onCancelActor?.(child.id, 5));
        await vi.waitFor(() =>
          expect(latestActivityPanelProps().cancellationError).toBe(
            "The provider is unavailable. Try again when it reconnects.",
          ),
        );
        expect(container.textContent).not.toContain("secret returning-target payload");

        const nextEnvironmentId = EnvironmentId.make("environment-away");
        const nextThreadRef = scopeThreadRef(nextEnvironmentId, threadId);
        const nextChild = actor("actor-away-target", "Away reviewer");
        const nextSnapshot = activitySnapshot({ _tag: "thread", threadId }, [nextChild], {
          scopeId: snapshot.scopeId,
          capabilities: snapshot.capabilities,
          control: {
            ...snapshot.control,
            actors: [
              {
                actorId: nextChild.id,
                state: "available",
                controlRevision: 5,
                activeDescendantCount: 0,
              },
            ],
          },
        });
        seedEnvironment(makeEnvironmentPresentation({ environmentId: nextEnvironmentId }));
        seedProject(makeProject({ environmentId: nextEnvironmentId }));
        seedServerThread(makeThread({ environmentId: nextEnvironmentId }));
        seedActivityState(nextEnvironmentId, nextSnapshot.scope, nextSnapshot);
        seedActivityQueries(nextEnvironmentId, nextSnapshot, [nextChild]);
        useRightPanelStore.getState().openActivity(nextThreadRef, "subagents", nextSnapshot.scope);
        await act(async () => {
          root.render(
            <ChatView
              environmentId={nextEnvironmentId}
              threadId={threadId}
              routeKind="server"
              reserveTitleBarControlInset
            />,
          );
          await Promise.resolve();
        });
        await vi.waitFor(() =>
          expect((latestActivityPanelProps().snapshot as ActivitySnapshot).actors[0]?.id).toBe(
            nextChild.id,
          ),
        );
        expect(latestActivityPanelProps().cancellationError).toBeNull();

        useRightPanelStore.getState().openActivity(threadRef, "subagents", snapshot.scope);
        await act(async () => {
          root.render(
            <ChatView
              environmentId={environmentId}
              threadId={threadId}
              routeKind="server"
              reserveTitleBarControlInset
            />,
          );
          await Promise.resolve();
        });
        await vi.waitFor(() =>
          expect((latestActivityPanelProps().snapshot as ActivitySnapshot).actors[0]?.id).toBe(
            child.id,
          ),
        );

        expect(latestActivityPanelProps().snapshot.control.revision).toBe(
          snapshot.control.revision,
        );
        expect(latestActivityPanelProps().cancellationError).toBeNull();
        expect(container.textContent).not.toContain("secret returning-target payload");
      } finally {
        await act(async () => root.unmount());
        container.remove();
      }
    });

    it("lets the latest cancel or retry success or failure exclusively own the single banner", async () => {
      const child = actor("actor-command-order", "Ordered reviewer");
      const snapshot = activitySnapshot({ _tag: "thread", threadId }, [child], {
        capabilities: {
          actors: true,
          attributedActivity: true,
          backgroundWork: true,
          historyRecovery: "full",
          terminalObservation: false,
          targetedActorCancellation: true,
        },
      });
      const olderCancel = deferredResult<ReturnType<typeof AsyncResult.failure>>();
      const newerRetry = deferredResult<ReturnType<typeof AsyncResult.failure>>();
      h.commandResults["activity.cancelSubtree"] = () => olderCancel.promise;
      h.commandResults["activity.retrySubtreeCancellation"] = () => newerRetry.promise;
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      seedActivityState(environmentId, snapshot.scope, snapshot);
      seedActivityQueries(environmentId, snapshot, [child]);

      const { container, root } = await mountActivityRoute();
      try {
        await openSubagents(container);
        void latestActivityPanelProps().onCancelActor?.(child.id, 5);
        void latestActivityPanelProps().onRetryCancellation?.(child.id, 7);

        await act(async () => {
          newerRetry.resolve(
            AsyncResult.failure(
              Cause.fail(
                new ActivityError({
                  reason: "providerUnavailable",
                  message: "new secret payload",
                }),
              ),
            ),
          );
          await Promise.resolve();
        });
        await vi.waitFor(() =>
          expect(latestActivityPanelProps().cancellationError).toBe(
            "The provider is unavailable. Try again when it reconnects.",
          ),
        );

        await act(async () => {
          olderCancel.resolve(
            AsyncResult.failure(
              Cause.fail(
                new ActivityError({ reason: "dispatchTimeout", message: "old secret payload" }),
              ),
            ),
          );
          await Promise.resolve();
        });
        expect(latestActivityPanelProps().cancellationError).toBe(
          "The provider is unavailable. Try again when it reconnects.",
        );
        expect(container.textContent).not.toContain("new secret payload");
        expect(container.textContent).not.toContain("old secret payload");

        const secondOlderCancel = deferredResult<ReturnType<typeof AsyncResult.failure>>();
        const latestRetrySuccess = deferredResult<ReturnType<typeof AsyncResult.success>>();
        h.commandResults["activity.cancelSubtree"] = () => secondOlderCancel.promise;
        h.commandResults["activity.retrySubtreeCancellation"] = () => latestRetrySuccess.promise;
        void latestActivityPanelProps().onCancelActor?.(child.id, 5);
        void latestActivityPanelProps().onRetryCancellation?.(child.id, 7);
        await act(async () => {
          latestRetrySuccess.resolve(AsyncResult.success(undefined));
          await Promise.resolve();
        });
        await act(async () => {
          secondOlderCancel.resolve(
            AsyncResult.failure(
              Cause.fail(
                new ActivityError({
                  reason: "dispatchTimeout",
                  message: "superseded failure payload",
                }),
              ),
            ),
          );
          await Promise.resolve();
        });
        expect(latestActivityPanelProps().cancellationError).toBeNull();
        expect(container.textContent).not.toContain("superseded failure payload");
      } finally {
        await act(async () => root.unmount());
        container.remove();
      }
    });

    it.each(["requested", "partial"] as const)(
      "invalidates a pending failure when authoritative control state advances to %s",
      async (operationState) => {
        const child = actor("actor-authoritative", "Authoritative reviewer");
        const snapshot = activitySnapshot({ _tag: "thread", threadId }, [child], {
          capabilities: {
            actors: true,
            attributedActivity: true,
            backgroundWork: true,
            historyRecovery: "full",
            terminalObservation: false,
            targetedActorCancellation: true,
          },
          control: {
            scopeId: `scope-${threadId}` as ActivitySnapshot["scopeId"],
            revision: 3,
            actors: [
              {
                actorId: child.id,
                state: "available",
                controlRevision: 3,
                activeDescendantCount: 0,
              },
            ],
            operations: [],
          },
        });
        const pending = deferredResult<ReturnType<typeof AsyncResult.failure>>();
        h.commandResults["activity.cancelSubtree"] = () => pending.promise;
        seedEnvironment(makeEnvironmentPresentation());
        seedProject(makeProject());
        seedServerThread(makeThread());
        seedGitStatus(true);
        seedActivityState(environmentId, snapshot.scope, snapshot);
        seedActivityQueries(environmentId, snapshot, [child]);

        const { container, root } = await mountActivityRoute();
        try {
          await openSubagents(container);
          void latestActivityPanelProps().onCancelActor?.(child.id, 3);
          const authoritative = activitySnapshot(snapshot.scope, [child], {
            scopeId: snapshot.scopeId,
            revision: snapshot.revision,
            capabilities: snapshot.capabilities,
            control: {
              scopeId: snapshot.scopeId,
              revision: 4,
              actors: [
                {
                  actorId: child.id,
                  state: operationState === "requested" ? "requested" : "available",
                  controlRevision: 4,
                  activeDescendantCount: 0,
                },
              ],
              operations: [
                {
                  rootActorId: child.id,
                  state: operationState,
                  residualCount: 1,
                  message: null,
                  operationRevision: 4,
                },
              ],
            },
          });
          seedActivityState(environmentId, authoritative.scope, authoritative);
          await act(async () => {
            useRightPanelStore.getState().navigateActivity(threadRef, {
              section: "subagents",
              selectedRecordKind: "actor",
              selectedRecordId: child.id,
            });
            await Promise.resolve();
          });

          await act(async () => {
            pending.resolve(
              AsyncResult.failure(
                Cause.fail(
                  new ActivityError({ reason: "dispatchTimeout", message: "late secret payload" }),
                ),
              ),
            );
            await Promise.resolve();
          });
          expect(latestActivityPanelProps().snapshot.control.revision).toBe(4);
          expect(latestActivityPanelProps().cancellationError).toBeNull();
          expect(container.textContent).not.toContain("late secret payload");
        } finally {
          await act(async () => root.unmount());
          container.remove();
        }
      },
    );

    it("keeps provider-terminal Activity bindings read-only", async () => {
      const terminalScope: ActivityScopeRef = {
        _tag: "terminal",
        threadId,
        terminalId: "terminal-activity-1",
      };
      const child = actor("terminal-actor", "Terminal observer");
      const snapshot = activitySnapshot(terminalScope, [child], {
        capabilities: {
          actors: true,
          attributedActivity: true,
          backgroundWork: true,
          historyRecovery: "bounded",
          terminalObservation: true,
          targetedActorCancellation: true,
        },
      });
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      h.settings = { ...h.settings, enableTerminalAgentActivity: true };
      seedActivityState(environmentId, terminalScope, snapshot);
      seedActivityQueries(environmentId, snapshot, [child]);
      useRightPanelStore.getState().openActivity(threadRef, "subagents", terminalScope);

      const { container, root } = await mountActivityRoute();
      try {
        await vi.waitFor(() =>
          expect(container.querySelector("[data-activity-panel]")).not.toBeNull(),
        );
        expect(latestActivityPanelProps().onCancelActor).toBeUndefined();
        expect(latestActivityPanelProps().onRetryCancellation).toBeUndefined();
      } finally {
        await act(async () => root.unmount());
        container.remove();
      }
    });

    it("removes activity UI and its persisted surface on settingsUpdated, then restores the dock", async () => {
      const child = actor("actor-settings-toggle", "Settings toggle reviewer");
      const snapshot = activitySnapshot({ _tag: "thread", threadId }, [child]);
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      seedActivityState(environmentId, snapshot.scope, snapshot);
      seedActivityQueries(environmentId, snapshot, [child]);

      const { container, root } = await mountActivityRoute();
      try {
        await openSubagents(container);
        const chatTimeline = container.querySelector('[data-mock="messages-timeline"]');
        expect(chatTimeline).not.toBeNull();
        expect(container.querySelector('[data-testid="activity-dock"]')).not.toBeNull();
        expect(container.querySelector("[data-activity-panel]")).not.toBeNull();

        await publishSettingsUpdated(environmentId, { enableChatAgentActivity: false });

        await vi.waitFor(() => {
          expect(container.querySelector('[data-testid="activity-dock"]')).toBeNull();
          expect(container.querySelector("[data-activity-panel]")).toBeNull();
          expect(
            useRightPanelStore
              .getState()
              .byThreadKey[scopedThreadKey(threadRef)]?.surfaces.some(
                (surface) => surface.kind === "activity",
              ) ?? false,
          ).toBe(false);
          expect(container.querySelector('[data-mock="messages-timeline"]')).toBe(chatTimeline);
        });

        await publishSettingsUpdated(environmentId, { enableChatAgentActivity: true });

        await vi.waitFor(() => {
          expect(container.querySelector('[data-testid="activity-dock"]')).not.toBeNull();
          expect(container.querySelector('[data-mock="messages-timeline"]')).toBe(chatTimeline);
        });
      } finally {
        await act(async () => root.unmount());
        container.remove();
      }
    });

    it("removes a stale Activity surface at disabled startup without creating an activity RPC consumer", async () => {
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      h.environmentSettingsById.set(environmentId, {
        ...(h.settings as Record<string, unknown>),
        enableChatAgentActivity: false,
      });
      useRightPanelStore.getState().openActivity(threadRef, "subagents", { _tag: "thread" });

      const { container, root } = await mountActivityRoute();
      try {
        await vi.waitFor(() => {
          expect(
            useRightPanelStore
              .getState()
              .byThreadKey[scopedThreadKey(threadRef)]?.surfaces.some(
                (surface) => surface.kind === "activity",
              ) ?? false,
          ).toBe(false);
        });
        expect(container.querySelector('[data-testid="activity-dock"]')).toBeNull();
        expect(container.querySelector("[data-activity-panel]")).toBeNull();
        expect(h.activityStateTargets).toEqual([]);
        expect(h.activityAtomTargets).toEqual([]);
      } finally {
        await act(async () => root.unmount());
        container.remove();
      }
    });

    it("closes an Activity surface opened after its source is already disabled", async () => {
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      h.environmentSettingsById.set(environmentId, {
        ...(h.settings as Record<string, unknown>),
        enableChatAgentActivity: false,
        enableTerminalAgentActivity: true,
      });

      const { container, root } = await mountActivityRoute();
      try {
        expect(
          useRightPanelStore
            .getState()
            .byThreadKey[scopedThreadKey(threadRef)]?.surfaces.some(
              (surface) => surface.kind === "activity",
            ) ?? false,
        ).toBe(false);

        await act(async () => {
          useRightPanelStore.getState().openActivity(threadRef, "subagents", { _tag: "thread" });
          await Promise.resolve();
        });

        await vi.waitFor(() => {
          expect(
            useRightPanelStore
              .getState()
              .byThreadKey[scopedThreadKey(threadRef)]?.surfaces.some(
                (surface) => surface.kind === "activity",
              ) ?? false,
          ).toBe(false);
        });
        expect(container.querySelector("[data-activity-panel]")).toBeNull();
      } finally {
        await act(async () => root.unmount());
        container.remove();
      }
    });

    it("closes an inactive persisted Activity surface using its own disabled scope", async () => {
      const terminalId = "terminal-inactive-activity";
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      h.environmentSettingsById.set(environmentId, {
        ...(h.settings as Record<string, unknown>),
        enableChatAgentActivity: true,
        enableTerminalAgentActivity: true,
      });
      useRightPanelStore
        .getState()
        .openActivity(threadRef, "subagents", { _tag: "terminal", terminalId });
      useRightPanelStore.getState().open(threadRef, "plan");

      const { container, root } = await mountActivityRoute();
      try {
        const initialRightPanelState =
          useRightPanelStore.getState().byThreadKey[scopedThreadKey(threadRef)];
        expect(initialRightPanelState?.activeSurfaceId).toBe("plan");
        expect(initialRightPanelState?.surfaces).toEqual(
          expect.arrayContaining([
            expect.objectContaining({
              kind: "activity",
              scope: { _tag: "terminal", terminalId },
            }),
            expect.objectContaining({ kind: "plan" }),
          ]),
        );
        expect(container.querySelector("[data-activity-panel]")).toBeNull();

        await publishSettingsUpdated(environmentId, { enableTerminalAgentActivity: false });

        await vi.waitFor(() => {
          const nextRightPanelState =
            useRightPanelStore.getState().byThreadKey[scopedThreadKey(threadRef)];
          expect(
            nextRightPanelState?.surfaces.some((surface) => surface.kind === "activity") ?? false,
          ).toBe(false);
          expect(nextRightPanelState?.surfaces).toContainEqual(
            expect.objectContaining({ kind: "plan" }),
          );
          expect(nextRightPanelState?.activeSurfaceId).toBe("plan");
        });
      } finally {
        await act(async () => root.unmount());
        container.remove();
      }
    });

    it.each([
      {
        name: "closes a thread surface when only chat activity is disabled",
        surfaceScope: { _tag: "thread" } as const,
        snapshotScope: { _tag: "thread", threadId } as ActivityScopeRef,
        nextSettings: { enableChatAgentActivity: false, enableTerminalAgentActivity: true },
        expectedOpen: false,
      },
      {
        name: "keeps a terminal surface when only chat activity is disabled",
        surfaceScope: { _tag: "terminal", terminalId: "terminal-isolation" } as const,
        snapshotScope: {
          _tag: "terminal",
          threadId,
          terminalId: "terminal-isolation",
        } as ActivityScopeRef,
        nextSettings: { enableChatAgentActivity: false, enableTerminalAgentActivity: true },
        expectedOpen: true,
      },
      {
        name: "keeps a thread surface when only terminal activity is disabled",
        surfaceScope: { _tag: "thread" } as const,
        snapshotScope: { _tag: "thread", threadId } as ActivityScopeRef,
        nextSettings: { enableChatAgentActivity: true, enableTerminalAgentActivity: false },
        expectedOpen: true,
      },
      {
        name: "closes a terminal surface when only terminal activity is disabled",
        surfaceScope: { _tag: "terminal", terminalId: "terminal-isolation" } as const,
        snapshotScope: {
          _tag: "terminal",
          threadId,
          terminalId: "terminal-isolation",
        } as ActivityScopeRef,
        nextSettings: { enableChatAgentActivity: true, enableTerminalAgentActivity: false },
        expectedOpen: false,
      },
    ])("$name", async ({ surfaceScope, snapshotScope, nextSettings, expectedOpen }) => {
      const snapshot = activitySnapshot(snapshotScope, []);
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      seedActivityState(environmentId, snapshotScope, snapshot);
      seedActivityQueries(environmentId, snapshot, []);
      h.environmentSettingsById.set(environmentId, {
        ...(h.settings as Record<string, unknown>),
        enableChatAgentActivity: true,
        enableTerminalAgentActivity: true,
      });
      useRightPanelStore.getState().openActivity(threadRef, "subagents", surfaceScope);

      const { container, root } = await mountActivityRoute();
      try {
        await vi.waitFor(() => {
          expect(container.querySelector("[data-activity-panel]")).not.toBeNull();
        });

        await publishSettingsUpdated(environmentId, nextSettings);

        await vi.waitFor(() => {
          const activitySurface = useRightPanelStore
            .getState()
            .byThreadKey[scopedThreadKey(threadRef)]?.surfaces.find(
              (surface) => surface.kind === "activity",
            );
          expect(activitySurface !== undefined).toBe(expectedOpen);
          expect(container.querySelector("[data-activity-panel]") !== null).toBe(expectedOpen);
        });
      } finally {
        await act(async () => root.unmount());
        container.remove();
      }
    });

    it("does not render a dock for a regular unsupported provider chat", () => {
      const unsupported = activitySnapshot({ _tag: "thread", threadId }, [], {
        provider: ProviderDriverKind.make("unsupported"),
        capabilities: {
          actors: false,
          attributedActivity: false,
          backgroundWork: false,
          historyRecovery: "none",
          terminalObservation: false,
          targetedActorCancellation: false,
        },
        sections: {
          subagents: {
            state: "unsupported",
            message: "This provider does not expose subagent activity.",
            retryable: false,
          },
          backgroundTasks: {
            state: "unsupported",
            message: "This provider does not expose background activity.",
            retryable: false,
          },
        },
        counts: {
          subagents: { active: 0, done: 0 },
          backgroundTasks: { active: 0, done: 0 },
        },
      });
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      seedActivityState(environmentId, unsupported.scope, unsupported);

      expect(renderServerRoute()).not.toContain('data-testid="activity-dock"');
      expect(
        Option.isSome(
          (
            h.atomValuesByKey.get(
              activityKey("activity-state", {
                environmentId,
                input: unsupported.scope,
              }),
            ) as { snapshot: Option.Option<ActivitySnapshot> }
          ).snapshot,
        ),
      ).toBe(true);
    });

    it("enables an incremental Codex fixture from snapshot capabilities without provider-name gating", () => {
      const codex = activitySnapshot({ _tag: "thread", threadId }, [actor("actor-codex")], {
        provider: ProviderDriverKind.make("codex"),
        capabilities: {
          actors: true,
          attributedActivity: true,
          backgroundWork: false,
          historyRecovery: "full",
          terminalObservation: false,
          targetedActorCancellation: false,
        },
        sections: {
          subagents: { state: "live", message: null, retryable: false },
          backgroundTasks: { state: "unsupported", message: null, retryable: false },
        },
      });
      const unsupportedFor = (provider: "claude" | "opencode"): ActivitySnapshot =>
        activitySnapshot({ _tag: "thread", threadId }, [], {
          provider: ProviderDriverKind.make(provider),
          capabilities: {
            actors: false,
            attributedActivity: false,
            backgroundWork: false,
            historyRecovery: "none",
            terminalObservation: false,
            targetedActorCancellation: false,
          },
          sections: {
            subagents: { state: "unsupported", message: null, retryable: false },
            backgroundTasks: { state: "unsupported", message: null, retryable: false },
          },
          counts: {
            subagents: { active: 0, done: 0 },
            backgroundTasks: { active: 0, done: 0 },
          },
        });
      seedEnvironment(
        makeEnvironmentPresentation({
          serverConfig: {
            providers: [codexProvider],
            environment: {
              label: "Local",
              capabilities: { activityProtocolVersion: 2 },
            },
          },
        }),
      );
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);

      seedActivityState(environmentId, codex.scope, codex);
      expect(renderServerRoute()).toContain('data-testid="activity-dock"');
      expect(
        (h.activityDockRenderProps.at(-1) as { snapshot: ActivitySnapshot }).snapshot.capabilities
          .terminalObservation,
      ).toBe(false);

      for (const provider of ["claude", "opencode"] as const) {
        const unsupported = unsupportedFor(provider);
        seedActivityState(environmentId, unsupported.scope, unsupported);
        expect(renderServerRoute()).not.toContain('data-testid="activity-dock"');
      }
    });

    it("marks only Background Tasks stale when its section fails independently", () => {
      const child = actor("actor-live", "Live reviewer");
      const snapshot = activitySnapshot({ _tag: "thread", threadId }, [child], {
        observationState: "live",
        sections: {
          subagents: { state: "live", message: null, retryable: false },
          backgroundTasks: {
            state: "stale",
            message: "Background observation failed.",
            retryable: true,
          },
        },
        counts: {
          subagents: { active: 1, done: 0 },
          backgroundTasks: { active: 1, done: 0 },
        },
        workItems: [
          {
            _tag: "workItem",
            id: ActivityRecordId.make("work-background"),
            name: "Background indexing",
            status: "running",
            summary: "Indexing",
            startedAt: "2026-07-22T20:00:00.000Z",
            updatedAt: "2026-07-22T20:10:00.000Z",
            terminalAt: null,
            ownerActorId: null,
            workKind: "index",
            command: null,
            cwd: null,
          },
        ],
      });
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      seedActivityState(environmentId, snapshot.scope, snapshot);
      useActivityDockStore.getState().setExpanded(`${environmentId}:${projectId}`, true);
      publishSeededStoreState(useActivityDockStore);

      const markup = renderServerRoute();

      expect(markup).not.toContain('aria-label="Activity data stale"');
      expect(markup.match(/data-activity-section-status="stale"/g)).toHaveLength(1);
      expect(markup).toContain(
        'aria-label="Open Background tasks: 1 active, 0 done. Status: stale"',
      );
      expect(markup).toContain('aria-label="Open Subagents: 1 active, 0 done"');
      expect(markup).not.toContain('aria-label="Open Subagents: 1 active, 0 done. Status: stale"');
    });

    it("retains a failed snapshot and retries the source state atom", async () => {
      const child = actor("actor-error", "Retained reviewer");
      const snapshot = activitySnapshot({ _tag: "thread", threadId }, [child]);
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      seedActivityState(
        environmentId,
        snapshot.scope,
        snapshot,
        "stale",
        "Activity synchronization failed.",
      );
      seedActivityQueries(environmentId, snapshot, [child]);

      const { container, root } = await mountActivityRoute();
      try {
        await openSubagents(container);
        await vi.waitFor(() => {
          expect(container.textContent).toContain(
            "Activity updates failed. Showing the last known activity.",
          );
          expect(container.textContent).toContain("Retained reviewer");
        });
        const retry = [...container.querySelectorAll("button")].find(
          (button) => button.textContent === "Retry",
        );
        expect(retry).toBeDefined();
        await click(retry!);
        expect(h.atomRefreshCalls).toContain(
          activityKey("activity-state-source", {
            environmentId,
            input: snapshot.scope,
          }),
        );
        expect(h.atomRefreshCalls).not.toContain(
          activityKey("activity-state", {
            environmentId,
            input: snapshot.scope,
          }),
        );
      } finally {
        await act(async () => root.unmount());
        container.remove();
      }
    });

    it("opens the singleton roster and detail without changing the chat route, then closes only the surface", async () => {
      const child = actor("actor-child", "Child reviewer");
      const snapshot = activitySnapshot({ _tag: "thread", threadId }, [child]);
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      seedActivityState(environmentId, snapshot.scope, snapshot);
      seedActivityQueries(environmentId, snapshot, [child]);

      const { container, root } = await mountActivityRoute();
      try {
        expect(container.querySelector('[data-testid="activity-dock"]')).not.toBeNull();
        await openSubagents(container);
        await vi.waitFor(() => expect(container.textContent).toContain("Child reviewer"));
        expect(
          useRightPanelStore
            .getState()
            .byThreadKey[scopedThreadKey(threadRef)]?.surfaces.filter(
              (surface) => surface.kind === "activity",
            ),
        ).toHaveLength(1);

        const rosterItem = [...container.querySelectorAll("button")].find((button) =>
          button.textContent?.includes("Child reviewer"),
        );
        expect(rosterItem).toBeDefined();
        await click(rosterItem!);
        await vi.waitFor(() => {
          expect(container.querySelector("[data-activity-detail-heading]")?.textContent).toBe(
            "Child reviewer",
          );
        });
        expect(h.navigateCalls).toEqual([]);
        const detailFirstPageKey = activityKey("activity-detail", {
          environmentId,
          input: {
            scope: snapshot.scope,
            scopeId: snapshot.scopeId,
            recordKind: "actor",
            recordId: child.id,
            limit: 200,
          },
        });
        await vi.waitFor(() => expect(h.queryRefreshCalls).toContain(detailFirstPageKey));
        h.queryRefreshCalls = [];

        const back = container.querySelector<HTMLButtonElement>(
          'button[aria-label="Back to Subagents"]',
        );
        expect(back).not.toBeNull();
        await click(back!);
        const updatedSnapshot = activitySnapshot(snapshot.scope, [child], {
          revision: 2,
          updatedAt: "2026-07-22T20:12:00.000Z",
        });
        seedActivityState(environmentId, snapshot.scope, updatedSnapshot);
        await act(async () => {
          useRightPanelStore
            .getState()
            .openActivity(scopeThreadRef(environmentId, threadId), "subagents", snapshot.scope);
          await Promise.resolve();
        });
        await vi.waitFor(() => expect(h.queryRefreshCalls.length).toBeGreaterThan(0));
        h.queryRefreshCalls = [];
        const reselectedRosterItem = [...container.querySelectorAll("button")].find((button) =>
          button.textContent?.includes("Child reviewer"),
        );
        expect(reselectedRosterItem).toBeDefined();
        await click(reselectedRosterItem!);
        await vi.waitFor(() => expect(h.queryRefreshCalls).toContain(detailFirstPageKey));

        h.queryRefreshCalls = [];
        await act(async () =>
          latestActivityPanelProps().onNavigate({
            section: "backgroundTasks",
            selectedRecordKind: null,
            selectedRecordId: null,
          }),
        );
        const backgroundFirstPageKey = activityKey("activity-roster", {
          environmentId,
          input: {
            scope: snapshot.scope,
            scopeId: snapshot.scopeId,
            section: "backgroundTasks",
            bucket: "active",
            limit: 200,
          },
        });
        await vi.waitFor(() => expect(h.queryRefreshCalls).toContain(backgroundFirstPageKey));

        const surface = useRightPanelStore
          .getState()
          .byThreadKey[scopedThreadKey(threadRef)]?.surfaces.find(
            (candidate) => candidate.kind === "activity",
          );
        expect(surface).toBeDefined();
        await act(async () => {
          capturedProps<{ onCloseSurface: (surface: unknown) => void }>(
            "rightPanelTabs",
          ).onCloseSurface(surface);
        });
        expect(container.querySelector("[data-activity-panel]")).toBeNull();
        expect(container.querySelector('[data-testid="activity-dock"]')).not.toBeNull();
        expect(h.activityStateTargets.at(-1)).toEqual({
          environmentId,
          input: { _tag: "thread", threadId },
        });
      } finally {
        await act(async () => root.unmount());
        container.remove();
      }
    });

    it("refreshes failed active and done roster cursors without discarding loaded records", async () => {
      const activeActor = actor("actor-active", "Active reviewer");
      const doneActor = actor("actor-done", "Done reviewer", {
        status: "completed",
        terminalAt: "2026-07-22T20:09:00.000Z",
      });
      const snapshot = activitySnapshot({ _tag: "thread", threadId }, [activeActor, doneActor], {
        counts: {
          subagents: { active: 1, done: 1 },
          backgroundTasks: { active: 0, done: 0 },
        },
      });
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      seedActivityState(environmentId, snapshot.scope, snapshot);
      seedRoster(environmentId, snapshot, "subagents", "active", [activeActor], "active-2");
      seedRoster(environmentId, snapshot, "subagents", "done", [doneActor], "done-2");

      const failure = (message: string) =>
        AsyncResult.failure(Cause.fail(new ActivityError({ reason: "invalidCursor", message })));
      const activeCursorKey = activityKey("activity-roster", {
        environmentId,
        input: {
          scope: snapshot.scope,
          scopeId: snapshot.scopeId,
          section: "subagents",
          bucket: "active",
          cursor: "active-2",
          limit: 200,
        },
      });
      const doneCursorKey = activityKey("activity-roster", {
        environmentId,
        input: {
          scope: snapshot.scope,
          scopeId: snapshot.scopeId,
          section: "subagents",
          bucket: "done",
          cursor: "done-2",
          limit: 200,
        },
      });
      h.queryEmissionsByKey.set(activeCursorKey, failure("Active page failed."));
      h.queryEmissionsByKey.set(doneCursorKey, failure("Done page failed."));

      const { container, root } = await mountActivityRoute();
      try {
        await openSubagents(container);
        await vi.waitFor(() => {
          expect(container.textContent).toContain("Active reviewer");
          expect(container.textContent).toContain("Done reviewer");
        });
        await vi.waitFor(() => {
          expect(h.queryRefreshCalls).toContain(
            activityKey("activity-roster", {
              environmentId,
              input: {
                scope: snapshot.scope,
                scopeId: snapshot.scopeId,
                section: "subagents",
                bucket: "active",
                limit: 200,
              },
            }),
          );
          expect(h.queryRefreshCalls).toContain(
            activityKey("activity-roster", {
              environmentId,
              input: {
                scope: snapshot.scope,
                scopeId: snapshot.scopeId,
                section: "subagents",
                bucket: "done",
                limit: 200,
              },
            }),
          );
        });
        h.queryRefreshCalls = [];

        await act(async () => {
          latestActivityPanelProps().onLoadMoreRoster("active");
          await Promise.resolve();
        });
        await vi.waitFor(() => expect(container.textContent).toContain("Active page failed."));
        expect(container.textContent).toContain("Active reviewer");
        await act(async () => latestActivityPanelProps().onLoadMoreRoster("active"));
        expect(h.queryRefreshCalls).toContain(activeCursorKey);

        await act(async () => {
          latestActivityPanelProps().onLoadMoreRoster("done");
          await Promise.resolve();
        });
        await vi.waitFor(() => expect(container.textContent).toContain("Done page failed."));
        expect(container.textContent).toContain("Done reviewer");
        await act(async () => latestActivityPanelProps().onLoadMoreRoster("done"));
        expect(h.queryRefreshCalls).toContain(doneCursorKey);
      } finally {
        await act(async () => root.unmount());
        container.remove();
      }
    });

    it("refreshes a failed detail cursor and retains the loaded detail", async () => {
      const child = actor("actor-detail", "Detailed reviewer");
      const snapshot = activitySnapshot({ _tag: "thread", threadId }, [child]);
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      seedActivityState(environmentId, snapshot.scope, snapshot);
      seedActivityQueries(environmentId, snapshot, [child]);
      seedDetail(environmentId, snapshot, child, "detail-2");
      const detailCursorKey = activityKey("activity-detail", {
        environmentId,
        input: {
          scope: snapshot.scope,
          scopeId: snapshot.scopeId,
          recordKind: "actor",
          recordId: child.id,
          cursor: "detail-2",
          limit: 200,
        },
      });
      h.queryEmissionsByKey.set(
        detailCursorKey,
        AsyncResult.failure(Cause.fail(new Error("Detail page failed."))),
      );

      const { container, root } = await mountActivityRoute();
      try {
        await openSubagents(container);
        const rosterItem = [...container.querySelectorAll("button")].find((button) =>
          button.textContent?.includes("Detailed reviewer"),
        );
        expect(rosterItem).toBeDefined();
        await click(rosterItem!);
        await vi.waitFor(() => {
          expect(container.querySelector("[data-activity-detail-heading]")?.textContent).toBe(
            "Detailed reviewer",
          );
        });

        await act(async () => {
          latestActivityPanelProps().onLoadMoreDetail();
          await Promise.resolve();
        });
        await vi.waitFor(() => expect(container.textContent).toContain("Detail page failed."));
        expect(container.textContent).toContain("Detailed reviewer");
        expect(container.textContent).toContain("The last loaded entries remain available.");
        await act(async () => latestActivityPanelProps().onLoadMoreDetail());
        expect(h.queryRefreshCalls).toContain(detailCursorKey);
      } finally {
        await act(async () => root.unmount());
        container.remove();
      }
    });

    it("treats a typed not-found detail failure as record removal", async () => {
      const child = actor("actor-removed", "Removed reviewer");
      const snapshot = activitySnapshot({ _tag: "thread", threadId }, [child]);
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      seedActivityState(environmentId, snapshot.scope, snapshot);
      seedRoster(environmentId, snapshot, "subagents", "active", [child]);
      seedRoster(environmentId, snapshot, "subagents", "done", []);
      h.queryEmissionsByKey.set(
        activityKey("activity-detail", {
          environmentId,
          input: {
            scope: snapshot.scope,
            scopeId: snapshot.scopeId,
            recordKind: "actor",
            recordId: child.id,
            limit: 200,
          },
        }),
        AsyncResult.failure(
          Cause.fail(
            new ActivityError({
              reason: "notFound",
              message: "The selected record was removed.",
            }),
          ),
        ),
      );

      const { container, root } = await mountActivityRoute();
      try {
        await openSubagents(container);
        const rosterItem = [...container.querySelectorAll("button")].find((button) =>
          button.textContent?.includes("Removed reviewer"),
        );
        expect(rosterItem).toBeDefined();
        await click(rosterItem!);
        await vi.waitFor(() => {
          expect(container.textContent).toContain("This activity record is no longer available.");
        });
        expect(container.querySelector("[data-activity-detail-heading]")).toBeNull();
      } finally {
        await act(async () => root.unmount());
        container.remove();
      }
    });

    it("switches the durable subscription and never leaks the previous thread roster", async () => {
      const secondThreadId = ThreadId.make("thread-2");
      const firstActor = actor("actor-first", "First thread actor");
      const secondActor = actor("actor-second", "Second thread actor");
      const firstSnapshot = activitySnapshot({ _tag: "thread", threadId }, [firstActor]);
      const secondSnapshot = activitySnapshot({ _tag: "thread", threadId: secondThreadId }, [
        secondActor,
      ]);
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedServerThread(makeThread({ id: secondThreadId }));
      seedGitStatus(true);
      seedActivityState(environmentId, firstSnapshot.scope, firstSnapshot);
      seedActivityState(environmentId, secondSnapshot.scope, secondSnapshot);
      seedActivityQueries(environmentId, firstSnapshot, [firstActor]);
      seedActivityQueries(environmentId, secondSnapshot, [secondActor]);

      const { container, root } = await mountActivityRoute();
      try {
        await openSubagents(container);
        await vi.waitFor(() => expect(container.textContent).toContain("First thread actor"));

        await act(async () => {
          root.render(
            <ChatView
              environmentId={environmentId}
              threadId={secondThreadId}
              routeKind="server"
              reserveTitleBarControlInset
            />,
          );
          await Promise.resolve();
        });
        expect(container.textContent).not.toContain("First thread actor");
        await openSubagents(container);
        await vi.waitFor(() => expect(container.textContent).toContain("Second thread actor"));
        expect(container.textContent).not.toContain("First thread actor");
      } finally {
        await act(async () => root.unmount());
        container.remove();
      }
    });

    it("uses compact dock presentation independently from the narrower right-panel sheet", async () => {
      const child = actor("actor-compact-band", "Compact band reviewer");
      const snapshot = activitySnapshot({ _tag: "thread", threadId }, [child]);
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      seedActivityState(environmentId, snapshot.scope, snapshot);
      seedActivityQueries(environmentId, snapshot, [child]);
      h.compactDesktop = false;
      h.compactActivityDock = true;

      const { container, root } = await mountActivityRoute();
      try {
        const dockProps = h.activityDockRenderProps.at(-1) as {
          readonly avoidRightPanelSheet: boolean;
          readonly compact: boolean;
        };
        expect(dockProps.compact).toBe(true);
        expect(dockProps.avoidRightPanelSheet).toBe(false);
        await openSubagents(container);
        expect(container.querySelector('[data-mock="right-panel-sheet"]')).toBeNull();
        expect(capturedProps<{ mode: string }>("rightPanelTabs").mode).toBe("inline");
      } finally {
        await act(async () => root.unmount());
        container.remove();
      }
    });

    it("consumes the first sheet Escape by collapsing the project dock, then releases close", async () => {
      const child = actor("actor-sheet-escape", "Sheet escape reviewer");
      const snapshot = activitySnapshot({ _tag: "thread", threadId }, [child]);
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      seedActivityState(environmentId, snapshot.scope, snapshot);
      seedActivityQueries(environmentId, snapshot, [child]);
      h.compactDesktop = true;
      h.compactActivityDock = true;
      useRightPanelStore.getState().openActivity(threadRef, "subagents", { _tag: "thread" });
      useActivityDockStore.getState().setExpanded(`${environmentId}:${projectId}`, true);
      publishSeededStoreState(useRightPanelStore);
      publishSeededStoreState(useActivityDockStore);

      const { container, root } = await mountActivityRoute();
      try {
        expect(container.querySelector('[data-mock="right-panel-sheet"]')).not.toBeNull();
        expect(
          (
            h.activityDockRenderProps.at(-1) as {
              readonly avoidRightPanelSheet: boolean;
            }
          ).avoidRightPanelSheet,
        ).toBe(true);
        const sheet = capturedProps<{ consumeEscapeClose: () => boolean }>("rightPanelSheet");

        expect(sheet.consumeEscapeClose()).toBe(true);
        expect(
          useActivityDockStore.getState().expandedByProject[`${environmentId}:${projectId}`],
        ).toBe(false);
        expect(sheet.consumeEscapeClose()).toBe(false);
      } finally {
        await act(async () => root.unmount());
        container.remove();
      }
    });

    it("retains stale activity through reconnect and uses the existing compact sheet with Back", async () => {
      const child = actor("actor-compact", "Compact reviewer");
      const snapshot = activitySnapshot({ _tag: "thread", threadId }, [child]);
      seedEnvironment(
        makeEnvironmentPresentation({
          connection: { phase: "reconnecting", error: null, traceId: null },
        }),
      );
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      seedActivityState(environmentId, snapshot.scope, snapshot, "stale");
      seedActivityQueries(environmentId, snapshot, [child]);
      h.compactDesktop = true;
      h.compactActivityDock = true;

      const { container, root } = await mountActivityRoute();
      try {
        expect(container.querySelector('[aria-label="Activity data stale"]')).not.toBeNull();
        await openSubagents(container);
        expect(container.querySelector('[data-mock="right-panel-sheet"]')).not.toBeNull();
        expect(capturedProps<{ mode: string }>("rightPanelTabs").mode).toBe("sheet");
        await vi.waitFor(() => expect(container.textContent).toContain("Compact reviewer"));

        const rosterItem = [...container.querySelectorAll("button")].find((button) =>
          button.textContent?.includes("Compact reviewer"),
        );
        await click(rosterItem!);
        await vi.waitFor(() => {
          expect(container.querySelector("[data-activity-detail-heading]")).not.toBeNull();
        });
        const back = container.querySelector<HTMLButtonElement>(
          'button[aria-label="Back to Subagents"]',
        );
        expect(back).not.toBeNull();
        await click(back!);
        await vi.waitFor(() => {
          expect(container.querySelector("[data-activity-detail-heading]")).toBeNull();
          expect(container.textContent).toContain("Compact reviewer");
        });
        expect(container.querySelector('[data-mock="right-panel-sheet"]')).not.toBeNull();
      } finally {
        await act(async () => root.unmount());
        container.remove();
      }
    });

    it("keeps activity bindings stable while the active thread streams new messages", async () => {
      const child = actor("actor-stable", "Stable reviewer");
      const snapshot = activitySnapshot({ _tag: "thread", threadId }, [child]);
      const initialThread = makeThread();
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(initialThread);
      seedGitStatus(true);
      seedActivityState(environmentId, snapshot.scope, snapshot);
      seedActivityQueries(environmentId, snapshot, [child]);

      const { container, root } = await mountActivityRoute();
      try {
        await openSubagents(container);
        await vi.waitFor(() => expect(container.textContent).toContain("Stable reviewer"));
        const dockCount = h.activityDockRenderProps.length;
        const panelCount = h.activityPanelRenderProps.length;
        const dockProps = h.activityDockRenderProps.at(-1);
        const panelProps = h.activityPanelRenderProps.at(-1);
        const atomTargetCount = h.activityAtomTargets.length;
        const stateTargetCount = h.activityStateTargets.length;
        h.threadsByKey.set(`${environmentId}:${threadId}`, {
          ...initialThread,
          messages: [
            {
              id: MessageId.make("message-streaming"),
              role: "assistant",
              text: "Streaming update",
              turnId: null,
              createdAt: now,
              updatedAt: "2026-07-22T20:11:00.000Z",
              streaming: true,
            },
          ],
          updatedAt: "2026-07-22T20:11:00.000Z",
        });

        await act(async () => {
          root.render(
            <ChatView
              environmentId={environmentId}
              threadId={threadId}
              routeKind="server"
              reserveTitleBarControlInset
            />,
          );
          await Promise.resolve();
        });

        expect(h.activityDockRenderProps).toHaveLength(dockCount);
        expect(h.activityPanelRenderProps).toHaveLength(panelCount);
        expect(h.activityAtomTargets).toHaveLength(atomTargetCount);
        expect(h.activityStateTargets).toHaveLength(stateTargetCount);
        expect(h.activityDockRenderProps.at(-1)).toBe(dockProps);
        expect(h.activityPanelRenderProps.at(-1)).toBe(panelProps);
        expect(h.scopedProjectKeyCalls).toContainEqual(scopeProjectRef(environmentId, projectId));
        expect(
          capturedProps<{ activeThread: Thread }>("chatComposer").activeThread.messages,
        ).toHaveLength(1);
      } finally {
        await act(async () => root.unmount());
        container.remove();
      }
    });

    it("refreshes open activity queries from the first page when the snapshot revision advances", async () => {
      const child = actor("actor-live-inspector", "Live inspector");
      const snapshot = activitySnapshot({ _tag: "thread", threadId }, [child]);
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      seedActivityState(environmentId, snapshot.scope, snapshot);
      seedActivityQueries(environmentId, snapshot, [child]);

      const { container, root } = await mountActivityRoute();
      try {
        await openSubagents(container);
        await vi.waitFor(() =>
          expect(container.querySelector(`[data-activity-row="${child.id}"]`)).not.toBeNull(),
        );
        await click(container.querySelector(`[data-activity-row="${child.id}"]`)!);
        await vi.waitFor(() => expect(h.queryRefreshCalls.length).toBeGreaterThan(0));
        h.queryRefreshCalls = [];

        const completed = actor(child.id, child.name, {
          status: "completed",
          terminalAt: "2026-07-22T20:12:00.000Z",
          updatedAt: "2026-07-22T20:12:00.000Z",
        });
        const updatedSnapshot = activitySnapshot(snapshot.scope, [completed], {
          revision: 2,
          counts: {
            subagents: { active: 0, done: 1 },
            backgroundTasks: { active: 0, done: 0 },
          },
          updatedAt: "2026-07-22T20:12:00.000Z",
        });
        seedActivityState(environmentId, snapshot.scope, updatedSnapshot);
        seedRoster(environmentId, updatedSnapshot, "subagents", "active", []);
        seedRoster(environmentId, updatedSnapshot, "subagents", "done", [completed]);
        seedDetail(environmentId, updatedSnapshot, completed);

        await act(async () => {
          useRightPanelStore
            .getState()
            .openActivity(scopeThreadRef(environmentId, threadId), "subagents", snapshot.scope);
          root.render(
            <ChatView
              environmentId={environmentId}
              threadId={threadId}
              routeKind="server"
              reserveTitleBarControlInset
            />,
          );
          await Promise.resolve();
        });

        const activeFirstPageKey = activityKey("activity-roster", {
          environmentId,
          input: {
            scope: snapshot.scope,
            scopeId: snapshot.scopeId,
            section: "subagents",
            bucket: "active",
            limit: 200,
          },
        });
        const doneFirstPageKey = activityKey("activity-roster", {
          environmentId,
          input: {
            scope: snapshot.scope,
            scopeId: snapshot.scopeId,
            section: "subagents",
            bucket: "done",
            limit: 200,
          },
        });
        const detailFirstPageKey = activityKey("activity-detail", {
          environmentId,
          input: {
            scope: snapshot.scope,
            scopeId: snapshot.scopeId,
            recordKind: "actor",
            recordId: child.id,
            limit: 200,
          },
        });
        await vi.waitFor(() => {
          expect(h.queryRefreshCalls).toContain(activeFirstPageKey);
          expect(h.queryRefreshCalls).toContain(doneFirstPageKey);
          expect(h.queryRefreshCalls).toContain(detailFirstPageKey);
        });
      } finally {
        await act(async () => root.unmount());
        container.remove();
      }
    });

    it("keeps exhausted roster and detail queries latched across snapshot timers", async () => {
      const child = actor("actor-liveness", "Liveness inspector");
      const snapshot = activitySnapshot({ _tag: "thread", threadId }, [child]);
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      seedActivityState(environmentId, snapshot.scope, snapshot);
      seedActivityQueries(environmentId, snapshot, [child]);
      const { container, root } = await mountActivityRoute();
      try {
        await openSubagents(container);
        await vi.waitFor(() =>
          expect(container.querySelector(`[data-activity-row="${child.id}"]`)).not.toBeNull(),
        );
        await click(container.querySelector(`[data-activity-row="${child.id}"]`)!);
        await vi.waitFor(() => expect(h.queryRefreshCalls.length).toBeGreaterThan(0));
        const keys = [...h.activeQuerySubscriptions.keys()].filter(
          (key) => key.startsWith("activity-roster:") || key.startsWith("activity-detail:"),
        );
        expect(keys.some((key) => key.startsWith("activity-detail:"))).toBe(true);
        expect(keys.filter((key) => key.startsWith("activity-roster:"))).toHaveLength(2);
        for (const key of keys) {
          h.queryAwaitingRetry.add(key);
          h.queryEmissionsByKey.set(
            key,
            AsyncResult.failure(
              Cause.fail(new Error("The connection dropped before the result arrived.")),
            ),
          );
        }
        h.queryRetryCalls = [];
        h.queryRevalidationCalls = [];
        vi.useFakeTimers();
        for (const revision of [2, 3, 4]) {
          seedActivityState(
            environmentId,
            snapshot.scope,
            activitySnapshot(snapshot.scope, [child], { revision }),
          );
          await act(async () => {
            // As in the snapshot-revision test above, the store update re-renders the binding.
            useRightPanelStore
              .getState()
              .openActivity(scopeThreadRef(environmentId, threadId), "subagents", snapshot.scope);
            root.render(
              <ChatView
                environmentId={environmentId}
                threadId={threadId}
                routeKind="server"
                reserveTitleBarControlInset
              />,
            );
          });
          await act(async () => {
            await vi.advanceTimersByTimeAsync(100);
          });
        }
        for (const key of keys) {
          expect(h.queryRevalidationCalls).toContain(key);
          expect(h.queryAwaitingRetry.has(key)).toBe(true);
        }
        expect(h.queryRetryCalls).toEqual([]);
        // The failed-detail Retry handler is an explicit user action.
        await act(async () => {
          latestActivityPanelProps().onLoadMoreDetail();
        });
        expect(h.queryRetryCalls.some((key) => key.startsWith("activity-detail:"))).toBe(true);
      } finally {
        await act(async () => root.unmount());
        container.remove();
        vi.useRealTimers();
      }
    });

    it("revalidates the newest roster page after paginating beyond the bounded window", async () => {
      const firstPage = Array.from({ length: 190 }, (_, index) =>
        actor(`actor-newer-${index}`, `Newer ${index}`),
      );
      const olderPage = Array.from({ length: 20 }, (_, index) =>
        actor(`actor-older-${index}`, `Older ${index}`),
      );
      const newArrival = actor("actor-new-arrival", "New arrival");
      const snapshot = activitySnapshot({ _tag: "thread", threadId }, firstPage, {
        counts: {
          subagents: { active: 210, done: 0 },
          backgroundTasks: { active: 0, done: 0 },
        },
      });
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      seedActivityState(environmentId, snapshot.scope, snapshot);
      seedRoster(environmentId, snapshot, "subagents", "active", firstPage, "active-older");
      seedRoster(environmentId, snapshot, "subagents", "active", olderPage, null, "active-older");
      seedRoster(environmentId, snapshot, "subagents", "done", []);

      const { container, root } = await mountActivityRoute();
      try {
        await openSubagents(container);
        await vi.waitFor(() =>
          expect(
            latestActivityPanelProps().roster.active.pages.flatMap((page) => page.records),
          ).toHaveLength(190),
        );
        await act(async () => {
          latestActivityPanelProps().onLoadMoreRoster("active");
          await Promise.resolve();
        });
        await vi.waitFor(() => {
          const records = latestActivityPanelProps().roster.active.pages.flatMap(
            (page) => page.records,
          );
          expect(records).toHaveLength(200);
          expect(records.some((record) => record.id === olderPage[0]?.id)).toBe(true);
        });
        h.queryRefreshCalls = [];

        const refreshedFirstPage = [...firstPage.slice(0, -1), newArrival];
        seedRoster(
          environmentId,
          snapshot,
          "subagents",
          "active",
          refreshedFirstPage,
          "active-older",
        );
        const updatedSnapshot = activitySnapshot(snapshot.scope, [newArrival], {
          revision: 2,
          counts: {
            subagents: { active: 210, done: 0 },
            backgroundTasks: { active: 0, done: 0 },
          },
          updatedAt: "2026-07-22T20:12:00.000Z",
        });
        seedActivityState(environmentId, snapshot.scope, updatedSnapshot);
        await act(async () => {
          useRightPanelStore
            .getState()
            .openActivity(scopeThreadRef(environmentId, threadId), "subagents", snapshot.scope);
          root.render(
            <ChatView
              environmentId={environmentId}
              threadId={threadId}
              routeKind="server"
              reserveTitleBarControlInset
            />,
          );
          await Promise.resolve();
        });

        const activeFirstPageKey = activityKey("activity-roster", {
          environmentId,
          input: {
            scope: snapshot.scope,
            scopeId: snapshot.scopeId,
            section: "subagents",
            bucket: "active",
            limit: 200,
          },
        });
        await vi.waitFor(() => expect(h.queryRefreshCalls).toContain(activeFirstPageKey));
        await vi.waitFor(() => {
          const records = latestActivityPanelProps().roster.active.pages.flatMap(
            (page) => page.records,
          );
          expect(records).toHaveLength(200);
          expect(records.some((record) => record.id === newArrival.id)).toBe(true);
          expect(records.some((record) => record.id === olderPage[0]?.id)).toBe(true);
        });
      } finally {
        await act(async () => root.unmount());
        container.remove();
      }
    });

    it("revalidates cached roster pages when the inspector reopens after a closed-panel delta", async () => {
      const child = actor("actor-reopened-inspector", "Reopened inspector");
      const snapshot = activitySnapshot({ _tag: "thread", threadId }, [child]);
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      seedActivityState(environmentId, snapshot.scope, snapshot);
      seedActivityQueries(environmentId, snapshot, [child]);

      const { container, root } = await mountActivityRoute();
      try {
        await openSubagents(container);
        await vi.waitFor(() => expect(h.queryRefreshCalls.length).toBeGreaterThan(0));
        h.queryRefreshCalls = [];
        await act(async () => {
          useRightPanelStore
            .getState()
            .closeSurface(scopeThreadRef(environmentId, threadId), "activity");
          await Promise.resolve();
        });

        const updatedSnapshot = activitySnapshot(snapshot.scope, [], {
          revision: 2,
          counts: {
            subagents: { active: 0, done: 0 },
            backgroundTasks: { active: 0, done: 0 },
          },
          updatedAt: "2026-07-22T20:12:00.000Z",
        });
        seedActivityState(environmentId, snapshot.scope, updatedSnapshot);
        seedRoster(environmentId, updatedSnapshot, "subagents", "active", []);
        seedRoster(environmentId, updatedSnapshot, "subagents", "done", []);

        await act(async () => {
          useRightPanelStore
            .getState()
            .openActivity(scopeThreadRef(environmentId, threadId), "subagents", snapshot.scope);
          await Promise.resolve();
        });

        const activeFirstPageKey = activityKey("activity-roster", {
          environmentId,
          input: {
            scope: snapshot.scope,
            scopeId: snapshot.scopeId,
            section: "subagents",
            bucket: "active",
            limit: 200,
          },
        });
        await vi.waitFor(() => expect(h.queryRefreshCalls).toContain(activeFirstPageKey));
      } finally {
        await act(async () => root.unmount());
        container.remove();
      }
    });

    it("binds a panel chat to its own thread and opens its activity inspector inline", async () => {
      const panelThreadId = ThreadId.make("thread-panel");
      const panelThreadRef = scopeThreadRef(environmentId, panelThreadId);
      const child = actor("actor-panel", "Panel reviewer");
      const snapshot = activitySnapshot({ _tag: "thread", threadId: panelThreadId }, [child]);
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread({ id: panelThreadId, title: "Panel thread" }));
      seedGitStatus(true);
      seedActivityState(environmentId, snapshot.scope, snapshot);
      seedActivityQueries(environmentId, snapshot, [child]);

      const { container, root } = await mountActivityPanel(panelThreadRef);
      try {
        expect(container.querySelectorAll('[data-testid="activity-dock"]')).toHaveLength(1);
        expect(h.activityStateTargets).toEqual([
          {
            environmentId,
            input: { _tag: "thread", threadId: panelThreadId },
          },
        ]);
        await openSubagents(container);
        await vi.waitFor(() => expect(container.textContent).toContain("Panel reviewer"));
        expect(container.querySelector('[data-mock="right-panel-tabs"]')).not.toBeNull();
        expect(container.querySelector('[data-mock="right-panel-sheet"]')).toBeNull();
        expect(container.querySelector("[data-activity-panel]")).not.toBeNull();
        expect(
          capturedProps<{ surfaces: Array<{ kind: string }>; allowAddSurfaces?: boolean }>(
            "rightPanelTabs",
          ).surfaces,
        ).toEqual([expect.objectContaining({ kind: "activity" })]);
        expect(
          capturedProps<{ allowAddSurfaces?: boolean }>("rightPanelTabs").allowAddSurfaces,
        ).toBe(false);
        expect(h.captured["chatHeaderActions"]).toBeUndefined();
        expect(h.captured["panelLayoutControls"]).toBeUndefined();
      } finally {
        await act(async () => root.unmount());
        container.remove();
      }
    });

    it("opens a panel chat activity inspector in the compact sheet", async () => {
      const child = actor("actor-panel-compact", "Compact panel reviewer");
      const snapshot = activitySnapshot({ _tag: "thread", threadId }, [child]);
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      seedActivityState(environmentId, snapshot.scope, snapshot);
      seedActivityQueries(environmentId, snapshot, [child]);
      h.compactDesktop = true;

      const { container, root } = await mountActivityPanel();
      try {
        await openSubagents(container);
        expect(container.querySelector('[data-mock="right-panel-sheet"]')).not.toBeNull();
        expect(container.querySelector("[data-activity-panel]")).not.toBeNull();
      } finally {
        await act(async () => root.unmount());
        container.remove();
      }
    });

    it("subscribes only the visible sibling chat when the host center surface is hidden", () => {
      const siblingThreadId = ThreadId.make("thread-sibling-activity");
      const hostActor = actor("actor-host", "Hidden host reviewer");
      const siblingActor = actor("actor-sibling", "Visible sibling reviewer");
      const hostSnapshot = activitySnapshot({ _tag: "thread", threadId }, [hostActor]);
      const siblingSnapshot = activitySnapshot({ _tag: "thread", threadId: siblingThreadId }, [
        siblingActor,
      ]);
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedServerThread(makeThread({ id: siblingThreadId, title: "Visible sibling" }));
      seedGitStatus(true);
      seedActivityState(environmentId, hostSnapshot.scope, hostSnapshot);
      seedActivityState(environmentId, siblingSnapshot.scope, siblingSnapshot);
      useCenterPanelStore.getState().openChatPanel(threadRef, siblingThreadId, "Codex");
      publishSeededStoreState(useCenterPanelStore);

      const markup = renderServerRoute();

      expect(markup.match(/data-testid="activity-dock"/g)).toHaveLength(1);
      expect(h.activityStateTargets).toEqual([
        {
          environmentId,
          input: { _tag: "thread", threadId: siblingThreadId },
        },
      ]);
    });

    it("keeps the inactive host error mounted inside its hidden surface host", () => {
      const siblingThreadId = ThreadId.make("thread-sibling-without-host-error");
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(
        makeThread({
          session: {
            threadId,
            status: "error",
            providerName: "codex",
            providerInstanceId: codexInstanceId,
            runtimeMode: "full-access",
            activeTurnId: null,
            lastError: "hidden host disconnected",
            updatedAt: now,
          },
        }),
      );
      seedServerThread(makeThread({ id: siblingThreadId, title: "Visible sibling" }));
      seedGitStatus(true);
      useCenterPanelStore.getState().openChatPanel(threadRef, siblingThreadId, "Codex");
      publishSeededStoreState(useCenterPanelStore);

      const markup = renderServerRoute();

      expect(markup).toContain(
        'data-mock-center-surface="chat:host" data-visible="false" class="hidden"',
      );
      expect(markup).toContain("hidden host disconnected");
    });

    it("hands the inspector between host and sibling chat without duplicate shells or targets", async () => {
      const siblingThreadId = ThreadId.make("thread-sibling-switch");
      const siblingThreadRef = scopeThreadRef(environmentId, siblingThreadId);
      const hostActor = actor("actor-host-switch", "Host reviewer");
      const siblingActor = actor("actor-sibling-switch", "Sibling reviewer");
      const hostSnapshot = activitySnapshot({ _tag: "thread", threadId }, [hostActor]);
      const siblingSnapshot = activitySnapshot({ _tag: "thread", threadId: siblingThreadId }, [
        siblingActor,
      ]);
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedServerThread(makeThread({ id: siblingThreadId, title: "Sibling switch" }));
      seedGitStatus(true);
      seedActivityState(environmentId, hostSnapshot.scope, hostSnapshot);
      seedActivityState(environmentId, siblingSnapshot.scope, siblingSnapshot);
      seedActivityQueries(environmentId, hostSnapshot, [hostActor]);
      seedActivityQueries(environmentId, siblingSnapshot, [siblingActor]);
      useRightPanelStore.getState().openActivity(threadRef, "subagents", { _tag: "thread" });
      useCenterPanelStore.getState().openChatPanel(threadRef, siblingThreadId, "Codex");
      useCenterPanelStore.getState().activateSurface(threadRef, "center:root", HOST_SURFACE_ID);
      publishSeededStoreState(useRightPanelStore);
      publishSeededStoreState(useCenterPanelStore);

      const { container, root } = await mountActivityRoute();
      try {
        expect(container.querySelectorAll('[data-testid="activity-dock"]')).toHaveLength(1);
        expect(container.querySelectorAll('[data-mock="right-panel-tabs"]')).toHaveLength(1);
        expect(container.querySelectorAll("[data-activity-panel]")).toHaveLength(1);
        expect((h.activityPanelRenderProps.at(-1) as ActivityPanelProps).snapshot.scope).toEqual({
          _tag: "thread",
          threadId,
        });

        const centerWorkspace = capturedProps<{
          state: {
            focusedGroupId: string;
            surfaces: Array<{ id: string; kind: string }>;
          };
          onActivate: (groupId: string, surface: { id: string; kind: string }) => void;
        }>("centerWorkspace");
        const siblingSurface = centerWorkspace.state.surfaces.find(
          (surface) => surface.id === `chat:${siblingThreadId}`,
        );
        expect(siblingSurface).toBeDefined();
        const siblingTargetStart = h.activityStateTargets.length;
        await act(async () => {
          centerWorkspace.onActivate(centerWorkspace.state.focusedGroupId, siblingSurface!);
          await Promise.resolve();
        });
        await vi.waitFor(() => {
          expect(container.querySelectorAll('[data-testid="activity-dock"]')).toHaveLength(1);
          expect(container.querySelectorAll('[data-mock="right-panel-tabs"]')).toHaveLength(0);
          expect(container.querySelectorAll("[data-activity-panel]")).toHaveLength(0);
        });
        expect(h.activityStateTargets.slice(siblingTargetStart)).toEqual([
          {
            environmentId,
            input: { _tag: "thread", threadId: siblingThreadId },
          },
        ]);

        await openSubagents(container);
        await vi.waitFor(() => {
          expect(container.querySelectorAll('[data-testid="activity-dock"]')).toHaveLength(1);
          expect(container.querySelectorAll('[data-mock="right-panel-tabs"]')).toHaveLength(1);
          expect(container.querySelectorAll("[data-activity-panel]")).toHaveLength(1);
        });
        expect((h.activityPanelRenderProps.at(-1) as ActivityPanelProps).snapshot.scope).toEqual({
          _tag: "thread",
          threadId: siblingThreadId,
        });
        expect(
          useRightPanelStore.getState().byThreadKey[scopedThreadKey(siblingThreadRef)]
            ?.activeSurfaceId,
        ).toBe("activity");

        const hostTargetStart = h.activityStateTargets.length;
        await act(async () => {
          centerWorkspace.onActivate(
            centerWorkspace.state.focusedGroupId,
            centerWorkspace.state.surfaces.find((surface) => surface.id === HOST_SURFACE_ID)!,
          );
          await Promise.resolve();
        });
        await vi.waitFor(() => {
          expect(container.querySelectorAll('[data-testid="activity-dock"]')).toHaveLength(1);
          expect(container.querySelectorAll('[data-mock="right-panel-tabs"]')).toHaveLength(1);
          expect(container.querySelectorAll("[data-activity-panel]")).toHaveLength(1);
        });
        expect(h.activityStateTargets.slice(hostTargetStart)).toEqual([
          {
            environmentId,
            input: { _tag: "thread", threadId },
          },
          {
            environmentId,
            input: { _tag: "thread", threadId },
          },
        ]);
        expect((h.activityPanelRenderProps.at(-1) as ActivityPanelProps).snapshot.scope).toEqual({
          _tag: "thread",
          threadId,
        });
      } finally {
        await act(async () => root.unmount());
        container.remove();
      }
    });

    it("keeps the sibling chat visible while a suppressed host Activity remains maximized", async () => {
      const siblingThreadId = ThreadId.make("thread-sibling-maximized-host");
      const hostActor = actor("actor-host-maximized", "Maximized host reviewer");
      const siblingActor = actor("actor-sibling-maximized", "Visible sibling reviewer");
      const hostSnapshot = activitySnapshot({ _tag: "thread", threadId }, [hostActor]);
      const siblingSnapshot = activitySnapshot({ _tag: "thread", threadId: siblingThreadId }, [
        siblingActor,
      ]);
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedServerThread(makeThread({ id: siblingThreadId, title: "Visible sibling" }));
      seedGitStatus(true);
      seedActivityState(environmentId, hostSnapshot.scope, hostSnapshot);
      seedActivityState(environmentId, siblingSnapshot.scope, siblingSnapshot);
      seedActivityQueries(environmentId, hostSnapshot, [hostActor]);
      seedActivityQueries(environmentId, siblingSnapshot, [siblingActor]);
      useRightPanelStore.getState().openActivity(threadRef, "subagents", { _tag: "thread" });
      useCenterPanelStore.getState().openChatPanel(threadRef, siblingThreadId, "Codex");
      useCenterPanelStore.getState().activateSurface(threadRef, "center:root", HOST_SURFACE_ID);
      publishSeededStoreState(useRightPanelStore);
      publishSeededStoreState(useCenterPanelStore);

      const { container, root } = await mountActivityRoute();
      try {
        expect(capturedProps<{ maximized: boolean }>("rightPanelMaximizeControl").maximized).toBe(
          false,
        );
        await act(async () => {
          capturedProps<{ onToggle: () => void }>("rightPanelMaximizeControl").onToggle();
          await Promise.resolve();
        });
        await vi.waitFor(() => {
          expect(
            container.querySelector('[data-chat-column-maximized-away="true"]'),
          ).not.toBeNull();
        });

        const centerWorkspace = capturedProps<{
          state: {
            focusedGroupId: string;
            surfaces: Array<{ id: string; kind: string }>;
          };
          onActivate: (groupId: string, surface: { id: string; kind: string }) => void;
        }>("centerWorkspace");
        const siblingSurface = centerWorkspace.state.surfaces.find(
          (surface) => surface.id === `chat:${siblingThreadId}`,
        );
        expect(siblingSurface).toBeDefined();
        await act(async () => {
          centerWorkspace.onActivate(centerWorkspace.state.focusedGroupId, siblingSurface!);
          await Promise.resolve();
        });

        await vi.waitFor(() => {
          const hostChatColumn = container.querySelector("[data-chat-column-maximized-away]");
          expect(hostChatColumn?.getAttribute("data-chat-column-maximized-away")).toBe("false");
          expect(container.querySelectorAll('[data-mock="right-panel-tabs"]')).toHaveLength(0);
          expect(container.querySelectorAll('[data-mock="right-panel-maximize"]')).toHaveLength(0);
          expect(container.querySelectorAll('[data-testid="activity-dock"]')).toHaveLength(1);
        });
        expect(
          capturedProps<{ rightPanelOpen: boolean }>("panelLayoutControls").rightPanelOpen,
        ).toBe(false);
        expect(
          capturedProps<{ reserveTitlebarControls: boolean }>("chatHeaderActions")
            .reserveTitlebarControls,
        ).toBe(true);

        await openSubagents(container);
        await vi.waitFor(() => {
          expect(container.querySelectorAll('[data-mock="right-panel-tabs"]')).toHaveLength(1);
          expect(container.querySelectorAll("[data-activity-panel]")).toHaveLength(1);
          expect(container.querySelectorAll('[data-mock="right-panel-maximize"]')).toHaveLength(0);
        });
        expect((h.activityPanelRenderProps.at(-1) as ActivityPanelProps).snapshot.scope).toEqual({
          _tag: "thread",
          threadId: siblingThreadId,
        });

        await act(async () => {
          centerWorkspace.onActivate(
            centerWorkspace.state.focusedGroupId,
            centerWorkspace.state.surfaces.find((surface) => surface.id === HOST_SURFACE_ID)!,
          );
          await Promise.resolve();
        });
        await vi.waitFor(() => {
          expect(
            container.querySelector('[data-chat-column-maximized-away="true"]'),
          ).not.toBeNull();
          expect(container.querySelectorAll('[data-mock="right-panel-tabs"]')).toHaveLength(1);
          expect(container.querySelectorAll('[data-mock="right-panel-maximize"]')).toHaveLength(1);
        });
        expect(capturedProps<{ maximized: boolean }>("rightPanelMaximizeControl").maximized).toBe(
          true,
        );
        expect(
          capturedProps<{ rightPanelOpen: boolean }>("panelLayoutControls").rightPanelOpen,
        ).toBe(true);
        expect(
          capturedProps<{ reserveTitlebarControls: boolean }>("chatHeaderActions")
            .reserveTitlebarControls,
        ).toBe(false);
      } finally {
        await act(async () => root.unmount());
        container.remove();
      }
    });

    it("keeps the persisted terminal activity scope pinned when another terminal pane becomes active", () => {
      const terminalId = "terminal-inspected-activity";
      const activeCenterTerminalId = "terminal-center-active";
      const scope: ActivityScopeRef = {
        _tag: "terminal",
        threadId,
        terminalId,
      };
      const child = actor("actor-terminal-center", "Terminal reviewer");
      const snapshot = activitySnapshot(scope, [child]);
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      seedActivityState(environmentId, scope, snapshot);
      seedActivityQueries(environmentId, snapshot, [child]);
      h.environmentSettingsById.set(environmentId, {
        ...(h.settings as Record<string, unknown>),
        enableTerminalAgentActivity: true,
      });
      useCenterPanelStore.getState().openTerminalPanel(threadRef, activeCenterTerminalId);
      useRightPanelStore
        .getState()
        .openActivity(threadRef, "subagents", { _tag: "terminal", terminalId });
      publishSeededStoreState(useCenterPanelStore);
      publishSeededStoreState(useRightPanelStore);

      const markup = renderServerRoute();

      expect(markup).toContain('data-mock="center-terminal-panel"');
      expect(markup).toContain('data-testid="activity-dock"');
      expect(markup).toContain('data-mock="right-panel-tabs"');
      expect(markup).toContain("data-activity-panel");
      expect(
        capturedProps<{ surface: { terminalId: string }; projectId: ProjectId }>(
          "centerTerminalPanel",
        ),
      ).toMatchObject({
        surface: { terminalId: activeCenterTerminalId },
        projectId,
      });
      expect(h.activityStateTargets).toEqual([
        { environmentId, input: scope },
        { environmentId, input: scope },
      ]);
    });

    it("closes only Activity from a panel with hidden generic right-panel surfaces", async () => {
      const panelThreadId = ThreadId.make("thread-panel-hidden-surfaces");
      const panelThreadRef = scopeThreadRef(environmentId, panelThreadId);
      const child = actor("actor-panel-hidden", "Hidden surfaces reviewer");
      const snapshot = activitySnapshot({ _tag: "thread", threadId: panelThreadId }, [child]);
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread({ id: panelThreadId, title: "Panel hidden surfaces" }));
      seedGitStatus(true);
      seedActivityState(environmentId, snapshot.scope, snapshot);
      seedActivityQueries(environmentId, snapshot, [child]);
      h.previewState = {
        ...h.previewState,
        sessions: {
          "preview-hidden": {
            threadId: panelThreadId,
            tabId: "preview-hidden",
            navStatus: { _tag: "Success", url: "https://hidden.test/", title: "Hidden" },
            canGoBack: false,
            canGoForward: false,
            updatedAt: now,
          },
        },
      };
      useRightPanelStore.getState().openBrowser(panelThreadRef, "preview-hidden");
      useRightPanelStore.getState().openTerminal(panelThreadRef, "terminal-hidden");
      useRightPanelStore.getState().openFile(panelThreadRef, "src/hidden.ts");
      useRightPanelStore.getState().openActivity(panelThreadRef, "subagents", { _tag: "thread" });
      publishSeededStoreState(useRightPanelStore);

      const { container, root } = await mountActivityPanel(panelThreadRef);
      try {
        expect(container.querySelectorAll('[data-mock="right-panel-tabs"]')).toHaveLength(1);
        const before = useRightPanelStore.getState().byThreadKey[scopedThreadKey(panelThreadRef)];
        expect(before?.surfaces).toHaveLength(4);
        expect(before?.surfaces.map((surface) => surface.kind)).toEqual(
          expect.arrayContaining(["preview", "terminal", "file", "activity"]),
        );

        await act(async () => {
          capturedProps<{ onCloseAllSurfaces: () => void }>("rightPanelTabs").onCloseAllSurfaces();
          await Promise.resolve();
        });

        const after = useRightPanelStore.getState().byThreadKey[scopedThreadKey(panelThreadRef)];
        expect(after?.surfaces).toHaveLength(3);
        expect(after?.surfaces.map((surface) => surface.kind)).toEqual(
          expect.arrayContaining(["preview", "terminal", "file"]),
        );
        expect(h.commandCalls.filter((call) => call.key === "preview.close")).toEqual([]);
        expect(h.commandCalls.filter((call) => call.key === "terminal.close")).toEqual([]);
        expect(h.releasedTerminalInputs).toEqual([]);
        expect(container.querySelector('[data-mock="right-panel-tabs"]')).toBeNull();
      } finally {
        await act(async () => root.unmount());
        container.remove();
      }
    });
  });

  describe("when: no server thread and no draft session exist", () => {
    it("renders the no-active-thread empty state inside the diff worker pool", () => {
      seedEnvironment(makeEnvironmentPresentation());
      const markup = renderServerRoute();

      expect(markup).toContain('data-mock="diff-worker-pool"');
      expect(markup).toContain('data-mock="no-active-thread"');
      expect(markup).not.toContain('data-mock="chat-composer"');
    });
  });

  describe("when: a server thread exists on a connected environment", () => {
    it("renders header, timeline, and composer", () => {
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);

      const markup = renderServerRoute();

      expect(markup).toContain('data-mock="center-panel-workspace"');
      expect(markup).toContain('data-mock="chat-header-actions"');
      expect(markup).not.toContain("data-center-panel-header-row");
      expect(markup).toContain('data-mock="messages-timeline"');
      expect(markup).toContain('data-mock="chat-composer"');
      expect(markup).not.toContain('data-mock="no-active-thread"');

      const workspace = capturedProps<Record<string, unknown>>("centerWorkspace");
      expect(workspace["hostLabel"]).toBe("Codex");
      expect(workspace["renderFocusedActions"]).toBeTypeOf("function");
      expect(capturedProps<Record<string, unknown>>("chatHeaderActions")).not.toHaveProperty(
        "activeThreadTitle",
      );

      const composer = capturedProps<Record<string, unknown>>("chatComposer");
      expect(composer["isServerThread"]).toBe(true);
      expect(composer["isLocalDraftThread"]).toBe(false);
      expect(composer["routeKind"]).toBe("server");
      expect(composer["environmentUnavailable"]).toBeNull();
      expect(composer["providerStatuses"]).toEqual([codexProvider]);

      const activeThread = composer["activeThread"] as Thread;
      expect(activeThread.messages).toEqual([]);
      expect(activeThread.session).toBeNull();
      expect(activeThread.latestTurn).toBeNull();

      const header = capturedProps<Record<string, unknown>>("chatHeaderActions");
      expect(header["density"]).toBe("compact");
      expect(header["activeThreadId"]).toBe(threadId);
      expect(header["activeProjectName"]).toBe("Demo Project");
      expect(header["canCreatePanel"]).toBe(true);

      const panelControls = capturedProps<Record<string, unknown>>("panelLayoutControls");
      expect(panelControls).not.toHaveProperty("terminalAvailable");
      expect(panelControls).not.toHaveProperty("terminalOpen");
      expect(panelControls).not.toHaveProperty("onToggleTerminal");
      expect(panelControls["rightPanelAvailable"]).toBe(true);
      expect(panelControls["rightPanelOpen"]).toBe(false);
      expect(markup.match(/data-mock="panel-layout-controls"/g)).toHaveLength(1);
      expect(markup).not.toContain("panel-bottom");
      expect(workspace).not.toHaveProperty("panelLayoutControls");

      const bannerStack = capturedProps<{ items: ComposerBannerStackItem[] }>(
        "composerBannerStack",
      );
      expect(bannerStack.items).toEqual([]);
    });

    it("renders one focused header owner with the focused group's local density", async () => {
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      useCenterPanelStore.setState({
        byThreadKey: {
          [scopedThreadKey(threadRef)]: {
            surfaces: [
              { id: HOST_SURFACE_ID, kind: "chat-host" },
              { id: "terminal:term-right", kind: "terminal", terminalId: "term-right" },
            ],
            groups: [
              {
                id: "group-left",
                surfaceIds: [HOST_SURFACE_ID],
                activeSurfaceId: HOST_SURFACE_ID,
              },
              {
                id: "group-right",
                surfaceIds: ["terminal:term-right"],
                activeSurfaceId: "terminal:term-right",
              },
            ],
            layout: {
              type: "split",
              direction: "horizontal",
              ratio: 0.5,
              first: { type: "leaf", groupId: "group-left" },
              second: { type: "leaf", groupId: "group-right" },
            },
            focusedGroupId: "group-left",
          },
        },
      });
      h.centerHeaderDensityByGroupId.set("group-left", "expanded");
      h.centerHeaderDensityByGroupId.set("group-right", "compact");

      vi.unstubAllGlobals();
      Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
      Object.defineProperty(Element.prototype, "getAnimations", {
        configurable: true,
        value: () => [],
      });
      const container = document.createElement("div");
      document.body.append(container);
      const root = createRoot(container);
      const renderView = () => (
        <ChatView
          environmentId={environmentId}
          threadId={threadId}
          routeKind="server"
          reserveTitleBarControlInset
        />
      );

      try {
        await act(async () => {
          root.render(renderView());
          await Promise.resolve();
        });

        const leftHeader = container.querySelector('[data-mock-center-header="group-left"]');
        const rightHeader = container.querySelector('[data-mock-center-header="group-right"]');
        const expandedActions = leftHeader?.querySelector('[data-mock="chat-header-actions"]');
        expect(expandedActions?.getAttribute("data-density")).toBe("expanded");
        expect(rightHeader?.querySelector('[data-mock="chat-header-actions"]')).toBeNull();
        expect(container.querySelectorAll('[data-mock="chat-header-actions"]')).toHaveLength(1);
        expect(
          capturedProps<{ reserveTitlebarControls: boolean }>("chatHeaderActions")
            .reserveTitlebarControls,
        ).toBe(false);
        const expandedInstanceId = expandedActions?.getAttribute("data-instance-id");

        h.centerHeaderDensityByGroupId.set("group-left", "compact");
        await act(async () => {
          root.render(renderView());
          await Promise.resolve();
        });

        const compactActions = container.querySelector(
          '[data-mock-center-header="group-left"] [data-mock="chat-header-actions"]',
        );
        expect(compactActions?.getAttribute("data-density")).toBe("compact");
        expect(compactActions?.getAttribute("data-instance-id")).toBe(expandedInstanceId);
        expect(h.chatHeaderActionsInstanceCount).toBe(1);

        await act(async () => {
          useCenterPanelStore.getState().focusGroup(threadRef, "group-right");
          await Promise.resolve();
        });

        expect(
          container.querySelector(
            '[data-mock-center-header="group-left"] [data-mock="chat-header-actions"]',
          ),
        ).toBeNull();
        expect(
          container
            .querySelector(
              '[data-mock-center-header="group-right"] [data-mock="chat-header-actions"]',
            )
            ?.getAttribute("data-density"),
        ).toBe("compact");
        expect(container.querySelectorAll('[data-mock="chat-header-actions"]')).toHaveLength(1);
        expect(
          capturedProps<{ reserveTitlebarControls: boolean }>("chatHeaderActions")
            .reserveTitlebarControls,
        ).toBe(true);
      } finally {
        await act(async () => root.unmount());
        container.remove();
      }
    });

    it("does not reserve titlebar controls for a focused left center group", () => {
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      useCenterPanelStore.setState({
        byThreadKey: {
          [scopedThreadKey(threadRef)]: {
            surfaces: [
              { id: HOST_SURFACE_ID, kind: "chat-host" },
              { id: "terminal:term-right", kind: "terminal", terminalId: "term-right" },
            ],
            groups: [
              {
                id: "group-left",
                surfaceIds: [HOST_SURFACE_ID],
                activeSurfaceId: HOST_SURFACE_ID,
              },
              {
                id: "group-right",
                surfaceIds: ["terminal:term-right"],
                activeSurfaceId: "terminal:term-right",
              },
            ],
            layout: {
              type: "split",
              direction: "horizontal",
              ratio: 0.5,
              first: { type: "leaf", groupId: "group-left" },
              second: { type: "leaf", groupId: "group-right" },
            },
            focusedGroupId: "group-left",
          },
        },
      });
      publishSeededStoreState(useCenterPanelStore);

      renderServerRoute();

      expect(
        capturedProps<{ reserveTitlebarControls: boolean }>("chatHeaderActions")
          .reserveTitlebarControls,
      ).toBe(false);
    });

    it("uses the selected provider display name for the host tab", () => {
      const namedProvider: ServerProvider = {
        ...codexProvider,
        displayName: "Codex Personal",
      };
      seedEnvironment(
        makeEnvironmentPresentation({
          serverConfig: { providers: [namedProvider], environment: { label: "Local" } },
        }),
      );
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);

      renderServerRoute();

      expect(capturedProps<Record<string, unknown>>("centerWorkspace")["hostLabel"]).toBe(
        "Codex Personal",
      );
    });

    it("falls back to the selected provider kind when its status is missing", () => {
      seedEnvironment(
        makeEnvironmentPresentation({
          serverConfig: { providers: [], environment: { label: "Local" } },
        }),
      );
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);

      renderServerRoute();

      expect(capturedProps<Record<string, unknown>>("centerWorkspace")["hostLabel"]).toBe("Codex");
    });

    it("updates the pre-start host tab label when the selected provider changes", () => {
      const claudeInstanceId = ProviderInstanceId.make("claude");
      const claudeProvider: ServerProvider = {
        ...codexProvider,
        instanceId: claudeInstanceId,
        driver: ProviderDriverKind.make("claude"),
        displayName: "Claude",
      };
      seedEnvironment(
        makeEnvironmentPresentation({
          serverConfig: {
            providers: [codexProvider, claudeProvider],
            environment: { label: "Local" },
          },
        }),
      );
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);

      renderServerRoute();
      expect(capturedProps<Record<string, unknown>>("centerWorkspace")["hostLabel"]).toBe("Codex");
      const initialComposer = capturedProps<Record<string, unknown>>("chatComposer");
      expect(initialComposer["lockProviderPickerToActiveInstance"]).toBe(false);
      const initialSurfaces =
        useCenterPanelStore.getState().byThreadKey[scopedThreadKey(threadRef)]?.surfaces;

      const selectProviderModel = initialComposer["onProviderModelSelect"] as (
        instanceId: ProviderInstanceId,
        model: string,
      ) => void;
      selectProviderModel(claudeInstanceId, "gpt-5.4");
      publishSeededStoreState(useComposerDraftStore);
      renderServerRoute();

      expect(capturedProps<Record<string, unknown>>("centerWorkspace")["hostLabel"]).toBe("Claude");
      expect(
        useCenterPanelStore.getState().byThreadKey[scopedThreadKey(threadRef)]?.surfaces,
      ).toEqual(initialSurfaces);

      seedServerThread(
        makeThread({
          session: {
            threadId,
            status: "ready",
            providerName: "codex",
            providerInstanceId: codexInstanceId,
            runtimeMode: "full-access",
            activeTurnId: null,
            lastError: null,
            updatedAt: now,
          },
        }),
      );
      renderServerRoute();

      const startedComposer = capturedProps<Record<string, unknown>>("chatComposer");
      expect(startedComposer["lockedProvider"]).toBe("codex");
      expect(startedComposer["providerBindingInstanceId"]).toBe("codex");
      expect(startedComposer["lockProviderPickerToActiveInstance"]).toBe(false);
      expect(capturedProps<Record<string, unknown>>("centerWorkspace")["hostLabel"]).toBe("Codex");

      const selectStartedProviderModel = startedComposer["onProviderModelSelect"] as (
        instanceId: ProviderInstanceId,
        model: string,
      ) => void;
      selectStartedProviderModel(codexInstanceId, "gpt-5.4");
      selectStartedProviderModel(claudeInstanceId, "gpt-5.4");
      expect(useComposerDraftStore.getState().getComposerDraft(threadRef)?.activeProvider).toBe(
        "codex",
      );
    });

    it("keeps a started host labeled from its bound provider despite stale composer selection", () => {
      const claudeInstanceId = ProviderInstanceId.make("claude");
      const claudeProvider: ServerProvider = {
        ...codexProvider,
        instanceId: claudeInstanceId,
        driver: ProviderDriverKind.make("claude"),
        displayName: "Claude",
      };
      seedEnvironment(
        makeEnvironmentPresentation({
          serverConfig: {
            providers: [codexProvider, claudeProvider],
            environment: { label: "Local" },
          },
        }),
      );
      seedProject(makeProject());
      seedServerThread(
        makeThread({
          session: {
            threadId,
            status: "ready",
            providerName: "codex",
            providerInstanceId: codexInstanceId,
            runtimeMode: "full-access",
            activeTurnId: null,
            lastError: null,
            updatedAt: now,
          },
        }),
      );
      useComposerDraftStore.getState().setModelSelection(threadRef, {
        instanceId: claudeInstanceId,
        model: "claude-sonnet",
      });
      publishSeededStoreState(useComposerDraftStore);
      seedGitStatus(true);

      renderServerRoute();

      expect(capturedProps<Record<string, unknown>>("centerWorkspace")["hostLabel"]).toBe("Codex");
    });

    it("uses the authoritative session driver when a legacy session has no instance id", () => {
      const claudeInstanceId = ProviderInstanceId.make("claude");
      const claudeProvider: ServerProvider = {
        ...codexProvider,
        instanceId: claudeInstanceId,
        driver: ProviderDriverKind.make("claude"),
        displayName: "Claude",
        models: [
          {
            slug: "claude-sonnet",
            name: "Claude Sonnet",
            isCustom: false,
            capabilities: null,
          },
        ],
      };
      seedEnvironment(
        makeEnvironmentPresentation({
          serverConfig: {
            providers: [codexProvider, claudeProvider],
            environment: { label: "Local" },
          },
        }),
      );
      seedProject(makeProject({ defaultModelSelection: null }));
      seedServerThread(
        makeThread({
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
      );
      useComposerDraftStore.getState().setModelSelection(threadRef, {
        instanceId: claudeInstanceId,
        model: "claude-sonnet",
      });
      publishSeededStoreState(useComposerDraftStore);
      seedGitStatus(true);

      renderServerRoute();

      const composer = capturedProps<Record<string, unknown>>("chatComposer");
      expect(capturedProps<Record<string, unknown>>("centerWorkspace")["hostLabel"]).toBe("Codex");
      expect(composer["lockedProvider"]).toBe("codex");
      expect(composer["providerBindingInstanceId"]).toBe("codex");
    });

    it("locks a sessionless started custom instance to its provider family", () => {
      const customCodexInstanceId = ProviderInstanceId.make("codex_personal");
      const claudeInstanceId = ProviderInstanceId.make("claude");
      const customCodexProvider: ServerProvider = {
        ...codexProvider,
        instanceId: customCodexInstanceId,
        displayName: "Codex Personal",
      };
      const claudeProvider: ServerProvider = {
        ...codexProvider,
        instanceId: claudeInstanceId,
        driver: ProviderDriverKind.make("claude"),
        displayName: "Claude",
      };
      seedEnvironment(
        makeEnvironmentPresentation({
          serverConfig: {
            providers: [customCodexProvider, claudeProvider],
            environment: { label: "Local" },
          },
        }),
      );
      seedProject(makeProject());
      seedServerThread(
        makeThread({
          modelSelection: { instanceId: customCodexInstanceId, model: "gpt-5.4" },
          session: null,
          messages: [
            {
              id: MessageId.make("started-custom-instance"),
              role: "user",
              text: "Started",
              turnId: null,
              createdAt: now,
              updatedAt: now,
              streaming: false,
            },
          ],
        }),
      );
      useComposerDraftStore.getState().setModelSelection(threadRef, {
        instanceId: claudeInstanceId,
        model: "claude-sonnet",
      });
      publishSeededStoreState(useComposerDraftStore);
      seedGitStatus(true);

      renderServerRoute();

      expect(capturedProps<Record<string, unknown>>("centerWorkspace")["hostLabel"]).toBe(
        "Codex Personal",
      );
      expect(capturedProps<Record<string, unknown>>("chatComposer")["lockedProvider"]).toBe(
        "codex",
      );
    });

    it("uses the driver's name for a locked custom instance whose snapshot has no display name", () => {
      const customClaudeInstanceId = ProviderInstanceId.make("claudeAgent_work");
      seedEnvironment(
        makeEnvironmentPresentation({
          serverConfig: {
            providers: [
              {
                ...codexProvider,
                instanceId: customClaudeInstanceId,
                driver: ProviderDriverKind.make("claudeAgent"),
              },
            ],
            environment: { label: "Local" },
          },
        }),
      );
      seedProject(makeProject());
      seedServerThread(
        makeThread({
          modelSelection: { instanceId: customClaudeInstanceId, model: "claude-sonnet" },
          session: null,
          messages: [
            {
              id: MessageId.make("started-custom-instance-legacy-snapshot"),
              role: "user",
              text: "Started",
              turnId: null,
              createdAt: now,
              updatedAt: now,
              streaming: false,
            },
          ],
        }),
      );
      seedGitStatus(true);

      renderServerRoute();

      expect(capturedProps<Record<string, unknown>>("centerWorkspace")["hostLabel"]).toBe("Claude");
      expect(capturedProps<Record<string, unknown>>("chatComposer")["lockedProvider"]).toBe(
        "claudeAgent",
      );
    });

    it("keeps a sessionless started custom instance defensively locked while statuses load", () => {
      const customCodexInstanceId = ProviderInstanceId.make("codex_personal");
      const claudeInstanceId = ProviderInstanceId.make("claude");
      seedEnvironment(
        makeEnvironmentPresentation({
          serverConfig: { providers: [], environment: { label: "Local" } },
        }),
      );
      seedProject(makeProject());
      seedServerThread(
        makeThread({
          modelSelection: { instanceId: customCodexInstanceId, model: "gpt-5.4" },
          session: null,
          messages: [
            {
              id: MessageId.make("started-custom-instance-loading"),
              role: "user",
              text: "Started",
              turnId: null,
              createdAt: now,
              updatedAt: now,
              streaming: false,
            },
          ],
        }),
      );
      useComposerDraftStore.getState().setModelSelection(threadRef, {
        instanceId: claudeInstanceId,
        model: "claude-sonnet",
      });
      publishSeededStoreState(useComposerDraftStore);
      seedGitStatus(true);

      renderServerRoute();

      expect(capturedProps<Record<string, unknown>>("centerWorkspace")["hostLabel"]).toBe(
        "Codex Personal",
      );
      const composer = capturedProps<Record<string, unknown>>("chatComposer");
      expect(composer["lockedProvider"]).toBeNull();
      expect(composer["providerBindingInstanceId"]).toBe("codex_personal");
      expect(composer["lockProviderPickerToActiveInstance"]).toBe(true);
    });

    it("uses an exact instance lock when a live driver collides with a missing bound instance", () => {
      const boundInstanceId = ProviderInstanceId.make("codex_personal");
      const staleInstanceId = ProviderInstanceId.make("stale_selection");
      const collidingProvider: ServerProvider = {
        ...codexProvider,
        instanceId: staleInstanceId,
        driver: ProviderDriverKind.make("codex_personal"),
        displayName: "Colliding Provider",
      };
      seedEnvironment(
        makeEnvironmentPresentation({
          serverConfig: {
            providers: [collidingProvider],
            environment: { label: "Local" },
          },
        }),
      );
      seedProject(makeProject());
      seedServerThread(
        makeThread({
          modelSelection: { instanceId: boundInstanceId, model: "gpt-5.4" },
          session: null,
          messages: [
            {
              id: MessageId.make("started-colliding-instance"),
              role: "user",
              text: "Started",
              turnId: null,
              createdAt: now,
              updatedAt: now,
              streaming: false,
            },
          ],
        }),
      );
      useComposerDraftStore.getState().setModelSelection(threadRef, {
        instanceId: staleInstanceId,
        model: "collision-model",
      });
      publishSeededStoreState(useComposerDraftStore);
      seedGitStatus(true);

      renderServerRoute();

      const composer = capturedProps<Record<string, unknown>>("chatComposer");
      expect(composer["lockedProvider"]).toBeNull();
      expect(composer["providerBindingInstanceId"]).toBe("codex_personal");
      expect(capturedProps<Record<string, unknown>>("centerWorkspace")["hostLabel"]).toBe(
        "Codex Personal",
      );
    });

    it("prefers a partial session custom instance over stale model selection", () => {
      const customCodexInstanceId = ProviderInstanceId.make("codex_personal");
      const claudeInstanceId = ProviderInstanceId.make("claude");
      const customCodexProvider: ServerProvider = {
        ...codexProvider,
        instanceId: customCodexInstanceId,
        displayName: "Codex Personal",
      };
      const claudeProvider: ServerProvider = {
        ...codexProvider,
        instanceId: claudeInstanceId,
        driver: ProviderDriverKind.make("claude"),
        displayName: "Claude",
      };
      seedEnvironment(
        makeEnvironmentPresentation({
          serverConfig: {
            providers: [customCodexProvider, claudeProvider],
            environment: { label: "Local" },
          },
        }),
      );
      seedProject(makeProject());
      seedServerThread(
        makeThread({
          modelSelection: { instanceId: claudeInstanceId, model: "claude-sonnet" },
          session: {
            threadId,
            status: "ready",
            providerName: "" as never,
            providerInstanceId: customCodexInstanceId,
            runtimeMode: "full-access",
            activeTurnId: null,
            lastError: null,
            updatedAt: now,
          },
        }),
      );
      useComposerDraftStore.getState().setModelSelection(threadRef, {
        instanceId: claudeInstanceId,
        model: "claude-sonnet",
      });
      publishSeededStoreState(useComposerDraftStore);
      seedGitStatus(true);

      renderServerRoute();

      expect(capturedProps<Record<string, unknown>>("centerWorkspace")["hostLabel"]).toBe(
        "Codex Personal",
      );
      expect(capturedProps<Record<string, unknown>>("chatComposer")["lockedProvider"]).toBe(
        "codex",
      );
    });

    it("keeps header actions aligned when every center surface is closed", () => {
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      useCenterPanelStore.getState().closeAllSurfaces(threadRef, "center:root");
      publishSeededStoreState(useCenterPanelStore);

      const markup = renderServerRoute();

      expect(markup).toContain('data-mock="chat-header-actions"');
      expect(markup).toContain('data-mock="center-panel-workspace"');
      expect(
        capturedProps<Record<string, unknown>>("centerWorkspace")["renderFocusedActions"],
      ).toBeTypeOf("function");
    });

    it("keeps ordinary chat and terminal launches available after activity is downgraded", async () => {
      seedEnvironment(
        makeEnvironmentPresentation({
          serverConfig: {
            providers: [codexProvider],
            environment: {
              label: "Local",
              capabilities: { activityProtocolVersion: null },
            },
          },
        }),
      );
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);

      const markup = renderServerRoute();
      const header = capturedProps<{
        onCreateChatPanel: (entry: ProviderInstanceEntry) => void;
        onOpenTerminalPanel: () => void;
      }>("chatHeaderActions");

      header.onCreateChatPanel({
        instanceId: codexInstanceId,
        driverKind: ProviderDriverKind.make("codex"),
        displayName: "Codex",
        enabled: true,
        installed: true,
        status: "ready",
        isDefault: true,
        isAvailable: true,
        snapshot: codexProvider,
        models: codexProvider.models,
      });
      header.onOpenTerminalPanel();
      await vi.waitFor(() => {
        expect(h.commandCalls.some((call) => call.key === "worktree.createPanel")).toBe(true);
        const surfaces =
          useCenterPanelStore.getState().byThreadKey[scopedThreadKey(threadRef)]?.surfaces ?? [];
        expect(surfaces.some((surface) => surface.kind === "chat")).toBe(true);
        expect(surfaces.some((surface) => surface.kind === "terminal")).toBe(true);
      });

      const createPanel = h.commandCalls.find((call) => call.key === "worktree.createPanel");
      expect(createPanel).toBeDefined();
      expect(createPanel!.input).toMatchObject({
        environmentId,
        input: {
          commandId: expect.any(String),
          hostThreadId: threadId,
          title: "Panel — Codex",
          threadDefaults: {
            modelSelection: expect.objectContaining({ instanceId: codexInstanceId }),
            runtimeMode: "full-access",
            interactionMode: "default",
          },
        },
      });
      const createPanelInput = (createPanel!.input as { input: object }).input;
      expect(createPanelInput).not.toHaveProperty("worktreePath");
      expect(createPanelInput).not.toHaveProperty("branch");
      expect(createPanelInput).not.toHaveProperty("kind");
      expect(createPanelInput).not.toHaveProperty("projectId");

      expect(markup).toContain('data-mock="messages-timeline"');
      expect(markup).toContain('data-mock="chat-composer"');
    });

    it("renders the composer when the workspace is not a git repository", () => {
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(false);

      const markup = renderServerRoute();

      expect(markup).toContain('data-mock="chat-composer"');
    });

    it("surfaces the session error through the thread error banner", () => {
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(
        makeThread({
          session: {
            threadId,
            status: "ready",
            providerName: "codex",
            providerInstanceId: codexInstanceId,
            runtimeMode: "full-access",
            activeTurnId: null,
            lastError: "provider exploded",
            updatedAt: now,
          },
        }),
      );
      seedGitStatus(true);

      const markup = renderServerRoute();

      expect(markup).toContain("provider exploded");
      const banner = capturedProps<Record<string, unknown>>("threadErrorBanner");
      expect(banner["error"]).toBe("provider exploded");
    });
  });

  describe("when: the active environment is unavailable", () => {
    it("pushes an environment-unavailable banner with reconnect actions", () => {
      seedEnvironment(
        makeEnvironmentPresentation({
          connection: { phase: "error", error: "socket closed", traceId: null },
        }),
      );
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);

      renderServerRoute();

      const bannerStack = capturedProps<{ items: ComposerBannerStackItem[] }>(
        "composerBannerStack",
      );
      expect(bannerStack.items).toHaveLength(1);
      const item = bannerStack.items[0]!;
      expect(item.id).toBe(`environment-unavailable:${environmentId}`);
      expect(item.variant).toBe("error");
      expect(item.title).toBe("Local: Connection failed");
      expect(item.description).toBe("socket closed");

      const composer = capturedProps<Record<string, unknown>>("chatComposer");
      expect(composer["environmentUnavailable"]).toEqual({
        environmentId,
        label: "Local",
        connection: { phase: "error", error: "socket closed", traceId: null },
      });
    });
  });

  describe("when: an unavailable environment's connection is managed in Settings", () => {
    function unavailableBanner(target: ConnectionTarget): ComposerBannerStackItem {
      h.presentationSurface = { surface: "desktop", platform: "linux" };
      seedEnvironment(
        makeEnvironmentPresentation({
          connection: {
            phase: "error",
            error:
              "devbox rejected a new pairing credential. Connect again; if it keeps failing, remove the environment and add it again.",
            traceId: null,
          },
          entry: { target },
        }),
      );
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      renderServerRoute();
      return capturedProps<{ items: ComposerBannerStackItem[] }>("composerBannerStack").items[0]!;
    }

    it("offers Open Remote Servers for a remote target on desktop and states the reason once", () => {
      const item = unavailableBanner(
        new SshConnectionTarget({ connectionId: "ssh:devbox", environmentId, label: "Local" }),
      );

      expect(item.title).toBe("Local: Connection failed");
      expect(item.description).toBe(
        "devbox rejected a new pairing credential. Connect again; if it keeps failing, remove the environment and add it again.",
      );
      expect(findElementByText(item.actions, "Reconnect")).toBeNull();
      const open = findElementByText(item.actions, "Open Remote Servers");
      expect(open).not.toBeNull();
      (open!.props as { onClick: () => void }).onClick();
      expect(h.navigateCalls).toEqual([{ to: "/settings/remote-servers" }]);
    });

    it("keeps Reconnect, and no Open Remote Servers, where the desktop reconnects in place", () => {
      const item = unavailableBanner(
        new PrimaryConnectionTarget({
          environmentId,
          label: "Local",
          httpBaseUrl: "http://127.0.0.1:13935",
          wsBaseUrl: "ws://127.0.0.1:13935",
        }),
      );

      expect(findElementByText(item.actions, "Reconnect")).not.toBeNull();
      expect(findElementByText(item.actions, "Open Remote Servers")).toBeNull();
    });
  });

  describe("when: server and client versions differ", () => {
    it("pushes a version mismatch warning banner", () => {
      seedEnvironment(
        makeEnvironmentPresentation({
          serverConfig: {
            providers: [codexProvider],
            environment: { label: "Local", serverVersion: "0.0.0-version-skew-test" },
          },
        }),
      );
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);

      renderServerRoute();

      const bannerStack = capturedProps<{ items: ComposerBannerStackItem[] }>(
        "composerBannerStack",
      );
      expect(bannerStack.items).toHaveLength(1);
      expect(bannerStack.items[0]!.title).toBe("Client and server versions differ");
    });
  });

  describe("when: rendering a draft route with a seeded draft session", () => {
    it("builds a local draft thread and renders the full chat chrome", () => {
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
          { threadId, createdAt: now, envMode: "local" },
        );
      publishSeededStoreState(useComposerDraftStore);

      const markup = renderToStaticMarkup(
        <ChatView
          environmentId={environmentId}
          threadId={threadId}
          routeKind="draft"
          draftId={draftId}
        />,
      );

      expect(markup).toContain('data-mock="chat-composer"');
      expect(markup).toContain('data-mock="messages-timeline"');
      expect(markup).not.toContain('data-mock="no-active-thread"');

      const composer = capturedProps<Record<string, unknown>>("chatComposer");
      expect(composer["isServerThread"]).toBe(false);
      expect(composer["isLocalDraftThread"]).toBe(true);
      expect(composer["routeKind"]).toBe("draft");
      const activeThread = composer["activeThread"] as Thread;
      expect(activeThread.title).toBe("New thread");
      expect(activeThread.id).toBe(threadId);
      expect(activeThread.session).toBeNull();

      const header = capturedProps<Record<string, unknown>>("chatHeaderActions");
      expect(header["draftId"]).toBe(draftId);
      expect(header["canCreatePanel"]).toBe(false);
    });

    it.each([
      { label: "main branch", envMode: "local" as const, worktreePath: null },
      {
        label: "new worktree",
        envMode: "worktree" as const,
        worktreePath: "X:/demo-worktree",
      },
    ])("exposes every right-panel module before the $label draft starts", (draftContext) => {
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedGitStatus(true);
      h.previewSupported = true;

      const draftId = newDraftId();
      useComposerDraftStore
        .getState()
        .setLogicalProjectDraftThreadId(
          "logical-project-1",
          scopeProjectRef(environmentId, projectId),
          draftId,
          {
            threadId,
            createdAt: now,
            envMode: draftContext.envMode,
            worktreePath: draftContext.worktreePath,
          },
        );
      useRightPanelStore.getState().open(threadRef, "plan");
      publishSeededStoreState(useComposerDraftStore);
      publishSeededStoreState(useRightPanelStore);

      const markup = renderToStaticMarkup(
        <ChatView
          environmentId={environmentId}
          threadId={threadId}
          routeKind="draft"
          draftId={draftId}
        />,
      );

      expect(markup).toContain('data-mock="right-panel-tabs"');
      const rightPanel = capturedProps<Record<string, unknown>>("rightPanelTabs");
      expect(rightPanel["browserAvailable"]).toBe(true);
      expect(rightPanel["diffAvailable"]).toBe(true);
      expect(rightPanel["sourceControlAvailable"]).toBe(true);
      expect(rightPanel["filesAvailable"]).toBe(true);

      (rightPanel["onAddTerminal"] as () => void)();
      (rightPanel["onAddDiff"] as () => void)();
      (rightPanel["onAddSourceControl"] as () => void)();
      (rightPanel["onAddFiles"] as () => void)();

      expect(h.commandCalls.filter((call) => call.key === "terminal.open")).toHaveLength(1);
      expect(
        (useRightPanelStore.getState().byThreadKey[scopedThreadKey(threadRef)]?.surfaces ?? []).map(
          (surface) => surface.kind,
        ),
      ).toEqual(expect.arrayContaining(["terminal", "diff", "sourceControl", "files"]));
    });
  });

  describe("when: rendering the panel variant", () => {
    it("omits host-only chrome (header and branch toolbar) but keeps the transcript", () => {
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);

      const markup = renderToStaticMarkup(
        <ChatView variant="panel" panelThreadRef={threadRef} workspaceUnavailable={null} />,
      );

      expect(markup).toContain('data-mock="messages-timeline"');
      expect(markup).toContain('data-mock="chat-composer"');
      expect(markup).not.toContain('data-mock="chat-header"');
      expect(h.activityStateTargets).toEqual([
        {
          environmentId,
          input: { _tag: "thread", threadId },
        },
      ]);

      const composer = capturedProps<Record<string, unknown>>("chatComposer");
      expect(composer["routeKind"]).toBe("server");
      expect(composer["isServerThread"]).toBe(true);
      expect(composer["lockProviderPickerToActiveInstance"]).toBe(true);
    });

    it("shows an opening state instead of the global empty state before its thread arrives", () => {
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());

      const markup = renderToStaticMarkup(
        <ChatView variant="panel" panelThreadRef={threadRef} workspaceUnavailable={null} />,
      );

      expect(markup).toContain("Opening chat…");
      expect(markup).not.toContain('data-mock="no-active-thread"');
    });
  });

  describe("when: the plan right-panel surface is open", () => {
    it("renders the plan sidebar inside the inline right panel tabs", () => {
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      useRightPanelStore.getState().open(threadRef, "plan");
      publishSeededStoreState(useRightPanelStore);

      const markup = renderServerRoute();

      expect(markup).toContain('data-mock="right-panel-tabs"');
      expect(markup).toContain('data-mock="plan-sidebar"');

      const planSidebar = capturedProps<Record<string, unknown>>("planSidebar");
      expect(planSidebar["label"]).toBe("Tasks");
      expect(planSidebar["environmentId"]).toBe(environmentId);
    });
  });

  describe("when: native preview tabs are owned by the host chat view", () => {
    it("passes every right-panel surface to persistent hosts while the panel is hidden", () => {
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      h.previewState = {
        ...h.previewState,
        sessions: {
          "tab-a": {
            threadId,
            tabId: "tab-a",
            navStatus: { _tag: "Success", url: "https://a.test/", title: "A" },
            canGoBack: false,
            canGoForward: false,
            updatedAt: now,
          },
          "tab-b": {
            threadId,
            tabId: "tab-b",
            navStatus: { _tag: "Success", url: "https://b.test/", title: "B" },
            canGoBack: false,
            canGoForward: false,
            updatedAt: now,
          },
        },
      };
      useRightPanelStore.getState().openBrowser(threadRef, "tab-a");
      useRightPanelStore.getState().openBrowser(threadRef, "tab-b");
      useRightPanelStore.getState().openTerminal(threadRef, "term-active");
      useRightPanelStore.getState().close(threadRef);
      publishSeededStoreState(useRightPanelStore);

      const markup = renderServerRoute();

      expect(markup).toContain('data-mock="desktop-preview-tab-hosts"');
      expect(markup).not.toContain('data-mock="right-panel-tabs"');
      const hosts = capturedProps<Record<string, unknown>>("desktopPreviewTabHosts");
      expect(hosts["threadRef"]).toEqual(threadRef);
      expect(hosts["surfaces"]).toEqual([
        { id: "browser:tab-a", kind: "preview", resourceId: "tab-a" },
        { id: "browser:tab-b", kind: "preview", resourceId: "tab-b" },
        {
          id: "terminal:term-active",
          kind: "terminal",
          resourceId: "term-active",
          terminalIds: ["term-active"],
          activeTerminalId: "term-active",
        },
      ]);
      expect(hosts["sessions"]).toBe(h.previewState.sessions);
    });
  });

  describe("when: a terminal right-panel surface is open", () => {
    it("renders the persistent terminal panel with the surface's terminal group", () => {
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      h.knownSessions = [
        {
          target: { environmentId, threadId, terminalId: "term-1" },
          state: {
            summary: {
              label: "Build shell",
              cwd: "X:/demo",
              worktreePath: null,
            },
          },
        },
      ];
      useRightPanelStore.getState().openTerminal(threadRef, "term-1");
      publishSeededStoreState(useRightPanelStore);

      const markup = renderServerRoute();

      expect(markup).toContain('data-mock="right-panel-tabs"');
      expect(markup).toContain('data-mock="thread-terminal-panel"');
      expect(capturedProps<Record<string, unknown>>("threadTerminalPanel")["owner"]).toBe(
        "right-panel",
      );

      const panel = capturedProps<Record<string, unknown>>("threadTerminalPanel");
      expect(panel["terminalIds"]).toEqual(["term-1"]);
      expect(panel["activeTerminalId"]).toBe("term-1");
      expect(panel["cwd"]).toBe("X:/demo");
      const labels = panel["terminalLabelsById"] as ReadonlyMap<string, string>;
      expect(labels.get("term-1")).toBe("Build shell");
    });

    it("releases terminal input state only after a successful close", async () => {
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      h.knownSessions = [
        {
          target: { environmentId, threadId, terminalId: "term-1" },
          state: { summary: { label: "Build shell", cwd: "X:/demo", worktreePath: null } },
        },
      ];
      useRightPanelStore.getState().openTerminal(threadRef, "term-1");
      publishSeededStoreState(useRightPanelStore);
      h.commandResults["terminal.close"] = () => AsyncResult.success(undefined);

      renderServerRoute();
      const panel = capturedProps<Record<string, unknown>>("threadTerminalPanel");
      const onCloseTerminal = panel["onCloseTerminal"] as (terminalId: string) => void;
      onCloseTerminal("term-1");
      await Promise.resolve();
      await Promise.resolve();

      expect(h.releasedTerminalInputs).toEqual([{ environmentId, threadId, terminalId: "term-1" }]);
    });

    it("falls back to exit and retains terminal input state when close fails", async () => {
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      h.knownSessions = [
        {
          target: { environmentId, threadId, terminalId: "term-1" },
          state: { summary: { label: "Build shell", cwd: "X:/demo", worktreePath: null } },
        },
      ];
      useRightPanelStore.getState().openTerminal(threadRef, "term-1");
      publishSeededStoreState(useRightPanelStore);
      h.commandResults["terminal.close"] = () =>
        AsyncResult.failure(Cause.fail(new Error("close rejected")));

      renderServerRoute();
      const panel = capturedProps<Record<string, unknown>>("threadTerminalPanel");
      const onCloseTerminal = panel["onCloseTerminal"] as (terminalId: string) => Promise<unknown>;
      await expect(onCloseTerminal("term-1")).resolves.toBe("exit-fallback");

      expect(h.releasedTerminalInputs).toEqual([]);
      expect(h.commandCalls.filter((call) => call.key === "terminal.write")).toEqual([
        {
          key: "terminal.write",
          input: {
            environmentId,
            input: { threadId, terminalId: "term-1", data: "exit\n" },
          },
        },
      ]);
    });
  });

  describe("when: a center terminal panel is active", () => {
    it("hides the host chat column and mounts the center terminal panel", () => {
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(makeThread());
      seedGitStatus(true);
      useCenterPanelStore.getState().openTerminalPanel(threadRef, "term-9");
      publishSeededStoreState(useCenterPanelStore);

      const markup = renderServerRoute();

      expect(markup).toContain('data-mock="center-panel-workspace"');
      expect(markup).toContain('data-mock="center-terminal-panel"');

      const centerTerminal = capturedProps<Record<string, unknown>>("centerTerminalPanel");
      expect(centerTerminal["surface"]).toMatchObject({
        kind: "terminal",
        terminalId: "term-9",
      });
    });
  });
});

describe("ChatView handlers (captured from mocked children)", () => {
  it("fences two mounted views from replaying the unresolved command independently", async () => {
    vi.unstubAllGlobals();
    Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
    Object.defineProperty(Element.prototype, "getAnimations", {
      configurable: true,
      value: () => [],
    });
    seedConnectedServerThread(makeThread({ branch: "main" }));
    const file = new File(["fake"], "shared.png", { type: "image/png" });
    const image = {
      type: "image" as const,
      id: "shared-file",
      name: file.name,
      mimeType: file.type,
      sizeBytes: 4,
      file,
      previewUrl: "blob:shared",
    };
    h.commandResults["thread.stageAttachments"] = () =>
      AsyncResult.success({
        _tag: "staged",
        attachments: [
          {
            type: "image",
            id: image.id,
            name: image.name,
            mimeType: image.mimeType,
            sizeBytes: 4,
            uploadId: "shared-upload",
          },
        ],
      });
    let calls = 0;
    let settleReplay!: (result: unknown) => void;
    h.commandResults["thread.startTurn"] = () =>
      ++calls === 1
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
    const containers = [document.createElement("div"), document.createElement("div")];
    const roots = containers.map((container) => {
      document.body.append(container);
      return createRoot(container);
    });
    const mount = async (index: number) => {
      await act(async () =>
        roots[index]!.render(
          <ChatView environmentId={environmentId} threadId={threadId} routeKind="server" />,
        ),
      );
      const composer = capturedProps<Record<string, unknown>>("chatComposer");
      (composer.composerRef as RefObject<ChatComposerHandle | null>).current = composerHandle({
        getSendContext: () => ({ ...composerHandle().getSendContext(), attachments: [image] }),
      });
      (composer.promptRef as RefObject<string>).current = "original shared send";
      return composer.onSend as () => Promise<void>;
    };
    let replay: Promise<void> | null = null;
    try {
      const sendFirst = await mount(0);
      await act(async () => sendFirst());
      const sendSecond = await mount(1);
      await act(async () => {
        replay = sendFirst();
        await Promise.resolve();
      });
      await vi.waitFor(() => expect(commandCallsFor("thread.startTurn")).toHaveLength(2));
      await act(async () => sendSecond());
      expect(commandCallsFor("thread.startTurn")).toHaveLength(2);
      expect(commandCallsFor("thread.stageAttachments")).toHaveLength(1);
      await act(async () => {
        settleReplay(AsyncResult.success(undefined));
        await replay;
      });
      const requests = commandCallsFor("thread.startTurn");
      expect(requests[1]!.input).toEqual(requests[0]!.input);
    } finally {
      if (settleReplay) settleReplay(AsyncResult.success(undefined));
      await act(async () => Promise.all(roots.map((root) => root.unmount())));
      for (const container of containers) container.remove();
    }
  });
  it.each(
    [false, true].flatMap((pendingAtUnmount) =>
      [false, true].map((notAccepted) => ({ pendingAtUnmount, notAccepted })),
    ),
  )(
    "retains the exact logical admission across actual unmount/remount (pending $pendingAtUnmount, not accepted $notAccepted)",
    async ({ pendingAtUnmount, notAccepted }) => {
      vi.unstubAllGlobals();
      Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
      Object.defineProperty(Element.prototype, "getAnimations", {
        configurable: true,
        value: () => [],
      });
      seedConnectedServerThread(makeThread({ branch: "main" }));
      const file = new File(["fake"], "retained.png", { type: "image/png" });
      const image = {
        type: "image" as const,
        id: "retained-file",
        name: file.name,
        mimeType: file.type,
        sizeBytes: 4,
        file,
        previewUrl: "blob:retained",
      };
      h.commandResults["thread.stageAttachments"] = () =>
        AsyncResult.success({
          _tag: "staged",
          attachments: [
            {
              type: "image",
              id: image.id,
              name: image.name,
              mimeType: image.mimeType,
              sizeBytes: 4,
              uploadId: "original-upload",
            },
          ],
        });
      const lost = AsyncResult.failure(
        Cause.fail(
          new RpcClientError.RpcClientError({
            reason: new RpcClientError.RpcClientDefect({
              message: "reply lost",
              cause: new Error(),
            }),
          }),
        ),
      );
      let calls = 0;
      let settleOld!: (result: unknown) => void;
      h.commandResults["thread.startTurn"] = () =>
        ++calls === 1
          ? pendingAtUnmount
            ? new Promise((resolve) => {
                settleOld = resolve;
              })
            : lost
          : calls === 2 && notAccepted
            ? AsyncResult.failure(
                Cause.fail(
                  new UploadError({ reason: "not_found", message: "the upload is unavailable" }),
                ),
              )
            : AsyncResult.success(undefined);
      const firstContainer = document.createElement("div");
      document.body.append(firstContainer);
      const first = createRoot(firstContainer);
      let second: Root | null = null;
      try {
        await act(async () =>
          first.render(
            <ChatView environmentId={environmentId} threadId={threadId} routeKind="server" />,
          ),
        );
        let composer = capturedProps<Record<string, unknown>>("chatComposer");
        expect(composer).toMatchObject({
          isSendBusy: false,
          isConnecting: false,
          providerBindingConflictReason: null,
        });
        const getSendContext = vi.fn(() => ({
          ...composerHandle().getSendContext(),
          attachments: [image],
        }));
        (composer.composerRef as RefObject<ChatComposerHandle | null>).current = composerHandle({
          getSendContext,
        });
        (composer.promptRef as RefObject<string>).current = "original text";
        expect(
          (composer.composerRef as RefObject<ChatComposerHandle | null>).current?.getSendContext()
            .attachments,
        ).toHaveLength(1);
        let sending!: Promise<void>;
        await act(async () => {
          sending = (composer.onSend as () => Promise<void>)();
          await Promise.resolve();
        });
        expect(getSendContext).toHaveBeenCalledTimes(2);
        expect(h.commandCalls.map((call) => call.key)).toContain("thread.stageAttachments");
        await vi.waitFor(() => expect(commandCallsFor("thread.startTurn")).toHaveLength(1));
        if (!pendingAtUnmount) await act(async () => sending);
        await act(async () => first.unmount());
        const secondContainer = document.createElement("div");
        document.body.append(secondContainer);
        second = createRoot(secondContainer);
        await act(async () =>
          second!.render(
            <ChatView environmentId={environmentId} threadId={threadId} routeKind="server" />,
          ),
        );
        composer = capturedProps<Record<string, unknown>>("chatComposer");
        (composer.composerRef as RefObject<ChatComposerHandle | null>).current = composerHandle({
          getSendContext: () => ({ ...composerHandle().getSendContext(), attachments: [image] }),
        });
        if (pendingAtUnmount) {
          (composer.promptRef as RefObject<string>).current = "another send";
          await act(async () => (composer.onSend as () => Promise<void>)());
          const countBeforeOldOutcome = commandCallsFor("thread.startTurn").length;
          await act(async () => {
            settleOld(lost);
            await sending;
          });
          expect(countBeforeOldOutcome).toBe(1);
        }
        await act(async () =>
          useComposerDraftStore.getState().setPrompt(threadRef, "fresh edited work"),
        );
        composer = capturedProps<Record<string, unknown>>("chatComposer");
        (composer.promptRef as RefObject<string>).current = "fresh edited work";
        await act(async () => (composer.onSend as () => Promise<void>)());
        const requests = commandCallsFor("thread.startTurn");
        expect(requests).toHaveLength(2);
        expect(requests[1]!.input).toEqual(requests[0]!.input);
        expect(commandCallsFor("thread.stageAttachments")).toHaveLength(1);
        expect(useComposerDraftStore.getState().getComposerDraft(threadRef)?.prompt).toBe(
          "fresh edited work",
        );
        if (notAccepted) {
          composer = capturedProps<Record<string, unknown>>("chatComposer");
          (composer.promptRef as RefObject<string>).current = "fresh edited work";
          await act(async () => (composer.onSend as () => Promise<void>)());
          const freshRequests = commandCallsFor("thread.startTurn");
          expect(freshRequests).toHaveLength(3);
          expect(
            (freshRequests[2]!.input as { input: { commandId: string } }).input.commandId,
          ).not.toBe((freshRequests[0]!.input as { input: { commandId: string } }).input.commandId);
          expect(commandCallsFor("thread.stageAttachments")).toHaveLength(2);
        }
      } finally {
        if (pendingAtUnmount && settleOld) settleOld(lost);
        if (second) await act(async () => second!.unmount());
        firstContainer.remove();
      }
    },
  );
  function seedConnectedServerThread(thread: Thread = makeThread()): void {
    seedEnvironment(makeEnvironmentPresentation());
    seedProject(makeProject());
    seedServerThread(thread);
    seedGitStatus(true);
  }

  function composerHandle(overrides: Partial<ChatComposerHandle> = {}): ChatComposerHandle {
    return {
      focusAtEnd: () => undefined,
      resetCursorState: () => undefined,
      addTerminalContext: () => false,
      getSendContext: () => ({
        attachments: [],
        terminalContexts: [],
        elementContexts: [],
        previewAnnotations: [],
        reviewComments: [],
        selectedProvider: ProviderDriverKind.make("codex"),
        selectedModel: "gpt-5.4",
        selectedProviderModels: codexProvider.models,
        selectedPromptEffort: null,
        selectedModelSelection: { instanceId: codexInstanceId, model: "gpt-5.4" },
      }),
      ...overrides,
    } as ChatComposerHandle;
  }

  function commandCallsFor(key: string): Array<{ key: string; input: unknown }> {
    return h.commandCalls.filter((call) => call.key === key);
  }

  it("provides message-scoped delivery resolution state to the timeline", () => {
    seedConnectedServerThread();
    renderServerRoute();

    const timeline = capturedProps<Record<string, unknown>>("messagesTimeline");
    expect(timeline["onResolveTurnDelivery"]).toBeTypeOf("function");
    expect(timeline["resolvingTurnDeliveryMessageId"]).toBeNull();
  });

  it("onInterrupt targets the running turn of the active session", async () => {
    const runningTurnId = TurnId.make("turn-running");
    seedConnectedServerThread(
      makeThread({
        session: {
          threadId,
          status: "running",
          providerName: "codex",
          providerInstanceId: codexInstanceId,
          runtimeMode: "full-access",
          activeTurnId: runningTurnId,
          lastError: null,
          updatedAt: now,
        },
      }),
    );

    renderServerRoute();
    const composer = capturedProps<Record<string, unknown>>("chatComposer");
    const onInterrupt = composer["onInterrupt"] as () => Promise<void>;
    await onInterrupt();

    expect(commandCallsFor("thread.interruptTurn")).toEqual([
      {
        key: "thread.interruptTurn",
        input: { environmentId, input: { threadId, turnId: runningTurnId } },
      },
    ]);
  });

  it("onRespondToApproval submits the decision for the active thread", async () => {
    seedConnectedServerThread();

    renderServerRoute();
    const composer = capturedProps<Record<string, unknown>>("chatComposer");
    const onRespondToApproval = composer["onRespondToApproval"] as (
      requestId: ApprovalRequestId,
      decision: string,
    ) => Promise<unknown>;
    const requestId = ApprovalRequestId.make("approval-1");
    await onRespondToApproval(requestId, "approve");

    expect(commandCallsFor("thread.respondToApproval")).toEqual([
      {
        key: "thread.respondToApproval",
        input: { environmentId, input: { threadId, requestId, decision: "approve" } },
      },
    ]);
  });

  it("commits composer options through thread metadata before they become local state", async () => {
    seedConnectedServerThread();
    renderServerRoute();

    const composer = capturedProps<Record<string, unknown>>("chatComposer");
    const onCommitModelSelection = composer["onCommitModelSelection"] as (
      selection: ModelSelection,
    ) => Promise<void>;
    const selection: ModelSelection = {
      instanceId: codexInstanceId,
      model: "gpt-5.4",
      options: [{ id: "fastMode", value: true }],
    };

    await onCommitModelSelection(selection);

    expect(commandCallsFor("thread.updateMetadata")).toContainEqual({
      key: "thread.updateMetadata",
      input: { environmentId, input: { threadId, modelSelection: selection } },
    });
  });

  it("rejects model selection outside an exact missing-instance lock", () => {
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
    seedEnvironment(
      makeEnvironmentPresentation({
        serverConfig: {
          providers: [collidingProvider],
          environment: { label: "Local" },
        },
      }),
    );
    seedProject(makeProject());
    seedServerThread(
      makeThread({
        modelSelection: { instanceId: boundInstanceId, model: "gpt-5.4" },
        messages: [
          {
            id: MessageId.make("started-handler-exact-lock"),
            role: "user",
            text: "Started",
            turnId: null,
            createdAt: now,
            updatedAt: now,
            streaming: false,
          },
        ],
      }),
    );
    useComposerDraftStore.getState().setModelSelection(threadRef, {
      instanceId: boundInstanceId,
      model: "gpt-5.4",
    });
    publishSeededStoreState(useComposerDraftStore);
    seedGitStatus(true);

    renderServerRoute();
    const composer = capturedProps<Record<string, unknown>>("chatComposer");
    const onProviderModelSelect = composer["onProviderModelSelect"] as (
      instanceId: ProviderInstanceId,
      model: string,
    ) => void;
    onProviderModelSelect(staleInstanceId, "collision-model");

    const draft = useComposerDraftStore.getState().getComposerDraft(threadRef);
    expect(draft?.activeProvider).toBe("codex_personal");
    expect(draft?.modelSelectionByProvider[staleInstanceId]).toBeUndefined();
  });

  it("onRespondToApproval interrupts the active turn when cancellation is requested", async () => {
    const runningTurnId = TurnId.make("turn-running");
    seedConnectedServerThread(
      makeThread({
        session: {
          threadId,
          status: "running",
          providerName: "cursor",
          providerInstanceId: ProviderInstanceId.make("cursor"),
          runtimeMode: "full-access",
          activeTurnId: runningTurnId,
          lastError: null,
          updatedAt: now,
        },
      }),
    );

    renderServerRoute();
    const composer = capturedProps<Record<string, unknown>>("chatComposer");
    const onRespondToApproval = composer["onRespondToApproval"] as (
      requestId: ApprovalRequestId,
      decision: string,
    ) => Promise<unknown>;
    await onRespondToApproval(ApprovalRequestId.make("approval-1"), "cancel");

    expect(commandCallsFor("thread.respondToApproval")).toEqual([]);
    expect(commandCallsFor("thread.interruptTurn")).toEqual([
      {
        key: "thread.interruptTurn",
        input: { environmentId, input: { threadId, turnId: runningTurnId } },
      },
    ]);
  });

  it("onSend starts a turn with the formatted prompt and auto-title", async () => {
    seedConnectedServerThread();

    renderServerRoute();
    const composer = capturedProps<Record<string, unknown>>("chatComposer");
    const composerRef = composer["composerRef"] as RefObject<ChatComposerHandle | null>;
    composerRef.current = composerHandle();
    const promptRef = composer["promptRef"] as RefObject<string>;
    promptRef.current = "hello world";

    const onSend = composer["onSend"] as () => Promise<void>;
    await onSend();

    const titleCalls = commandCallsFor("thread.updateMetadata");
    expect(titleCalls.length).toBeGreaterThanOrEqual(1);
    expect(titleCalls[0]!.input).toMatchObject({
      environmentId,
      input: { threadId, title: "hello world" },
    });

    const startCalls = commandCallsFor("thread.startTurn");
    expect(startCalls).toHaveLength(1);
    expect(startCalls[0]!.input).toMatchObject({
      environmentId,
      input: {
        threadId,
        message: { role: "user", text: "hello world", attachments: [] },
        modelSelection: { instanceId: codexInstanceId, model: "gpt-5.4" },
        titleSeed: "hello world",
        runtimeMode: "full-access",
        interactionMode: "default",
      },
    });
    // Server threads with no worktree bootstrap never send a bootstrap payload.
    expect(
      (startCalls[0]!.input as { input: Record<string, unknown> }).input["bootstrap"],
    ).toBeUndefined();
  });

  it("keeps an exact session account in the label, picker guard, and outgoing turn", async () => {
    const customInstanceId = ProviderInstanceId.make("codex_personal");
    const startedThread = makeThread({
      modelSelection: { instanceId: codexInstanceId, model: "gpt-5.4" },
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
    });
    seedConnectedServerThread(startedThread);
    useComposerDraftStore.getState().setModelSelection(threadRef, {
      instanceId: customInstanceId,
      model: "gpt-5.4",
    });
    publishSeededStoreState(useComposerDraftStore);

    renderServerRoute();
    const composer = capturedProps<Record<string, unknown>>("chatComposer");
    expect(capturedProps<Record<string, unknown>>("centerWorkspace")["hostLabel"]).toBe(
      "Codex Personal",
    );
    expect(composer["lockedProvider"]).toBe("codex");
    expect(composer["providerBindingInstanceId"]).toBe("codex_personal");
    expect(composer["lockProviderPickerToActiveInstance"]).toBe(true);
    expect(composer["providerBindingConflictReason"]).toBeNull();

    const onProviderModelSelect = composer["onProviderModelSelect"] as (
      instanceId: ProviderInstanceId,
      model: string,
    ) => void;
    onProviderModelSelect(codexInstanceId, "gpt-5.4");
    expect(useComposerDraftStore.getState().getComposerDraft(threadRef)?.activeProvider).toBe(
      "codex_personal",
    );

    const composerRef = composer["composerRef"] as RefObject<ChatComposerHandle | null>;
    composerRef.current = composerHandle({
      getSendContext: () => ({
        prompt: "keep exact account",
        attachments: [],
        terminalContexts: [],
        elementContexts: [],
        previewAnnotations: [],
        reviewComments: [],
        selectedProvider: ProviderDriverKind.make("codex"),
        selectedModel: "gpt-5.4",
        selectedProviderModels: codexProvider.models,
        selectedPromptEffort: null,
        selectedModelOptionsForDispatch: [],
        selectedModelSelection: { instanceId: customInstanceId, model: "gpt-5.4" },
      }),
    });
    const promptRef = composer["promptRef"] as RefObject<string>;
    promptRef.current = "keep exact account";

    await (composer["onSend"] as () => Promise<void>)();

    expect(commandCallsFor("thread.startTurn")).toHaveLength(1);
    expect(commandCallsFor("thread.startTurn")[0]!.input).toMatchObject({
      input: {
        modelSelection: { instanceId: "codex_personal", model: "gpt-5.4" },
      },
    });
  });

  it("fails closed when live status contradicts the exact session account", async () => {
    const customInstanceId = ProviderInstanceId.make("codex_personal");
    const contradictoryProvider: ServerProvider = {
      ...codexProvider,
      instanceId: customInstanceId,
      driver: ProviderDriverKind.make("claude"),
      displayName: "Contradictory Claude",
      models: [
        {
          slug: "claude-sonnet",
          name: "Claude Sonnet",
          isCustom: false,
          capabilities: null,
        },
      ],
    };
    const conflictReason =
      'Provider instance "codex_personal" reports driver "claude", but the active session expects "codex". Sending is blocked until provider metadata agrees.';
    seedEnvironment(
      makeEnvironmentPresentation({
        serverConfig: {
          providers: [contradictoryProvider, codexProvider],
          environment: { label: "Local" },
        },
      }),
    );
    seedProject(makeProject());
    seedServerThread(
      makeThread({
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
    );
    useComposerDraftStore.getState().setModelSelection(threadRef, {
      instanceId: customInstanceId,
      model: "gpt-5.4",
    });
    publishSeededStoreState(useComposerDraftStore);
    seedGitStatus(true);

    renderServerRoute();
    const composer = capturedProps<Record<string, unknown>>("chatComposer");
    expect(capturedProps<Record<string, unknown>>("centerWorkspace")["hostLabel"]).toBe(
      "Codex Personal",
    );
    expect(composer["lockedProvider"]).toBeNull();
    expect(composer["providerBindingInstanceId"]).toBe("codex_personal");
    expect(composer["lockProviderPickerToActiveInstance"]).toBe(true);
    expect(composer["providerBindingConflictReason"]).toBe(conflictReason);
    const banners = capturedProps<{ items: ComposerBannerStackItem[] }>("composerBannerStack");
    expect(banners.items).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          id: "provider-binding-conflict:codex_personal",
          variant: "warning",
          title: "Provider session conflict",
          description: conflictReason,
        }),
      ]),
    );

    const onProviderModelSelect = composer["onProviderModelSelect"] as (
      instanceId: ProviderInstanceId,
      model: string,
    ) => void;
    onProviderModelSelect(customInstanceId, "claude-sonnet");
    const draftAfterRejectedSelection = useComposerDraftStore
      .getState()
      .getComposerDraft(threadRef);
    expect(draftAfterRejectedSelection?.activeProvider).toBe("codex_personal");
    expect(draftAfterRejectedSelection?.modelSelectionByProvider[customInstanceId]).toEqual({
      instanceId: customInstanceId,
      model: "gpt-5.4",
    });

    const getSendContext = vi.fn(() => ({
      prompt: "must stay blocked",
      attachments: [],
      terminalContexts: [],
      elementContexts: [],
      previewAnnotations: [],
      reviewComments: [],
      selectedProvider: ProviderDriverKind.make("claude"),
      selectedModel: "claude-sonnet",
      selectedProviderModels: contradictoryProvider.models,
      selectedPromptEffort: null,
      selectedModelOptionsForDispatch: [],
      selectedModelSelection: { instanceId: customInstanceId, model: "claude-sonnet" },
    }));
    const composerRef = composer["composerRef"] as RefObject<ChatComposerHandle | null>;
    composerRef.current = composerHandle({ getSendContext });
    const promptRef = composer["promptRef"] as RefObject<string>;
    promptRef.current = "must stay blocked";

    await (composer["onSend"] as () => Promise<void>)();

    expect(getSendContext).not.toHaveBeenCalled();
    expect(commandCallsFor("thread.startTurn")).toEqual([]);
  });

  it("canonicalizes legacy file links before starting a provider turn", async () => {
    seedConnectedServerThread();

    renderServerRoute();
    const composer = capturedProps<Record<string, unknown>>("chatComposer");
    const composerRef = composer["composerRef"] as RefObject<ChatComposerHandle | null>;
    composerRef.current = composerHandle();
    const promptRef = composer["promptRef"] as RefObject<string>;
    promptRef.current = "Inspect [main.ts](src/main.ts) and @README.md";

    await (composer["onSend"] as () => Promise<void>)();

    expect(commandCallsFor("thread.startTurn")[0]!.input).toMatchObject({
      input: {
        message: { text: "Inspect @src/main.ts and @README.md" },
      },
    });
  });

  it("does not rewrite normal Markdown links or historical messages", async () => {
    const legacyHistoricalText = "Previously inspected [main.ts](src/main.ts)";
    const historicalThread = makeThread({
      messages: [
        {
          id: MessageId.make("historical-message"),
          role: "user",
          text: legacyHistoricalText,
          turnId: null,
          createdAt: now,
          updatedAt: now,
          streaming: false,
        },
      ],
    });
    seedConnectedServerThread(historicalThread);

    renderServerRoute();
    const composer = capturedProps<Record<string, unknown>>("chatComposer");
    const composerRef = composer["composerRef"] as RefObject<ChatComposerHandle | null>;
    composerRef.current = composerHandle();
    const promptRef = composer["promptRef"] as RefObject<string>;
    promptRef.current = "Read [docs](https://example.com) first";

    await (composer["onSend"] as () => Promise<void>)();

    expect(commandCallsFor("thread.startTurn")[0]!.input).toMatchObject({
      input: {
        message: { text: "Read [docs](https://example.com) first" },
      },
    });
    expect(historicalThread.messages[0]?.text).toBe(legacyHistoricalText);
  });

  it("onSend reports a failure from the turn start as a thread error", async () => {
    seedConnectedServerThread();
    h.commandResults["thread.startTurn"] = () =>
      AsyncResult.failure(Cause.fail(new Error("turn rejected by server")));

    renderServerRoute();
    const composer = capturedProps<Record<string, unknown>>("chatComposer");
    const composerRef = composer["composerRef"] as RefObject<ChatComposerHandle | null>;
    composerRef.current = composerHandle();
    const promptRef = composer["promptRef"] as RefObject<string>;
    promptRef.current = "will fail";

    const onSend = composer["onSend"] as () => Promise<void>;
    await onSend();

    expect(commandCallsFor("thread.startTurn")).toHaveLength(1);
    const setThreadError = composer["setThreadError"];
    expect(typeof setThreadError).toBe("function");
  });

  it("onSend is a no-op when the environment is unavailable", async () => {
    seedEnvironment(
      makeEnvironmentPresentation({
        connection: { phase: "error", error: "socket closed", traceId: null },
      }),
    );
    seedProject(makeProject());
    seedServerThread(makeThread());
    seedGitStatus(true);

    renderServerRoute();
    const composer = capturedProps<Record<string, unknown>>("chatComposer");
    const composerRef = composer["composerRef"] as RefObject<ChatComposerHandle | null>;
    composerRef.current = composerHandle();
    const promptRef = composer["promptRef"] as RefObject<string>;
    promptRef.current = "hello";

    const onSend = composer["onSend"] as () => Promise<void>;
    await onSend();

    expect(commandCallsFor("thread.startTurn")).toHaveLength(0);
  });

  it("onSend promotes a draft session by bootstrapping thread creation", async () => {
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
        { threadId, createdAt: now, envMode: "local" },
      );
    publishSeededStoreState(useComposerDraftStore);

    renderToStaticMarkup(
      <ChatView
        environmentId={environmentId}
        threadId={threadId}
        routeKind="draft"
        draftId={draftId}
      />,
    );

    const composer = capturedProps<Record<string, unknown>>("chatComposer");
    const composerRef = composer["composerRef"] as RefObject<ChatComposerHandle | null>;
    composerRef.current = composerHandle();
    const promptRef = composer["promptRef"] as RefObject<string>;
    promptRef.current = "kick off draft";

    const onSend = composer["onSend"] as () => Promise<void>;
    await onSend();

    // Draft sends never update metadata first; they bootstrap the thread.
    expect(commandCallsFor("thread.updateMetadata")).toHaveLength(0);
    const startCalls = commandCallsFor("thread.startTurn");
    expect(startCalls).toHaveLength(1);
    expect(startCalls[0]!.input).toMatchObject({
      environmentId,
      input: {
        threadId,
        titleSeed: "kick off draft",
        bootstrap: {
          createThread: {
            projectId,
            title: "kick off draft",
            runtimeMode: "full-access",
            interactionMode: "default",
            branch: null,
            worktreePath: null,
            createdAt: now,
          },
        },
      },
    });
  });

  it("getModelDisabledReason blocks switching providers on a started restricted session", () => {
    const grokInstanceId = ProviderInstanceId.make("grok");
    const grokProvider: ServerProvider = {
      ...codexProvider,
      instanceId: grokInstanceId,
      driver: ProviderDriverKind.make("grok"),
      requiresNewThreadForModelChange: true,
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
    seedServerThread(
      makeThread({
        session: {
          threadId,
          status: "ready",
          providerName: "codex",
          providerInstanceId: codexInstanceId,
          runtimeMode: "full-access",
          activeTurnId: null,
          lastError: null,
          updatedAt: now,
        },
      }),
    );
    seedGitStatus(true);

    renderServerRoute();
    const composer = capturedProps<Record<string, unknown>>("chatComposer");
    const getModelDisabledReason = composer["getModelDisabledReason"] as (
      instanceId: ProviderInstanceId,
      model: string,
    ) => string | null;

    expect(getModelDisabledReason(codexInstanceId, "gpt-5.4")).toBeNull();
    expect(getModelDisabledReason(grokInstanceId, "grok-build")).toBe(
      "This provider does not allow switching models after a conversation has started. Start a new thread to use this model.",
    );
  });

  it("handleRuntimeModeChange stores the next runtime mode in the composer draft", () => {
    seedConnectedServerThread();

    renderServerRoute();
    const composer = capturedProps<Record<string, unknown>>("chatComposer");
    const handleRuntimeModeChange = composer["handleRuntimeModeChange"] as (
      mode: string,
    ) => unknown;
    handleRuntimeModeChange("approval-required");

    expect(useComposerDraftStore.getState().getComposerDraft(threadRef)?.runtimeMode).toBe(
      "approval-required",
    );
    // Persisting to the server only happens on the next turn start.
    expect(commandCallsFor("thread.setRuntimeMode")).toHaveLength(0);
  });

  it("handleInteractionModeChange stores the next interaction mode in the composer draft", () => {
    seedConnectedServerThread();

    renderServerRoute();
    const composer = capturedProps<Record<string, unknown>>("chatComposer");
    const toggleInteractionMode = composer["toggleInteractionMode"] as () => unknown;
    toggleInteractionMode();

    expect(useComposerDraftStore.getState().getComposerDraft(threadRef)?.interactionMode).toBe(
      "plan",
    );
  });
});

describe("ChatView file editing registry lifetime", () => {
  interface MockFilePreviewView {
    instanceId: number;
    viewState: {
      annotationEntryIds: string[];
      selectedRange: { start: number; end: number } | null;
    };
    setViewState: (state: {
      annotationEntryIds: string[];
      selectedRange: { start: number; end: number } | null;
    }) => void;
    submitAnnotation: (entryId: string) => void;
    removeAnnotation: (entryId: string) => void;
  }

  function prepareDomTest(): void {
    vi.unstubAllGlobals();
    Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  }

  async function renderStrictChatView(
    root: Root,
    nextEnvironmentId: EnvironmentId,
    nextThreadId: ThreadId,
  ): Promise<FileEditingSessionRegistry<ReturnType<typeof fakeEditingSession>>> {
    delete h.captured["filePreviewPanel"];
    await act(async () => {
      root.render(
        <StrictMode>
          <ChatView environmentId={nextEnvironmentId} threadId={nextThreadId} routeKind="server" />
        </StrictMode>,
      );
      await vi.dynamicImportSettled();
      await Promise.resolve();
    });
    return capturedProps<{
      editingSessions: FileEditingSessionRegistry<ReturnType<typeof fakeEditingSession>>;
    }>("filePreviewPanel").editingSessions;
  }

  function seedProjectAndThread(project: Project, thread: Thread, surface: "file" | "files"): void {
    h.projectsByKey.set(`${project.environmentId}:${project.id}`, project);
    h.allProjects = [...h.allProjects, project];
    h.threadsByKey.set(`${thread.environmentId}:${thread.id}`, thread);
    h.threadRefs = [...h.threadRefs, scopeThreadRef(thread.environmentId, thread.id)];
    const nextThreadRef = scopeThreadRef(thread.environmentId, thread.id);
    if (surface === "file") {
      useRightPanelStore.getState().openFile(nextThreadRef, "src/app.ts");
    } else {
      useRightPanelStore.getState().open(nextThreadRef, "files");
    }
  }

  it("remounts thread-local file view state while reusing the project editing session", async () => {
    prepareDomTest();
    const secondThreadId = ThreadId.make("thread-2");
    const project = makeProject();
    const firstThread = makeThread();
    const secondThread = makeThread({ id: secondThreadId });
    h.environments = [makeEnvironmentPresentation()];
    h.primaryEnvironment = h.environments[0]!;
    seedProjectAndThread(project, firstThread, "file");
    seedProjectAndThread(project, secondThread, "file");
    seedGitStatus(true);

    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);
    try {
      const firstRegistry = await renderStrictChatView(root, environmentId, threadId);
      const session = firstRegistry.getOrCreate("src/app.ts", () =>
        fakeEditingSession("src/app.ts"),
      );
      session.editor.history.push("thread-a edit");
      const firstView = capturedProps<{ mockView: MockFilePreviewView }>(
        "filePreviewPanel",
      ).mockView;
      await act(async () => {
        firstView.setViewState({
          annotationEntryIds: ["thread-a-comment"],
          selectedRange: { start: 3, end: 5 },
        });
      });
      const seededFirstView = capturedProps<{ mockView: MockFilePreviewView }>(
        "filePreviewPanel",
      ).mockView;
      expect(seededFirstView.viewState).toEqual({
        annotationEntryIds: ["thread-a-comment"],
        selectedRange: { start: 3, end: 5 },
      });
      const revealEventBeforeSwitch = h.filePreviewRevealEvents.at(-1);
      const revealCountBeforeSwitch = h.filePreviewRevealEvents.length;

      const secondRegistry = await renderStrictChatView(root, environmentId, secondThreadId);
      const secondPanel = capturedProps<{
        composerDraftTarget: unknown;
        mockView: MockFilePreviewView;
      }>("filePreviewPanel");

      expect(secondPanel.composerDraftTarget).toEqual(
        scopeThreadRef(environmentId, secondThreadId),
      );
      expect(secondPanel.mockView.instanceId).not.toBe(seededFirstView.instanceId);
      expect(secondPanel.mockView.viewState).toEqual({
        annotationEntryIds: [],
        selectedRange: null,
      });
      secondPanel.mockView.submitAnnotation("thread-a-comment");
      secondPanel.mockView.removeAnnotation("thread-a-comment");
      expect(h.filePreviewCommentActions).toEqual([]);
      expect(h.filePreviewRevealEvents.length).toBeGreaterThan(revealCountBeforeSwitch);
      expect(h.filePreviewRevealEvents.at(-1)).toEqual(revealEventBeforeSwitch);

      expect(secondRegistry).toBe(firstRegistry);
      expect(secondRegistry.get("src/app.ts")).toBe(session);
      expect(secondRegistry.get("src/app.ts")?.editor).toBe(session.editor);
      expect(session.editor.history).toEqual(["thread-a edit"]);
    } finally {
      await act(async () => root.unmount());
      container.remove();
    }
  });

  it("reuses one registry across same-workspace project threads and replaces it for workspace, project, and environment changes", async () => {
    prepareDomTest();
    const secondThreadId = ThreadId.make("thread-2");
    const worktreeThreadId = ThreadId.make("thread-worktree");
    const secondProjectId = ProjectId.make("project-2");
    const secondEnvironmentId = EnvironmentId.make("environment-remote");
    const thirdThreadId = ThreadId.make("thread-3");
    const fourthThreadId = ThreadId.make("thread-4");
    const firstProject = makeProject();
    const secondProject = makeProject({
      id: secondProjectId,
      title: "Other project",
      workspaceRoot: firstProject.workspaceRoot,
    });
    const remoteProject = makeProject({
      environmentId: secondEnvironmentId,
      title: "Remote project",
    });
    const firstThread = makeThread();
    const sameProjectThread = makeThread({ id: secondThreadId });
    const worktreeThread = makeThread({
      id: worktreeThreadId,
      worktreePath: "X:/demo-worktree",
    });
    const otherProjectThread = makeThread({
      id: thirdThreadId,
      projectId: secondProjectId,
    });
    const remoteThread = makeThread({
      id: fourthThreadId,
      environmentId: secondEnvironmentId,
    });
    h.environments = [
      makeEnvironmentPresentation(),
      makeEnvironmentPresentation({ environmentId: secondEnvironmentId }),
    ];
    h.primaryEnvironment = h.environments[0]!;
    seedProjectAndThread(firstProject, firstThread, "file");
    seedProjectAndThread(firstProject, sameProjectThread, "file");
    seedProjectAndThread(firstProject, worktreeThread, "file");
    seedProjectAndThread(secondProject, otherProjectThread, "file");
    seedProjectAndThread(remoteProject, remoteThread, "file");
    seedGitStatus(true);

    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);
    try {
      const firstRegistry = await renderStrictChatView(root, environmentId, threadId);
      const firstDispose = vi.spyOn(firstRegistry, "dispose");

      const sameProjectRegistry = await renderStrictChatView(root, environmentId, secondThreadId);
      expect(sameProjectRegistry).toBe(firstRegistry);
      expect(firstDispose).not.toHaveBeenCalled();

      const worktreeRegistry = await renderStrictChatView(root, environmentId, worktreeThreadId);
      await vi.waitFor(() => expect(firstDispose).toHaveBeenCalledOnce());
      expect(worktreeRegistry).not.toBe(firstRegistry);
      const worktreeDispose = vi.spyOn(worktreeRegistry, "dispose");

      const otherProjectRegistry = await renderStrictChatView(root, environmentId, thirdThreadId);
      await vi.waitFor(() => expect(worktreeDispose).toHaveBeenCalledOnce());
      expect(otherProjectRegistry).not.toBe(worktreeRegistry);
      const otherProjectDispose = vi.spyOn(otherProjectRegistry, "dispose");

      const remoteRegistry = await renderStrictChatView(root, secondEnvironmentId, fourthThreadId);
      await vi.waitFor(() => expect(otherProjectDispose).toHaveBeenCalledOnce());
      expect(remoteRegistry).not.toBe(otherProjectRegistry);
      const remoteDispose = vi.spyOn(remoteRegistry, "dispose");

      await act(async () => root.unmount());
      await vi.waitFor(() => expect(remoteDispose).toHaveBeenCalledOnce());
      container.remove();
    } finally {
      if (container.isConnected) {
        await act(async () => root.unmount());
        container.remove();
      }
    }
  });

  it.each([
    ["rename", "saved"],
    ["rename", "failed"],
    ["delete", "saved"],
    ["delete", "failed"],
  ] as const)(
    "keeps the outgoing session in the shared registry for an incoming %s after a %s close",
    async (operation, settleResult) => {
      prepareDomTest();
      const secondThreadId = ThreadId.make("thread-2");
      const project = makeProject();
      const firstThread = makeThread();
      const secondThread = makeThread({ id: secondThreadId });
      h.environments = [makeEnvironmentPresentation()];
      h.primaryEnvironment = h.environments[0]!;
      seedProjectAndThread(project, firstThread, "file");
      seedProjectAndThread(project, secondThread, "files");
      seedGitStatus(true);

      const container = document.createElement("div");
      document.body.append(container);
      const root = createRoot(container);
      try {
        const outgoingRegistry = await renderStrictChatView(root, environmentId, threadId);
        const session = outgoingRegistry.getOrCreate("src/app.ts", () =>
          fakeEditingSession("src/app.ts"),
        );
        const settlement = deferredResult<"saved" | "failed">();
        session.settle.mockReturnValueOnce(settlement.promise);

        const incomingRegistry = await renderStrictChatView(root, environmentId, secondThreadId);
        let acquisitionCompleted = false;
        const acquisition = incomingRegistry
          .beginPathMutation(
            operation === "rename"
              ? {
                  kind: "rename",
                  fromRelativePath: "src/app.ts",
                  toRelativePath: "src/renamed.ts",
                }
              : { kind: "delete", relativePath: "src/app.ts" },
          )
          .then((lease) => {
            acquisitionCompleted = true;
            return lease;
          });
        await Promise.resolve();
        const waitedForOutgoingClose = !acquisitionCompleted;

        settlement.resolve(settleResult);
        const lease = await acquisition;

        expect(incomingRegistry).toBe(outgoingRegistry);
        expect(waitedForOutgoingClose).toBe(true);
        expect(session.settle).toHaveBeenCalledOnce();
        if (settleResult === "failed") {
          expect(lease).toBeNull();
          expect(incomingRegistry.get("src/app.ts")).toBe(session);
          expect(session.rename).not.toHaveBeenCalled();
          expect(session.discardPendingSave).not.toHaveBeenCalled();
          expect(session.dispose).not.toHaveBeenCalled();
          return;
        }

        expect(lease).not.toBeNull();
        if (operation === "rename") {
          lease!.commitRename("src/renamed.ts");
          expect(session.rename).toHaveBeenCalledWith("src/renamed.ts");
        } else {
          lease!.commitDelete();
          expect(session.discardPendingSave).toHaveBeenCalledOnce();
        }
        lease!.release();
        await vi.waitFor(() => expect(session.dispose).toHaveBeenCalledOnce());
        expect(incomingRegistry.get("src/app.ts")).toBeUndefined();
        expect(session.settle).toHaveBeenCalledOnce();
      } finally {
        await act(async () => root.unmount());
        container.remove();
      }
    },
  );
});

type _AssertRouteProps = ComponentProps<typeof ChatView>;

describe("ChatView adopted workspace availability", () => {
  it("guards a stale script keybinding and a deferred terminal open with the latest host availability", async () => {
    const script = {
      id: "build",
      name: "Build",
      command: "vp build",
      icon: "build" as const,
      runOnWorktreeCreate: false,
    };
    seedEnvironment(makeEnvironmentPresentation());
    seedProject(makeProject({ scripts: [script] }));
    seedServerThread(makeThread({ worktreePath: "/external/repo-feature" }));
    seedGitStatus(true);
    h.atomValuesByKey.set("atom:keybindings", [
      {
        command: "script.build.run",
        shortcut: {
          key: "p",
          metaKey: false,
          ctrlKey: true,
          shiftKey: false,
          altKey: false,
          modKey: false,
        },
      },
    ]);
    const terminalOpen = deferredResult<ReturnType<typeof AsyncResult.success<void>>>();
    h.commandResults["terminal.open"] = () => terminalOpen.promise;
    const setAvailability = (availability: "present" | "missing-registered") => {
      h.queryDataByKey.set("worktree.catalog", {
        adoptedWorkspaces: [
          {
            threadId,
            path: "/external/repo-feature",
            branch: "feature/adopted",
            availability,
            registrationState: "registered",
            locked: false,
            lockReason: null,
          },
        ],
      });
    };
    setAvailability("present");

    vi.unstubAllGlobals();
    Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
    Object.defineProperty(Element.prototype, "getAnimations", {
      configurable: true,
      value: () => [],
    });
    const addWindowListener = vi.spyOn(window, "addEventListener");
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);
    const render = async () => {
      await act(async () => {
        root.render(
          <ChatView
            environmentId={environmentId}
            threadId={threadId}
            routeKind="server"
            reserveTitleBarControlInset
          />,
        );
        await Promise.resolve();
      });
    };

    try {
      await render();
      const staleKeydownHandler = addWindowListener.mock.calls.findLast(
        ([eventName]) => eventName === "keydown",
      )?.[1];
      expect(staleKeydownHandler).toBeTypeOf("function");
      const shortcut = () =>
        new KeyboardEvent("keydown", { key: "p", ctrlKey: true, bubbles: true });

      await act(async () => {
        (staleKeydownHandler as EventListener)(shortcut());
        await Promise.resolve();
      });
      expect(h.commandCalls.filter((call) => call.key === "terminal.open")).toHaveLength(1);

      setAvailability("missing-registered");
      await render();
      await act(async () => {
        (staleKeydownHandler as EventListener)(shortcut());
        await Promise.resolve();
      });
      expect(h.commandCalls.filter((call) => call.key === "terminal.open")).toHaveLength(1);

      await act(async () => terminalOpen.resolve(AsyncResult.success(undefined)));
      expect(h.commandCalls.filter((call) => call.key === "terminal.write")).toHaveLength(0);
    } finally {
      await act(async () => root.unmount());
      container.remove();
      addWindowListener.mockRestore();
    }
  });

  it("owns one host subscription and propagates availability through sibling panels across rerenders", async () => {
    const siblingThreadId = ThreadId.make("thread-adopted-sibling");
    const siblingPath = "/external/repo-feature-sibling";
    seedEnvironment(makeEnvironmentPresentation());
    seedProject(makeProject());
    seedServerThread(makeThread({ worktreePath: "/external/repo-feature" }));
    seedServerThread(
      makeThread({ id: siblingThreadId, title: "Sibling", worktreePath: siblingPath }),
    );
    seedGitStatus(true);
    useCenterPanelStore.getState().openChatPanel(threadRef, siblingThreadId, "Codex");

    const setAvailability = (
      availability: "present" | "verification-unavailable" | "missing-registered" | "removing",
    ) => {
      h.queryDataByKey.set("worktree.catalog", {
        adoptedWorkspaces: [
          {
            threadId,
            path: "/external/repo-feature",
            branch: "feature/adopted",
            availability,
            registrationState: "registered",
            locked: false,
            lockReason: null,
          },
        ],
      });
    };
    const latestSiblingComposer = () =>
      h.chatComposerProps.findLast(
        (props) => (props["activeThread"] as Thread | undefined)?.id === siblingThreadId,
      );

    setAvailability("present");
    vi.unstubAllGlobals();
    Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
    Object.defineProperty(Element.prototype, "getAnimations", {
      configurable: true,
      value: () => [],
    });
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);
    const render = async () => {
      await act(async () => {
        root.render(
          <ChatView
            environmentId={environmentId}
            threadId={threadId}
            routeKind="server"
            reserveTitleBarControlInset
          />,
        );
        await Promise.resolve();
      });
    };

    try {
      await render();
      expect(h.querySubscriptionStarts.filter((key) => key === "worktree.catalog")).toHaveLength(1);
      expect(h.activeQuerySubscriptions.get("worktree.catalog")).toBe(1);
      expect(latestSiblingComposer()?.["providerBindingConflictReason"]).toBeNull();

      setAvailability("verification-unavailable");
      await render();
      expect(latestSiblingComposer()?.["providerBindingConflictReason"]).toBeNull();
      expect(
        capturedProps<Record<string, unknown>>("chatHeaderActions")["workspaceUnavailable"],
      ).toBeNull();
      expect(h.activeQuerySubscriptions.get("worktree.catalog")).toBe(1);

      for (const availability of ["missing-registered", "removing"] as const) {
        setAvailability(availability);
        await render();
        expect(latestSiblingComposer()?.["providerBindingConflictReason"]).toEqual(
          expect.stringContaining("Retry detection"),
        );
        expect(h.activeQuerySubscriptions.get("worktree.catalog")).toBe(1);
      }

      setAvailability("present");
      await render();
      expect(latestSiblingComposer()?.["providerBindingConflictReason"]).toBeNull();
      expect(h.querySubscriptionStarts.filter((key) => key === "worktree.catalog")).toHaveLength(1);
    } finally {
      await act(async () => root.unmount());
      container.remove();
    }

    expect(h.activeQuerySubscriptions.has("worktree.catalog")).toBe(false);
    expect(h.querySubscriptionStops.filter((key) => key === "worktree.catalog")).toHaveLength(1);
  });

  it("keeps legacy workspace threads active when the catalog capability is unsupported", () => {
    seedEnvironment(
      makeEnvironmentPresentation({
        serverConfig: {
          providers: [codexProvider],
          environment: {
            label: "Legacy",
            capabilities: { activityProtocolVersion: 2, worktreeCatalog: false },
          },
        },
      }),
    );
    seedProject(makeProject());
    seedServerThread(makeThread({ worktreePath: "/legacy/worktree" }));
    h.queryDataByKey.set("worktree.catalog", {
      adoptedWorkspaces: [{ threadId, availability: "missing-registered" }],
    });

    renderServerRoute();

    expect(capturedProps<Record<string, unknown>>("chatHeaderActions")).toMatchObject({
      workspaceUnavailable: null,
    });
    expect(capturedProps<Record<string, unknown>>("chatComposer")).toMatchObject({
      providerBindingConflictReason: null,
    });
    expect(h.querySubscriptionStarts.filter((key) => key === "worktree.catalog")).toHaveLength(0);
  });

  it("uses an adopted worktree path unchanged across the active chat surface", () => {
    seedEnvironment(makeEnvironmentPresentation());
    seedProject(makeProject());
    seedServerThread(makeThread({ worktreePath: "/external/repo-feature" }));
    seedGitStatus(true);
    h.queryDataByKey.set("worktree.catalog", {
      adoptedWorkspaces: [
        {
          threadId,
          path: "/external/repo-feature",
          branch: "feature/adopted",
          availability: "present",
          registrationState: "registered",
          locked: false,
          lockReason: null,
        },
      ],
    });

    renderServerRoute();

    expect(capturedProps<Record<string, unknown>>("chatHeaderActions")).toMatchObject({
      gitCwd: "/external/repo-feature",
      openInCwd: "/external/repo-feature",
      workspaceUnavailable: null,
    });
    expect(capturedProps<Record<string, unknown>>("chatComposer")).toMatchObject({
      providerBindingConflictReason: null,
    });
  });

  it.each(["missing-registered", "missing-unregistered", "removing"] as const)(
    "shares one actionable guard while retaining conversation history for %s",
    async (availability) => {
      seedEnvironment(makeEnvironmentPresentation());
      seedProject(makeProject());
      seedServerThread(
        makeThread({
          worktreePath: "/external/repo-feature",
          messages: [
            {
              id: MessageId.make("message-history"),
              role: "user",
              text: "Retained history",
              turnId: null,
              createdAt: now,
              updatedAt: now,
              streaming: false,
            },
          ],
        }),
      );
      seedGitStatus(true);
      h.queryDataByKey.set("worktree.catalog", {
        adoptedWorkspaces: [
          {
            threadId,
            path: "/external/repo-feature",
            branch: "feature/adopted",
            availability,
            registrationState: availability === "missing-unregistered" ? null : "registered",
            locked: false,
            lockReason: null,
          },
        ],
      });

      renderServerRoute();
      const header = capturedProps<Record<string, unknown>>("chatHeaderActions");
      const composer = capturedProps<Record<string, unknown>>("chatComposer");
      const reason = header["workspaceUnavailable"];

      expect(reason).toEqual(expect.stringContaining("Retry detection"));
      expect(composer["providerBindingConflictReason"]).toBe(reason);
      expect(
        capturedProps<{ timelineEntries: Array<{ message?: { text?: string } }> }>(
          "messagesTimeline",
        ).timelineEntries,
      ).toEqual(
        expect.arrayContaining([
          expect.objectContaining({
            message: expect.objectContaining({ text: "Retained history" }),
          }),
        ]),
      );
      expect(
        capturedProps<{ items: ComposerBannerStackItem[] }>("composerBannerStack").items,
      ).toEqual(expect.arrayContaining([expect.objectContaining({ description: reason })]));

      await (composer["onSend"] as () => Promise<void>)();
      expect(h.commandCalls.some((call) => call.key === "thread.startTurn")).toBe(false);
    },
  );
});
