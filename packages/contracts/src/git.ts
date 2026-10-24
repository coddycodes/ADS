import * as Effect from "effect/Effect";
import * as Schema from "effect/Schema";
import { NonNegativeInt, PositiveInt, ThreadId, TrimmedNonEmptyString } from "./baseSchemas.ts";
import { SourceControlProviderError, SourceControlProviderInfo } from "./sourceControl.ts";
import { VcsDriverKind, VcsRepositoryUnavailableReasonField } from "./vcs.ts";

const TrimmedNonEmptyStringSchema = TrimmedNonEmptyString;
const GIT_LIST_BRANCHES_MAX_LIMIT = 200;

// Domain Types

export const GitStackedAction = Schema.Literals([
  "commit",
  "push",
  "create_pr",
  "commit_push",
  "commit_push_pr",
]);
export type GitStackedAction = typeof GitStackedAction.Type;
export const GitActionProgressPhase = Schema.Literals(["branch", "commit", "push", "pr"]);
export type GitActionProgressPhase = typeof GitActionProgressPhase.Type;
export const GitActionProgressKind = Schema.Literals([
  "action_started",
  "phase_started",
  "hook_started",
  "hook_output",
  "hook_finished",
  "action_finished",
  "action_failed",
]);
export type GitActionProgressKind = typeof GitActionProgressKind.Type;
export const GitActionProgressStream = Schema.Literals(["stdout", "stderr"]);
export type GitActionProgressStream = typeof GitActionProgressStream.Type;
const GitCommitStepStatus = Schema.Literals([
  "created",
  "skipped_no_changes",
  "skipped_not_requested",
]);
const GitPushStepStatus = Schema.Literals([
  "pushed",
  "skipped_not_requested",
  "skipped_up_to_date",
]);
const GitBranchStepStatus = Schema.Literals(["created", "skipped_not_requested"]);
const GitPrStepStatus = Schema.Literals(["created", "opened_existing", "skipped_not_requested"]);
const VcsStatusChangeRequestState = Schema.Literals(["open", "closed", "merged"]);
const GitPullRequestReference = TrimmedNonEmptyStringSchema;
const GitPullRequestState = Schema.Literals(["open", "closed", "merged"]);
export const VcsWorkingTreeFileStatus = Schema.Literals([
  "modified",
  "added",
  "deleted",
  "renamed",
  "copied",
  "untracked",
  "conflicted",
]);
export type VcsWorkingTreeFileStatus = typeof VcsWorkingTreeFileStatus.Type;
/** Repository operation left in progress (MERGE_HEAD, rebase state, ...). */
export const VcsInProgressOperationKind = Schema.Literals([
  "merge",
  "rebase",
  "cherry-pick",
  "revert",
  "squash",
]);
export type VcsInProgressOperationKind = typeof VcsInProgressOperationKind.Type;
export const VcsStagingArea = Schema.Literals(["staged", "unstaged", "untracked"]);
export type VcsStagingArea = typeof VcsStagingArea.Type;
const GitPreparePullRequestThreadMode = Schema.Literal("local");
export const GitRunStackedActionToastRunAction = Schema.Struct({
  kind: GitStackedAction,
});
export type GitRunStackedActionToastRunAction = typeof GitRunStackedActionToastRunAction.Type;
const GitRunStackedActionToastCta = Schema.Union([
  Schema.Struct({
    kind: Schema.Literal("none"),
  }),
  Schema.Struct({
    kind: Schema.Literal("open_pr"),
    label: TrimmedNonEmptyStringSchema,
    url: Schema.String,
  }),
  Schema.Struct({
    kind: Schema.Literal("run_action"),
    label: TrimmedNonEmptyStringSchema,
    action: GitRunStackedActionToastRunAction,
  }),
]);
export type GitRunStackedActionToastCta = typeof GitRunStackedActionToastCta.Type;
const GitRunStackedActionToast = Schema.Struct({
  title: TrimmedNonEmptyStringSchema,
  description: Schema.optional(TrimmedNonEmptyStringSchema),
  cta: GitRunStackedActionToastCta,
});
export type GitRunStackedActionToast = typeof GitRunStackedActionToast.Type;

