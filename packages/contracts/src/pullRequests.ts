import * as Schema from "effect/Schema";

import { TrimmedNonEmptyString } from "./baseSchemas.ts";
import { GitManagerBlockedReason } from "./gitManager.ts";
import { SourceControlProviderKind } from "./sourceControl.ts";

const TrimmedNonEmptyStringSchema = TrimmedNonEmptyString;

export const PullRequestsCwdInput = Schema.Struct({ cwd: TrimmedNonEmptyStringSchema });
export type PullRequestsCwdInput = typeof PullRequestsCwdInput.Type;

/**
 * `rescan` bypasses the server's bounded context caches; only Rescan and auth recovery set it.
 * `allowStale` lets the server answer from its stored context (marked `stale`); a live read follows.
 */
export const PullRequestsGetContextInput = Schema.Struct({
  ...PullRequestsCwdInput.fields,
  rescan: Schema.optional(Schema.Boolean),
  allowStale: Schema.optional(Schema.Boolean),
});
export type PullRequestsGetContextInput = typeof PullRequestsGetContextInput.Type;

export const PullRequestsNumberInput = Schema.Struct({
  ...PullRequestsCwdInput.fields,
  number: Schema.Number,
});
export type PullRequestsNumberInput = typeof PullRequestsNumberInput.Type;

export const PullRequestsProviderKind = Schema.Literals(["github", "gitlab"]);
export type PullRequestsProviderKind = typeof PullRequestsProviderKind.Type;

export const PullRequestsActor = Schema.Struct({
  login: TrimmedNonEmptyStringSchema,
  name: Schema.NullOr(Schema.String),
  isBot: Schema.Boolean,
});
export type PullRequestsActor = typeof PullRequestsActor.Type;

export const PullRequestsLabel = Schema.Struct({
  name: TrimmedNonEmptyStringSchema,
  color: Schema.NullOr(Schema.String),
  description: Schema.NullOr(Schema.String),
});
export type PullRequestsLabel = typeof PullRequestsLabel.Type;

export const PullRequestsMilestone = Schema.Struct({
  id: TrimmedNonEmptyStringSchema,
  title: TrimmedNonEmptyStringSchema,
  dueOn: Schema.NullOr(Schema.String),
});
export type PullRequestsMilestone = typeof PullRequestsMilestone.Type;

export const PullRequestsReviewer = Schema.Struct({
  actor: PullRequestsActor,
  state: Schema.Literals([
    "unreviewed",
    "commented",
    "approved",
    "changes_requested",
    "dismissed",
    "review_started",
  ]),
  canRerequest: Schema.Boolean,
});
export type PullRequestsReviewer = typeof PullRequestsReviewer.Type;

export const PullRequestsState = Schema.Literals(["open", "closed", "merged"]);
export type PullRequestsState = typeof PullRequestsState.Type;

export const PullRequestsMergeMethod = Schema.Literals(["merge", "squash", "rebase"]);
export type PullRequestsMergeMethod = typeof PullRequestsMergeMethod.Type;

export const PullRequestsPermission = Schema.Struct({
  allowed: Schema.Boolean,
  reason: Schema.NullOr(Schema.String),
});
export type PullRequestsPermission = typeof PullRequestsPermission.Type;

