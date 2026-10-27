/**
 * Unit tests for the Sidebar mega-component.
 *
 * Strategy: the sidebar's leaf UI primitives, router, and environment-bound
 * state hooks are replaced with capture-mocks that record every props object
 * they receive during a `renderToStaticMarkup` pass. Tests then walk the
 * captured React element trees to find host elements/handlers (by
 * data-testid / aria-label) and invoke them directly with fake events, which
 * exercises the component's callback bodies without a DOM.
 */
import {
  ENV_MAIN,
  ENV_REMOTE,
  ENV_WSL,
  baseScenario,
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
  threadDefault,
  threadIdle,
  threadKeyOf,
} from "./Sidebar.testHarness";
import * as Cause from "effect/Cause";
import * as Schema from "effect/Schema";
import * as React from "react";
import { beforeEach, expect, it, vi } from "vite-plus/test";
import {
  DEFAULT_SERVER_SETTINGS,
  ProjectId,
  ProviderInstanceId,
  ThreadId,
  VcsStatusSummary,
  WorktreeKey,
  WorktreeRepositoryKey,
} from "@coddy/contracts";
import { DEFAULT_CLIENT_SETTINGS } from "@coddy/contracts/settings";
import { createModelSelection } from "@coddy/shared/model";
import { scopeThreadRef } from "@coddy/client-runtime/environment";
import { createEnvironmentPresentationPolicy } from "../connection/environmentPresentationPolicy";
import { derivePhysicalProjectKey } from "../logicalProject";
import type { EnvironmentThreadShell } from "@coddy/client-runtime/state/shell";
import Sidebar, {
  SidebarBrandContent,
  SidebarThreadRow,
  handleSidebarNavigationKeyDown,
  handleSidebarSelectionMouseDown,
  projectExpansionPreferenceKeys,
  projectExpansionToggleKeys,
  projectHeaderButtonClassName,
} from "./Sidebar";
import { resolveProjectExpanded, setProjectExpanded, type UiState } from "../uiStateStore";
import { WORKSPACE_CARD_STATUS } from "./Sidebar.logic";

const decodeStatusSummary = Schema.decodeUnknownSync(VcsStatusSummary);

/** A card menu item as `api.contextMenu.show` receives it. */
type CardMenuItem = {
  id?: string;
  label?: string;
  disabled?: boolean;
  description?: string;
  separator?: true;
};

/** Sets the session status of the worktree fixture, whose own session is running. */
function setWorktreeSessionStatus(
  status: NonNullable<EnvironmentThreadShell["session"]>["status"],
): void {
  h.state.threads = h.state.threads.map((thread: EnvironmentThreadShell) =>
    thread.id === threadActive.id
      ? { ...threadActive, session: { ...threadActive.session!, status } }
      : thread,
  );
}

/** Makes the card menu choose Delete; `current` is the Delete item the menu was built with. */
function chooseDeleteFromCardMenu(): { current: CardMenuItem | undefined } {
  const deleteItem: { current: CardMenuItem | undefined } = { current: undefined };
  h.spies.contextMenuShow.mockImplementation(async (items: CardMenuItem[]) => {
    deleteItem.current = items.find((item) => item.id === "delete");
    return "delete";
  });
  return deleteItem;
}

staticDescribe("Sidebar global event helpers", () => {
  function navigationEvent(overrides: { defaultPrevented?: boolean; repeat?: boolean } = {}) {
    return {
      defaultPrevented: overrides.defaultPrevented ?? false,
      repeat: overrides.repeat ?? false,
      preventDefault: vi.fn(),
      stopPropagation: vi.fn(),
    };
  }

  it("writes a shared expansion key right after the project key", () => {
    const project = {
      projectKey: "project-key",
      sharedExpansionKey: "github.com/acme/repo-a",
      memberProjects: [{ physicalProjectKey: "member-key", workspaceRoot: "/work/repo" }],
    } as unknown as Parameters<typeof projectExpansionPreferenceKeys>[0];
    const keys = projectExpansionPreferenceKeys(project);
    expect(keys.slice(0, 3)).toEqual(["project-key", "github.com/acme/repo-a", "member-key"]);
    const { sharedExpansionKey: _omitted, ...withoutShared } = project;
    expect(projectExpansionPreferenceKeys(withoutShared)).toEqual(
      keys.filter((key) => key !== "github.com/acme/repo-a"),
    );
  });

  it("collapses one environment card without collapsing its sibling", () => {
    type Snapshot = Parameters<typeof projectExpansionPreferenceKeys>[0];
    const card = (physical: string, workspaceRoot: string) =>
      ({
        projectKey: physical,
        sharedExpansionKey: "github.com/acme/repo-a",
        memberProjects: [{ physicalProjectKey: physical, workspaceRoot }],
      }) as unknown as Snapshot;
    // The same checkout path on both environments: the path-only legacy key must stay read-only.
    const cardA = card("env-a:/work/repo", "/work/repo");
    const cardB = card("env-b:/work/repo", "/work/repo");
    const fresh = { projectExpandedById: {} } as unknown as UiState;
    const expanded = (state: UiState, project: Snapshot) =>
      resolveProjectExpanded(state.projectExpandedById, projectExpansionPreferenceKeys(project));

    const collapsedA = setProjectExpanded(fresh, projectExpansionToggleKeys(cardA), false);
    expect(expanded(collapsedA, cardA)).toBe(false);
    expect(expanded(collapsedA, cardB)).toBe(true);
    expect(new Set(projectExpansionToggleKeys(cardA))).toEqual(new Set(["env-a:/work/repo"]));

    // An Environments-view row toggle writes the shared key, which a card without its own state reads.
    const row = {
      projectKey: "github.com/acme/repo-a",
      memberProjects: [{ physicalProjectKey: "env-a:/work/repo", workspaceRoot: "/work/repo" }],
    } as unknown as Snapshot;
    const collapsedRow = setProjectExpanded(fresh, projectExpansionToggleKeys(row), false);
    expect(expanded(collapsedRow, cardB)).toBe(false);
  });

  it("lets the project header button grow only for an environment card", () => {
    const base = { showsSandboxBadge: false, isManualProjectSorting: false };
    expect(projectHeaderButtonClassName({ ...base, isEnvironmentCard: true })).toContain("h-auto");
    expect(projectHeaderButtonClassName({ ...base, isEnvironmentCard: false })).not.toContain(
      "h-auto",
    );
  });

  it("handles traversal and numbered jump shortcuts across every guard", () => {
    const first = makeThread("first");
    const second = makeThread("second");
    const firstKey = threadKeyOf(first);
    const secondKey = threadKeyOf(second);
    const threadByKey = new Map([
      [firstKey, first],
      [secondKey, second],
    ]);
    const navigateToThread = vi.fn();
    const base = {
      orderedThreadKeys: [firstKey, secondKey],
      currentThreadKey: firstKey,
      jumpThreadKeys: [secondKey],
      threadByKey,
      navigateToThread,
    };

    expect(
      handleSidebarNavigationKeyDown({
        ...base,
        event: navigationEvent({ defaultPrevented: true }),
        resolveCommand: () => "thread.next",
      }),
    ).toBe(false);
    expect(
      handleSidebarNavigationKeyDown({
        ...base,
        event: navigationEvent({ repeat: true }),
        resolveCommand: () => "thread.next",
      }),
    ).toBe(false);
    expect(
      handleSidebarNavigationKeyDown({
        ...base,
        event: navigationEvent(),
        resolveCommand: () => null,
      }),
    ).toBe(false);

    const traversalEvent = navigationEvent();
    expect(
      handleSidebarNavigationKeyDown({
        ...base,
        event: traversalEvent,
        resolveCommand: () => "thread.next",
      }),
    ).toBe(true);
    expect(traversalEvent.preventDefault).toHaveBeenCalledOnce();
    expect(navigateToThread).toHaveBeenLastCalledWith(scopeThreadRef(ENV_MAIN, second.id));

    const jumpEvent = navigationEvent();
    expect(
      handleSidebarNavigationKeyDown({
        ...base,
        event: jumpEvent,
        resolveCommand: () => "thread.jump.1",
      }),
    ).toBe(true);
    expect(jumpEvent.stopPropagation).toHaveBeenCalledOnce();

    expect(
      handleSidebarNavigationKeyDown({
        ...base,
        event: navigationEvent(),
        currentThreadKey: secondKey,
        resolveCommand: () => "thread.next",
      }),
    ).toBe(false);
    expect(
      handleSidebarNavigationKeyDown({
        ...base,
        event: navigationEvent(),
        jumpThreadKeys: [],
        resolveCommand: () => "thread.jump.1",
      }),
    ).toBe(false);
    expect(
      handleSidebarNavigationKeyDown({
        ...base,
        event: navigationEvent(),
        jumpThreadKeys: ["missing"],
        resolveCommand: () => "thread.jump.1",
      }),
    ).toBe(false);
  });

  it("clears selection only for mouse targets outside safe controls", () => {
    class FakeHtmlElement {
      constructor(private readonly safe: boolean) {}
      closest(): FakeHtmlElement | null {
        return this.safe ? this : null;
      }
    }
    vi.stubGlobal("HTMLElement", FakeHtmlElement);
    const clearSelection = vi.fn();

    expect(
      handleSidebarSelectionMouseDown({ hasSelection: false, target: null, clearSelection }),
    ).toBe(false);
    expect(
      handleSidebarSelectionMouseDown({
        hasSelection: true,
        target: new FakeHtmlElement(true) as never,
        clearSelection,
      }),
    ).toBe(false);
    expect(
      handleSidebarSelectionMouseDown({
        hasSelection: true,
        target: new FakeHtmlElement(false) as never,
        clearSelection,
      }),
    ).toBe(true);
    expect(clearSelection).toHaveBeenCalledOnce();
  });
});

staticDescribe("SidebarBrandContent", () => {
  it("renders base name and stage label", () => {
    const markup = render(<SidebarBrandContent appBaseName="Coddy Studio" stageLabel="Dev" />);
    expect(markup).toContain("Coddy Studio");
    expect(markup).toContain("Dev");
    // UI.md: no letter-spacing; the stage badge keeps its capitals.
    expect(markup).not.toContain("tracking-");
    expect(markup).toContain("uppercase");
  });
});

