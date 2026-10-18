import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import {
  createThreadJumpHintVisibilityController,
  findDefaultThread,
  getSidebarThreadIdsToPrewarm,
  getVisibleSidebarThreadIds,
  orderRowsWithPins,
  resolveAdjacentThreadId,
  getFallbackThreadIdAfterDelete,
  getVisibleThreadsForProject,
  getProjectSortTimestamp,
  hasUnseenCompletion,
  isContextMenuPointerDown,
  isTrailingDoubleClick,
  orderItemsByPreferredIds,
  resolveSidebarNewThreadSeedContext,
  resolveSidebarNewThreadEnvMode,
  resolveSidebarProjectAvailability,
  resolveSidebarStageBadgeLabel,
  resolveThreadStatusPill,
  resolveWorkspaceBranchLabel,
  shouldClearThreadSelectionOnMouseDown,
  sortProjectsForSidebar,
  splitPrimaryAndWorkspaceThreads,
  THREAD_JUMP_HINT_SHOW_DELAY_MS,
  contextMenuAnchorForRect,
  pickMoreUrgentWorkspaceCardStatus,
  resolveHighestWorkspaceCardStatus,
  resolveWorkspaceCardStatus,
  WORKSPACE_CARD_STATUS,
  formatCompactAge,
  isContextMenuShortcut,
  isKeyboardContextMenuEcho,
  resolveWorkspaceCardAgeSource,
  resolveWorkspaceCardClassName,
  resolveWorkspaceDirty,
  shouldShowWorkspaceBranchText,
  summarizeWorkspaceChats,
  workspaceCheckoutKey,
  isWorktreeSessionRunning,
} from "./Sidebar.logic";
import { KEYBOARD_CONTEXT_MENU_ECHO_MS } from "../contextMenuKeyboard";
import {
  EnvironmentId,
  OrchestrationLatestTurn,
  ProjectId,
  ProviderInstanceId,
  ThreadId,
  TurnId,
  type OrchestrationSessionStatus,
  type VcsStatusResult,
  type VcsStatusSummary,
} from "@bibcode/contracts";
import {
  DEFAULT_INTERACTION_MODE,
  DEFAULT_RUNTIME_MODE,
  type Project,
  type Thread,
} from "../types";

const localEnvironmentId = EnvironmentId.make("environment-local");

function availabilityEnvironment(
  status:
    | "starting"
    | "synchronizing"
    | "live"
    | "degraded"
    | "storage-changed"
    | "recovery-required"
    | "unavailable"
    | "configuration-error",
  options: { readonly hasSnapshot?: boolean } = {},
) {
  return {
    environmentId: localEnvironmentId,
    status,
    hasSnapshot: options.hasSnapshot ?? false,
    error: null,
  };
}

describe("resolveSidebarProjectAvailability", () => {
  it("renders corrupt catalog health as configuration recovery instead of empty", () => {
    expect(
      resolveSidebarProjectAvailability({
        projectCount: 0,
        catalogReady: true,
        catalogHealth: {
          status: "recovery-required",
          message: "The connection catalog needs recovery.",
        },
        environments: [],
      }),
    ).toEqual({
      kind: "configuration-error",
      environmentId: null,
      error: "The connection catalog needs recovery.",
      hasCachedProjects: false,
    });
  });

  it.each([
    ["catalog-loading", false, []],
    ["starting", true, [availabilityEnvironment("starting")]],
    ["synchronizing", true, [availabilityEnvironment("synchronizing")]],
    ["degraded", true, [availabilityEnvironment("degraded", { hasSnapshot: true })]],
    ["storage-changed", true, [availabilityEnvironment("storage-changed")]],
    ["recovery-required", true, [availabilityEnvironment("recovery-required")]],
    ["unavailable", true, [availabilityEnvironment("unavailable")]],
    ["configuration-error", true, [availabilityEnvironment("configuration-error")]],
  ] as const)("does not claim empty projects for %s", (_name, catalogReady, environments) => {
    expect(
      resolveSidebarProjectAvailability({
        projectCount: 0,
        catalogReady,
        environments,
      }).kind,
    ).not.toBe("empty-confirmed");
  });

  it("does not claim empty projects when the loaded catalog has no desired environments", () => {
    expect(
      resolveSidebarProjectAvailability({
        projectCount: 0,
        catalogReady: true,
        environments: [],
      }).kind,
    ).not.toBe("empty-confirmed");
  });

  it("confirms empty only when every desired environment is live and authoritative", () => {
    expect(
      resolveSidebarProjectAvailability({
        projectCount: 0,
        catalogReady: true,
        environments: [availabilityEnvironment("live", { hasSnapshot: true })],
      }),
    ).toEqual({ kind: "empty-confirmed", environmentId: null, error: null });
  });

  it("keeps cached projects visible while reporting a storage change", () => {
    expect(
      resolveSidebarProjectAvailability({
        projectCount: 1,
        catalogReady: true,
        environments: [availabilityEnvironment("storage-changed", { hasSnapshot: true })],
      }),
    ).toMatchObject({
      kind: "storage-changed",
      environmentId: localEnvironmentId,
      hasCachedProjects: true,
    });
  });
});

describe("resolveSidebarStageBadgeLabel", () => {
  it("returns Nightly for nightly primary server versions", () => {
    expect(
      resolveSidebarStageBadgeLabel({
        primaryServerVersion: "0.0.28-nightly.20260616.12",
        fallbackStageLabel: "Latest",
      }),
    ).toBe("Nightly");
  });

  it("omits the badge for stable primary server versions", () => {
    expect(
      resolveSidebarStageBadgeLabel({
        primaryServerVersion: "0.0.27",
        fallbackStageLabel: "Latest",
      }),
    ).toBeNull();
  });

  it("returns the fallback label when the primary server version is missing", () => {
    expect(
      resolveSidebarStageBadgeLabel({
        primaryServerVersion: null,
        fallbackStageLabel: "Dev",
      }),
    ).toBe("Dev");
  });

  it("omits the badge for malformed nightly prerelease versions in stable builds", () => {
    expect(
      resolveSidebarStageBadgeLabel({
        primaryServerVersion: "0.0.28-nightly.20260616",
        fallbackStageLabel: "Latest",
      }),
    ).toBeNull();
  });
});

function makeLatestTurn(overrides?: {
  completedAt?: string | null;
  startedAt?: string | null;
}): OrchestrationLatestTurn {
  return {
    turnId: "turn-1" as never,
    state: "completed",
    assistantMessageId: null,
    requestedAt: "2026-03-09T10:00:00.000Z",
    startedAt: overrides?.startedAt ?? "2026-03-09T10:00:00.000Z",
    completedAt: overrides?.completedAt ?? "2026-03-09T10:05:00.000Z",
  };
}

describe("hasUnseenCompletion", () => {
  it("returns true when a thread completed after its last visit", () => {
    expect(
      hasUnseenCompletion({
        hasActionableProposedPlan: false,
        hasPendingApprovals: false,
        hasPendingUserInput: false,
        interactionMode: "default",
        latestTurn: makeLatestTurn(),
        lastVisitedAt: "2026-03-09T10:04:00.000Z",
        session: null,
      }),
    ).toBe(true);
  });

  it("treats a missing client visit marker as read", () => {
    expect(
      hasUnseenCompletion({
        hasActionableProposedPlan: false,
        hasPendingApprovals: false,
        hasPendingUserInput: false,
        interactionMode: "default",
        latestTurn: makeLatestTurn(),
        lastVisitedAt: undefined,
        session: null,
      }),
    ).toBe(false);
  });
});

