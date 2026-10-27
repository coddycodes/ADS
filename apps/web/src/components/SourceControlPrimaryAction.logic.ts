import type { GitStackedAction, VcsStatusResult } from "@coddy/contracts";
import { isTemporaryWorktreeBranch } from "@coddy/shared/git";
import { GIT_MANAGER_STASH_MERGE_DISABLED_REASON } from "./gitManager/gitManagerAvailability";
import {
  DEFAULT_CHANGE_REQUEST_TERMINOLOGY,
  getChangeRequestTerminology,
  type ChangeRequestTerminology,
} from "../sourceControlPresentation";

/**
 * Commit-first primary-action model for the Source Control panel.
 *
 * This is a deliberate divergence from the chat-header `GitActionsControl`
 * cascade (`GitActionsControl.logic.ts`), which defaults a dirty tree to
 * `commit_push` / `commit_push_pr`. Here the primary button leads with the
 * staging area: staged content -> Commit, otherwise Stage All, and only a clean
 * tree walks the remote ladder (push / pull / PR). The chat-header behavior is
 * left untouched — do NOT reuse this module there.
 *
 * Two intentional divergences from Orca:
 * - Commit stays ENABLED with an empty message when there are staged changes
 *   (our auto-generate affordance fills the message server-side). Orca disables
 *   commit without a message.
 * - No stash / amend / Ctrl+Enter here: stash and amend live in Git Manager; fetch and
 *   merge reach this panel through the "Merge into current branch…" dialog.
 */

export type SourceControlPrimaryActionKind =
  | "commit"
  | "stage_all"
  | "push"
  | "pull"
  | "sync_hint"
  | "publish"
  | "create_pr"
  | "open_pr"
  | "none";

export interface SourceControlPrimaryAction {
  kind: SourceControlPrimaryActionKind;
  label: string;
  disabled: boolean;
  /** Hint copy for disabled / no-op states (shown as tooltip / helper text). */
  hint?: string;
  /** Ahead / behind / staged / stageable count surfaced next to the label. */
  count?: number;
  /**
   * For stacked-action kinds (`commit`, `push`, `create_pr`) the concrete
   * `GitStackedAction` the caller dispatches through `git.runStackedAction`.
   */
  stackedAction?: GitStackedAction;
  /**
   * Commit the staged index exactly as-is. Preserves the ledger guard: a commit
   * of the staged set must carry `commitStagedIndexAsIs: true`, never
   * `filePaths: []` (contracts git.ts). Only set on the `commit` kind.
   */
  commitStagedIndexAsIs?: boolean;
}

export type SourceControlMenuItemId =
  | "commit"
  | "commit_push"
  | "commit_push_pr"
  | "push"
  | "pull"
  | "create_pr"
  | "open_pr"
  | "open_pr_panel"
  | "publish"
  | "merge";

export interface SourceControlMenuItem {
  id: SourceControlMenuItemId;
  label: string;
  disabled: boolean;
  /** Why the item is disabled — rendered as tooltip on the disabled entry. */
  reason?: string;
  /**
   * Visual grouping: the commit-family sits above a separator, the remote
   * actions below it. The caller renders the separator between groups.
   */
  group: "commit" | "remote";
  kind: "run_stacked" | "run_pull" | "open_pr" | "open_pr_panel" | "open_publish" | "open_merge";
  stackedAction?: GitStackedAction;
  commitStagedIndexAsIs?: boolean;
}

export interface SourceControlPrimaryActionInput {
  gitStatus: VcsStatusResult | null;
  isBusy: boolean;
  isDefaultRef: boolean;
  hasPrimaryRemote: boolean;
  /** Files in the staged area. */
  stagedCount: number;
  /** Files in the unstaged + untracked areas (everything stage-able). */
  stageableCount: number;
  /** The environment runs Git Manager fetch and merge operations for this project. */
  mergeAvailable: boolean;
  /** The thread's right panel can show this branch's request (setting on, host supported). */
  canOpenInPanel: boolean;
}

function resolveChangeRequestTerminology(
  gitStatus: VcsStatusResult | null,
): ChangeRequestTerminology {
  return gitStatus?.sourceControlProvider
    ? getChangeRequestTerminology(gitStatus.sourceControlProvider)
    : DEFAULT_CHANGE_REQUEST_TERMINOLOGY;
}

