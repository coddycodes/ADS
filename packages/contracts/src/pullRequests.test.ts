import * as Schema from "effect/Schema";
import { describe, expect, it } from "vite-plus/test";
import * as PullRequests from "./pullRequests.ts";
import { WS_METHODS, WsRpcGroup } from "./rpc.ts";
import {
  PullRequestsActionRequest,
  PullRequestsContext,
  PullRequestsListPage,
  PullRequestsOperationError,
  PullRequestsPermissions,
} from "./pullRequests.ts";

const decodePullRequestsContext = Schema.decodeUnknownSync(PullRequestsContext);
const decodePullRequestsPermissions = Schema.decodeUnknownSync(PullRequestsPermissions);
const decodePullRequestsListPage = Schema.decodeUnknownSync(PullRequestsListPage);
const decodePullRequestsActionRequest = Schema.decodeUnknownSync(PullRequestsActionRequest);
const decodePullRequestsActor = Schema.decodeUnknownSync(PullRequests.PullRequestsActor);
const decodePullRequestsListRow = Schema.decodeUnknownSync(PullRequests.PullRequestsListRow);
const decodePullRequestsCwdInput = Schema.decodeUnknownSync(PullRequests.PullRequestsCwdInput);
const decodePullRequestsGetContextInput = Schema.decodeUnknownSync(
  PullRequests.PullRequestsGetContextInput,
);
const decodePullRequestsListInput = Schema.decodeUnknownSync(PullRequests.PullRequestsListInput);
const decodePullRequestsDetail = Schema.decodeUnknownSync(PullRequests.PullRequestsDetail);
const encodePullRequestsActionRequest = Schema.encodeSync(PullRequestsActionRequest);
const encodePullRequestsActionResult = Schema.encodeSync(PullRequests.PullRequestsActionResult);
const decodePullRequestsActionResult = Schema.decodeUnknownSync(
  PullRequests.PullRequestsActionResult,
);
const encodePullRequestsCheckoutInput = Schema.encodeSync(PullRequests.PullRequestsCheckoutInput);
const decodePullRequestsCheckoutInput = Schema.decodeUnknownSync(
  PullRequests.PullRequestsCheckoutInput,
);
const encodePullRequestsCheckoutResult = Schema.encodeSync(PullRequests.PullRequestsCheckoutResult);
const decodePullRequestsCheckoutResult = Schema.decodeUnknownSync(
  PullRequests.PullRequestsCheckoutResult,
);

const permission = (allowed: boolean, reason: string | null = null) => ({ allowed, reason });