export const PullRequestsPermissions = Schema.Struct({
  comment: PullRequestsPermission,
  editOwnComment: PullRequestsPermission,
  deleteOwnComment: PullRequestsPermission,
  minimizeComment: PullRequestsPermission,
  react: PullRequestsPermission,
  resolveThreads: PullRequestsPermission,
  review: PullRequestsPermission,
  approve: PullRequestsPermission,
  requestChanges: PullRequestsPermission,
  revokeApproval: PullRequestsPermission,
  removeOwnChangeRequest: PullRequestsPermission,
  dismissReview: PullRequestsPermission,
  rerequestReview: PullRequestsPermission,
  applySuggestion: PullRequestsPermission,
  editPullRequest: PullRequestsPermission,
  editReviewers: PullRequestsPermission,
  editAssignees: PullRequestsPermission,
  editLabels: PullRequestsPermission,
  editMilestone: PullRequestsPermission,
  lock: PullRequestsPermission,
  unlock: PullRequestsPermission,
  updateBranch: Schema.Struct({
    ...PullRequestsPermission.fields,
    methods: Schema.Array(Schema.Literals(["merge", "rebase"])),
  }),
  merge: Schema.Struct({
    ...PullRequestsPermission.fields,
    methods: Schema.Array(PullRequestsMergeMethod),
    defaultMethod: Schema.NullOr(PullRequestsMergeMethod),
    deleteBranchDefault: Schema.Boolean,
  }),
  mergeBypass: PullRequestsPermission,
  enableAutoMerge: PullRequestsPermission,
  disableAutoMerge: PullRequestsPermission,
  markReady: PullRequestsPermission,
  convertToDraft: PullRequestsPermission,
  close: PullRequestsPermission,
  reopen: PullRequestsPermission,
  delete: PullRequestsPermission,
  revert: PullRequestsPermission,
  checkout: PullRequestsPermission,
});
export type PullRequestsPermissions = typeof PullRequestsPermissions.Type;

export const PullRequestsHostCapabilities = Schema.Struct({
  vocabulary: Schema.Struct({
    // Compatibility-only request nouns: both fields remain required for older
    // clients. Current clients derive them from the provider kind through
    // @bibcode/shared/sourceControl instead of using this server copy.
    pullRequest: TrimmedNonEmptyStringSchema,
    pullRequests: TrimmedNonEmptyStringSchema,
    checks: TrimmedNonEmptyStringSchema,
    filesChanged: TrimmedNonEmptyStringSchema,
    reviewer: TrimmedNonEmptyStringSchema,
    approve: TrimmedNonEmptyStringSchema,
    requestChanges: TrimmedNonEmptyStringSchema,
  }),
  requestChanges: Schema.Boolean,
  revokeApproval: Schema.Boolean,
  removeOwnChangeRequest: Schema.Boolean,
  dismissReview: Schema.Boolean,
  applySuggestion: Schema.Boolean,
  minimizeComment: Schema.Boolean,
  deletePullRequest: Schema.Boolean,
  lockReasons: Schema.Array(Schema.String),
  updateBranchMethods: Schema.Array(Schema.Literals(["merge", "rebase"])),
  mergeMethodsSource: Schema.Literals(["per_pull_request", "project_setting"]),
  autoMergeLabel: Schema.String,
  reviewerStates: Schema.Boolean,
  closedTabIncludesMerged: Schema.Boolean,
});
export type PullRequestsHostCapabilities = typeof PullRequestsHostCapabilities.Type;

export const PullRequestsContext = Schema.Union([
  Schema.Struct({
    status: Schema.Literal("available"),
    provider: PullRequestsProviderKind,
    host: TrimmedNonEmptyStringSchema,
    hostVersion: Schema.NullOr(Schema.String),
    repository: TrimmedNonEmptyStringSchema,
    defaultBranch: TrimmedNonEmptyStringSchema,
    account: PullRequestsActor,
    repositoryPermission: Schema.Literals(["none", "read", "triage", "write", "maintain", "admin"]),
    mergePolicy: Schema.Struct({
      methods: Schema.Array(PullRequestsMergeMethod),
      defaultMethod: Schema.NullOr(PullRequestsMergeMethod),
      deleteBranchDefault: Schema.Boolean,
      autoMergeAllowed: Schema.Boolean,
      requiresPipelineSuccess: Schema.Boolean,
      requiresResolvedDiscussions: Schema.Boolean,
    }),
    capabilities: PullRequestsHostCapabilities,
    webUrl: TrimmedNonEmptyStringSchema,
    /** Answered from the server's stored copy (`allowStale`); a live read follows. */
    stale: Schema.optional(Schema.Boolean),
  }),
  Schema.Struct({
    status: Schema.Literal("unavailable"),
    code: Schema.Literals([
      "no_remote",
      "unsupported_provider",
      "unknown_host",
      "cli_missing",
      "not_authenticated",
      "repository_unreachable",
      "cli_too_old",
    ]),
    message: TrimmedNonEmptyStringSchema,
    provider: Schema.NullOr(SourceControlProviderKind),
    host: Schema.NullOr(Schema.String),
    installHint: Schema.NullOr(Schema.String),
    authCommand: Schema.NullOr(Schema.String),
  }),
]);
export type PullRequestsContext = typeof PullRequestsContext.Type;

