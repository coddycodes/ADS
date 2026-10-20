import * as Schema from "effect/Schema";

import { NonNegativeInt, PositiveInt, ProjectId, TrimmedNonEmptyString } from "./baseSchemas.ts";

const TrimmedNonEmptyStringSchema = TrimmedNonEmptyString;

export const GitManagerBlockedCode = Schema.Literals([
  "worktree-checked-out",
  "dirty-working-tree",
  "operation-in-flight",
  "merge-in-progress",
  "current-branch",
  "default-branch",
  "no-upstream",
  "detached-head",
  "no-remote",
]);
export type GitManagerBlockedCode = typeof GitManagerBlockedCode.Type;

export const GitManagerBlockedReason = Schema.Struct({
  operation: TrimmedNonEmptyStringSchema,
  code: GitManagerBlockedCode,
  message: TrimmedNonEmptyStringSchema,
});
export type GitManagerBlockedReason = typeof GitManagerBlockedReason.Type;

export const GitManagerWorktreeEntry = Schema.Struct({
  path: TrimmedNonEmptyStringSchema,
  headSha: TrimmedNonEmptyStringSchema,
  branch: Schema.NullOr(TrimmedNonEmptyStringSchema),
  isPrimary: Schema.Boolean,
  isBare: Schema.Boolean,
  isDetached: Schema.Boolean,
  locked: Schema.Boolean,
  lockReason: Schema.NullOr(TrimmedNonEmptyStringSchema),
  prunable: Schema.Boolean,
});
export type GitManagerWorktreeEntry = typeof GitManagerWorktreeEntry.Type;

export const GitManagerRefEntry = Schema.Struct({
  name: TrimmedNonEmptyStringSchema,
  tipSha: TrimmedNonEmptyStringSchema,
  upstream: Schema.NullOr(TrimmedNonEmptyStringSchema),
  ahead: NonNegativeInt,
  behind: NonNegativeInt,
  current: Schema.Boolean,
  isDefault: Schema.Boolean,
  worktreePath: Schema.NullOr(TrimmedNonEmptyStringSchema),
  blocked: Schema.Array(GitManagerBlockedReason),
});
export type GitManagerRefEntry = typeof GitManagerRefEntry.Type;

export const GitManagerInProgressOperation = Schema.Struct({
  kind: Schema.Literals(["merge", "rebase", "cherry-pick", "revert", "squash"]),
  current: Schema.NullOr(NonNegativeInt),
  total: Schema.NullOr(NonNegativeInt),
});
export type GitManagerInProgressOperation = typeof GitManagerInProgressOperation.Type;

export const GitManagerRefsSnapshot = Schema.Struct({
  generation: NonNegativeInt,
  headRef: Schema.NullOr(TrimmedNonEmptyStringSchema),
  detachedSha: Schema.NullOr(TrimmedNonEmptyStringSchema),
  isDirty: Schema.Boolean,
  defaultBranch: Schema.NullOr(TrimmedNonEmptyStringSchema),
  remotes: Schema.Array(TrimmedNonEmptyStringSchema),
  localBranches: Schema.Array(GitManagerRefEntry),
  remoteBranches: Schema.Array(GitManagerRefEntry),
  tags: Schema.Array(GitManagerRefEntry),
  worktrees: Schema.Array(GitManagerWorktreeEntry),
  inProgressOperation: Schema.NullOr(GitManagerInProgressOperation),
  conflictedPaths: Schema.Array(TrimmedNonEmptyStringSchema),
});
export type GitManagerRefsSnapshot = typeof GitManagerRefsSnapshot.Type;

