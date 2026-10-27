import type { VcsStatusResult } from "@coddy/contracts";

import type { GitManagerTab } from "../../gitManagerStore";

/**
 * The working tree as the status stream reports it. A status for a directory Git cannot
 * read as a repository (`isRepo` false, as a broken `HEAD` or `.git/config` reports) lists
 * no files, so it is `unreadable`: it says nothing about the working tree.
 */
export type GitManagerWorkingTree = "loading" | "unreadable" | "dirty" | "clean";

export function resolveGitManagerWorkingTree(
  status: Pick<VcsStatusResult, "isRepo" | "hasWorkingTreeChanges"> | null,
): GitManagerWorkingTree {
  if (status === null) return "loading";
  if (!status.isRepo) return "unreadable";
  return status.hasWorkingTreeChanges ? "dirty" : "clean";
}

/** The repository facts whose transitions move the selected tab. */
export interface GitManagerTabTransitionInputs {
  /** Null until refs provide a snapshot; absence of refs does not mean a merge ended. */
  readonly mergePending: boolean | null;
  readonly workingTree: GitManagerWorkingTree;
}

/**
 * Keep settled observations across status subscription gaps; only an opening starts at
 * loading. Refs load independently, so retain the last known merge state until they return.
 */
export function resolveGitManagerTabTransitionBaseline(
  previous: GitManagerTabTransitionInputs | null,
  next: GitManagerTabTransitionInputs,
): GitManagerTabTransitionInputs {
  if (previous !== null && previous.workingTree !== "loading" && next.workingTree === "loading") {
    return previous;
  }
  return {
    ...next,
    mergePending: next.mergePending ?? previous?.mergePending ?? null,
  };
}

/**
 * The tab to select when the inputs change from `previous` to `next`, or null to keep the
 * selected tab. `previous` is null when the panel opens or moves to another project or
 * checkout, so each of those starts like an opening. Only these transitions move the tab:
 * - a merge that appears, or is pending on opening, selects Changes, where it is finished;
 * - a checkout that becomes clean selects History: opening on a clean checkout, a commit or
 *   discard that cleans a dirty one, or a merge that ends on a clean tree. Tags is unrelated
 *   to the working tree, so this never pulls the user off Tags.
 *
 * A manual tab pick changes no input, so it is never overridden. An unreadable status is not
 * a clean checkout, and the readable status after it starts a new baseline rather than a
 * transition, so neither a Git failure nor its recovery moves the tab.
 */
export function resolveGitManagerTabTransition(
  previous: GitManagerTabTransitionInputs | null,
  next: GitManagerTabTransitionInputs,
  currentTab: GitManagerTab,
): GitManagerTab | null {
  const mergePending = next.mergePending ?? previous?.mergePending ?? null;
  if (mergePending) return previous?.mergePending === true ? null : "changes";
  if (next.workingTree !== "clean" || currentTab === "tags") return null;
  if (previous === null || previous.mergePending) return "history";
  return previous.workingTree === "loading" || previous.workingTree === "dirty" ? "history" : null;
}