const VocabularyKind = Schema.Literals(["labels", "milestones", "users", "branches"]);

export const PullRequestsVocabularyInput = Schema.Struct({
  ...PullRequestsCwdInput.fields,
  kind: VocabularyKind,
  query: Schema.NullOr(Schema.String),
});
export type PullRequestsVocabularyInput = typeof PullRequestsVocabularyInput.Type;

export const PullRequestsVocabulary = Schema.Struct({
  kind: VocabularyKind,
  entries: Schema.Array(
    Schema.Struct({
      id: TrimmedNonEmptyStringSchema,
      label: TrimmedNonEmptyStringSchema,
      color: Schema.NullOr(Schema.String),
      description: Schema.NullOr(Schema.String),
    }),
  ),
  truncated: Schema.Boolean,
});
export type PullRequestsVocabulary = typeof PullRequestsVocabulary.Type;

export const PullRequestsSquashOption = Schema.Literals([
  "never",
  "always",
  "default_on",
  "default_off",
]);
export type PullRequestsSquashOption = typeof PullRequestsSquashOption.Type;

/** What the create dialog prefills: the viewer for "Assign to me" and GitLab's merge defaults. */
export const PullRequestsCreateDefaults = Schema.Struct({
  viewer: Schema.NullOr(
    Schema.Struct({ id: TrimmedNonEmptyStringSchema, label: TrimmedNonEmptyStringSchema }),
  ),
  squash: Schema.NullOr(PullRequestsSquashOption),
  removeSourceBranch: Schema.NullOr(Schema.Boolean),
});
export type PullRequestsCreateDefaults = typeof PullRequestsCreateDefaults.Type;

export const PullRequestsListInput = Schema.Struct({
  ...PullRequestsCwdInput.fields,
  state: Schema.Literals(["open", "closed", "merged", "all"]),
  search: Schema.NullOr(Schema.String),
  author: Schema.NullOr(Schema.String),
  assignee: Schema.NullOr(Schema.String),
  reviewer: Schema.NullOr(Schema.String),
  reviewStatus: Schema.NullOr(
    Schema.Literals(["review_required", "approved", "changes_requested", "not_approved"]),
  ),
  draft: Schema.NullOr(Schema.Literals(["only", "exclude"])),
  labels: Schema.Array(TrimmedNonEmptyStringSchema),
  milestone: Schema.NullOr(Schema.String),
  targetBranch: Schema.NullOr(Schema.String),
  sort: Schema.Literals(["newest", "oldest", "recently_updated", "most_commented"]),
  cursor: Schema.NullOr(Schema.String),
  /** Re-read the repository-wide tab totals instead of reusing the server's bounded copy. */
  refreshTotals: Schema.optional(Schema.Boolean),
});
export type PullRequestsListInput = typeof PullRequestsListInput.Type;

export const PullRequestsListRow = Schema.Struct({
  number: Schema.Number,
  title: TrimmedNonEmptyStringSchema,
  state: PullRequestsState,
  isDraft: Schema.Boolean,
  author: PullRequestsActor,
  createdAt: Schema.String,
  updatedAt: Schema.String,
  mergedAt: Schema.NullOr(Schema.String),
  closedAt: Schema.NullOr(Schema.String),
  headBranch: TrimmedNonEmptyStringSchema,
  baseBranch: TrimmedNonEmptyStringSchema,
  labels: Schema.Array(PullRequestsLabel),
  reviewDecision: Schema.NullOr(
    Schema.Literals(["review_required", "approved", "changes_requested"]),
  ),
  checksSummary: Schema.NullOr(Schema.Literals(["pending", "success", "failure", "neutral"])),
  commentCount: Schema.Number,
  approvals: Schema.NullOr(Schema.Struct({ approved: Schema.Number, required: Schema.Number })),
  unresolvedThreads: Schema.NullOr(Schema.Number),
  url: TrimmedNonEmptyStringSchema,
});
export type PullRequestsListRow = typeof PullRequestsListRow.Type;