export const GitManagerCommitEntry = Schema.Struct({
  sha: TrimmedNonEmptyStringSchema,
  shortSha: TrimmedNonEmptyStringSchema,
  parents: Schema.Array(TrimmedNonEmptyStringSchema),
  decorations: Schema.Array(TrimmedNonEmptyStringSchema),
  subject: Schema.String,
  body: Schema.String,
  authorName: Schema.String,
  authorEmail: Schema.String,
  authoredAtMs: NonNegativeInt,
  committerName: Schema.String,
  committerEmail: Schema.String,
  committedAtMs: NonNegativeInt,
  changedFiles: Schema.Array(TrimmedNonEmptyStringSchema),
});
export type GitManagerCommitEntry = typeof GitManagerCommitEntry.Type;

export const GitManagerCommitPage = Schema.Struct({
  generation: NonNegativeInt,
  pinnedTips: Schema.Array(TrimmedNonEmptyStringSchema),
  commits: Schema.Array(GitManagerCommitEntry),
  nextOffset: Schema.NullOr(NonNegativeInt),
  exhausted: Schema.Boolean,
  degradedToAllPaging: Schema.Boolean,
});
export type GitManagerCommitPage = typeof GitManagerCommitPage.Type;

export const GitManagerDiffSource = Schema.Union([
  Schema.TaggedStruct("working-tree", {
    path: TrimmedNonEmptyStringSchema,
    staged: Schema.Boolean,
  }),
  Schema.TaggedStruct("commit", {
    sha: TrimmedNonEmptyStringSchema,
    path: TrimmedNonEmptyStringSchema,
  }),
  Schema.TaggedStruct("stash", {
    sha: TrimmedNonEmptyStringSchema,
    path: TrimmedNonEmptyStringSchema,
  }),
]);
export type GitManagerDiffSource = typeof GitManagerDiffSource.Type;

const GitManagerDiffMetadata = {
  generation: NonNegativeInt,
  source: GitManagerDiffSource,
  byteLength: NonNegativeInt,
  longestLineLength: NonNegativeInt,
};

export const GitManagerImageDiffSide = Schema.Struct({
  contentBase64: Schema.NullOr(Schema.String),
  mimeType: Schema.NullOr(TrimmedNonEmptyStringSchema),
});
export type GitManagerImageDiffSide = typeof GitManagerImageDiffSide.Type;

export const GitManagerDiff = Schema.Union([
  Schema.TaggedStruct("patch", {
    ...GitManagerDiffMetadata,
    patch: Schema.String,
  }),
  Schema.TaggedStruct("large-text", GitManagerDiffMetadata),
  Schema.TaggedStruct("unrenderable", GitManagerDiffMetadata),
  Schema.TaggedStruct("image", {
    ...GitManagerDiffMetadata,
    before: GitManagerImageDiffSide,
    after: GitManagerImageDiffSide,
  }),
]);
export type GitManagerDiff = typeof GitManagerDiff.Type;

export const GitManagerChangedFileStatus = Schema.Literals([
  "modified",
  "added",
  "deleted",
  "renamed",
  "copied",
  "untracked",
  "unmerged",
]);
export type GitManagerChangedFileStatus = typeof GitManagerChangedFileStatus.Type;

export const GitManagerChangedFile = Schema.Struct({
  path: TrimmedNonEmptyStringSchema,
  status: GitManagerChangedFileStatus,
  insertions: NonNegativeInt,
  deletions: NonNegativeInt,
});
export type GitManagerChangedFile = typeof GitManagerChangedFile.Type;

export const GitManagerStashEntry = Schema.Struct({
  index: NonNegativeInt,
  sha: TrimmedNonEmptyStringSchema,
  message: Schema.String,
  committedAtMs: NonNegativeInt,
  parents: Schema.Array(TrimmedNonEmptyStringSchema),
  files: Schema.Array(GitManagerChangedFile),
});
export type GitManagerStashEntry = typeof GitManagerStashEntry.Type;

export const GitManagerConflictState = Schema.Struct({
  path: TrimmedNonEmptyStringSchema,
  kind: Schema.Literals(["text", "binary", "submodule"]),
  markerCount: NonNegativeInt,
  resolution: Schema.NullOr(Schema.Literals(["ours", "theirs"])),
});
export type GitManagerConflictState = typeof GitManagerConflictState.Type;

