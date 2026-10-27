import type {
  AdoptedWorktreeAvailability,
  ContextMenuEntry,
  ContextMenuItem,
  ContextMenuSeparator,
} from "@coddy/contracts";

import { getDiscoveryVisibilityMenuLabel } from "../WorktreeDiscoverySection.logic";

/**
 * Pure builders for every left-panel menu, grouped as in the approved
 * Menus.dc.html. Renderers trim and collapse separators, so a builder may put
 * one between groups even when a neighbouring item is omitted.
 */

export type SidebarMenuEntry = ContextMenuEntry<string>;

const SEPARATOR: ContextMenuSeparator = { separator: true };

export interface WorkspaceCardMenuInput {
  /** Worktree cards delete through the worktree removal flow. */
  readonly isWorktree: boolean;
  /** "Open in" children: File Explorer first, then editors. Empty disables the item. */
  readonly openIn: readonly ContextMenuItem<string>[];
  readonly pullDisabledReason: string | null;
  /**
   * Why the card's checkout can't be used (a missing or removing worktree).
   * Disables Open in and Pull with this explanation; see `describeUnavailableWorkspace`.
   */
  readonly workspaceUnavailableReason: string | null;
  /** The branch the card shows; "Copy Branch Name" is omitted when null. */
  readonly branchName: string | null;
  readonly pinned: boolean;
  readonly unread: boolean;
  readonly confirmThreadDelete: boolean;
  /**
   * A provider session in the card's worktree is running (see
   * `isWorktreeSessionRunning`). Disables Delete Worktree with
   * `WORKTREE_DELETE_BLOCKED_REASON`; Delete Thread stops its own session first.
   */
  readonly worktreeSessionRunning: boolean;
}

/**
 * Why Delete Worktree waits: it is refused while a session in the worktree runs
 * (`isWorktreeSessionRunning`), so the checkout never disappears under a working
 * agent. "Runs" is the card menu's Archive rule, `isWorkspaceThreadRunning`,
 * which is stricter than `archiveThread`'s refusal of a running session only
 * while it has an active turn.
 */
export const WORKTREE_DELETE_BLOCKED_REASON =
  "Stop the running session before deleting this worktree.";

export interface PrimaryCardMenuInput {
  readonly openIn: readonly ContextMenuItem<string>[];
  readonly branchName: string | null;
  /** Pin and read state live on the default thread; a project without one has neither. */
  readonly hasDefaultThread: boolean;
  readonly pinned: boolean;
  readonly unread: boolean;
}

export type ProjectHeaderAction =
  | "new-worktree"
  | "import-sessions"
  | "rename"
  | "grouping"
  | "copy-path"
  | "delete";

const PROJECT_HEADER_ACTIONS: readonly ProjectHeaderAction[] = [
  "new-worktree",
  "import-sessions",
  "rename",
  "grouping",
  "copy-path",
  "delete",
];

export interface ProjectHeaderMenuMember {
  readonly physicalProjectKey: string;
  /** The member's label inside a grouped project's submenu. */
  readonly label: string;
}

export interface ProjectHeaderMenuInput {
  readonly members: readonly ProjectHeaderMenuMember[];
  /** Present when a member supports worktree discovery. */
  readonly discovery: {
    readonly visibility: "hidden" | "shown";
    readonly hiddenCount: number | null;
  } | null;
}

/**
 * The disabled-action explanation for a worktree whose checkout can't be used,
 * in the terms and recovery actions of its card's availability warning.
 */
export function describeUnavailableWorkspace(
  availability: Exclude<AdoptedWorktreeAvailability, "present" | "verification-unavailable">,
): string {
  return availability === "removing"
    ? "This worktree is being removed."
    : "The worktree directory is missing. Use Retry detection or Remove from Coddy Studio on its card.";
}

function openInEntry(
  openIn: readonly ContextMenuItem<string>[],
  unavailableReason: string | null = null,
): ContextMenuItem<string> {
  return openIn.length > 0 && unavailableReason === null
    ? { id: "open-in", label: "Open in", children: openIn }
    : {
        id: "open-in",
        label: "Open in",
        disabled: true,
        description: unavailableReason ?? "No local opener is available for this workspace.",
      };
}

function pinAndReadEntries(pinned: boolean, unread: boolean): ContextMenuItem<string>[] {
  return [
    { id: "toggle-pin", label: pinned ? "Unpin" : "Pin" },
    unread
      ? { id: "mark-read", label: "Mark as Read" }
      : { id: "mark-unread", label: "Mark as Unread" },
  ];
}

function copyBranchEntry(branchName: string | null): ContextMenuItem<string>[] {
  return branchName === null ? [] : [{ id: "copy-branch-name", label: "Copy Branch Name" }];
}