export const VcsRef = Schema.Struct({
  name: TrimmedNonEmptyStringSchema,
  isRemote: Schema.optional(Schema.Boolean),
  remoteName: Schema.optional(TrimmedNonEmptyStringSchema),
  current: Schema.Boolean,
  isDefault: Schema.Boolean,
  worktreePath: TrimmedNonEmptyStringSchema.pipe(Schema.NullOr),
});
export type VcsRef = typeof VcsRef.Type;

const GitResolvedPullRequest = Schema.Struct({
  number: PositiveInt,
  title: TrimmedNonEmptyStringSchema,
  url: Schema.String,
  baseBranch: TrimmedNonEmptyStringSchema,
  headBranch: TrimmedNonEmptyStringSchema,
  state: GitPullRequestState,
});
export type GitResolvedPullRequest = typeof GitResolvedPullRequest.Type;

// RPC Inputs

export const VcsStatusInput = Schema.Struct({
  cwd: TrimmedNonEmptyStringSchema,
});
export type VcsStatusInput = typeof VcsStatusInput.Type;

export const VcsPullInput = Schema.Struct({
  cwd: TrimmedNonEmptyStringSchema,
});
export type VcsPullInput = typeof VcsPullInput.Type;

export const VcsStageFilesInput = Schema.Struct({
  cwd: TrimmedNonEmptyStringSchema,
  filePaths: Schema.Array(TrimmedNonEmptyStringSchema).check(Schema.isMinLength(1)),
});
export type VcsStageFilesInput = typeof VcsStageFilesInput.Type;

export const VcsUnstageFilesInput = VcsStageFilesInput;
export type VcsUnstageFilesInput = typeof VcsUnstageFilesInput.Type;

export const VcsDiscardFilesInput = VcsStageFilesInput;
export type VcsDiscardFilesInput = typeof VcsDiscardFilesInput.Type;

const CreateOptionId = TrimmedNonEmptyStringSchema.check(Schema.isMaxLength(255));
/** Reviewed create-time options; merge options apply to GitLab only. */
export const GitPullRequestCreateOptions = Schema.Struct({
  draft: Schema.Boolean,
  assignees: Schema.Array(CreateOptionId).check(Schema.isMaxLength(20)),
  reviewers: Schema.Array(CreateOptionId).check(Schema.isMaxLength(20)),
  labels: Schema.Array(CreateOptionId).check(Schema.isMaxLength(50)),
  milestone: Schema.NullOr(Schema.Struct({ id: CreateOptionId, title: CreateOptionId })),
  removeSourceBranch: Schema.NullOr(Schema.Boolean),
  squash: Schema.NullOr(Schema.Boolean),
});
export type GitPullRequestCreateOptions = typeof GitPullRequestCreateOptions.Type;

export const GitRunStackedActionInput = Schema.Struct({
  actionId: TrimmedNonEmptyStringSchema,
  cwd: TrimmedNonEmptyStringSchema,
  action: GitStackedAction,
  commitMessage: Schema.optional(TrimmedNonEmptyStringSchema.check(Schema.isMaxLength(10_000))),
  featureBranch: Schema.optional(Schema.Boolean),
  filePaths: Schema.optional(
    Schema.Array(TrimmedNonEmptyStringSchema).check(Schema.isMinLength(1)),
  ),
  commitStagedIndexAsIs: Schema.optional(Schema.Boolean),
  /** Reviewed pull-request title; only meaningful for `create_pr` and `commit_push_pr`. */
  pullRequestTitle: Schema.optional(TrimmedNonEmptyStringSchema.check(Schema.isMaxLength(256))),
  /** Reviewed pull-request body; only meaningful for `create_pr` and `commit_push_pr`. */
  pullRequestBody: Schema.optional(Schema.String.check(Schema.isMaxLength(65_536))),
  /** Explicitly selected target; required for `create_pr` and `commit_push_pr`. */
  pullRequestBaseBranch: Schema.optional(TrimmedNonEmptyStringSchema),
  /** Selected source branch; publication does not change the current checkout. */
  pullRequestHeadBranch: Schema.optional(TrimmedNonEmptyStringSchema),
  /** Create-time options; requires capability `pullRequestCreateOptions`. */
  pullRequestOptions: Schema.optional(GitPullRequestCreateOptions),
});
export type GitRunStackedActionInput = typeof GitRunStackedActionInput.Type;