function supportsChangeRequests(gitStatus: VcsStatusResult): boolean {
  return gitStatus.sourceControlProvider?.kind !== "unknown";
}

function canPushFromCurrentRef(gitStatus: VcsStatusResult): boolean {
  return gitStatus.refName === null || !isTemporaryWorktreeBranch(gitStatus.refName);
}

const NO_STATUS_ACTION: SourceControlPrimaryAction = {
  kind: "none",
  label: "Commit",
  disabled: true,
  hint: "Git status is unavailable.",
};

/**
 * Resolve the single primary button for the Source Control panel.
 *
 * Ladder (commit-first): staged -> Commit, else stage-able -> Stage All, else
 * the clean-tree remote ladder adapted from `resolveQuickAction` with the
 * commit step stripped. When busy, the resolved action is returned disabled
 * ("previous-kind disabled") — the caller renders the spinner.
 */
export function resolveSourceControlPrimaryAction(
  input: SourceControlPrimaryActionInput,
): SourceControlPrimaryAction {
  const { gitStatus, isBusy } = input;
  if (!gitStatus) {
    return NO_STATUS_ACTION;
  }

  const decision = resolvePrimaryActionForStatus(input, gitStatus);
  return isBusy ? { ...decision, disabled: true } : decision;
}

function resolvePrimaryActionForStatus(
  input: SourceControlPrimaryActionInput,
  gitStatus: VcsStatusResult,
): SourceControlPrimaryAction {
  const { isDefaultRef, hasPrimaryRemote, stagedCount, stageableCount } = input;
  const terminology = resolveChangeRequestTerminology(gitStatus);
  const changeRequestsEnabled = supportsChangeRequests(gitStatus);
  const pushEnabledForRef = canPushFromCurrentRef(gitStatus);

  // Staged content always wins — commit the staged index as-is. Enabled even
  // with an empty message (auto-generate divergence); ordering intentionally
  // precedes the no-branch guard, so a detached HEAD with staged files can
  // still commit (faithful to the pinned ladder order).
  if (stagedCount > 0) {
    return {
      kind: "commit",
      label: "Commit",
      disabled: false,
      count: stagedCount,
      stackedAction: "commit",
      commitStagedIndexAsIs: true,
    };
  }

  if (stageableCount > 0) {
    return {
      kind: "stage_all",
      label: "Stage All Changes",
      disabled: false,
      count: stageableCount,
    };
  }

  // Clean tree: walk the remote ladder (commit stripped).
  const hasBranch = gitStatus.refName !== null;
  if (!hasBranch) {
    return {
      kind: "none",
      label: "Commit",
      disabled: true,
      hint: `Create and checkout a ref before pushing or opening a ${terminology.singular}.`,
    };
  }

  const hasOpenPr = gitStatus.pr?.state === "open";
  const isAhead = gitStatus.aheadCount > 0;
  const isBehind = gitStatus.behindCount > 0;
  const isDiverged = isAhead && isBehind;
  const hasDefaultBranchDelta = (gitStatus.aheadOfDefaultCount ?? gitStatus.aheadCount) > 0;

  if (!gitStatus.hasUpstream) {
    if (!hasPrimaryRemote) {
      if (changeRequestsEnabled && hasOpenPr && !isAhead) {
        return { kind: "open_pr", label: `View ${terminology.shortLabel}`, disabled: false };
      }
      return { kind: "publish", label: "Publish repository", disabled: false };
    }
    if (!pushEnabledForRef) {
      return {
        kind: "none",
        label: "Up to date",
        disabled: true,
        hint: "Push is only available from the active project branch.",
      };
    }
    if (!changeRequestsEnabled || hasOpenPr || isDefaultRef || !hasDefaultBranchDelta) {
      return {
        kind: "push",
        label: "Push",
        disabled: false,
        count: gitStatus.aheadCount,
        stackedAction: "push",
      };
    }
    return {
      kind: "create_pr",
      label: `Push & create ${terminology.shortLabel}`,
      disabled: false,
      stackedAction: "create_pr",
    };
  }

  if (isDiverged) {
    return {
      kind: "sync_hint",
      label: "Sync ref",
      disabled: true,
      hint: "Branch has diverged from upstream. Rebase/merge first.",
    };
  }

  if (isBehind) {
    return { kind: "pull", label: "Pull", disabled: false, count: gitStatus.behindCount };
  }

  if (isAhead) {
    if (!pushEnabledForRef) {
      return {
        kind: "none",
        label: "Up to date",
        disabled: true,
        hint: "Push is only available from the active project branch.",
      };
    }
    if (!changeRequestsEnabled || hasOpenPr || isDefaultRef) {
      return {
        kind: "push",
        label: "Push",
        disabled: false,
        count: gitStatus.aheadCount,
        stackedAction: "push",
      };
    }
    return {
      kind: "create_pr",
      label: `Push & create ${terminology.shortLabel}`,
      disabled: false,
      stackedAction: "create_pr",
    };
  }

  if (changeRequestsEnabled && hasOpenPr) {
    return { kind: "open_pr", label: `View ${terminology.shortLabel}`, disabled: false };
  }

  if (changeRequestsEnabled && pushEnabledForRef && hasDefaultBranchDelta && !isDefaultRef) {
    return {
      kind: "create_pr",
      label: `Create ${terminology.shortLabel}`,
      disabled: false,
      stackedAction: "create_pr",
    };
  }

  return {
    kind: "none",
    label: "Up to date",
    disabled: true,
    hint: "Branch is up to date. No action needed.",
  };
}

