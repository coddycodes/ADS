import type { GitManagerBlockedReason, GitManagerCommitEntry } from "@coddy/contracts";

export type GitManagerCommitMenuItemId =
  | "reset"
  | "revert"
  | "cherry-pick"
  | "reorder"
  | "create-branch"
  | "create-tag"
  | "copy-sha"
  | "squash";

export interface GitManagerCommitMenuItem {
  readonly id: GitManagerCommitMenuItemId;
  readonly label: string;
  readonly enabled: boolean;
  readonly disabledReason: string | null;
}

export interface GitManagerCommitMenuContext {
  readonly loadedCommits: ReadonlyArray<GitManagerCommitEntry>;
  readonly blockedReasons: ReadonlyArray<GitManagerBlockedReason>;
  readonly branchSyncDisabledReason: string | null;
  readonly rewriteDisabledReason: string | null;
  readonly tagDisabledReason: string | null;
}

export interface GitManagerCommitSelectionPresentation {
  readonly kind: "single" | "range" | "non-contiguous";
  readonly suppressDiff: boolean;
  readonly message: string | null;
}

function item(id: GitManagerCommitMenuItemId, label: string): GitManagerCommitMenuItem {
  return { id, label, enabled: true, disabledReason: null };
}

function disabledItem(
  id: GitManagerCommitMenuItemId,
  label: string,
  disabledReason: string,
): GitManagerCommitMenuItem {
  return { id, label, enabled: false, disabledReason };
}

function isContiguousSelection(
  selection: ReadonlyArray<string>,
  loadedCommits: ReadonlyArray<GitManagerCommitEntry>,
): boolean {
  const selected = new Set(selection);
  if (selected.size !== selection.length) return false;
  const indexes = loadedCommits.flatMap((commit, index) =>
    selected.has(commit.sha) ? [index] : [],
  );
  if (indexes.length !== selection.length) return false;
  return indexes.at(-1)! - indexes[0]! + 1 === indexes.length;
}

function selectionContainsMerge(
  selection: ReadonlyArray<string>,
  loadedCommits: ReadonlyArray<GitManagerCommitEntry>,
): boolean {
  const selected = new Set(selection);
  return loadedCommits.some((commit) => selected.has(commit.sha) && commit.parents.length > 1);
}

export function resolveCommitSelectionPresentation(
  selection: ReadonlyArray<string>,
  loadedCommits: ReadonlyArray<GitManagerCommitEntry>,
): GitManagerCommitSelectionPresentation {
  if (selection.length <= 1) return { kind: "single", suppressDiff: false, message: null };
  if (isContiguousSelection(selection, loadedCommits)) {
    return { kind: "range", suppressDiff: false, message: null };
  }
  return {
    kind: "non-contiguous",
    suppressDiff: true,
    message: "Select a contiguous range to compare multiple commits.",
  };
}

function operationForItem(id: GitManagerCommitMenuItemId): string | null {
  switch (id) {
    case "reset":
      return "reset";
    case "revert":
      return "revert";
    case "cherry-pick":
      return "cherry-pick";
    case "reorder":
      return "reorder";
    case "create-branch":
      return "branch-create";
    case "create-tag":
      return "tag-create";
    case "squash":
      return "squash";
    case "copy-sha":
      return null;
  }
}

function applyServerBlocks(
  items: ReadonlyArray<GitManagerCommitMenuItem>,
  blockedReasons: ReadonlyArray<GitManagerBlockedReason>,
): ReadonlyArray<GitManagerCommitMenuItem> {
  return items.map((menuItem) => {
    const operation = operationForItem(menuItem.id);
    const blocked =
      operation === null
        ? null
        : (blockedReasons.find((reason) => reason.operation === operation) ?? null);
    return blocked === null
      ? menuItem
      : { ...menuItem, enabled: false, disabledReason: blocked.message };
  });
}

function capabilityDisabledReason(
  id: GitManagerCommitMenuItemId,
  context: GitManagerCommitMenuContext,
): string | null {
  switch (id) {
    case "reset":
    case "revert":
    case "cherry-pick":
    case "reorder":
    case "squash":
      return context.rewriteDisabledReason;
    case "create-branch":
      return context.branchSyncDisabledReason;
    case "create-tag":
      return context.tagDisabledReason;
    case "copy-sha":
      return null;
  }
}

function applyCapabilityBlocks(
  items: ReadonlyArray<GitManagerCommitMenuItem>,
  context: GitManagerCommitMenuContext,
): ReadonlyArray<GitManagerCommitMenuItem> {
  return items.map((menuItem) => {
    const disabledReason = capabilityDisabledReason(menuItem.id, context);
    return disabledReason === null ? menuItem : { ...menuItem, enabled: false, disabledReason };
  });
}

export function buildCommitMenuItems(
  selection: ReadonlyArray<string>,
  context: GitManagerCommitMenuContext,
): ReadonlyArray<GitManagerCommitMenuItem> {
  if (selection.length > 1) {
    const structuralReason = !isContiguousSelection(selection, context.loadedCommits)
      ? "Select a contiguous range of commits."
      : selectionContainsMerge(selection, context.loadedCommits)
        ? "Merge commits cannot be squashed or reordered."
        : null;
    return applyCapabilityBlocks(
      applyServerBlocks(
        [
          item("cherry-pick", `Cherry-Pick ${selection.length}`),
          structuralReason === null
            ? item("squash", `Squash ${selection.length}`)
            : disabledItem("squash", `Squash ${selection.length}`, structuralReason),
          structuralReason === null
            ? item("reorder", `Reorder ${selection.length}`)
            : disabledItem("reorder", `Reorder ${selection.length}`, structuralReason),
        ],
        context.blockedReasons,
      ),
      context,
    );
  }
  return applyCapabilityBlocks(
    applyServerBlocks(
      [
        item("reset", "Reset to Commit"),
        item("revert", "Revert"),
        item("cherry-pick", "Cherry-Pick"),
        item("reorder", "Reorder"),
        item("create-branch", "Create Branch from Commit"),
        item("create-tag", "Create Tag"),
        item("copy-sha", "Copy SHA"),
      ],
      context.blockedReasons,
    ),
    context,
  );
}
