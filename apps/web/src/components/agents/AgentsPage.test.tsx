// @vitest-environment happy-dom

import type { EnvironmentThreadShell } from "@bibcode/client-runtime/state/models";
import { EnvironmentId, ProjectId, ProviderInstanceId, ThreadId } from "@bibcode/contracts";
import { createModelSelection } from "@bibcode/shared/model";
import { act, type ComponentPropsWithoutRef, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const h = vi.hoisted(() => ({
  pathname: "/agents",
  shells: [] as EnvironmentThreadShell[],
  projects: [] as Array<{
    id: string;
    title: string;
    environmentId?: string;
    workspaceRoot?: string;
  }>,
  vcsSummaryByCwd: {} as Record<string, string>,
  environments: [] as Array<{ environmentId: string; label: string }>,
  availability: [] as Array<{ environmentId: string; status: string }>,
  unreadThreadKeys: [] as string[],
  groupExpandedById: {} as Record<string, boolean>,
  navigate: vi.fn(),
  routerNavigate: vi.fn(),
  historyCanGoBack: vi.fn(),
  historyBack: vi.fn(),
  markRead: vi.fn(),
  setActiveEnvironmentId: vi.fn(),
  setAgentsGroupExpanded: vi.fn(),
  reset() {
    h.pathname = "/agents";
    h.shells = [];
    h.projects = [];
    h.vcsSummaryByCwd = {};
    h.environments = [];
    h.availability = [];
    h.unreadThreadKeys = [];
    h.groupExpandedById = {};
    h.navigate.mockReset().mockResolvedValue(undefined);
    h.routerNavigate.mockReset().mockResolvedValue(undefined);
    h.historyCanGoBack.mockReset().mockReturnValue(false);
    h.historyBack.mockReset();
    h.markRead.mockReset().mockImplementation((key: string) => {
      h.unreadThreadKeys = h.unreadThreadKeys.filter((candidate) => candidate !== key);
    });
    h.setActiveEnvironmentId.mockReset();
    h.setAgentsGroupExpanded
      .mockReset()
      .mockImplementation((groupId: string, expanded: boolean) => {
        h.groupExpandedById = { ...h.groupExpandedById, [groupId]: expanded };
      });
  },
}));

vi.mock("@effect/atom-react", () => ({
  useAtomValue: () => [],
}));

vi.mock("@tanstack/react-router", () => ({
  useLocation: ({ select }: { select: (location: { pathname: string }) => string }) =>
    select({ pathname: h.pathname }),
  useNavigate: () => h.navigate,
  useRouter: () => ({
    history: {
      canGoBack: h.historyCanGoBack,
      back: h.historyBack,
    },
    navigate: h.routerNavigate,
  }),
}));

vi.mock("../../state/entities", () => ({
  useThreadShells: () => h.shells,
  useProjects: () => h.projects,
  setActiveEnvironmentId: h.setActiveEnvironmentId,
}));

// The hook reads each checkout's passive summary; here a summary is its ref name.
vi.mock("./useAgentLiveBranches", () => ({
  useAgentLiveBranches: (checkouts: ReadonlyArray<{ key: string; cwd: string }>) =>
    new Map(
      checkouts.flatMap((checkout) => {
        const refName = h.vcsSummaryByCwd[checkout.cwd];
        return typeof refName === "string" ? [[checkout.key, refName]] : [];
      }),
    ),
}));

vi.mock("../../state/environments", () => ({
  useEnvironments: () => ({ environments: h.environments }),
}));

vi.mock("../../state/shell", () => ({
  useEnvironmentShellSummary: () => ({ statuses: h.availability }),
}));

vi.mock("../../uiStateStore", () => ({
  resolveAgentsGroupExpanded: (expandedById: Record<string, boolean>, groupId: string) =>
    expandedById[groupId] ?? (groupId !== "done" && groupId !== "status:done"),
  useUiStateStore: (selector: (state: Record<string, unknown>) => unknown) =>
    selector({
      agentsGroupExpandedById: h.groupExpandedById,
      setAgentsGroupExpanded: h.setAgentsGroupExpanded,
    }),
}));

vi.mock("../../sidebarWorkspaceMetaStore", () => ({
  selectIsUnread: (unreadThreadKeys: readonly string[], key: string) =>
    unreadThreadKeys.includes(key),
  useSidebarWorkspaceMetaStore: (selector: (state: Record<string, unknown>) => unknown) =>
    selector({ unreadThreadKeys: h.unreadThreadKeys, markRead: h.markRead }),
}));

vi.mock("../ChatView", () => ({
  default: ({
    environmentId,
    threadId,
    routeKind,
  }: {
    environmentId: string;
    threadId: string;
    routeKind: string;
  }) => (
    <div
      data-testid="chat-view-stub"
      data-environment-id={environmentId}
      data-thread-id={threadId}
      data-route-kind={routeKind}
    />
  ),
}));

vi.mock("../Sidebar", () => ({
  default: () => <nav data-testid="thread-sidebar-mock" />,
}));

vi.mock("../sidebar/EnvironmentRail", () => ({
  EnvironmentRail: () => <nav data-testid="environment-rail-mock" />,
}));

vi.mock("../ui/sidebar", async (importOriginal) => ({
  SidebarInset: (await importOriginal<typeof import("../ui/sidebar")>()).SidebarInset,
  // AppSidebarLayout reads this to seed its initial width; this suite doesn't
  // exercise sidebar-width behavior, so simulate "nothing saved" statically.
  readStoredSidebarWidth: () => null,
  Sidebar: ({
    children,
    resizable: _resizable,
    ...props
  }: ComponentPropsWithoutRef<"aside"> & { resizable?: unknown }) => (
    <aside data-testid="app-sidebar-mock" {...props}>
      {children}
    </aside>
  ),
  SidebarProvider: ({
    children,
    defaultOpen: _defaultOpen,
    ...props
  }: ComponentPropsWithoutRef<"div"> & { defaultOpen?: boolean }) => (
    <div data-testid="sidebar-provider-mock" {...props}>
      {children}
    </div>
  ),
  SidebarRail: () => <div data-testid="sidebar-rail-mock" />,
  SidebarTrigger: (props: ComponentPropsWithoutRef<"button">) => (
    <button data-testid="sidebar-trigger-mock" {...props} />
  ),
  SidebarMenu: ({ children, ...props }: ComponentPropsWithoutRef<"ul">) => (
    <ul {...props}>{children}</ul>
  ),
  SidebarMenuItem: ({ children, ...props }: ComponentPropsWithoutRef<"li">) => (
    <li {...props}>{children}</li>
  ),
  SidebarMenuButton: ({
    isActive,
    size: _size,
    children,
    ...props
  }: ComponentPropsWithoutRef<"button"> & { isActive?: boolean; size?: string }) => (
    <button data-active={String(Boolean(isActive))} {...props}>
      {children}
    </button>
  ),
  SidebarMenuAction: ({
    showOnHover: _showOnHover,
    children,
    ...props
  }: ComponentPropsWithoutRef<"button"> & { showOnHover?: boolean }) => (
    <button {...props}>{children}</button>
  ),
  useSidebar: () => ({ toggleSidebar: vi.fn() }),
}));

vi.mock("../ui/tooltip", () => ({
  Tooltip: ({ children }: { children?: ReactNode }) => <>{children}</>,
  TooltipPopup: () => null,
  TooltipTrigger: ({ render }: { render?: ReactNode }) => <>{render}</>,
}));

vi.mock("../ui/select", () => {
  let onValueChange: ((value: string | null) => void) | undefined;
  return {
    Select: ({
      children,
      onValueChange: nextOnValueChange,
    }: {
      children?: ReactNode;
      onValueChange?: (value: string | null) => void;
    }) => {
      onValueChange = nextOnValueChange;
      return <div>{children}</div>;
    },
    SelectTrigger: ({ children, ...props }: ComponentPropsWithoutRef<"button">) => (
      <button {...props}>{children}</button>
    ),
    SelectValue: ({ children }: { children?: ReactNode }) => <span>{children}</span>,
    SelectPopup: ({ children }: { children?: ReactNode }) => <div>{children}</div>,
    SelectItem: ({ children, value }: { children?: ReactNode; value: string }) => (
      <button onClick={() => onValueChange?.(value)}>{children}</button>
    ),
  };
});

vi.mock("../ui/toggle", () => ({
  Toggle: ({
    children,
    pressed,
    onPressedChange,
    size: _size,
    variant: _variant,
    ...props
  }: ComponentPropsWithoutRef<"button"> & {
    pressed?: boolean;
    onPressedChange?: (pressed: boolean) => void;
    size?: string;
    variant?: string;
  }) => (
    <button aria-pressed={pressed} onClick={() => onPressedChange?.(!pressed)} {...props}>
      {children}
    </button>
  ),
}));

vi.mock("../ui/menu", () => ({
  DropdownMenu: ({ children }: { children?: ReactNode }) => <div>{children}</div>,
  DropdownMenuTrigger: ({ children, render }: { children?: ReactNode; render?: ReactNode }) => (
    <>
      {render}
      {children}
    </>
  ),
  DropdownMenuContent: ({ children }: { children?: ReactNode }) => <div>{children}</div>,
  DropdownMenuItem: ({ children, ...props }: ComponentPropsWithoutRef<"button">) => (
    <button {...props}>{children}</button>
  ),
}));

import { AppSidebarLayout } from "../AppSidebarLayout";
import { AgentsPage } from "./AgentsPage";

const ENVIRONMENT_LOCAL = EnvironmentId.make("environment-local");
const ENVIRONMENT_REMOTE = EnvironmentId.make("environment-remote");
const PROJECT_ALPHA = ProjectId.make("project-alpha");
const PROJECT_BETA = ProjectId.make("project-beta");
const UPDATED_AT = "2026-08-31T12:00:00.000Z";

type ThreadSession = NonNullable<EnvironmentThreadShell["session"]>;

function makeSession(overrides: Partial<ThreadSession> = {}): ThreadSession {
  return {
    threadId: ThreadId.make("thread-default"),
    status: "ready",
    providerName: "Codex",
    runtimeMode: "full-access",
    activeTurnId: null,
    lastError: null,
    updatedAt: UPDATED_AT,
    ...overrides,
  } as ThreadSession;
}

function makeShell(overrides: Partial<EnvironmentThreadShell> = {}): EnvironmentThreadShell {
  const id = overrides.id ?? ThreadId.make("thread-default");
  return {
    id,
    projectId: PROJECT_ALPHA,
    title: "Default agent",
    modelSelection: createModelSelection(ProviderInstanceId.make("codex"), "gpt-5-codex"),
    runtimeMode: "full-access",
    interactionMode: "default",
    branch: "main",
    worktreePath: null,
    latestTurn: null,
    createdAt: UPDATED_AT,
    updatedAt: UPDATED_AT,
    archivedAt: null,
    session: makeSession({ threadId: id }),
    latestUserMessageAt: UPDATED_AT,
    hasPendingApprovals: false,
    hasPendingUserInput: false,
    hasActionableProposedPlan: false,
    conversationPreview: {
      prompt: "Inspect the project",
      tool: null,
      assistantMessage: "Ready to continue",
    },
    environmentId: ENVIRONMENT_LOCAL,
    ...overrides,
  } as EnvironmentThreadShell;
}

interface MountedTree {
  readonly container: HTMLDivElement;
  readonly root: Root;
}

const mountedTrees: MountedTree[] = [];

async function mount(node: ReactNode): Promise<MountedTree> {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  const mounted = { container, root };
  mountedTrees.push(mounted);
  await act(async () => {
    root.render(node);
  });
  return mounted;
}

async function rerender(mounted: MountedTree, node: ReactNode): Promise<void> {
  await act(async () => {
    mounted.root.render(node);
    await Promise.resolve();
  });
}

async function click(element: Element): Promise<void> {
  await act(async () => {
    (element as HTMLElement).click();
  });
}

async function changeInput(input: HTMLInputElement, value: string): Promise<void> {
  await act(async () => {
    const valueSetter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
    valueSetter?.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
    await Promise.resolve();
  });
  await act(async () => Promise.resolve());
}

function getButtonByLabel(container: HTMLElement, label: string): HTMLButtonElement {
  const button = container.querySelector<HTMLButtonElement>(`button[aria-label="${label}"]`);
  if (button === null) throw new Error(`Missing button with aria-label=${label}`);
  return button;
}

function getButtonByText(container: HTMLElement, text: string): HTMLButtonElement {
  const button = Array.from(container.querySelectorAll<HTMLButtonElement>("button")).find(
    (candidate) => candidate.textContent === text,
  );
  if (button === undefined) throw new Error(`Missing button with text=${text}`);
  return button;
}

function queryAgentRow(container: HTMLElement, title: string, status = "Working", env = "Local") {
  return container.querySelector<HTMLButtonElement>(
    `button[aria-label="${title}, ${status}, ${env}"]`,
  );
}

function configureDefaultSources(): void {
  h.projects = [
    { id: PROJECT_ALPHA, title: "Project Alpha" },
    { id: PROJECT_BETA, title: "Project Beta" },
  ];
  h.environments = [
    { environmentId: ENVIRONMENT_LOCAL, label: "Local" },
    { environmentId: ENVIRONMENT_REMOTE, label: "Build farm" },
  ];
  h.availability = [
    { environmentId: ENVIRONMENT_LOCAL, status: "live" },
    { environmentId: ENVIRONMENT_REMOTE, status: "live" },
  ];
}

function workingShell(overrides: Partial<EnvironmentThreadShell> = {}): EnvironmentThreadShell {
  const id = overrides.id ?? ThreadId.make("thread-working");
  return makeShell({
    id,
    title: "Working agent",
    session: makeSession({ threadId: id, status: "running" }),
    ...overrides,
  });
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  h.reset();
  configureDefaultSources();
});