const GitManagerMergePreviewBase = {
  source: TrimmedNonEmptyStringSchema,
  current: TrimmedNonEmptyStringSchema,
  ahead: NonNegativeInt,
  behind: NonNegativeInt,
};

export const GitManagerMergePreview = Schema.Union([
  Schema.TaggedStruct("clean", GitManagerMergePreviewBase),
  Schema.TaggedStruct("conflicted", {
    ...GitManagerMergePreviewBase,
    fileCount: NonNegativeInt,
  }),
  Schema.TaggedStruct("unrelated-histories", GitManagerMergePreviewBase),
]);
export type GitManagerMergePreview = typeof GitManagerMergePreview.Type;

const GitManagerOperationBase = Schema.Struct({
  cwd: TrimmedNonEmptyStringSchema,
  projectId: ProjectId,
});
const GitManagerBranchPushBase = {
  ...GitManagerOperationBase.fields,
  remote: TrimmedNonEmptyStringSchema,
  localBranch: TrimmedNonEmptyStringSchema,
  remoteBranch: Schema.NullOr(TrimmedNonEmptyStringSchema),
  /** Push every local tag with the branch (`--tags --atomic`). Older clients omit it. */
  pushTags: Schema.optional(Schema.Boolean),
};
const GitManagerStashSelectionBase = {
  ...GitManagerOperationBase.fields,
  index: NonNegativeInt,
};