export const PullRequestsListPage = Schema.Struct({
  rows: Schema.Array(PullRequestsListRow),
  nextCursor: Schema.NullOr(Schema.String),
  totalCount: Schema.NullOr(Schema.Number),
  counts: Schema.NullOr(
    Schema.Struct({
      open: Schema.NullOr(Schema.Number),
      closed: Schema.NullOr(Schema.Number),
      merged: Schema.NullOr(Schema.Number),
    }),
  ),
});
export type PullRequestsListPage = typeof PullRequestsListPage.Type;

export const PullRequestsMergeReadiness = Schema.Struct({
  status: Schema.Literals([
    "mergeable",
    "checks_pending",
    "checks_failing",
    "review_required",
    "changes_requested",
    "conflicts",
    "behind",
    "blocked",
    "draft",
    "merged",
    "closed",
    "unknown",
  ]),
  summary: Schema.String,
  details: Schema.Array(Schema.String),
  requiredApprovals: Schema.NullOr(
    Schema.Struct({ approved: Schema.Number, required: Schema.Number }),
  ),
  autoMerge: Schema.NullOr(
    Schema.Struct({ enabled: Schema.Boolean, method: Schema.NullOr(PullRequestsMergeMethod) }),
  ),
  headSha: TrimmedNonEmptyStringSchema,
});
export type PullRequestsMergeReadiness = typeof PullRequestsMergeReadiness.Type;

const ReactionContent = Schema.Literals([
  "+1",
  "-1",
  "laugh",
  "confused",
  "heart",
  "hooray",
  "rocket",
  "eyes",
]);

export const PullRequestsReactionSummary = Schema.Array(
  Schema.Struct({
    content: ReactionContent,
    count: Schema.Number,
    viewerReacted: Schema.Boolean,
  }),
);
export type PullRequestsReactionSummary = typeof PullRequestsReactionSummary.Type;

export const PullRequestsDetail = Schema.Struct({
  number: Schema.Number,
  title: TrimmedNonEmptyStringSchema,
  body: Schema.String,
  state: PullRequestsState,
  isDraft: Schema.Boolean,
  locked: Schema.Boolean,
  lockReason: Schema.NullOr(Schema.String),
  author: PullRequestsActor,
  createdAt: Schema.String,
  updatedAt: Schema.String,
  mergedAt: Schema.NullOr(Schema.String),
  closedAt: Schema.NullOr(Schema.String),
  mergedBy: Schema.NullOr(PullRequestsActor),
  headBranch: TrimmedNonEmptyStringSchema,
  baseBranch: TrimmedNonEmptyStringSchema,
  headSha: TrimmedNonEmptyStringSchema,
  baseSha: TrimmedNonEmptyStringSchema,
  isCrossRepository: Schema.Boolean,
  headRepository: Schema.NullOr(Schema.String),
  maintainerCanModify: Schema.Boolean,
  commitCount: Schema.Number,
  changedFiles: Schema.Number,
  additions: Schema.Number,
  deletions: Schema.Number,
  labels: Schema.Array(PullRequestsLabel),
  milestone: Schema.NullOr(PullRequestsMilestone),
  assignees: Schema.Array(PullRequestsActor),
  reviewers: Schema.Array(PullRequestsReviewer),
  approvalRules: Schema.Array(
    Schema.Struct({
      name: TrimmedNonEmptyStringSchema,
      approved: Schema.Number,
      required: Schema.Number,
      approvers: Schema.Array(PullRequestsActor),
    }),
  ),
  linkedIssues: Schema.Array(
    Schema.Struct({
      reference: TrimmedNonEmptyStringSchema,
      title: Schema.NullOr(Schema.String),
      url: TrimmedNonEmptyStringSchema,
    }),
  ),
  reactions: PullRequestsReactionSummary,
  readiness: PullRequestsMergeReadiness,
  permissions: PullRequestsPermissions,
  url: TrimmedNonEmptyStringSchema,
  tabCounts: Schema.Struct({
    conversation: Schema.NullOr(Schema.Number),
    commits: Schema.Number,
    checks: Schema.NullOr(Schema.Number),
    files: Schema.Number,
  }),
});
export type PullRequestsDetail = typeof PullRequestsDetail.Type;

