//! Registers the repository-scoped Pull Requests RPC surface.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{
    persistence::Repositories,
    pull_requests::{
        ContextRead, PullRequestsService,
        error::PullRequestsOperationError,
        model::{self, ActionRequest, ListQuery, SubscribeInput, VocabularyKind},
    },
    rpc::{RpcRegistry, RpcRequest, RpcResult, RpcStreamChunk},
};

const SUBSCRIBE_STREAM_CAPACITY: usize = 8;

pub const PULL_REQUESTS_UNARY_METHODS: &[&str] = &[
    "pullRequests.getContext",
    "pullRequests.getVocabulary",
    "pullRequests.getCreateDefaults",
    "pullRequests.list",
    "pullRequests.get",
    "pullRequests.getTimeline",
    "pullRequests.getCommits",
    "pullRequests.getChecks",
    "pullRequests.getFiles",
    "pullRequests.runAction",
    "pullRequests.checkout",
    "pullRequests.readSnapshot",
];

pub const PULL_REQUESTS_STREAM_METHODS: &[&str] = &["pullRequests.subscribe"];

#[derive(Clone, Copy, Debug, Default)]
pub struct PullRequestsRpcServices;

impl PullRequestsRpcServices {
    pub fn with_dependencies(
        state_dir: PathBuf,
        repositories: Repositories,
        provider_hosts: Arc<crate::source_control::ProviderHosts>,
    ) -> ConfiguredPullRequestsRpcServices {
        let database = repositories.database().clone();
        ConfiguredPullRequestsRpcServices {
            service: PullRequestsService::new(state_dir)
                .with_provider_hosts(provider_hosts)
                .with_database(database),
            repositories: Some(repositories),
            worktrees: None,
        }
    }
}

#[derive(Clone)]
pub struct ConfiguredPullRequestsRpcServices {
    pub service: PullRequestsService,
    /// Durable workspace ownership is consumed by the later checkout phase.
    pub repositories: Option<Repositories>,
    pub worktrees: Option<super::worktree_catalog_rpc::WorktreeCatalogRpcServices>,
}

/// For tests and stand-alone registration: the service keeps a private host
/// observation. The server builds its services with
/// `PullRequestsRpcServices::with_dependencies(..)`, requiring its shared observation.
impl Default for ConfiguredPullRequestsRpcServices {
    fn default() -> Self {
        Self {
            service: PullRequestsService::new(PathBuf::new()),
            repositories: None,
            worktrees: None,
        }
    }
}

impl From<PullRequestsRpcServices> for ConfiguredPullRequestsRpcServices {
    fn from(_: PullRequestsRpcServices) -> Self {
        Self::default()
    }
}

impl ConfiguredPullRequestsRpcServices {
    pub fn with_worktrees(
        mut self,
        worktrees: super::worktree_catalog_rpc::WorktreeCatalogRpcServices,
    ) -> Self {
        self.worktrees = Some(worktrees);
        self
    }

    pub async fn read_unary(
        &self,
        request: RpcRequest,
        cancellation: CancellationToken,
    ) -> RpcResult {
        self.handle_read_unary(request, cancellation).await
    }

    pub async fn mutation_unary(
        &self,
        request: RpcRequest,
        cancellation: CancellationToken,
    ) -> RpcResult {
        self.handle_mutation_unary(request, cancellation).await
    }

    pub fn subscribe(
        &self,
        request: RpcRequest,
        cancellation: CancellationToken,
    ) -> mpsc::Receiver<RpcStreamChunk> {
        self.handle_subscribe(request, cancellation)
    }