export const VcsListRefsInput = Schema.Struct({
  cwd: TrimmedNonEmptyStringSchema,
  query: Schema.optional(TrimmedNonEmptyStringSchema.check(Schema.isMaxLength(256))),
  cursor: Schema.optional(NonNegativeInt),
  includeMatchingRemoteRefs: Schema.optional(Schema.Boolean),
  refKind: Schema.optional(Schema.Literals(["all", "local", "remote"])),
  limit: Schema.optional(
    PositiveInt.check(Schema.isLessThanOrEqualTo(GIT_LIST_BRANCHES_MAX_LIMIT)),
  ),
});
export type VcsListRefsInput = typeof VcsListRefsInput.Type;

export const VcsCreateWorktreeInput = Schema.Struct({
  cwd: TrimmedNonEmptyStringSchema,
  refName: TrimmedNonEmptyStringSchema,
  newRefName: Schema.optional(Schema.NullOr(TrimmedNonEmptyStringSchema)),
  baseRefName: Schema.optional(Schema.NullOr(TrimmedNonEmptyStringSchema)),
  path: Schema.NullOr(TrimmedNonEmptyStringSchema),
});
export type VcsCreateWorktreeInput = typeof VcsCreateWorktreeInput.Type;

export const GitCloneInput = Schema.Struct({
  url: TrimmedNonEmptyStringSchema,
  parentDir: TrimmedNonEmptyStringSchema,
  directoryName: Schema.optional(TrimmedNonEmptyStringSchema),
  /** Join-only: never starts a clone. Sent only when the server advertises `vcsCloneReattach`. */
  attach: Schema.optional(Schema.Boolean),
  /** The caller leaving (socket close, Interrupt) does not stop a clone it started. */
  detach: Schema.optional(Schema.Boolean),
});
export type GitCloneInput = typeof GitCloneInput.Type;

export const GitCancelCloneInput = Schema.Struct({
  url: TrimmedNonEmptyStringSchema,
  parentDir: TrimmedNonEmptyStringSchema,
  directoryName: Schema.optional(TrimmedNonEmptyStringSchema),
});
export type GitCancelCloneInput = typeof GitCancelCloneInput.Type;

export const GitCancelCloneResult = Schema.Struct({
  cancelled: Schema.Boolean,
});
export type GitCancelCloneResult = typeof GitCancelCloneResult.Type;

export const GitPullRequestRefInput = Schema.Struct({
  cwd: TrimmedNonEmptyStringSchema,
  reference: GitPullRequestReference,
});
export type GitPullRequestRefInput = typeof GitPullRequestRefInput.Type;

export const GitPreparePullRequestThreadInput = Schema.Struct({
  cwd: TrimmedNonEmptyStringSchema,
  reference: GitPullRequestReference,
  mode: GitPreparePullRequestThreadMode,
});
export type GitPreparePullRequestThreadInput = typeof GitPreparePullRequestThreadInput.Type;

export const VcsRemoveWorktreeInput = Schema.Struct({
  cwd: TrimmedNonEmptyStringSchema,
  path: TrimmedNonEmptyStringSchema,
  force: Schema.optional(Schema.Boolean),
  ownerThreadId: ThreadId,
  threadIds: Schema.NonEmptyArray(ThreadId),
});
export type VcsRemoveWorktreeInput = typeof VcsRemoveWorktreeInput.Type;

export const VcsCreateRefInput = Schema.Struct({
  cwd: TrimmedNonEmptyStringSchema,
  refName: TrimmedNonEmptyStringSchema,
  switchRef: Schema.optional(Schema.Boolean),
});
export type VcsCreateRefInput = typeof VcsCreateRefInput.Type;

export const VcsCreateRefResult = Schema.Struct({
  refName: TrimmedNonEmptyStringSchema,
});
export type VcsCreateRefResult = typeof VcsCreateRefResult.Type;

export const VcsSwitchRefInput = Schema.Struct({
  cwd: TrimmedNonEmptyStringSchema,
  refName: TrimmedNonEmptyStringSchema,
});
export type VcsSwitchRefInput = typeof VcsSwitchRefInput.Type;

export const VcsInitInput = Schema.Struct({
  cwd: TrimmedNonEmptyStringSchema,
  kind: Schema.optional(VcsDriverKind),
});
export type VcsInitInput = typeof VcsInitInput.Type;

