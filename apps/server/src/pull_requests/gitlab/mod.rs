mod actions;
mod files;
mod graphql;
mod parse;
pub(super) mod poll;
pub(crate) mod refresh;
mod review_positions;
mod timeline;

use std::sync::Arc;

use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{
    pull_requests::{
        error::{PullRequestsOperationError, from_process_error},
        host::*,
        model::*,
    },
    source_control::ProviderKind,
};

// RFC 3986 path/query operands: retain unreserved characters only.
const OPERAND: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'!')
    .add(b'"')
    .add(b'#')
    .add(b'$')
    .add(b'%')
    .add(b'&')
    .add(b'\'')
    .add(b'(')
    .add(b')')
    .add(b'*')
    .add(b'+')
    .add(b',')
    .add(b'/')
    .add(b':')
    .add(b';')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

pub struct GitLabHost {
    runner: Arc<HostCommandRunner>,
    contexts: super::cache::ContextCache<(String, String), HostContext>,
    /// Repository-wide tab totals, reused across filter changes and pages.
    totals: super::cache::ContextCache<(String, String), ListCounts>,
}

impl GitLabHost {
    pub fn new(runner: Arc<HostCommandRunner>) -> Self {
        Self {
            runner,
            contexts: Default::default(),
            totals: Default::default(),
        }
    }

    async fn viewer(
        &self,
        scope: &HostScope,
        operation: &str,
        c: &CancellationToken,
    ) -> Result<String, PullRequestsOperationError> {
        if let Some(context) = self.contexts.get(&scope.context_key()) {
            return Ok(context.account.login);
        }
        let user = self.api(scope, "user", operation, c).await?;
        parse::string(&user, "username", operation)
    }

    async fn api(
        &self,
        scope: &HostScope,
        path: &str,
        operation: &str,
        c: &CancellationToken,
    ) -> Result<Value, PullRequestsOperationError> {
        let started = std::time::Instant::now();
        let output = self
            .runner
            .glab(
                scope,
                &["api", path],
                if matches!(
                    operation,
                    "pullRequests.getContext"
                        | "pullRequests.getVocabulary"
                        | "pullRequests.getCreateDefaults"
                        | "pullRequests.subscribe"
                ) {
                    Budget::Read
                } else {
                    Budget::Mutation
                },
                None,
                c,
            )
            .await;
        trace_gitlab_read(operation, call_label(path), started);
        let output = output.map_err(|failure| {
            from_process_error(operation, "glab", &failure.error, &failure.stderr)
        })?;
        serde_json::from_str(&output.stdout).map_err(|_| parse::invalid(operation))
    }

    async fn graphql(
        &self,
        scope: &HostScope,
        number: u64,
        document: &str,
        operation: &str,
        c: &CancellationToken,
    ) -> Result<Value, PullRequestsOperationError> {
        self.graphql_with_variables(
            scope,
            document,
            json!({"projectPath":scope.repository,"iid":number.to_string()}),
            operation,
            c,
        )
        .await
    }

    async fn graphql_with_variables(
        &self,
        scope: &HostScope,
        document: &str,
        variables: Value,
        operation: &str,
        c: &CancellationToken,
    ) -> Result<Value, PullRequestsOperationError> {
        let input = json!({"query":document,"variables":variables});
        let started = std::time::Instant::now();
        let output = self
            .runner
            .glab_api_with_body(scope, "POST", "graphql", &input, c)
            .await;
        trace_gitlab_read(operation, "graphql", started);
        let output = output.map_err(|failure| {
            from_process_error(operation, "glab", &failure.error, &failure.stderr)
        })?;
        let value: Value =
            serde_json::from_str(&output.stdout).map_err(|_| parse::invalid(operation))?;
        if value["errors"]
            .as_array()
            .is_some_and(|errors| !errors.is_empty())
        {
            return Err(parse::invalid(operation));
        }
        Ok(value)
    }

    async fn awards(
        &self,
        scope: &HostScope,
        path: &str,
        operation: &str,
        c: &CancellationToken,
    ) -> Result<(Value, bool), PullRequestsOperationError> {
        let mut awards = Vec::new();
        for page in 1..=10 {
            let value = self
                .api(
                    scope,
                    &format!("{path}/award_emoji?per_page=100&page={page}"),
                    operation,
                    c,
                )
                .await?;
            let rows = value.as_array().ok_or_else(|| parse::invalid(operation))?;
            awards.extend(rows.iter().cloned());
            if rows.len() < 100 {
                return Ok((Value::Array(awards), false));
            }
        }
        Ok((Value::Array(awards), true))
    }