export const GitManagerOperationRequest = Schema.Union([
  Schema.TaggedStruct("branch-create", {
    ...GitManagerOperationBase.fields,
    name: TrimmedNonEmptyStringSchema,
    startPoint: Schema.NullOr(TrimmedNonEmptyStringSchema),
    checkout: Schema.Boolean,
  }),
  Schema.TaggedStruct("branch-checkout", {
    ...GitManagerOperationBase.fields,
    name: TrimmedNonEmptyStringSchema,
    strategy: Schema.NullOr(Schema.Literals(["stash", "bring"])),
  }),
  Schema.TaggedStruct("branch-rename", {
    ...GitManagerOperationBase.fields,
    name: TrimmedNonEmptyStringSchema,
    newName: TrimmedNonEmptyStringSchema,
  }),
  Schema.TaggedStruct("branch-delete", {
    ...GitManagerOperationBase.fields,
    name: TrimmedNonEmptyStringSchema,
    force: Schema.Boolean,
    deleteRemote: Schema.Boolean,
  }),
  Schema.TaggedStruct("fetch", {
    ...GitManagerOperationBase.fields,
    remote: TrimmedNonEmptyStringSchema,
  }),
  Schema.TaggedStruct("pull", {
    ...GitManagerOperationBase.fields,
    remote: TrimmedNonEmptyStringSchema,
  }),
  Schema.TaggedStruct("push", GitManagerBranchPushBase),
  Schema.TaggedStruct("publish-branch", GitManagerBranchPushBase),
  Schema.TaggedStruct("force-push", GitManagerBranchPushBase),
  Schema.TaggedStruct("stash-push", {
    ...GitManagerOperationBase.fields,
    message: TrimmedNonEmptyStringSchema,
    paths: Schema.Array(TrimmedNonEmptyStringSchema),
    /**
     * Also stash untracked files (`--include-untracked`); ignored files are never
     * stashed. When set, a push that saves nothing fails with `nothing-to-stash`
     * instead of finishing. Older clients omit it.
     */
    includeUntracked: Schema.optional(Schema.Boolean),
  }),
  Schema.TaggedStruct("stash-apply", GitManagerStashSelectionBase),
  Schema.TaggedStruct("stash-pop", GitManagerStashSelectionBase),
  Schema.TaggedStruct("stash-drop", GitManagerStashSelectionBase),
  Schema.TaggedStruct("merge", {
    ...GitManagerOperationBase.fields,
    source: TrimmedNonEmptyStringSchema,
    noVerify: Schema.Boolean,
  }),
  Schema.TaggedStruct("squash-merge", {
    ...GitManagerOperationBase.fields,
    source: TrimmedNonEmptyStringSchema,
    noVerify: Schema.Boolean,
  }),
  Schema.TaggedStruct("merge-into", {
    ...GitManagerOperationBase.fields,
    /** Full ref: `refs/heads/<name>` or `refs/remotes/<remote>/<name>`. */
    source: TrimmedNonEmptyStringSchema,
    /** Local branch updated without checking it out. */
    target: TrimmedNonEmptyStringSchema,
  }),
  Schema.TaggedStruct("rebase", {
    ...GitManagerOperationBase.fields,
    base: TrimmedNonEmptyStringSchema,
    target: TrimmedNonEmptyStringSchema,
  }),
  Schema.TaggedStruct("cherry-pick", {
    ...GitManagerOperationBase.fields,
    shas: Schema.NonEmptyArray(TrimmedNonEmptyStringSchema),
  }),
  Schema.TaggedStruct("squash", {
    ...GitManagerOperationBase.fields,
    shas: Schema.NonEmptyArray(TrimmedNonEmptyStringSchema),
    message: TrimmedNonEmptyStringSchema,
  }),
  Schema.TaggedStruct("reorder", {
    ...GitManagerOperationBase.fields,
    shas: Schema.NonEmptyArray(TrimmedNonEmptyStringSchema),
    insertBeforeSha: Schema.NullOr(TrimmedNonEmptyStringSchema),
  }),
  Schema.TaggedStruct("revert", {
    ...GitManagerOperationBase.fields,
    sha: TrimmedNonEmptyStringSchema,
  }),
  Schema.TaggedStruct("reset", {
    ...GitManagerOperationBase.fields,
    sha: TrimmedNonEmptyStringSchema,
    mode: Schema.Literals(["hard", "soft", "mixed"]),
  }),
  Schema.TaggedStruct("continue", {
    ...GitManagerOperationBase.fields,
    operation: Schema.Literals(["merge", "rebase", "cherry-pick", "revert"]),
  }),
  Schema.TaggedStruct("abort", {
    ...GitManagerOperationBase.fields,
    operation: Schema.Literals(["merge", "rebase", "cherry-pick", "revert"]),
  }),
  Schema.TaggedStruct("resolve-conflict", {
    ...GitManagerOperationBase.fields,
    path: TrimmedNonEmptyStringSchema,
    side: Schema.Literals(["ours", "theirs"]),
  }),
  Schema.TaggedStruct("tag-create", {
    ...GitManagerOperationBase.fields,
    name: TrimmedNonEmptyStringSchema,
    sha: TrimmedNonEmptyStringSchema,
    /** Push the new tag to this remote right after creating it. */
    pushRemote: Schema.optional(Schema.NullOr(TrimmedNonEmptyStringSchema)),
  }),
  Schema.TaggedStruct("tag-delete", {
    ...GitManagerOperationBase.fields,
    name: TrimmedNonEmptyStringSchema,
  }),
  Schema.TaggedStruct("tag-push", {
    ...GitManagerOperationBase.fields,
    name: TrimmedNonEmptyStringSchema,
    remote: TrimmedNonEmptyStringSchema,
  }),
]);
export type GitManagerOperationRequest = typeof GitManagerOperationRequest.Type;