    async fn handle_read_unary(
        &self,
        request: RpcRequest,
        cancellation: CancellationToken,
    ) -> RpcResult {
        let operation = request.tag.as_str();
        match operation {
            "pullRequests.getContext" => {
                let input: ContextInput = decode(request.payload, operation)?;
                validate_cwd(&input.cwd, operation)?;
                let context = if input.rescan {
                    self.service
                        .context(&input.cwd, ContextRead::Rescan, &cancellation)
                        .await
                } else if input.allow_stale {
                    self.service
                        .context_allowing_stale(&input.cwd, &cancellation)
                        .await
                } else {
                    self.service
                        .context(&input.cwd, ContextRead::Open, &cancellation)
                        .await
                };
                encode(context, operation)
            }
            "pullRequests.getVocabulary" => {
                let input: VocabularyInput = decode(request.payload, operation)?;
                validate_cwd(&input.cwd, operation)?;
                encode(
                    self.service
                        .vocabulary(
                            &input.cwd,
                            input.kind,
                            input.query.as_deref(),
                            &cancellation,
                        )
                        .await,
                    operation,
                )
            }
            "pullRequests.getCreateDefaults" => {
                let input: CwdOnlyInput = decode(request.payload, operation)?;
                validate_cwd(&input.cwd, operation)?;
                encode(
                    self.service
                        .create_defaults(&input.cwd, &cancellation)
                        .await,
                    operation,
                )
            }
            "pullRequests.list" => {
                let input: ListQuery = decode(request.payload, operation)?;
                let cwd = PathBuf::from(&input.cwd);
                validate_cwd(&cwd, operation)?;
                encode(
                    self.service.list(&cwd, input, &cancellation).await,
                    operation,
                )
            }
            "pullRequests.readSnapshot" => {
                let input: SubscribeInput = decode(request.payload, operation)?;
                validate_cwd(Path::new(&input.list.cwd), operation)?;
                encode(
                    self.service.read_snapshot(input, &cancellation).await,
                    operation,
                )
            }
            "pullRequests.get"
            | "pullRequests.getTimeline"
            | "pullRequests.getCommits"
            | "pullRequests.getChecks"
            | "pullRequests.getFiles" => {
                let input: PullRequestsNumberInput = decode(request.payload, operation)?;
                validate_cwd(&input.cwd, operation)?;
                if input.number == 0 || input.number > 9_007_199_254_740_991 {
                    return Err(invalid_request(operation));
                }
                match operation {
                    "pullRequests.get" => encode(
                        self.service
                            .get(&input.cwd, input.number, &cancellation)
                            .await,
                        operation,
                    ),
                    "pullRequests.getTimeline" => encode(
                        self.service
                            .timeline(&input.cwd, input.number, &cancellation)
                            .await,
                        operation,
                    ),
                    "pullRequests.getCommits" => encode(
                        self.service
                            .commits(&input.cwd, input.number, &cancellation)
                            .await,
                        operation,
                    ),
                    "pullRequests.getChecks" => encode(
                        self.service
                            .checks(&input.cwd, input.number, &cancellation)
                            .await,
                        operation,
                    ),
                    _ => encode(
                        self.service
                            .files(&input.cwd, input.number, &cancellation)
                            .await,
                        operation,
                    ),
                }
            }
            _ => Err(json!(PullRequestsOperationError::new(
                operation,
                "unavailable"
            ))),
        }
    }

    async fn handle_mutation_unary(
        &self,
        request: RpcRequest,
        cancellation: CancellationToken,
    ) -> RpcResult {
        if request.tag == "pullRequests.runAction" {
            let action: ActionRequest = decode(request.payload, &request.tag)?;
            return encode(
                self.service.run_action(&action, &cancellation).await,
                &request.tag,
            );
        }
        if request.tag == "pullRequests.checkout" {
            let (Some(worktrees), Some(repositories)) = (&self.worktrees, &self.repositories)
            else {
                return Err(json!(PullRequestsOperationError::new(
                    &request.tag,
                    "unavailable"
                )));
            };
            let input = decode::<crate::pull_requests::checkout::CheckoutInput>(
                request.payload,
                &request.tag,
            )?;
            let runtime = worktrees.operation_runtime();
            let worktrees = worktrees.clone();
            let repositories = repositories.clone();
            let service = self.service.clone();
            // One runtime-owned task retains the catalog lock and availability lease
            // after disconnect. Cancellation of this waiter never drops that owner.
            let wait_cancellation = cancellation.clone();
            let wait_cancelled = CancellationToken::new();
            let wait_signal = wait_cancelled.clone();
            let write_started = CancellationToken::new();
            let write_signal = write_started.clone();
            let operation = runtime.run(async move {
                encode(
                    service
                        .checkout(
                            &worktrees,
                            &repositories,
                            input,
                            &cancellation,
                            &write_signal,
                            &wait_signal,
                        )
                        .await,
                    "pullRequests.checkout",
                )
            });
            tokio::pin!(operation);
            let result = tokio::select! {
                biased;
                result = &mut operation => result,
                () = async { tokio::select! { () = wait_cancellation.cancelled() => {}, () = wait_cancelled.cancelled() => {} } } => {
                    // Handoff and cancellation can race. Await either the pre-write
                    // cleanup result or the write signal before relinquishing the wait.
                    tokio::select! {
                        biased;
                        result = &mut operation => result,
                        () = write_started.cancelled() => {
                            let mut error = PullRequestsOperationError::new("pullRequests.checkout", "timeout");
                            error.message = "Checkout continues in the background; refresh to see the result".into();
                            error.retryable = false;
                            Err(json!(error))
                        }
                    }
                }
            };
            return result.map_err(|mut failure| {
                if failure["_tag"] == "PullRequestsOperationError" {
                    if failure["code"] == "timeout" && !write_started.is_cancelled()
                        && (wait_cancellation.is_cancelled() || wait_cancelled.is_cancelled()) {
                        failure["message"] = json!("Checkout stopped before any Git changes; refresh to try again");
                        failure["retryable"] = json!(false);
                    }
                    return failure;
                }
                let mut error = PullRequestsOperationError::new("pullRequests.checkout", "unavailable");
                error.message = "Checkout could not start. The worktree service is busy or shutting down; try again once it is available.".into();
                json!(error)
            });
        }
        Err(json!(PullRequestsOperationError::new(
            &request.tag,
            "unavailable"
        )))
    }