describe("createThreadJumpHintVisibilityController", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("delays showing jump hints until the configured delay elapses", () => {
    const visibilityChanges: boolean[] = [];
    const controller = createThreadJumpHintVisibilityController({
      delayMs: THREAD_JUMP_HINT_SHOW_DELAY_MS,
      onVisibilityChange: (visible) => {
        visibilityChanges.push(visible);
      },
    });

    controller.sync(true);
    vi.advanceTimersByTime(THREAD_JUMP_HINT_SHOW_DELAY_MS - 1);

    expect(visibilityChanges).toEqual([]);

    vi.advanceTimersByTime(1);

    expect(visibilityChanges).toEqual([true]);
  });

  it("hides immediately when the modifiers are released", () => {
    const visibilityChanges: boolean[] = [];
    const controller = createThreadJumpHintVisibilityController({
      delayMs: THREAD_JUMP_HINT_SHOW_DELAY_MS,
      onVisibilityChange: (visible) => {
        visibilityChanges.push(visible);
      },
    });

    controller.sync(true);
    vi.advanceTimersByTime(THREAD_JUMP_HINT_SHOW_DELAY_MS);
    controller.sync(false);

    expect(visibilityChanges).toEqual([true, false]);
  });

  it("cancels a pending reveal when the modifier is released early", () => {
    const visibilityChanges: boolean[] = [];
    const controller = createThreadJumpHintVisibilityController({
      delayMs: THREAD_JUMP_HINT_SHOW_DELAY_MS,
      onVisibilityChange: (visible) => {
        visibilityChanges.push(visible);
      },
    });

    controller.sync(true);
    vi.advanceTimersByTime(Math.floor(THREAD_JUMP_HINT_SHOW_DELAY_MS / 2));
    controller.sync(false);
    vi.advanceTimersByTime(THREAD_JUMP_HINT_SHOW_DELAY_MS);

    expect(visibilityChanges).toEqual([]);
  });
});

describe("getSidebarThreadIdsToPrewarm", () => {
  it("returns only the first visible thread ids up to the prewarm limit", () => {
    expect(getSidebarThreadIdsToPrewarm(["t1", "t2", "bibcode"], 2)).toEqual(["t1", "t2"]);
  });

  it("returns all visible thread ids when they fit within the limit", () => {
    expect(getSidebarThreadIdsToPrewarm(["t1", "t2"], 10)).toEqual(["t1", "t2"]);
  });

  it("returns no thread ids when the limit is zero", () => {
    expect(getSidebarThreadIdsToPrewarm(["t1", "t2"], 0)).toEqual([]);
  });
});

describe("shouldClearThreadSelectionOnMouseDown", () => {
  it("preserves selection for thread items", () => {
    const child = {
      closest: (selector: string) =>
        selector.includes("[data-thread-item]") ? ({} as Element) : null,
    } as unknown as HTMLElement;

    expect(shouldClearThreadSelectionOnMouseDown(child)).toBe(false);
  });

  it("preserves selection for thread list toggle controls", () => {
    const selectionSafe = {
      closest: (selector: string) =>
        selector.includes("[data-thread-selection-safe]") ? ({} as Element) : null,
    } as unknown as HTMLElement;

    expect(shouldClearThreadSelectionOnMouseDown(selectionSafe)).toBe(false);
  });

  it("clears selection for unrelated sidebar clicks", () => {
    const unrelated = {
      closest: () => null,
    } as unknown as HTMLElement;

    expect(shouldClearThreadSelectionOnMouseDown(unrelated)).toBe(true);
  });
});

describe("isTrailingDoubleClick", () => {
  it("treats a single click as a normal activation", () => {
    expect(isTrailingDoubleClick(1)).toBe(false);
  });

  it("treats synthetic/keyboard activations (detail 0) as a normal activation", () => {
    expect(isTrailingDoubleClick(0)).toBe(false);
  });

  it("ignores the second click of a double-click so it does not navigate", () => {
    expect(isTrailingDoubleClick(2)).toBe(true);
  });

  it("ignores further clicks of a triple-click", () => {
    expect(isTrailingDoubleClick(3)).toBe(true);
  });
});

describe("resolveSidebarNewThreadEnvMode", () => {
  it("uses the app default when the caller does not request a specific mode", () => {
    expect(
      resolveSidebarNewThreadEnvMode({
        defaultEnvMode: "worktree",
      }),
    ).toBe("worktree");
  });

  it("preserves an explicit requested mode over the app default", () => {
    expect(
      resolveSidebarNewThreadEnvMode({
        requestedEnvMode: "local",
        defaultEnvMode: "worktree",
      }),
    ).toBe("local");
  });
});

describe("resolveSidebarNewThreadSeedContext", () => {
  it("prefers the default worktree mode over active thread context", () => {
    expect(
      resolveSidebarNewThreadSeedContext({
        projectId: "project-1",
        defaultEnvMode: "worktree",
        activeThread: {
          projectId: "project-1",
          branch: "feature/existing",
          worktreePath: "/repo/.bibcode/worktrees/existing",
        },
        activeDraftThread: {
          projectId: "project-1",
          branch: "feature/draft",
          worktreePath: "/repo/.bibcode/worktrees/draft",
          envMode: "worktree",
          startFromOrigin: true,
        },
      }),
    ).toEqual({
      envMode: "worktree",
    });
  });

  it("inherits the active server thread context when creating a new thread in the same project", () => {
    expect(
      resolveSidebarNewThreadSeedContext({
        projectId: "project-1",
        defaultEnvMode: "local",
        activeThread: {
          projectId: "project-1",
          branch: "effect-atom",
          worktreePath: null,
        },
        activeDraftThread: null,
      }),
    ).toEqual({
      branch: "effect-atom",
      worktreePath: null,
      envMode: "local",
    });
  });

  it("prefers the active draft thread context when it matches the target project", () => {
    expect(
      resolveSidebarNewThreadSeedContext({
        projectId: "project-1",
        defaultEnvMode: "local",
        activeThread: {
          projectId: "project-1",
          branch: "effect-atom",
          worktreePath: null,
        },
        activeDraftThread: {
          projectId: "project-1",
          branch: "feature/new-draft",
          worktreePath: "/repo/worktree",
          envMode: "worktree",
          startFromOrigin: true,
        },
      }),
    ).toEqual({
      branch: "feature/new-draft",
      worktreePath: "/repo/worktree",
      envMode: "worktree",
      startFromOrigin: true,
    });
  });

  it("falls back to the default env mode when there is no matching active thread context", () => {
    expect(
      resolveSidebarNewThreadSeedContext({
        projectId: "project-2",
        defaultEnvMode: "worktree",
        activeThread: {
          projectId: "project-1",
          branch: "effect-atom",
          worktreePath: null,
        },
        activeDraftThread: null,
      }),
    ).toEqual({
      envMode: "worktree",
    });
  });
});

