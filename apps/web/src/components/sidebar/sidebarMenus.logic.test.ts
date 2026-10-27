import type { ContextMenuEntry } from "@coddy/contracts";
import { describe, expect, it } from "vite-plus/test";

import {
  buildMultiSelectMenu,
  buildPrimaryCardMenu,
  buildProjectHeaderMenu,
  buildWorkspaceCardMenu,
  describeUnavailableWorkspace,
  parseProjectHeaderSelection,
  WORKTREE_DELETE_BLOCKED_REASON,
  type WorkspaceCardMenuInput,
} from "./sidebarMenus.logic";

function outline(entries: readonly ContextMenuEntry<string>[]): string[] {
  return entries.map((entry) => ("separator" in entry ? "---" : entry.label));
}

function ids(entries: readonly ContextMenuEntry<string>[]): string[] {
  return entries.map((entry) => ("separator" in entry ? "---" : entry.id));
}

const openIn = [
  { id: "open-in:file-explorer", label: "File Explorer" },
  { id: "open-in:vscode", label: "VS Code" },
];

const worktreeCard: WorkspaceCardMenuInput = {
  isWorktree: true,
  openIn,
  pullDisabledReason: null,
  workspaceUnavailableReason: null,
  branchName: "fix-TRI-150",
  pinned: false,
  unread: false,
  confirmThreadDelete: true,
  worktreeSessionRunning: false,
};