export function buildWorkspaceCardMenu(input: WorkspaceCardMenuInput): SidebarMenuEntry[] {
  const deleteLabel = input.isWorktree
    ? "Delete Worktree…"
    : input.confirmThreadDelete
      ? "Delete Thread…"
      : "Delete Thread";
  const deleteEntry: ContextMenuItem<string> = {
    id: "delete",
    label: deleteLabel,
    destructive: true,
    icon: "trash",
  };
  return [
    openInEntry(input.openIn, input.workspaceUnavailableReason),
    (input.pullDisabledReason ?? input.workspaceUnavailableReason)
      ? {
          id: "pull",
          label: "Pull",
          disabled: true,
          description: (input.pullDisabledReason ?? input.workspaceUnavailableReason)!,
        }
      : { id: "pull", label: "Pull" },
    SEPARATOR,
    { id: "copy-path", label: "Copy Path" },
    ...copyBranchEntry(input.branchName),
    { id: "copy-thread-id", label: "Copy Thread ID" },
    SEPARATOR,
    ...pinAndReadEntries(input.pinned, input.unread),
    { id: "rename", label: "Rename…" },
    SEPARATOR,
    input.isWorktree && input.worktreeSessionRunning
      ? { ...deleteEntry, disabled: true, description: WORKTREE_DELETE_BLOCKED_REASON }
      : deleteEntry,
  ];
}

export function buildPrimaryCardMenu(input: PrimaryCardMenuInput): SidebarMenuEntry[] {
  return [
    openInEntry(input.openIn),
    { id: "pull", label: "Pull" },
    SEPARATOR,
    { id: "copy-path", label: "Copy Path" },
    ...copyBranchEntry(input.branchName),
    ...(input.hasDefaultThread
      ? [SEPARATOR, ...pinAndReadEntries(input.pinned, input.unread)]
      : []),
  ];
}

export function buildMultiSelectMenu(count: number): SidebarMenuEntry[] {
  return [
    { id: "mark-unread", label: `Mark as Unread (${count})` },
    SEPARATOR,
    { id: "delete", label: `Delete (${count})`, destructive: true },
  ];
}

function projectHeaderEntry(
  members: readonly ProjectHeaderMenuMember[],
  action: ProjectHeaderAction,
  label: string,
  destructive: boolean,
): ContextMenuItem<string> {
  const onlyMember = members.length === 1 ? members[0] : undefined;
  if (onlyMember !== undefined) {
    return {
      id: `${action}:${onlyMember.physicalProjectKey}`,
      label,
      ...(destructive ? { destructive: true, icon: "trash" } : {}),
    };
  }
  return {
    id: `${action}:submenu`,
    label,
    ...(destructive ? { icon: "trash" } : {}),
    children: members.map((member) => ({
      id: `${action}:${member.physicalProjectKey}`,
      label: member.label,
      ...(destructive ? { destructive: true } : {}),
    })),
  };
}

export function buildProjectHeaderMenu(input: ProjectHeaderMenuInput): SidebarMenuEntry[] {
  const entry = (action: ProjectHeaderAction, label: string, destructive = false) =>
    projectHeaderEntry(input.members, action, label, destructive);
  return [
    entry("new-worktree", "New Worktree…"),
    entry("import-sessions", "Import CLI sessions…"),
    SEPARATOR,
    entry("rename", "Rename…"),
    entry("grouping", "Group into…"),
    entry("copy-path", "Copy Path"),
    SEPARATOR,
    ...(input.discovery === null
      ? []
      : [
          {
            id: "worktree-discovery-visibility",
            label: getDiscoveryVisibilityMenuLabel(
              input.discovery.visibility,
              input.discovery.hiddenCount,
            ),
          },
        ]),
    { id: "archive", label: "Archived Threads" },
    SEPARATOR,
    entry("delete", "Remove Project…", true),
  ];
}

/**
 * Reads a project-header leaf id back. Physical project keys contain colons
 * (`<environmentId>:<path>`), so the action ends at the first colon.
 */
export function parseProjectHeaderSelection(
  id: string,
): { readonly action: ProjectHeaderAction; readonly physicalProjectKey: string } | null {
  const colonIndex = id.indexOf(":");
  if (colonIndex <= 0) {
    return null;
  }
  const action = PROJECT_HEADER_ACTIONS.find((candidate) => candidate === id.slice(0, colonIndex));
  const physicalProjectKey = id.slice(colonIndex + 1);
  if (action === undefined || physicalProjectKey === "" || physicalProjectKey === "submenu") {
    return null;
  }
  return { action, physicalProjectKey };
}