staticDescribe("Sidebar full render", () => {
  it("renders the Projects label and Settings in the nav rows' type", () => {
    baseScenario();
    const markup = render(<Sidebar />);
    expect(markup).toContain(
      '<span class="text-xs font-medium text-muted-foreground">Projects</span>',
    );
    expect(markup).toContain('<span class="text-[13px] font-medium">Settings</span>');
    expect(markup).not.toContain("uppercase tracking-wider");
  });

  it("summarises a collapsed project and the hidden cards with the card glyphs", () => {
    baseScenario();
    h.state.threads = [
      threadDefault,
      threadActive,
      makeThread("thread-idle", { title: "Idle thread", hasPendingApprovals: true }),
    ];
    h.state.clientSettings = { ...DEFAULT_CLIENT_SETTINGS, sidebarThreadPreviewCount: 1 };
    const expanded = render(<Sidebar />);
    // thread-idle sorts below thread-active, so it is hidden behind Show more.
    expect(expanded).toMatch(/aria-label="Needs approval"[\s\S]*Show more/);

    h.uiStore.setState({
      projectExpandedById: { [derivePhysicalProjectKey(projectA)]: false },
    });
    h.state.routeParams = {};
    const collapsed = render(<Sidebar />);
    expect(collapsed).toContain('aria-label="Needs approval"');
    expect(collapsed).not.toContain('data-testid="thread-row-thread-idle"');
  });

  it("collects lookup data once for the actual project list, not once per card", () => {
    baseScenario();
    h.state.threads = [
      threadDefault,
      ...Array.from({ length: 6 }, (_, i) =>
        makeThread("lookup-" + i, { title: "Workspace " + i }),
      ),
    ];
    h.spies.cardLookupPass.mockClear();
    render(<Sidebar />);
    expect(h.spies.cardLookupPass).toHaveBeenCalledTimes(1);
    const input = h.spies.cardLookupPass.mock.calls[0]![0] as { threads: readonly unknown[] };
    expect(input.threads.length).toBeGreaterThan(1);
  });

  it("passes active terminal surfaces to shortcut label resolution", () => {
    baseScenario();
    h.state.terminalSurfaceOpen = true;

    render(<Sidebar />);

    expect(h.state.shortcutLabelOptions).toContainEqual(
      expect.objectContaining({
        context: expect.objectContaining({ terminalOpen: true }),
      }),
    );
  });

  it("surfaces a failed session in the sidebar instead of hiding it as disconnected", () => {
    // `derivePhase` collapses `status: "error"` into `disconnected`, so without
    // an explicit branch an errored thread is indistinguishable from a stopped
    // one and the failure is invisible outside the open chat.
    baseScenario();
    h.state.threads = [
      threadDefault,
      makeThread("thread-failed", {
        title: "Failed worktree",
        branch: "feat/y",
        worktreePath: "C:/wt/y",
        session: {
          threadId: ThreadId.make("thread-failed"),
          status: "error",
          providerName: "Claude Code",
          activeTurnId: null,
          lastError: "The upstream API returned an error (HTTP 401).",
          updatedAt: iso(30),
          runtimeMode: "full-access",
        },
      }),
    ];
    const markup = render(<Sidebar />);

    expect(markup).toContain('aria-label="Failed"');
    expect(markup).toContain("Claude Code");
    expect(markup).not.toContain("Connecting");
  });

  it("renders the project header, primary row, and workspace thread rows", () => {
    baseScenario();
    const markup = render(<Sidebar />);

    expect(markup).toContain("Repo A");
    expect(markup).toContain(">primary<");
    expect(markup).toContain("thread-row-thread-active");
    expect(markup).toContain("thread-row-thread-idle");
    // Panel and archived threads never render as workspace rows.
    expect(markup).not.toContain("thread-row-thread-panel");
    expect(markup).not.toContain("thread-row-thread-archived");
    // A running session shows the Working glyph; line 3 names the provider.
    expect(markup).toContain('aria-label="Working"');
    expect(markup).toContain("Claude Code");
    expect(markup).not.toContain("thread-agent-row-");
    // The live branch from vcs.status becomes the primary row title.
    expect(markup).toContain("main");
    // Search entry + shortcut label.
    expect(markup).toContain("Search");
    expect(markup).toContain("Mod+K");
    const searchIndex = markup.indexOf("Search");
    const agentsNavIndex = markup.indexOf('data-testid="agents-nav-row"');
    const projectsIndex = markup.indexOf("Projects");
    expect(agentsNavIndex).toBeGreaterThan(searchIndex);
    expect(agentsNavIndex).toBeLessThan(projectsIndex);
    expect(markup).not.toContain("agents-section-header");
  });

  it("uses the passive summary for the primary project row", () => {
    baseScenario();
    render(<Sidebar />);

    expect(h.state.vcsQueries).toContainEqual(
      expect.objectContaining({
        __q: "vcs.summary",
        args: expect.objectContaining({ input: { cwd: "C:/repo-a" } }),
      }),
    );
  });

  it("uses the detached HEAD identity for the primary project row", () => {
    baseScenario();
    h.state.vcsStatusByCwd["C:/repo-a"] = {
      refName: null,
      detachedHead: "abc1234",
    };

    const markup = render(<Sidebar />);

    expect(markup).toContain("abc1234");
  });

  // After a Git Manager checkout the summary names the new branch, then a failed
  // PR lookup republishes it as stale. The default thread still records the branch
  // its chat header last synced, so falling back to it showed the old branch.
  it("titles the primary card with a stale summary's branch, not the recorded one", () => {
    baseScenario();
    h.state.threads = [{ ...threadDefault, branch: "main" }];
    h.state.vcsStatusByCwd["C:/repo-a"] = {
      refName: "feature",
      detachedHead: null,
      stale: true,
    };

    const markup = render(<Sidebar />);

    expect(markup.match(/data-testid="primary-card-title-project-a"[^>]*>([^<]+)/)?.[1]).toBe(
      "feature",
    );
  });

  it("renders the settings navigation when routed to /settings", () => {
    baseScenario();
    h.state.pathname = "/settings/appearance";
    render(<Sidebar />);

    const nav = captured("SettingsSidebarNav");
    expect(nav).toHaveLength(1);
    expect(nav[0]!.props["pathname"]).toBe("/settings/appearance");
  });

  it("renders the empty projects state only after a live authoritative snapshot", () => {
    h.state.shellSummary = {
      ...h.state.shellSummary,
      catalogReady: true,
      desiredEnvironmentCount: 1,
      statuses: [
        {
          environmentId: ENV_MAIN,
          status: "live",
          hasSnapshot: true,
          error: null,
        },
      ],
      canShowEmptyProjects: true,
      hasSnapshot: true,
      hasLiveShell: true,
    };
    const markup = render(<Sidebar />);
    expect(markup).toContain("No projects yet");
  });

  it("offers Add project from an empty Repositories view", () => {
    h.state.shellSummary = {
      ...h.state.shellSummary,
      catalogReady: true,
      desiredEnvironmentCount: 1,
      statuses: [{ environmentId: ENV_MAIN, status: "live", hasSnapshot: true, error: null }],
      canShowEmptyProjects: true,
      hasSnapshot: true,
      hasLiveShell: true,
    };
    h.uiStore.setState({ sidebarView: "repositories" });
    const markup = render(<Sidebar />);
    expect(markup).toContain("No projects yet.");
    expect(markup).not.toContain("Switch to Environments");
    invoke(
      mustFindProps(
        (props) => props["children"] === "Add project" && props["variant"] === "outline",
        "empty-state add project",
      ),
      "onClick",
      mouseEvent(),
    );
    expect(h.spies.openAddProject).toHaveBeenCalledTimes(1);
  });

  it("renders loading rather than claiming an empty catalog before catalog readiness", () => {
    const markup = render(<Sidebar />);
    expect(markup).toContain("Project data is still loading");
    expect(markup).not.toContain("No projects yet");
  });

  it("keeps a saved remote project row while hiding desktop remote recovery controls", () => {
    h.state.presentationPolicy = createEnvironmentPresentationPolicy({
      surface: "desktop",
      platform: "macos",
    });
    const savedRemoteProject = makeProject("saved-remote", {
      environmentId: ENV_REMOTE,
      title: "Saved remote repository",
      workspaceRoot: "/srv/saved-remote",
    });
    h.state.projects = [savedRemoteProject];
    h.state.environments = [
      environmentFixture({
        environmentId: ENV_REMOTE,
        label: "Remote Box",
        connectionId: "remote:box",
        phase: "error",
        error: "Remote host is offline.",
      }),
    ];
    h.state.shellSummary = {
      ...h.state.shellSummary,
      catalogReady: true,
      desiredEnvironmentCount: 1,
      statuses: [
        {
          environmentId: ENV_REMOTE,
          status: "unavailable",
          hasSnapshot: true,
          error: "Remote host is offline.",
        },
      ],
      hasSnapshot: true,
      hasCachedShell: true,
    };

    const markup = render(<Sidebar />);
    expect(markup).toContain("Saved remote repository");
    const availabilityActions = captured("Button").map((entry) => entry.props["children"]);
    expect(availabilityActions).toContain("Diagnostics");
    expect(availabilityActions).not.toContain("Retry");
    expect(availabilityActions).not.toContain("Settings");
  });

  it("wires storage-change actions and confirms storage adoption through the local dialog", async () => {
    fakeLocalApi();
    h.spies.dialogConfirm.mockResolvedValue(true);
    (globalThis.window as unknown as Record<string, unknown>)["desktopBridge"] = {
      getProjectDataStatuses: vi.fn(),
    };
    h.state.environments = [
      environmentFixture({
        environmentId: ENV_MAIN,
        label: "Local",
        primary: true,
      }),
    ];
    h.state.shellSummary = {
      ...h.state.shellSummary,
      catalogReady: true,
      desiredEnvironmentCount: 1,
      statuses: [
        {
          environmentId: ENV_MAIN,
          status: "storage-changed",
          hasSnapshot: false,
          error: "Persistent storage changed.",
        },
      ],
    };
    const markup = render(<Sidebar />);
    expect(markup).toContain("Project data location changed");

    invoke(
      mustFindProps((props) => props["children"] === "Retry", "project retry action"),
      "onClick",
      mouseEvent(),
    );
    invoke(
      mustFindProps((props) => props["children"] === "Recover data", "project recovery action"),
      "onClick",
      mouseEvent(),
    );
    invoke(
      mustFindProps((props) => props["children"] === "Settings", "project settings action"),
      "onClick",
      mouseEvent(),
    );
    invoke(
      mustFindProps((props) => props["children"] === "Diagnostics", "project diagnostics action"),
      "onClick",
      mouseEvent(),
    );
    invoke(
      mustFindProps(
        (props) => props["children"] === "Use this data location",
        "project storage adoption action",
      ),
      "onClick",
      mouseEvent(),
    );
    await flush();

    expect(h.state.commandCalls).toEqual([
      { label: "environment.retry", input: ENV_MAIN },
      { label: "environment.adoptStorage", input: ENV_MAIN },
    ]);
    expect(h.spies.navigate).toHaveBeenCalledWith({ to: "/settings/remote-servers" });
    expect(h.spies.navigate).toHaveBeenCalledWith({ to: "/settings/diagnostics" });
    expect(h.spies.openProjectDataRecovery).toHaveBeenCalledWith(ENV_MAIN, "manual");
    expect(h.spies.dialogConfirm).toHaveBeenCalledWith(
      "Use this project data location? Projects from the two locations will not be merged.",
    );
    expect(h.spies.windowConfirm).not.toHaveBeenCalled();
  });

  it("cancels storage adoption when the local dialog is declined", async () => {
    fakeLocalApi();
    h.spies.dialogConfirm.mockResolvedValue(false);
    h.state.environments = [
      environmentFixture({
        environmentId: ENV_MAIN,
        label: "Local",
        primary: true,
      }),
    ];
    h.state.shellSummary = {
      ...h.state.shellSummary,
      catalogReady: true,
      desiredEnvironmentCount: 1,
      statuses: [
        {
          environmentId: ENV_MAIN,
          status: "storage-changed",
          hasSnapshot: false,
          error: "Persistent storage changed.",
        },
      ],
    };
    render(<Sidebar />);

    invoke(
      mustFindProps(
        (props) => props["children"] === "Use this data location",
        "project storage adoption action",
      ),
      "onClick",
      mouseEvent(),
    );
    await flush();

    expect(h.spies.dialogConfirm).toHaveBeenCalledWith(
      "Use this project data location? Projects from the two locations will not be merged.",
    );
    expect(h.spies.windowConfirm).not.toHaveBeenCalled();
    expect(h.state.commandCalls).not.toContainEqual({
      label: "environment.adoptStorage",
      input: ENV_MAIN,
    });
  });

  it("shows only the primary card for an expanded project without workspace threads", () => {
    h.state.projects = [projectA];
    h.state.threads = [threadDefault];
    h.state.environments = [environmentFixture({ environmentId: ENV_MAIN, label: "Main" })];
    const markup = render(<Sidebar />);
    expect(markup).toContain('data-testid="primary-card-project-a"');
    expect(markup).not.toContain("No threads yet");
  });

  it("collapses a project but keeps the active thread row visible", () => {
    baseScenario();
    h.uiStore.setState({
      projectExpandedById: { [derivePhysicalProjectKey(projectA)]: false },
    });
    const markup = render(<Sidebar />);
    // Active thread peeks through even while collapsed; the inactive primary card does not.
    expect(markup).toContain("thread-row-thread-active");
    expect(markup).not.toContain("thread-row-thread-idle");
    expect(markup).not.toContain('data-testid="primary-card-project-a"');
  });

  it("writes only an environment card's own expansion keys, not the repository row's", () => {
    groupedScenario();
    h.uiStore.setState({ sidebarView: "repositories" });
    // The header's test id sits on EnvironmentCardHeader's own output, which the harness does not capture.
    expect(render(<Sidebar />)).toContain(`data-testid="environment-card-header-${ENV_REMOTE}"`);
    const toggle = mustFindProps(
      (props) =>
        props["aria-expanded"] !== undefined &&
        typeof props["onClick"] === "function" &&
        String(props["className"] ?? "").includes("group-hover/project-header"),
      "environment card toggle",
    );
    invoke(toggle, "onClick", mouseEvent());
    expect(h.spies.setProjectExpanded).toHaveBeenCalledTimes(1);
    expect(h.spies.setProjectExpanded).not.toHaveBeenCalledWith(
      expect.arrayContaining(["github.com/acme/repo-a"]),
      expect.anything(),
    );
  });

  it("lets an environment card's header grow, but not a project row's", () => {
    const projectHeaderClassNames = () =>
      captured("SidebarMenuButton")
        .map((entry) => String(entry.props["className"] ?? ""))
        .filter((className) => className.includes("group-hover/project-header"));
    groupedScenario();
    render(<Sidebar />);
    expect(projectHeaderClassNames().length).toBeGreaterThan(0);
    expect(projectHeaderClassNames().some((className) => className.includes("h-auto"))).toBe(false);
    h.uiStore.setState({ sidebarView: "repositories" });
    render(<Sidebar />);
    expect(projectHeaderClassNames()).toHaveLength(2);
    expect(projectHeaderClassNames().every((className) => className.includes("h-auto"))).toBe(true);
  });

  it("groups every environment's checkout of a repository under one read-only card", () => {
    groupedScenario();
    h.uiStore.setState({ sidebarView: "repositories" });
    const markup = render(<Sidebar />);
    expect(markup.match(/data-testid="repository-group-github\.com\/acme\/repo-a"/g)).toHaveLength(
      1,
    );
    expect(markup).toContain(`data-testid="environment-card-header-${ENV_MAIN}"`);
    expect(markup).toContain(`data-testid="environment-card-header-${ENV_REMOTE}"`);
    expect(markup).toContain("2 environments");
    expect(markup).toContain("Repositories · all environments");
    expect(markup).not.toContain('data-testid="repository-group-actions"');
    // The view always groups by repository, so the sort menu offers no grouping choice.
    expect(markup).not.toContain("Group projects");
  });

  it("lifts each Environments view project onto a card over the list well", () => {
    baseScenario();
    const markup = render(<Sidebar />);
    expect(
      String(mustFindProps(byTestId("sidebar-projects-group"), "projects group")["className"]),
    ).toContain("bg-sidebar-well");
    expect(markup.match(/data-sidebar-card="true"/g)).toHaveLength(1);
  });

  it("makes repositories the cards in the Repositories view, with environment rows on a band", () => {
    groupedScenario();
    h.uiStore.setState({ sidebarView: "repositories" });
    const markup = render(<Sidebar />);
    // One repository card; its two environment checkouts are bands inside it, not nested cards.
    expect(markup.match(/data-sidebar-card="true"/g)).toHaveLength(1);
    expect(markup.match(/bg-sidebar-env-band/g)).toHaveLength(2);
  });

  it("orders Repositories view groups by name by default, not by recent activity", () => {
    h.state.projects = [
      makeProject("project-zeta", { title: "zeta", workspaceRoot: "C:/zeta", updatedAt: iso(1) }),
      makeProject("project-alpha", {
        title: "alpha",
        workspaceRoot: "C:/alpha",
        updatedAt: iso(500),
      }),
    ];
    h.state.environments = [environmentFixture({ environmentId: ENV_MAIN, label: "Main" })];
    h.uiStore.setState({ sidebarView: "repositories" });
    const alphaFirst = (markup: string) => markup.indexOf(">alpha<") < markup.indexOf(">zeta<");

    expect(alphaFirst(render(<Sidebar />))).toBe(true);

    h.state.clientSettings = {
      ...DEFAULT_CLIENT_SETTINGS,
      sidebarRepositorySortOrder: "updated_at",
    };
    expect(alphaFirst(render(<Sidebar />))).toBe(false);
  });

  it("offers repository sort orders in the Repositories view sort menu", () => {
    groupedScenario();
    h.uiStore.setState({ sidebarView: "repositories" });
    const markup = render(<Sidebar />);
    expect(markup).toContain("Sort repositories");
    expect(markup).not.toContain("Sort projects");
    const [repositorySort] = captured("MenuRadioGroup");
    expect(repositorySort!.props["value"]).toBe("name");
    (repositorySort!.props["onValueChange"] as (value: string) => void)("created_at");
    expect(h.spies.updateSettings).toHaveBeenCalledWith({
      sidebarRepositorySortOrder: "created_at",
    });
    expect(markup).not.toContain(">Manual<");
  });

  it("lists every environment's projects even when one environment is selected", () => {
    groupedScenario();
    h.state.activeEnvironmentId = ENV_REMOTE;
    h.uiStore.setState({ sidebarView: "repositories" });
    const markup = render(<Sidebar />);
    expect(markup).toContain(`data-testid="environment-card-header-${ENV_MAIN}"`);
  });

  it("hides a collapsed repository group's environment cards", () => {
    groupedScenario();
    h.uiStore.setState({
      sidebarView: "repositories",
      repositoryGroupExpandedById: { "github.com/acme/repo-a": false },
    });
    const markup = render(<Sidebar />);
    expect(markup).toContain('data-testid="repository-group-github.com/acme/repo-a"');
    expect(markup).not.toContain("environment-card-header-");
  });

  it("reserves the environment rail's width at the header start only in the Repositories view", () => {
    const reserve = "md:pl-[var(--environment-rail-width)]";
    expect(render(<Sidebar />)).not.toContain(reserve);
    h.uiStore.setState({ sidebarView: "repositories" });
    const markup = render(<Sidebar />);
    expect(markup).toContain(reserve);
  });

  it("switches views from the toggle", () => {
    baseScenario();
    expect(render(<Sidebar />)).toContain("Group projects");
    const repositories = mustFindProps(byAriaLabel("Repositories view"), "repositories toggle");
    invoke(repositories, "onClick", mouseEvent());
    expect(h.uiStore.getState().setSidebarView).toHaveBeenCalledWith("repositories");
  });

  it("shows the overflow 'Show more' affordance and expands on click", () => {
    baseScenario();
    h.state.clientSettings = { ...DEFAULT_CLIENT_SETTINGS, sidebarThreadPreviewCount: 1 };
    const markup = render(<Sidebar />);
    expect(markup).toContain("Show more");

    const showMore = mustFindProps(
      (props) =>
        typeof props["onClick"] === "function" &&
        props["data-thread-selection-safe"] !== undefined &&
        props["size"] === "sm" &&
        props["className"] !== undefined &&
        String(props["className"]).includes(
          "text-left text-xs text-muted-foreground hover:bg-accent",
        ),
      "show more button",
    );
    invoke(showMore, "onClick", mouseEvent());
  });

  it("renders the arm64 warning with a download action and runs the download flow", async () => {
    baseScenario();
    h.state.desktopUpdateState = { phase: "idle" };
    h.state.showArmWarning = true;
    h.state.updateBtnAction = "download";
    const downloadUpdate = vi.fn(async () => ({ completed: true, toast: false }));
    (globalThis.window as unknown as Record<string, unknown>)["desktopBridge"] = {
      downloadUpdate,
      installUpdate: vi.fn(),
    };

    const markup = render(<Sidebar />);
    expect(markup).toContain("Intel build on Apple Silicon");
    expect(markup).toContain("Download ARM build");

    const button = captured("Button").find(
      (entry) => entry.props["children"] === "Download ARM build",
    );
    expect(button).toBeDefined();
    invoke(button!.props, "onClick", mouseEvent());
    await flush();
    expect(downloadUpdate).toHaveBeenCalled();
    expect(h.spies.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Update downloaded" }),
    );
  });

  it("toasts when the update download reports an actionable error", async () => {
    baseScenario();
    h.state.desktopUpdateState = { phase: "idle" };
    h.state.showArmWarning = true;
    h.state.updateBtnAction = "download";
    const downloadUpdate = vi.fn(async () => ({ completed: false, toast: true, error: "no disk" }));
    (globalThis.window as unknown as Record<string, unknown>)["desktopBridge"] = {
      downloadUpdate,
      installUpdate: vi.fn(),
    };

    render(<Sidebar />);
    const button = captured("Button").find(
      (entry) => entry.props["children"] === "Download ARM build",
    );
    invoke(button!.props, "onClick", mouseEvent());
    await flush();
    expect(h.spies.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Could not download update", description: "no disk" }),
    );
  });

  it("toasts when the update download rejects", async () => {
    baseScenario();
    h.state.desktopUpdateState = { phase: "idle" };
    h.state.showArmWarning = true;
    h.state.updateBtnAction = "download";
    (globalThis.window as unknown as Record<string, unknown>)["desktopBridge"] = {
      downloadUpdate: vi.fn(async () => {
        throw new Error("network down");
      }),
      installUpdate: vi.fn(),
    };

    render(<Sidebar />);
    const button = captured("Button").find(
      (entry) => entry.props["children"] === "Download ARM build",
    );
    invoke(button!.props, "onClick", mouseEvent());
    await flush();
    expect(h.spies.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({
        title: "Could not start update download",
        description: "network down",
      }),
    );
  });

  it("runs installation through typed project protection and toasts errors", async () => {
    baseScenario();
    h.state.desktopUpdateState = { phase: "downloaded" };
    h.state.showArmWarning = true;
    h.state.updateBtnAction = "install";
    const installUpdate = vi.fn(async () => ({
      completed: false,
      state: { message: "install failed" },
    }));
    (globalThis.window as unknown as Record<string, unknown>)["desktopBridge"] = {
      downloadUpdate: vi.fn(),
      installUpdate,
    };

    render(<Sidebar />);
    const button = captured("Button").find(
      (entry) => entry.props["children"] === "Install ARM build",
    );
    expect(button).toBeDefined();

    // The update action opens the typed protection dialog without using a
    // browser confirmation or invoking the installer directly.
    invoke(button!.props, "onClick", mouseEvent());
    await flush();
    expect(installUpdate).not.toHaveBeenCalled();
    expect(h.spies.windowConfirm).not.toHaveBeenCalled();

    const protectAndInstall = captured("Button").find(
      (entry) => entry.props["children"] === "Protect projects and install",
    );
    expect(protectAndInstall).toBeDefined();
    invoke(protectAndInstall!.props, "onClick", mouseEvent());
    await flush();
    expect(installUpdate).toHaveBeenCalled();
    expect(h.spies.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Could not install update", description: "install failed" }),
    );

    // And: install rejects entirely.
    installUpdate.mockRejectedValueOnce(new Error("io error"));
    invoke(protectAndInstall!.props, "onClick", mouseEvent());
    await flush();
    expect(h.spies.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Could not install update", description: "io error" }),
    );
  });

  it("keeps update actions quiet when the bridge or actionable result is absent", async () => {
    baseScenario();
    h.state.desktopUpdateState = { phase: "idle" };
    h.state.showArmWarning = true;
    h.state.updateBtnAction = "download";
    render(<Sidebar />);
    const button = captured("Button").find(
      (entry) => entry.props["children"] === "Download ARM build",
    )!;
    invoke(button.props, "onClick", mouseEvent());
    expect(h.spies.toastAdd).not.toHaveBeenCalled();

    const downloadUpdate = vi.fn(async () => ({ completed: false, toast: false }));
    (globalThis.window as unknown as Record<string, unknown>)["desktopBridge"] = {
      downloadUpdate,
      installUpdate: vi.fn(),
    };
    invoke(button.props, "onClick", mouseEvent());
    await flush();
    expect(downloadUpdate).toHaveBeenCalled();
    expect(h.spies.toastAdd).not.toHaveBeenCalled();
  });

  it("uses generic messages for opaque update failures", async () => {
    baseScenario();
    h.state.desktopUpdateState = { phase: "idle" };
    h.state.showArmWarning = true;
    h.state.updateBtnAction = "download";
    (globalThis.window as unknown as Record<string, unknown>)["desktopBridge"] = {
      downloadUpdate: vi.fn(async () => Promise.reject("opaque download failure")),
      installUpdate: vi.fn(),
    };
    render(<Sidebar />);
    const download = captured("Button").find(
      (entry) => entry.props["children"] === "Download ARM build",
    )!;
    invoke(download.props, "onClick", mouseEvent());
    await flush();
    expect(h.spies.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ description: "An unexpected error occurred." }),
    );

    h.spies.toastAdd.mockClear();
    h.state.desktopUpdateState = { phase: "downloaded" };
    h.state.updateBtnAction = "install";
    (globalThis.window as unknown as Record<string, unknown>)["desktopBridge"] = {
      downloadUpdate: vi.fn(),
      installUpdate: vi.fn(async () => Promise.reject("opaque install failure")),
    };
    h.state.captures.length = 0;
    render(<Sidebar />);
    const install = captured("Button").find(
      (entry) => entry.props["children"] === "Install ARM build",
    )!;
    invoke(install.props, "onClick", mouseEvent());
    await flush();
    const protectAndInstall = captured("Button").find(
      (entry) => entry.props["children"] === "Protect projects and install",
    )!;
    invoke(protectAndInstall.props, "onClick", mouseEvent());
    await flush();
    expect(h.spies.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ description: "An unexpected error occurred." }),
    );
  });

  it("renders local secondary backend connection status", () => {
    baseScenario();
    h.state.environments = [
      environmentFixture({ environmentId: ENV_MAIN, label: "Main", connectionId: "primary" }),
      environmentFixture({
        environmentId: ENV_WSL,
        label: "WSL",
        connectionId: "local:wsl",
        displayUrl: "http://localhost:9001",
        phase: "error",
        error: "boot failed",
      }),
    ];
    h.state.desktopBootstraps = [
      { label: "WSL", httpBaseUrl: "http://localhost:9001" },
      { label: "Podman", httpBaseUrl: null },
    ];
    const markup = render(<Sidebar />);
    expect(markup).toContain("Connecting Podman");
    expect(markup).toContain("Couldn&#x27;t connect WSL");
    expect(markup).toContain("boot failed");
  });

  it("renders an unavailable local secondary as failed instead of connecting", () => {
    baseScenario();
    h.state.desktopBootstraps = [
      {
        id: "wsl:ubuntu-20.04",
        label: "WSL (Ubuntu-20.04)",
        httpBaseUrl: null,
        wsBaseUrl: null,
        preflightError: {
          kind: "wsl-secondary-unavailable",
          detail: "Could not find a Linux coddy-studio server binary for WSL.",
        },
      },
    ];

    const markup = render(<Sidebar />);

    expect(markup).toContain("Couldn&#x27;t connect WSL (Ubuntu-20.04)");
    expect(markup).toContain("Could not find a Linux coddy-studio server binary for WSL.");
    expect(markup).toMatch(/data-mock="AlertDescription"[^>]*class="[^"]*wrap-anywhere[^"]*"/);
    expect(markup).not.toContain("Connecting WSL (Ubuntu-20.04)");
  });

  it("falls back to a generic error when a failed backend reports no error text", () => {
    baseScenario();
    h.state.environments = [
      environmentFixture({ environmentId: ENV_MAIN, label: "Main", connectionId: "primary" }),
      environmentFixture({
        environmentId: ENV_WSL,
        label: "WSL",
        connectionId: "local:wsl",
        displayUrl: "http://localhost:9001",
        phase: "error",
        error: null,
      }),
    ];
    h.state.desktopBootstraps = [{ label: "WSL", httpBaseUrl: "http://localhost:9001" }];
    const markup = render(<Sidebar />);
    expect(markup).toContain("The backend didn&#x27;t respond.");
  });

  it("attaches auto-animate to project and thread lists once per node", () => {
    baseScenario();
    render(<Sidebar />);
    const menus = captured("SidebarMenu").filter(
      (entry) => typeof entry.props["ref"] === "function",
    );
    expect(menus.length).toBeGreaterThan(0);
    const attach = menus[0]!.props["ref"] as (node: unknown) => void;
    const node = {};
    attach(node);
    attach(node);
    attach(null);
    expect(h.spies.autoAnimate).toHaveBeenCalledTimes(1);

    const subMenus = captured("SidebarMenuSub").filter(
      (entry) => typeof entry.props["ref"] === "function",
    );
    expect(subMenus.length).toBeGreaterThan(0);
    const attachThreads = subMenus[0]!.props["ref"] as (node: unknown) => void;
    const threadNode = {};
    attachThreads(threadNode);
    attachThreads(threadNode);
    expect(h.spies.autoAnimate).toHaveBeenCalledTimes(2);
  });

  it("opens worktree creation for the row clicked instead of the active project", () => {
    baseScenario();
    const projectB = makeProject("project-b", {
      title: "Repo B",
      workspaceRoot: "C:/repo-b",
    });
    h.state.projects = [projectA, projectB];
    render(<Sidebar />);

    const click = mouseEvent();
    invoke(
      mustFindProps(byAriaLabel("New worktree in Repo B"), "Repo B worktree action"),
      "onClick",
      click,
    );
    expect(click.preventDefault).toHaveBeenCalled();
    expect(click.stopPropagation).toHaveBeenCalled();
  });

  it("navigates to settings from the footer, closing the mobile sheet when needed", () => {
    baseScenario();
    h.state.sidebarCtx = { isMobile: true, setOpenMobile: h.spies.setOpenMobile };
    render(<Sidebar />);

    const footerButton = captured("SidebarMenuButton").find((entry) => {
      const elements: React.ReactElement[] = [];
      collectElements(entry.props["children"], elements);
      return elements.some(
        (element) => (element.props as { children?: unknown }).children === "Settings",
      );
    });
    expect(footerButton).toBeDefined();
    invoke(footerButton!.props, "onClick", mouseEvent());
    expect(h.spies.setOpenMobile).toHaveBeenCalledWith(false);
    expect(h.spies.navigate).toHaveBeenCalledWith({ to: "/settings" });
  });
});