const DiffSide = Schema.Literals(["left", "right"]);

export const PullRequestsTimelineItem = Schema.Union([
  Schema.Struct({
    kind: Schema.Literal("comment"),
    id: TrimmedNonEmptyStringSchema,
    author: PullRequestsActor,
    body: Schema.String,
    createdAt: Schema.String,
    updatedAt: Schema.String,
    viewerIsAuthor: Schema.Boolean,
    minimized: Schema.Boolean,
    reactions: PullRequestsReactionSummary,
  }),
  Schema.Struct({
    kind: Schema.Literal("review"),
    id: TrimmedNonEmptyStringSchema,
    author: PullRequestsActor,
    state: Schema.Literals(["commented", "approved", "changes_requested", "dismissed", "pending"]),
    body: Schema.String,
    submittedAt: Schema.String,
    commitSha: Schema.NullOr(Schema.String),
    viewerIsAuthor: Schema.Boolean,
    canDismiss: Schema.Boolean,
  }),
  Schema.Struct({
    kind: Schema.Literal("thread"),
    id: TrimmedNonEmptyStringSchema,
    path: TrimmedNonEmptyStringSchema,
    line: Schema.NullOr(Schema.Number),
    startLine: Schema.NullOr(Schema.Number),
    side: DiffSide,
    isResolved: Schema.Boolean,
    isOutdated: Schema.Boolean,
    canResolve: Schema.Boolean,
    diffHunk: Schema.NullOr(Schema.String),
    comments: Schema.Array(
      Schema.Struct({
        id: TrimmedNonEmptyStringSchema,
        author: PullRequestsActor,
        body: Schema.String,
        createdAt: Schema.String,
        updatedAt: Schema.String,
        viewerIsAuthor: Schema.Boolean,
        minimized: Schema.Boolean,
        reactions: PullRequestsReactionSummary,
        suggestion: Schema.NullOr(
          Schema.Struct({
            id: Schema.NullOr(Schema.String),
            applicable: Schema.Boolean,
            fromLine: Schema.Number,
            toLine: Schema.Number,
            fromContent: Schema.String,
            toContent: Schema.String,
          }),
        ),
      }),
    ),
  }),
  Schema.Struct({
    kind: Schema.Literal("event"),
    id: TrimmedNonEmptyStringSchema,
    actor: Schema.NullOr(PullRequestsActor),
    event: Schema.String,
    detail: Schema.NullOr(Schema.String),
    createdAt: Schema.String,
  }),
]);
export type PullRequestsTimelineItem = typeof PullRequestsTimelineItem.Type;

export const PullRequestsTimeline = Schema.Struct({
  items: Schema.Array(PullRequestsTimelineItem),
  truncated: Schema.Boolean,
});
export type PullRequestsTimeline = typeof PullRequestsTimeline.Type;

export const PullRequestsCommits = Schema.Struct({
  commits: Schema.Array(
    Schema.Struct({
      sha: TrimmedNonEmptyStringSchema,
      shortSha: TrimmedNonEmptyStringSchema,
      subject: Schema.String,
      body: Schema.NullOr(Schema.String),
      author: PullRequestsActor,
      authoredAt: Schema.String,
      url: TrimmedNonEmptyStringSchema,
    }),
  ),
});
export type PullRequestsCommits = typeof PullRequestsCommits.Type;