afterEach(async () => {
  await act(async () => {
    for (const mounted of mountedTrees.splice(0)) mounted.root.unmount();
  });
  document.body.replaceChildren();
  delete (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT;
});

describe("Agents page shell", () => {
  it("takes over the app layout only on /agents", async () => {
    const mounted = await mount(<AppSidebarLayout>Agents workspace</AppSidebarLayout>);

    expect(mounted.container.textContent).toContain("Agents workspace");
    expect(mounted.container.querySelector('[data-testid="app-sidebar-mock"]')).toBeNull();
    expect(mounted.container.querySelector('[data-testid="environment-rail-mock"]')).toBeNull();
    expect(mounted.container.querySelector('[data-testid="sidebar-rail-mock"]')).toBeNull();
    expect(mounted.container.querySelector('[data-testid="sidebar-trigger-mock"]')).toBeNull();

    h.pathname = "/";
    await rerender(mounted, <AppSidebarLayout>Normal workspace</AppSidebarLayout>);

    expect(mounted.container.querySelector('[data-testid="app-sidebar-mock"]')).not.toBeNull();
    expect(mounted.container.querySelector('[data-testid="environment-rail-mock"]')).not.toBeNull();
    expect(mounted.container.querySelector('[data-testid="sidebar-rail-mock"]')).not.toBeNull();
    expect(mounted.container.querySelector('[data-testid="sidebar-trigger-mock"]')).not.toBeNull();
  });
});

describe("AgentsPage", () => {
  it("uses router history for Back when an entry exists", async () => {
    h.historyCanGoBack.mockReturnValue(true);
    const { container } = await mount(<AgentsPage />);

    await click(getButtonByLabel(container, "Back"));

    expect(h.historyBack).toHaveBeenCalledOnce();
    expect(h.routerNavigate).not.toHaveBeenCalled();
  });

  it("falls back to the workspace root when Back has no history entry", async () => {
    const { container } = await mount(<AgentsPage />);

    await click(getButtonByLabel(container, "Back"));

    expect(h.historyBack).not.toHaveBeenCalled();
    expect(h.routerNavigate).toHaveBeenCalledExactlyOnceWith({ to: "/" });
  });

  it("counts unread agents from the full row set", async () => {
    h.shells = [workingShell({ id: ThreadId.make("thread-unread") })];
    h.unreadThreadKeys = ["environment-local:thread-unread", "environment-local:not-an-agent"];

    const { container } = await mount(<AgentsPage />);

    expect(container.textContent).toContain("1 unread");
  });

  it("marks every unread agent read from the overflow menu", async () => {
    h.shells = [
      workingShell({ id: ThreadId.make("thread-alpha"), title: "Alpha agent" }),
      workingShell({ id: ThreadId.make("thread-beta"), title: "Beta agent" }),
    ];
    h.unreadThreadKeys = ["environment-local:thread-alpha", "environment-local:thread-beta"];
    const mounted = await mount(<AgentsPage />);

    await click(getButtonByText(mounted.container, "Mark all read"));
    await rerender(mounted, <AgentsPage />);

    expect(h.markRead).toHaveBeenCalledTimes(2);
    expect(h.markRead).toHaveBeenNthCalledWith(1, "environment-local:thread-alpha");
    expect(h.markRead).toHaveBeenNthCalledWith(2, "environment-local:thread-beta");
    expect(mounted.container.textContent).toContain("0 unread");
  });

  it("selects an agent in place, mounts its ChatView, and marks it read", async () => {
    h.shells = [
      workingShell({
        id: ThreadId.make("thread-remote"),
        title: "Remote agent",
        environmentId: ENVIRONMENT_REMOTE,
      }),
    ];
    h.unreadThreadKeys = ["environment-remote:thread-remote"];
    const { container } = await mount(<AgentsPage />);

    const row = queryAgentRow(container, "Remote agent", "Working", "Build farm");
    expect(row).not.toBeNull();
    await click(row!);

    const chatView = container.querySelector<HTMLElement>('[data-testid="chat-view-stub"]');
    expect(chatView?.dataset.environmentId).toBe("environment-remote");
    expect(chatView?.dataset.threadId).toBe("thread-remote");
    expect(chatView?.dataset.routeKind).toBe("server");
    const chatInset = chatView?.closest<HTMLElement>('[data-slot="sidebar-inset"]');
    expect(chatInset?.className.split(/\s+/)).toEqual(
      expect.arrayContaining([
        "flex",
        "h-full",
        "min-h-0",
        "flex-1",
        "flex-col",
        "overflow-hidden",
      ]),
    );
    expect(h.markRead).toHaveBeenCalledExactlyOnceWith("environment-remote:thread-remote");
  });

  it("lays out a row as project eyebrow, branch, title, preview, and status footer", async () => {
    h.shells = [
      workingShell({
        id: ThreadId.make("thread-layout"),
        title: "Layout agent",
        branch: "feature/layout",
      }),
    ];
    const { container } = await mount(<AgentsPage />);

    const row = queryAgentRow(container, "Layout agent");
    expect(row).not.toBeNull();
    const lines = Array.from(row!.querySelectorAll<HTMLElement>("div.truncate")).map(
      (element) => element.textContent,
    );
    expect(lines.slice(0, 2)).toEqual(["feature/layout", "Layout agent"]);
    expect(row!.textContent).toContain("Project Alpha");
    expect(row!.textContent).toContain("Codex");
    expect(row!.textContent).toContain("Working");
    expect(row!.textContent).toContain("Local");
    expect(row!.className).not.toContain("opacity-");
  });

  // A Git Manager checkout moves the checkout without the thread's recorded branch;
  // the row names the live branch like the left panel, even from a stale summary.
  it("names the checkout's live branch over the thread's recorded one", async () => {
    h.shells = [
      workingShell({
        id: ThreadId.make("thread-live"),
        title: "Live agent",
        branch: "main",
        worktreePath: "/wt/live",
      }),
    ];
    h.vcsSummaryByCwd["/wt/live"] = "feature/live";
    const { container } = await mount(<AgentsPage />);

    const lines = Array.from(
      queryAgentRow(container, "Live agent")!.querySelectorAll<HTMLElement>("div.truncate"),
    ).map((element) => element.textContent);
    expect(lines.slice(0, 2)).toEqual(["feature/live", "Live agent"]);

    const input = container.querySelector<HTMLInputElement>('[data-testid="agents-filter-input"]');
    await changeInput(input!, "feature/live");
    expect(queryAgentRow(container, "Live agent")).not.toBeNull();
  });

  it("reads a local thread's branch from its project's checkout", async () => {
    h.projects = h.projects.map((project) =>
      project.id === PROJECT_ALPHA
        ? { ...project, environmentId: ENVIRONMENT_LOCAL, workspaceRoot: "/repo/alpha" }
        : project,
    );
    h.shells = [workingShell({ id: ThreadId.make("thread-local"), title: "Local agent" })];
    h.vcsSummaryByCwd["/repo/alpha"] = "develop";
    const { container } = await mount(<AgentsPage />);

    expect(queryAgentRow(container, "Local agent")!.textContent).toContain("develop");
  });

  it("keeps the recorded branch while the environment is not live", async () => {
    h.availability = [{ environmentId: ENVIRONMENT_LOCAL, status: "synchronizing" }];
    h.shells = [
      workingShell({
        id: ThreadId.make("thread-offline"),
        title: "Offline agent",
        branch: "main",
        worktreePath: "/wt/offline",
      }),
    ];
    h.vcsSummaryByCwd["/wt/offline"] = "feature/old";
    const { container } = await mount(<AgentsPage />);

    const row = queryAgentRow(container, "Offline agent", "Working", "Local");
    expect(row!.textContent).toContain("main");
    expect(row!.textContent).not.toContain("feature/old");
  });

  it("switches grouping from Status to Project", async () => {
    h.shells = [
      workingShell({ id: ThreadId.make("thread-alpha"), title: "Alpha agent" }),
      workingShell({
        id: ThreadId.make("thread-beta"),
        title: "Beta agent",
        projectId: PROJECT_BETA,
      }),
    ];
    const { container } = await mount(<AgentsPage />);

    expect(container.querySelector('[data-testid="agents-group-status:working"]')).not.toBeNull();
    await click(getButtonByText(container, "Project"));

    expect(container.querySelector('[data-testid="agents-group-status:working"]')).toBeNull();
    expect(
      container.querySelector('[data-testid="agents-group-project:Project Alpha"]'),
    ).not.toBeNull();
    expect(
      container.querySelector('[data-testid="agents-group-project:Project Beta"]'),
    ).not.toBeNull();
  });

  it("keeps the selected row visible when unread-only marks it read", async () => {
    h.shells = [workingShell({ id: ThreadId.make("thread-selected") })];
    h.unreadThreadKeys = ["environment-local:thread-selected"];
    const { container } = await mount(<AgentsPage />);

    await click(queryAgentRow(container, "Working agent")!);
    await click(getButtonByLabel(container, "Show unread only"));

    expect(queryAgentRow(container, "Working agent")).not.toBeNull();
    expect(container.querySelector('[data-testid="chat-view-stub"]')).not.toBeNull();
  });

  it("jumps to an agent workspace in mark-read, activate, navigate order", async () => {
    h.shells = [
      workingShell({
        id: ThreadId.make("thread-remote"),
        title: "Remote agent",
        environmentId: ENVIRONMENT_REMOTE,
      }),
    ];
    h.unreadThreadKeys = ["environment-remote:thread-remote"];
    const { container } = await mount(<AgentsPage />);

    await click(getButtonByLabel(container, "Jump to workspace for Remote agent"));

    expect(h.markRead).toHaveBeenCalledExactlyOnceWith("environment-remote:thread-remote");
    expect(h.setActiveEnvironmentId).toHaveBeenCalledExactlyOnceWith(ENVIRONMENT_REMOTE);
    expect(h.routerNavigate).toHaveBeenCalledExactlyOnceWith({
      to: "/$environmentId/$threadId",
      params: {
        environmentId: ENVIRONMENT_REMOTE,
        threadId: ThreadId.make("thread-remote"),
      },
    });
    expect(h.markRead.mock.invocationCallOrder[0]).toBeLessThan(
      h.setActiveEnvironmentId.mock.invocationCallOrder[0]!,
    );
    expect(h.setActiveEnvironmentId.mock.invocationCallOrder[0]).toBeLessThan(
      h.routerNavigate.mock.invocationCallOrder[0]!,
    );
  });

  it("uses the v1 filter semantics", async () => {
    h.shells = [
      workingShell({ id: ThreadId.make("thread-alpha"), title: "Alpha agent" }),
      workingShell({ id: ThreadId.make("thread-beta"), title: "Beta agent" }),
    ];
    const { container } = await mount(<AgentsPage />);
    const input = container.querySelector<HTMLInputElement>('[data-testid="agents-filter-input"]');
    expect(input).not.toBeNull();

    await changeInput(input!, "beta");

    expect(queryAgentRow(container, "Alpha agent")).toBeNull();
    expect(queryAgentRow(container, "Beta agent")).not.toBeNull();
  });

  it("clears selection when the agent leaves the full row set", async () => {
    h.shells = [workingShell({ id: ThreadId.make("thread-selected") })];
    const mounted = await mount(<AgentsPage />);
    await click(queryAgentRow(mounted.container, "Working agent")!);
    expect(mounted.container.querySelector('[data-testid="chat-view-stub"]')).not.toBeNull();

    h.shells = [];
    await rerender(mounted, <AgentsPage />);

    expect(mounted.container.querySelector('[data-testid="chat-view-stub"]')).toBeNull();
    expect(mounted.container.textContent).toContain("Select an agent to view its activity");
  });
});