export const GitManagerOperationEvent = Schema.Union([
  Schema.TaggedStruct("started", {
    operation: TrimmedNonEmptyStringSchema,
  }),
  Schema.TaggedStruct("output", {
    operation: TrimmedNonEmptyStringSchema,
    stream: Schema.Literals(["stdout", "stderr"]),
    text: Schema.String,
  }),
  Schema.TaggedStruct("finished", {
    operation: TrimmedNonEmptyStringSchema,
    message: TrimmedNonEmptyStringSchema,
  }),
  Schema.TaggedStruct("failed", {
    operation: TrimmedNonEmptyStringSchema,
    code: TrimmedNonEmptyStringSchema,
    message: TrimmedNonEmptyStringSchema,
    blocked: Schema.NullOr(GitManagerBlockedReason),
  }),
]);
export type GitManagerOperationEvent = typeof GitManagerOperationEvent.Type;

export const GitManagerSignalEvent = Schema.Struct({
  cwd: TrimmedNonEmptyStringSchema,
  /**
   * Advances once per changed repository signature. Generation 0 means no
   * signature has been computed yet; the first computed signature always
   * advances it, so clients can treat that step as initialization.
   */
  generation: NonNegativeInt,
  watcherDegraded: Schema.optional(Schema.Boolean),
});
export type GitManagerSignalEvent = typeof GitManagerSignalEvent.Type;

export const GitManagerCwdInput = Schema.Struct({
  cwd: TrimmedNonEmptyStringSchema,
});
export type GitManagerCwdInput = typeof GitManagerCwdInput.Type;

export const GitManagerGetCommitsInput = Schema.Struct({
  cwd: TrimmedNonEmptyStringSchema,
  pinnedTips: Schema.optional(Schema.Array(TrimmedNonEmptyStringSchema)),
  offset: Schema.optional(NonNegativeInt),
  limit: Schema.optional(PositiveInt.check(Schema.isLessThanOrEqualTo(100))),
});
export type GitManagerGetCommitsInput = typeof GitManagerGetCommitsInput.Type;

export const GitManagerGetDiffInput = Schema.Struct({
  cwd: TrimmedNonEmptyStringSchema,
  source: GitManagerDiffSource,
});
export type GitManagerGetDiffInput = typeof GitManagerGetDiffInput.Type;

export const GitManagerRemoteTagsInput = Schema.Struct({
  cwd: TrimmedNonEmptyStringSchema,
  remote: TrimmedNonEmptyStringSchema,
});
export type GitManagerRemoteTagsInput = typeof GitManagerRemoteTagsInput.Type;

export const GitManagerRemoteTag = Schema.Struct({
  name: TrimmedNonEmptyStringSchema,
  targetSha: TrimmedNonEmptyStringSchema,
});
export type GitManagerRemoteTag = typeof GitManagerRemoteTag.Type;

/**
 * The tags one remote advertises. An unreachable or rejecting remote is an
 * `unavailable` result with a reason, not an RPC failure.
 */
export const GitManagerRemoteTags = Schema.Struct({
  remote: TrimmedNonEmptyStringSchema,
  status: Schema.Literals(["available", "unavailable"]),
  reason: Schema.NullOr(Schema.String),
  tags: Schema.Array(GitManagerRemoteTag),
});
export type GitManagerRemoteTags = typeof GitManagerRemoteTags.Type;

export const GitManagerPreviewMergeInput = Schema.Struct({
  cwd: TrimmedNonEmptyStringSchema,
  source: TrimmedNonEmptyStringSchema,
  /** Compare against this local branch instead of HEAD; servers without merge-into ignore it. */
  target: Schema.optionalKey(TrimmedNonEmptyStringSchema),
});
export type GitManagerPreviewMergeInput = typeof GitManagerPreviewMergeInput.Type;

export const GitManagerCoAuthor = Schema.Struct({
  name: TrimmedNonEmptyStringSchema,
  email: TrimmedNonEmptyStringSchema,
});
export type GitManagerCoAuthor = typeof GitManagerCoAuthor.Type;