staticDescribe("Sidebar environment scoping", () => {
  function seedTwoEnvironments() {
    h.state.environments = [
      environmentFixture({ environmentId: ENV_MAIN, label: "Local", primary: true }),
      environmentFixture({
        environmentId: ENV_REMOTE,
        label: "AI-SERVER",
        connectionId: "paired-1",
      }),
    ];
    h.state.primaryEnvironmentId = ENV_MAIN;
    h.state.projects = [
      makeProject("project-a"),
      makeProject("project-b", {
        environmentId: ENV_REMOTE,
        title: "Remote Repo",
        workspaceRoot: "/srv/remote-repo",
      }),
    ];
    h.state.threads = [
      makeThread("thread-local"),
      makeThread("thread-remote", {
        environmentId: ENV_REMOTE,
        projectId: ProjectId.make("project-b"),
      }),
    ];
  }

  it("filters a null selection to local environments", () => {
    seedTwoEnvironments();
    h.state.activeEnvironmentId = null;
    const markup = render(<Sidebar />);
    expect(markup).toContain("Repo A");
    expect(markup).not.toContain("Remote Repo");
  });

  it("filters projects and threads to the selected remote without lifecycle commands", () => {
    seedTwoEnvironments();
    h.state.activeEnvironmentId = ENV_REMOTE;
    h.state.commandCalls = [];
    const markup = render(<Sidebar />);
    expect(markup).toContain("Remote Repo");
    expect(markup).not.toContain("Repo A");
    expect(
      h.state.commandCalls.filter((call: { label?: string }) =>
        String(call.label ?? "").startsWith("environment."),
      ),
    ).toEqual([]);
  });

  it("keeps local environments merged when a local environment is selected", () => {
    seedTwoEnvironments();
    h.state.environments.push(
      environmentFixture({
        environmentId: ENV_WSL,
        label: "Ubuntu",
        connectionId: "local:wsl-ubuntu",
      }),
    );
    h.state.projects.push(
      makeProject("project-wsl", {
        environmentId: ENV_WSL,
        title: "WSL Repo",
        workspaceRoot: "/home/user/wsl-repo",
      }),
    );
    h.state.activeEnvironmentId = ENV_MAIN;
    const markup = render(<Sidebar />);
    expect(markup).toContain("Repo A");
    expect(markup).toContain("WSL Repo");
    expect(markup).not.toContain("Remote Repo");
  });

  it("drops the cloud from a saved server's project headers", () => {
    seedTwoEnvironments();
    h.state.activeEnvironmentId = ENV_REMOTE;
    const markup = render(<Sidebar />);
    expect(markup).toContain("Remote Repo");
    expect(markup).not.toContain('aria-label="Remote project"');
    expect(markup).not.toContain("lucide-cloud");
  });

  it("keeps the container badge on a WSL project's header", () => {
    seedTwoEnvironments();
    h.state.environments.push(
      environmentFixture({
        environmentId: ENV_WSL,
        label: "Ubuntu",
        connectionId: "local:wsl-ubuntu",
      }),
    );
    h.state.projects.push(
      makeProject("project-wsl", {
        environmentId: ENV_WSL,
        title: "WSL Repo",
        workspaceRoot: "/home/user/wsl-repo",
      }),
    );
    h.state.activeEnvironmentId = ENV_MAIN;
    const markup = render(<Sidebar />);
    expect(markup).toContain('aria-label="Local sandbox project"');
    expect(markup).toContain("lucide-container");
  });

  it("mounts the environment context card under the brand row", () => {
    seedTwoEnvironments();
    h.state.activeEnvironmentId = ENV_REMOTE;
    render(<Sidebar />);
    expect(captured("EnvironmentContextCard")).toHaveLength(1);
  });

  it("shows shared update progress in the card without observing its status query", () => {
    seedTwoEnvironments();
    h.state.environments[1] = environmentFixture({
      environmentId: ENV_REMOTE,
      label: "AI-SERVER",
      serverConfig: {
        environment: { serverVersion: "0.7.2", capabilities: { remoteUpdateControl: true } },
      },
    });
    h.state.activeEnvironmentId = ENV_REMOTE;
    const run = { phase: "restarting", targetVersion: "0.7.3" };
    h.state.remoteUpdateRuns.set(ENV_REMOTE, run);
    render(<Sidebar />);
    const badge = captured("EnvironmentContextCard")[0]!.props.updateBadge as React.ReactElement<{
      run: unknown;
      name: string;
    }>;
    expect(badge.props.run).toEqual(run);
    expect(badge.props.name).toBe("AI-SERVER");
    expect(h.state.remoteUpdateSnapshotQueries).not.toContain(ENV_REMOTE);
  });

  it("requests confirmation for the selected card's available update", () => {
    seedTwoEnvironments();
    h.state.environments[1] = environmentFixture({
      environmentId: ENV_REMOTE,
      label: "AI-SERVER",
      serverConfig: {
        environment: {
          serverVersion: "0.7.2",
          capabilities: { remoteUpdateControl: true, remoteUpdateProgress: true },
        },
      },
    });
    h.state.activeEnvironmentId = ENV_REMOTE;
    h.state.remoteUpdateSnapshots.set(ENV_REMOTE, {
      latestVersion: "0.7.3",
      state: "update-available",
      support: { installMode: "interactive" },
    });
    render(<Sidebar />);
    const badge = captured("EnvironmentContextCard")[0]!.props.updateBadge as React.ReactElement<{
      onUpdate: () => void;
    }>;
    badge.props.onUpdate();
    expect(h.state.requestRemoteUpdate).toHaveBeenCalledExactlyOnceWith({
      environmentId: ENV_REMOTE,
      name: "AI-SERVER",
      targetVersion: "0.7.3",
      progress: true,
    });
  });

  it("adds a project from the Repositories view without naming the hidden rail selection", () => {
    seedTwoEnvironments();
    h.state.activeEnvironmentId = ENV_REMOTE;
    h.uiStore.setState({ sidebarView: "repositories" });
    const markup = render(<Sidebar />);
    expect(markup).toContain('data-testid="sidebar-add-project-trigger"');
    expect(markup).not.toContain("Add project on");
    invoke(
      mustFindProps(byTestId("sidebar-add-project-trigger"), "add project"),
      "onClick",
      mouseEvent(),
    );
    expect(h.spies.openAddProject).toHaveBeenCalledTimes(1);
  });

  it('labels the add-project trigger "Add project on <name>" only for a remote selection', () => {
    seedTwoEnvironments();
    h.state.activeEnvironmentId = ENV_REMOTE;
    const remoteMarkup = render(<Sidebar />);
    expect(remoteMarkup).toContain("Add project on AI-SERVER");

    h.state.activeEnvironmentId = ENV_MAIN;
    const localMarkup = render(<Sidebar />);
    expect(localMarkup).not.toContain("Add project on");
  });
});

staticDescribe("Sidebar sort menu", () => {
  it("updates settings from the sort menus and clamps the preview count", () => {
    baseScenario();
    render(<Sidebar />);

    const radioGroups = captured("MenuRadioGroup");
    expect(radioGroups.length).toBe(3);

    (radioGroups[0]!.props["onValueChange"] as (value: string) => void)("created_at");
    expect(h.spies.updateSettings).toHaveBeenCalledWith({ sidebarProjectSortOrder: "created_at" });

    (radioGroups[1]!.props["onValueChange"] as (value: string) => void)("created_at");
    expect(h.spies.updateSettings).toHaveBeenCalledWith({ sidebarThreadSortOrder: "created_at" });

    (radioGroups[2]!.props["onValueChange"] as (value: string) => void)("separate");
    expect(h.spies.updateSettings).toHaveBeenCalledWith({ sidebarProjectGroupingMode: "separate" });
    (radioGroups[2]!.props["onValueChange"] as (value: string) => void)("bogus");

    const numberField = captured("NumberField")[0]!;
    const onValueChange = numberField.props["onValueChange"] as (value: number | null) => void;
    onValueChange(null);
    onValueChange(100);
    expect(h.spies.updateSettings).toHaveBeenCalledWith({ sidebarThreadPreviewCount: 15 });
    h.spies.updateSettings.mockClear();
    onValueChange(DEFAULT_CLIENT_SETTINGS.sidebarThreadPreviewCount);
    expect(h.spies.updateSettings).not.toHaveBeenCalled();

    const numberInput = captured("NumberFieldInput")[0]!;
    const keydown = keyboardEvent("a");
    invoke(numberInput.props, "onKeyDownCapture", keydown);
    expect(keydown.stopPropagation).toHaveBeenCalled();
  });
});

staticDescribe("Sidebar manual project sorting", () => {
  function manualScenario() {
    const projectB = makeProject("project-b", { title: "Repo B", workspaceRoot: "C:/repo-b" });
    h.state.projects = [projectA, projectB];
    h.state.threads = [threadDefault, threadIdle];
    h.state.environments = [environmentFixture({ environmentId: ENV_MAIN, label: "Main" })];
    h.state.clientSettings = { ...DEFAULT_CLIENT_SETTINGS, sidebarProjectSortOrder: "manual" };
    return { projectB };
  }

  it("renders inside a DndContext and reorders on drag end", () => {
    const { projectB } = manualScenario();
    const keyA = derivePhysicalProjectKey(projectA);
    const keyB = derivePhysicalProjectKey(projectB);
    render(<Sidebar />);

    const dnd = captured("DndContext")[0]!;
    expect(dnd).toBeDefined();

    const onDragStart = dnd.props["onDragStart"] as (event: unknown) => void;
    const onDragEnd = dnd.props["onDragEnd"] as (event: unknown) => void;
    const onDragCancel = dnd.props["onDragCancel"] as (event: unknown) => void;

    onDragStart({ active: { id: keyA } });
    onDragCancel({});

    // no target
    onDragEnd({ active: { id: keyA }, over: null });
    // same target
    onDragEnd({ active: { id: keyA }, over: { id: keyA } });
    // unknown project
    onDragEnd({ active: { id: "nope" }, over: { id: keyA } });
    expect(h.spies.reorderProjects).not.toHaveBeenCalled();

    // valid reorder
    onDragEnd({ active: { id: keyA }, over: { id: keyB } });
    expect(h.spies.reorderProjects).toHaveBeenCalledTimes(1);

    // collision detection prefers pointer hits, falls back to corners
    const collisionDetection = dnd.props["collisionDetection"] as (args: unknown) => unknown;
    h.spies.pointerWithin.mockReturnValueOnce([{ id: "x" }]);
    expect(collisionDetection({})).toEqual([{ id: "x" }]);
    h.spies.pointerWithin.mockReturnValueOnce([]);
    collisionDetection({});
    expect(h.spies.closestCorners).toHaveBeenCalled();
  });

  it("suppresses project header clicks around drags and context menus", () => {
    manualScenario();
    const keyA = derivePhysicalProjectKey(projectA);
    render(<Sidebar />);

    const dnd = captured("DndContext")[0]!;
    const onDragStart = dnd.props["onDragStart"] as (event: unknown) => void;
    const onDragEnd = dnd.props["onDragEnd"] as (event: unknown) => void;

    const header = captured("SidebarMenuButton").find(
      (entry) => typeof entry.props["onPointerDownCapture"] === "function",
    )!;
    expect(header).toBeDefined();
    expect(header.props["aria-expanded"]).toBe(true);

    // Drag in progress: click swallowed.
    onDragStart({ active: { id: keyA } });
    const duringDrag = mouseEvent();
    invoke(header.props, "onClick", duringDrag);
    expect(duringDrag.preventDefault).toHaveBeenCalled();
    expect(h.spies.setProjectExpanded).not.toHaveBeenCalled();

    // Drag finished: the trailing click is swallowed once.
    onDragEnd({ active: { id: keyA }, over: null });
    const afterDrag = mouseEvent();
    invoke(header.props, "onClick", afterDrag);
    expect(afterDrag.preventDefault).toHaveBeenCalled();

    // Subsequent click toggles expansion.
    invoke(header.props, "onClick", mouseEvent());
    expect(h.spies.setProjectExpanded).toHaveBeenCalledTimes(1);

    // Keyboard toggle.
    invoke(header.props, "onKeyDown", keyboardEvent("Enter"));
    expect(h.spies.setProjectExpanded).toHaveBeenCalledTimes(2);
    invoke(header.props, "onKeyDown", keyboardEvent("x"));
    expect(h.spies.setProjectExpanded).toHaveBeenCalledTimes(2);

    // Keyboard toggle suppressed during drag.
    onDragStart({ active: { id: keyA } });
    invoke(header.props, "onKeyDown", keyboardEvent(" "));
    expect(h.spies.setProjectExpanded).toHaveBeenCalledTimes(2);
    onDragEnd({ active: { id: keyA }, over: null });
    // Consume the post-drag click suppression left behind by drag start.
    invoke(header.props, "onClick", mouseEvent());

    // Pointer-down capture: context-menu-ish press stops propagation.
    const rightClick = mouseEvent({ button: 2 });
    invoke(header.props, "onPointerDownCapture", rightClick);
    expect(rightClick.stopPropagation).toHaveBeenCalled();
    const plainDown = mouseEvent({ button: 0 });
    invoke(header.props, "onPointerDownCapture", plainDown);
    expect(plainDown.stopPropagation).not.toHaveBeenCalled();

    // With a selection active, a plain click clears it before toggling.
    h.selectionStore.setState({ selectedThreadKeys: new Set(["some-key"]) });
    invoke(header.props, "onClick", mouseEvent());
    expect(h.spies.clearSelection).toHaveBeenCalled();
  });
});