export const PullRequestsChecks = Schema.Struct({
  groups: Schema.Array(
    Schema.Struct({
      name: TrimmedNonEmptyStringSchema,
      checks: Schema.Array(
        Schema.Struct({
          name: TrimmedNonEmptyStringSchema,
          state: Schema.Literals([
            "pending",
            "success",
            "failure",
            "cancelled",
            "skipped",
            "neutral",
          ]),
          url: Schema.NullOr(Schema.String),
          startedAt: Schema.NullOr(Schema.String),
          completedAt: Schema.NullOr(Schema.String),
          durationSeconds: Schema.NullOr(Schema.Number),
        }),
      ),
    }),
  ),
  summary: Schema.Literals(["pending", "success", "failure", "neutral", "none"]),
  pipelineUrl: Schema.NullOr(Schema.String),
});
export type PullRequestsChecks = typeof PullRequestsChecks.Type;

export const PullRequestsFile = Schema.Struct({
  path: TrimmedNonEmptyStringSchema,
  previousPath: Schema.NullOr(Schema.String),
  changeType: Schema.Literals(["added", "modified", "removed", "renamed", "copied", "binary"]),
  additions: Schema.Number,
  deletions: Schema.Number,
  patch: Schema.NullOr(Schema.String),
  tooLarge: Schema.Boolean,
});
export type PullRequestsFile = typeof PullRequestsFile.Type;

export const PullRequestsFiles = Schema.Struct({
  files: Schema.Array(PullRequestsFile),
  diffRefs: Schema.Struct({
    baseSha: TrimmedNonEmptyStringSchema,
    startSha: TrimmedNonEmptyStringSchema,
    headSha: TrimmedNonEmptyStringSchema,
  }),
  truncated: Schema.Boolean,
});
export type PullRequestsFiles = typeof PullRequestsFiles.Type;

export const PullRequestsSubscribeTab = Schema.Literals([
  "conversation",
  "commits",
  "checks",
  "files",
]);
export type PullRequestsSubscribeTab = typeof PullRequestsSubscribeTab.Type;

export const PullRequestsSubscribeInput = Schema.Struct({
  ...PullRequestsListInput.fields,
  number: Schema.NullOr(Schema.Number),
  tab: Schema.NullOr(PullRequestsSubscribeTab),
});
export type PullRequestsSubscribeInput = typeof PullRequestsSubscribeInput.Type;

const PullRequestsSnapshotList = Schema.Struct({
  payload: PullRequestsListPage,
  observedAt: Schema.Number,
});

const PullRequestsSnapshotDetail = Schema.Struct({
  payload: PullRequestsDetail,
  observedAt: Schema.Number,
});

export const PullRequestsSnapshotTab = Schema.Struct({
  kind: PullRequestsSubscribeTab,
  payload: Schema.Union([
    PullRequestsTimeline,
    PullRequestsCommits,
    PullRequestsChecks,
    PullRequestsFiles,
  ]),
  observedAt: Schema.Number,
});
export type PullRequestsSnapshotTab = typeof PullRequestsSnapshotTab.Type;

export const PullRequestsSnapshot = Schema.Struct({
  list: Schema.NullOr(PullRequestsSnapshotList),
  detail: Schema.NullOr(PullRequestsSnapshotDetail),
  tab: Schema.NullOr(PullRequestsSnapshotTab),
});
export type PullRequestsSnapshot = typeof PullRequestsSnapshot.Type;

export const PullRequestsChanged = Schema.Struct({
  list: Schema.Boolean,
  detail: Schema.Boolean,
  timeline: Schema.Boolean,
  commits: Schema.Boolean,
  checks: Schema.Boolean,
  files: Schema.Boolean,
});
export type PullRequestsChanged = typeof PullRequestsChanged.Type;