export const BUSY_REASON = "Git action in progress.";

/**
 * Build the primary-action dropdown for the Source Control panel.
 *
 * Orca's always-rendered-disabled pattern: every item is always present in a
 * fixed order (Commit / Commit & Push / Commit & Push & PR / — / Push / Pull /
 * Create-or-View PR / Publish-when-applicable) and carries `disabled` + a
 * `reason` when inapplicable rather than being hidden.
 *
 * Force-push, rebase and stash stay Git Manager–only. Fetch and merge run through
 * Git Manager operations from the "Merge into current branch…" dialog.
 */
export function buildSourceControlMenuItems(
  input: SourceControlPrimaryActionInput,
): SourceControlMenuItem[] {
  const { gitStatus, isBusy, hasPrimaryRemote, stagedCount } = input;
  if (!gitStatus) {
    return [];
  }

  const terminology = resolveChangeRequestTerminology(gitStatus);
  const changeRequestsEnabled = supportsChangeRequests(gitStatus);
  const pushEnabledForRef = canPushFromCurrentRef(gitStatus);
  const hasStaged = stagedCount > 0;
  const hasBranch = gitStatus.refName !== null;
  const hasOpenPr = gitStatus.pr?.state === "open";
  const isAhead = gitStatus.aheadCount > 0;
  const isBehind = gitStatus.behindCount > 0;
  const isDiverged = isAhead && isBehind;
  const hasDefaultBranchDelta = (gitStatus.aheadOfDefaultCount ?? gitStatus.aheadCount) > 0;
  const canPushWithoutUpstream = hasPrimaryRemote && !gitStatus.hasUpstream;
  const hasPushTarget = gitStatus.hasUpstream || canPushWithoutUpstream;

  // Returns disabled/reason from the first blocking reason, or enabled.
  const gate = (reason: string | undefined): { disabled: boolean; reason?: string } => {
    if (isBusy) return { disabled: true, reason: BUSY_REASON };
    return reason === undefined ? { disabled: false } : { disabled: true, reason };
  };

  const noBranchReason = hasBranch ? undefined : "Create and checkout a ref first.";
  const noPushTargetReason = hasPushTarget ? undefined : "No remote configured to push to.";
  const behindReason = isDiverged
    ? "Branch has diverged from upstream. Rebase/merge first."
    : isBehind
      ? "Pull upstream changes before pushing."
      : undefined;
  const commitReason = hasStaged ? undefined : "No staged changes to commit.";

  const commitPushReason =
    commitReason ??
    noBranchReason ??
    (pushEnabledForRef ? undefined : "Push is only available from the active project branch.") ??
    noPushTargetReason ??
    behindReason ??
    undefined;
  const commitPushPrReason =
    commitPushReason ??
    (!changeRequestsEnabled ? `${terminology.singular} actions are unavailable.` : undefined) ??
    (hasOpenPr ? `A ${terminology.singular} is already open.` : undefined);

  const items: Array<SourceControlMenuItem | null> = [
    {
      id: "commit",
      label: "Commit",
      group: "commit",
      kind: "run_stacked",
      stackedAction: "commit",
      commitStagedIndexAsIs: true,
      ...gate(commitReason),
    },
    {
      id: "pull",
      label: "Pull",
      group: "remote",
      kind: "run_pull",
      ...gate(
        noBranchReason ??
          (!gitStatus.hasUpstream
            ? "No upstream to pull from."
            : isBehind
              ? undefined
              : "Already up to date."),
      ),
    },
    {
      id: "merge",
      label: "Merge into current branch…",
      group: "remote",
      kind: "open_merge",
      ...gate(
        (input.mergeAvailable ? undefined : GIT_MANAGER_STASH_MERGE_DISABLED_REASON) ??
          (hasBranch ? undefined : "Check out a branch to merge into.") ??
          (gitStatus.operationInProgress === undefined
            ? undefined
            : "Finish or abort the current merge first.") ??
          (gitStatus.hasWorkingTreeChanges ? "Commit your changes before merging." : undefined),
      ),
    },
    changeRequestsEnabled && hasOpenPr
      ? {
          id: "open_pr",
          label: `View ${terminology.shortLabel}`,
          group: "remote",
          kind: "open_pr",
          ...gate(undefined),
        }
      : changeRequestsEnabled
        ? {
            id: "create_pr",
            label: `Create ${terminology.shortLabel}`,
            group: "remote",
            kind: "run_stacked",
            stackedAction: "create_pr",
            // No isDefaultRef gate — creating a PR from the default ref is
            // allowed-but-confirmed (SC-C's default-branch dialog), matching the
            // chat-header buildMenuItems. Requires commits ahead of default only.
            ...gate(
              noBranchReason ??
                (pushEnabledForRef
                  ? undefined
                  : "Push is only available from the active project branch.") ??
                noPushTargetReason ??
                behindReason ??
                (hasDefaultBranchDelta
                  ? undefined
                  : `No commits to open a ${terminology.singular} for.`),
            ),
          }
        : null,
    changeRequestsEnabled && hasOpenPr && input.canOpenInPanel
      ? {
          id: "open_pr_panel",
          label: `Open ${terminology.shortLabel} in panel`,
          group: "remote",
          kind: "open_pr_panel",
          ...gate(undefined),
        }
      : null,
  ];

  if (pushEnabledForRef) {
    const changeRequestCommitItems: SourceControlMenuItem[] = changeRequestsEnabled
      ? [
          {
            id: "commit_push_pr",
            label: `Commit, Push & ${terminology.shortLabel}`,
            group: "commit",
            kind: "run_stacked",
            stackedAction: "commit_push_pr",
            commitStagedIndexAsIs: true,
            ...gate(commitPushPrReason),
          },
        ]
      : [];

    items.splice(
      1,
      0,
      {
        id: "commit_push",
        label: "Commit & Push",
        group: "commit",
        kind: "run_stacked",
        stackedAction: "commit_push",
        commitStagedIndexAsIs: true,
        ...gate(commitPushReason),
      },
      ...changeRequestCommitItems,
    );
    items.splice(changeRequestsEnabled ? 3 : 2, 0, {
      id: "push",
      label: "Push",
      group: "remote",
      kind: "run_stacked",
      stackedAction: "push",
      ...gate(
        noBranchReason ??
          noPushTargetReason ??
          behindReason ??
          (isAhead || canPushWithoutUpstream ? undefined : "No local commits to push."),
      ),
    });
  }

  // Publish is only meaningful before a primary remote exists (research §5).
  if (!hasPrimaryRemote) {
    items.push({
      id: "publish",
      label: "Publish repository",
      group: "remote",
      kind: "open_publish",
      ...gate(noBranchReason),
    });
  }

  return items.filter((item): item is SourceControlMenuItem => item !== null);
}