export const VcsGenerateCommitMessageInput = Schema.Struct({
  cwd: TrimmedNonEmptyStringSchema,
  filePaths: Schema.optional(
    Schema.Array(TrimmedNonEmptyStringSchema).check(Schema.isMinLength(1)),
  ),
});
export type VcsGenerateCommitMessageInput = typeof VcsGenerateCommitMessageInput.Type;

// RPC Results

const VcsStatusChangeRequest = Schema.Struct({
  number: PositiveInt,
  title: TrimmedNonEmptyStringSchema,
  url: Schema.String,
  baseRef: TrimmedNonEmptyStringSchema,
  headRef: TrimmedNonEmptyStringSchema,
  state: VcsStatusChangeRequestState,
});

const VcsStatusLocalShape = {
  isRepo: Schema.Boolean,
  repositoryUnavailableReason: VcsRepositoryUnavailableReasonField,
  sourceControlProvider: Schema.optional(SourceControlProviderInfo),
  hasPrimaryRemote: Schema.Boolean,
  isDefaultRef: Schema.Boolean,
  refName: Schema.NullOr(TrimmedNonEmptyStringSchema),
  /** Resolved default/base branch name (e.g. "main") — for `vs <base>` context. */
  defaultRefName: Schema.optional(TrimmedNonEmptyStringSchema),
  hasWorkingTreeChanges: Schema.Boolean,
  workingTree: Schema.Struct({
    files: Schema.Array(
      Schema.Struct({
        path: TrimmedNonEmptyStringSchema,
        insertions: NonNegativeInt,
        deletions: NonNegativeInt,
        // A status this client does not know, from a newer server, decodes as absent.
        status: Schema.optionalKey(
          VcsWorkingTreeFileStatus.pipe(Schema.catchDecoding(() => Effect.succeedNone)),
        ),
        area: Schema.optional(VcsStagingArea),
      }),
    ),
    insertions: NonNegativeInt,
    deletions: NonNegativeInt,
  }),
  /** Absent when no operation is in progress, and from older servers. */
  operationInProgress: Schema.optionalKey(
    VcsInProgressOperationKind.pipe(Schema.catchDecoding(() => Effect.succeedNone)),
  ),
};

const VcsStatusRemoteShape = {
  hasUpstream: Schema.Boolean,
  aheadCount: NonNegativeInt,
  behindCount: NonNegativeInt,
  aheadOfDefaultCount: Schema.optional(NonNegativeInt),
  pr: Schema.NullOr(VcsStatusChangeRequest),
};

export const VcsStatusLocalResult = Schema.Struct(VcsStatusLocalShape);
export type VcsStatusLocalResult = typeof VcsStatusLocalResult.Type;

export const VcsStatusRemoteResult = Schema.Struct(VcsStatusRemoteShape);
export type VcsStatusRemoteResult = typeof VcsStatusRemoteResult.Type;

export const VcsStatusResult = Schema.Struct({
  ...VcsStatusLocalShape,
  ...VcsStatusRemoteShape,
});
export type VcsStatusResult = typeof VcsStatusResult.Type;

export const VcsStatusStreamEvent = Schema.Union([
  Schema.TaggedStruct("snapshot", {
    local: VcsStatusLocalResult,
    remote: Schema.NullOr(VcsStatusRemoteResult),
  }),
  Schema.TaggedStruct("localUpdated", {
    local: VcsStatusLocalResult,
  }),
  Schema.TaggedStruct("remoteUpdated", {
    remote: Schema.NullOr(VcsStatusRemoteResult),
  }),
]);
export type VcsStatusStreamEvent = typeof VcsStatusStreamEvent.Type;

export const VcsListRefsResult = Schema.Struct({
  refs: Schema.Array(VcsRef),
  isRepo: Schema.Boolean,
  hasPrimaryRemote: Schema.Boolean,
  nextCursor: NonNegativeInt.pipe(Schema.NullOr),
  totalCount: NonNegativeInt,
});
export type VcsListRefsResult = typeof VcsListRefsResult.Type;

export const GitCloneResult = Schema.Struct({
  path: TrimmedNonEmptyStringSchema,
});
export type GitCloneResult = typeof GitCloneResult.Type;

export const GitResolvePullRequestResult = Schema.Struct({
  pullRequest: GitResolvedPullRequest,
});
export type GitResolvePullRequestResult = typeof GitResolvePullRequestResult.Type;