describe("PullRequests contracts", () => {
  it("registers the eleven pullRequests methods in the RPC group", () => {
    const tags = [...WsRpcGroup.requests.keys()];
    for (const key of Object.keys(WS_METHODS).filter((key) => key.startsWith("pullRequests"))) {
      expect(tags).toContain(WS_METHODS[key as keyof typeof WS_METHODS]);
    }
    expect(tags.filter((tag) => tag.startsWith("pullRequests.")).sort()).toEqual([
      "pullRequests.checkout",
      "pullRequests.get",
      "pullRequests.getChecks",
      "pullRequests.getCommits",
      "pullRequests.getContext",
      "pullRequests.getCreateDefaults",
      "pullRequests.getFiles",
      "pullRequests.getTimeline",
      "pullRequests.getVocabulary",
      "pullRequests.list",
      "pullRequests.readSnapshot",
      "pullRequests.runAction",
      "pullRequests.subscribe",
    ]);
  });
  it("decodes an unavailable context with an actionable auth command", () => {
    const context = decodePullRequestsContext({
      status: "unavailable",
      code: "not_authenticated",
      message: "glab is not authenticated for git.acme.example.",
      provider: "gitlab",
      host: "git.acme.example",
      installHint: null,
      authCommand: "glab auth login --hostname git.acme.example",
    });
    expect(context.status).toBe("unavailable");
  });

  it("keeps every permission as {allowed, reason} and merge with its methods", () => {
    const base = Object.fromEntries(
      [
        "comment",
        "editOwnComment",
        "deleteOwnComment",
        "minimizeComment",
        "react",
        "resolveThreads",
        "review",
        "approve",
        "requestChanges",
        "revokeApproval",
        "removeOwnChangeRequest",
        "dismissReview",
        "rerequestReview",
        "applySuggestion",
        "editPullRequest",
        "editReviewers",
        "editAssignees",
        "editLabels",
        "editMilestone",
        "lock",
        "unlock",
        "mergeBypass",
        "enableAutoMerge",
        "disableAutoMerge",
        "markReady",
        "convertToDraft",
        "close",
        "reopen",
        "delete",
        "revert",
        "checkout",
      ].map((key) => [key, permission(false, "not now")]),
    );
    const decoded = decodePullRequestsPermissions({
      ...base,
      updateBranch: { allowed: true, reason: null, methods: ["merge", "rebase"] },
      merge: {
        allowed: false,
        reason: "Merging is blocked: 1 approving review required",
        methods: ["squash"],
        defaultMethod: "squash",
        deleteBranchDefault: true,
      },
    });
    expect(decoded.merge.methods).toEqual(["squash"]);
    expect(decoded.approve.reason).toBe("not now");
  });

  it("decodes a GitHub cursor page and a GitLab numbered page the same way", () => {
    const page = decodePullRequestsListPage({
      rows: [],
      nextCursor: "2",
      totalCount: 67,
      counts: { open: 67, closed: 783, merged: null },
    });
    expect(page.nextCursor).toBe("2");
  });

  it("rejects an action with an unknown tag and accepts a review submission", () => {
    expect(() =>
      decodePullRequestsActionRequest({
        cwd: "/repo",
        number: 1,
        action: "explode",
      }),
    ).toThrow();
    const review = decodePullRequestsActionRequest({
      cwd: "/repo",
      number: 14,
      action: "submitReview",
      event: "request_changes",
      body: "Please fix",
      headSha: "56f329c8d73023c9b3712e4e8699e6e1a030f0f1",
      comments: [{ path: "src/a.ts", line: 10, startLine: null, side: "right", body: "nit" }],
    });
    expect(review.action).toBe("submitReview");
  });

  it("carries host detail and retryability on the error", () => {
    const error = new PullRequestsOperationError({
      operation: "merge",
      code: "stale_head",
      message: "The pull request changed since you loaded it.",
      hostDetail: null,
      retryable: false,
    });
    expect(error._tag).toBe("PullRequestsOperationError");
  });
});

