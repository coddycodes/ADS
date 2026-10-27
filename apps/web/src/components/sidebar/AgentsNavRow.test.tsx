// @vitest-environment happy-dom

import type { EnvironmentThreadShell } from "@coddy/client-runtime/state/models";
import { EnvironmentId, ProjectId, ProviderInstanceId, ThreadId, TurnId } from "@coddy/contracts";
import { createModelSelection } from "@coddy/shared/model";
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const h = vi.hoisted(() => ({
  shells: [] as EnvironmentThreadShell[],
  projects: [] as Array<{ id: string; title: string }>,
  environments: [] as Array<{ environmentId: string; label: string }>,
  availability: [] as Array<{ environmentId: string; status: string }>,
  unreadThreadKeys: [] as string[],
  pathname: "/",
  router: { state: { matches: [{ params: {} }] } },
  navigate: vi.fn(),
  markUnread: vi.fn(),
  reset() {
    h.shells = [];
    h.projects = [];
    h.environments = [];
    h.availability = [];
    h.unreadThreadKeys = [];
    h.pathname = "/";
    h.router.state.matches = [{ params: {} }];
    h.navigate.mockReset();
    h.markUnread.mockReset();
  },
}));

vi.mock("@tanstack/react-router", () => ({
  useLocation: (options?: { select?: (location: { pathname: string }) => unknown }) =>
    options?.select ? options.select({ pathname: h.pathname }) : { pathname: h.pathname },
  useNavigate: () => h.navigate,
  useRouter: () => h.router,
}));

vi.mock("../../state/entities", () => ({
  useThreadShells: () => h.shells,
  useProjects: () => h.projects,
}));

vi.mock("../../state/environments", () => ({
  useEnvironments: () => ({ environments: h.environments }),
}));

vi.mock("../../state/shell", () => ({
  useEnvironmentShellSummary: () => ({ statuses: h.availability }),
}));

vi.mock("../../sidebarWorkspaceMetaStore", () => ({
  selectIsUnread: (unreadThreadKeys: readonly string[], key: string) =>
    unreadThreadKeys.includes(key),
  useSidebarWorkspaceMetaStore: (selector: (state: Record<string, unknown>) => unknown) =>
    selector({ unreadThreadKeys: h.unreadThreadKeys, markUnread: h.markUnread }),
}));

vi.mock("../ui/sidebar", () => ({
  SidebarGroup: ({ children, ...props }: { children?: ReactNode }) => (
    <section {...props}>{children}</section>
  ),
  SidebarMenu: ({ children, ...props }: { children?: ReactNode }) => (
    <div {...props}>{children}</div>
  ),
  SidebarMenuItem: ({ children, ...props }: { children?: ReactNode }) => (
    <div {...props}>{children}</div>
  ),
  SidebarMenuButton: ({
    children,
    size: _size,
    isActive: _isActive,
    tooltip: _tooltip,
    ...props
  }: {
    children?: ReactNode;
    size?: string;
    isActive?: boolean;
    tooltip?: unknown;
  }) => <button {...props}>{children}</button>,
}));

import { AgentsNavRow } from "./AgentsNavRow";

const ENVIRONMENT_LOCAL = EnvironmentId.make("environment-local");
const PROJECT_ALPHA = ProjectId.make("project-alpha");
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
    conversationPreview: null,
    environmentId: ENVIRONMENT_LOCAL,
    ...overrides,
  } as EnvironmentThreadShell;
}

interface MountedNavRow {
  readonly container: HTMLDivElement;
  readonly root: Root;
}

const mountedNavRows: MountedNavRow[] = [];

async function mountNavRow(): Promise<MountedNavRow> {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  const mounted = { container, root };
  mountedNavRows.push(mounted);
  await act(async () => {
    root.render(<AgentsNavRow />);
  });
  return mounted;
}

async function rerenderNavRow(mounted: MountedNavRow): Promise<void> {
  await act(async () => {
    mounted.root.render(<AgentsNavRow />);
  });
}

function getNavRow(container: HTMLElement): HTMLButtonElement {
  const row = container.querySelector<HTMLButtonElement>('[data-testid="agents-nav-row"]');
  if (row === null) throw new Error("Missing Agents nav row");
  return row;
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  h.reset();
  h.projects = [{ id: PROJECT_ALPHA, title: "Project Alpha" }];
  h.environments = [{ environmentId: ENVIRONMENT_LOCAL, label: "Local" }];
  h.availability = [{ environmentId: ENVIRONMENT_LOCAL, status: "live" }];
});

afterEach(async () => {
  for (const mounted of mountedNavRows.splice(0)) {
    await act(async () => mounted.root.unmount());
    mounted.container.remove();
  }
  document.body.replaceChildren();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

describe("AgentsNavRow", () => {
  it("shows the unread count for the full agent row set", async () => {
    h.shells = [
      makeShell({ id: ThreadId.make("thread-unread-a") }),
      makeShell({ id: ThreadId.make("thread-read") }),
      makeShell({ id: ThreadId.make("thread-unread-b") }),
    ];
    h.unreadThreadKeys = [
      "environment-local:thread-unread-a",
      "environment-local:not-an-agent",
      "environment-local:thread-unread-b",
    ];

    const { container } = await mountNavRow();

    expect(getNavRow(container).textContent).toBe("Agents2");
  });

  it("hides the unread badge when nothing is unread", async () => {
    h.shells = [makeShell({ id: ThreadId.make("thread-read") })];
    h.unreadThreadKeys = [];

    const { container } = await mountNavRow();

    expect(getNavRow(container).textContent).toBe("Agents");
  });

  it("navigates to the full Agents view when clicked", async () => {
    const { container } = await mountNavRow();

    await act(async () => getNavRow(container).click());

    expect(h.navigate).toHaveBeenCalledExactlyOnceWith({ to: "/agents" });
  });

  it("sets aria-current only on the Agents route", async () => {
    h.pathname = "/agents";
    const mounted = await mountNavRow();
    expect(getNavRow(mounted.container).getAttribute("aria-current")).toBe("page");

    h.pathname = "/";
    await rerenderNavRow(mounted);
    expect(getNavRow(mounted.container).getAttribute("aria-current")).toBeNull();
  });

  it("marks a settling turn unread while mounted on a normal route", async () => {
    const threadId = ThreadId.make("thread-settling");
    const runningTurn = {
      turnId: TurnId.make("turn-settling"),
      state: "running" as const,
      requestedAt: UPDATED_AT,
      startedAt: UPDATED_AT,
      completedAt: null,
      assistantMessageId: null,
    };
    h.pathname = "/";
    h.shells = [makeShell({ id: threadId, latestTurn: runningTurn })];
    const mounted = await mountNavRow();
    expect(h.markUnread).not.toHaveBeenCalled();

    h.shells = [
      makeShell({
        id: threadId,
        latestTurn: { ...runningTurn, state: "completed", completedAt: UPDATED_AT },
      }),
    ];
    await rerenderNavRow(mounted);

    expect(h.markUnread).toHaveBeenCalledExactlyOnceWith("environment-local:thread-settling");
  });
});
