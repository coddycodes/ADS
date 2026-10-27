// @vitest-environment happy-dom
/**
 * Mounted Sidebar tests (happy-dom): real DOM events, focus and the real
 * fallback menu. The static-rendering suite is `Sidebar.test.tsx`; both share
 * `Sidebar.testHarness.tsx`.
 */
import {
  ENV_MAIN,
  ENV_REMOTE,
  baseScenario,
  captured,
  cardDataProps,
  environmentFixture,
  fakeLocalApi,
  flush,
  groupedScenario,
  h,
  makeProject,
  makeThread,
  projectA,
  threadActive,
  threadKeyOf,
} from "./Sidebar.testHarness";
import { createRoot, type Root } from "react-dom/client";
import * as React from "react";
import { beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { ProjectId, WorktreeKey, WorktreeRepositoryKey } from "@coddy/contracts";
import { scopeProjectRef } from "@coddy/client-runtime/environment";
import { derivePhysicalProjectKey } from "../logicalProject";
import type { EnvironmentThreadShell } from "@coddy/client-runtime/state/shell";
import Sidebar, { SidebarThreadRow } from "./Sidebar";
import { showContextMenuFallback } from "../contextMenuFallback";

describe("SidebarThreadRow browser interactions", () => {
  type ThreadRowProps = React.ComponentProps<typeof SidebarThreadRow>;

  beforeEach(() => {
    h.state.environments = [
      environmentFixture({ environmentId: ENV_MAIN, label: "Main", connectionId: "primary" }),
    ];
    h.state.primaryEnvironmentId = ENV_MAIN;
    h.state.projects = [projectA];
    (
      globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }
    ).IS_REACT_ACT_ENVIRONMENT = true;
  });

  function requiredElement<T extends Element>(container: ParentNode, selector: string): T {
    const element = container.querySelector<T>(selector);
    if (!element) throw new Error(`Missing DOM element: ${selector}`);
    return element;
  }

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

  async function mount(element: React.ReactElement): Promise<{
    container: HTMLDivElement;
    root: Root;
  }> {
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);
    await React.act(async () => {
      root.render(element);
    });
    return { container, root };
  }

  async function dispatch(element: Element, event: Event): Promise<void> {
    await React.act(async () => {
      element.dispatchEvent(event);
    });
  }

  async function nextFrame(): Promise<void> {
    await React.act(
      () =>
        new Promise<void>((resolve) => {
          requestAnimationFrame(() => resolve());
        }),
    );
  }

  async function unmount(root: Root, container: HTMLElement): Promise<void> {
    await React.act(async () => root.unmount());
    container.remove();
  }

  it.each(["thread-card-button-thread-idle", "primary-card-button-project-a"])(
    "keeps one real fallback menu open after the delayed keyboard echo on %s",
    async (testId) => {
      baseScenario();
      fakeLocalApi();
      h.spies.contextMenuShow.mockImplementation(showContextMenuFallback);
      const now = vi.spyOn(performance, "now").mockReturnValue(10_000);
      const { container, root } = await mount(<Sidebar />);
      try {
        const button = requiredElement<HTMLButtonElement>(container, `[data-testid='${testId}']`);
        button.focus();
        await dispatch(
          button,
          new KeyboardEvent("keydown", {
            key: "F10",
            shiftKey: true,
            bubbles: true,
            cancelable: true,
          }),
        );
        await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
        const menu = document.querySelector('[role="menu"]');
        expect(menu).not.toBeNull();
        now.mockReturnValue(10_500);
        await dispatch(
          button,
          new PointerEvent("pointerdown", { button: 2, bubbles: true, cancelable: true }),
        );
        await dispatch(button, new MouseEvent("contextmenu", { bubbles: true, cancelable: true }));
        expect(document.querySelectorAll('[role="menu"]')).toHaveLength(1);
        expect(document.querySelector('[role="menu"]')).toBe(menu);
        expect(h.spies.contextMenuShow).toHaveBeenCalledTimes(1);
        await React.act(async () => {
          document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
        });
        expect(document.activeElement).toBe(button);
        expect(document.querySelectorAll('[role="menu"]')).toHaveLength(0);
      } finally {
        document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
        now.mockRestore();
        await unmount(root, container);
      }
    },
  );

  it("explains the disabled Delete Worktree in the real menu while the worktree's session runs", async () => {
    const reason = "Stop the running session before deleting this worktree.";
    baseScenario();
    fakeLocalApi();
    h.spies.contextMenuShow.mockImplementation(showContextMenuFallback);
    const { container, root } = await mount(<Sidebar />);
    try {
      const card = requiredElement<HTMLLIElement>(
        container,
        "[data-testid='thread-row-thread-active']",
      );
      await dispatch(
        card,
        new MouseEvent("contextmenu", {
          bubbles: true,
          cancelable: true,
          clientX: 24,
          clientY: 48,
        }),
      );
      await nextFrame();
      const deleteItem = [
        ...document.querySelectorAll<HTMLButtonElement>('[role="menu"] [role="menuitem"]'),
      ].find((item) => item.textContent?.startsWith("Delete Worktree…"));
      expect(deleteItem).toBeDefined();
      expect(deleteItem!.hasAttribute("disabled")).toBe(false);
      expect(deleteItem!.getAttribute("aria-disabled")).toBe("true");
      expect(deleteItem!.getAttribute("aria-description")).toBe(reason);
      expect(deleteItem!.textContent).toBe(`Delete Worktree…${reason}`);
    } finally {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
      await unmount(root, container);
    }
  });

  it("refuses a Delete Worktree chosen after a session started while the menu was open", async () => {
    const readyWorktree: EnvironmentThreadShell = {
      ...threadActive,
      session: { ...threadActive.session!, status: "ready" },
    };
    const withWorktree = (worktree: EnvironmentThreadShell) =>
      (h.state.threads as EnvironmentThreadShell[]).map((thread) =>
        thread.id === worktree.id ? worktree : thread,
      );
    baseScenario();
    h.state.threads = withWorktree(readyWorktree);
    fakeLocalApi();
    let deleteItem: { disabled?: boolean } | undefined;
    let choose: (id: string | null) => void = () => {};
    h.spies.contextMenuShow.mockImplementation(
      (items: Array<{ id?: string; disabled?: boolean }>) =>
        new Promise<string | null>((resolve) => {
          deleteItem = items.find((item) => item.id === "delete");
          choose = resolve;
        }),
    );
    const { container, root } = await mount(<Sidebar />);
    try {
      const card = requiredElement<HTMLLIElement>(
        container,
        "[data-testid='thread-row-thread-active']",
      );
      const cardStatus = () => card.querySelector("[data-status]")?.getAttribute("data-status");
      await dispatch(card, new MouseEvent("contextmenu", { bubbles: true, cancelable: true }));
      expect(deleteItem?.disabled).not.toBe(true);
      expect(cardStatus()).not.toBe("working");

      // A session starts while the menu is open. The store replaces the thread
      // with a new object, never touching the one the menu was built from, and
      // the sidebar re-renders with it (`root.render` stands in for the store
      // subscription, which the harness mocks away).
      h.state.threads = withWorktree({
        ...readyWorktree,
        session: { ...readyWorktree.session!, status: "running" },
      });
      await React.act(async () => root.render(<Sidebar />));
      expect(cardStatus()).toBe("working");

      await React.act(async () => {
        choose("delete");
        await flush();
      });

      expect(h.spies.requestWorktreeRemoval).not.toHaveBeenCalled();
      expect(h.spies.deleteThread).not.toHaveBeenCalled();
      expect(h.spies.dialogConfirm).not.toHaveBeenCalled();
      expect(h.spies.toastAdd).toHaveBeenCalledWith(
        expect.objectContaining({
          type: "warning",
          title: "Worktree not deleted",
          description: "Stop the running session before deleting this worktree.",
        }),
      );
    } finally {
      await unmount(root, container);
    }
  });

  it("opens the dialog for the clicked worktree row project", async () => {
    const projectB = makeProject("project-browser-b", {
      title: "Repo B",
      workspaceRoot: "C:/repo-b",
    });
    baseScenario();
    h.state.projects = [projectA, projectB];
    h.state.sidebarCtx = { isMobile: true, setOpenMobile: h.spies.setOpenMobile };
    const { container, root } = await mount(<Sidebar />);

    h.state.captures.length = 0;
    const worktree = requiredElement<HTMLButtonElement>(
      container,
      "[aria-label='New worktree in Repo B']",
    );
    const click = new MouseEvent("click", { bubbles: true, cancelable: true });
    await dispatch(worktree, click);

    expect(click.defaultPrevented).toBe(true);
    expect(h.spies.setOpenMobile).toHaveBeenCalledWith(false);
    expect(captured("CreateWorktreeDialog").at(-1)?.props["defaultProjectRef"]).toEqual(
      scopeProjectRef(projectB.environmentId, projectB.id),
    );
    await unmount(root, container);
  });

  it("opens the dialog for the remote member chosen from a grouped worktree row", async () => {
    groupedScenario();
    fakeLocalApi();
    h.spies.contextMenuShow.mockImplementation(
      async (items: Array<{ id: string }>) => items[1]!.id,
    );
    const { container, root } = await mount(<Sidebar />);

    h.state.captures.length = 0;
    const worktree = requiredElement<HTMLButtonElement>(
      container,
      "[data-testid='new-worktree-button']",
    );
    await dispatch(worktree, new MouseEvent("click", { bubbles: true, cancelable: true }));
    await React.act(async () => flush());

    expect(h.spies.contextMenuShow).toHaveBeenCalled();
    expect(captured("CreateWorktreeDialog").at(-1)?.props["defaultProjectRef"]).toEqual(
      scopeProjectRef(ENV_REMOTE, ProjectId.make("project-a-remote")),
    );
    await unmount(root, container);
  });

  it("starts worktree creation from New Worktree… for the selected project", async () => {
    baseScenario();
    h.state.sidebarCtx = { isMobile: true, setOpenMobile: h.spies.setOpenMobile };
    fakeLocalApi();
    const { container, root } = await mount(<Sidebar />);
    h.spies.contextMenuShow.mockResolvedValue(`new-worktree:${derivePhysicalProjectKey(projectA)}`);
    try {
      const actions = requiredElement<HTMLButtonElement>(
        container,
        '[data-testid="project-actions-button"]',
      );
      await dispatch(actions, new MouseEvent("click", { bubbles: true, cancelable: true }));
      await React.act(async () => flush());
      expect(h.spies.setOpenMobile).toHaveBeenCalledWith(false);
      expect(captured("CreateWorktreeDialog").at(-1)?.props).toMatchObject({
        open: true,
        defaultProjectRef: scopeProjectRef(projectA.environmentId, projectA.id),
      });
    } finally {
      await unmount(root, container);
    }
  });

  it("starts inline rename only for an unmodified row-body double-click", async () => {
    const thread = makeThread("thread-browser-rename", { title: "Rename me" });
    const startThreadRename = vi.fn();

    function Harness() {
      const [renamingThreadKey, setRenamingThreadKey] = React.useState<string | null>(null);
      const [renamingTitle, setRenamingTitle] = React.useState("");
      const renamingInputRef = React.useRef<HTMLInputElement | null>(null);
      const renamingCommittedRef = React.useRef(false);
      const beginRename = React.useCallback((threadKey: string, title: string) => {
        startThreadRename(threadKey, title);
        setRenamingThreadKey(threadKey);
        setRenamingTitle(title);
      }, []);
      return (
        <SidebarThreadRow
          {...rowProps(thread, {
            renamingThreadKey,
            renamingTitle,
            setRenamingTitle,
            startThreadRename: beginRename,
          })}
          renamingInputRef={renamingInputRef}
          renamingCommittedRef={renamingCommittedRef}
        />
      );
    }

    const { container, root } = await mount(<Harness />);
    const row = requiredElement<HTMLElement>(
      container,
      "[data-testid='thread-row-thread-browser-rename']",
    );
    for (const modifier of ["metaKey", "ctrlKey", "shiftKey", "altKey"] as const) {
      await dispatch(
        row,
        new MouseEvent("dblclick", { bubbles: true, cancelable: true, [modifier]: true }),
      );
      expect(startThreadRename, `${modifier} must not start rename`).not.toHaveBeenCalled();
      expect(container.querySelector("input")).toBeNull();
    }

    const archive = requiredElement<HTMLButtonElement>(
      container,
      "[data-testid='thread-archive-thread-browser-rename']",
    );
    await dispatch(archive, new MouseEvent("dblclick", { bubbles: true, cancelable: true }));
    expect(startThreadRename, "nested controls must not start rename").not.toHaveBeenCalled();
    expect(container.querySelector("input")).toBeNull();

    await dispatch(row, new MouseEvent("dblclick", { bubbles: true, cancelable: true }));
    expect(startThreadRename).toHaveBeenCalledOnce();
    expect(startThreadRename).toHaveBeenCalledWith(threadKeyOf(thread), "Rename me");
    const input = requiredElement<HTMLInputElement>(container, "input");
    expect(input.value).toBe("Rename me");
    expect(document.activeElement).toBe(input);
    await unmount(root, container);
  });

  it("retains archive confirmation for focus inside the row and clears it after focus leaves", async () => {
    const thread = makeThread("thread-browser-archive", { title: "Archive me" });

    function Harness() {
      const [confirmingArchiveThreadKey, setConfirmingArchiveThreadKey] = React.useState<
        string | null
      >(null);
      const confirmArchiveButtonRefs = React.useRef(new Map<string, HTMLButtonElement>());
      return (
        <SidebarThreadRow
          {...rowProps(thread, {
            appSettingsConfirmThreadArchive: true,
            confirmingArchiveThreadKey,
            setConfirmingArchiveThreadKey,
          })}
          confirmArchiveButtonRefs={confirmArchiveButtonRefs}
        />
      );
    }

    const outside = document.createElement("button");
    outside.textContent = "Outside";
    document.body.append(outside);
    const { container, root } = await mount(<Harness />);
    const archive = requiredElement<HTMLButtonElement>(
      container,
      "[data-testid='thread-archive-thread-browser-archive']",
    );
    await dispatch(archive, new MouseEvent("click", { bubbles: true, cancelable: true }));
    await nextFrame();

    const confirmSelector = "[data-testid='thread-archive-confirm-thread-browser-archive']";
    const confirm = requiredElement<HTMLButtonElement>(container, confirmSelector);
    expect(confirm.textContent).toBe("Confirm");
    expect(document.activeElement).toBe(confirm);

    const row = requiredElement<HTMLElement>(
      container,
      "[data-testid='thread-card-button-thread-browser-archive']",
    );
    await React.act(async () => row.focus());
    await nextFrame();
    expect(requiredElement<HTMLButtonElement>(container, confirmSelector).textContent).toBe(
      "Confirm",
    );

    await React.act(async () => outside.focus());
    await nextFrame();
    expect(container.querySelector(confirmSelector)).toBeNull();

    await unmount(root, container);
    outside.remove();
  });

  it.each([true, false])(
    "shows discovered worktrees only while the project is expanded (expanded=%s)",
    async (expanded) => {
      baseScenario();
      h.state.serverConfigs = new Map([
        [ENV_MAIN, { environment: { capabilities: { worktreeCatalog: true } } }],
      ]);
      h.state.worktreeCatalogs.set(`${ENV_MAIN}:${projectA.id}`, {
        repositoryKey: WorktreeRepositoryKey.make("repository:C:/worktrees/discovered"),
        generation: 1,
        authoritative: true,
        observedAt: "2026-08-09T12:00:00.000Z",
        scanStatus: { _tag: "ready" },
        worktrees: [
          {
            worktreeKey: WorktreeKey.make("worktree:C:/worktrees/discovered"),
            path: "C:/worktrees/discovered",
            branch: "feature/discovered",
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
      });
      // Routed to one of the project's threads: collapsing must still hide discovery.
      h.uiStore.setState({
        projectExpandedById: { [derivePhysicalProjectKey(projectA)]: expanded },
      });
      const { container, root } = await mount(<Sidebar />);
      try {
        await React.act(async () => flush());
        expect(container.querySelector('[data-testid^="worktree-discovery-card-"]') !== null).toBe(
          expanded,
        );
      } finally {
        await unmount(root, container);
      }
    },
  );

  it.each([
    { expanded: true, label: "Show Hidden Worktrees (1)" },
    { expanded: false, label: "Show Hidden Worktrees" },
  ])(
    "labels the hidden-worktree count only on an expanded project (expanded=$expanded)",
    async ({ expanded, label }) => {
      baseScenario();
      h.state.serverConfigs = new Map([
        [ENV_MAIN, { environment: { capabilities: { worktreeCatalog: true } } }],
      ]);
      h.state.worktreeCatalogs.set(`${ENV_MAIN}:${projectA.id}`, {
        repositoryKey: WorktreeRepositoryKey.make("repository:C:/worktrees/hidden"),
        generation: 1,
        authoritative: true,
        observedAt: "2026-08-09T12:00:00.000Z",
        scanStatus: { _tag: "ready" },
        worktrees: [
          {
            worktreeKey: WorktreeKey.make("worktree:C:/worktrees/hidden"),
            path: "C:/worktrees/hidden",
            branch: "feature/hidden",
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
      });
      // Only an expanded project mounts discovery, so only it reports a count.
      h.uiStore.setState({
        projectExpandedById: { [derivePhysicalProjectKey(projectA)]: expanded },
      });
      fakeLocalApi();
      let labels: string[] = [];
      h.spies.contextMenuShow.mockImplementation(async (items: Array<{ label?: string }>) => {
        labels = items.map((item) => item.label ?? "---");
        return null;
      });
      const { container, root } = await mount(<Sidebar />);
      try {
        await React.act(async () => flush());
        const actions = requiredElement<HTMLButtonElement>(
          container,
          '[data-testid="project-actions-button"]',
        );
        await dispatch(actions, new MouseEvent("click", { bubbles: true, cancelable: true }));
        await React.act(async () => flush());
        expect(labels).toContain(label);
      } finally {
        await unmount(root, container);
      }
    },
  );
});