const actor = { login: "alice", name: null, isBot: false };
const label = { name: "bug", color: "ff0000", description: null };
const milestone = { id: "7", title: "Release", dueOn: null };
const reviewer = { actor, state: "unreviewed", canRerequest: false };
const permissions = {
  ...Object.fromEntries(
    [
      "comment",
      "editOwnComment",
      "deleteOwnComment",
      "minimizeComment",
      "react",
      "resolveThreads",
      "review",
      "approve",
      "requestChanges",
      "revokeApproval",
      "removeOwnChangeRequest",
      "dismissReview",
      "rerequestReview",
      "applySuggestion",
      "editPullRequest",
      "editReviewers",
      "editAssignees",
      "editLabels",
      "editMilestone",
      "lock",
      "unlock",
      "mergeBypass",
      "enableAutoMerge",
      "disableAutoMerge",
      "markReady",
      "convertToDraft",
      "close",
      "reopen",
      "delete",
      "revert",
      "checkout",
    ].map((key) => [key, permission(false, "Requires write access")]),
  ),
  updateBranch: { ...permission(true), methods: ["merge", "rebase"] },
  merge: {
    ...permission(true),
    methods: ["squash"],
    defaultMethod: "squash",
    deleteBranchDefault: true,
  },
};
const capabilities = {
  vocabulary: {
    pullRequest: "Merge request",
    pullRequests: "Merge requests",
    checks: "Pipelines",
    filesChanged: "Changes",
    reviewer: "Reviewer",
    approve: "Approve",
    requestChanges: "Request changes",
  },
  requestChanges: true,
  revokeApproval: true,
  removeOwnChangeRequest: true,
  dismissReview: false,
  applySuggestion: true,
  minimizeComment: false,
  deletePullRequest: true,
  lockReasons: [],
  updateBranchMethods: ["rebase"],
  mergeMethodsSource: "project_setting",
  autoMergeLabel: "Merge when pipeline succeeds",
  reviewerStates: true,
  closedTabIncludesMerged: false,
};
const context = {
  status: "available",
  provider: "gitlab",
  host: "git.acme.example",
  hostVersion: "18.0",
  repository: "team/repo",
  defaultBranch: "main",
  account: actor,
  repositoryPermission: "write",
  mergePolicy: {
    methods: ["squash"],
    defaultMethod: "squash",
    deleteBranchDefault: true,
    autoMergeAllowed: true,
    requiresPipelineSuccess: true,
    requiresResolvedDiscussions: true,
  },
  capabilities,
  webUrl: "https://git.acme.example/team/repo",
};
const listInput = {
  cwd: "/repo",
  state: "all",
  search: null,
  author: "@me",
  assignee: null,
  reviewer: null,
  reviewStatus: "not_approved",
  draft: "exclude",
  labels: ["bug"],
  milestone: null,
  targetBranch: "main",
  sort: "recently_updated",
  cursor: null,
};
const row = {
  number: 14,
  title: "Fix",
  state: "open",
  isDraft: false,
  author: actor,
  createdAt: "2026-09-20T00:00:00Z",
  updatedAt: "2026-09-20T00:00:00Z",
  mergedAt: null,
  closedAt: null,
  headBranch: "fix",
  baseBranch: "main",
  labels: [label],
  reviewDecision: "review_required",
  checksSummary: "pending",
  commentCount: 1,
  approvals: { approved: 0, required: 1 },
  unresolvedThreads: 1,
  url: "https://git.acme.example/team/repo/-/merge_requests/14",
};
const reactions = [{ content: "+1", count: 1, viewerReacted: true }];
const readiness = {
  status: "review_required",
  summary: "One approval required",
  details: ["Waiting for review"],
  requiredApprovals: { approved: 0, required: 1 },
  autoMerge: { enabled: true, method: "squash" },
  headSha: "abc123",
};
const {
  reviewDecision: _reviewDecision,
  checksSummary: _checksSummary,
  commentCount: _commentCount,
  approvals: _approvals,
  unresolvedThreads: _unresolvedThreads,
  ...detailBase
} = row;
const detail = {
  ...detailBase,
  body: "",
  locked: false,
  lockReason: null,
  mergedBy: null,
  headSha: "abc123",
  baseSha: "def456",
  isCrossRepository: false,
  headRepository: null,
  maintainerCanModify: true,
  commitCount: 1,
  changedFiles: 1,
  additions: 1,
  deletions: 0,
  milestone,
  assignees: [actor],
  reviewers: [reviewer],
  approvalRules: [{ name: "Owners", approved: 0, required: 1, approvers: [actor] }],
  linkedIssues: [
    { reference: "#1", title: null, url: "https://git.acme.example/team/repo/-/issues/1" },
  ],
  reactions,
  readiness,
  permissions,
  tabCounts: { conversation: null, commits: 1, checks: null, files: 1 },
};
const comment = {
  id: "comment-1",
  author: actor,
  body: "",
  createdAt: row.createdAt,
  updatedAt: row.updatedAt,
  viewerIsAuthor: true,
  minimized: false,
  reactions,
};
const timelineItems = [
  { kind: "comment", ...comment, minimized: false },
  {
    kind: "review",
    id: "review-1",
    author: actor,
    state: "approved",
    body: "",
    submittedAt: row.createdAt,
    commitSha: null,
    viewerIsAuthor: true,
    canDismiss: false,
  },
  {
    kind: "thread",
    id: "thread-1",
    path: "src/a.ts",
    line: 10,
    startLine: null,
    side: "right",
    isResolved: false,
    isOutdated: false,
    canResolve: true,
    diffHunk: "",
    comments: [
      {
        ...comment,
        suggestion: {
          id: "suggestion-1",
          applicable: true,
          fromLine: 10,
          toLine: 10,
          fromContent: "",
          toContent: "fixed",
        },
      },
    ],
  },
  {
    kind: "event",
    id: "event-1",
    actor: null,
    event: "renamed",
    detail: null,
    createdAt: row.createdAt,
  },
];
const file = {
  path: "src/a.ts",
  previousPath: null,
  changeType: "modified",
  additions: 1,
  deletions: 0,
  patch: null,
  tooLarge: true,
};