    fn handle_subscribe(
        &self,
        request: RpcRequest,
        cancellation: CancellationToken,
    ) -> mpsc::Receiver<RpcStreamChunk> {
        let (sender, receiver) = mpsc::channel(SUBSCRIBE_STREAM_CAPACITY);
        let service = self.service.clone();
        let operation = request.tag.clone();
        tokio::spawn(async move {
            let input = match decode::<SubscribeInput>(request.payload, &operation) {
                Ok(input) => input,
                Err(error) => {
                    let _ = sender.send(Err(error)).await;
                    return;
                }
            };
            if let Err(error) = validate_cwd(Path::new(&input.list.cwd), &operation) {
                let _ = sender.send(Err(error)).await;
                return;
            }
            let emit = forward_changed_to_stream(sender.clone());
            if let Err(error) = service.subscribe(input, &emit, &cancellation).await {
                let _ = sender.send(Err(json!(error))).await;
            }
        });
        receiver
    }
}

/// The poller calls `emit` synchronously from inside its tick; a plain `Fn`
/// cannot await the channel, so a dropped send under backpressure simply
/// skips that one change notification. The client's own `readSnapshot` call
/// stays authoritative for the stored payload.
///
/// Extracted so a regression test can assert this forwards onto the stream
/// directly: `pullRequests.subscribe` previously wired a no-op emit closure
/// here, which silently dropped every poller change event.
fn forward_changed_to_stream(
    sender: mpsc::Sender<RpcStreamChunk>,
) -> impl Fn(model::Changed) + Send + Sync {
    move |changed: model::Changed| {
        let _ = sender.try_send(Ok(vec![json!(changed)]));
    }
}

#[derive(Deserialize)]
struct PullRequestsNumberInput {
    cwd: PathBuf,
    number: u64,
}

#[derive(Deserialize)]
struct ContextInput {
    cwd: PathBuf,
    /// Only Rescan and auth recovery bypass the bounded context caches.
    #[serde(default)]
    rescan: bool,
    /// The panel's first read may answer from the stored context (`stale`).
    #[serde(default, rename = "allowStale")]
    allow_stale: bool,
}

#[derive(Deserialize)]
struct CwdOnlyInput {
    cwd: PathBuf,
}

#[derive(Deserialize)]
struct VocabularyInput {
    cwd: PathBuf,
    kind: VocabularyKind,
    query: Option<String>,
}

fn decode<T: for<'de> Deserialize<'de>>(payload: Value, operation: &str) -> Result<T, Value> {
    serde_json::from_value(payload).map_err(|_| invalid_request(operation))
}

fn validate_cwd(cwd: &Path, operation: &str) -> Result<(), Value> {
    if cwd.as_os_str().is_empty() || cwd.to_string_lossy().trim().is_empty() {
        Err(invalid_request(operation))
    } else {
        Ok(())
    }
}

fn invalid_request(operation: &str) -> Value {
    let mut error = PullRequestsOperationError::new(operation, "host_rejected");
    error.message = "The Pull Requests request is invalid.".into();
    json!(error)
}

fn encode<T: Serialize>(
    result: Result<T, PullRequestsOperationError>,
    operation: &str,
) -> RpcResult {
    result.map_err(|error| json!(error)).and_then(|value| {
        serde_json::to_value(value).map_err(|_| {
            json!(PullRequestsOperationError::new(
                operation,
                "invalid_response"
            ))
        })
    })
}