staticDescribe("project header context menu", () => {
  function projectHeaderProps() {
    baseScenario();
    render(<Sidebar />);
    return captured("SidebarMenuButton").find(
      (entry) => typeof entry.props["onContextMenu"] === "function",
    )!.props;
  }

  it("does nothing when the local API is unavailable", async () => {
    const header = projectHeaderProps();
    h.state.localApi = null;
    invoke(header, "onContextMenu", mouseEvent());
    await flush();
    expect(h.spies.contextMenuShow).not.toHaveBeenCalled();
  });

  it("copies the project path", async () => {
    const header = projectHeaderProps();
    fakeLocalApi();
    h.spies.contextMenuShow.mockImplementation(async (items: Array<{ id: string }>) => {
      const copy = items.find((item) => item.id?.startsWith("copy-path:"));
      return copy!.id;
    });
    invoke(header, "onContextMenu", mouseEvent());
    await flush();
    expect(h.spies.copyToClipboard).toHaveBeenCalledWith("C:/repo-a", { path: "C:/repo-a" });
    expect(h.spies.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Path copied" }),
    );
  });

  it("opens archived threads from the project menu", async () => {
    const header = projectHeaderProps();
    fakeLocalApi();
    h.spies.contextMenuShow.mockImplementation(async (items: Array<{ id: string }>) => {
      return items.find((item) => item.id === "archive")!.id;
    });

    invoke(header, "onContextMenu", mouseEvent());
    await flush();

    expect(h.spies.routerNavigate).toHaveBeenCalledWith({ to: "/settings/archived" });
  });

  it("toggles hidden and shown discovered worktrees from the project menu", async () => {
    baseScenario();
    h.state.serverConfigs = new Map([
      [ENV_MAIN, { environment: { capabilities: { worktreeCatalog: true } } }],
    ]);
    fakeLocalApi();
    h.spies.contextMenuShow.mockImplementation(
      async (items: Array<{ id: string; label: string }>) => {
        const visibility = items.find((item) => item.id === "worktree-discovery-visibility");
        expect(visibility?.label).toBe("Show Hidden Worktrees");
        return visibility!.id;
      },
    );
    render(<Sidebar />);
    const hiddenHeader = captured("SidebarMenuButton").find(
      (entry) => typeof entry.props["onContextMenu"] === "function",
    )!.props;

    invoke(hiddenHeader, "onContextMenu", mouseEvent());
    await flush();

    expect(h.state.commandCalls).toContainEqual({
      label: "worktree.updatePolicy",
      input: {
        environmentId: ENV_MAIN,
        input: expect.objectContaining({
          projectId: ProjectId.make("project-a"),
          visibility: "shown",
        }),
      },
    });

    h.state.projects = [
      makeProject("project-a", {
        worktreeDiscovery: {
          visibility: "shown",
          initialPromptDismissedAt: null,
          baselinePaths: [],
        },
      }),
    ];
    h.state.commandCalls = [];
    h.state.captures.length = 0;
    h.spies.contextMenuShow.mockImplementation(
      async (items: Array<{ id: string; label: string }>) => {
        const visibility = items.find((item) => item.id === "worktree-discovery-visibility");
        expect(visibility?.label).toBe("Hide Discovered Worktrees");
        return visibility!.id;
      },
    );
    render(<Sidebar />);
    const shownHeader = captured("SidebarMenuButton").find(
      (entry) => typeof entry.props["onContextMenu"] === "function",
    )!.props;

    invoke(shownHeader, "onContextMenu", mouseEvent());
    await flush();

    expect(h.state.commandCalls).toContainEqual({
      label: "worktree.updatePolicy",
      input: {
        environmentId: ENV_MAIN,
        input: expect.objectContaining({
          projectId: ProjectId.make("project-a"),
          visibility: "hidden",
        }),
      },
    });
  });

  it("opens the rename and grouping dialogs from the menu", async () => {
    const header = projectHeaderProps();
    fakeLocalApi();
    h.spies.contextMenuShow.mockImplementationOnce(async (items: Array<{ id: string }>) => {
      return items.find((item) => item.id?.startsWith("rename:"))!.id;
    });
    invoke(header, "onContextMenu", mouseEvent());
    await flush();

    h.spies.contextMenuShow.mockImplementationOnce(async (items: Array<{ id: string }>) => {
      return items.find((item) => item.id?.startsWith("grouping:"))!.id;
    });
    invoke(header, "onContextMenu", mouseEvent());
    await flush();
    expect(h.spies.contextMenuShow).toHaveBeenCalledTimes(2);
  });

  it("removes an empty project after confirmation", async () => {
    h.state.projects = [projectA];
    h.state.threads = [];
    h.state.environments = [environmentFixture({ environmentId: ENV_MAIN, label: "Main" })];
    render(<Sidebar />);
    const header = captured("SidebarMenuButton").find(
      (entry) => typeof entry.props["onContextMenu"] === "function",
    )!.props;

    fakeLocalApi();
    h.spies.getDraftThreadByProjectRef.mockReturnValue({ draftId: "draft-1" });
    h.spies.contextMenuShow.mockImplementation(async (items: Array<{ id: string }>) => {
      return items.find((item) => item.id?.startsWith("delete:"))!.id;
    });
    invoke(header, "onContextMenu", mouseEvent());
    await flush();

    expect(h.spies.dialogConfirm).toHaveBeenCalled();
    expect(h.state.commandCalls.map((call: { label: string }) => call.label)).toContain(
      "project.delete",
    );
    expect(h.spies.clearDraftThread).toHaveBeenCalledWith("draft-1");
    expect(h.spies.clearProjectDraftThreadId).toHaveBeenCalled();
  });

  it("aborts project removal when the confirmation is declined", async () => {
    h.state.projects = [projectA];
    h.state.threads = [];
    h.state.environments = [environmentFixture({ environmentId: ENV_MAIN, label: "Main" })];
    render(<Sidebar />);
    const header = captured("SidebarMenuButton").find(
      (entry) => typeof entry.props["onContextMenu"] === "function",
    )!.props;

    fakeLocalApi();
    h.spies.dialogConfirm.mockResolvedValue(false);
    h.spies.contextMenuShow.mockImplementation(async (items: Array<{ id: string }>) => {
      return items.find((item) => item.id?.startsWith("delete:"))!.id;
    });
    invoke(header, "onContextMenu", mouseEvent());
    await flush();
    expect(h.state.commandCalls.map((call: { label: string }) => call.label)).not.toContain(
      "project.delete",
    );
  });

  it("toasts when project removal fails", async () => {
    h.state.projects = [projectA];
    h.state.threads = [];
    h.state.environments = [environmentFixture({ environmentId: ENV_MAIN, label: "Main" })];
    render(<Sidebar />);
    const header = captured("SidebarMenuButton").find(
      (entry) => typeof entry.props["onContextMenu"] === "function",
    )!.props;

    fakeLocalApi();
    h.state.commandResults["project.delete"] = () => failureResult("delete blew up");
    h.spies.contextMenuShow.mockImplementation(async (items: Array<{ id: string }>) => {
      return items.find((item) => item.id?.startsWith("delete:"))!.id;
    });
    invoke(header, "onContextMenu", mouseEvent());
    await flush();
    expect(h.spies.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({
        title: 'Failed to remove "Repo A"',
        description: "delete blew up",
      }),
    );
  });

  it("warns before force-removing a project with threads and honors 'Delete anyway'", async () => {
    const header = projectHeaderProps();
    fakeLocalApi();
    h.spies.contextMenuShow.mockImplementation(async (items: Array<{ id: string }>) => {
      return items.find((item) => item.id?.startsWith("delete:"))!.id;
    });
    invoke(header, "onContextMenu", mouseEvent());
    await flush();

    const warningToast = h.spies.toastAdd.mock.calls
      .map((call) => call[0] as { title?: string; actionProps?: { onClick?: () => void } })
      .find((toast) => toast.title === "Project is not empty");
    expect(warningToast).toBeDefined();
    warningToast!.actionProps!.onClick!();
    await flush();

    expect(h.spies.toastClose).toHaveBeenCalledWith("toast-1");
    expect(h.spies.dialogConfirm).toHaveBeenCalledWith(expect.stringContaining("Remove project"));
    expect(h.state.commandCalls.map((call: { label: string }) => call.label)).toContain(
      "project.delete",
    );
  });

  it("opens the same menu from the ⋯ button, anchored below it", async () => {
    baseScenario();
    render(<Sidebar />);
    fakeLocalApi();
    const actions = mustFindProps(byTestId("project-actions-button"), "project actions button");
    const click = mouseEvent({
      currentTarget: {
        getBoundingClientRect: () => ({ left: 300.4, top: 96, right: 324, bottom: 120.6 }),
      },
    });
    invoke(actions, "onClick", click);
    await flush();

    expect(click.preventDefault).toHaveBeenCalled();
    expect(click.stopPropagation).toHaveBeenCalled();
    const [items, position] = h.spies.contextMenuShow.mock.calls[0]!;
    expect(position).toEqual({ x: 300, y: 121 });
    expect(
      (items as Array<{ label?: string; separator?: true }>).map((entry) =>
        entry.separator ? "---" : entry.label,
      ),
    ).toEqual([
      "New Worktree…",
      "Import CLI sessions…",
      "---",
      "Rename…",
      "Group into…",
      "Copy Path",
      "---",
      "Archived Threads",
      "---",
      "Remove Project…",
    ]);
  });

  it("toasts an import and opens the newest imported thread", async () => {
    baseScenario();
    render(<Sidebar />);

    const dialog = captured("ImportCliSessionsDialog").at(-1)!;
    expect(dialog.props).toMatchObject({ open: false, target: null });
    const target = {
      environmentId: projectA.environmentId,
      projectId: projectA.id,
      workspaceRoot: projectA.workspaceRoot,
    };
    const onImported = dialog.props.onImported as (
      target: unknown,
      summary: { type: string; title: string; description?: string; newestThreadId: string | null },
    ) => void;
    onImported(target, {
      type: "warning",
      title: "Imported 2 sessions",
      description: "1 session skipped: Already imported.",
      newestThreadId: "import:codex:session-one",
    });
    await flush();

    expect(h.spies.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "warning",
        title: "Imported 2 sessions",
        description: "1 session skipped: Already imported.",
      }),
    );
    expect(h.spies.routerNavigate).toHaveBeenCalledWith({
      to: "/$environmentId/$threadId",
      params: { environmentId: projectA.environmentId, threadId: "import:codex:session-one" },
    });

    h.spies.routerNavigate.mockClear();
    onImported(target, { type: "error", title: "No sessions imported", newestThreadId: null });
    await flush();
    expect(h.spies.routerNavigate).not.toHaveBeenCalled();
  });
});

staticDescribe("worktree discovery integration", () => {
  function discoveredCatalog(path: string, branch: string) {
    return {
      repositoryKey: WorktreeRepositoryKey.make(`repository:${path}`),
      generation: 42,
      authoritative: true,
      observedAt: "2026-08-09T12:00:00.000Z",
      scanStatus: { _tag: "ready" },
      worktrees: [
        {
          worktreeKey: WorktreeKey.make(`worktree:${path}`),
          path,
          branch,
          head: "abcdef0123456789",
          isPrimary: false,
          isBare: false,
          locked: false,
          registrationState: "registered",
          directoryState: "present",
          adoptionState: "none",
          eligibleForAdoption: true,
        },
      ],
      adoptedWorkspaces: [],
    };
  }

  it("subscribes only while the existing child panel is rendered and places discovery before primary", () => {
    baseScenario();
    h.state.routeParams = {};
    h.state.serverConfigs = new Map([
      [ENV_MAIN, { environment: { capabilities: { worktreeCatalog: true } } }],
    ]);
    h.state.worktreeCatalogs.set(
      `${ENV_MAIN}:${projectA.id}`,
      discoveredCatalog("C:/worktrees/discovered", "feature/discovered"),
    );
    h.uiStore.setState({
      projectExpandedById: { [derivePhysicalProjectKey(projectA)]: false },
    });

    const collapsedMarkup = render(<Sidebar />);
    expect(collapsedMarkup).not.toContain("Discovered worktrees");
    expect(collapsedMarkup).not.toContain("feature/discovered");
    expect(h.state.discoveryCatalogSubscriptions).toEqual([]);
    expect(h.state.discoveryFocusRefreshCalls).toEqual([]);

    h.state.discoveryCatalogSubscriptions = [];
    h.state.discoveryFocusRefreshCalls = [];
    h.uiStore.setState({
      projectExpandedById: { [derivePhysicalProjectKey(projectA)]: true },
    });
    const expandedMarkup = render(<Sidebar />);
    const discoveryIndex = expandedMarkup.indexOf("Discovered worktrees");
    const primaryIndex = expandedMarkup.indexOf(">primary<");

    expect(discoveryIndex).toBeGreaterThan(-1);
    expect(primaryIndex).toBeGreaterThan(discoveryIndex);
    expect(h.state.discoveryCatalogSubscriptions).toEqual([
      { environmentId: ENV_MAIN, input: { projectId: projectA.id } },
      { environmentId: ENV_MAIN, input: { projectId: projectA.id } },
    ]);
    expect(h.state.discoveryFocusRefreshCalls).toEqual([
      [{ environmentId: ENV_MAIN, projectId: projectA.id }],
    ]);
    expect(
      captured("SidebarMenuSubItem").some(
        (entry) => entry.props["data-testid"] === "thread-row-thread-idle",
      ),
    ).toBe(true);

    h.state.discoveryCatalogSubscriptions = [];
    h.state.discoveryFocusRefreshCalls = [];
    h.uiStore.setState({
      projectExpandedById: { [derivePhysicalProjectKey(projectA)]: false },
    });
    const recollapsedMarkup = render(<Sidebar />);
    expect(recollapsedMarkup).not.toContain("Discovered worktrees");
    expect(h.state.discoveryCatalogSubscriptions).toEqual([]);
    expect(h.state.discoveryFocusRefreshCalls).toEqual([]);
  });

  it("keeps grouped mixed-capability discovery at the supported physical boundary", () => {
    groupedScenario();
    h.state.routeParams = {};
    const remoteProjectId = ProjectId.make("project-a-remote");
    h.state.serverConfigs = new Map([
      [ENV_MAIN, { environment: { capabilities: { worktreeCatalog: true } } }],
      [ENV_REMOTE, { environment: { capabilities: { worktreeCatalog: false } } }],
    ]);
    h.state.worktreeCatalogs.set(
      `${ENV_MAIN}:${projectA.id}`,
      discoveredCatalog("C:/local/discovered", "feature/local"),
    );
    h.state.worktreeCatalogs.set(
      `${ENV_REMOTE}:${remoteProjectId}`,
      discoveredCatalog("C:/remote/discovered", "feature/remote"),
    );

    const markup = render(<Sidebar />);

    expect(markup).toContain("feature/local");
    expect(markup).not.toContain("feature/remote");
    expect(h.state.discoveryCatalogSubscriptions).toEqual([
      { environmentId: ENV_MAIN, input: { projectId: projectA.id } },
      { environmentId: ENV_MAIN, input: { projectId: projectA.id } },
    ]);
    expect(h.state.discoveryFocusRefreshCalls).toEqual([
      [{ environmentId: ENV_MAIN, projectId: projectA.id }],
    ]);
  });

  it("keeps a missing adopted row selectable while disabling workspace actions and exposing recovery", async () => {
    baseScenario();
    h.state.serverConfigs = new Map([
      [ENV_MAIN, { environment: { capabilities: { worktreeCatalog: true } } }],
    ]);
    h.state.worktreeCatalogs.set(`${ENV_MAIN}:${projectA.id}`, {
      repositoryKey: WorktreeRepositoryKey.make("repository:repo-a"),
      generation: 43,
      authoritative: true,
      observedAt: "2026-08-09T12:01:00.000Z",
      scanStatus: { _tag: "ready" },
      worktrees: [],
      adoptedWorkspaces: [
        {
          threadId: threadActive.id,
          worktreeKey: WorktreeKey.make("worktree:missing"),
          path: "C:/wt/x",
          branch: "feature/x",
          availability: "missing-registered",
          registrationState: "prunable",
          locked: false,
        },
      ],
    });

    const markup = render(<Sidebar />);
    expect(markup).toContain("availability-warning-thread-active");
    expect(markup).toContain("C:/wt/x");
    const row = mustFindProps(byTestId("thread-row-thread-active"), "missing row");
    invoke(row, "onClick", mouseEvent());
    expect(h.spies.routerNavigate).toHaveBeenCalledWith(
      expect.objectContaining({ to: "/$environmentId/$threadId" }),
    );

    fakeLocalApi();
    let menuItems: Array<{
      id: string;
      disabled?: boolean;
      description?: string;
      children?: unknown[];
    }> = [];
    h.spies.contextMenuShow.mockImplementation(async (items) => {
      menuItems = items;
      return null;
    });
    invoke(row, "onContextMenu", mouseEvent());
    await flush();
    const missingReason =
      "The worktree directory is missing. Use Retry detection or Remove from Coddy Studio on its card.";
    expect(menuItems.find((item) => item.id === "pull")).toMatchObject({
      disabled: true,
      description: missingReason,
    });
    expect(menuItems.find((item) => item.id === "open-in")).toMatchObject({
      disabled: true,
      description: missingReason,
    });

    const warning = captured("WorktreeAvailabilityWarning").find(
      (entry) => (entry.props.status as { threadId: string }).threadId === threadActive.id,
    );
    expect(warning).toBeDefined();
    invoke(warning!.props, "onRetry", undefined);
    expect(h.state.commandCalls).toContainEqual({
      label: "worktree.refresh",
      input: { environmentId: ENV_MAIN, input: { projectId: projectA.id } },
    });
    invoke(warning!.props, "onRemove", undefined);
    expect(h.spies.requestWorktreeRemoval).toHaveBeenCalledWith(
      expect.objectContaining({
        threadId: threadActive.id,
        availability: "missing-registered",
        path: "C:/wt/x",
      }),
    );
  });

  it("keeps retained verification-unavailable rows usable for workspace actions", async () => {
    baseScenario();
    h.state.serverConfigs = new Map([
      [
        ENV_MAIN,
        {
          availableEditors: ["vscode"],
          environment: { capabilities: { worktreeCatalog: true } },
        },
      ],
    ]);
    h.state.worktreeCatalogs.set(`${ENV_MAIN}:${projectA.id}`, {
      repositoryKey: WorktreeRepositoryKey.make("repository:repo-a"),
      generation: 44,
      authoritative: false,
      observedAt: "2026-08-09T12:02:00.000Z",
      scanStatus: {
        _tag: "degraded",
        reason: "git-failed",
        message: "verification is temporarily unavailable",
        failedAt: "2026-08-09T12:02:00.000Z",
        lastAuthoritativeAt: "2026-08-09T12:01:00.000Z",
      },
      worktrees: [],
      adoptedWorkspaces: [
        {
          threadId: threadActive.id,
          worktreeKey: WorktreeKey.make("worktree:retained"),
          path: "C:/wt/x",
          branch: "feature/x",
          availability: "verification-unavailable",
          registrationState: "registered",
          locked: false,
        },
      ],
    });

    render(<Sidebar />);
    fakeLocalApi();
    let menuItems: Array<{ id: string; disabled?: boolean; children?: unknown[] }> = [];
    h.spies.contextMenuShow.mockImplementation(async (items) => {
      menuItems = items;
      return null;
    });
    const row = mustFindProps(byTestId("thread-row-thread-active"), "retained degraded row");
    invoke(row, "onContextMenu", mouseEvent());
    await flush();

    expect(menuItems.some((item) => item.id === "pull")).toBe(true);
    expect(menuItems.find((item) => item.id === "pull")?.disabled).not.toBe(true);
    expect(menuItems.find((item) => item.id === "open-in")?.disabled).not.toBe(true);
  });

  it("starts no row subscription and uses direct legacy detach when capability is false", async () => {
    baseScenario();
    setWorktreeSessionStatus("ready");
    h.state.serverConfigs = new Map([
      [ENV_MAIN, { environment: { capabilities: { worktreeCatalog: false } } }],
    ]);

    render(<Sidebar />);
    expect(h.state.discoveryCatalogSubscriptions).toEqual([]);
    fakeLocalApi();
    h.spies.contextMenuShow.mockResolvedValue("delete");
    const row = mustFindProps(byTestId("thread-row-thread-active"), "legacy worktree row");
    invoke(row, "onContextMenu", mouseEvent());
    await flush();

    expect(h.spies.deleteThread).toHaveBeenCalledWith({
      environmentId: ENV_MAIN,
      threadId: threadActive.id,
    });
    expect(h.spies.requestWorktreeRemoval).not.toHaveBeenCalled();
  });

  it("holds legacy detach too while the worktree's session runs", async () => {
    baseScenario();
    h.state.serverConfigs = new Map([
      [ENV_MAIN, { environment: { capabilities: { worktreeCatalog: false } } }],
    ]);

    render(<Sidebar />);
    fakeLocalApi();
    const deleteItem = chooseDeleteFromCardMenu();
    const row = mustFindProps(byTestId("thread-row-thread-active"), "legacy worktree row");
    invoke(row, "onContextMenu", mouseEvent());
    await flush();

    expect(deleteItem.current).toMatchObject({
      disabled: true,
      description: "Stop the running session before deleting this worktree.",
    });
    expect(h.spies.dialogConfirm).not.toHaveBeenCalled();
    expect(h.spies.deleteThread).not.toHaveBeenCalled();
  });

  it("keeps false-capability bulk deletion on detach-only thread actions", async () => {
    baseScenario();
    h.state.serverConfigs = new Map([
      [ENV_MAIN, { environment: { capabilities: { worktreeCatalog: false } } }],
    ]);
    h.selectionStore.setState({
      selectedThreadKeys: new Set([threadKeyOf(threadIdle), threadKeyOf(threadActive)]),
    });

    render(<Sidebar />);
    expect(h.state.discoveryCatalogSubscriptions).toEqual([]);
    fakeLocalApi();
    h.spies.contextMenuShow.mockResolvedValue("delete");
    const row = mustFindProps(byTestId("thread-row-thread-idle"), "legacy bulk row");
    invoke(row, "onContextMenu", mouseEvent());
    await flush();

    expect(h.spies.deleteThread).toHaveBeenCalledTimes(2);
    expect(h.spies.requestWorktreeRemoval).not.toHaveBeenCalled();
  });
});

staticDescribe("worktree removal completion owner", () => {
  const removedTarget = {
    environmentId: ENV_MAIN,
    projectId: projectA.id,
    threadId: threadActive.id,
    title: threadActive.title,
    path: "C:/wt/x",
    branch: "feat/x",
    availability: "present",
    registrationState: "registered",
    locked: false,
  } as const;
  const removedResult = {
    threadRemoved: true,
    gitOutcome: "removed",
    orphanCleanupPending: false,
  } as const;

  async function reportRemoval(): Promise<void> {
    baseScenario();
    render(<Sidebar />);
    const dialog = captured("WorktreeRemovalDialog").at(-1);
    expect(dialog).toBeDefined();
    const onRemoved = dialog!.props.onRemoved as (
      target: typeof removedTarget,
      result: typeof removedResult,
    ) => void;
    onRemoved(removedTarget, removedResult);
    await flush();
  }

  it("surfaces fallback-navigation failure exactly once", async () => {
    h.spies.completeWorktreeRemoval.mockResolvedValueOnce(failureResult("route unavailable"));

    await reportRemoval();

    expect(h.spies.toastAdd).toHaveBeenCalledTimes(1);
    expect(h.spies.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "error",
        title: "Worktree removed, but navigation failed",
        description: "route unavailable",
      }),
    );
  });

  it("keeps successful and interrupted cleanup silent", async () => {
    await reportRemoval();
    expect(h.spies.toastAdd).not.toHaveBeenCalled();

    vi.clearAllMocks();
    h.spies.completeWorktreeRemoval.mockResolvedValueOnce({
      _tag: "Failure",
      cause: Cause.interrupt(1),
    });
    await reportRemoval();
    expect(h.spies.toastAdd).not.toHaveBeenCalled();
  });
});