export const GitManagerCommitInput = Schema.Struct({
  cwd: TrimmedNonEmptyStringSchema,
  summary: TrimmedNonEmptyStringSchema,
  description: Schema.String,
  amend: Schema.Boolean,
  noVerify: Schema.Boolean,
  signoff: Schema.Boolean,
  allowEmpty: Schema.Boolean,
  coAuthors: Schema.Array(GitManagerCoAuthor),
});
export type GitManagerCommitInput = typeof GitManagerCommitInput.Type;

export const GitManagerCommitResult = Schema.Struct({
  sha: Schema.NullOr(TrimmedNonEmptyStringSchema),
  empty: Schema.Boolean,
});
export type GitManagerCommitResult = typeof GitManagerCommitResult.Type;

export const GitManagerUndoCommitResult = Schema.Struct({
  summary: TrimmedNonEmptyStringSchema,
  description: Schema.String,
  coAuthors: Schema.Array(GitManagerCoAuthor),
});
export type GitManagerUndoCommitResult = typeof GitManagerUndoCommitResult.Type;

export const GitManagerDiscardInput = Schema.Struct({
  cwd: TrimmedNonEmptyStringSchema,
  paths: Schema.NonEmptyArray(TrimmedNonEmptyStringSchema),
  permitPermanent: Schema.Boolean,
});
export type GitManagerDiscardInput = typeof GitManagerDiscardInput.Type;

export const GitManagerDiscardResult = Schema.Struct({
  trashed: Schema.Array(TrimmedNonEmptyStringSchema),
  permanentlyDiscarded: Schema.Array(TrimmedNonEmptyStringSchema),
  trashUnavailable: Schema.Array(TrimmedNonEmptyStringSchema),
});
export type GitManagerDiscardResult = typeof GitManagerDiscardResult.Type;

export const GitManagerPartialSelectionInput = Schema.Struct({
  cwd: TrimmedNonEmptyStringSchema,
  projectId: ProjectId,
  path: TrimmedNonEmptyStringSchema,
  selectedLines: Schema.NonEmptyArray(NonNegativeInt),
  baseGeneration: NonNegativeInt,
});
export type GitManagerPartialSelectionInput = typeof GitManagerPartialSelectionInput.Type;

export const GitManagerPartialSelectionResult = Schema.Struct({
  generation: NonNegativeInt,
});
export type GitManagerPartialSelectionResult = typeof GitManagerPartialSelectionResult.Type;

export const GitManagerPullRequestEntry = Schema.Struct({
  number: PositiveInt,
  title: TrimmedNonEmptyStringSchema,
  url: Schema.String,
  baseBranch: TrimmedNonEmptyStringSchema,
  headBranch: TrimmedNonEmptyStringSchema,
  state: Schema.Literals(["open", "closed", "merged"]),
});
export type GitManagerPullRequestEntry = typeof GitManagerPullRequestEntry.Type;

export const GitManagerCheckEntry = Schema.Struct({
  name: TrimmedNonEmptyStringSchema,
  state: TrimmedNonEmptyStringSchema,
  link: Schema.NullOr(Schema.String),
  workflow: Schema.NullOr(TrimmedNonEmptyStringSchema),
});
export type GitManagerCheckEntry = typeof GitManagerCheckEntry.Type;

export const GitManagerPullRequestsResult = Schema.Struct({
  status: Schema.Literals(["available", "unavailable"]),
  pullRequests: Schema.Array(GitManagerPullRequestEntry),
  checks: Schema.Array(GitManagerCheckEntry),
});
export type GitManagerPullRequestsResult = typeof GitManagerPullRequestsResult.Type;

export class GitManagerOperationError extends Schema.TaggedError<GitManagerOperationError>()(
  "GitManagerOperationError",
  {
    operation: TrimmedNonEmptyStringSchema,
    code: TrimmedNonEmptyStringSchema,
    message: TrimmedNonEmptyStringSchema,
    blocked: Schema.NullOr(GitManagerBlockedReason),
  },
) {}