pub fn register_pull_requests_rpc(
    registry: &mut RpcRegistry,
    services: impl Into<ConfiguredPullRequestsRpcServices>,
) {
    let services = services.into();
    for method in PULL_REQUESTS_UNARY_METHODS {
        let services = services.clone();
        if matches!(*method, "pullRequests.runAction" | "pullRequests.checkout") {
            registry.register_unary(*method, move |request, cancellation| {
                let services = services.clone();
                async move { services.handle_mutation_unary(request, cancellation).await }
            });
        } else {
            registry.register_unary(*method, move |request, cancellation| {
                let services = services.clone();
                async move { services.handle_read_unary(request, cancellation).await }
            });
        }
    }
    for method in PULL_REQUESTS_STREAM_METHODS {
        let services = services.clone();
        registry.register_stream(*method, move |request, cancellation| {
            services.subscribe(request, cancellation)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::{ACTIVE_RPC_METHODS, MethodMode, RequestId, RpcRequest};
    use tokio_util::sync::CancellationToken;

    #[test]
    fn registers_every_pull_requests_method_needed_by_production_startup() {
        let mut registry = RpcRegistry::empty();
        for method in ACTIVE_RPC_METHODS
            .iter()
            .filter(|method| !method.name.starts_with("pullRequests."))
        {
            match method.mode {
                MethodMode::Unary => {
                    registry.register_unary(method.name, |_, _| async { Ok(json!({})) })
                }
                MethodMode::Stream => registry.register_stream(method.name, |_, _| {
                    let (_, receiver) = tokio::sync::mpsc::channel(1);
                    receiver
                }),
            }
        }
        assert!(registry.validate_complete().is_err());
        register_pull_requests_rpc(&mut registry, PullRequestsRpcServices);
        registry.validate_complete().unwrap();
        assert_eq!(PULL_REQUESTS_UNARY_METHODS.len(), 12);
    }

    #[tokio::test]
    async fn pull_requests_unimplemented_methods_return_the_contract_failure() {
        let services = ConfiguredPullRequestsRpcServices::default();
        for method in ["checkout"] {
            let request = RpcRequest {
                id: RequestId::try_from("1").unwrap(),
                tag: format!("pullRequests.{method}"),
                payload: json!({"cwd":"/repo","number":14}),
                headers: vec![],
                trace_id: None,
                span_id: None,
                sampled: None,
            };
            let result = if matches!(method, "runAction" | "checkout") {
                services
                    .handle_mutation_unary(request, CancellationToken::new())
                    .await
            } else {
                services
                    .handle_read_unary(request, CancellationToken::new())
                    .await
            };
            let error = result.unwrap_err();
            assert_eq!(
                error,
                json!({"_tag":"PullRequestsOperationError","operation":format!("pullRequests.{method}"),"code":"unavailable","message":"Not implemented in this server build.","hostDetail":null,"retryable":false})
            );
        }
    }

    #[tokio::test]
    async fn pull_requests_subscribe_emit_forwards_changed_events_onto_the_stream() {
        // Guards against `handle_subscribe` ever going back to the no-op emit
        // closure it shipped with before the poller's change events were wired
        // through: a dropped event here would mean the client never repaints.
        let (sender, mut receiver) = mpsc::channel(SUBSCRIBE_STREAM_CAPACITY);
        let emit = forward_changed_to_stream(sender);
        let changed = model::Changed {
            list: true,
            detail: false,
            timeline: true,
            commits: false,
            checks: false,
            files: false,
        };
        emit(changed.clone());
        let chunk = receiver
            .recv()
            .await
            .expect("the emitted change must reach the RPC stream");
        assert_eq!(chunk, Ok(vec![json!(changed)]));
    }

    #[tokio::test]
    async fn pull_requests_rpc_invalid_payload_is_a_sanitized_typed_error() {
        let services = ConfiguredPullRequestsRpcServices::default();
        let request = RpcRequest {
            id: RequestId::try_from("1").unwrap(),
            tag: "pullRequests.getContext".into(),
            payload: json!({"cwd":17,"token":"never expose this"}),
            headers: vec![],
            trace_id: None,
            span_id: None,
            sampled: None,
        };
        let error = services
            .handle_read_unary(request, CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(error["_tag"], "PullRequestsOperationError");
        assert!(!error.to_string().contains("never expose"));
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn pull_requests_rpc_dispatches_all_detail_reads_and_context_is_read_once() {
        use crate::{pull_requests::host::HostCommandRunner, test_support::TestSandbox};
        use std::fs;
        let s = TestSandbox::new("pr-rpc-detail");
        fs::write(
            s.path("detail.json"),
            include_str!("../../tests/fixtures/pull_requests/github_detail.json"),
        )
        .unwrap();
        fs::write(
            s.path("viewer.json"),
            include_str!("../../tests/fixtures/pull_requests/github_detail_viewer.json"),
        )
        .unwrap();
        fs::write(
            s.path("timeline.json"),
            include_str!("../../tests/fixtures/pull_requests/github_timeline.json"),
        )
        .unwrap();
        fs::write(
            s.path("repository.json"),
            include_str!("../../tests/fixtures/pull_requests/github_repository.json"),
        )
        .unwrap();
        let gh=s.executable_script("gh",r#"
printf '%s\n' "$*" >> calls
case "$1 $2" in
  '--version ') echo 'gh version 2.97.0' ;;
  'auth status') echo '{"hosts":{"github.com":[{"state":"success","active":true,"host":"github.com","login":"viewer"}]}}' ;;
  'api user') echo '{"login":"viewer","name":null}' ;;
  'api repos/openai/codex') cat repository.json ;;
  'api graphql') cat > query.json; if grep -q PullRequestsTimeline query.json; then cat timeline.json; else cat viewer.json; fi ;;
  'pr view') case "$5" in
    commits) echo '{"commits":[]}' ;;
    statusCheckRollup) echo '{"statusCheckRollup":[]}' ;;
    files,baseRefOid,headRefOid) echo '{"files":[],"baseRefOid":"base-sha","headRefOid":"head-sha"}' ;;
    *) cat detail.json ;; esac ;;
  'pr diff') : ;;
  *) exit 64 ;;