    /// One lightweight `GET merge_requests/:iid`, read for the poller's change
    /// detection only. It never calls `detail`.
    pub(super) async fn probe(
        &self,
        scope: &HostScope,
        number: u64,
        c: &CancellationToken,
    ) -> Result<refresh::ProbeFingerprint, PullRequestsOperationError> {
        let operation = "pullRequests.subscribe";
        let path = format!("{}/merge_requests/{number}", project_path(scope));
        let value = self.api(scope, &path, operation, c).await?;
        parse::probe_fingerprint(&value, operation)
    }

    async fn counts(&self, scope: &HostScope, c: &CancellationToken) -> Option<ListCounts> {
        let total = |state: &'static str| async move {
            let path = format!(
                "{}/merge_requests?state={state}&per_page=1",
                project_path(scope)
            );
            let started = std::time::Instant::now();
            let output = self
                .runner
                .glab(scope, &["api", "-i", &path], Budget::Read, None, c)
                .await;
            trace_gitlab_read("pullRequests.list", "count", started);
            let output = output.ok()?;
            parse::total(&output.stdout)
        };
        // Independent host round trips: read the three totals together.
        let (open, closed, merged) =
            tokio::join!(total("opened"), total("closed"), total("merged"));
        Some(ListCounts {
            open: Some(open?),
            closed: Some(closed?),
            merged: Some(merged?),
        })
    }
}