describe("PullRequests schema round trips", () => {
  const cases = [
    ["cwd", PullRequests.PullRequestsCwdInput, { cwd: "/repo" }],
    ["number", PullRequests.PullRequestsNumberInput, { cwd: "/repo", number: 14 }],
    ["provider", PullRequests.PullRequestsProviderKind, "gitlab"],
    ["actor", PullRequests.PullRequestsActor, actor],
    ["label", PullRequests.PullRequestsLabel, label],
    ["milestone", PullRequests.PullRequestsMilestone, milestone],
    ["reviewer", PullRequests.PullRequestsReviewer, reviewer],
    ["state", PullRequests.PullRequestsState, "merged"],
    ["merge method", PullRequests.PullRequestsMergeMethod, "rebase"],
    ["permission", PullRequests.PullRequestsPermission, permission(false, "Requires write access")],
    ["permissions", PullRequestsPermissions, permissions],
    ["host capabilities", PullRequests.PullRequestsHostCapabilities, capabilities],
    ["available context", PullRequestsContext, context],
    ["stored context", PullRequestsContext, { ...context, stale: true }],
    [
      "vocabulary input",
      PullRequests.PullRequestsVocabularyInput,
      { cwd: "/repo", kind: "users", query: null },
    ],
    [
      "vocabulary",
      PullRequests.PullRequestsVocabulary,
      {
        kind: "labels",
        entries: [{ id: "bug", label: "Bug", color: null, description: null }],
        truncated: true,
      },
    ],
    ["list input", PullRequests.PullRequestsListInput, listInput],
    ["list row", PullRequests.PullRequestsListRow, row],
    [
      "cursor page",
      PullRequestsListPage,
      { rows: [row], nextCursor: "endCursor", totalCount: null, counts: null },
    ],
    ["reactions", PullRequests.PullRequestsReactionSummary, reactions],
    ["readiness", PullRequests.PullRequestsMergeReadiness, readiness],
    ["detail", PullRequests.PullRequestsDetail, detail],
    ...timelineItems.map(
      (item) =>
        [`${item.kind} timeline item`, PullRequests.PullRequestsTimelineItem, item] as const,
    ),
    ["timeline", PullRequests.PullRequestsTimeline, { items: timelineItems, truncated: true }],
    [
      "commits",
      PullRequests.PullRequestsCommits,
      {
        commits: [
          {
            sha: "abc123",
            shortSha: "abc",
            subject: "Fix",
            body: null,
            author: actor,
            authoredAt: row.createdAt,
            url: row.url,
          },
        ],
      },
    ],
    [
      "checks",
      PullRequests.PullRequestsChecks,
      {
        groups: [
          {
            name: "CI",
            checks: [
              {
                name: "Build",
                state: "pending",
                url: null,
                startedAt: null,
                completedAt: null,
                durationSeconds: null,
              },
            ],
          },
        ],
        summary: "pending",
        pipelineUrl: null,
      },
    ],
    ["file", PullRequests.PullRequestsFile, file],
    [
      "files",
      PullRequests.PullRequestsFiles,
      {
        files: [file],
        diffRefs: { baseSha: "base", startSha: "start", headSha: "head" },
        truncated: true,
      },
    ],
    [
      "error",
      PullRequestsOperationError,
      {
        _tag: "PullRequestsOperationError",
        operation: "merge",
        code: "host_rejected",
        message: "Merge rejected",
        hostDetail: "Required checks failed",
        retryable: true,
      },
    ],
  ] as const;
  it.each(cases)("preserves %s on the wire", (_name, schema, value) => {
    const codec: Schema.Codec<unknown> = schema;
    expect(Schema.encodeSync(codec)(Schema.decodeUnknownSync(codec)(value))).toEqual(value);
  });

  it("rejects blank identifiers while preserving empty bodies", () => {
    expect(() => decodePullRequestsActor({ ...actor, login: " " })).toThrow();
    expect(() => decodePullRequestsListRow({ ...row, title: " " })).toThrow();
    expect(() => decodePullRequestsCwdInput({ cwd: " " })).toThrow();
    expect(decodePullRequestsDetail(detail).body).toBe("");
  });

  it("keeps the context rescan and list totals refresh optional", () => {
    expect(decodePullRequestsGetContextInput({ cwd: "/repo" })).toEqual({ cwd: "/repo" });
    expect(decodePullRequestsGetContextInput({ cwd: "/repo", rescan: true })).toEqual({
      cwd: "/repo",
      rescan: true,
    });
    expect(() => decodePullRequestsGetContextInput({ cwd: "/repo", rescan: "yes" })).toThrow();
    expect(decodePullRequestsGetContextInput({ cwd: "/repo", allowStale: true })).toEqual({
      cwd: "/repo",
      allowStale: true,
    });
    expect(() => decodePullRequestsGetContextInput({ cwd: "/repo", allowStale: 1 })).toThrow();
    const list = {
      cwd: "/repo",
      state: "open",
      search: null,
      author: null,
      assignee: null,
      reviewer: null,
      reviewStatus: null,
      draft: null,
      labels: [],
      milestone: null,
      targetBranch: null,
      sort: "newest",
      cursor: null,
    };
    expect(decodePullRequestsListInput(list)).toEqual(list);
    expect(decodePullRequestsListInput({ ...list, refreshTotals: true }).refreshTotals).toBe(true);
  });

  it("rejects missing permission reasons and invalid merge methods", () => {
    expect(() =>
      decodePullRequestsPermissions({
        ...permissions,
        approve: { allowed: false },
      }),
    ).toThrow();
    expect(() =>
      decodePullRequestsPermissions({
        ...permissions,
        merge: { ...permissions.merge, methods: ["fast_forward"] },
      }),
    ).toThrow();
  });
});

