import type {
  PullRequestsDetail,
  PullRequestsFile,
  PullRequestsPermissions,
  PullRequestsTimelineItem,
} from "@coddy/contracts";
import { vi } from "vite-plus/test";
import { context, row } from "../testFixtures";
import type { PullRequestsNavigation } from "./pullRequestsNavigation";
export { context };
export const gitlabContext = {
  ...context,
  provider: "gitlab" as const,
  capabilities: {
    ...context.capabilities,
    closedTabIncludesMerged: false,
    mergeMethodsSource: "project_setting" as const,
    autoMergeLabel: "Merge when pipeline succeeds",
    deletePullRequest: true,
    vocabulary: {
      ...context.capabilities.vocabulary,
      pullRequest: "merge request",
      pullRequests: "merge requests",
      checks: "Pipelines",
      filesChanged: "Changes",
    },
  },
};
export const denied = { allowed: false, reason: "Write access is required for this action." };
export const permissions: PullRequestsPermissions = {
  comment: denied,
  editOwnComment: denied,
  deleteOwnComment: denied,
  minimizeComment: denied,
  react: denied,
  resolveThreads: denied,
  review: denied,
  approve: denied,
  requestChanges: denied,
  revokeApproval: denied,
  removeOwnChangeRequest: denied,
  dismissReview: denied,
  rerequestReview: denied,
  applySuggestion: denied,
  editPullRequest: denied,
  editReviewers: denied,
  editAssignees: denied,
  editLabels: denied,
  editMilestone: denied,
  lock: denied,
  unlock: denied,
  updateBranch: { ...denied, methods: ["merge"] },
  merge: {
    ...denied,
    methods: ["merge", "squash", "rebase"],
    defaultMethod: "squash",
    deleteBranchDefault: true,
  },
  mergeBypass: denied,
  enableAutoMerge: denied,
  disableAutoMerge: denied,
  markReady: denied,
  convertToDraft: denied,
  close: denied,
  reopen: denied,
  delete: denied,
  revert: denied,
  checkout: denied,
};
export const detail: PullRequestsDetail = {
  ...row,
  author: { login: "mubeda", name: null, isBot: false },
  headBranch: "feature",
  body: "A **description**",
  locked: false,
  lockReason: null,
  mergedBy: null,
  headSha: "head-sha",
  baseSha: "base-sha",
  isCrossRepository: false,
  headRepository: null,
  maintainerCanModify: false,
  commitCount: 3,
  changedFiles: 2,
  additions: 4,
  deletions: 2,
  milestone: null,
  assignees: [],
  reviewers: [],
  approvalRules: [],
  linkedIssues: [],
  reactions: [],
  permissions,
  readiness: {
    status: "review_required",
    summary: "1 approving review required.",
    details: ["0 of 1 required approvals"],
    requiredApprovals: { approved: 0, required: 1 },
    autoMerge: null,
    headSha: "head-sha",
  },
  tabCounts: { conversation: 4, commits: 3, checks: 2, files: 2 },
};
export const file: PullRequestsFile = {
  path: "src/a.ts",
  previousPath: null,
  changeType: "modified",
  additions: 1,
  deletions: 1,
  patch: "--- a/src/a.ts\n+++ b/src/a.ts\n@@ -1 +1 @@\n-old\n+new\n",
  tooLarge: false,
};
export const comment: Extract<PullRequestsTimelineItem, { kind: "comment" }> = {
  kind: "comment",
  id: "ic:17",
  author: detail.author,
  body: "A comment",
  createdAt: "2026-09-20T10:00:00Z",
  updatedAt: "2026-09-20T11:00:00Z",
  viewerIsAuthor: false,
  minimized: false,
  reactions: [{ content: "+1", count: 2, viewerReacted: true }],
};
export const thread: Extract<PullRequestsTimelineItem, { kind: "thread" }> = {
  kind: "thread",
  id: "th:17",
  path: "src/a b#é.ts",
  line: 4,
  startLine: 3,
  side: "right",
  isResolved: true,
  isOutdated: true,
  canResolve: false,
  diffHunk: "@@ -3 +3 @@\n-old\n+new",
  comments: [
    {
      ...comment,
      id: "rc:17",
      body: "Thread comment",
      suggestion: {
        id: "opaque:suggestion",
        applicable: true,
        fromLine: 3,
        toLine: 4,
        fromContent: "old",
        toContent: "new",
      },
    },
  ],
};
export const review: Extract<PullRequestsTimelineItem, { kind: "review" }> = {
  kind: "review",
  id: "rv:17",
  author: detail.author,
  state: "approved",
  body: "Looks good",
  submittedAt: "2026-09-20T12:00:00Z",
  commitSha: "head-sha",
  viewerIsAuthor: false,
  canDismiss: false,
};
export const event: Extract<PullRequestsTimelineItem, { kind: "event" }> = {
  kind: "event",
  id: "ev:17",
  actor: detail.author,
  event: "labeled",
  detail: "bug",
  createdAt: "2026-09-20T09:00:00Z",
};

export function fakeNavigation(
  overrides: Partial<PullRequestsNavigation> = {},
): PullRequestsNavigation {
  return {
    selectTab: vi.fn(),
    showFile: vi.fn(),
    setFileAnchor: vi.fn(),
    openRequest: vi.fn(),
    requestGone: vi.fn(),
    discardDeleted: null,
    back: null,
    ...overrides,
  };
}