describe("orderItemsByPreferredIds", () => {
  it("keeps preferred ids first, skips stale ids, and preserves the relative order of remaining items", () => {
    const ordered = orderItemsByPreferredIds({
      items: [
        { id: ProjectId.make("project-1"), name: "One" },
        { id: ProjectId.make("project-2"), name: "Two" },
        { id: ProjectId.make("project-3"), name: "Three" },
      ],
      preferredIds: [
        ProjectId.make("project-3"),
        ProjectId.make("project-missing"),
        ProjectId.make("project-1"),
      ],
      getId: (project) => project.id,
    });

    expect(ordered.map((project) => project.id)).toEqual([
      ProjectId.make("project-3"),
      ProjectId.make("project-1"),
      ProjectId.make("project-2"),
    ]);
  });

  it("does not duplicate items when preferred ids repeat", () => {
    const ordered = orderItemsByPreferredIds({
      items: [
        { id: ProjectId.make("project-1"), name: "One" },
        { id: ProjectId.make("project-2"), name: "Two" },
      ],
      preferredIds: [
        ProjectId.make("project-2"),
        ProjectId.make("project-1"),
        ProjectId.make("project-2"),
      ],
      getId: (project) => project.id,
    });

    expect(ordered.map((project) => project.id)).toEqual([
      ProjectId.make("project-2"),
      ProjectId.make("project-1"),
    ]);
  });

  it("honors projectOrder physical keys via getProjectOrderKey", async () => {
    // Regression guard for #1904 / the regression introduced by #2055:
    // `projectOrder` is populated with physical keys (envId + cwd-derived)
    // by the store and by drag-end handlers. Readers must identify projects
    // with the same key format, or manual sort silently snaps back.
    const { getProjectOrderKey } = await import("../logicalProject");
    const projects = [
      {
        environmentId: EnvironmentId.make("environment-local"),
        id: ProjectId.make("id-alpha"),
        workspaceRoot: "/work/alpha",
      },
      {
        environmentId: EnvironmentId.make("environment-local"),
        id: ProjectId.make("id-beta"),
        workspaceRoot: "/work/beta",
      },
      {
        environmentId: EnvironmentId.make("environment-local"),
        id: ProjectId.make("id-gamma"),
        workspaceRoot: "/work/gamma",
      },
    ];
    const ordered = orderItemsByPreferredIds({
      items: projects,
      preferredIds: [getProjectOrderKey(projects[2]!), getProjectOrderKey(projects[0]!)],
      getId: getProjectOrderKey,
    });

    expect(ordered.map((project) => project.workspaceRoot)).toEqual([
      "/work/gamma",
      "/work/alpha",
      "/work/beta",
    ]);
  });

  it("resolves legacy preference aliases without materializing project state", () => {
    const ordered = orderItemsByPreferredIds({
      items: [
        { id: "physical-a", cwd: "/work/a" },
        { id: "physical-b", cwd: "/work/b" },
        { id: "physical-c", cwd: "/work/c" },
      ],
      preferredIds: ["legacy:/work/c", "legacy:/work/a"],
      getId: (project) => project.id,
      getPreferenceIds: (project) => [project.id, `legacy:${project.cwd}`],
    });

    expect(ordered.map((project) => project.id)).toEqual([
      "physical-c",
      "physical-a",
      "physical-b",
    ]);
  });
});

describe("resolveAdjacentThreadId", () => {
  it("resolves adjacent thread ids in ordered sidebar traversal", () => {
    const threads = [
      ThreadId.make("thread-1"),
      ThreadId.make("thread-2"),
      ThreadId.make("thread-3"),
    ];

    expect(
      resolveAdjacentThreadId({
        threadIds: threads,
        currentThreadId: threads[1] ?? null,
        direction: "previous",
      }),
    ).toBe(threads[0]);
    expect(
      resolveAdjacentThreadId({
        threadIds: threads,
        currentThreadId: threads[1] ?? null,
        direction: "next",
      }),
    ).toBe(threads[2]);
    expect(
      resolveAdjacentThreadId({
        threadIds: threads,
        currentThreadId: null,
        direction: "next",
      }),
    ).toBe(threads[0]);
    expect(
      resolveAdjacentThreadId({
        threadIds: threads,
        currentThreadId: null,
        direction: "previous",
      }),
    ).toBe(threads[2]);
    expect(
      resolveAdjacentThreadId({
        threadIds: threads,
        currentThreadId: threads[0] ?? null,
        direction: "previous",
      }),
    ).toBeNull();
  });
});

describe("getVisibleSidebarThreadIds", () => {
  it("returns only the rendered visible thread order across projects", () => {
    expect(
      getVisibleSidebarThreadIds([
        {
          renderedThreadIds: [
            ThreadId.make("thread-12"),
            ThreadId.make("thread-11"),
            ThreadId.make("thread-10"),
          ],
        },
        {
          renderedThreadIds: [ThreadId.make("thread-8"), ThreadId.make("thread-6")],
        },
      ]),
    ).toEqual([
      ThreadId.make("thread-12"),
      ThreadId.make("thread-11"),
      ThreadId.make("thread-10"),
      ThreadId.make("thread-8"),
      ThreadId.make("thread-6"),
    ]);
  });

  it("skips threads from collapsed projects whose thread panels are not shown", () => {
    expect(
      getVisibleSidebarThreadIds([
        {
          shouldShowThreadPanel: false,
          renderedThreadIds: [ThreadId.make("thread-hidden-2"), ThreadId.make("thread-hidden-1")],
        },
        {
          shouldShowThreadPanel: true,
          renderedThreadIds: [ThreadId.make("thread-12"), ThreadId.make("thread-11")],
        },
      ]),
    ).toEqual([ThreadId.make("thread-12"), ThreadId.make("thread-11")]);
  });
});

describe("isContextMenuPointerDown", () => {
  it("treats secondary-button presses as context menu gestures on all platforms", () => {
    expect(
      isContextMenuPointerDown({
        button: 2,
        ctrlKey: false,
        isMac: false,
      }),
    ).toBe(true);
  });

  it("treats ctrl+primary-click as a context menu gesture on macOS", () => {
    expect(
      isContextMenuPointerDown({
        button: 0,
        ctrlKey: true,
        isMac: true,
      }),
    ).toBe(true);
  });

  it("does not treat ctrl+primary-click as a context menu gesture off macOS", () => {
    expect(
      isContextMenuPointerDown({
        button: 0,
        ctrlKey: true,
        isMac: false,
      }),
    ).toBe(false);
  });
});

describe("resolveThreadStatusPill", () => {
  const baseThread = {
    hasActionableProposedPlan: false,
    hasPendingApprovals: false,
    hasPendingUserInput: false,
    interactionMode: "plan" as const,
    latestTurn: null,
    lastVisitedAt: undefined,
    session: {
      threadId: ThreadId.make("thread-1"),
      status: "running" as const,
      providerName: "Codex",
      providerInstanceId: ProviderInstanceId.make("codex"),
      runtimeMode: DEFAULT_RUNTIME_MODE,
      activeTurnId: "turn-1" as never,
      lastError: null,
      updatedAt: "2026-03-09T10:00:00.000Z",
    },
  };

  it("shows no Working or unread completion for a ready thread containing only queued messages", () => {
    const thread = {
      ...baseThread,
      session: { ...baseThread.session, status: "ready" as const, activeTurnId: null },
      messages: [
        {
          role: "user",
          text: "later",
          delivery: { state: "queued", mode: "start", provider: "codex" },
        },
      ],
    };
    expect(resolveThreadStatusPill({ thread })).toBeNull();
    expect(hasUnseenCompletion(thread)).toBe(false);
  });

  it("shows pending approval before all other statuses", () => {
    expect(
      resolveThreadStatusPill({
        thread: {
          ...baseThread,
          hasPendingApprovals: true,
          hasPendingUserInput: true,
        },
      }),
    ).toMatchObject({ label: "Pending Approval", pulse: false });
  });

  it("shows awaiting input when plan mode is blocked on user answers", () => {
    expect(
      resolveThreadStatusPill({
        thread: {
          ...baseThread,
          hasPendingUserInput: true,
        },
      }),
    ).toMatchObject({ label: "Awaiting Input", pulse: false });
  });

  it("falls back to working when the thread is actively running without blockers", () => {
    expect(
      resolveThreadStatusPill({
        thread: baseThread,
      }),
    ).toMatchObject({ label: "Working", pulse: true });
  });

  it("shows plan ready when a settled plan turn has a proposed plan ready for follow-up", () => {
    expect(
      resolveThreadStatusPill({
        thread: {
          ...baseThread,
          hasActionableProposedPlan: true,
          latestTurn: makeLatestTurn(),
          session: {
            ...baseThread.session,
            status: "ready",
            activeTurnId: null,
          },
        },
      }),
    ).toMatchObject({ label: "Plan Ready", pulse: false });
  });

  it("does not manufacture completed state without a client visit marker", () => {
    expect(
      resolveThreadStatusPill({
        thread: {
          ...baseThread,
          latestTurn: makeLatestTurn(),
          session: {
            ...baseThread.session,
            status: "ready",
            activeTurnId: null,
          },
        },
      }),
    ).toBeNull();
  });

  it("shows completed when there is an unseen completion and no active blocker", () => {
    expect(
      resolveThreadStatusPill({
        thread: {
          ...baseThread,
          interactionMode: "default",
          latestTurn: makeLatestTurn(),
          lastVisitedAt: "2026-03-09T10:04:00.000Z",
          session: {
            ...baseThread.session,
            status: "ready",
            activeTurnId: null,
          },
        },
      }),
    ).toMatchObject({ label: "Completed", pulse: false });
  });
});