export const PullRequestsActionRequest = Schema.Union([
  Schema.Struct({
    ...PullRequestsNumberInput.fields,
    action: Schema.Literal("comment"),
    body: Schema.String,
  }),
  Schema.Struct({
    ...PullRequestsNumberInput.fields,
    action: Schema.Literal("editComment"),
    commentId: TrimmedNonEmptyStringSchema,
    body: Schema.String,
  }),
  Schema.Struct({
    ...PullRequestsNumberInput.fields,
    action: Schema.Literal("deleteComment"),
    commentId: TrimmedNonEmptyStringSchema,
  }),
  Schema.Struct({
    ...PullRequestsNumberInput.fields,
    action: Schema.Literal("minimizeComment"),
    commentId: TrimmedNonEmptyStringSchema,
    minimized: Schema.Boolean,
  }),
  Schema.Struct({
    ...PullRequestsNumberInput.fields,
    action: Schema.Literal("react"),
    targetId: Schema.NullOr(Schema.String),
    content: ReactionContent,
    on: Schema.Boolean,
  }),
  Schema.Struct({
    ...PullRequestsNumberInput.fields,
    action: Schema.Literal("replyThread"),
    threadId: TrimmedNonEmptyStringSchema,
    body: Schema.String,
  }),
  Schema.Struct({
    ...PullRequestsNumberInput.fields,
    action: Schema.Literal("resolveThread"),
    threadId: TrimmedNonEmptyStringSchema,
    resolved: Schema.Boolean,
  }),
  Schema.Struct({
    ...PullRequestsNumberInput.fields,
    action: Schema.Literal("submitReview"),
    event: Schema.Literals(["comment", "approve", "request_changes"]),
    body: Schema.NullOr(Schema.String),
    headSha: TrimmedNonEmptyStringSchema,
    comments: Schema.Array(
      Schema.Struct({
        path: TrimmedNonEmptyStringSchema,
        line: Schema.Number,
        startLine: Schema.NullOr(Schema.Number),
        side: DiffSide,
        body: Schema.String,
      }),
    ),
  }),
  Schema.Struct({ ...PullRequestsNumberInput.fields, action: Schema.Literal("revokeApproval") }),
  Schema.Struct({
    ...PullRequestsNumberInput.fields,
    action: Schema.Literal("removeOwnChangeRequest"),
  }),
  Schema.Struct({
    ...PullRequestsNumberInput.fields,
    action: Schema.Literal("dismissReview"),
    reviewId: TrimmedNonEmptyStringSchema,
    message: Schema.String,
  }),
  Schema.Struct({
    ...PullRequestsNumberInput.fields,
    action: Schema.Literal("rerequestReview"),
    login: TrimmedNonEmptyStringSchema,
  }),
  Schema.Struct({
    ...PullRequestsNumberInput.fields,
    action: Schema.Literal("applySuggestions"),
    suggestionIds: Schema.Array(TrimmedNonEmptyStringSchema),
    commitMessage: Schema.NullOr(Schema.String),
  }),
  Schema.Struct({
    ...PullRequestsNumberInput.fields,
    action: Schema.Literal("editPullRequest"),
    title: Schema.NullOr(TrimmedNonEmptyStringSchema),
    body: Schema.NullOr(Schema.String),
    baseBranch: Schema.NullOr(TrimmedNonEmptyStringSchema),
  }),
  Schema.Struct({
    ...PullRequestsNumberInput.fields,
    action: Schema.Literal("setReviewers"),
    add: Schema.Array(TrimmedNonEmptyStringSchema),
    remove: Schema.Array(TrimmedNonEmptyStringSchema),
  }),
  Schema.Struct({
    ...PullRequestsNumberInput.fields,
    action: Schema.Literal("setAssignees"),
    add: Schema.Array(TrimmedNonEmptyStringSchema),
    remove: Schema.Array(TrimmedNonEmptyStringSchema),
  }),
  Schema.Struct({
    ...PullRequestsNumberInput.fields,
    action: Schema.Literal("setLabels"),
    add: Schema.Array(TrimmedNonEmptyStringSchema),
    remove: Schema.Array(TrimmedNonEmptyStringSchema),
  }),
  Schema.Struct({
    ...PullRequestsNumberInput.fields,
    action: Schema.Literal("setMilestone"),
    milestoneId: Schema.NullOr(Schema.String),
  }),
  Schema.Struct({
    ...PullRequestsNumberInput.fields,
    action: Schema.Literal("lock"),
    reason: Schema.NullOr(Schema.String),
  }),
  Schema.Struct({ ...PullRequestsNumberInput.fields, action: Schema.Literal("unlock") }),
  Schema.Struct({
    ...PullRequestsNumberInput.fields,
    action: Schema.Literal("updateBranch"),
    method: Schema.Literals(["merge", "rebase"]),
    skipCi: Schema.Boolean,
  }),
  Schema.Struct({
    ...PullRequestsNumberInput.fields,
    action: Schema.Literal("merge"),
    method: PullRequestsMergeMethod,
    deleteBranch: Schema.Boolean,
    auto: Schema.Boolean,
    bypass: Schema.Boolean,
    headSha: TrimmedNonEmptyStringSchema,
    subject: Schema.NullOr(Schema.String),
    body: Schema.NullOr(Schema.String),
  }),
  Schema.Struct({ ...PullRequestsNumberInput.fields, action: Schema.Literal("disableAutoMerge") }),
  Schema.Struct({
    ...PullRequestsNumberInput.fields,
    action: Schema.Literal("setDraft"),
    draft: Schema.Boolean,
  }),
  Schema.Struct({ ...PullRequestsNumberInput.fields, action: Schema.Literal("close") }),
  Schema.Struct({ ...PullRequestsNumberInput.fields, action: Schema.Literal("reopen") }),
  Schema.Struct({ ...PullRequestsNumberInput.fields, action: Schema.Literal("delete") }),
  Schema.Struct({ ...PullRequestsNumberInput.fields, action: Schema.Literal("revert") }),
]);
export type PullRequestsActionRequest = typeof PullRequestsActionRequest.Type;