staticDescribe("thread rows in the full sidebar", () => {
  function renderedRow(threadId: string) {
    return mustFindProps(byTestId(`thread-row-${threadId}`), `row ${threadId}`);
  }

  it("navigates on plain click and clears an existing selection", () => {
    baseScenario();
    render(<Sidebar />);
    const row = renderedRow("thread-idle");

    h.selectionStore.setState({ selectedThreadKeys: new Set(["other"]) });
    invoke(row, "onClick", mouseEvent());
    expect(h.spies.clearSelection).toHaveBeenCalled();
    expect(h.spies.setAnchor).toHaveBeenCalledWith(threadKeyOf(threadIdle));
    expect(h.spies.routerNavigate).toHaveBeenCalledWith(
      expect.objectContaining({ to: "/$environmentId/$threadId" }),
    );
    expect(h.spies.markRead).toHaveBeenCalledWith(threadKeyOf(threadIdle));
  });

  it("toggles selection on modifier-click and range-selects on shift-click", () => {
    baseScenario();
    render(<Sidebar />);
    const row = renderedRow("thread-idle");

    invoke(row, "onClick", mouseEvent({ metaKey: true, ctrlKey: true }));
    expect(h.spies.toggleThread).toHaveBeenCalledWith(threadKeyOf(threadIdle));

    invoke(row, "onClick", mouseEvent({ shiftKey: true }));
    expect(h.spies.rangeSelectTo).toHaveBeenCalled();

    // Trailing double click does not navigate.
    h.spies.routerNavigate.mockClear();
    invoke(row, "onClick", mouseEvent({ detail: 2 }));
    expect(h.spies.routerNavigate).not.toHaveBeenCalled();
  });

  it("activates from the card button and opens exactly one menu with Shift+F10", async () => {
    baseScenario();
    render(<Sidebar />);
    fakeLocalApi();
    // Enter and Space on the card's <button> dispatch a click with detail 0.
    invoke(renderedRow("thread-idle"), "onClick", mouseEvent({ detail: 0 }));
    expect(h.spies.routerNavigate).toHaveBeenCalled();

    const button = mustFindProps(byTestId("thread-card-button-thread-idle"), "card button");
    const enter = keyboardEvent("Enter");
    invoke(button, "onKeyDown", enter);
    expect(enter.preventDefault).not.toHaveBeenCalled();

    const shiftF10 = keyboardEvent("F10", {
      shiftKey: true,
      ctrlKey: false,
      altKey: false,
      metaKey: false,
      currentTarget: {
        getBoundingClientRect: () => ({ left: 40.4, top: 100, right: 380, bottom: 152.6 }),
      },
    });
    invoke(button, "onKeyDown", shiftF10);
    expect(shiftF10.preventDefault).toHaveBeenCalled();
    // Chromium and WebView2 follow the key with a contextmenu event; it must not open a second menu.
    invoke(renderedRow("thread-idle"), "onContextMenu", mouseEvent());
    await flush();
    expect(h.spies.contextMenuShow).toHaveBeenCalledTimes(1);
    expect(h.spies.contextMenuShow.mock.calls[0]![1]).toEqual({ x: 40, y: 153 });
  });

  it("keeps the pointer position for a right-click after the keyboard echo window", async () => {
    baseScenario();
    render(<Sidebar />);
    fakeLocalApi();
    const now = vi.spyOn(performance, "now").mockReturnValue(10_000);
    try {
      const button = mustFindProps(byTestId("thread-card-button-thread-idle"), "card button");
      invoke(
        button,
        "onKeyDown",
        keyboardEvent("ContextMenu", {
          shiftKey: false,
          ctrlKey: false,
          altKey: false,
          metaKey: false,
          currentTarget: {
            getBoundingClientRect: () => ({ left: 8, top: 0, right: 300, bottom: 60 }),
          },
        }),
      );
      now.mockReturnValue(11_500);
      invoke(
        renderedRow("thread-idle"),
        "onContextMenu",
        mouseEvent({ clientX: 120, clientY: 90 }),
      );
      await flush();
      expect(h.spies.contextMenuShow).toHaveBeenCalledTimes(2);
      expect(h.spies.contextMenuShow.mock.calls[0]![1]).toEqual({ x: 8, y: 60 });
      expect(h.spies.contextMenuShow.mock.calls[1]![1]).toEqual({ x: 120, y: 90 });
    } finally {
      now.mockRestore();
    }
  });

  it("archives immediately when confirmation is disabled", async () => {
    baseScenario();
    const markup = render(<Sidebar />);
    // The captured-tree walk also sees unrendered slot props; prove the control renders.
    expect(markup).toContain('data-testid="thread-archive-thread-idle"');
    const archive = mustFindProps(byTestId("thread-archive-thread-idle"), "archive button");
    const pointer = mouseEvent();
    invoke(archive, "onPointerDown", pointer);
    expect(pointer.stopPropagation).toHaveBeenCalled();
    invoke(archive, "onClick", mouseEvent());
    await flush();
    expect(h.spies.archiveThread).toHaveBeenCalled();
  });

  it("toasts when archiving fails", async () => {
    baseScenario();
    h.spies.archiveThread.mockResolvedValue(failureResult("archive nope"));
    const markup = render(<Sidebar />);
    // The captured-tree walk also sees unrendered slot props; prove the control renders.
    expect(markup).toContain('data-testid="thread-archive-thread-idle"');
    const archive = mustFindProps(byTestId("thread-archive-thread-idle"), "archive button");
    invoke(archive, "onClick", mouseEvent());
    await flush();
    expect(h.spies.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Failed to archive thread", description: "archive nope" }),
    );
  });

  it("enters archive-confirmation mode when the setting is enabled", () => {
    baseScenario();
    h.state.clientSettings = { ...DEFAULT_CLIENT_SETTINGS, confirmThreadArchive: true };
    const markup = render(<Sidebar />);
    // The captured-tree walk also sees unrendered slot props; prove the control renders.
    expect(markup).toContain('data-testid="thread-archive-thread-idle"');
    const archive = mustFindProps(byTestId("thread-archive-thread-idle"), "archive button");
    invoke(archive, "onClick", mouseEvent());
    expect(h.spies.archiveThread).not.toHaveBeenCalled();
  });

  it("opens the PR link from the status indicator", () => {
    baseScenario();
    render(<Sidebar />);
    const prButton = mustFindProps(byAriaLabel("PR open"), "pr button");
    invoke(prButton, "onClick", mouseEvent());
    expect(h.spies.openPrLink).toHaveBeenCalledWith(expect.anything(), "https://example.com/pr/1");
  });

  it("opens a discovered port preview and toasts failures", async () => {
    baseScenario();
    h.state.discoveredPortsByThreadId = {
      "thread-idle": [{ port: 3000 }, { port: 4000 }],
    };
    const markup = render(<Sidebar />);
    expect(markup).toContain("Open localhost:3000");
    expect(markup).toContain("(+1)");

    const portButton = mustFindProps(byAriaLabel("Open localhost:3000"), "port button");
    invoke(portButton, "onClick", mouseEvent());
    await flush();
    expect(h.spies.openDiscoveredPort).toHaveBeenCalled();
    expect(h.spies.routerNavigate).toHaveBeenCalled();

    h.state.openDiscoveredPortResult = failureResult("preview broke");
    invoke(portButton, "onClick", mouseEvent());
    await flush();
    expect(h.spies.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Unable to open preview", description: "preview broke" }),
    );
  });

  it("uses a generic message for opaque preview failures", async () => {
    baseScenario();
    h.state.discoveredPortsByThreadId = {
      [threadIdle.id]: [{ port: 5733, protocol: "http", label: "Preview" }],
    };
    h.state.openDiscoveredPortResult = {
      _tag: "Failure",
      cause: Cause.fail("opaque preview failure"),
    };
    render(<Sidebar />);
    const preview = mustFindProps(byAriaLabel("Open localhost:5733"), "preview button");
    invoke(preview, "onClick", mouseEvent());
    await flush();
    expect(h.spies.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ description: "The preview could not be opened." }),
    );
  });
});

staticDescribe("thread context menu", () => {
  /** Opens a card's menu and returns the items it was built with, choosing nothing. */
  async function openCardMenu(rowTestId: string): Promise<CardMenuItem[]> {
    let menuItems: CardMenuItem[] = [];
    h.spies.contextMenuShow.mockImplementation(async (items: CardMenuItem[]) => {
      menuItems = items;
      return null;
    });
    invoke(mustFindProps(byTestId(rowTestId), rowTestId), "onContextMenu", mouseEvent());
    await flush();
    return menuItems;
  }

  function setupMenu(clickedId: string | null) {
    baseScenario();
    h.state.serverConfigs.set(ENV_MAIN, {
      availableEditors: ["vscode"],
      environment: { capabilities: { worktreeCatalog: true } },
    });
    render(<Sidebar />);
    fakeLocalApi();
    if (clickedId !== null) {
      h.spies.contextMenuShow.mockResolvedValue(clickedId);
    }
    return mustFindProps(byTestId("thread-row-thread-idle"), "idle row");
  }

  it("skips entirely without a local API", async () => {
    baseScenario();
    render(<Sidebar />);
    h.state.localApi = null;
    const row = mustFindProps(byTestId("thread-row-thread-idle"), "idle row");
    invoke(row, "onContextMenu", mouseEvent());
    await flush();
    expect(h.spies.contextMenuShow).not.toHaveBeenCalled();
  });

  it("runs vcs pull for 'Pull' and refreshes the status", async () => {
    const row = setupMenu("pull");
    invoke(row, "onContextMenu", mouseEvent());
    await flush();
    const labels = h.state.commandCalls.map((call: { label: string }) => call.label);
    expect(labels).toContain("vcs.pull");
    expect(labels).toContain("vcs.refreshStatus");
  });

  it("toasts when 'Pull' fails", async () => {
    const row = setupMenu("pull");
    h.state.commandResults["vcs.pull"] = () => failureResult("pull failed");
    invoke(row, "onContextMenu", mouseEvent());
    await flush();
    expect(h.spies.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Failed to pull", description: "pull failed" }),
    );
  });

  it("opens the workspace in an available editor", async () => {
    const row = setupMenu("open-in:vscode");
    invoke(row, "onContextMenu", mouseEvent());
    await flush();
    const openCall = h.state.commandCalls.find(
      (call: { label: string }) => call.label === "shell.openInEditor",
    );
    expect(openCall).toBeDefined();
    expect(openCall.input.input.editor).toBe("vscode");
  });

  it("opens a local worktree path in File Explorer", async () => {
    baseScenario();
    h.state.serverConfigs.set(ENV_MAIN, {
      availableEditors: ["vscode"],
      environment: { capabilities: { worktreeCatalog: true } },
    });
    const openInFileManager = vi.fn(async () => {});
    (globalThis.window as unknown as Record<string, unknown>)["desktopBridge"] = {
      openInFileManager,
    };
    render(<Sidebar />);
    fakeLocalApi();
    let openInChildren: Array<{ id: string }> = [];
    h.spies.contextMenuShow.mockImplementation(
      async (items: Array<{ id: string; children?: Array<{ id: string }> }>) => {
        openInChildren = items.find((item) => item.id === "open-in")?.children ?? [];
        return "open-in:file-explorer";
      },
    );
    const row = mustFindProps(byTestId("thread-row-thread-active"), "active worktree row");

    invoke(row, "onContextMenu", mouseEvent());
    await flush();

    expect(openInChildren.map((item) => item.id)).toEqual([
      "open-in:file-explorer",
      "open-in:vscode",
    ]);
    expect(openInFileManager).toHaveBeenCalledWith("C:/wt/x", true);
  });

  it.each([
    [new Error("finder unavailable"), "finder unavailable"],
    ["opaque failure", "An unexpected error occurred."],
  ])("reports File Explorer launch failures", async (rejection, description) => {
    baseScenario();
    const openInFileManager = vi.fn(async () => Promise.reject(rejection));
    (globalThis.window as unknown as Record<string, unknown>)["desktopBridge"] = {
      openInFileManager,
    };
    render(<Sidebar />);
    fakeLocalApi();
    h.spies.contextMenuShow.mockResolvedValue("open-in:file-explorer");
    const row = mustFindProps(byTestId("thread-row-thread-active"), "active worktree row");

    invoke(row, "onContextMenu", mouseEvent());
    await flush();

    expect(h.spies.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "error",
        title: "Unable to open File Explorer",
        description,
      }),
    );
  });

  it("omits File Explorer for a remote row", async () => {
    const { remoteThread } = groupedScenario();
    h.state.serverConfigs.set(ENV_REMOTE, {
      availableEditors: ["vscode"],
      environment: { capabilities: {} },
    });
    const openInFileManager = vi.fn(async () => {});
    (globalThis.window as unknown as Record<string, unknown>)["desktopBridge"] = {
      openInFileManager,
    };
    render(<Sidebar />);
    fakeLocalApi();
    let menuItems: Array<{ id: string; children?: Array<{ id: string }> }> = [];
    h.spies.contextMenuShow.mockImplementation(
      async (items: Array<{ id: string; children?: Array<{ id: string }> }>) => {
        menuItems = items;
        return null;
      },
    );
    const row = mustFindProps(byTestId(`thread-row-${remoteThread.id}`), "remote row");

    invoke(row, "onContextMenu", mouseEvent());
    await flush();

    const openIn = menuItems.find((item) => item.id === "open-in");
    expect(openIn?.children?.some((item) => item.id === "open-in:file-explorer")).toBe(false);
    expect(openInFileManager).not.toHaveBeenCalled();
  });

  it("builds Open in from the row's own environment editors", async () => {
    const { remoteThread } = groupedScenario();
    h.state.serverConfigs = new Map([
      [ENV_MAIN, { environment: { capabilities: {} }, availableEditors: ["vscode"] }],
      [ENV_REMOTE, { environment: { capabilities: {} }, availableEditors: ["cursor"] }],
    ]);
    render(<Sidebar />);
    fakeLocalApi();
    let menuItems: Array<{ id: string; children?: Array<{ id: string }> }> = [];
    h.spies.contextMenuShow.mockImplementation(
      async (items: Array<{ id: string; children?: Array<{ id: string }> }>) => {
        menuItems = items;
        return null;
      },
    );
    const row = mustFindProps(byTestId(`thread-row-${remoteThread.id}`), "remote row");

    invoke(row, "onContextMenu", mouseEvent());
    await flush();

    const childIds = menuItems
      .find((item) => item.id === "open-in")
      ?.children?.map((item) => item.id);
    expect(childIds).toContain("open-in:cursor");
    expect(childIds).not.toContain("open-in:vscode");
  });

  it("omits File Explorer when the desktop bridge capability is unavailable", async () => {
    baseScenario();
    h.state.serverConfigs.set(ENV_MAIN, {
      availableEditors: ["vscode"],
      environment: { capabilities: { worktreeCatalog: true } },
    });
    render(<Sidebar />);
    fakeLocalApi();
    let menuItems: Array<{ id: string; children?: Array<{ id: string }> }> = [];
    h.spies.contextMenuShow.mockImplementation(
      async (items: Array<{ id: string; children?: Array<{ id: string }> }>) => {
        menuItems = items;
        return null;
      },
    );
    const row = mustFindProps(byTestId("thread-row-thread-active"), "active worktree row");

    invoke(row, "onContextMenu", mouseEvent());
    await flush();

    const openIn = menuItems.find((item) => item.id === "open-in");
    expect(openIn?.children?.map((item) => item.id)).toEqual(["open-in:vscode"]);
  });

  it("toasts when opening an editor fails", async () => {
    const row = setupMenu("open-in:vscode");
    h.state.commandResults["shell.openInEditor"] = () => failureResult("no editor");
    invoke(row, "onContextMenu", mouseEvent());
    await flush();
    expect(h.spies.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Failed to open editor", description: "no editor" }),
    );
  });

  it("starts a rename from the menu", async () => {
    const row = setupMenu("rename");
    invoke(row, "onContextMenu", mouseEvent());
    await flush();
    expect(h.spies.contextMenuShow).toHaveBeenCalled();
  });

  it("marks threads unread/read and toggles pins", async () => {
    const row = setupMenu("mark-unread");
    invoke(row, "onContextMenu", mouseEvent());
    await flush();
    expect(h.spies.markUnread).toHaveBeenCalledWith(threadKeyOf(threadIdle));

    h.metaStore.setState({ unreadThreadKeys: [threadKeyOf(threadIdle)] });
    h.spies.contextMenuShow.mockResolvedValue("mark-read");
    invoke(row, "onContextMenu", mouseEvent());
    await flush();
    expect(h.spies.markRead).toHaveBeenCalledWith(threadKeyOf(threadIdle));

    h.spies.contextMenuShow.mockResolvedValue("toggle-pin");
    invoke(row, "onContextMenu", mouseEvent());
    await flush();
    expect(h.spies.togglePinned).toHaveBeenCalledWith(threadKeyOf(threadIdle));
  });

  it("copies the workspace path and thread id", async () => {
    const row = setupMenu("copy-path");
    invoke(row, "onContextMenu", mouseEvent());
    await flush();
    expect(h.spies.copyToClipboard).toHaveBeenCalledWith("C:/repo-a", { path: "C:/repo-a" });

    h.spies.contextMenuShow.mockResolvedValue("copy-thread-id");
    invoke(row, "onContextMenu", mouseEvent());
    await flush();
    expect(h.spies.copyToClipboard).toHaveBeenCalledWith("thread-idle", {
      threadId: "thread-idle",
    });
    expect(h.spies.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Thread ID copied" }),
    );
  });

  it("copies the branch shown on the row, grouped with the other copy actions", async () => {
    baseScenario();
    render(<Sidebar />);
    fakeLocalApi();
    let menuItems: Array<{ id?: string; label?: string; separator?: true }> = [];
    h.spies.contextMenuShow.mockImplementation(async (items) => {
      menuItems = items;
      return "copy-branch-name";
    });
    const row = mustFindProps(byTestId("thread-row-thread-active"), "active worktree row");
    invoke(row, "onContextMenu", mouseEvent());
    await flush();

    expect(menuItems.map((item) => (item.separator ? "---" : item.id))).toEqual([
      "open-in",
      "pull",
      "---",
      "copy-path",
      "copy-branch-name",
      "copy-thread-id",
      "---",
      "toggle-pin",
      "mark-unread",
      "rename",
      "---",
      "delete",
    ]);
    expect(h.spies.copyToClipboard).toHaveBeenCalledWith("feat/x", { branch: "feat/x" });
    expect(h.spies.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Branch name copied", description: "feat/x" }),
    );
  });

  it("switches branch display and copy together to the fresh live ref", async () => {
    baseScenario();
    h.state.vcsStatusByCwd["C:/wt/x"] = { refName: "fresh-displayed" };
    const markup = render(<Sidebar />);
    fakeLocalApi();
    h.spies.contextMenuShow.mockResolvedValue("copy-branch-name");
    expect(markup).toContain(">fresh-displayed<");
    invoke(
      mustFindProps(byTestId("thread-row-thread-active"), "row"),
      "onContextMenu",
      mouseEvent(),
    );
    await flush();
    expect(h.spies.copyToClipboard).toHaveBeenCalledWith("fresh-displayed", {
      branch: "fresh-displayed",
    });
  });

  it("omits Copy Branch Name when the row shows no branch", async () => {
    baseScenario();
    h.state.vcsStatusByCwd[projectA.workspaceRoot] = { refName: null, detachedHead: null };
    render(<Sidebar />);
    fakeLocalApi();
    const row = mustFindProps(byTestId("thread-row-thread-idle"), "idle row");
    let menuItems: Array<{ id?: string; label?: string; separator?: true }> = [];
    h.spies.contextMenuShow.mockImplementation(async (items) => {
      menuItems = items;
      return null;
    });
    invoke(row, "onContextMenu", mouseEvent());
    await flush();

    expect(menuItems.some((item) => item.id === "copy-branch-name")).toBe(false);
    expect(menuItems.at(-1)).toMatchObject({ id: "delete", label: "Delete Thread…" });
  });

  it("toasts when copying the thread id fails", async () => {
    const row = setupMenu("copy-thread-id");
    h.state.copyShouldFail = true;
    invoke(row, "onContextMenu", mouseEvent());
    await flush();
    expect(h.spies.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Failed to copy thread ID" }),
    );
  });

  it("deletes the thread after confirmation and toasts failures", async () => {
    const row = setupMenu("delete");
    invoke(row, "onContextMenu", mouseEvent());
    await flush();
    expect(h.spies.dialogConfirm).toHaveBeenCalledWith(expect.stringContaining("Delete thread"));
    expect(h.spies.deleteThread).toHaveBeenCalled();

    h.spies.deleteThread.mockResolvedValue(failureResult("delete failed"));
    invoke(row, "onContextMenu", mouseEvent());
    await flush();
    expect(h.spies.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Failed to delete thread", description: "delete failed" }),
    );

    h.spies.deleteThread.mockClear();
    h.spies.dialogConfirm.mockResolvedValue(false);
    invoke(row, "onContextMenu", mouseEvent());
    await flush();
    expect(h.spies.deleteThread).not.toHaveBeenCalled();
  });

  it("opens the typed removal dialog for a worktree instead of native delete confirmation", async () => {
    baseScenario();
    setWorktreeSessionStatus("ready");
    render(<Sidebar />);
    fakeLocalApi();
    const deleteItem = chooseDeleteFromCardMenu();
    const row = mustFindProps(byTestId("thread-row-thread-active"), "worktree row");

    invoke(row, "onContextMenu", mouseEvent());
    await flush();

    expect(deleteItem.current).toMatchObject({ label: "Delete Worktree…" });
    expect(deleteItem.current?.disabled).not.toBe(true);
    expect(h.spies.requestWorktreeRemoval).toHaveBeenCalledWith(
      expect.objectContaining({
        projectId: projectA.id,
        threadId: threadActive.id,
        path: "C:/wt/x",
      }),
    );
    expect(h.spies.dialogConfirm).not.toHaveBeenCalled();
    expect(h.spies.deleteThread).not.toHaveBeenCalled();
  });

  it("disables Delete Worktree, saying how to proceed, while the worktree's session runs", async () => {
    baseScenario();
    // An editor keeps Open in enabled, so only the session can disable an item.
    h.state.serverConfigs.set(ENV_MAIN, {
      availableEditors: ["vscode"],
      environment: { capabilities: { worktreeCatalog: true } },
    });
    render(<Sidebar />);
    fakeLocalApi();
    const items = await openCardMenu("thread-row-thread-active");

    expect(items.find((item) => item.id === "delete")).toEqual({
      id: "delete",
      label: "Delete Worktree…",
      destructive: true,
      icon: "trash",
      disabled: true,
      description: "Stop the running session before deleting this worktree.",
    });
    // Only Delete waits for the session; the card's other actions stay usable.
    expect(items.filter((item) => item.disabled === true).map((item) => item.id)).toEqual([
      "delete",
    ]);
  });

  it("disables Delete Worktree while another chat open in the worktree runs, not one elsewhere", async () => {
    const runningChat = (id: string, worktreePath: string) =>
      makeThread(id, {
        kind: "panel",
        worktreePath,
        session: { ...threadActive.session!, threadId: ThreadId.make(id) },
      });
    baseScenario();
    setWorktreeSessionStatus("ready");
    h.state.threads = [...h.state.threads, runningChat("chat-elsewhere", "C:/wt/other")];
    render(<Sidebar />);
    fakeLocalApi();
    let items = await openCardMenu("thread-row-thread-active");
    expect(items.find((item) => item.id === "delete")?.disabled).not.toBe(true);

    h.state.threads = [...h.state.threads, runningChat("chat-here", "C:/wt/x")];
    render(<Sidebar />);
    items = await openCardMenu("thread-row-thread-active");
    expect(items.find((item) => item.id === "delete")).toMatchObject({
      disabled: true,
      description: "Stop the running session before deleting this worktree.",
    });
  });

  it("shows the multi-select menu when the row is part of a selection", async () => {
    baseScenario();
    // The row's isSelected flag is read at render time, so select first.
    h.selectionStore.setState({
      selectedThreadKeys: new Set([threadKeyOf(threadIdle), threadKeyOf(threadActive)]),
    });
    render(<Sidebar />);
    fakeLocalApi();
    const row = mustFindProps(byTestId("thread-row-thread-idle"), "idle row");

    // Mark unread across the selection.
    h.spies.contextMenuShow.mockResolvedValue("mark-unread");
    invoke(row, "onContextMenu", mouseEvent());
    await flush();
    expect(h.spies.markThreadUnread).toHaveBeenCalledTimes(2);
    expect(h.spies.clearSelection).toHaveBeenCalled();

    // Delete across the selection.
    h.spies.contextMenuShow.mockResolvedValue("delete");
    invoke(row, "onContextMenu", mouseEvent());
    await flush();
    expect(h.spies.dialogConfirm).toHaveBeenCalledWith(
      expect.stringContaining(
        "1 worktree-backed thread will be removed from Coddy Studio only. Git worktrees and files are left untouched.",
      ),
    );
    expect(h.spies.deleteThread).toHaveBeenCalledTimes(2);
    expect(h.spies.removeFromSelection).toHaveBeenCalled();
  });

  it("toasts when the multi-select menu itself fails", async () => {
    baseScenario();
    h.selectionStore.setState({ selectedThreadKeys: new Set([threadKeyOf(threadIdle)]) });
    render(<Sidebar />);
    fakeLocalApi();
    h.spies.contextMenuShow.mockRejectedValue(new Error("menu exploded"));
    const row = mustFindProps(byTestId("thread-row-thread-idle"), "idle row");
    invoke(row, "onContextMenu", mouseEvent());
    await flush();
    expect(h.spies.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Thread action failed" }),
    );
  });

  it("uses generic messages for opaque row and multi-select menu failures", async () => {
    baseScenario();
    h.selectionStore.setState({ selectedThreadKeys: new Set([threadKeyOf(threadIdle)]) });
    render(<Sidebar />);
    fakeLocalApi();
    h.spies.contextMenuShow.mockRejectedValue("opaque menu failure");
    let row = mustFindProps(byTestId("thread-row-thread-idle"), "idle row");
    invoke(row, "onContextMenu", mouseEvent());
    await flush();
    expect(h.spies.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ description: "An error occurred." }),
    );

    h.spies.toastAdd.mockClear();
    h.selectionStore.setState({ selectedThreadKeys: new Set() });
    h.state.captures.length = 0;
    render(<Sidebar />);
    row = mustFindProps(byTestId("thread-row-thread-idle"), "idle row");
    invoke(row, "onContextMenu", mouseEvent());
    await flush();
    expect(h.spies.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ description: "An error occurred." }),
    );
  });

  it("clears a selection that does not include the row before showing the row menu", async () => {
    baseScenario();
    render(<Sidebar />);
    fakeLocalApi();
    h.selectionStore.setState({ selectedThreadKeys: new Set(["someone-else"]) });
    const row = mustFindProps(byTestId("thread-row-thread-idle"), "idle row");
    invoke(row, "onContextMenu", mouseEvent());
    await flush();
    expect(h.spies.clearSelection).toHaveBeenCalled();
    expect(h.spies.contextMenuShow).toHaveBeenCalled();
  });
});