describe("getVisibleThreadsForProject", () => {
  it("includes the active thread even when it falls below the folded preview", () => {
    const threads = Array.from({ length: 8 }, (_, index) =>
      makeThread({
        id: ThreadId.make(`thread-${index + 1}`),
        title: `Thread ${index + 1}`,
      }),
    );

    const result = getVisibleThreadsForProject({
      threads,
      activeThreadId: ThreadId.make("thread-8"),
      isThreadListExpanded: false,
      previewLimit: 6,
    });

    expect(result.hasHiddenThreads).toBe(true);
    expect(result.visibleThreads.map((thread) => thread.id)).toEqual([
      ThreadId.make("thread-1"),
      ThreadId.make("thread-2"),
      ThreadId.make("thread-3"),
      ThreadId.make("thread-4"),
      ThreadId.make("thread-5"),
      ThreadId.make("thread-6"),
      ThreadId.make("thread-8"),
    ]);
    expect(result.hiddenThreads.map((thread) => thread.id)).toEqual([ThreadId.make("thread-7")]);
  });

  it("returns all threads when the list is expanded", () => {
    const threads = Array.from({ length: 8 }, (_, index) =>
      makeThread({
        id: ThreadId.make(`thread-${index + 1}`),
      }),
    );

    const result = getVisibleThreadsForProject({
      threads,
      activeThreadId: ThreadId.make("thread-8"),
      isThreadListExpanded: true,
      previewLimit: 6,
    });

    expect(result.hasHiddenThreads).toBe(true);
    expect(result.visibleThreads.map((thread) => thread.id)).toEqual(
      threads.map((thread) => thread.id),
    );
    expect(result.hiddenThreads).toEqual([]);
  });
});

function makeProject(overrides: Partial<Project> = {}): Project {
  const { defaultModelSelection, ...rest } = overrides;
  return {
    id: ProjectId.make("project-1"),
    environmentId: localEnvironmentId,
    title: "Project",
    workspaceRoot: "/tmp/project",
    repositoryIdentity: null,
    defaultModelSelection: {
      instanceId: ProviderInstanceId.make("codex"),
      model: "gpt-5.4",
      ...defaultModelSelection,
    },
    createdAt: "2026-03-09T10:00:00.000Z",
    updatedAt: "2026-03-09T10:00:00.000Z",
    scripts: [],
    worktreeDiscovery: { visibility: "hidden", initialPromptDismissedAt: null, baselinePaths: [] },
    ...rest,
  };
}

function makeThread(overrides: Partial<Thread> = {}): Thread {
  return {
    id: ThreadId.make("thread-1"),
    environmentId: localEnvironmentId,
    projectId: ProjectId.make("project-1"),
    title: "Thread",
    modelSelection: {
      instanceId: ProviderInstanceId.make("codex"),
      model: "gpt-5.4",
      ...overrides?.modelSelection,
    },
    runtimeMode: DEFAULT_RUNTIME_MODE,
    interactionMode: DEFAULT_INTERACTION_MODE,
    session: null,
    messages: [],
    proposedPlans: [],
    createdAt: "2026-03-09T10:00:00.000Z",
    archivedAt: null,
    deletedAt: null,
    updatedAt: "2026-03-09T10:00:00.000Z",
    latestTurn: null,
    branch: null,
    worktreePath: null,
    checkpoints: [],
    activities: [],
    ...overrides,
  };
}

describe("getFallbackThreadIdAfterDelete", () => {
  it("returns the top remaining thread in the deleted thread's project sidebar order", () => {
    const fallbackThreadId = getFallbackThreadIdAfterDelete({
      threads: [
        makeThread({
          id: ThreadId.make("thread-oldest"),
          projectId: ProjectId.make("project-1"),
          createdAt: "2026-03-09T10:00:00.000Z",
          messages: [],
        }),
        makeThread({
          id: ThreadId.make("thread-active"),
          projectId: ProjectId.make("project-1"),
          createdAt: "2026-03-09T10:05:00.000Z",
          messages: [],
        }),
        makeThread({
          id: ThreadId.make("thread-newest"),
          projectId: ProjectId.make("project-1"),
          createdAt: "2026-03-09T10:10:00.000Z",
          messages: [],
        }),
        makeThread({
          id: ThreadId.make("thread-other-project"),
          projectId: ProjectId.make("project-2"),
          createdAt: "2026-03-09T10:20:00.000Z",
          messages: [],
        }),
      ],
      deletedThreadId: ThreadId.make("thread-active"),
      sortOrder: "created_at",
    });

    expect(fallbackThreadId).toBe(ThreadId.make("thread-newest"));
  });

  it("skips other threads being deleted in the same action", () => {
    const fallbackThreadId = getFallbackThreadIdAfterDelete({
      threads: [
        makeThread({
          id: ThreadId.make("thread-active"),
          projectId: ProjectId.make("project-1"),
          createdAt: "2026-03-09T10:05:00.000Z",
          messages: [],
        }),
        makeThread({
          id: ThreadId.make("thread-newest"),
          projectId: ProjectId.make("project-1"),
          createdAt: "2026-03-09T10:10:00.000Z",
          messages: [],
        }),
        makeThread({
          id: ThreadId.make("thread-next"),
          projectId: ProjectId.make("project-1"),
          createdAt: "2026-03-09T10:07:00.000Z",
          messages: [],
        }),
      ],
      deletedThreadId: ThreadId.make("thread-active"),
      deletedThreadIds: new Set([ThreadId.make("thread-active"), ThreadId.make("thread-newest")]),
      sortOrder: "created_at",
    });

    expect(fallbackThreadId).toBe(ThreadId.make("thread-next"));
  });
});
describe("sortProjectsForSidebar", () => {
  it("sorts projects by the most recent user message across their threads", () => {
    const projects = [
      makeProject({ id: ProjectId.make("project-1"), title: "Older project" }),
      makeProject({ id: ProjectId.make("project-2"), title: "Newer project" }),
    ];
    const threads = [
      makeThread({
        projectId: ProjectId.make("project-1"),
        updatedAt: "2026-03-09T10:20:00.000Z",
        messages: [
          {
            id: "message-1" as never,
            role: "user",
            text: "older project user message",
            turnId: null,
            createdAt: "2026-03-09T10:01:00.000Z",
            updatedAt: "2026-03-09T10:01:00.000Z",
            streaming: false,
          },
        ],
      }),
      makeThread({
        id: ThreadId.make("thread-2"),
        projectId: ProjectId.make("project-2"),
        updatedAt: "2026-03-09T10:05:00.000Z",
        messages: [
          {
            id: "message-2" as never,
            role: "user",
            text: "newer project user message",
            turnId: null,
            createdAt: "2026-03-09T10:05:00.000Z",
            updatedAt: "2026-03-09T10:05:00.000Z",
            streaming: false,
          },
        ],
      }),
    ];

    const sorted = sortProjectsForSidebar(projects, threads, "updated_at");

    expect(sorted.map((project) => project.id)).toEqual([
      ProjectId.make("project-2"),
      ProjectId.make("project-1"),
    ]);
  });

  it("falls back to project timestamps when a project has no threads", () => {
    const sorted = sortProjectsForSidebar(
      [
        makeProject({
          id: ProjectId.make("project-1"),
          title: "Older project",
          updatedAt: "2026-03-09T10:01:00.000Z",
        }),
        makeProject({
          id: ProjectId.make("project-2"),
          title: "Newer project",
          updatedAt: "2026-03-09T10:05:00.000Z",
        }),
      ],
      [],
      "updated_at",
    );

    expect(sorted.map((project) => project.id)).toEqual([
      ProjectId.make("project-2"),
      ProjectId.make("project-1"),
    ]);
  });

  it("falls back to name and id ordering when projects have no sortable timestamps", () => {
    const sorted = sortProjectsForSidebar(
      [
        makeProject({
          id: ProjectId.make("project-2"),
          title: "Beta",
          createdAt: "invalid-created-at" as never,
          updatedAt: "invalid-updated-at" as never,
        }),
        makeProject({
          id: ProjectId.make("project-1"),
          title: "Alpha",
          createdAt: "invalid-created-at" as never,
          updatedAt: "invalid-updated-at" as never,
        }),
      ],
      [],
      "updated_at",
    );

    expect(sorted.map((project) => project.id)).toEqual([
      ProjectId.make("project-1"),
      ProjectId.make("project-2"),
    ]);
  });

  it("preserves manual project ordering", () => {
    const projects = [
      makeProject({ id: ProjectId.make("project-2"), title: "Second" }),
      makeProject({ id: ProjectId.make("project-1"), title: "First" }),
    ];

    const sorted = sortProjectsForSidebar(projects, [], "manual");

    expect(sorted.map((project) => project.id)).toEqual([
      ProjectId.make("project-2"),
      ProjectId.make("project-1"),
    ]);
  });

  it("ignores archived threads when sorting projects", () => {
    const sorted = sortProjectsForSidebar(
      [
        makeProject({
          id: ProjectId.make("project-1"),
          title: "Visible project",
          updatedAt: "2026-03-09T10:01:00.000Z",
        }),
        makeProject({
          id: ProjectId.make("project-2"),
          title: "Archived-only project",
          updatedAt: "2026-03-09T10:00:00.000Z",
        }),
      ],
      [
        makeThread({
          id: ThreadId.make("thread-visible"),
          projectId: ProjectId.make("project-1"),
          updatedAt: "2026-03-09T10:02:00.000Z",
          archivedAt: null,
        }),
        makeThread({
          id: ThreadId.make("thread-archived"),
          projectId: ProjectId.make("project-2"),
          updatedAt: "2026-03-09T10:10:00.000Z",
          archivedAt: "2026-03-09T10:11:00.000Z",
        }),
      ].filter((thread) => thread.archivedAt === null),
      "updated_at",
    );

    expect(sorted.map((project) => project.id)).toEqual([
      ProjectId.make("project-1"),
      ProjectId.make("project-2"),
    ]);
  });

  it("returns the project timestamp when no threads are present", () => {
    const timestamp = getProjectSortTimestamp(
      makeProject({ updatedAt: "2026-03-09T10:10:00.000Z" }),
      [],
      "updated_at",
    );

    expect(timestamp).toBe(Date.parse("2026-03-09T10:10:00.000Z"));
  });
});