impl PullRequestHost for GitLabHost {
    fn create_defaults<'a>(
        &'a self,
        scope: &'a HostScope,
        c: &'a CancellationToken,
    ) -> HostFuture<'a, CreateDefaults> {
        Box::pin(async move {
            let operation = "pullRequests.getCreateDefaults";
            let project = project_path(scope);
            let (user, project) = tokio::join!(
                self.api(scope, "user", operation, c),
                self.api(scope, &project, operation, c),
            );
            parse::create_defaults(&user?, &project?, operation)
        })
    }

    fn invalidate_context(&self) {
        self.contexts.clear();
        self.totals.clear();
    }
    fn invalidate_totals(&self, scope: &HostScope) {
        self.totals.remove(&scope.context_key());
    }
    fn kind(&self) -> ProviderKind {
        ProviderKind::Gitlab
    }

    fn capabilities(&self, version: Option<&HostVersion>) -> HostCapabilities {
        let version = version
            .copied()
            .unwrap_or(HostVersion { major: 0, minor: 0 });
        HostCapabilities {
            vocabulary: HostVocabulary {
                pull_request: "merge request".into(),
                pull_requests: "merge requests".into(),
                checks: "Pipelines".into(),
                files_changed: "Changes".into(),
                reviewer: "Reviewers".into(),
                approve: "Approve".into(),
                request_changes: "Request changes".into(),
            },
            request_changes: version
                >= HostVersion {
                    major: 17,
                    minor: 2,
                },
            revoke_approval: true,
            remove_own_change_request: version
                >= HostVersion {
                    major: 17,
                    minor: 8,
                },
            dismiss_review: false,
            apply_suggestion: true,
            minimize_comment: false,
            delete_pull_request: true,
            lock_reasons: vec![],
            update_branch_methods: vec![UpdateBranchMethod::Rebase],
            merge_methods_source: MergeMethodsSource::ProjectSetting,
            auto_merge_label: "Merge when pipeline succeeds".into(),
            reviewer_states: true,
            closed_tab_includes_merged: false,
        }
    }

    fn context<'a>(
        &'a self,
        scope: &'a HostScope,
        c: &'a CancellationToken,
    ) -> HostFuture<'a, HostContext> {
        Box::pin(self.contexts.get_or_load(
            scope.context_key(),
            c,
            || PullRequestsOperationError::new("pullRequests.getContext", "timeout"),
            async move {
                let operation = "pullRequests.getContext";
                let path = project_path(scope);
                // Independent host round trips: issue them together, keep the error order.
                let (user, project, version) = tokio::join!(
                    self.api(scope, "user", operation, c),
                    self.api(scope, &path, operation, c),
                    self.api(scope, "version", operation, c),
                );
                let (user, project) = (user?, project?);
                let version = match version {
                    Ok(value) => value["version"].as_str().map(str::to_owned),
                    Err(error) if error.code == "timeout" => return Err(error),
                    Err(_) => None,
                };
                parse::context(scope, &user, &project, version.as_deref())
            },
        ))
    }

    fn vocabulary<'a>(
        &'a self,
        scope: &'a HostScope,
        kind: VocabularyKind,
        query: Option<&'a str>,
        c: &'a CancellationToken,
    ) -> HostFuture<'a, Vocabulary> {
        Box::pin(async move {
            let suffix = match kind {
                VocabularyKind::Labels => "labels?per_page=100",
                VocabularyKind::Milestones => "milestones?state=active&per_page=100",
                VocabularyKind::Users => "members/all?per_page=100",
                VocabularyKind::Branches => "repository/branches?per_page=100",
            };
            let mut path = format!("{}/{suffix}", project_path(scope));
            if let Some(query) = query {
                match kind {
                    VocabularyKind::Users => path.push_str(&format!("&query={}", encoded(query))),
                    VocabularyKind::Branches => {
                        path.push_str(&format!("&search={}", encoded(query)))
                    }
                    _ => {}
                }
            }
            let value = self
                .api(scope, &path, "pullRequests.getVocabulary", c)
                .await?;
            parse::vocabulary(kind, &value, query)
        })
    }

    fn list<'a>(
        &'a self,
        scope: &'a HostScope,
        query: &'a ListQuery,
        c: &'a CancellationToken,
    ) -> HostFuture<'a, ListPage> {
        Box::pin(async move {
            let operation = "pullRequests.list";
            if query.review_status.is_some() {
                let mut error = PullRequestsOperationError::new(operation, "unavailable");
                error.message = "Review status filters are unavailable in this server build. Clear the review status filter to view merge requests.".into();
                return Err(error);
            }
            let page = query
                .cursor
                .as_deref()
                .map_or(Ok(1), str::parse::<u64>)
                .ok()
                .filter(|page| *page > 0 && *page < u64::MAX)
                .ok_or_else(|| {
                    let mut error = PullRequestsOperationError::new(operation, "host_rejected");
                    error.message =
                        "The merge request page cursor is invalid. Refresh the list.".into();
                    error
                })?;
            let order = match query.sort {
                ListSort::Newest | ListSort::Oldest => "created_at",
                ListSort::RecentlyUpdated => "updated_at",
                ListSort::MostCommented => "popularity",
            };
            let sort = if query.sort == ListSort::Oldest {
                "asc"
            } else {
                "desc"
            };
            let mut args: Vec<String> = [
                "mr",
                "list",
                "-F",
                "json",
                "--per-page",
                "30",
                "--page",
                &page.to_string(),
                "--order",
                order,
                "--sort",
                sort,
            ]
            .into_iter()
            .map(str::to_owned)
            .collect();
            match query.state {
                // glab defaults to open; 1.114.0 has no --state flag.
                ListState::Open => {}
                ListState::Closed => args.push("--closed".into()),
                ListState::Merged => args.push("--merged".into()),
                ListState::All => args.push("--all".into()),
            }
            for (flag, value) in [
                ("--author", &query.author),
                ("--assignee", &query.assignee),
                ("--reviewer", &query.reviewer),
                ("--milestone", &query.milestone),
                ("--target-branch", &query.target_branch),
                ("--search", &query.search),
            ] {
                if let Some(value) = value {
                    args.extend([flag.into(), value.clone()]);
                }
            }
            for label in &query.labels {
                args.extend(["--label".into(), label.clone()]);
            }
            if let Some(draft) = query.draft {
                args.push(
                    if draft == DraftFilter::Only {
                        "--draft"
                    } else {
                        "--not-draft"
                    }
                    .into(),
                );
            }
            // The tab totals do not depend on the page or its filters: reuse the bounded
            // copy (30 s, successful answers only) unless this is an explicit Refresh.
            if query.refresh_totals {
                self.totals.remove(&scope.context_key());
            }
            let totals = async {
                self.totals
                    .get_or_load(scope.context_key(), c, || (), async {
                        self.counts(scope, c).await.ok_or(())
                    })
                    .await
                    .ok()
            };
            let started = std::time::Instant::now();
            let (output, counts) = tokio::join!(
                self.runner.glab(scope, &args, Budget::Mutation, None, c),
                totals,
            );
            trace_gitlab_read(operation, "list", started);
            let output = output.map_err(|failure| {
                from_process_error(operation, "glab", &failure.error, &failure.stderr)
            })?;
            let value: Value =
                serde_json::from_str(&output.stdout).map_err(|_| parse::invalid(operation))?;
            let values = value.as_array().ok_or_else(|| parse::invalid(operation))?;
            let rows = values
                .iter()
                .take(30)
                .map(parse::row)
                .collect::<Result<Vec<_>, _>>()?;
            if c.is_cancelled() {
                return Err(PullRequestsOperationError::new(operation, "timeout"));
            }
            let total_count = counts
                .as_ref()
                .filter(|_| !query.has_filters())
                .and_then(|counts| match query.state {
                    ListState::Open => counts.open,
                    ListState::Closed => counts.closed,
                    ListState::Merged => counts.merged,
                    ListState::All => counts
                        .open?
                        .checked_add(counts.closed?)?
                        .checked_add(counts.merged?),
                });
            Ok(ListPage {
                rows,
                next_cursor: (values.len() >= 30).then(|| (page + 1).to_string()),
                total_count,
                counts,
            })
        })
    }

    fn detail<'a>(
        &'a self,
        scope: &'a HostScope,
        number: u64,
        context: &'a HostContext,
        c: &'a CancellationToken,
    ) -> HostFuture<'a, DetailRaw> {
        Box::pin(async move {
            let operation = "pullRequests.get";
            let path = format!("{}/merge_requests/{number}", project_path(scope));
            let approvals_path = format!("{path}/approvals");
            let reviewers_path = format!("{path}/reviewers");
            let approval_state_path = format!("{path}/approval_state");
            let issues_path = format!("{path}/closes_issues?per_page=100");
            let (request, approvals, reviewers, approval_state, awards, issues, metadata) = tokio::join!(
                self.api(scope, &path, operation, c),
                self.api(scope, &approvals_path, operation, c),
                self.api(scope, &reviewers_path, operation, c),
                self.api(scope, &approval_state_path, operation, c),
                self.awards(scope, &path, operation, c),
                self.api(scope, &issues_path, operation, c),
                self.graphql(scope, number, graphql::DETAIL_METADATA_QUERY, operation, c),
            );
            let mut request = request?;
            if !request.is_object() {
                return Err(parse::invalid(operation));
            }
            let approvals = approvals?;
            let reviewers = reviewers?;
            let approval_state = match approval_state {
                Ok(v) => v,
                Err(e) if matches!(e.code, "not_found" | "forbidden") => Value::Null,
                Err(e) => return Err(e),
            };
            if request["diff_refs"]["base_sha"]
                .as_str()
                .is_none_or(str::is_empty)
            {
                let versions = self
                    .api(scope, &format!("{path}/versions"), operation, c)
                    .await?;
                request["diff_refs"] = json!({"base_sha":versions[0]["base_commit_sha"],"start_sha":versions[0]["start_commit_sha"],"head_sha":versions[0]["head_commit_sha"]});
            }
            let (awards, awards_truncated) = awards?;
            if awards_truncated {
                return Err(PullRequestsOperationError::new(operation, "output_limit"));
            }
            let issues = issues?;
            let metadata = metadata?;
            parse::detail(
                parse::DetailResponses {
                    request: &request,
                    approvals: &approvals,
                    reviewers: &reviewers,
                    approval_state: &approval_state,
                    awards: &awards,
                    metadata: &metadata,
                    issues: &issues,
                },
                context,
                self.capabilities(context.version().as_ref()),
            )
        })
    }

    fn action_detail<'a>(
        &'a self,
        scope: &'a HostScope,
        number: u64,
        context: &'a HostContext,
        c: &'a CancellationToken,
    ) -> HostFuture<'a, ActionDetail> {
        Box::pin(async move {
            let op = "pullRequests.runAction";
            // Cached identity/version are reusable, but repository access and policy
            // must be fresh for this write, alongside the MR and permission answers.
            let path = format!("{}/merge_requests/{number}", project_path(scope));
            let approvals_path = format!("{path}/approvals");
            let reviewers_path = format!("{path}/reviewers");
            let project_read = project_path(scope);
            // Independent reads: one wave instead of five glab starts in a row.
            let (request, approvals, reviewers, metadata, project) = tokio::join!(
                self.api(scope, &path, op, c),
                self.api(scope, &approvals_path, op, c),
                self.api(scope, &reviewers_path, op, c),
                self.graphql(scope, number, graphql::DETAIL_METADATA_QUERY, op, c),
                self.api(scope, &project_read, op, c),
            );
            let (request, approvals, reviewers, metadata, project) =
                (request?, approvals?, reviewers?, metadata?, project?);
            let user = json!({"username":context.account.login,"name":context.account.name,"bot":context.account.is_bot});
            let context =
                parse::context(scope, &user, &project, context.raw_host_version.as_deref())?;
            parse::action_detail(
                parse::PermissionResponses {
                    request: &request,
                    approvals: &approvals,
                    reviewers: &reviewers,
                    metadata: &metadata,
                },
                &context,
                self.capabilities(context.version().as_ref()),
            )
        })
    }

    fn timeline<'a>(
        &'a self,
        scope: &'a HostScope,
        number: u64,
        c: &'a CancellationToken,
    ) -> HostFuture<'a, Timeline> {
        Box::pin(self.read_timeline(scope, number, c))
    }
    fn commits<'a>(
        &'a self,
        scope: &'a HostScope,
        number: u64,
        c: &'a CancellationToken,
    ) -> HostFuture<'a, Commits> {
        Box::pin(async move {
            let value = self
                .api(
                    scope,
                    &format!(
                        "{}/merge_requests/{number}/commits?per_page=100",
                        project_path(scope)
                    ),
                    "pullRequests.getCommits",
                    c,
                )
                .await?;
            parse::commits(&value)
        })
    }
    fn checks<'a>(
        &'a self,
        scope: &'a HostScope,
        number: u64,
        c: &'a CancellationToken,
    ) -> HostFuture<'a, Checks> {
        Box::pin(async move {
            let op = "pullRequests.getChecks";
            let pipelines = self
                .api(
                    scope,
                    &format!(
                        "{}/merge_requests/{number}/pipelines?per_page=1",
                        project_path(scope)
                    ),
                    op,
                    c,
                )
                .await?;
            let pipelines = pipelines.as_array().ok_or_else(|| parse::invalid(op))?;
            let Some(pipeline) = pipelines.first() else {
                return Ok(Checks {
                    groups: vec![],
                    summary: CheckSummary::None,
                    pipeline_url: None,
                });
            };
            let project = pipeline["project_id"]
                .as_u64()
                .map_or_else(|| project_path(scope), |id| format!("projects/{id}"));
            let id = pipeline["id"].as_u64().ok_or_else(|| parse::invalid(op))?;
            let jobs = self
                .api(
                    scope,
                    &format!("{project}/pipelines/{id}/jobs?per_page=100"),
                    op,
                    c,
                )
                .await?;
            parse::checks(&jobs, pipeline)
        })
    }
    fn files<'a>(
        &'a self,
        scope: &'a HostScope,
        number: u64,
        c: &'a CancellationToken,
    ) -> HostFuture<'a, Files> {
        Box::pin(self.read_files(scope, number, c))
    }

    fn run_action<'a>(
        &'a self,
        scope: &'a HostScope,
        action: &'a ActionRequest,
        context: &'a ActionContext,
        c: &'a CancellationToken,
    ) -> HostFuture<'a, ActionResult> {
        Box::pin(async move {
            if action.is_review_action() {
                self.review_action(scope, action, context, c).await
            } else {
                self.edit_action(scope, action, context, c).await
            }
        })
    }

    fn head_branch<'a>(
        &'a self,
        scope: &'a HostScope,
        number: u64,
        c: &'a CancellationToken,
    ) -> HostFuture<'a, String> {
        Box::pin(async move {
            let value = self
                .api(
                    scope,
                    &format!(
                        "projects/{}/merge_requests/{number}",
                        encoded(&scope.repository)
                    ),
                    super::checkout::OP,
                    c,
                )
                .await?;
            value["source_branch"]
                .as_str()
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| {
                    PullRequestsOperationError::new(super::checkout::OP, "invalid_response")
                })
        })
    }

    fn head_ref_spec(&self, number: u64) -> String {
        format!("refs/merge-requests/{number}/head")
    }
}