export const PullRequestsActionResult = Schema.Union([
  Schema.Struct({ kind: Schema.Literal("done") }),
  Schema.Struct({
    kind: Schema.Literal("reviewSubmitted"),
    reviewPosted: Schema.Boolean,
    landed: Schema.Number,
    failed: Schema.Array(
      Schema.Struct({
        path: TrimmedNonEmptyStringSchema,
        line: Schema.Number,
        body: Schema.String,
        message: Schema.String,
      }),
    ),
  }),
  Schema.Struct({
    kind: Schema.Literal("merged"),
    mergedSha: Schema.NullOr(Schema.String),
    autoMergeEnabled: Schema.Boolean,
  }),
  Schema.Struct({
    kind: Schema.Literal("pullRequestCreated"),
    number: Schema.Number,
    url: TrimmedNonEmptyStringSchema,
  }),
  Schema.Struct({ kind: Schema.Literal("deleted") }),
]);
export type PullRequestsActionResult = typeof PullRequestsActionResult.Type;

export const PullRequestsCheckoutInput = Schema.Struct({
  ...PullRequestsNumberInput.fields,
  target: Schema.Union([
    Schema.Struct({ kind: Schema.Literal("checkout"), cwd: TrimmedNonEmptyStringSchema }),
    Schema.Struct({
      kind: Schema.Literal("worktree"),
      branchName: Schema.NullOr(TrimmedNonEmptyStringSchema),
    }),
  ]),
});
export type PullRequestsCheckoutInput = typeof PullRequestsCheckoutInput.Type;

export const PullRequestsCheckoutResult = Schema.Union([
  Schema.Struct({
    kind: Schema.Literal("checked_out"),
    cwd: TrimmedNonEmptyStringSchema,
    branch: TrimmedNonEmptyStringSchema,
  }),
  Schema.Struct({
    kind: Schema.Literal("worktree_created"),
    cwd: TrimmedNonEmptyStringSchema,
    branch: TrimmedNonEmptyStringSchema,
    worktreeId: TrimmedNonEmptyStringSchema,
  }),
  Schema.Struct({ kind: Schema.Literal("blocked"), reason: GitManagerBlockedReason }),
]);
export type PullRequestsCheckoutResult = typeof PullRequestsCheckoutResult.Type;

export class PullRequestsOperationError extends Schema.TaggedError<PullRequestsOperationError>()(
  "PullRequestsOperationError",
  {
    operation: TrimmedNonEmptyStringSchema,
    code: TrimmedNonEmptyStringSchema,
    message: TrimmedNonEmptyStringSchema,
    hostDetail: Schema.NullOr(Schema.String),
    retryable: Schema.Boolean,
  },
) {}