describe("sidebar logic defensive branch coverage", () => {
  it("handles jump-controller default timers and repeated lifecycle calls", () => {
    vi.useFakeTimers();
    const onVisibilityChange = vi.fn();
    const controller = createThreadJumpHintVisibilityController({
      delayMs: 1,
      onVisibilityChange,
    });

    controller.dispose();
    controller.sync(true);
    controller.sync(true);
    vi.advanceTimersByTime(1);
    controller.sync(true);
    controller.sync(false);
    controller.sync(false);
    controller.dispose();

    expect(onVisibilityChange.mock.calls).toEqual([[true], [false]]);
  });

  it("rejects missing or invalid completion timestamps and detects invalid visit markers", () => {
    const base = {
      hasActionableProposedPlan: false,
      hasPendingApprovals: false,
      hasPendingUserInput: false,
      interactionMode: "default" as const,
      session: null,
    };
    expect(hasUnseenCompletion({ ...base, latestTurn: null, lastVisitedAt: "invalid" })).toBe(
      false,
    );
    expect(
      hasUnseenCompletion({
        ...base,
        latestTurn: makeLatestTurn({ completedAt: "invalid" }),
        lastVisitedAt: "2026-03-09T10:00:00.000Z",
      }),
    ).toBe(false);
    expect(
      hasUnseenCompletion({
        ...base,
        latestTurn: makeLatestTurn(),
        lastVisitedAt: "invalid",
      }),
    ).toBe(true);
    expect(shouldClearThreadSelectionOnMouseDown(null)).toBe(true);
  });

  it("covers seed-context fallbacks and empty preference ordering", () => {
    expect(
      resolveSidebarNewThreadSeedContext({
        projectId: "project-1",
        defaultEnvMode: "local",
        activeThread: {
          projectId: "project-1",
          branch: null,
          worktreePath: null,
        },
      }),
    ).toEqual({ branch: null, worktreePath: null, envMode: "local" });
    expect(
      resolveSidebarNewThreadSeedContext({
        projectId: "project-1",
        defaultEnvMode: "local",
        activeDraftThread: {
          projectId: "other-project",
          branch: "other",
          worktreePath: null,
          envMode: "local",
          startFromOrigin: false,
        },
      }),
    ).toEqual({ envMode: "local" });
    const items = [{ id: "a" }, { id: "b" }];
    expect(orderItemsByPreferredIds({ items, preferredIds: [], getId: (item) => item.id })).toEqual(
      items,
    );
    expect(
      orderItemsByPreferredIds({
        items,
        preferredIds: ["missing", "a"],
        getId: (item) => item.id,
        getPreferenceIds: (item) => [item.id, item.id],
      }),
    ).toEqual(items);
  });

  it("returns null at every adjacent traversal boundary", () => {
    expect(
      resolveAdjacentThreadId({ threadIds: [], currentThreadId: null, direction: "next" }),
    ).toBeNull();
    expect(
      resolveAdjacentThreadId({
        threadIds: ["a", "b"],
        currentThreadId: "missing",
        direction: "next",
      }),
    ).toBeNull();
    expect(
      resolveAdjacentThreadId({
        threadIds: ["a", "b"],
        currentThreadId: "b",
        direction: "next",
      }),
    ).toBeNull();
  });

  it("covers folded project lists without a usable active thread", () => {
    const threads = [
      { id: ThreadId.make("thread-1") },
      { id: ThreadId.make("thread-2") },
      { id: ThreadId.make("thread-3") },
    ];
    expect(
      getVisibleThreadsForProject({
        threads,
        activeThreadId: undefined,
        isThreadListExpanded: false,
        previewLimit: 2,
      }).visibleThreads,
    ).toEqual(threads.slice(0, 2));
    expect(
      getVisibleThreadsForProject({
        threads,
        activeThreadId: ThreadId.make("thread-1"),
        isThreadListExpanded: false,
        previewLimit: 2,
      }).visibleThreads,
    ).toEqual(threads.slice(0, 2));
    expect(
      getVisibleThreadsForProject({
        threads,
        activeThreadId: ThreadId.make("missing"),
        isThreadListExpanded: false,
        previewLimit: 2,
      }).visibleThreads,
    ).toEqual(threads.slice(0, 2));
    expect(
      getVisibleThreadsForProject({
        threads: threads.slice(0, 2),
        activeThreadId: undefined,
        isThreadListExpanded: false,
        previewLimit: 2,
      }).hasHiddenThreads,
    ).toBe(false);
  });

  it("handles missing deleted threads and projects with invalid timestamps", () => {
    expect(
      getFallbackThreadIdAfterDelete({
        threads: [makeThread()],
        deletedThreadId: ThreadId.make("missing"),
        sortOrder: "created_at",
      }),
    ).toBeNull();
    expect(
      getFallbackThreadIdAfterDelete({
        threads: [makeThread()],
        deletedThreadId: ThreadId.make("thread-1"),
        sortOrder: "created_at",
      }),
    ).toBeNull();
    expect(
      getProjectSortTimestamp(
        { id: "project", title: "Project", createdAt: undefined, updatedAt: undefined },
        [],
        "created_at",
      ),
    ).toBe(Number.NEGATIVE_INFINITY);
    expect(
      getProjectSortTimestamp(
        { id: "project", title: "Project", createdAt: "invalid", updatedAt: undefined },
        [],
        "updated_at",
      ),
    ).toBe(Number.NEGATIVE_INFINITY);
  });

  it("clamps negative prewarm limits", () => {
    expect(getSidebarThreadIdsToPrewarm(["a", "b"], -1)).toEqual([]);
  });
});