fn gitlab_read_timing() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("BIBCODE_PULL_REQUESTS_TIMING").is_some())
}

fn trace_gitlab_read(operation: &str, call: &str, started: std::time::Instant) {
    if gitlab_read_timing() {
        eprintln!(
            "bibcode_pull_requests operation={operation} call={call} elapsed_ms={}",
            started.elapsed().as_millis()
        );
    }
}

fn call_label(path: &str) -> &'static str {
    let path = path.split('?').next().unwrap_or(path);
    if path.contains("/notes/") {
        "note"
    } else if path.ends_with("/approvals") {
        "approvals"
    } else if path.ends_with("/reviewers") {
        "reviewers"
    } else if path.ends_with("/approval_state") {
        "approval_state"
    } else if path.contains("/award_emoji") {
        "award_emoji"
    } else if path.contains("/closes_issues") {
        "closes_issues"
    } else if path.ends_with("/versions") {
        "versions"
    } else if path.contains("/resource_label_events") {
        "label_events"
    } else if path.contains("/resource_milestone_events") {
        "milestone_events"
    } else if path.contains("/resource_state_events") {
        "state_events"
    } else if path.contains("/merge_requests") {
        "merge_request"
    } else if path == "user" || path.ends_with("/user") {
        "user"
    } else if path == "version" || path.ends_with("/version") {
        "version"
    } else {
        "other"
    }
}