staticDescribe("primary row", () => {
  const primaryCard = () => mustFindProps(byTestId("primary-card-project-a"), "primary card");

  it.each([
    {
      reason: "unreadable",
      label: "Repository unreadable",
      message:
        "Git can't read this repository. Check its .git folder, for example a damaged HEAD or config file.",
    },
    {
      reason: "absent",
      label: "Not a Git repository",
      message: "This folder isn't a Git repository. Run git init to create one.",
    },
    {
      reason: "untrusted",
      label: "Repository not trusted",
      message:
        "Git doesn't trust this repository because another user owns it. Run git config --global --add safe.directory C:/repo-a to trust it.",
    },
    {
      reason: undefined,
      label: "Repository unavailable",
      message:
        "Git can't read this folder as a repository. Run git init to create one, or check its .git folder if it already is one.",
    },
  ])("explains an unavailable primary checkout: $label", ({ reason, label, message }) => {
    baseScenario();
    h.state.threads = [{ ...threadDefault, branch: "main" }];
    h.state.vcsStatusByCwd[projectA.workspaceRoot] = {
      isRepo: false,
      ...(reason === undefined ? {} : { repositoryUnavailableReason: reason }),
      refName: null,
      detachedHead: null,
      pr: null,
      sourceControlProvider: null,
      hasWorkingTreeChanges: false,
      stale: false,
    };
    const markup = render(<Sidebar />);
    expect(markup.match(/data-testid="primary-card-title-project-a"[^>]*>([^<]+)/)?.[1]).toBe(
      "Repo A",
    );
    expect(markup).toContain(`>${label}</span>`);
    expect(markup).toContain(`<span class="sr-only">${message.replaceAll("'", "&#x27;")}</span>`);
    const cardMarkup = markup.match(/data-testid="primary-card-project-a"[\s\S]*?<\/li>/)?.[0];
    expect(cardMarkup).toBeDefined();
    expect(cardMarkup).not.toContain("lucide-git-branch");
    expect(markup).not.toContain('aria-label="Uncommitted changes"');
    const button = mustFindProps(byTestId("primary-card-button-project-a"), "primary button");
    expect(button["aria-describedby"]).toContain("-branch");
  });

  it("renders generic primary-card copy for an unknown reason decoded from a newer server", () => {
    baseScenario();
    h.state.threads = [{ ...threadDefault, branch: "main" }];
    h.state.vcsStatusByCwd[projectA.workspaceRoot] = decodeStatusSummary({
      isRepo: false,
      repositoryUnavailableReason: "futureReason",
      refName: null,
      detachedHead: null,
      pr: null,
      sourceControlProvider: null,
      hasWorkingTreeChanges: false,
      observedAt: iso(0),
      stale: false,
    });

    const markup = render(<Sidebar />);
    const cardMarkup = markup.match(/data-testid="primary-card-project-a"[\s\S]*?<\/li>/)?.[0];
    expect(cardMarkup).toBeDefined();
    expect(cardMarkup).toContain(">Repository unavailable</span>");
    expect(cardMarkup).toContain(
      '<span class="sr-only">Git can&#x27;t read this folder as a repository. Run git init to create one, or check its .git folder if it already is one.</span>',
    );
  });

  it("keeps a stale unavailable summary visible instead of the recorded branch", () => {
    baseScenario();
    h.state.threads = [{ ...threadDefault, branch: "main" }];
    h.state.vcsStatusByCwd[projectA.workspaceRoot] = {
      isRepo: false,
      repositoryUnavailableReason: "unreadable",
      stale: true,
    };
    const markup = render(<Sidebar />);
    expect(markup.match(/data-testid="primary-card-title-project-a"[^>]*>([^<]+)/)?.[1]).toBe(
      "Repo A",
    );
    expect(markup).toContain(">Repository unreadable</span>");
  });

  it("keeps terminal and port indicators on an unavailable primary card", () => {
    baseScenario();
    h.state.threads = [threadDefault];
    h.state.vcsStatusByCwd[projectA.workspaceRoot] = {
      isRepo: false,
      repositoryUnavailableReason: "absent",
    };
    h.state.runningTerminalIds = ["terminal-1"];
    h.state.discoveredPortsByThreadId[threadDefault.id] = [{ port: 3000 }];
    const markup = render(<Sidebar />);
    expect(markup).toContain(">Not a Git repository</span>");
    expect(markup).toContain("lucide-terminal");
    expect(markup).toContain('aria-label="Open localhost:3000"');
  });

  it("passes no branch to the unavailable primary card's context menu", async () => {
    baseScenario();
    h.state.threads = [{ ...threadDefault, branch: "main" }];
    h.state.vcsStatusByCwd[projectA.workspaceRoot] = {
      isRepo: false,
      repositoryUnavailableReason: "unreadable",
    };
    render(<Sidebar />);
    fakeLocalApi();
    h.spies.contextMenuShow.mockResolvedValue("copy-branch-name");
    invoke(primaryCard(), "onContextMenu", mouseEvent());
    await flush();
    expect(h.spies.contextMenuShow).toHaveBeenCalledOnce();
    const items = h.spies.contextMenuShow.mock.calls[0]![0] as CardMenuItem[];
    expect(items.some((item) => item.id === "copy-branch-name")).toBe(false);
    expect(h.spies.copyToClipboard).not.toHaveBeenCalled();
  });

  it("navigates to the default thread on click", () => {
    baseScenario();
    render(<Sidebar />);
    const primaryRow = { props: primaryCard() };
    expect(primaryRow).toBeDefined();
    invoke(primaryRow.props, "onClick", mouseEvent());
    expect(h.spies.routerNavigate).toHaveBeenCalledWith(
      expect.objectContaining({
        params: expect.objectContaining({ threadId: "thread-default" }),
      }),
    );
  });

  it("creates a default thread on demand when the server has not backfilled one", async () => {
    h.state.projects = [
      makeProject("project-a", {
        defaultModelSelection: createModelSelection(
          ProviderInstanceId.make("claude"),
          "claude-fable-stale",
          [
            { id: "effort", value: "medium" },
            { id: "fastMode", value: false },
          ],
        ),
      }),
    ];
    h.state.threads = [threadIdle];
    h.state.environments = [environmentFixture({ environmentId: ENV_MAIN, label: "Main" })];
    h.state.serverConfigs = new Map([
      [
        ENV_MAIN,
        {
          providers: [
            {
              enabled: true,
              instanceId: ProviderInstanceId.make("claude"),
              driver: "claudeAgent",
              models: [
                {
                  slug: "claude-fable-stale",
                  capabilities: {
                    optionDescriptors: [
                      {
                        id: "effort",
                        label: "Effort",
                        type: "select",
                        options: [
                          { id: "medium", label: "Medium", isDefault: true },
                          { id: "high", label: "High" },
                        ],
                        currentValue: "medium",
                      },
                      {
                        id: "fastMode",
                        label: "Fast",
                        type: "boolean",
                        currentValue: false,
                      },
                    ],
                  },
                },
                {
                  slug: "claude-fable-5",
                  capabilities: {
                    optionDescriptors: [
                      {
                        id: "effort",
                        label: "Effort",
                        type: "select",
                        options: [
                          { id: "medium", label: "Medium", isDefault: true },
                          { id: "high", label: "High" },
                        ],
                        currentValue: "medium",
                      },
                      {
                        id: "fastMode",
                        label: "Fast",
                        type: "boolean",
                        currentValue: false,
                      },
                    ],
                  },
                },
              ],
            },
          ],
          settings: {
            ...DEFAULT_SERVER_SETTINGS,
            providerSessionDefaults: {
              claudeAgent: {
                model: "claude-fable-5",
                options: [
                  { id: "effort", value: "high" },
                  { id: "fastMode", value: true },
                ],
              },
            },
          },
        },
      ],
    ]);
    render(<Sidebar />);
    const primaryRow = { props: primaryCard() };
    invoke(primaryRow.props, "onClick", mouseEvent());
    await flush();

    const createCall = h.state.commandCalls.find(
      (call: { label: string }) => call.label === "thread.create",
    );
    expect(createCall).toBeDefined();
    expect(createCall.input.input).not.toHaveProperty("kind");
    expect(createCall.input.input).not.toHaveProperty("worktreePath");
    expect(createCall.input.input.modelSelection.model).toBe("claude-fable-5");
    expect(createCall.input.input.modelSelection.options).toEqual([
      { id: "effort", value: "high" },
      { id: "fastMode", value: true },
    ]);
    expect(h.spies.routerNavigate).toHaveBeenCalled();
  });

  it("falls back to the codex provider when no server config exists", async () => {
    h.state.projects = [projectA];
    h.state.threads = [];
    h.state.environments = [environmentFixture({ environmentId: ENV_MAIN, label: "Main" })];
    render(<Sidebar />);
    const primaryRow = { props: primaryCard() };
    invoke(primaryRow.props, "onClick", mouseEvent());
    await flush();
    const createCall = h.state.commandCalls.find(
      (call: { label: string }) => call.label === "thread.create",
    );
    expect(createCall.input.input.modelSelection.instanceId).toBe("codex");
  });

  it("stops after a failed default-thread creation", async () => {
    h.state.projects = [projectA];
    h.state.threads = [];
    h.state.environments = [environmentFixture({ environmentId: ENV_MAIN, label: "Main" })];
    h.state.commandResults["thread.create"] = () => failureResult("create failed");
    render(<Sidebar />);
    const primaryRow = { props: primaryCard() };
    invoke(primaryRow.props, "onClick", mouseEvent());
    await flush();
    expect(h.spies.routerNavigate).not.toHaveBeenCalled();
  });

  it("shows the primary-row context menu and handles pull / copy / pin actions", async () => {
    baseScenario();
    h.state.serverConfigs.set(ENV_MAIN, {
      availableEditors: ["vscode"],
      environment: { capabilities: { worktreeCatalog: true } },
    });
    render(<Sidebar />);
    fakeLocalApi();
    const primaryRow = { props: primaryCard() };

    h.spies.contextMenuShow.mockResolvedValue("pull");
    invoke(primaryRow.props, "onContextMenu", mouseEvent());
    await flush();
    expect(h.state.commandCalls.map((call: { label: string }) => call.label)).toContain("vcs.pull");

    h.spies.contextMenuShow.mockResolvedValue("copy-path");
    invoke(primaryRow.props, "onContextMenu", mouseEvent());
    await flush();
    expect(h.spies.copyToClipboard).toHaveBeenCalledWith("C:/repo-a", { path: "C:/repo-a" });

    h.spies.contextMenuShow.mockResolvedValue("mark-unread");
    invoke(primaryRow.props, "onContextMenu", mouseEvent());
    await flush();
    expect(h.spies.markUnread).toHaveBeenCalledWith(threadKeyOf(threadDefault));

    h.metaStore.setState({ unreadThreadKeys: [threadKeyOf(threadDefault)] });
    h.spies.contextMenuShow.mockResolvedValue("mark-read");
    invoke(primaryRow.props, "onContextMenu", mouseEvent());
    await flush();
    expect(h.spies.markRead).toHaveBeenCalledWith(threadKeyOf(threadDefault));

    h.spies.contextMenuShow.mockResolvedValue("toggle-pin");
    invoke(primaryRow.props, "onContextMenu", mouseEvent());
    await flush();
    expect(h.spies.togglePinned).toHaveBeenCalledWith(threadKeyOf(threadDefault));

    h.spies.contextMenuShow.mockResolvedValue("open-in:vscode");
    invoke(primaryRow.props, "onContextMenu", mouseEvent());
    await flush();
    expect(h.state.commandCalls.map((call: { label: string }) => call.label)).toContain(
      "shell.openInEditor",
    );
  });

  it("offers and copies the live checkout branch from the primary row", async () => {
    baseScenario();
    render(<Sidebar />);
    fakeLocalApi();
    const primaryRow = { props: primaryCard() };
    let menuItems: Array<{ id?: string; separator?: true }> = [];
    h.spies.contextMenuShow.mockImplementation(async (items) => {
      menuItems = items;
      return "copy-branch-name";
    });
    invoke(primaryRow.props, "onContextMenu", mouseEvent());
    await flush();

    expect(menuItems.map((item) => (item.separator ? "---" : item.id))).toEqual([
      "open-in",
      "pull",
      "---",
      "copy-path",
      "copy-branch-name",
      "---",
      "toggle-pin",
      "mark-unread",
    ]);
    expect(h.spies.copyToClipboard).toHaveBeenCalledWith("main", { branch: "main" });
  });

  it("opens the local primary checkout in File Explorer even when no editor is available", async () => {
    baseScenario();
    h.state.serverConfigs.set(ENV_MAIN, {
      availableEditors: [],
      environment: { capabilities: {}, serverVersion: "0.1.0" },
    });
    const openInFileManager = vi.fn(async () => {});
    (globalThis.window as unknown as Record<string, unknown>)["desktopBridge"] = {
      openInFileManager,
    };
    render(<Sidebar />);
    fakeLocalApi();
    const primaryRow = { props: primaryCard() };
    h.spies.contextMenuShow.mockImplementation(
      async (
        items: Array<{ id: string; disabled?: boolean; children?: Array<{ id: string }> }>,
      ) => {
        const openIn = items.find((item) => item.id === "open-in");
        expect(openIn?.disabled).not.toBe(true);
        expect(openIn?.children?.map((item) => item.id)).toEqual(["open-in:file-explorer"]);
        return "open-in:file-explorer";
      },
    );

    invoke(primaryRow.props, "onContextMenu", mouseEvent());
    await flush();

    expect(openInFileManager).toHaveBeenCalledOnce();
    expect(openInFileManager).toHaveBeenCalledWith("C:/repo-a", true);
  });

  it("keeps the primary-row menu inert without a local API or a matching editor", async () => {
    baseScenario();
    render(<Sidebar />);
    const primaryRow = { props: primaryCard() };

    invoke(primaryRow.props, "onContextMenu", mouseEvent());
    await flush();
    expect(h.spies.contextMenuShow).not.toHaveBeenCalled();

    fakeLocalApi();
    h.spies.contextMenuShow.mockResolvedValue("open-in:unknown");
    invoke(primaryRow.props, "onContextMenu", mouseEvent());
    await flush();
    expect(h.state.commandCalls).toEqual([]);
  });

  it("suppresses interrupted primary-row actions and reports opaque failures", async () => {
    baseScenario();
    h.state.serverConfigs.set(ENV_MAIN, {
      availableEditors: ["vscode"],
      environment: { capabilities: { worktreeCatalog: true } },
    });
    render(<Sidebar />);
    fakeLocalApi();
    const primaryRow = { props: primaryCard() };

    h.spies.contextMenuShow.mockResolvedValue("pull");
    h.state.commandResults["vcs.pull"] = () => ({
      _tag: "Failure",
      cause: Cause.interrupt(1),
    });
    invoke(primaryRow.props, "onContextMenu", mouseEvent());
    await flush();
    expect(h.spies.toastAdd).not.toHaveBeenCalled();

    h.spies.contextMenuShow.mockResolvedValue("open-in:vscode");
    h.state.commandResults["shell.openInEditor"] = () => ({
      _tag: "Failure",
      cause: Cause.fail("opaque editor failure"),
    });
    invoke(primaryRow.props, "onContextMenu", mouseEvent());
    await flush();
    expect(h.spies.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({
        title: "Failed to open editor",
        description: "An error occurred.",
      }),
    );
  });

  it("does not expose project removal from the primary branch row", async () => {
    h.state.projects = [projectA];
    h.state.threads = [threadDefault];
    h.state.environments = [environmentFixture({ environmentId: ENV_MAIN, label: "Main" })];
    render(<Sidebar />);
    fakeLocalApi();
    const primaryRow = { props: primaryCard() };

    h.spies.contextMenuShow.mockImplementation(async (items: Array<{ id: string }>) => {
      expect(items.some((item) => item.id?.startsWith("delete") === true)).toBe(false);
      return null;
    });
    invoke(primaryRow.props, "onContextMenu", mouseEvent());
    await flush();
    expect(h.spies.contextMenuShow).toHaveBeenCalled();
    expect(h.spies.toastAdd).not.toHaveBeenCalledWith(
      expect.objectContaining({ title: "Project is not empty" }),
    );
  });

  // A stale summary is still the newest observation: a failed PR lookup marks a
  // just-read base stale, and the server carries a PR only for the same ref.
  it.each([
    { label: "fresh passive summary", status: { stale: false }, showsIndicators: true },
    { label: "full status result", status: {}, showsIndicators: true },
    { label: "stale passive summary", status: { stale: true }, showsIndicators: true },
  ])("reads primary PR/dirty state directly from $label", ({ status, showsIndicators }) => {
    baseScenario();
    h.state.threads = [threadDefault];
    h.state.vcsStatusByCwd[projectA.workspaceRoot] = {
      refName: "main",
      hasWorkingTreeChanges: true,
      pr: { url: "https://example.invalid/pr/1" },
      ...status,
    };
    const markup = render(<Sidebar />);
    expect(threadDefault.branch).toBeNull();
    expect(markup.includes("#1")).toBe(showsIndicators);
    expect(markup.includes('aria-label="Uncommitted changes"')).toBe(showsIndicators);
    expect(markup.includes("h-[18px]")).toBe(showsIndicators);
  });

  it("hides the primary branch line when the title already shows its branch and there are no indicators", () => {
    baseScenario();
    h.state.threads = [threadDefault];
    h.state.vcsStatusByCwd[projectA.workspaceRoot] = {
      refName: "main",
      stale: false,
      pr: null,
      hasWorkingTreeChanges: false,
    };
    expect(render(<Sidebar />)).not.toContain("h-[18px]");
  });

  it("renders the primary card first in the project's one list", () => {
    baseScenario();
    const markup = render(<Sidebar />);
    expect(captured("SidebarMenuSub")).toHaveLength(1);
    // Chats are borderless rows on the project card, so 2 px separates them.
    expect(String(captured("SidebarMenuSub")[0]!.props["className"])).toContain("gap-0.5");
    expect(String(captured("SidebarMenuSub")[0]!.props["className"])).not.toContain("gap-1.5");
    // The list clips overflow, so it keeps the primitive's 2 px vertical padding:
    // the first and last card's 2 px focus ring reaches past the card border.
    expect(String(captured("SidebarMenuSub")[0]!.props["className"])).not.toMatch(
      /(^|\s)py-0(\s|$)/,
    );
    // The primary card is a borderless row like the workspace cards.
    expect(
      String(mustFindProps(byTestId("primary-card-project-a"), "primary card")["className"]),
    ).toContain("border-transparent");
    expect(markup.indexOf('data-testid="primary-card-project-a"')).toBeLessThan(
      markup.indexOf('data-testid="thread-row-thread-active"'),
    );
    expect(markup).toContain('data-testid="primary-card-button-project-a"');
  });

  it("shows the primary card's unseen completion once the default thread was visited", () => {
    baseScenario();
    h.state.threads = [
      makeThread("thread-default", {
        kind: "default",
        title: "Repo A",
        latestTurn: {
          turnId: "turn-default",
          state: "completed",
          requestedAt: iso(12),
          startedAt: iso(12),
          completedAt: iso(10),
          assistantMessageId: null,
        } as EnvironmentThreadShell["latestTurn"],
      }),
    ];
    h.uiStore.setState({
      threadLastVisitedAtById: { [threadKeyOf(threadDefault)]: iso(20) },
    });
    const markup = render(<Sidebar />);
    expect(markup).toContain('aria-label="Finished, not opened yet"');
  });

  it("counts panel chats on the worktree card and on the primary card", () => {
    baseScenario();
    h.state.threads = [
      ...h.state.threads,
      makeThread("panel-on-worktree", { kind: "panel", worktreePath: "C:/wt/x" }),
      makeThread("panel-main", { kind: "panel" }),
      makeThread("panel-archived", { kind: "panel", archivedAt: iso(3) }),
    ];
    const markup = render(<Sidebar />);
    // The base scenario's own panel thread has no path, so the primary card counts two.
    expect(markup).toContain("2 more chats");
    expect(markup).toContain("1 more chat<");
  });
});