describe("findDefaultThread / splitPrimaryAndWorkspaceThreads", () => {
  it("finds the thread whose kind is default", () => {
    const threads = [
      { id: "a", kind: "workspace" as const },
      { id: "b", kind: "default" as const },
    ];
    expect(findDefaultThread(threads)?.id).toBe("b");
  });

  it("returns null when no thread is kind default (server hasn't backfilled yet)", () => {
    const threads = [{ id: "a", kind: "workspace" as const }, { id: "b" }];
    expect(findDefaultThread(threads)).toBeNull();
  });

  it("splits primary thread out of the workspace-row list", () => {
    const threads = [
      { id: "a", kind: "workspace" as const },
      { id: "b", kind: "default" as const },
      { id: "c", kind: "workspace" as const },
    ];
    const { primaryThread, workspaceThreads } = splitPrimaryAndWorkspaceThreads(threads);
    expect(primaryThread?.id).toBe("b");
    expect(workspaceThreads.map((t) => t.id)).toEqual(["a", "c"]);
  });

  it("workspaceThreads is the full list (primary null) when no default thread exists", () => {
    const threads = [
      { id: "a", kind: "workspace" as const },
      { id: "b", kind: "workspace" as const },
    ];
    const { primaryThread, workspaceThreads } = splitPrimaryAndWorkspaceThreads(threads);
    expect(primaryThread).toBeNull();
    expect(workspaceThreads.map((t) => t.id)).toEqual(["a", "b"]);
  });

  it("excludes panel threads from workspace rows", () => {
    const threads = [
      { id: "a", kind: "workspace" as const },
      { id: "p1", kind: "panel" as const },
      { id: "b", kind: "default" as const },
      { id: "p2", kind: "panel" as const },
      { id: "c", kind: "workspace" as const },
    ];
    const { primaryThread, workspaceThreads } = splitPrimaryAndWorkspaceThreads(threads);
    expect(primaryThread?.id).toBe("b");
    expect(workspaceThreads.map((t) => t.id)).toEqual(["a", "c"]);
  });

  it("panel-only projects still resolve the primary thread and show no workspace rows", () => {
    const threads = [
      { id: "b", kind: "default" as const },
      { id: "p1", kind: "panel" as const },
    ];
    const { primaryThread, workspaceThreads } = splitPrimaryAndWorkspaceThreads(threads);
    expect(primaryThread?.id).toBe("b");
    expect(workspaceThreads).toEqual([]);
  });

  it("panel threads are never picked as the primary thread", () => {
    const threads = [{ id: "p1", kind: "panel" as const }];
    expect(findDefaultThread(threads)).toBeNull();
  });
});

describe("orderRowsWithPins", () => {
  it("moves pinned items to the front, preserving relative order within each partition", () => {
    const items = [{ key: "a" }, { key: "b" }, { key: "c" }, { key: "d" }];
    const ordered = orderRowsWithPins(items, ["c", "a"], (item) => item.key);
    expect(ordered.map((item) => item.key)).toEqual(["a", "c", "b", "d"]);
  });

  it("returns items unchanged (by order) when nothing is pinned", () => {
    const items = [{ key: "a" }, { key: "b" }];
    expect(orderRowsWithPins(items, [], (item) => item.key).map((i) => i.key)).toEqual(["a", "b"]);
  });

  it("accepts a Set of pinned keys", () => {
    const items = [{ key: "a" }, { key: "b" }];
    const ordered = orderRowsWithPins(items, new Set(["b"]), (item) => item.key);
    expect(ordered.map((item) => item.key)).toEqual(["b", "a"]);
  });
});

describe("resolveWorkspaceBranchLabel", () => {
  const summary = (overrides: Record<string, unknown>) =>
    ({
      isRepo: true,
      refName: "main",
      detachedHead: null,
      hasWorkingTreeChanges: false,
      sourceControlProvider: null,
      pr: null,
      observedAt: "2026-09-24T10:00:00.000Z",
      stale: false,
      ...overrides,
    }) as unknown as VcsStatusSummary;

  it("prefers the fresh ref name", () => {
    expect(resolveWorkspaceBranchLabel(summary({}), "stored")).toBe("main");
  });

  it("falls back to the detached head", () => {
    expect(
      resolveWorkspaceBranchLabel(summary({ refName: null, detachedHead: "abc1234" }), "stored"),
    ).toBe("abc1234");
  });

  // A failed PR lookup republishes a just-read summary as stale; its ref is still
  // the newest observation, while the recorded branch only follows an open thread.
  it("keeps a stale summary's ref over the recorded branch", () => {
    expect(resolveWorkspaceBranchLabel(summary({ refName: "feature", stale: true }), "main")).toBe(
      "feature",
    );
  });

  it("uses the recorded branch without a status", () => {
    expect(resolveWorkspaceBranchLabel(null, "stored")).toBe("stored");
    expect(resolveWorkspaceBranchLabel(undefined, null)).toBeNull();
  });

  it("reads the ref from a full status result", () => {
    expect(
      resolveWorkspaceBranchLabel({ refName: "feat/x" } as unknown as VcsStatusResult, null),
    ).toBe("feat/x");
  });
});

describe("contextMenuAnchorForRect", () => {
  it("anchors a keyboard-opened menu at the element's rounded bottom-left", () => {
    expect(contextMenuAnchorForRect({ left: 40.4, bottom: 152.6 })).toEqual({ x: 40, y: 153 });
  });
});