export const GitPreparePullRequestThreadResult = Schema.Struct({
  pullRequest: GitResolvedPullRequest,
  branch: TrimmedNonEmptyStringSchema,
});
export type GitPreparePullRequestThreadResult = typeof GitPreparePullRequestThreadResult.Type;

export const VcsSwitchRefResult = Schema.Struct({
  refName: Schema.NullOr(TrimmedNonEmptyStringSchema),
});
export type VcsSwitchRefResult = typeof VcsSwitchRefResult.Type;

export const GitRunStackedActionResult = Schema.Struct({
  action: GitStackedAction,
  branch: Schema.Struct({
    status: GitBranchStepStatus,
    name: Schema.optional(TrimmedNonEmptyStringSchema),
  }),
  commit: Schema.Struct({
    status: GitCommitStepStatus,
    commitSha: Schema.optional(TrimmedNonEmptyStringSchema),
    subject: Schema.optional(TrimmedNonEmptyStringSchema),
  }),
  push: Schema.Struct({
    status: GitPushStepStatus,
    branch: Schema.optional(TrimmedNonEmptyStringSchema),
    upstreamBranch: Schema.optional(TrimmedNonEmptyStringSchema),
    setUpstream: Schema.optional(Schema.Boolean),
  }),
  pr: Schema.Struct({
    status: GitPrStepStatus,
    url: Schema.optional(Schema.String),
    number: Schema.optional(PositiveInt),
    baseBranch: Schema.optional(TrimmedNonEmptyStringSchema),
    headBranch: Schema.optional(TrimmedNonEmptyStringSchema),
    title: Schema.optional(TrimmedNonEmptyStringSchema),
    /** The request exists, but some chosen options were not applied. */
    warning: Schema.optional(Schema.String.check(Schema.isMaxLength(1_000))),
  }),
  toast: GitRunStackedActionToast,
});
export type GitRunStackedActionResult = typeof GitRunStackedActionResult.Type;

export const VcsPullResult = Schema.Struct({
  status: Schema.Literals(["pulled", "skipped_up_to_date"]),
  refName: TrimmedNonEmptyStringSchema,
  upstreamRef: TrimmedNonEmptyStringSchema.pipe(Schema.NullOr),
});
export type VcsPullResult = typeof VcsPullResult.Type;

export const VcsCommit = Schema.Struct({
  sha: TrimmedNonEmptyStringSchema,
  shortSha: TrimmedNonEmptyStringSchema,
  subject: Schema.String,
  authorName: Schema.String,
  authoredAtMs: NonNegativeInt,
});
export type VcsCommit = typeof VcsCommit.Type;

export const VcsListCommitsInput = Schema.Struct({
  cwd: TrimmedNonEmptyStringSchema,
  limit: Schema.optional(PositiveInt.check(Schema.isLessThanOrEqualTo(200))),
  cursor: Schema.optional(NonNegativeInt),
});
export type VcsListCommitsInput = typeof VcsListCommitsInput.Type;

export const VcsListCommitsResult = Schema.Struct({
  commits: Schema.Array(VcsCommit),
  nextCursor: NonNegativeInt.pipe(Schema.NullOr),
});
export type VcsListCommitsResult = typeof VcsListCommitsResult.Type;

export const VcsGenerateCommitMessageResult = Schema.Struct({
  message: Schema.String,
});
export type VcsGenerateCommitMessageResult = typeof VcsGenerateCommitMessageResult.Type;

// RPC / domain errors
export class GitCommandError extends Schema.TaggedError<GitCommandError>()("GitCommandError", {
  operation: Schema.String,
  command: Schema.String,
  cwd: Schema.String,
  argumentCount: Schema.optional(Schema.Finite),
  exitCode: Schema.optional(Schema.Finite),
  stdoutLength: Schema.optional(Schema.Finite),
  stderrLength: Schema.optional(Schema.Finite),
  outputLength: Schema.optional(Schema.Finite),
  detail: Schema.String,
  cause: Schema.optional(Schema.Defect()),
}) {
  override get message(): string {
    return `Git command failed in ${this.operation} (${this.cwd}): ${this.detail}`;
  }
}

export const GitCloneOperationReason = Schema.Literals([
  "busy",
  "capacity",
  "shutting-down",
  "not-in-progress",
  "cancelled",
]);
export type GitCloneOperationReason = typeof GitCloneOperationReason.Type;

/**
 * The server's clone runtime refused or ended a clone. `destination` is the folder the clone
 * targets; a `busy` error never names the running clone's URL, which may embed credentials.
 */