fn encoded(value: &str) -> String {
    utf8_percent_encode(value, OPERAND).to_string()
}
fn project_path(scope: &HostScope) -> String {
    format!("projects/{}", encoded(&scope.repository))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::path::PathBuf;

    fn scope() -> HostScope {
        HostScope {
            cwd: PathBuf::new(),
            host: "git.acme.example".into(),
            repository: "team/sub/repo".into(),
            provider: PullRequestsProvider::Gitlab,
        }
    }

    #[test]
    fn pull_requests_gitlab_timing_label_hides_the_project_path() {
        assert_eq!(
            call_label("projects/team%2Frepo/merge_requests/42"),
            "merge_request"
        );
        assert_eq!(
            call_label("projects/team%2Frepo/merge_requests/42/notes/9?per_page=1"),
            "note"
        );
        assert_eq!(call_label("projects/team%2Frepo"), "other");
        assert_eq!(call_label("user"), "user");
        assert_eq!(call_label("version"), "version");
    }

    #[test]
    fn pull_requests_gitlab_recorded_list_shape_does_not_invent_approvals_or_pipelines() {
        let mut value: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/pull_requests/gitlab_row.json"
        ))
        .unwrap();
        for (raw, state) in [
            ("opened", PullRequestState::Open),
            ("locked", PullRequestState::Open),
            ("merged", PullRequestState::Merged),
            ("closed", PullRequestState::Closed),
        ] {
            value["state"] = json!(raw);
            let row = parse::row(&value).unwrap();
            assert_eq!(row.state, state);
            assert_eq!(row.number, 42);
            assert_eq!(row.author.login, "alice");
            assert_eq!(row.comment_count, 3);
            assert_eq!(row.labels[0].color, None);
            assert_eq!(row.approvals, None);
            assert_eq!(row.review_decision, None);
            assert_eq!(row.checks_summary, None);
        }
    }

    #[test]
    fn pull_requests_gitlab_capability_version_boundaries_and_head_ref() {
        let host = GitLabHost::new(Arc::new(HostCommandRunner::new(PathBuf::new())));
        for (version, request_changes, remove) in [
            (None, false, false),
            (Some("17.1.0"), false, false),
            (Some("17.2.0"), true, false),
            (Some("17.7.9"), true, false),
            (Some("17.8.0"), true, true),
            (Some("18.0.0"), true, true),
        ] {
            let version = version.and_then(HostVersion::parse);
            let c = host.capabilities(version.as_ref());
            assert_eq!(c.request_changes, request_changes);
            assert_eq!(c.remove_own_change_request, remove);
            assert!(
                c.revoke_approval
                    && c.apply_suggestion
                    && c.delete_pull_request
                    && c.reviewer_states
            );
            assert!(!c.dismiss_review && !c.minimize_comment && !c.closed_tab_includes_merged);
            assert_eq!(c.update_branch_methods, vec![UpdateBranchMethod::Rebase]);
            assert_eq!(c.vocabulary.pull_requests, "merge requests");
            assert_eq!(c.vocabulary.checks, "Pipelines");
        }
        assert_eq!(host.head_ref_spec(42), "refs/merge-requests/42/head");
    }

    #[test]
    fn pull_requests_gitlab_permissions_use_max_access_and_merge_policy_has_valid_default() {
        let mut project: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/pull_requests/gitlab_project.json"
        ))
        .unwrap();
        let user = json!({"username":"alice","name":"Alice","id":7});
        for (level, expected) in [
            (0, RepositoryPermission::None),
            (5, RepositoryPermission::Read),
            (10, RepositoryPermission::Read),
            (15, RepositoryPermission::Read),
            (20, RepositoryPermission::Triage),
            (25, RepositoryPermission::Triage),
            (30, RepositoryPermission::Write),
            (40, RepositoryPermission::Maintain),
            (50, RepositoryPermission::Admin),
        ] {
            project["permissions"] =
                json!({"project_access":{"access_level":0},"group_access":{"access_level":level}});
            assert_eq!(
                parse::context(&scope(), &user, &project, Some("17.9.1"))
                    .unwrap()
                    .repository_permission,
                expected
            );
        }
        for (method, squash, methods, default) in [
            (
                "merge",
                "default_on",
                vec![MergeMethod::Merge, MergeMethod::Squash],
                MergeMethod::Squash,
            ),
            (
                "merge",
                "never",
                vec![MergeMethod::Merge],
                MergeMethod::Merge,
            ),
            (
                "rebase_merge",
                "default_off",
                vec![MergeMethod::Merge],
                MergeMethod::Merge,
            ),
            (
                "ff",
                "default_on",
                vec![MergeMethod::Rebase],
                MergeMethod::Rebase,
            ),
        ] {
            project["merge_method"] = json!(method);
            project["squash_option"] = json!(squash);
            let context = parse::context(&scope(), &user, &project, None).unwrap();
            assert_eq!(context.merge_policy.methods, methods);
            assert_eq!(context.merge_policy.default_method, Some(default));
            assert_eq!(
                context.version_unavailable_reason(),
                Some("GitLab version could not be read")
            );
        }
    }

    #[test]
    fn pull_requests_gitlab_totals_read_headers_only_and_ignore_case() {
        assert_eq!(
            parse::total("HTTP/2 200 OK\r\nX-Total: 41\r\n\r\n[]"),
            Some(41)
        );
        assert_eq!(parse::total("HTTP/2 200 OK\n\nX-Total: 99"), None);
        assert_eq!(parse::total("HTTP/2 200 OK\nX-Total: invalid\n\n[]"), None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pull_requests_gitlab_context_vocabulary_list_filters_and_totals() {
        use crate::test_support::TestSandbox;
        use std::fs;
        use tokio_util::sync::CancellationToken;
        let s = TestSandbox::new("pr-gitlab-adapter");
        fs::write(
            s.path("project.json"),
            include_str!("../../../tests/fixtures/pull_requests/gitlab_project.json"),
        )
        .unwrap();
        let row: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/pull_requests/gitlab_row.json"
        ))
        .unwrap();
        fs::write(s.path("list.json"), json!(vec![row; 30]).to_string()).unwrap();
        let script = s.executable_script("glab", r#"printf '%s|%s\n' "$GITLAB_HOST" "$*" >> calls
case "$1 $2" in
  'api user') echo '{"username":"alice","name":"Alice","id":7}' ;;
  'api version') if [ -e unknown-version ]; then echo '{}'; else echo '{"version":"17.9.1"}'; fi ;;
  'api projects/team%2Fsub%2Frepo') cat project.json ;;
  'api projects/team%2Fsub%2Frepo/members/all?per_page=100&query=Ali%20ce') echo '[{"id":7,"username":"alice","name":"Ali ce"}]' ;;
  'api -i') if [ -e no-counts ]; then exit 1; fi; printf 'HTTP/2 200 OK\r\nx-total: 31\r\n\r\n[]' ;;
  'mr list') cat list.json ;;
  *) exit 64 ;;