esac
"#,"");
        let git = s.executable_script("git", "echo 'https://github.com/openai/codex.git'", "");
        let service = PullRequestsService::with_runner(
            HostCommandRunner::new(s.path("state")).with_commands(gh, "missing-glab", git),
        );
        let services = ConfiguredPullRequestsRpcServices {
            service,
            repositories: None,
            worktrees: None,
        };
        for (method, key) in [
            ("get", "number"),
            ("getTimeline", "items"),
            ("getCommits", "commits"),
            ("getChecks", "groups"),
            ("getFiles", "files"),
        ] {
            let request = RpcRequest {
                id: RequestId::try_from("1").unwrap(),
                tag: format!("pullRequests.{method}"),
                payload: json!({"cwd":s.root(),"number":35882}),
                headers: vec![],
                trace_id: None,
                span_id: None,
                sampled: None,
            };
            let value = services
                .read_unary(request, CancellationToken::new())
                .await
                .unwrap();
            assert!(value.get(key).is_some(), "{method}: {value}");
            if method == "get" {
                assert_eq!(value["permissions"]["approve"]["allowed"], true);
                assert_eq!(value["readiness"]["status"], "review_required");
                assert_eq!(value["reviewers"][0]["canRerequest"], false);
            }
        }
        let calls = fs::read_to_string(s.path("calls")).unwrap();
        assert_eq!(calls.lines().filter(|l| *l == "api user").count(), 1);
    }

    #[tokio::test]
    async fn pull_requests_rpc_detail_number_validation_is_typed_and_sanitized() {
        let services = ConfiguredPullRequestsRpcServices::default();
        for method in ["get", "getTimeline", "getCommits", "getChecks", "getFiles"] {
            for number in [
                json!(0),
                json!(-1),
                json!(1.5),
                json!("secret-number"),
                Value::Null,
            ] {
                let request = RpcRequest {
                    id: RequestId::try_from("1").unwrap(),
                    tag: format!("pullRequests.{method}"),
                    payload: json!({"cwd":"/repo","number":number}),
                    headers: vec![],
                    trace_id: None,
                    span_id: None,
                    sampled: None,
                };
                let error = services
                    .read_unary(request, CancellationToken::new())
                    .await
                    .unwrap_err();
                assert_eq!(error["code"], "host_rejected");
                assert_eq!(error["message"], "The Pull Requests request is invalid.");
                assert!(!error.to_string().contains("secret-number"));
            }
        }
    }
}