staticDescribe("new thread entry points", () => {
  it("leads the hover strip with project actions and uses + for New worktree", () => {
    baseScenario();
    const markup = render(<Sidebar />);

    expect(findProps(byTestId("new-main-chat-button"))).toBeNull();
    const actions = mustFindProps(byTestId("project-actions-button"), "project actions button");
    expect(actions["aria-label"]).toBe("Project actions for Repo A");
    expect(actions["aria-haspopup"]).toBe("menu");
    expect(markup).toContain("lucide-ellipsis");
    expect(markup).toContain("lucide-plus");
    const hoverStrip = markup.slice(
      markup.indexOf('data-testid="project-actions-button"'),
      markup.indexOf('data-testid="pull-requests-button"'),
    );
    expect(hoverStrip).toContain("lucide-plus");
    expect(hoverStrip).not.toContain("lucide-folder-git-2");
    expect(markup.indexOf("lucide-ellipsis")).toBeLessThan(markup.indexOf("lucide-plus"));
    expect(markup).toContain('data-testid="sidebar-projects-group"');
  });

  it("closes the mobile sheet before creating a worktree from its project row", () => {
    baseScenario();
    h.state.sidebarCtx = { isMobile: true, setOpenMobile: h.spies.setOpenMobile };
    render(<Sidebar />);

    const click = mouseEvent();
    invoke(mustFindProps(byTestId("new-worktree-button"), "new worktree button"), "onClick", click);

    expect(click.preventDefault).toHaveBeenCalled();
    expect(click.stopPropagation).toHaveBeenCalled();
    expect(h.spies.setOpenMobile).toHaveBeenCalledWith(false);
  });

  it("renders the project actions in the project row and not in the Projects toolbar", () => {
    baseScenario();
    render(<Sidebar />);

    expect(findProps(byTestId("sidebar-new-main-chat-trigger"))).toBeNull();
    expect(findProps(byTestId("sidebar-new-worktree-trigger"))).toBeNull();
    expect(findProps(byAriaLabel("New main-branch chat in Repo A"))).toBeNull();
    expect(mustFindProps(byAriaLabel("Project actions for Repo A"), "row actions")).toBeDefined();
    expect(mustFindProps(byAriaLabel("New worktree in Repo A"), "row worktree")).toBeDefined();
    expect(mustFindProps(byAriaLabel("Git Manager for Repo A"), "row Git Manager")).toBeDefined();
  });

  it("keeps a clicked project header selected until another thread route opens", () => {
    baseScenario();
    const findHeader = () =>
      captured("SidebarMenuButton").find(
        (entry) => typeof entry.props["onPointerDownCapture"] === "function",
      )!;
    render(<Sidebar />);
    expect(findHeader().props["isActive"]).toBe(false);

    invoke(findHeader().props, "onClick", mouseEvent());
    h.state.captures = [];
    render(<Sidebar />);
    expect(findHeader().props["isActive"]).toBe(true);
    expect(findHeader().props["data-selected"]).toBe(true);

    h.state.captures = [];
    h.state.routeParams = { environmentId: ENV_MAIN, threadId: "thread-idle" };
    render(<Sidebar />);
    expect(findHeader().props["isActive"]).toBe(false);
    expect(findHeader().props["data-selected"]).toBe(false);
  });

  it("highlights the project header while its Git Manager route is open", () => {
    baseScenario();
    h.state.pathname = `/project/${ENV_MAIN}/${projectA.id}/git`;
    h.state.routeParams = { environmentId: ENV_MAIN, projectId: projectA.id };
    render(<Sidebar />);
    const header = captured("SidebarMenuButton").find(
      (entry) => typeof entry.props["onPointerDownCapture"] === "function",
    )!;
    expect(header.props["isActive"]).toBe(true);
    expect(header.props["aria-current"]).toBe("page");

    h.state.captures = [];
    h.state.pathname = "/";
    h.state.routeParams = {};
    render(<Sidebar />);
    const idleHeader = captured("SidebarMenuButton").find(
      (entry) => typeof entry.props["onPointerDownCapture"] === "function",
    )!;
    expect(idleHeader.props["isActive"]).toBe(false);
    expect(idleHeader.props["aria-current"]).toBeUndefined();
  });

  it("navigates idempotently to one project-scoped Git Manager", () => {
    baseScenario();
    render(<Sidebar />);
    const gitManager = mustFindProps(byTestId("git-manager-button"), "Git Manager button");
    const firstClick = mouseEvent();
    const secondClick = mouseEvent();

    invoke(gitManager, "onClick", firstClick);
    invoke(gitManager, "onClick", secondClick);

    const navigation = {
      to: "/project/$environmentId/$projectId/git",
      params: { environmentId: ENV_MAIN, projectId: projectA.id },
    };
    expect(h.spies.navigate).toHaveBeenNthCalledWith(1, navigation);
    expect(h.spies.navigate).toHaveBeenNthCalledWith(2, navigation);
    expect(firstClick.preventDefault).toHaveBeenCalled();
    expect(secondClick.stopPropagation).toHaveBeenCalled();
  });
  it("highlights the project header while its Pull Requests route is open", () => {
    baseScenario();
    h.state.pathname = `/project/${ENV_MAIN}/${projectA.id}/pull-requests/14`;
    h.state.routeParams = { environmentId: ENV_MAIN, projectId: projectA.id };
    render(<Sidebar />);
    const header = captured("SidebarMenuButton").find(
      (entry) => typeof entry.props["onPointerDownCapture"] === "function",
    )!;
    expect(header.props["isActive"]).toBe(true);
    expect(header.props["aria-current"]).toBe("page");

    h.state.captures = [];
    h.state.pathname = "/";
    h.state.routeParams = {};
    render(<Sidebar />);
    const idleHeader = captured("SidebarMenuButton").find(
      (entry) => typeof entry.props["onPointerDownCapture"] === "function",
    )!;
    expect(idleHeader.props["isActive"]).toBe(false);
    expect(idleHeader.props["aria-current"]).toBeUndefined();
  });

  it("navigates idempotently to one project-scoped Pull Requests", () => {
    baseScenario();
    render(<Sidebar />);
    const pullRequests = mustFindProps(byTestId("pull-requests-button"), "Pull Requests button");
    const firstClick = mouseEvent();
    const secondClick = mouseEvent();

    invoke(pullRequests, "onClick", firstClick);
    invoke(pullRequests, "onClick", secondClick);

    const navigation = {
      to: "/project/$environmentId/$projectId/pull-requests",
      params: { environmentId: ENV_MAIN, projectId: projectA.id },
    };
    expect(h.spies.navigate).toHaveBeenNthCalledWith(1, navigation);
    expect(h.spies.navigate).toHaveBeenNthCalledWith(2, navigation);
    expect(firstClick.preventDefault).toHaveBeenCalled();
    expect(secondClick.stopPropagation).toHaveBeenCalled();
  });
  it("hides the Pull Requests button when disabled in settings", () => {
    baseScenario();
    render(<Sidebar />);
    expect(findProps(byTestId("pull-requests-button"))).not.toBeNull();
    h.state.captures = [];
    h.state.clientSettings.pullRequestsEnabled = false;
    render(<Sidebar />);
    expect(findProps(byTestId("pull-requests-button"))).toBeNull();
  });

  it("labels the Pull Requests button for its project", () => {
    baseScenario();
    render(<Sidebar />);
    expect(
      mustFindProps(byAriaLabel("Pull Requests for Repo A"), "Pull Requests button"),
    ).toBeDefined();
  });
});

staticDescribe("grouped and remote projects", () => {
  it("groups projects by repository across environments", () => {
    groupedScenario();
    const markup = render(<Sidebar />);
    expect(markup).toContain("2 projects");
    expect(markup).toContain("thread-row-thread-remote");
    expect(markup).not.toContain("lucide-cloud");
  });

  it("uses the chosen grouped-project member for worktree creation", async () => {
    groupedScenario();
    render(<Sidebar />);
    fakeLocalApi();
    h.spies.contextMenuShow.mockImplementation(
      async (items: Array<{ id: string }>) => items[1]!.id,
    );

    const worktree = mustFindProps(byTestId("new-worktree-button"), "new worktree button");
    invoke(worktree, "onClick", mouseEvent());
    await flush();

    expect(h.spies.contextMenuShow).toHaveBeenCalled();
  });

  it("opens the chosen grouped-project member's Git Manager", async () => {
    groupedScenario();
    render(<Sidebar />);
    fakeLocalApi();
    h.spies.contextMenuShow.mockImplementation(
      async (items: Array<{ id: string }>) => items[1]!.id,
    );

    const gitManager = mustFindProps(byTestId("git-manager-button"), "Git Manager button");
    invoke(gitManager, "onClick", mouseEvent());
    await flush();

    expect(h.spies.contextMenuShow).toHaveBeenCalled();
    expect(h.spies.navigate).toHaveBeenCalledWith({
      to: "/project/$environmentId/$projectId/git",
      params: { environmentId: ENV_REMOTE, projectId: ProjectId.make("project-a-remote") },
    });
  });

  it("opens the chosen grouped-project member's Pull Requests", async () => {
    groupedScenario();
    render(<Sidebar />);
    fakeLocalApi();
    h.spies.contextMenuShow.mockImplementation(
      async (items: Array<{ id: string }>) => items[1]!.id,
    );

    const pullRequests = mustFindProps(byTestId("pull-requests-button"), "Pull Requests button");
    invoke(pullRequests, "onClick", mouseEvent());
    await flush();

    expect(h.spies.contextMenuShow).toHaveBeenCalled();
    expect(h.spies.navigate).toHaveBeenCalledWith({
      to: "/project/$environmentId/$projectId/pull-requests",
      params: { environmentId: ENV_REMOTE, projectId: ProjectId.make("project-a-remote") },
    });
  });

  it("does not navigate when the grouped-project picker is unavailable or cancelled", async () => {
    groupedScenario();
    render(<Sidebar />);
    const gitManager = mustFindProps(byTestId("git-manager-button"), "Git Manager button");
    invoke(gitManager, "onClick", mouseEvent());
    await flush();
    expect(h.spies.navigate).not.toHaveBeenCalled();

    fakeLocalApi();
    h.spies.contextMenuShow.mockResolvedValue(null);
    invoke(gitManager, "onClick", mouseEvent());
    await flush();
    h.spies.contextMenuShow.mockResolvedValue("missing-member");
    invoke(gitManager, "onClick", mouseEvent());
    await flush();
    expect(h.spies.navigate).not.toHaveBeenCalled();
  });

  it("uses workspace paths when grouped members have no environment label", async () => {
    groupedScenario();
    h.state.environments = [];
    render(<Sidebar />);
    fakeLocalApi();
    h.spies.contextMenuShow.mockImplementation(async (items: Array<{ label: string }>) => {
      expect(items.every((item) => item.label.includes("C:/"))).toBe(true);
      return null;
    });
    const newThread = mustFindProps(byTestId("git-manager-button"), "Git Manager button");
    invoke(newThread, "onClick", mouseEvent());
    await flush();
    expect(h.spies.contextMenuShow).toHaveBeenCalled();
  });

  it("uses a generic message for opaque member-picker failures", async () => {
    groupedScenario();
    render(<Sidebar />);
    fakeLocalApi();
    h.spies.contextMenuShow.mockRejectedValue("opaque picker failure");
    const newThread = mustFindProps(byTestId("git-manager-button"), "Git Manager button");
    invoke(newThread, "onClick", mouseEvent());
    await flush();
    expect(h.spies.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ description: "An error occurred." }),
    );
  });

  it("toasts when the environment picker fails", async () => {
    groupedScenario();
    render(<Sidebar />);
    fakeLocalApi();
    h.spies.contextMenuShow.mockRejectedValue(new Error("picker broke"));
    const newThread = mustFindProps(byTestId("git-manager-button"), "Git Manager button");
    invoke(newThread, "onClick", mouseEvent());
    await flush();
    expect(h.spies.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Could not choose environment" }),
    );
  });

  it("renders a container icon for desktop-local sandbox projects", () => {
    const wslProject = makeProject("project-wsl", {
      workspaceRoot: "/home/user/repo",
      environmentId: ENV_WSL,
    });
    h.state.projects = [wslProject];
    h.state.threads = [];
    h.state.environments = [
      environmentFixture({ environmentId: ENV_MAIN, label: "Main", connectionId: "primary" }),
      environmentFixture({ environmentId: ENV_WSL, label: "WSL", connectionId: "local:wsl" }),
    ];
    const markup = render(<Sidebar />);
    expect(markup).toContain("Local sandbox project");
  });
});