describe("PullRequests actions and outcomes", () => {
  const actions = [
    { action: "comment", body: "" },
    { action: "editComment", commentId: "1", body: "" },
    { action: "deleteComment", commentId: "1" },
    { action: "minimizeComment", commentId: "1", minimized: true },
    { action: "react", targetId: null, content: "heart", on: true },
    { action: "replyThread", threadId: "1", body: "Reply" },
    { action: "resolveThread", threadId: "1", resolved: false },
    {
      action: "submitReview",
      event: "approve",
      body: null,
      headSha: "abc123",
      comments: [{ path: "a.ts", line: 2, startLine: 1, side: "left", body: "" }],
    },
    { action: "revokeApproval" },
    { action: "removeOwnChangeRequest" },
    { action: "dismissReview", reviewId: "1", message: "Resolved" },
    { action: "rerequestReview", login: "alice" },
    { action: "applySuggestions", suggestionIds: ["1"], commitMessage: null },
    { action: "editPullRequest", title: null, body: "", baseBranch: null },
    { action: "setReviewers", add: ["alice"], remove: [] },
    { action: "setAssignees", add: [], remove: ["alice"] },
    { action: "setLabels", add: ["bug"], remove: [] },
    { action: "setMilestone", milestoneId: null },
    { action: "lock", reason: null },
    { action: "unlock" },
    { action: "updateBranch", method: "rebase", skipCi: true },
    {
      action: "merge",
      method: "squash",
      deleteBranch: true,
      auto: false,
      bypass: false,
      headSha: "abc123",
      subject: null,
      body: null,
    },
    { action: "disableAutoMerge" },
    { action: "setDraft", draft: true },
    { action: "close" },
    { action: "reopen" },
    { action: "delete" },
    { action: "revert" },
  ];
  it.each(actions)("round-trips $action", (action) => {
    const value = { cwd: "/repo", number: 14, ...action };
    expect(encodePullRequestsActionRequest(decodePullRequestsActionRequest(value))).toEqual(value);
  });
  it.each(["merge", "submitReview"])("requires the reviewed head for %s", (action) => {
    const complete = actions.find((item) => item.action === action);
    expect(() =>
      decodePullRequestsActionRequest({
        cwd: "/repo",
        number: 14,
        ...complete,
        headSha: undefined,
      }),
    ).toThrow();
  });
  it.each([
    { kind: "done" },
    {
      kind: "reviewSubmitted",
      reviewPosted: true,
      landed: 2,
      failed: [{ path: "a.ts", line: 1, body: "Keep this comment", message: "Line is outdated" }],
    },
    { kind: "merged", mergedSha: null, autoMergeEnabled: true },
    { kind: "pullRequestCreated", number: 15, url: row.url },
    { kind: "deleted" },
  ])("round-trips $kind results", (value) => {
    expect(encodePullRequestsActionResult(decodePullRequestsActionResult(value))).toEqual(value);
  });
  it.each([
    { kind: "checkout", cwd: "/repo-other" },
    { kind: "worktree", branchName: null },
  ])("round-trips $kind targets", (target) => {
    const value = { cwd: "/repo", number: 14, target };
    expect(encodePullRequestsCheckoutInput(decodePullRequestsCheckoutInput(value))).toEqual(value);
  });
  it.each([
    { kind: "checked_out", cwd: "/repo", branch: "fix" },
    { kind: "worktree_created", cwd: "/repo-new", branch: "fix", worktreeId: "worktree-1" },
    {
      kind: "blocked",
      reason: {
        operation: "checkout",
        code: "dirty-working-tree",
        message: "Commit or stash changes first",
      },
    },
  ])("round-trips $kind checkout results", (value) => {
    expect(encodePullRequestsCheckoutResult(decodePullRequestsCheckoutResult(value))).toEqual(
      value,
    );
  });
});