esac"#, "");
        let host = GitLabHost::new(Arc::new(
            HostCommandRunner::new(s.path("state")).with_commands(&script, &script, &script),
        ));
        let scope = HostScope {
            cwd: s.root().into(),
            ..scope()
        };
        let c = CancellationToken::new();
        let context = host.context(&scope, &c).await.unwrap();
        assert_eq!(context.host_version.as_deref(), Some("17.9.1"));
        assert_eq!(
            context.repository_permission,
            RepositoryPermission::Maintain
        );
        let vocab = host
            .vocabulary(&scope, VocabularyKind::Users, Some("Ali ce"), &c)
            .await
            .unwrap();
        assert_eq!(vocab.entries[0].id, "7");
        let mut query: ListQuery = serde_json::from_value(json!({"cwd":s.root(),"state":"open","search":null,"author":null,"assignee":null,"reviewer":null,"reviewStatus":null,"draft":null,"labels":[],"milestone":null,"targetBranch":null,"sort":"most_commented","cursor":null})).unwrap();
        let page = host.list(&scope, &query, &c).await.unwrap();
        assert_eq!(page.rows.len(), 30);
        assert_eq!(page.next_cursor.as_deref(), Some("2"));
        assert_eq!(page.total_count, Some(31));
        assert_eq!(page.counts.unwrap().merged, Some(31));
        query.review_status = Some(ReviewStatus::Approved);
        let error = host.list(&scope, &query, &c).await.unwrap_err();
        assert_eq!(error.code, "unavailable");
        query.review_status = None;
        fs::write(s.path("no-counts"), "").unwrap();
        // An explicit Refresh re-reads the totals; a failed read yields none.
        query.refresh_totals = true;
        let page = host.list(&scope, &query, &c).await.unwrap();
        assert!(page.counts.is_none());
        assert!(page.total_count.is_none());
        query.refresh_totals = false;
        fs::write(s.path("unknown-version"), "").unwrap();
        host.invalidate_context(); // Explicit Rescan bypasses the bounded context cache.
        let context = host.context(&scope, &c).await.unwrap();
        assert!(context.host_version.is_none());
        assert_eq!(
            context.version_unavailable_reason(),
            Some("GitLab version could not be read")
        );
        let calls = fs::read_to_string(s.path("calls")).unwrap();
        assert!(
            calls
                .lines()
                .all(|line| line.starts_with("git.acme.example|"))
        );
        assert!(calls.contains("--page 1"));
        assert!(calls.contains("--order popularity"));
        assert!(
            calls
                .lines()
                .filter(|line| line.contains("|api "))
                .all(|line| line.contains("--hostname git.acme.example"))
        );
    }

    /// Each fake read waits until every independent sibling read is running, so a
    /// sequential adapter times out instead of answering (no wall-clock threshold).
    #[cfg(unix)]
    #[tokio::test]
    async fn pull_requests_gitlab_context_and_list_run_independent_reads_together() {
        use crate::test_support::TestSandbox;
        use std::fs;
        use tokio_util::sync::CancellationToken;
        let s = TestSandbox::new("pr-gitlab-concurrent");
        fs::write(
            s.path("project.json"),
            include_str!("../../../tests/fixtures/pull_requests/gitlab_project.json"),
        )
        .unwrap();
        let script = s.executable_script(
            "glab",
            r#"arrive() {
  : > "$1.$$"; i=0
  while [ "$(ls "$1".* 2>/dev/null | wc -l)" -lt "$2" ]; do
    i=$((i + 1)); if [ "$i" -gt 60 ]; then rm -f "$1.$$"; echo "$1 read ran alone" >&2; exit 75; fi
    sleep 0.05
  done
}
case "$1 $2" in
  'api user') arrive context 3; echo '{"username":"alice","name":"Alice","id":7}' ;;
  'api version') arrive context 3; echo '{"version":"17.9.1"}' ;;
  'api projects/team%2Fsub%2Frepo') arrive context 3; cat project.json ;;
  'mr list') arrive list 4; echo '[]' ;;
  'api -i') arrive list 4; printf 'HTTP/2 200 OK\r\nx-total: 5\r\n\r\n[]' ;;
  *) exit 64 ;;