staticDescribe("SidebarThreadRow direct rendering", () => {
  type ThreadRowProps = React.ComponentProps<typeof SidebarThreadRow>;

  function rowProps(
    thread: EnvironmentThreadShell,
    overrides: Partial<ThreadRowProps> = {},
  ): ThreadRowProps {
    return {
      thread,
      projectCwd: "C:/repo-a",
      orderedProjectThreadKeys: [threadKeyOf(thread)],
      isActive: false,
      jumpLabel: null,
      appSettingsConfirmThreadArchive: false,
      renamingThreadKey: null,
      renamingTitle: "",
      setRenamingTitle: vi.fn(),
      startThreadRename: vi.fn(),
      renamingInputRef: { current: null },
      renamingCommittedRef: { current: false },
      confirmingArchiveThreadKey: null,
      setConfirmingArchiveThreadKey: vi.fn(),
      confirmArchiveButtonRefs: { current: new Map<string, HTMLButtonElement>() },
      handleThreadClick: vi.fn(),
      navigateToThread: vi.fn(),
      handleMultiSelectContextMenu: vi.fn(async () => {}),
      handleThreadContextMenu: vi.fn(async () => {}),
      clearSelection: vi.fn(),
      commitRename: vi.fn(async () => {}),
      cancelRename: vi.fn(),
      attemptArchiveThread: vi.fn(async () => {}),
      openPrLink: vi.fn(),
      ...cardDataProps(thread),
      modelLabel: "gpt-5-codex",
      moreChatsCount: 0,
      moreChatsStatus: null,
      ...overrides,
    };
  }

  beforeEach(() => {
    h.state.environments = [
      environmentFixture({ environmentId: ENV_MAIN, label: "Main", connectionId: "primary" }),
      environmentFixture({ environmentId: ENV_REMOTE, label: null, connectionId: "remote" }),
      environmentFixture({ environmentId: ENV_WSL, label: "WSL", connectionId: "local:wsl" }),
    ];
    h.state.primaryEnvironmentId = ENV_MAIN;
    h.state.projects = [projectA];
  });

  it("renders unread and pinned markers with a jump label", () => {
    const thread = makeThread("thread-a");
    h.metaStore.setState({
      pinnedThreadKeys: [threadKeyOf(thread)],
      unreadThreadKeys: [threadKeyOf(thread)],
    });
    const markup = render(<SidebarThreadRow {...rowProps(thread, { jumpLabel: "⌘1" })} />);
    expect(markup).toContain('data-unread="true"');
    expect(markup).toContain(">unread, pinned</span>");
    expect(markup).toContain("thread-pinned-thread-a");
    expect(markup).toContain("⌘1");
  });

  it("uses summary for inactive rows and full status for the active row", () => {
    const thread = makeThread("thread-passive", {
      branch: "feature/passive",
      worktreePath: "C:/wt/passive",
    });
    render(<SidebarThreadRow {...rowProps(thread)} />);
    expect(h.state.vcsQueries.at(-1)?.__q).toBe("vcs.summary");

    h.state.vcsQueries = [];
    render(<SidebarThreadRow {...rowProps(thread, { isActive: true })} />);
    expect(h.state.vcsQueries.at(-1)?.__q).toBe("vcs.status");
  });

  it("folds failure and unresolved delivery into the glyph and the session line", () => {
    const failed = makeThread("thread-provider-failed", {
      branch: "feature/failure",
      worktreePath: "C:/wt/failure",
      session: {
        threadId: ThreadId.make("thread-provider-failed"),
        status: "error",
        providerName: "Claude Code",
        activeTurnId: null,
        lastError: "provider exited",
        updatedAt: iso(1),
        runtimeMode: "full-access",
      } as EnvironmentThreadShell["session"],
    });
    h.state.vcsStatusByCwd["C:/wt/failure"] = { refName: "feature/failure" };
    const failedMarkup = render(<SidebarThreadRow {...rowProps(failed)} />);
    expect(failedMarkup).toContain('aria-label="Failed"');
    expect(failedMarkup).not.toContain("Claude Code – Failed");

    const unresolved = makeThread("thread-delivery-uncertain", {
      branch: "feature/delivery",
      worktreePath: "C:/wt/delivery",
      unresolvedDelivery: { state: "uncertain" },
      session: {
        threadId: ThreadId.make("thread-delivery-uncertain"),
        status: "starting",
        providerName: "OpenCode",
        activeTurnId: null,
        lastError: null,
        updatedAt: iso(2),
        runtimeMode: "full-access",
      },
    } as never);
    h.state.vcsStatusByCwd["C:/wt/delivery"] = { refName: "feature/updated" };
    const unresolvedMarkup = render(<SidebarThreadRow {...rowProps(unresolved)} />);
    expect(unresolvedMarkup).toContain("Delivery uncertain");
    expect(unresolvedMarkup).toContain('aria-label="Connecting"');
    expect(h.state.vcsQueries.at(-1)?.__q).toBe("vcs.summary");
  });

  it("shows no environment icon on cards, even for remote threads", () => {
    const markup = render(
      <SidebarThreadRow
        {...rowProps(makeThread("thread-remote-b", { environmentId: ENV_REMOTE }))}
      />,
    );
    expect(markup).not.toContain('aria-label="Remote"');
    expect(markup).not.toContain("lucide-cloud");
  });

  it("shows a running terminal indicator", () => {
    h.state.runningTerminalIds = ["term-1", "term-2"];
    const thread = makeThread("thread-a");
    const markup = render(<SidebarThreadRow {...rowProps(thread)} />);
    expect(markup).toContain("2 terminal running");
  });

  it("shows Connecting for a starting session without a provider name", () => {
    const thread = makeThread("thread-a", {
      session: {
        threadId: ThreadId.make("thread-a"),
        status: "starting",
        providerName: null,
        activeTurnId: null,
        lastError: null,
        updatedAt: "not-a-date",
        runtimeMode: "full-access",
      },
    });
    const markup = render(<SidebarThreadRow {...rowProps(thread)} />);
    expect(markup).toContain('aria-label="Connecting"');
    expect(markup).not.toContain("Agent –");
  });

  it("hides the archive button while a turn is actively running", () => {
    const thread = makeThread("thread-a", {
      session: {
        threadId: ThreadId.make("thread-a"),
        status: "running",
        providerName: "Claude Code",
        activeTurnId: "turn-1",
        lastError: null,
        updatedAt: iso(1),
        runtimeMode: "full-access",
      } as EnvironmentThreadShell["session"],
    });
    const markup = render(<SidebarThreadRow {...rowProps(thread)} />);
    expect(markup).not.toContain("thread-archive-thread-a");
    expect(markup).toContain('aria-label="Working"');
  });

  it("renders the confirm-archive button and archives on confirm", async () => {
    const thread = makeThread("thread-a");
    const attemptArchiveThread = vi.fn(async () => {});
    const setConfirming = vi.fn();
    const props = rowProps(thread, {
      confirmingArchiveThreadKey: threadKeyOf(thread),
      setConfirmingArchiveThreadKey: setConfirming,
      attemptArchiveThread,
    });
    const markup = render(<SidebarThreadRow {...props} />);
    expect(markup).toContain("thread-archive-confirm-thread-a");

    const confirm = mustFindProps(byTestId("thread-archive-confirm-thread-a"), "confirm");
    const refCallback = confirm["ref"] as (element: unknown) => void;
    refCallback({ focus: vi.fn() });
    refCallback(null);
    invoke(confirm, "onClick", mouseEvent());
    await flush();
    expect(attemptArchiveThread).toHaveBeenCalled();
    expect(setConfirming).toHaveBeenCalled();

    // Row-level mouse leave clears the pending confirmation.
    const rowItem = captured("SidebarMenuSubItem")[0]!;
    invoke(rowItem.props, "onMouseLeave", mouseEvent());
    const blurEvent = {
      currentTarget: { contains: () => false },
    } as unknown as React.FocusEvent<HTMLLIElement>;
    invoke(rowItem.props, "onBlurCapture", blurEvent);
    const blurInside = {
      currentTarget: { contains: () => true },
    } as unknown as React.FocusEvent<HTMLLIElement>;
    invoke(rowItem.props, "onBlurCapture", blurInside);
  });

  it("starts the archive confirmation flow when confirmation is required", () => {
    const thread = makeThread("thread-a");
    const setConfirming = vi.fn();
    const props = rowProps(thread, {
      appSettingsConfirmThreadArchive: true,
      setConfirmingArchiveThreadKey: setConfirming,
    });
    const markup = render(<SidebarThreadRow {...props} />);
    // The captured-tree walk also sees unrendered slot props; prove the control renders.
    expect(markup).toContain('data-testid="thread-archive-thread-a"');
    const archive = mustFindProps(byTestId("thread-archive-thread-a"), "archive");
    invoke(archive, "onClick", mouseEvent());
    expect(setConfirming).toHaveBeenCalledWith(threadKeyOf(thread));
  });

  it("renders the inline rename input and wires its handlers", () => {
    const thread = makeThread("thread-a", { title: "Original title" });
    const setRenamingTitle = vi.fn();
    const commitRename = vi.fn(async () => {});
    const cancelRename = vi.fn();
    const renamingCommittedRef = { current: false };
    const props = rowProps(thread, {
      renamingThreadKey: threadKeyOf(thread),
      renamingTitle: "Edited title",
      setRenamingTitle,
      commitRename,
      cancelRename,
      renamingCommittedRef,
    });
    const markup = render(<SidebarThreadRow {...props} />);
    // The captured-tree walk also sees unrendered slot props; prove the input renders.
    expect(markup).toContain('value="Edited title"');

    const input = mustFindProps(
      (candidate) =>
        typeof candidate["onBlur"] === "function" && candidate["value"] === "Edited title",
      "rename input",
    );

    const focus = vi.fn();
    const select = vi.fn();
    (input["ref"] as (element: unknown) => void)({ focus, select });
    expect(focus).toHaveBeenCalled();
    expect(select).toHaveBeenCalled();

    invoke(input, "onChange", { target: { value: "New" } });
    expect(setRenamingTitle).toHaveBeenCalledWith("New");

    invoke(input, "onKeyDown", keyboardEvent("Enter"));
    expect(commitRename).toHaveBeenCalledWith(
      expect.objectContaining({ threadId: thread.id }),
      "Edited title",
      "Original title",
    );

    renamingCommittedRef.current = false;
    invoke(input, "onKeyDown", keyboardEvent("Escape"));
    expect(cancelRename).toHaveBeenCalled();

    renamingCommittedRef.current = false;
    commitRename.mockClear();
    invoke(input, "onBlur", {});
    expect(commitRename).toHaveBeenCalled();

    renamingCommittedRef.current = true;
    commitRename.mockClear();
    invoke(input, "onBlur", {});
    expect(commitRename).not.toHaveBeenCalled();

    const click = mouseEvent();
    invoke(input, "onClick", click);
    expect(click.stopPropagation).toHaveBeenCalled();
  });

  it("ignores a double-click while already renaming and on mobile", () => {
    const thread = makeThread("thread-a");
    const startThreadRename = vi.fn();
    const props = rowProps(thread, {
      renamingThreadKey: threadKeyOf(thread),
      renamingTitle: thread.title,
      startThreadRename,
    });
    render(<SidebarThreadRow {...props} />);
    const row = mustFindProps(byTestId("thread-row-thread-a"), "row");
    invoke(row, "onDoubleClick", mouseEvent());
    expect(startThreadRename).not.toHaveBeenCalled();

    h.state.isMobile = true;
    render(<SidebarThreadRow {...rowProps(thread, { startThreadRename })} />);
    const mobileRow = mustFindProps(byTestId("thread-row-thread-a"), "row");
    invoke(mobileRow, "onDoubleClick", mouseEvent());
    expect(startThreadRename).not.toHaveBeenCalled();
  });

  it("observes detached worktrees and shows their detached HEAD and dirty indicator", () => {
    const thread = makeThread("thread-detached", { branch: null, worktreePath: "C:/wt/detached" });
    h.state.vcsStatusByCwd["C:/wt/detached"] = {
      refName: null,
      detachedHead: "abc1234",
      hasWorkingTreeChanges: true,
    };
    const markup = render(<SidebarThreadRow {...rowProps(thread)} />);
    expect(h.state.vcsQueries.at(-1)?.args?.input?.cwd).toBe("C:/wt/detached");
    expect(markup).toContain(">abc1234<");
    expect(markup).toContain('aria-label="Uncommitted changes"');
  });

  it.each([false, true])(
    "never offers Archive for a running session with no active turn (approval=%s)",
    (approval) => {
      const thread = makeThread("running-no-turn", {
        hasPendingApprovals: approval,
        session: { ...threadActive.session!, status: "running", activeTurnId: null },
      });
      const markup = render(<SidebarThreadRow {...rowProps(thread)} />);
      expect(markup).not.toContain("thread-archive-running-no-turn");
      expect(markup).not.toContain("thread-archive-confirm-running-no-turn");
    },
  );

  it("omits line 2 when the branch equals the title and there are no indicators", () => {
    const thread = makeThread("same-title", {
      title: "feature/x",
      branch: "feature/x",
      worktreePath: "C:/wt/same",
    });
    h.state.vcsStatusByCwd["C:/wt/same"] = { refName: "feature/x" };
    const markup = render(<SidebarThreadRow {...rowProps(thread)} />);
    expect(markup).not.toContain("h-[18px]");
  });

  it("renders the worktree card's three lines and the more-chats row", () => {
    const thread = makeThread("thread-card", {
      title: "Fix invoice date format",
      branch: "fix-TRI-150",
      worktreePath: "C:/wt/fix",
      session: {
        threadId: ThreadId.make("thread-card"),
        status: "running",
        providerName: "claudeAgent",
        activeTurnId: "turn-1",
        lastError: null,
        updatedAt: iso(6),
        runtimeMode: "full-access",
      } as EnvironmentThreadShell["session"],
      latestTurn: {
        turnId: "turn-1",
        state: "running",
        requestedAt: iso(6),
        startedAt: iso(6),
        completedAt: null,
        assistantMessageId: null,
      } as EnvironmentThreadShell["latestTurn"],
      conversationPreview: {
        prompt: "Fix the export date format",
        tool: "Editing src/export/invoiceDates.ts",
        assistantMessage: null,
      },
    } as never);
    h.state.vcsStatusByCwd["C:/wt/fix"] = {
      refName: "fix-TRI-150",
      hasWorkingTreeChanges: true,
      pr: { url: "https://example.com/pr/57" },
    };
    h.state.runningTerminalIds = ["term-1"];
    const markup = render(
      <SidebarThreadRow
        {...rowProps(thread, {
          modelLabel: "opus",
          moreChatsCount: 2,
          moreChatsStatus: WORKSPACE_CARD_STATUS.done,
        })}
      />,
    );
    expect(markup).toContain('aria-label="Working"');
    expect(markup).toContain(">fix-TRI-150<");
    expect(markup).toContain("#1");
    expect(markup).toContain('aria-label="Uncommitted changes"');
    expect(markup).toContain('aria-label="1 terminal running"');
    expect(markup).toContain("Editing src/export/invoiceDates.ts");
    expect(markup).toContain(">opus<");
    expect(markup).toContain("2 more chats");
  });

  // A Git Manager checkout while the thread was closed: the recorded branch lags,
  // and the PR lookup failed, so the summary that names the new branch is stale.
  it("shows the PR and dirty state of the branch the card names, from a stale summary", () => {
    const thread = makeThread("thread-lagging", {
      title: "Lagging branch",
      branch: "main",
      worktreePath: "C:/wt/lagging",
    });
    h.state.vcsStatusByCwd["C:/wt/lagging"] = {
      refName: "feature",
      hasWorkingTreeChanges: true,
      pr: { url: "https://example.com/pr/58" },
      stale: true,
    };
    const markup = render(<SidebarThreadRow {...rowProps(thread)} />);
    expect(markup).toContain(">feature<");
    expect(markup).toContain("#1");
    expect(markup).toContain('aria-label="Uncommitted changes"');
  });

  it("names the card button by status, title and flags and describes it by its lines", () => {
    const thread = makeThread("thread-a11y", {
      title: "Upgrade PDF renderer",
      branch: "chore/pdf-renderer",
      worktreePath: "C:/wt/pdf",
    });
    h.state.vcsStatusByCwd["C:/wt/pdf"] = { refName: "chore/pdf-renderer" };
    h.metaStore.setState({
      unreadThreadKeys: [threadKeyOf(thread)],
      pinnedThreadKeys: [threadKeyOf(thread)],
    });
    const markup = render(<SidebarThreadRow {...rowProps(thread, { isActive: true })} />);
    const button = mustFindProps(byTestId("thread-card-button-thread-a11y"), "card button");
    const labelledBy = String(button["aria-labelledby"]).split(" ");
    expect(labelledBy).toHaveLength(3);
    for (const id of labelledBy) {
      expect(markup).toContain(`id="${id}"`);
    }
    expect(button["aria-current"]).toBe("page");
    expect(String(button["aria-describedby"]).split(" ")).toHaveLength(1);
  });
});

staticDescribe("project rename and grouping dialogs", () => {
  it("wires the rename dialog inputs and guards empty submits", async () => {
    baseScenario();
    render(<Sidebar />);

    const dialogs = captured("Dialog");
    expect(dialogs.length).toBeGreaterThanOrEqual(2);
    for (const dialog of dialogs) {
      const onOpenChange = dialog.props["onOpenChange"] as (open: boolean) => void;
      onOpenChange(false);
      onOpenChange(true);
    }

    const titleInput = mustFindProps(byAriaLabel("Project title"), "project title input");
    invoke(titleInput, "onChange", { target: { value: "Renamed" } });
    const enter = keyboardEvent("Enter");
    invoke(titleInput, "onKeyDown", enter);
    expect(enter.preventDefault).toHaveBeenCalled();
    await flush();

    // Rename target is null in a fresh render → submit early-returns.
    const saveButtons = captured("Button").filter((entry) => entry.props["children"] === "Save");
    for (const save of saveButtons) {
      invoke(save.props, "onClick", mouseEvent());
    }
    const cancelButtons = captured("Button").filter(
      (entry) => entry.props["children"] === "Cancel",
    );
    for (const cancel of cancelButtons) {
      invoke(cancel.props, "onClick", mouseEvent());
    }
    await flush();
  });

  it("validates the grouping selection values", () => {
    baseScenario();
    render(<Sidebar />);
    const select = captured("Select")[0]!;
    const onValueChange = select.props["onValueChange"] as (value: string) => void;
    onValueChange("repository");
    onValueChange("repository_path");
    onValueChange("separate");
    onValueChange("inherit");
    onValueChange("bogus");
  });

  it("describes each grouping mode", () => {
    baseScenario();
    h.state.clientSettings = {
      ...DEFAULT_CLIENT_SETTINGS,
      sidebarProjectGroupingMode: "repository_path",
    };
    let markup = render(<Sidebar />);
    expect(markup).toContain("repo-relative path");

    h.state.clientSettings = {
      ...DEFAULT_CLIENT_SETTINGS,
      sidebarProjectGroupingMode: "separate",
    };
    markup = render(<Sidebar />);
    expect(markup).toContain("own sidebar row");
  });
});
