import type { GitManagerMergePreview, GitManagerRefEntry } from "@coddy/contracts";

/** Display name for a merge ref: drops `refs/heads/` and `refs/remotes/`. */
export function shortRefName(ref: string): string {
  return ref.replace(/^refs\/(heads|remotes)\//, "");
}

/** A merge needs a local branch other than the checked-out one, or any remote branch. */
export function hasMergeSource(
  localBranches: ReadonlyArray<GitManagerRefEntry>,
  remoteBranches: ReadonlyArray<GitManagerRefEntry>,
): boolean {
  return remoteBranches.length > 0 || localBranches.some((branch) => !branch.current);
}

/**
 * Prefix of the server's `git-too-old` preview message, which reaches the client only as
 * text. A merge into the checked-out branch still works on that Git, without a preview.
 */
export const MERGE_PREVIEW_GIT_TOO_OLD_PREFIX = "Merge preview needs Git";

export interface MergePreviewSummary {
  readonly kind: GitManagerMergePreview["_tag"];
  readonly message: string;
  readonly mergeEnabled: boolean;
  readonly ahead: number;
  readonly behind: number;
}

export function summarizeMergePreview(
  preview: GitManagerMergePreview,
  options: { readonly intoOtherBranch?: boolean } = {},
): MergePreviewSummary {
  const base = { ahead: preview.ahead, behind: preview.behind };
  const source = shortRefName(preview.source);
  const current = shortRefName(preview.current);
  switch (preview._tag) {
    case "clean":
      if (preview.ahead === 0) {
        return {
          kind: preview._tag,
          message: `Nothing to merge: \`${source}\` has no commits that \`${current}\` lacks.`,
          mergeEnabled: false,
          ...base,
        };
      }
      return {
        kind: preview._tag,
        message: `This will merge ${preview.ahead} commits from \`${source}\` into \`${current}\`.`,
        mergeEnabled: true,
        ...base,
      };
    case "conflicted":
      // No checkout holds a merge into another branch, so its conflicts cannot be resolved.
      if (options.intoOtherBranch) {
        return {
          kind: preview._tag,
          message: `${preview.fileCount} files would conflict. Check out \`${current}\` to merge and resolve them.`,
          mergeEnabled: false,
          ...base,
        };
      }
      return {
        kind: preview._tag,
        message: `There will be ${preview.fileCount} conflicted files.`,
        mergeEnabled: true,
        ...base,
      };
    case "unrelated-histories":
      return {
        kind: preview._tag,
        message: "These branches have unrelated histories and cannot be merged.",
        mergeEnabled: false,
        ...base,
      };
  }
}

export function resolveMergeConfirmCopy(
  mode: "merge" | "squash",
  target: string | null = null,
): {
  readonly title: string;
  readonly confirmLabel: string;
} {
  if (target !== null) return { title: `Merge into ${target}`, confirmLabel: "Merge" };
  return mode === "merge"
    ? { title: "Merge into current branch", confirmLabel: "Merge" }
    : { title: "Squash and merge into current branch", confirmLabel: "Squash and Merge" };
}