describe("resolveWorkspaceCardStatus", () => {
  const base = {
    hasActionableProposedPlan: false,
    hasPendingApprovals: false,
    hasPendingUserInput: false,
    interactionMode: "default" as const,
    latestTurn: null,
    session: null,
    unresolvedDelivery: null,
  };
  const session = (status: "running" | "starting" | "ready" | "error") => ({
    threadId: ThreadId.make("thread-1"),
    status,
    providerName: "claudeAgent",
    runtimeMode: DEFAULT_RUNTIME_MODE,
    activeTurnId: status === "running" ? ("turn-1" as never) : null,
    lastError: status === "error" ? "provider exited" : null,
    updatedAt: "2026-03-09T10:00:00.000Z",
  });
  const settled = (state: "completed" | "error" | "interrupted") => ({
    ...makeLatestTurn(),
    state,
  });
  // makeLatestTurn completes at 10:05.
  const visitedBefore = "2026-03-09T10:04:00.000Z";
  const visitedAfter = "2026-03-09T10:06:00.000Z";
  const W = WORKSPACE_CARD_STATUS;

  it("maps each row of the status table, first match winning", () => {
    expect(
      resolveWorkspaceCardStatus(
        {
          ...base,
          hasPendingApprovals: true,
          hasPendingUserInput: true,
          session: session("error"),
        },
        null,
      ),
    ).toBe(W.approval);
    expect(
      resolveWorkspaceCardStatus(
        { ...base, hasPendingUserInput: true, session: session("running") },
        null,
      ),
    ).toBe(W.input);
    expect(resolveWorkspaceCardStatus({ ...base, session: session("running") }, null)).toBe(
      W.working,
    );
    expect(resolveWorkspaceCardStatus({ ...base, session: session("starting") }, null)).toBe(
      W.connecting,
    );
    expect(resolveWorkspaceCardStatus({ ...base, session: session("error") }, null)).toBe(W.failed);
    expect(
      resolveWorkspaceCardStatus(
        {
          ...base,
          interactionMode: "plan",
          hasActionableProposedPlan: true,
          latestTurn: settled("completed"),
          session: session("ready"),
        },
        null,
      ),
    ).toBe(W.plan);
    expect(
      resolveWorkspaceCardStatus(
        { ...base, latestTurn: settled("completed"), session: session("ready") },
        visitedBefore,
      ),
    ).toBe(W.done);
    expect(resolveWorkspaceCardStatus(base, null)).toBe(W.idle);
  });

  it("names and colours each glyph as the spec's table does", () => {
    expect(W.approval).toEqual({
      kind: "approval",
      label: "Needs approval",
      colorClass: "text-amber-600 dark:text-amber-300/90",
    });
    expect(W.input).toEqual({
      kind: "input",
      label: "Waiting for your answer",
      colorClass: "text-indigo-600 dark:text-indigo-300/90",
    });
    expect(W.working.label).toBe("Working");
    expect(W.connecting.label).toBe("Connecting");
    expect(W.failed).toEqual({ kind: "failed", label: "Failed", colorClass: "text-destructive" });
    expect(W.plan.label).toBe("Plan ready");
    expect(W.done.label).toBe("Finished, not opened yet");
    expect(W.idle).toEqual({ kind: "idle", label: "Idle", colorClass: "text-muted-foreground" });
  });

  it("shows an unseen errored turn as Failed, where the pill said Completed", () => {
    const errored = { ...base, latestTurn: settled("error"), session: session("ready") };
    expect(
      resolveThreadStatusPill({ thread: { ...errored, lastVisitedAt: visitedBefore } })?.label,
    ).toBe("Completed");
    expect(resolveWorkspaceCardStatus(errored, visitedBefore)).toBe(W.failed);
  });

  it("keeps never-visited threads idle, even after an error", () => {
    expect(
      resolveWorkspaceCardStatus(
        { ...base, latestTurn: settled("error"), session: session("ready") },
        null,
      ),
    ).toBe(W.idle);
  });

  it("keeps an interrupted turn Finished until opened, then Idle", () => {
    const interrupted = { ...base, latestTurn: settled("interrupted"), session: session("ready") };
    expect(resolveWorkspaceCardStatus(interrupted, visitedBefore)).toBe(W.done);
    expect(resolveWorkspaceCardStatus(interrupted, visitedAfter)).toBe(W.idle);
  });

  it("fails on a refused delivery and leaves the glyph alone for an uncertain one", () => {
    expect(
      resolveWorkspaceCardStatus({ ...base, unresolvedDelivery: { state: "failed" } }, null),
    ).toBe(W.failed);
    expect(
      resolveWorkspaceCardStatus({ ...base, unresolvedDelivery: { state: "uncertain" } }, null),
    ).toBe(W.idle);
    expect(
      resolveWorkspaceCardStatus(
        { ...base, unresolvedDelivery: { state: "uncertain" }, session: session("running") },
        null,
      ),
    ).toBe(W.working);
  });

  it("ranks Failed above Plan ready", () => {
    expect(
      resolveWorkspaceCardStatus(
        {
          ...base,
          interactionMode: "plan",
          hasActionableProposedPlan: true,
          latestTurn: settled("completed"),
          session: session("ready"),
          unresolvedDelivery: { state: "failed" },
        },
        null,
      ),
    ).toBe(W.failed);
  });
});

describe("workspace card status priority", () => {
  const W = WORKSPACE_CARD_STATUS;

  it("summarises by the most urgent non-idle status", () => {
    expect(resolveHighestWorkspaceCardStatus([W.idle, W.done, W.failed, W.plan])).toBe(W.failed);
    expect(resolveHighestWorkspaceCardStatus([W.working, W.input])).toBe(W.input);
    expect(resolveHighestWorkspaceCardStatus([W.input, W.approval])).toBe(W.approval);
    expect(resolveHighestWorkspaceCardStatus([W.connecting, W.plan])).toBe(W.connecting);
    expect(resolveHighestWorkspaceCardStatus([W.idle])).toBeNull();
    expect(resolveHighestWorkspaceCardStatus([])).toBeNull();
  });

  it("picks the more urgent of two statuses, idle included", () => {
    expect(pickMoreUrgentWorkspaceCardStatus(W.idle, W.done)).toBe(W.done);
    expect(pickMoreUrgentWorkspaceCardStatus(W.approval, W.working)).toBe(W.approval);
    expect(pickMoreUrgentWorkspaceCardStatus(W.idle, W.idle)).toBe(W.idle);
  });
});

describe("card line 2 helpers", () => {
  it("hides the branch text when it repeats the title", () => {
    expect(shouldShowWorkspaceBranchText("develop", "develop")).toBe(false);
    expect(shouldShowWorkspaceBranchText(null, "Fix invoice date format")).toBe(false);
    expect(shouldShowWorkspaceBranchText("fix-TRI-150", "Fix invoice date format")).toBe(true);
  });

  // A stale summary's working tree is the newest local read: a failed PR lookup
  // marks a just-read base stale. No dot would claim the checkout is clean.
  it("reads uncommitted changes from fresh and stale statuses", () => {
    expect(
      resolveWorkspaceDirty({ hasWorkingTreeChanges: true } as unknown as VcsStatusResult),
    ).toBe(true);
    expect(
      resolveWorkspaceDirty({
        hasWorkingTreeChanges: true,
        stale: true,
      } as unknown as VcsStatusSummary),
    ).toBe(true);
    expect(
      resolveWorkspaceDirty({ hasWorkingTreeChanges: false } as unknown as VcsStatusResult),
    ).toBe(false);
    expect(resolveWorkspaceDirty(null)).toBe(false);
  });
});

describe("card age", () => {
  const thread = {
    latestTurn: {
      ...makeLatestTurn({ startedAt: "2026-07-03T11:54:00.000Z" }),
      state: "running" as const,
    },
    latestUserMessageAt: "2026-07-03T11:40:00.000Z",
    updatedAt: "2026-07-03T11:30:00.000Z",
    createdAt: "2026-07-01T00:00:00.000Z",
  };

  it("counts from the running turn's start while working, else from the last message", () => {
    expect(resolveWorkspaceCardAgeSource(thread, true)).toBe("2026-07-03T11:54:00.000Z");
    expect(resolveWorkspaceCardAgeSource(thread, false)).toBe("2026-07-03T11:40:00.000Z");
    expect(resolveWorkspaceCardAgeSource({ ...thread, latestTurn: null }, true)).toBe(
      "2026-07-03T11:40:00.000Z",
    );
    expect(resolveWorkspaceCardAgeSource({ ...thread, latestUserMessageAt: null }, false)).toBe(
      "2026-07-03T11:30:00.000Z",
    );
  });

  it("formats compactly", () => {
    const now = Date.parse("2026-07-03T12:00:00.000Z");
    expect(formatCompactAge("2026-07-03T11:59:45.000Z", now)).toBe("now");
    expect(formatCompactAge("2026-07-03T11:54:00.000Z", now)).toBe("6m");
    expect(formatCompactAge("2026-07-03T09:00:00.000Z", now)).toBe("3h");
    expect(formatCompactAge("2026-07-01T12:00:00.000Z", now)).toBe("2d");
    expect(formatCompactAge("2026-07-03T12:05:00.000Z", now)).toBe("now");
    expect(formatCompactAge("invalid", now)).toBeNull();
    expect(formatCompactAge(null, now)).toBeNull();
  });
});