esac"#,
            "",
        );
        let host = GitLabHost::new(Arc::new(
            HostCommandRunner::new(s.path("state")).with_commands(&script, &script, &script),
        ));
        let scope = HostScope {
            cwd: s.root().into(),
            ..scope()
        };
        let c = CancellationToken::new();
        let context = host.context(&scope, &c).await.unwrap();
        assert_eq!(context.host_version.as_deref(), Some("17.9.1"));
        let query: ListQuery = serde_json::from_value(json!({"cwd":s.root(),"state":"open","search":null,"author":null,"assignee":null,"reviewer":null,"reviewStatus":null,"draft":null,"labels":[],"milestone":null,"targetBranch":null,"sort":"newest","cursor":null})).unwrap();
        let page = host.list(&scope, &query, &c).await.unwrap();
        assert!(page.rows.is_empty());
        assert_eq!(page.total_count, Some(5));
        let counts = page.counts.unwrap();
        assert_eq!(
            (counts.open, counts.closed, counts.merged),
            (Some(5), Some(5), Some(5))
        );
    }

    /// The tab totals are reused across filters and pages; an explicit Refresh, a
    /// successful mutation, Rescan and the 30 s expiry read them again. Every page
    /// still reads its own rows.
    #[cfg(unix)]
    #[tokio::test]
    async fn pull_requests_gitlab_list_reuses_totals_until_refresh_mutation_rescan_or_expiry() {
        use crate::test_support::TestSandbox;
        use std::fs;
        use tokio_util::sync::CancellationToken;
        let s = TestSandbox::new("pr-gitlab-totals");
        let script = s.executable_script(
            "glab",
            r#"printf '%s\n' "$*" >> calls
case "$1 $2" in
  'mr list') echo '[]' ;;
  'api -i') printf 'HTTP/2 200 OK\r\nx-total: 3\r\n\r\n[]' ;;
  *) exit 64 ;;
