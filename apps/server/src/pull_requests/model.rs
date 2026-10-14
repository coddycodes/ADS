//! Wire types mirror `packages/contracts/src/pullRequests.ts`.

use serde::{Deserialize, Serialize};

use crate::source_control::ProviderKind;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Actor {
    pub login: String,
    pub name: Option<String>,
    pub is_bot: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Label {
    pub name: String,
    pub color: Option<String>,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Milestone {
    pub id: String,
    pub title: String,
    pub due_on: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Reviewer {
    pub actor: Actor,
    pub state: ReviewerState,
    pub can_rerequest: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewerState {
    Unreviewed,
    Commented,
    Approved,
    ChangesRequested,
    Dismissed,
    ReviewStarted,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestState {
    Open,
    Closed,
    Merged,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpdateBranchMethod {
    Merge,
    Rebase,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Permission {
    pub allowed: bool,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateBranchPermission {
    #[serde(flatten)]
    pub permission: Permission,
    pub methods: Vec<UpdateBranchMethod>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MergePermission {
    #[serde(flatten)]
    pub permission: Permission,
    pub methods: Vec<MergeMethod>,
    pub default_method: Option<MergeMethod>,
    pub delete_branch_default: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Permissions {
    pub comment: Permission,
    pub edit_own_comment: Permission,
    pub delete_own_comment: Permission,
    pub minimize_comment: Permission,
    pub react: Permission,
    pub resolve_threads: Permission,
    pub review: Permission,
    pub approve: Permission,
    pub request_changes: Permission,
    pub revoke_approval: Permission,
    pub remove_own_change_request: Permission,
    pub dismiss_review: Permission,
    pub rerequest_review: Permission,
    pub apply_suggestion: Permission,
    pub edit_pull_request: Permission,
    pub edit_reviewers: Permission,
    pub edit_assignees: Permission,
    pub edit_labels: Permission,
    pub edit_milestone: Permission,
    pub lock: Permission,
    pub unlock: Permission,
    pub update_branch: UpdateBranchPermission,
    pub merge: MergePermission,
    pub merge_bypass: Permission,
    pub enable_auto_merge: Permission,
    pub disable_auto_merge: Permission,
    pub mark_ready: Permission,
    pub convert_to_draft: Permission,
    pub close: Permission,
    pub reopen: Permission,
    pub delete: Permission,
    pub revert: Permission,
    pub checkout: Permission,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HostVocabulary {
    pub pull_request: String,
    pub pull_requests: String,
    pub checks: String,
    pub files_changed: String,
    pub reviewer: String,
    pub approve: String,
    pub request_changes: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MergeMethodsSource {
    PerPullRequest,
    ProjectSetting,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HostCapabilities {
    pub vocabulary: HostVocabulary,
    pub request_changes: bool,
    pub revoke_approval: bool,
    pub remove_own_change_request: bool,
    pub dismiss_review: bool,
    pub apply_suggestion: bool,
    pub minimize_comment: bool,
    pub delete_pull_request: bool,
    pub lock_reasons: Vec<String>,
    pub update_branch_methods: Vec<UpdateBranchMethod>,
    pub merge_methods_source: MergeMethodsSource,
    pub auto_merge_label: String,
    pub reviewer_states: bool,
    pub closed_tab_includes_merged: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PullRequestsProvider {
    Github,
    Gitlab,
}

impl PullRequestsProvider {
    /// The single admission policy for providers implemented by this module.
    pub(super) fn from_kind(provider: ProviderKind) -> Option<Self> {
        match provider {
            ProviderKind::Github => Some(Self::Github),
            ProviderKind::Gitlab => Some(Self::Gitlab),
            ProviderKind::AzureDevops | ProviderKind::Bitbucket | ProviderKind::Unknown => None,
        }
    }

    pub(super) fn kind(self) -> ProviderKind {
        match self {
            Self::Github => ProviderKind::Github,
            Self::Gitlab => ProviderKind::Gitlab,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RepositoryPermission {
    None,
    Read,
    Triage,
    Write,
    Maintain,
    Admin,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MergePolicy {
    pub methods: Vec<MergeMethod>,
    pub default_method: Option<MergeMethod>,
    pub delete_branch_default: bool,
    pub auto_merge_allowed: bool,
    pub requires_pipeline_success: bool,
    pub requires_resolved_discussions: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableCode {
    NoRemote,
    UnsupportedProvider,
    UnknownHost,
    CliMissing,
    NotAuthenticated,
    RepositoryUnreachable,
    CliTooOld,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Context {
    Available {
        provider: PullRequestsProvider,
        host: String,
        host_version: Option<String>,
        repository: String,
        default_branch: String,
        account: Actor,
        repository_permission: RepositoryPermission,
        merge_policy: MergePolicy,
        capabilities: Box<HostCapabilities>,
        web_url: String,
        /// Answered from the stored copy (`allowStale`); a live read follows.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        stale: bool,
    },
    Unavailable {
        code: UnavailableCode,
        message: String,
        provider: Option<ProviderKind>,
        host: Option<String>,
        install_hint: Option<String>,
        auth_command: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VocabularyKind {
    Labels,
    Milestones,
    Users,
    Branches,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VocabularyEntry {
    pub id: String,
    pub label: String,
    pub color: Option<String>,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Vocabulary {
    pub kind: VocabularyKind,
    pub entries: Vec<VocabularyEntry>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ViewerRef {
    pub id: String,
    pub label: String,
}

/// Prefill for the create dialog; GitHub has no per-request merge settings.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CreateDefaults {
    pub viewer: Option<ViewerRef>,
    pub squash: Option<String>,
    pub remove_source_branch: Option<bool>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ListState {
    Open,
    Closed,
    Merged,
    All,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    ReviewRequired,
    Approved,
    ChangesRequested,
    NotApproved,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    ReviewRequired,
    Approved,
    ChangesRequested,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DraftFilter {
    Only,
    Exclude,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ListSort {
    Newest,
    Oldest,
    RecentlyUpdated,
    MostCommented,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ListQuery {
    pub cwd: String,
    pub state: ListState,
    pub search: Option<String>,
    pub author: Option<String>,
    pub assignee: Option<String>,
    pub reviewer: Option<String>,
    pub review_status: Option<ReviewStatus>,
    pub draft: Option<DraftFilter>,
    pub labels: Vec<String>,
    pub milestone: Option<String>,
    pub target_branch: Option<String>,
    pub sort: ListSort,
    pub cursor: Option<String>,
    /// Explicit Refresh: re-read the repository-wide totals instead of reusing them.
    #[serde(default)]
    pub refresh_totals: bool,
}

impl ListQuery {
    /// The poller's list key: the canonical query without the explicit-Refresh
    /// bit, which never changes the merge requests a page shows.
    pub(crate) fn snapshot_key(&self) -> String {
        let mut canonical = self.clone();
        canonical.refresh_totals = false;
        serde_json::to_string(&canonical).unwrap_or_default()
    }

    pub fn has_filters(&self) -> bool {
        self.search.is_some()
            || self.author.is_some()
            || self.assignee.is_some()
            || self.reviewer.is_some()
            || self.review_status.is_some()
            || self.draft.is_some()
            || !self.labels.is_empty()
            || self.milestone.is_some()
            || self.target_branch.is_some()
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChecksSummary {
    Pending,
    Success,
    Failure,
    Neutral,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Approvals {
    pub approved: u64,
    pub required: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ListRow {
    pub number: u64,
    pub title: String,
    pub state: PullRequestState,
    pub is_draft: bool,
    pub author: Actor,
    pub created_at: String,
    pub updated_at: String,
    pub merged_at: Option<String>,
    pub closed_at: Option<String>,
    pub head_branch: String,
    pub base_branch: String,
    pub labels: Vec<Label>,
    pub review_decision: Option<ReviewDecision>,
    pub checks_summary: Option<ChecksSummary>,
    pub comment_count: u64,
    pub approvals: Option<Approvals>,
    pub unresolved_threads: Option<u64>,
    pub url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ListCounts {
    pub open: Option<u64>,
    pub closed: Option<u64>,
    pub merged: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ListPage {
    pub rows: Vec<ListRow>,
    pub next_cursor: Option<String>,
    pub total_count: Option<u64>,
    pub counts: Option<ListCounts>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessStatus {
    Mergeable,
    ChecksPending,
    ChecksFailing,
    ReviewRequired,
    ChangesRequested,
    Conflicts,
    Behind,
    Blocked,
    Draft,
    Merged,
    Closed,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AutoMerge {
    pub enabled: bool,
    pub method: Option<MergeMethod>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MergeReadiness {
    pub status: ReadinessStatus,
    pub summary: String,
    pub details: Vec<String>,
    pub required_approvals: Option<Approvals>,
    pub auto_merge: Option<AutoMerge>,
    pub head_sha: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ReactionContent {
    #[serde(rename = "+1")]
    ThumbsUp,
    #[serde(rename = "-1")]
    ThumbsDown,
    #[serde(rename = "laugh")]
    Laugh,
    #[serde(rename = "confused")]
    Confused,
    #[serde(rename = "heart")]
    Heart,
    #[serde(rename = "hooray")]
    Hooray,
    #[serde(rename = "rocket")]
    Rocket,
    #[serde(rename = "eyes")]
    Eyes,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Reaction {
    pub content: ReactionContent,
    pub count: u64,
    pub viewer_reacted: bool,
}
pub type ReactionSummary = Vec<Reaction>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRule {
    pub name: String,
    pub approved: u64,
    pub required: u64,
    pub approvers: Vec<Actor>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LinkedIssue {
    pub reference: String,
    pub title: Option<String>,
    pub url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TabCounts {
    pub conversation: Option<u64>,
    pub commits: u64,
    pub checks: Option<u64>,
    pub files: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Detail {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub state: PullRequestState,
    pub is_draft: bool,
    pub locked: bool,
    pub lock_reason: Option<String>,
    pub author: Actor,
    pub created_at: String,
    pub updated_at: String,
    pub merged_at: Option<String>,
    pub closed_at: Option<String>,
    pub merged_by: Option<Actor>,
    pub head_branch: String,
    pub base_branch: String,
    pub head_sha: String,
    pub base_sha: String,
    pub is_cross_repository: bool,
    pub head_repository: Option<String>,
    pub maintainer_can_modify: bool,
    pub commit_count: u64,
    pub changed_files: u64,
    pub additions: u64,
    pub deletions: u64,
    pub labels: Vec<Label>,
    pub milestone: Option<Milestone>,
    pub assignees: Vec<Actor>,
    pub reviewers: Vec<Reviewer>,
    pub approval_rules: Vec<ApprovalRule>,
    pub linked_issues: Vec<LinkedIssue>,
    pub reactions: ReactionSummary,
    pub readiness: MergeReadiness,
    pub permissions: Permissions,
    pub url: String,
    pub tab_counts: TabCounts,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    Commented,
    Approved,
    ChangesRequested,
    Dismissed,
    Pending,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DiffSide {
    Left,
    Right,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Suggestion {
    pub id: Option<String>,
    pub applicable: bool,
    pub from_line: u64,
    pub to_line: u64,
    pub from_content: String,
    pub to_content: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ThreadComment {
    pub id: String,
    pub author: Actor,
    pub body: String,
    pub created_at: String,
    pub updated_at: String,
    pub viewer_is_author: bool,
    pub minimized: bool,
    pub reactions: ReactionSummary,
    pub suggestion: Option<Suggestion>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum TimelineItem {
    Comment {
        id: String,
        author: Actor,
        body: String,
        created_at: String,
        updated_at: String,
        viewer_is_author: bool,
        minimized: bool,
        reactions: ReactionSummary,
    },
    Review {
        id: String,
        author: Actor,
        state: ReviewState,
        body: String,
        submitted_at: String,
        commit_sha: Option<String>,
        viewer_is_author: bool,
        can_dismiss: bool,
    },
    Thread {
        id: String,
        path: String,
        line: Option<u64>,
        start_line: Option<u64>,
        side: DiffSide,
        is_resolved: bool,
        is_outdated: bool,
        can_resolve: bool,
        diff_hunk: Option<String>,
        comments: Vec<ThreadComment>,
    },
    Event {
        id: String,
        actor: Option<Actor>,
        event: String,
        detail: Option<String>,
        created_at: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Timeline {
    pub items: Vec<TimelineItem>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Commit {
    pub sha: String,
    pub short_sha: String,
    pub subject: String,
    pub body: Option<String>,
    pub author: Actor,
    pub authored_at: String,
    pub url: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Commits {
    pub commits: Vec<Commit>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CheckState {
    Pending,
    Success,
    Failure,
    Cancelled,
    Skipped,
    Neutral,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CheckSummary {
    Pending,
    Success,
    Failure,
    Neutral,
    None,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    pub name: String,
    pub state: CheckState,
    pub url: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub duration_seconds: Option<f64>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CheckGroup {
    pub name: String,
    pub checks: Vec<Check>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Checks {
    pub groups: Vec<CheckGroup>,
    pub summary: CheckSummary,
    pub pipeline_url: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ChangeType {
    Added,
    Modified,
    Removed,
    Renamed,
    Copied,
    Binary,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct File {
    pub path: String,
    pub previous_path: Option<String>,
    pub change_type: ChangeType,
    pub additions: u64,
    pub deletions: u64,
    pub patch: Option<String>,
    pub too_large: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DiffRefs {
    pub base_sha: String,
    pub start_sha: String,
    pub head_sha: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Files {
    pub files: Vec<File>,
    pub diff_refs: DiffRefs,
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SubscribeTab {
    Conversation,
    Commits,
    Checks,
    Files,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeInput {
    #[serde(flatten)]
    pub list: ListQuery,
    pub number: Option<u64>,
    pub tab: Option<SubscribeTab>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotPayload<T> {
    pub payload: T,
    pub observed_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotTab {
    pub kind: SubscribeTab,
    pub payload: serde_json::Value,
    pub observed_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub list: Option<SnapshotPayload<ListPage>>,
    pub detail: Option<SnapshotPayload<Detail>>,
    pub tab: Option<SnapshotTab>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Changed {
    pub list: bool,
    pub detail: bool,
    pub timeline: bool,
    pub commits: bool,
    pub checks: bool,
    pub files: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ActionTarget {
    pub cwd: String,
    pub number: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewEvent {
    Comment,
    Approve,
    RequestChanges,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewComment {
    pub path: String,
    pub line: u64,
    pub start_line: Option<u64>,
    pub side: DiffSide,
    pub body: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "action",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ActionRequest {
    Comment {
        #[serde(flatten)]
        target: ActionTarget,
        body: String,
    },
    EditComment {
        #[serde(flatten)]
        target: ActionTarget,
        comment_id: String,
        body: String,
    },
    DeleteComment {
        #[serde(flatten)]
        target: ActionTarget,
        comment_id: String,
    },
    MinimizeComment {
        #[serde(flatten)]
        target: ActionTarget,
        comment_id: String,
        minimized: bool,
    },
    React {
        #[serde(flatten)]
        target: ActionTarget,
        target_id: Option<String>,
        content: ReactionContent,
        on: bool,
    },
    ReplyThread {
        #[serde(flatten)]
        target: ActionTarget,
        thread_id: String,
        body: String,
    },
    ResolveThread {
        #[serde(flatten)]
        target: ActionTarget,
        thread_id: String,
        resolved: bool,
    },
    SubmitReview {
        #[serde(flatten)]
        target: ActionTarget,
        event: ReviewEvent,
        body: Option<String>,
        head_sha: String,
        comments: Vec<ReviewComment>,
    },
    RevokeApproval {
        #[serde(flatten)]
        target: ActionTarget,
    },
    RemoveOwnChangeRequest {
        #[serde(flatten)]
        target: ActionTarget,
    },
    DismissReview {
        #[serde(flatten)]
        target: ActionTarget,
        review_id: String,
        message: String,
    },
    RerequestReview {
        #[serde(flatten)]
        target: ActionTarget,
        login: String,
    },
    ApplySuggestions {
        #[serde(flatten)]
        target: ActionTarget,
        suggestion_ids: Vec<String>,
        commit_message: Option<String>,
    },
    EditPullRequest {
        #[serde(flatten)]
        target: ActionTarget,
        title: Option<String>,
        body: Option<String>,
        base_branch: Option<String>,
    },
    SetReviewers {
        #[serde(flatten)]
        target: ActionTarget,
        add: Vec<String>,
        remove: Vec<String>,
    },
    SetAssignees {
        #[serde(flatten)]
        target: ActionTarget,
        add: Vec<String>,
        remove: Vec<String>,
    },
    SetLabels {
        #[serde(flatten)]
        target: ActionTarget,
        add: Vec<String>,
        remove: Vec<String>,
    },
    SetMilestone {
        #[serde(flatten)]
        target: ActionTarget,
        milestone_id: Option<String>,
    },
    Lock {
        #[serde(flatten)]
        target: ActionTarget,
        reason: Option<String>,
    },
    Unlock {
        #[serde(flatten)]
        target: ActionTarget,
    },
    UpdateBranch {
        #[serde(flatten)]
        target: ActionTarget,
        method: UpdateBranchMethod,
        skip_ci: bool,
    },
    Merge {
        #[serde(flatten)]
        target: ActionTarget,
        method: MergeMethod,
        delete_branch: bool,
        auto: bool,
        bypass: bool,
        head_sha: String,
        subject: Option<String>,
        body: Option<String>,
    },
    DisableAutoMerge {
        #[serde(flatten)]
        target: ActionTarget,
    },
    SetDraft {
        #[serde(flatten)]
        target: ActionTarget,
        draft: bool,
    },
    Close {
        #[serde(flatten)]
        target: ActionTarget,
    },
    Reopen {
        #[serde(flatten)]
        target: ActionTarget,
    },
    Delete {
        #[serde(flatten)]
        target: ActionTarget,
    },
    Revert {
        #[serde(flatten)]
        target: ActionTarget,
    },
}

impl ActionRequest {
    pub fn is_review_action(&self) -> bool {
        matches!(
            self,
            Self::Comment { .. }
                | Self::EditComment { .. }
                | Self::DeleteComment { .. }
                | Self::MinimizeComment { .. }
                | Self::React { .. }
                | Self::ReplyThread { .. }
                | Self::ResolveThread { .. }
                | Self::SubmitReview { .. }
                | Self::RevokeApproval { .. }
                | Self::RemoveOwnChangeRequest { .. }
                | Self::DismissReview { .. }
                | Self::RerequestReview { .. }
                | Self::ApplySuggestions { .. }
        )
    }

    pub fn target(&self) -> &ActionTarget {
        match self {
            Self::Comment { target, .. }
            | Self::EditComment { target, .. }
            | Self::DeleteComment { target, .. }
            | Self::MinimizeComment { target, .. }
            | Self::React { target, .. }
            | Self::ReplyThread { target, .. }
            | Self::ResolveThread { target, .. }
            | Self::SubmitReview { target, .. }
            | Self::RevokeApproval { target, .. }
            | Self::RemoveOwnChangeRequest { target, .. }
            | Self::DismissReview { target, .. }
            | Self::RerequestReview { target, .. }
            | Self::ApplySuggestions { target, .. }
            | Self::EditPullRequest { target, .. }
            | Self::SetReviewers { target, .. }
            | Self::SetAssignees { target, .. }
            | Self::SetLabels { target, .. }
            | Self::SetMilestone { target, .. }
            | Self::Lock { target, .. }
            | Self::Unlock { target, .. }
            | Self::UpdateBranch { target, .. }
            | Self::Merge { target, .. }
            | Self::DisableAutoMerge { target, .. }
            | Self::SetDraft { target, .. }
            | Self::Close { target, .. }
            | Self::Reopen { target, .. }
            | Self::Delete { target, .. }
            | Self::Revert { target, .. } => target,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FailedComment {
    pub path: String,
    pub line: u64,
    pub body: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ActionResult {
    Done,
    ReviewSubmitted {
        review_posted: bool,
        landed: u32,
        failed: Vec<FailedComment>,
    },
    Merged {
        merged_sha: Option<String>,
        auto_merge_enabled: bool,
    },
    PullRequestCreated {
        number: u64,
        url: String,
    },
    Deleted,
}

/// Internal host observations. These never cross the RPC boundary.
#[derive(Clone, Debug)]
pub struct DetailRaw {
    pub detail_without_permissions: Detail,
    pub inputs: PermissionInputs,
    pub merge_commit_sha: Option<String>,
}

/// Only the fresh observations needed to authorize and dispatch a host write.
pub struct ActionDetail {
    pub inputs: PermissionInputs,
    pub context: ActionContext,
}

impl From<DetailRaw> for ActionDetail {
    fn from(raw: DetailRaw) -> Self {
        let context = ActionContext::from(&raw);
        Self {
            inputs: raw.inputs,
            context,
        }
    }
}

/// Fresh server observations needed by mutations; never supplied by the client.
#[derive(Clone, Debug, Default)]
pub struct ActionContext {
    pub viewer_login: Option<String>,
    pub title: String,
    pub url: String,
    pub base_branch: String,
    pub merge_commit_sha: Option<String>,
    pub reviewers: Vec<String>,
    pub assignees: Vec<String>,
}

impl From<&DetailRaw> for ActionContext {
    fn from(raw: &DetailRaw) -> Self {
        let detail = &raw.detail_without_permissions;
        Self {
            viewer_login: Some(raw.inputs.viewer_login.clone()),
            title: detail.title.clone(),
            url: detail.url.clone(),
            base_branch: detail.base_branch.clone(),
            merge_commit_sha: raw.merge_commit_sha.clone(),
            reviewers: detail
                .reviewers
                .iter()
                .map(|r| r.actor.login.clone())
                .collect(),
            assignees: detail.assignees.iter().map(|a| a.login.clone()).collect(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct PermissionInputs {
    pub viewer_login: String,
    pub viewer_can_comment: Option<bool>,
    pub repository_permission: RepositoryPermission,
    pub state: PullRequestState,
    pub is_draft: bool,
    pub locked: bool,
    pub viewer_is_author: bool,
    pub is_cross_repository: bool,
    pub maintainer_can_modify: bool,
    pub merged: bool,
    pub merge_commit_known: bool,
    pub gh: Option<GitHubInputs>,
    pub gl: Option<GitLabInputs>,
    pub merge_policy: MergePolicy,
    pub capabilities: HostCapabilities,
    // Observations needed by the readiness text and item-level permissions.
    pub head_sha: String,
    pub base_branch: String,
    pub required_approvals: Option<Approvals>,
    pub failing_checks: Option<u64>,
    pub has_reviewed_reviewers: bool,
    pub has_dismissible_reviews: bool,
    pub can_resolve_threads: bool,
    pub auto_merge_method: Option<MergeMethod>,
    pub version_unavailable_reason: Option<String>,
}

#[derive(Clone, Debug)]
pub struct GitHubInputs {
    pub viewer_can_update: bool,
    pub viewer_can_merge_as_admin: bool,
    pub viewer_can_enable_auto_merge: bool,
    pub viewer_can_disable_auto_merge: bool,
    pub viewer_can_apply_suggestion: bool,
    pub merge_state_status: String,
    pub mergeable: String,
    pub review_decision: Option<String>,
    pub viewer_allowed_to_dismiss_reviews: Option<bool>,
    pub auto_merge_enabled: bool,
    pub behind_by: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct GitLabInputs {
    pub can_merge: bool,
    pub detailed_merge_status: String,
    pub has_conflicts: bool,
    pub blocking_discussions_resolved: bool,
    pub discussion_locked: bool,
    pub user_can_approve: bool,
    pub user_has_approved: bool,
    pub approvals_left: u32,
    pub viewer_reviewer_state: Option<String>,
    pub is_reviewer: bool,
    pub merge_when_pipeline_succeeds: bool,
    pub pipeline_status: Option<String>,
    pub host_version: (u32, u32),
    pub access_level: u32,
    pub project_allows_maintainer_delete: bool,
    pub can_push_source_branch: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pull_requests_action_models_round_trip_contract_shapes() {
        let variants = [
            json!({"action":"comment","body":""}),
            json!({"action":"editComment","commentId":"ic:1:IC_1","body":"edited"}),
            json!({"action":"deleteComment","commentId":"nt:abc:1"}),
            json!({"action":"minimizeComment","commentId":"ic:1:IC_1","minimized":true}),
            json!({"action":"react","targetId":null,"content":"+1","on":true}),
            json!({"action":"replyThread","threadId":"th:TH_1:1","body":"reply"}),
            json!({"action":"resolveThread","threadId":"ds:abc","resolved":false}),
            json!({"action":"submitReview","event":"request_changes","body":null,"headSha":"head","comments":[{"path":"a.rs","line":3,"startLine":1,"side":"right","body":"inline"}]}),
            json!({"action":"revokeApproval"}),
            json!({"action":"removeOwnChangeRequest"}),
            json!({"action":"dismissReview","reviewId":"rv:1:RV_1","message":"obsolete"}),
            json!({"action":"rerequestReview","login":"alice"}),
            json!({"action":"applySuggestions","suggestionIds":["1","2"],"commitMessage":null}),
            json!({"action":"editPullRequest","title":null,"body":null,"baseBranch":null}),
            json!({"action":"setReviewers","add":["alice"],"remove":[]}),
            json!({"action":"setAssignees","add":[],"remove":["bob"]}),
            json!({"action":"setLabels","add":[],"remove":[]}),
            json!({"action":"setMilestone","milestoneId":null}),
            json!({"action":"lock","reason":null}),
            json!({"action":"unlock"}),
            json!({"action":"updateBranch","method":"rebase","skipCi":false}),
            json!({"action":"merge","method":"squash","deleteBranch":true,"auto":false,"bypass":false,"headSha":"head","subject":null,"body":null}),
            json!({"action":"disableAutoMerge"}),
            json!({"action":"setDraft","draft":true}),
            json!({"action":"close"}),
            json!({"action":"reopen"}),
            json!({"action":"delete"}),
            json!({"action":"revert"}),
        ];
        for mut value in variants {
            value["cwd"] = json!("/repo");
            value["number"] = json!(14);
            let request: ActionRequest = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(request).unwrap(), value);
        }
        for value in [
            json!({"kind":"done"}),
            json!({"kind":"reviewSubmitted","reviewPosted":true,"landed":2,"failed":[{"path":"a.rs","line":3,"body":"draft","message":"position invalid"}]}),
            json!({"kind":"merged","mergedSha":null,"autoMergeEnabled":true}),
            json!({"kind":"pullRequestCreated","number":14,"url":"https://example.test/14"}),
            json!({"kind":"deleted"}),
        ] {
            let result: ActionResult = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(result).unwrap(), value);
        }
        assert!(
            serde_json::from_value::<ActionRequest>(
                json!({"cwd":"/repo","number":14,"action":"unknown"})
            )
            .is_err()
        );
        let failure: serde_json::Value = serde_json::from_str(include_str!("../../../../packages/contracts/fixtures/rpc-wire/typed-failures/pullRequests__runAction-00.json")).unwrap();
        assert_eq!(
            failure["exit"]["cause"][0]["error"]["_tag"],
            "PullRequestsOperationError"
        );
        assert!(
            failure["exit"]["cause"][0]["error"]
                .get("hostDetail")
                .is_some()
        );
    }

    #[test]
    fn pull_requests_unavailable_context_matches_contract_keys() {
        let context = Context::Unavailable {
            code: UnavailableCode::NotAuthenticated,
            message: "Authenticate on this environment.".into(),
            provider: Some(crate::source_control::ProviderKind::Github),
            host: Some("github.com".into()),
            install_hint: None,
            auth_command: Some("gh auth login --hostname github.com".into()),
        };
        let value = serde_json::to_value(context).unwrap();
        assert_eq!(
            value,
            json!({
                "status": "unavailable", "code": "not_authenticated",
                "message": "Authenticate on this environment.", "provider": "github",
                "host": "github.com", "installHint": null,
                "authCommand": "gh auth login --hostname github.com"
            })
        );
        assert!(serde_json::from_value::<Context>(value).is_ok());
    }
    #[test]
    fn pull_requests_detail_read_models_keep_contract_fields_and_nulls() {
        let actor = json!({"login":"alice","name":null,"isBot":false});
        let checks = json!({"groups":[{"name":"CI","checks":[{"name":"test","state":"pending","url":null,"startedAt":null,"completedAt":null,"durationSeconds":null}]}],"summary":"pending","pipelineUrl":null});
        let files = json!({"files":[{"path":"a.rs","previousPath":null,"changeType":"modified","additions":1,"deletions":0,"patch":null,"tooLarge":true}],"diffRefs":{"baseSha":"a","startSha":"b","headSha":"c"},"truncated":true});
        let commits = json!({"commits":[{"sha":"abcdef12","shortSha":"abcdef1","subject":"title","body":null,"author":actor,"authoredAt":"2026-09-20T00:00:00Z","url":"https://example.test/commit/abcdef12"}]});
        let timeline = json!({"items":[
          {"kind":"comment","id":"ic:1:IC_1","author":actor,"body":"","createdAt":"a","updatedAt":"b","viewerIsAuthor":true,"minimized":false,"reactions":[{"content":"+1","count":1,"viewerReacted":true}]},
          {"kind":"review","id":"rv:2:RV_2","author":actor,"state":"approved","body":"","submittedAt":"c","commitSha":null,"viewerIsAuthor":false,"canDismiss":true},
          {"kind":"thread","id":"th:TH_3:3","path":"a.rs","line":2,"startLine":null,"side":"right","isResolved":false,"isOutdated":false,"canResolve":false,"diffHunk":null,"comments":[{"id":"rc:3:RC_3","author":actor,"body":"suggestion","createdAt":"d","updatedAt":"e","viewerIsAuthor":false,"minimized":false,"reactions":[],"suggestion":{"id":null,"applicable":false,"fromLine":2,"toLine":2,"fromContent":"old","toContent":"new"}}]},
          {"kind":"event","id":"ev:4","actor":null,"event":"commits_added","detail":null,"createdAt":"f"}
        ],"truncated":false});
        for (actual, expected) in [
            (
                serde_json::to_value(serde_json::from_value::<Checks>(checks.clone()).unwrap())
                    .unwrap(),
                checks,
            ),
            (
                serde_json::to_value(serde_json::from_value::<Files>(files.clone()).unwrap())
                    .unwrap(),
                files,
            ),
            (
                serde_json::to_value(serde_json::from_value::<Commits>(commits.clone()).unwrap())
                    .unwrap(),
                commits,
            ),
            (
                serde_json::to_value(serde_json::from_value::<Timeline>(timeline.clone()).unwrap())
                    .unwrap(),
                timeline,
            ),
        ] {
            assert_eq!(actual, expected);
        }
    }
}