describe("summarizeWorkspaceChats", () => {
  const environmentId = localEnvironmentId;
  const panel = (id: string, overrides: Record<string, unknown> = {}) => ({
    id,
    environmentId,
    projectId: ProjectId.make("project-1"),
    kind: "panel" as const,
    worktreePath: null as string | null,
    archivedAt: null as string | null,
    hasActionableProposedPlan: false,
    hasPendingApprovals: false,
    hasPendingUserInput: false,
    interactionMode: "default" as const,
    latestTurn: null,
    session: null,
    unresolvedDelivery: null,
    ...overrides,
  });

  it("groups panels by worktree path, sends pathless panels to the main checkout and skips archived ones", () => {
    const summaries = summarizeWorkspaceChats(
      [
        panel("a", { worktreePath: "/wt/fix" }),
        panel("b", { worktreePath: "/wt/fix", hasPendingApprovals: true }),
        panel("c"),
        panel("d", { archivedAt: "2026-03-09T10:00:00.000Z" }),
        { ...panel("e"), kind: "workspace" as const },
      ],
      () => null,
    );
    expect(
      summaries.get(
        workspaceCheckoutKey({
          environmentId,
          projectId: ProjectId.make("project-1"),
          worktreePath: "/wt/fix",
        }),
      ),
    ).toEqual({ count: 2, status: WORKSPACE_CARD_STATUS.approval });
    expect(
      summaries.get(
        workspaceCheckoutKey({
          environmentId,
          projectId: ProjectId.make("project-1"),
          worktreePath: null,
        }),
      ),
    ).toEqual({ count: 1, status: WORKSPACE_CARD_STATUS.idle });
    expect(summaries.size).toBe(2);
  });

  it("keeps worktree keys apart across environments and projects", () => {
    expect(
      workspaceCheckoutKey({ environmentId: "a", projectId: "p", worktreePath: "/wt" }),
    ).not.toBe(workspaceCheckoutKey({ environmentId: "b", projectId: "p", worktreePath: "/wt" }));
    expect(
      workspaceCheckoutKey({ environmentId: "a", projectId: "p", worktreePath: null }),
    ).not.toBe(workspaceCheckoutKey({ environmentId: "a", projectId: "q", worktreePath: null }));
  });
});

describe("isWorktreeSessionRunning", () => {
  const environmentId = localEnvironmentId;
  const session = (status: OrchestrationSessionStatus, activeTurnId: TurnId | null = null) => ({
    threadId: ThreadId.make("session-thread"),
    status,
    providerName: "Claude Code",
    runtimeMode: DEFAULT_RUNTIME_MODE,
    activeTurnId,
    lastError: null,
    updatedAt: "2026-03-09T10:00:00.000Z",
  });
  const thread = (id: string, overrides: Record<string, unknown> = {}) => ({
    id,
    environmentId: environmentId as string,
    projectId: "project-1",
    worktreePath: "/wt/fix" as string | null,
    archivedAt: null as string | null,
    session: null as ReturnType<typeof session> | null,
    ...overrides,
  });

  it("is true while the card's own session runs", () => {
    const card = thread("card", { session: session("running") });
    expect(isWorktreeSessionRunning(card, [card])).toBe(true);
  });

  it("is true while a chat open in the same worktree runs", () => {
    const card = thread("card", { session: session("ready") });
    const chat = thread("chat", { session: session("running") });
    expect(isWorktreeSessionRunning(card, [card, chat])).toBe(true);
  });

  it("refuses Delete Worktree for the realistic busy case: a running session mid-turn", () => {
    const busy = session("running", TurnId.make("turn-busy"));
    const card = thread("card", { session: busy });
    expect(isWorktreeSessionRunning(card, [card])).toBe(true);
    const idleCard = thread("idle-card", { session: session("ready") });
    const chat = thread("chat", { session: busy });
    expect(isWorktreeSessionRunning(idleCard, [idleCard, chat])).toBe(true);
  });

  it("ignores other worktrees, other environments, the main checkout and archived chats", () => {
    const card = thread("card");
    expect(
      isWorktreeSessionRunning(card, [
        card,
        thread("other-worktree", { worktreePath: "/wt/other", session: session("running") }),
        thread("other-environment", {
          environmentId: "environment-remote",
          session: session("running"),
        }),
        thread("main-checkout", { worktreePath: null, session: session("running") }),
        thread("archived", {
          archivedAt: "2026-03-09T10:00:00.000Z",
          session: session("running"),
        }),
      ]),
    ).toBe(false);
  });

  it("counts a starting session on the card or another chat in the checkout", () => {
    const card = thread("card", { session: session("starting") });
    expect(isWorktreeSessionRunning(card, [card])).toBe(true);
    const idleCard = thread("idle-card");
    expect(isWorktreeSessionRunning(idleCard, [idleCard, card])).toBe(true);
  });

  it("ignores sessions that are neither running nor starting", () => {
    const card = thread("card");
    for (const status of ["idle", "ready", "interrupted", "stopped", "error"] as const) {
      expect(
        isWorktreeSessionRunning(card, [card, thread("chat", { session: session(status) })]),
      ).toBe(false);
    }
  });

  it("is false for a thread without a worktree", () => {
    const card = thread("card", { worktreePath: null, session: session("running") });
    expect(isWorktreeSessionRunning(card, [card])).toBe(false);
  });
});

describe("resolveWorkspaceCardClassName", () => {
  // Chats are rows inside their project's card: the card draws the outline, not each chat.
  it("draws idle chats as borderless rows with a hover wash", () => {
    const idle = resolveWorkspaceCardClassName({ isActive: false, isSelected: false });
    expect(idle).toContain("border-transparent");
    expect(idle).not.toContain("border-border");
    expect(idle).toContain("hover:bg-accent/60");
  });

  it("fills the active chat strongly enough to read on the card in both themes", () => {
    const active = resolveWorkspaceCardClassName({ isActive: true, isSelected: false });
    expect(active).toContain("border-transparent");
    expect(active).toContain("bg-foreground/8");
    expect(active).toContain("dark:bg-foreground/12");
    expect(active).not.toContain("font-medium");
  });

  it("keeps the multi-select tints with a tinted border", () => {
    const selected = resolveWorkspaceCardClassName({ isActive: false, isSelected: true });
    expect(selected).toContain("bg-primary/15");
    expect(selected).toContain("border-primary/40");
    expect(selected).not.toContain("border-transparent");
    const selectedActive = resolveWorkspaceCardClassName({ isActive: true, isSelected: true });
    expect(selectedActive).toContain("bg-primary/22");
    expect(selectedActive).toContain("border-primary/60");
  });
});

describe("keyboard context menu helpers", () => {
  const key = (
    overrides: Partial<Record<"key" | "shiftKey" | "ctrlKey" | "altKey" | "metaKey", unknown>>,
  ) =>
    ({ key: "", shiftKey: false, ctrlKey: false, altKey: false, metaKey: false, ...overrides }) as {
      key: string;
      shiftKey: boolean;
      ctrlKey: boolean;
      altKey: boolean;
      metaKey: boolean;
    };

  it("recognises Shift+F10 and the Menu key only", () => {
    expect(isContextMenuShortcut(key({ key: "F10", shiftKey: true }))).toBe(true);
    expect(isContextMenuShortcut(key({ key: "ContextMenu" }))).toBe(true);
    expect(isContextMenuShortcut(key({ key: "F10" }))).toBe(false);
    expect(isContextMenuShortcut(key({ key: "F10", shiftKey: true, ctrlKey: true }))).toBe(false);
    expect(isContextMenuShortcut(key({ key: "Enter" }))).toBe(false);
  });

  it("treats a contextmenu inside the window after a keyboard open as its echo", () => {
    expect(isKeyboardContextMenuEcho(1_000, 1_000)).toBe(true);
    expect(isKeyboardContextMenuEcho(1_000, 1_000 + KEYBOARD_CONTEXT_MENU_ECHO_MS - 1)).toBe(true);
    expect(isKeyboardContextMenuEcho(1_000, 1_000 + KEYBOARD_CONTEXT_MENU_ECHO_MS)).toBe(false);
    expect(isKeyboardContextMenuEcho(Number.NEGATIVE_INFINITY, 5)).toBe(false);
  });
});