esac"#,
            "",
        );
        let host = GitLabHost::new(Arc::new(
            HostCommandRunner::new(s.path("state")).with_commands(&script, &script, &script),
        ));
        let scope = HostScope {
            cwd: s.root().into(),
            ..scope()
        };
        let c = CancellationToken::new();
        let mut query: ListQuery = serde_json::from_value(json!({"cwd":s.root(),"state":"open","search":null,"author":null,"assignee":null,"reviewer":null,"reviewStatus":null,"draft":null,"labels":[],"milestone":null,"targetBranch":null,"sort":"newest","cursor":null})).unwrap();
        let reads = |prefix: &str| {
            fs::read_to_string(s.path("calls"))
                .unwrap()
                .lines()
                .filter(|line| line.starts_with(prefix))
                .count()
        };
        let page = host.list(&scope, &query, &c).await.unwrap();
        let counts = page.counts.unwrap();
        assert_eq!(
            (counts.open, counts.closed, counts.merged),
            (Some(3), Some(3), Some(3))
        );
        assert_eq!((reads("api -i"), reads("mr list")), (3, 1));
        query.target_branch = Some("main".into());
        let page = host.list(&scope, &query, &c).await.unwrap();
        assert_eq!(page.counts.as_ref().and_then(|counts| counts.open), Some(3));
        assert_eq!(
            (reads("api -i"), reads("mr list")),
            (3, 2),
            "a filter reuses the totals"
        );
        query.cursor = Some("2".into());
        host.list(&scope, &query, &c).await.unwrap();
        assert_eq!(
            (reads("api -i"), reads("mr list")),
            (3, 3),
            "a later page reuses the totals"
        );
        query.cursor = None;
        query.refresh_totals = true;
        host.list(&scope, &query, &c).await.unwrap();
        assert_eq!(reads("api -i"), 6, "an explicit Refresh re-reads them");
        query.refresh_totals = false;
        host.invalidate_totals(&scope);
        host.list(&scope, &query, &c).await.unwrap();
        assert_eq!(reads("api -i"), 9, "a successful mutation re-reads them");
        host.invalidate_context();
        host.list(&scope, &query, &c).await.unwrap();
        assert_eq!(reads("api -i"), 12, "Rescan re-reads them");
        tokio::time::pause();
        tokio::time::advance(std::time::Duration::from_secs(31)).await;
        tokio::time::resume();
        host.list(&scope, &query, &c).await.unwrap();
        assert_eq!(reads("api -i"), 15, "they expire after 30 s");
        assert_eq!(reads("mr list"), 7);
    }
}

#[cfg(all(test, unix))]
mod detail_tests;