describe("PullRequests snapshots and change events", () => {
  it("decodes a snapshot with a null list and a detail observedAt of 1", () => {
    const decode = Schema.decodeUnknownSync(PullRequests.PullRequestsSnapshot);
    expect(
      decode({
        list: null,
        detail: { payload: detail, observedAt: 1 },
        tab: null,
      }),
    ).toEqual({
      list: null,
      detail: { payload: detail, observedAt: 1 },
      tab: null,
    });
  });

  it("rejects a changed event that omits checks", () => {
    const decode = Schema.decodeUnknownSync(PullRequests.PullRequestsChanged);
    expect(() =>
      decode({
        list: false,
        detail: false,
        timeline: false,
        commits: false,
        files: false,
      }),
    ).toThrow();
  });
});

describe("PullRequestsCreateDefaults", () => {
  it("decodes GitLab defaults and GitHub's empty merge settings", () => {
    const decode = Schema.decodeUnknownSync(PullRequests.PullRequestsCreateDefaults);
    expect(
      decode({
        viewer: { id: "7", label: "alice" },
        squash: "default_on",
        removeSourceBranch: true,
      }),
    ).toEqual({
      viewer: { id: "7", label: "alice" },
      squash: "default_on",
      removeSourceBranch: true,
    });
    expect(
      decode({ viewer: { id: "octo", label: "octo" }, squash: null, removeSourceBranch: null }),
    ).toEqual({ viewer: { id: "octo", label: "octo" }, squash: null, removeSourceBranch: null });
    expect(() => decode({ viewer: null, squash: "sometimes", removeSourceBranch: null })).toThrow();
  });

  it("is served by pullRequests.getCreateDefaults", () => {
    expect(WS_METHODS.pullRequestsGetCreateDefaults).toBe("pullRequests.getCreateDefaults");
    expect(WsRpcGroup.requests.has("pullRequests.getCreateDefaults")).toBe(true);
  });
});