export class GitCloneOperationError extends Schema.TaggedError<GitCloneOperationError>()(
  "GitCloneOperationError",
  {
    reason: GitCloneOperationReason,
    destination: Schema.String,
    message: Schema.String,
  },
) {}

export class TextGenerationError extends Schema.TaggedError<TextGenerationError>()(
  "TextGenerationError",
  {
    operation: Schema.String,
    detail: Schema.String,
    cause: Schema.optional(Schema.Defect()),
  },
) {
  override get message(): string {
    return `Text generation failed in ${this.operation}: ${this.detail}`;
  }
}

export class GitManagerError extends Schema.TaggedError<GitManagerError>()("GitManagerError", {
  operation: Schema.String,
  cwd: Schema.String,
  detail: Schema.String,
  cause: Schema.optional(Schema.Defect()),
}) {
  override get message(): string {
    return `Git manager failed in ${this.operation}: ${this.detail}`;
  }
}

export class GitPullRequestMaterializationError extends Schema.TaggedError<GitPullRequestMaterializationError>()(
  "GitPullRequestMaterializationError",
  {
    cwd: TrimmedNonEmptyStringSchema,
    pullRequestNumber: PositiveInt,
    headRepository: Schema.NullOr(TrimmedNonEmptyStringSchema),
    headBranch: TrimmedNonEmptyStringSchema,
    localBranch: TrimmedNonEmptyStringSchema,
    cause: Schema.Defect(),
  },
) {
  override get message(): string {
    return `Failed to materialize pull request #${this.pullRequestNumber} branch ${this.headBranch} as ${this.localBranch}.`;
  }
}

export const GitManagerServiceError = Schema.Union([
  GitManagerError,
  GitPullRequestMaterializationError,
  GitCommandError,
  SourceControlProviderError,
  TextGenerationError,
]);
export type GitManagerServiceError = typeof GitManagerServiceError.Type;

const GitActionProgressBase = Schema.Struct({
  actionId: TrimmedNonEmptyStringSchema,
  cwd: TrimmedNonEmptyStringSchema,
  action: GitStackedAction,
});

const GitActionStartedEvent = Schema.Struct({
  ...GitActionProgressBase.fields,
  kind: Schema.Literal("action_started"),
  phases: Schema.Array(GitActionProgressPhase),
});
const GitActionPhaseStartedEvent = Schema.Struct({
  ...GitActionProgressBase.fields,
  kind: Schema.Literal("phase_started"),
  phase: GitActionProgressPhase,
  label: TrimmedNonEmptyStringSchema,
});
const GitActionHookStartedEvent = Schema.Struct({
  ...GitActionProgressBase.fields,
  kind: Schema.Literal("hook_started"),
  hookName: TrimmedNonEmptyStringSchema,
});
const GitActionHookOutputEvent = Schema.Struct({
  ...GitActionProgressBase.fields,
  kind: Schema.Literal("hook_output"),
  hookName: Schema.NullOr(TrimmedNonEmptyStringSchema),
  stream: GitActionProgressStream,
  text: TrimmedNonEmptyStringSchema,
});
const GitActionHookFinishedEvent = Schema.Struct({
  ...GitActionProgressBase.fields,
  kind: Schema.Literal("hook_finished"),
  hookName: TrimmedNonEmptyStringSchema,
  exitCode: Schema.NullOr(Schema.Int),
  durationMs: Schema.NullOr(NonNegativeInt),
});
const GitActionFinishedEvent = Schema.Struct({
  ...GitActionProgressBase.fields,
  kind: Schema.Literal("action_finished"),
  result: GitRunStackedActionResult,
});
const GitActionFailedEvent = Schema.Struct({
  ...GitActionProgressBase.fields,
  kind: Schema.Literal("action_failed"),
  phase: Schema.NullOr(GitActionProgressPhase),
  message: TrimmedNonEmptyStringSchema,
});

export const GitActionProgressEvent = Schema.Union([
  GitActionStartedEvent,
  GitActionPhaseStartedEvent,
  GitActionHookStartedEvent,
  GitActionHookOutputEvent,
  GitActionHookFinishedEvent,
  GitActionFinishedEvent,
  GitActionFailedEvent,
]);
export type GitActionProgressEvent = typeof GitActionProgressEvent.Type;