describe("buildWorkspaceCardMenu", () => {
  it("groups a worktree card's items as in Menus.dc.html", () => {
    const menu = buildWorkspaceCardMenu(worktreeCard);
    expect(outline(menu)).toEqual([
      "Open in",
      "Pull",
      "---",
      "Copy Path",
      "Copy Branch Name",
      "Copy Thread ID",
      "---",
      "Pin",
      "Mark as Unread",
      "Rename…",
      "---",
      "Delete Worktree…",
    ]);
    expect(ids(menu)).toEqual([
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
    expect(menu.at(-1)).toMatchObject({ destructive: true, icon: "trash" });
    expect(menu.at(-1)).not.toHaveProperty("disabled");
    expect(menu[0]).toMatchObject({ children: openIn });
  });

  it("disables Delete Worktree in place, with how to proceed, while a session in the worktree runs", () => {
    expect(WORKTREE_DELETE_BLOCKED_REASON).toBe(
      "Stop the running session before deleting this worktree.",
    );
    const menu = buildWorkspaceCardMenu({ ...worktreeCard, worktreeSessionRunning: true });
    expect(ids(menu)).toEqual(ids(buildWorkspaceCardMenu(worktreeCard)));
    expect(menu.at(-1)).toEqual({
      id: "delete",
      label: "Delete Worktree…",
      destructive: true,
      icon: "trash",
      disabled: true,
      description: WORKTREE_DELETE_BLOCKED_REASON,
    });
    expect(menu.filter((entry) => "disabled" in entry && entry.disabled === true)).toHaveLength(1);
  });

  it("keeps Delete Thread enabled for a running thread without a worktree, which stops its session first", () => {
    const menu = buildWorkspaceCardMenu({
      ...worktreeCard,
      isWorktree: false,
      worktreeSessionRunning: true,
    });
    expect(menu.at(-1)).toEqual({
      id: "delete",
      label: "Delete Thread…",
      destructive: true,
      icon: "trash",
    });
  });

  it("omits Copy Branch Name without a branch and reflects pin, read and pull state", () => {
    const menu = buildWorkspaceCardMenu({
      ...worktreeCard,
      branchName: null,
      pinned: true,
      unread: true,
      pullDisabledReason: "Workspace is unavailable.",
      openIn: [],
    });
    expect(outline(menu)).not.toContain("Copy Branch Name");
    expect(outline(menu)).toContain("Unpin");
    expect(outline(menu)).toContain("Mark as Read");
    expect(menu.find((entry) => "id" in entry && entry.id === "pull")).toMatchObject({
      disabled: true,
      description: "Workspace is unavailable.",
    });
    expect(menu[0]).toEqual({
      id: "open-in",
      label: "Open in",
      disabled: true,
      description: "No local opener is available for this workspace.",
    });
  });

  it("explains a missing worktree on Open in and Pull with its recovery", () => {
    const reason = describeUnavailableWorkspace("missing-registered");
    expect(reason).toBe(
      "The worktree directory is missing. Use Retry detection or Remove from Coddy Studio on its card.",
    );
    expect(describeUnavailableWorkspace("missing-unregistered")).toBe(reason);
    expect(describeUnavailableWorkspace("removing")).toBe("This worktree is being removed.");
    const menu = buildWorkspaceCardMenu({
      ...worktreeCard,
      openIn: [],
      workspaceUnavailableReason: reason,
    });
    expect(menu[0]).toEqual({
      id: "open-in",
      label: "Open in",
      disabled: true,
      description: reason,
    });
    expect(menu[1]).toEqual({ id: "pull", label: "Pull", disabled: true, description: reason });
  });

  it("labels deleting a thread without a worktree by the confirmation setting", () => {
    expect(outline(buildWorkspaceCardMenu({ ...worktreeCard, isWorktree: false })).at(-1)).toBe(
      "Delete Thread…",
    );
    expect(
      outline(
        buildWorkspaceCardMenu({
          ...worktreeCard,
          isWorktree: false,
          confirmThreadDelete: false,
        }),
      ).at(-1),
    ).toBe("Delete Thread");
  });
});

describe("buildPrimaryCardMenu", () => {
  it("groups the main checkout card's items as in Menus.dc.html", () => {
    expect(
      outline(
        buildPrimaryCardMenu({
          openIn,
          branchName: "develop",
          hasDefaultThread: true,
          pinned: false,
          unread: false,
        }),
      ),
    ).toEqual([
      "Open in",
      "Pull",
      "---",
      "Copy Path",
      "Copy Branch Name",
      "---",
      "Pin",
      "Mark as Unread",
    ]);
  });

  it("drops the pin group without a default thread and the branch copy without a branch", () => {
    expect(
      outline(
        buildPrimaryCardMenu({
          openIn,
          branchName: null,
          hasDefaultThread: false,
          pinned: false,
          unread: false,
        }),
      ),
    ).toEqual(["Open in", "Pull", "---", "Copy Path"]);
  });
});

describe("buildMultiSelectMenu", () => {
  it("counts the selection and separates the destructive item", () => {
    const menu = buildMultiSelectMenu(3);
    expect(outline(menu)).toEqual(["Mark as Unread (3)", "---", "Delete (3)"]);
    expect(ids(menu)).toEqual(["mark-unread", "---", "delete"]);
    expect(menu.at(-1)).toMatchObject({ destructive: true });
  });
});

describe("buildProjectHeaderMenu", () => {
  const single = [{ physicalProjectKey: "env-main:/repo", label: "Repo" }];

  it("groups a single project's items as in Menus.dc.html", () => {
    const menu = buildProjectHeaderMenu({
      members: single,
      discovery: { visibility: "hidden", hiddenCount: 1 },
    });
    expect(outline(menu)).toEqual([
      "New Worktree…",
      "Import CLI sessions…",
      "---",
      "Rename…",
      "Group into…",
      "Copy Path",
      "---",
      "Show Hidden Worktrees (1)",
      "Archived Threads",
      "---",
      "Remove Project…",
    ]);
    expect(ids(menu)).toEqual([
      "new-worktree:env-main:/repo",
      "import-sessions:env-main:/repo",
      "---",
      "rename:env-main:/repo",
      "grouping:env-main:/repo",
      "copy-path:env-main:/repo",
      "---",
      "worktree-discovery-visibility",
      "archive",
      "---",
      "delete:env-main:/repo",
    ]);
    expect(menu.at(-1)).toMatchObject({ destructive: true, icon: "trash" });
  });

  it("omits the discovery item without discovery support and hides an unknown count", () => {
    expect(outline(buildProjectHeaderMenu({ members: single, discovery: null }))).not.toContain(
      "Show Hidden Worktrees",
    );
    expect(
      outline(
        buildProjectHeaderMenu({
          members: single,
          discovery: { visibility: "hidden", hiddenCount: null },
        }),
      ),
    ).toContain("Show Hidden Worktrees");
    expect(
      outline(
        buildProjectHeaderMenu({
          members: single,
          discovery: { visibility: "shown", hiddenCount: 3 },
        }),
      ),
    ).toContain("Hide Discovered Worktrees");
  });

  it("keeps per-member submenus for grouped projects", () => {
    const menu = buildProjectHeaderMenu({
      members: [
        { physicalProjectKey: "env-main:/repo", label: "Main — /repo" },
        { physicalProjectKey: "env-remote:/srv/repo", label: "Remote — /srv/repo" },
      ],
      discovery: null,
    });
    const rename = menu.find((entry) => "id" in entry && entry.id === "rename:submenu");
    expect(rename).toMatchObject({
      label: "Rename…",
      children: [
        { id: "rename:env-main:/repo", label: "Main — /repo" },
        { id: "rename:env-remote:/srv/repo", label: "Remote — /srv/repo" },
      ],
    });
    const remove = menu.at(-1);
    expect(remove).toMatchObject({ id: "delete:submenu", label: "Remove Project…", icon: "trash" });
    expect(remove && "children" in remove ? remove.children : []).toEqual([
      { id: "delete:env-main:/repo", label: "Main — /repo", destructive: true },
      { id: "delete:env-remote:/srv/repo", label: "Remote — /srv/repo", destructive: true },
    ]);
  });
});

describe("parseProjectHeaderSelection", () => {
  it("splits at the first colon because physical keys contain colons", () => {
    expect(parseProjectHeaderSelection("copy-path:env-main:/repo")).toEqual({
      action: "copy-path",
      physicalProjectKey: "env-main:/repo",
    });
    expect(parseProjectHeaderSelection("new-worktree:env-remote:R:\\repo")).toEqual({
      action: "new-worktree",
      physicalProjectKey: "env-remote:R:\\repo",
    });
    expect(parseProjectHeaderSelection("import-sessions:env-main:/repo")).toEqual({
      action: "import-sessions",
      physicalProjectKey: "env-main:/repo",
    });
  });

  it("rejects ids that are not project-header leaves", () => {
    expect(parseProjectHeaderSelection("archive")).toBeNull();
    expect(parseProjectHeaderSelection("worktree-discovery-visibility")).toBeNull();
    expect(parseProjectHeaderSelection("rename:submenu")).toBeNull();
    expect(parseProjectHeaderSelection("unknown:env-main:/repo")).toBeNull();
  });
});
