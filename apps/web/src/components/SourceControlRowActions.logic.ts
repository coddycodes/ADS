// Pure decision helpers for a source-control change row. No React, no I/O — so the
// per-area inline action set and the navigation-only context menu shape are unit
// testable in isolation. SourceControlChangesList.tsx renders these descriptors.

import type { VcsStagingArea, VcsWorkingTreeFileStatus } from "@coddy/contracts";
import { MinusIcon, PlusIcon, Trash2Icon, Undo2Icon, type LucideIcon } from "lucide-react";

/** The three change buckets a row can sit in (Orca §2/§4 inline actions). */
export type RowArea = "staged" | "unstaged" | "untracked";

/** What an inline row action does; the panel maps these to RPCs / confirm dialogs. */
export type RowActionKind = "stage" | "unstage" | "discard" | "delete";

export interface RowActionDescriptor {
  readonly kind: RowActionKind;
  readonly label: string;
  readonly icon: LucideIcon;
  /** Delete-untracked is irreversible; the renderer styles it destructive. */
  readonly destructive?: boolean;
}

/**
 * Collapse the server's staging area (which may be undefined on legacy servers)
 * into the row bucket, matching `groupFilesByArea`: staged/untracked are explicit,
 * everything else (including no area) is treated as unstaged.
 */
export function rowAreaOf(area: VcsStagingArea | undefined): RowArea {
  if (area === "staged") return "staged";
  if (area === "untracked") return "untracked";
  return "unstaged";
}

/**
 * Ordered inline actions for a row, by area (Orca §2/§4):
 *   staged    → [Unstage]
 *   unstaged  → [Discard changes | Restore file (deleted), Stage]
 *   untracked → [Delete untracked file (destructive), Stage]
 * Discard and delete both surface through the panel's confirm dialog; the row
 * only signals intent.
 */
export function getRowActions(
  area: RowArea,
  status?: VcsWorkingTreeFileStatus,
): RowActionDescriptor[] {
  switch (area) {
    case "staged":
      return [{ kind: "unstage", label: "Unstage", icon: MinusIcon }];
    case "unstaged":
      return [
        {
          kind: "discard",
          label: status === "deleted" ? "Restore file" : "Discard changes",
          icon: Undo2Icon,
        },
        { kind: "stage", label: "Stage", icon: PlusIcon },
      ];
    case "untracked":
      return [
        { kind: "delete", label: "Delete untracked file", icon: Trash2Icon, destructive: true },
        { kind: "stage", label: "Stage", icon: PlusIcon },
      ];
  }
}

export type RowContextMenuItemId =
  | "view"
  | "ignore-file-name"
  | "ignore-parent-folder"
  | "copy-path"
  | "copy-relative-path"
  | "open-external-editor";

export interface RowContextMenuItem {
  readonly id: RowContextMenuItemId;
  readonly label: string;
  readonly enabled: boolean;
}

export interface RowContextMenuModel {
  /** Ordered item groups; the renderer draws a separator between groups. */
  readonly groups: RowContextMenuItem[][];
}

/**
 * The right-click menu is navigation-only (Orca §2): View, Copy Path, Copy
 * Relative Path, and — primary env only — Open in External Editor. Stage /
 * discard / delete stay inline-only, so they are deliberately absent here. The
 * renderer additionally drops any item whose host handler is unwired.
 */
export function buildRowContextMenu(input: { isPrimaryEnv: boolean }): RowContextMenuModel {
  const groups: RowContextMenuItem[][] = [
    [{ id: "view", label: "View", enabled: true }],
    [
      { id: "ignore-file-name", label: "Add file name to .gitignore", enabled: true },
      { id: "ignore-parent-folder", label: "Add parent folder to .gitignore", enabled: true },
    ],
    [
      { id: "copy-path", label: "Copy Path", enabled: true },
      { id: "copy-relative-path", label: "Copy Relative Path", enabled: true },
    ],
  ];
  if (input.isPrimaryEnv) {
    groups.push([{ id: "open-external-editor", label: "Open in External Editor", enabled: true }]);
  }
  return { groups };
}
