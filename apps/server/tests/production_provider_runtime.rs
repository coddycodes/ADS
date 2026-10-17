#![cfg_attr(not(unix), allow(dead_code, unused_imports))]
#![allow(clippy::await_holding_lock)]
// Windows compile-checks shared provider fixtures whose integration tests are Unix-only.
// Fixture snapshot guards are explicitly dropped before async shutdown, which this lint
// does not model reliably.

#[path = "support/hermetic_providers.rs"]
mod hermetic_providers;

#[path = "support/executable_fixture.rs"]
mod executable_fixture;

use bibcode_server::production::provider_runtime;

use std::{
    collections::VecDeque,
    convert::Infallible,
    future::Future,
    io,
    path::{Path, PathBuf},
    pin::Pin,
    process::Command,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::Poll,
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    extract::Path as AxumPath,
    http::StatusCode,
    response::{
        IntoResponse,
        sse::{Event, Sse},
    },
    routing::{get, post},
};
use bibcode_server::{
    RequestId, RpcExit, RpcRegistry, ServerConfig, ServerMessage, ServerRuntime,
    activity::{
        ActivityCapabilities, ActivityEntry, ActivityEntryKind, ActivityEntryTone,
        ActivityHistoryRecovery, ActivityLifecycle, ActivityObservationState, ActivityProjection,
        ActivityRecordKind, ActivityRepository, ActivityScopeRef, ActivityScopeSeed,
        ActivitySection, ActivitySectionHealth, ActivitySectionObservationState,
        ActivityTargetDispatchDisposition, ActivityWorkItemSummary, AgentActivityController,
        ProviderActivityMutation, ProviderActivityNativeTarget,
    },
    diagnostics::{NativeProcessSampler, ProcessAttributionRegistry, ProcessRow, ProcessSampler},
    git::GitRepository,
    orchestration::{
        ProviderTurnDelivery, TurnDeliveryState,
        engine::{
            EngineOptions, OrchestrationCommand, OrchestrationEngine, SessionInput, TestHooks,
            ThreadMessageInput,
        },
        load_snapshot,
    },
    persistence::{Database, ProjectionThreadMessage, ProviderSessionRuntime, run_migrations},
    production::{
        orchestration_effects::{
            self, BoxEffectFuture, EffectsOptions, OrchestrationEffectCallbacks,
            OrchestrationEffects, ProviderRewind,
        },
        orchestration_rpc::register_orchestration_rpc_with_delivery,
        turn_delivery::TurnDeliveryService,
    },
    provider::{claude::ClaudeTranscriptReaderFixture, codex::resolve_codex_home_layout},
    worktree_catalog::{
        AdoptedWorktreeAvailability, WorkspaceAvailabilityRegistry, WorkspaceLossTransition,
    },
};
use futures_util::{SinkExt, StreamExt, stream};
use provider_runtime::{
    BoxRuntimeFuture, ClaudeActivityProbeResolutionGate, ClaudeActivityProbeTestContext,
    ClaudeActivitySupport, NativeProviderDriverFactory, ProviderDeliveryOutcome, ProviderDriver,
    ProviderDriverFactory, ProviderEvent, ProviderLaunchRequest, ProviderMcpConfig,
    ProviderNativeEventId, ProviderReconciliationOutcome, ProviderRuntimeError,
    ProviderRuntimeSupervisor, StartedSession, SupervisorOptions,
    build_claude_launch_arguments_for_test, build_claude_launch_arguments_with_settings_for_test,
    claude_output_shutdown_with_open_stream_for_test, deliver_durable_orchestration_turn,
    deliver_orchestration_turn, freeze_delivery_route, reconcile_abandoned_provider_sessions,
    reconcile_orchestration_turn, rewind_thread_conversation, route_orchestration_command,
};
use serde_json::{Value, json};
use tempfile::TempDir;
#[cfg(unix)]
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::time::{timeout, timeout_at};
use tokio::{net::TcpListener, sync::mpsc};
use tokio_tungstenite::{WebSocketStream, connect_async, tungstenite::Message};

#[path = "support/websocket_frames.rs"]
mod websocket_frames;
use websocket_frames::next_frame_past_heartbeat;

const NOW: &str = "2026-07-10T10:00:00.000Z";
const CHECKPOINT_RPC_INTEGRATION_DEADLINE: Duration = Duration::from_secs(30);
const NATIVE_PROVIDER_EVENT_INTEGRATION_DEADLINE: Duration = Duration::from_secs(30);

const WINDOWS_CLAUDE_FIXTURE: &str = r#"
[Console]::Out.WriteLine("ignored non-json output")
[Console]::Error.WriteLine("fixture warning")
while ($null -ne [Console]::In.ReadLine()) {}
"#;

const WINDOWS_CODEX_FIXTURE: &str = r#"
while ($null -ne ($line = [Console]::In.ReadLine())) {
  try { $request = $line | ConvertFrom-Json } catch { continue }
  $id = [string]$request.id
  $response = $null
  switch ([string]$request.method) {
    "initialize" {
      if ($env:BIBCODE_TEST_CODEX_INITIALIZE_RESPONSE_READY_MARKER) {
        [IO.File]::WriteAllText($env:BIBCODE_TEST_CODEX_INITIALIZE_RESPONSE_READY_MARKER, "ready")
      }
      $response = '{"id":' + $id + ',"result":{"userAgent":"fixture"}}'
    }
    "thread/start" {
      if ($env:BIBCODE_TEST_CODEX_THREAD_RESPONSE_READY_MARKER) {
        [IO.File]::WriteAllText($env:BIBCODE_TEST_CODEX_THREAD_RESPONSE_READY_MARKER, "ready")
      }
      $response = '{"id":' + $id + ',"result":{"cwd":"C:\\tmp","model":"gpt-5","thread":{"id":"native-codex-thread","historyMode":"paginated"}}}'
    }
    "mcpServerStatus/list" { $response = '{"id":' + $id + ',"result":{"data":[],"nextCursor":null}}' }
    "thread/goal/set" { $response = '{"id":' + $id + ',"result":{"goal":{"status":"active"}}}' }
    "turn/start" { $response = '{"id":' + $id + ',"result":{"turn":{"id":"native-codex-turn"}}}' + [Environment]::NewLine + '{"method":"item/started","emittedAtMs":1001,"params":{"threadId":"native-codex-thread","turnId":"native-codex-turn","item":{"id":"spawn-1","type":"collabAgentToolCall","tool":"spawnAgent","status":"inProgress","senderThreadId":"native-codex-thread","receiverThreadIds":["native-child"],"agentsStates":{"native-child":{"status":"running","message":null}}},"startedAtMs":1001}}' + [Environment]::NewLine + '{"method":"turn/started","emittedAtMs":1002,"params":{"threadId":"native-child","turn":{"id":"native-child-turn","status":"inProgress","startedAt":1}}}' }
    "turn/interrupt" { $response = '{"id":' + $id + ',"result":{}}' }
    "thread/turns/list" { $response = '{"id":' + $id + ',"result":{"data":[{"id":"native-codex-turn","items":[]}],"nextCursor":null}}' }
    "thread/revert" {
      if ([string]$request.params.beforeTurnId -eq "native-codex-turn") {
        $response = '{"id":' + $id + ',"result":{"thread":{"id":"native-codex-thread","historyMode":"paginated","turns":[]}}}'
      }
    }
    "shutdown" { $response = '{"id":' + $id + ',"result":null}' }
  }
  if ($null -ne $response) {
    [Console]::Out.WriteLine($response)
    [Console]::Out.Flush()
  }
  if ([string]$request.method -eq "turn/start") {
    [Console]::Out.WriteLine('{"method":"item/started","emittedAtMs":1001,"params":{"threadId":"native-codex-thread","turnId":"native-codex-turn","item":{"id":"spawn-1","type":"collabAgentToolCall","tool":"spawnAgent","status":"inProgress","senderThreadId":"native-codex-thread","receiverThreadIds":["native-child"],"agentsStates":{"native-child":{"status":"running","message":null}}},"startedAtMs":1001}}')
    [Console]::Out.Flush()
  }
}
"#;

const WINDOWS_ACP_FIXTURE: &str = r#"
while ($null -ne ($line = [Console]::In.ReadLine())) {
  try { $request = $line | ConvertFrom-Json } catch { continue }
  $id = [string]$request.id
  $response = $null
  switch ([string]$request.method) {
    "initialize" { $response = '{"jsonrpc":"2.0","id":' + $id + ',"result":{}}' }
    "authenticate" { $response = '{"jsonrpc":"2.0","id":' + $id + ',"result":{}}' }
    "session/new" { $response = '{"jsonrpc":"2.0","id":' + $id + ',"result":{"sessionId":"cursor-session","configOptions":[{"id":"model","category":"model"}],"modes":{"currentModeId":"ask","availableModes":[{"id":"ask","name":"Ask"},{"id":"code","name":"Agent"},{"id":"architect","name":"Plan"}]}}}' }
    "session/create" { $response = '{"jsonrpc":"2.0","id":' + $id + ',"result":{"sessionId":"grok-session","modes":{"currentModeId":"code","availableModes":[{"id":"code","name":"Agent"},{"id":"ask","name":"Ask"}]}}}' }
    "session/set_config_option" { $response = '{"jsonrpc":"2.0","id":' + $id + ',"result":{"configOptions":[]}}' }
    "session/set_mode" { $response = '{"jsonrpc":"2.0","id":' + $id + ',"result":{}}' }
    "session/set_model" { $response = '{"jsonrpc":"2.0","id":' + $id + ',"result":{}}' }
    "session/prompt" {
      Start-Sleep -Milliseconds 100
      $response = '{"jsonrpc":"2.0","id":' + $id + ',"result":{"stopReason":"end_turn"}}'
    }
  }
  if ($null -ne $response) {
    [Console]::Out.WriteLine($response)
    [Console]::Out.Flush()
  }
}
"#;

#[cfg(any(unix, windows))]
/// Waits until every capture file a fixture script writes exists and has
/// content. A shell redirect creates the file before its content lands, so an
/// existence check alone can read an empty token or a half-written settings
/// document on a starved runner.
async fn wait_for_captures(paths: &[&std::path::Path]) {
    timeout(Duration::from_secs(5), async {
        loop {
            if paths
                .iter()
                .all(|path| std::fs::metadata(path).is_ok_and(|meta| meta.len() > 0))
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fixture should write its capture files");
}

fn executable_fixture(
    temp: &TempDir,
    name: &str,
    unix_contents: &str,
    windows_contents: &str,
) -> PathBuf {
    #[cfg(unix)]
    {
        let _ = windows_contents;
        let executable = temp.path().join(format!("{name}.sh"));
        executable_fixture::write_executable(&executable, unix_contents);
        executable
    }
    #[cfg(windows)]
    {
        let _ = unix_contents;
        let executable = temp.path().join(format!("{name}.ps1"));
        std::fs::write(&executable, windows_contents).expect("provider fixture should write");
        executable
    }
}

#[derive(Clone, Default)]
struct TraceCapture(Arc<StdMutex<Vec<u8>>>);

struct TraceCaptureWriter(Arc<StdMutex<Vec<u8>>>);

impl io::Write for TraceCaptureWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for TraceCapture {
    type Writer = TraceCaptureWriter;

    fn make_writer(&'writer self) -> Self::Writer {
        TraceCaptureWriter(self.0.clone())
    }
}

impl TraceCapture {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

#[derive(Default)]
struct DriverState {
    launches: Vec<ProviderLaunchRequest>,
    start_results: VecDeque<Result<StartedSession, ProviderRuntimeError>>,
    starts: usize,
    sends: Vec<String>,
    sent_attachments: Vec<Vec<Value>>,
    delivery_outcomes: VecDeque<ProviderDeliveryOutcome>,
    delivery_entered: Option<Arc<tokio::sync::Notify>>,
    delivery_release: Option<Arc<tokio::sync::Semaphore>>,
    delivery_preflight_entered: Option<Arc<tokio::sync::Notify>>,
    delivery_preflight_release: Option<Arc<tokio::sync::Semaphore>>,
    delivery_panics: usize,
    delivery_started: Vec<String>,
    operation_order: Vec<&'static str>,
    delivery_routes: Vec<(String, Option<String>, String)>,
    delivery_active: usize,
    delivery_max_active: usize,
    interrupts: Vec<Option<String>>,
    approvals: Vec<(String, String)>,
    answers: Vec<(String, Value)>,
    modes: Vec<String>,
    set_mode_results: VecDeque<Result<(), ProviderRuntimeError>>,
    interaction_modes: Vec<String>,
    set_interaction_mode_results: VecDeque<Result<(), ProviderRuntimeError>>,
    models: Vec<String>,
    reapply_options_on_model_change: bool,
    set_model_results: VecDeque<Result<(), ProviderRuntimeError>>,
    option_updates: Vec<Vec<Value>>,
    set_options_results: VecDeque<Result<(), ProviderRuntimeError>>,
    rollbacks: Vec<i64>,
    rollback_observations: Vec<(i64, Option<String>)>,
    rollback_workspace: Option<PathBuf>,
    rollback_error: Option<String>,
    agent_activity_transitions: Vec<bool>,
    agent_activity_results: VecDeque<Result<(), ProviderRuntimeError>>,
    targeted_activity_cancellations: usize,
    targeted_activity_entered: Option<Arc<tokio::sync::Notify>>,
    targeted_activity_release: Option<Arc<tokio::sync::Notify>>,
    targeted_activity_active: Arc<AtomicUsize>,
    shutdowns: usize,
    shutdown_results: VecDeque<Result<(), ProviderRuntimeError>>,
    shutdown_gate: Option<Arc<tokio::sync::Semaphore>>,
    stream_ended: Option<Arc<tokio::sync::Notify>>,
}

struct FakeDriver {
    provider: String,
    provider_instance_id: Option<String>,
    state: Arc<StdMutex<DriverState>>,
    events: tokio::sync::Mutex<mpsc::Receiver<ProviderEvent>>,
}

struct TargetedActivityCallLease(Arc<AtomicUsize>);

impl Drop for TargetedActivityCallLease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn started_session(session_id: &str) -> StartedSession {
    StartedSession {
        resume_cursor: Some(json!({ "sessionId": session_id })),
        runtime_payload: Some(json!({ "transport": "native" })),
        activity_capabilities: ActivityCapabilities::none(),
    }
}

fn started_session_for_provider(provider: &str, session_id: &str) -> StartedSession {
    StartedSession {
        resume_cursor: Some(if provider == "codex" {
            json!({ "threadId": session_id })
        } else {
            json!({ "sessionId": session_id })
        }),
        runtime_payload: Some(json!({ "transport": "native" })),
        activity_capabilities: ActivityCapabilities::none(),
    }
}

impl ProviderDriver for FakeDriver {
    fn start(&self) -> BoxRuntimeFuture<'_, Result<StartedSession, ProviderRuntimeError>> {
        Box::pin(async move {
            {
                let mut state = self.state.lock().unwrap();
                state.starts += 1;
                state.start_results.pop_front().unwrap_or_else(|| {
                    Ok(started_session_for_provider(
                        &self.provider,
                        "provider-session-1",
                    ))
                })
            }
        })
    }

    fn send(
        &self,
        text: String,
        attachments: Vec<Value>,
        _interaction_mode: String,
    ) -> BoxRuntimeFuture<'_, Result<Option<String>, ProviderRuntimeError>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            state.sends.push(text);
            state.sent_attachments.push(attachments);
            Ok(Some("provider-turn-1".to_owned()))
        })
    }

    fn deliver(
        &self,
        text: String,
        attachments: Vec<Value>,
        interaction_mode: String,
        _delivery_key: String,
    ) -> BoxRuntimeFuture<'_, ProviderDeliveryOutcome> {
        Box::pin(async move {
            let (preflight_entered, preflight_release) = {
                let state = self.state.lock().unwrap();
                (
                    state.delivery_preflight_entered.clone(),
                    state.delivery_preflight_release.clone(),
                )
            };
            if let Some(entered) = preflight_entered {
                entered.notify_one();
            }
            if let Some(release) = preflight_release {
                release
                    .acquire()
                    .await
                    .expect("delivery preflight release")
                    .forget();
            }
            let should_panic = {
                let mut state = self.state.lock().unwrap();
                if state.delivery_panics > 0 {
                    state.delivery_panics -= 1;
                    true
                } else {
                    false
                }
            };
            if should_panic {
                panic!("injected delivery panic");
            }
            let (entered, release) = {
                let mut state = self.state.lock().unwrap();
                state.operation_order.push("delivery");
                state.delivery_started.push(text.clone());
                state.delivery_routes.push((
                    self.provider.clone(),
                    self.provider_instance_id.clone(),
                    text.clone(),
                ));
                state.delivery_active += 1;
                state.delivery_max_active = state.delivery_max_active.max(state.delivery_active);
                (
                    state.delivery_entered.clone(),
                    state.delivery_release.clone(),
                )
            };
            if let Some(entered) = entered {
                entered.notify_one();
            }
            if let Some(release) = release {
                release.acquire().await.expect("delivery release").forget();
            }
            let configured = self.state.lock().unwrap().delivery_outcomes.pop_front();
            let outcome = if let Some(outcome) = configured {
                outcome
            } else {
                match self.send(text, attachments, interaction_mode).await {
                    Ok(turn_id) => ProviderDeliveryOutcome::Accepted { turn_id },
                    Err(error) => ProviderDeliveryOutcome::Ambiguous {
                        detail: error.to_string(),
                    },
                }
            };
            self.state.lock().unwrap().delivery_active -= 1;
            outcome
        })
    }

    fn interrupt(
        &self,
        turn_id: Option<String>,
    ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
        Box::pin(async move {
            self.state.lock().unwrap().interrupts.push(turn_id);
            Ok(())
        })
    }

    fn approve(
        &self,
        request_id: String,
        decision: String,
    ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
        Box::pin(async move {
            self.state
                .lock()
                .unwrap()
                .approvals
                .push((request_id, decision));
            Ok(())
        })
    }

    fn answer(
        &self,
        request_id: String,
        answers: Value,
    ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
        Box::pin(async move {
            self.state
                .lock()
                .unwrap()
                .answers
                .push((request_id, answers));
            Ok(())
        })
    }

    fn set_mode(&self, mode: String) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
        Box::pin(async move {
            {
                let mut state = self.state.lock().unwrap();
                state.operation_order.push("runtime");
                state.modes.push(mode);
                state.set_mode_results.pop_front().unwrap_or(Ok(()))
            }
        })
    }

    fn set_interaction_mode(
        &self,
        mode: String,
    ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
        Box::pin(async move {
            {
                let mut state = self.state.lock().unwrap();
                state.operation_order.push("interaction");
                state.interaction_modes.push(mode);
                state
                    .set_interaction_mode_results
                    .pop_front()
                    .unwrap_or(Ok(()))
            }
        })
    }

    fn set_model(&self, model: String) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
        Box::pin(async move {
            {
                let mut state = self.state.lock().unwrap();
                state.models.push(model);
                state.set_model_results.pop_front().unwrap_or(Ok(()))
            }
        })
    }

    fn reapply_options_on_model_change(&self) -> bool {
        self.state.lock().unwrap().reapply_options_on_model_change
    }

    fn set_options(
        &self,
        options: Vec<Value>,
    ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            state.operation_order.push("options");
            state.option_updates.push(options);
            state.set_options_results.pop_front().unwrap_or(Ok(()))
        })
    }

    fn rollback(
        &self,
        turn_count: i64,
        _rewind_key: String,
    ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
        Box::pin(async move {
            let (workspace, error) = {
                let mut state = self.state.lock().unwrap();
                state.rollbacks.push(turn_count);
                (
                    state.rollback_workspace.clone(),
                    state.rollback_error.clone(),
                )
            };
            let restored = workspace.map(|workspace| {
                std::fs::read_to_string(workspace.join("tracked.txt"))
                    .expect("restored checkpoint is readable")
                    .replace("\r\n", "\n")
            });
            self.state
                .lock()
                .unwrap()
                .rollback_observations
                .push((turn_count, restored));
            if let Some(detail) = error {
                return Err(ProviderRuntimeError::Provider {
                    provider: "codex".to_owned(),
                    detail,
                });
            }
            Ok(())
        })
    }

    fn set_agent_activity_enabled(
        &self,
        enabled: bool,
    ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            state.agent_activity_transitions.push(enabled);
            state.agent_activity_results.pop_front().unwrap_or(Ok(()))
        })
    }

    fn cancel_activity_target(
        &self,
        _target: ProviderActivityNativeTarget,
    ) -> BoxRuntimeFuture<'_, Result<ActivityTargetDispatchDisposition, ProviderRuntimeError>> {
        Box::pin(async move {
            let (entered, release, active) = {
                let mut state = self.state.lock().unwrap();
                state.targeted_activity_cancellations += 1;
                (
                    state.targeted_activity_entered.clone(),
                    state.targeted_activity_release.clone(),
                    state.targeted_activity_active.clone(),
                )
            };
            active.fetch_add(1, Ordering::AcqRel);
            let _lease = TargetedActivityCallLease(active);
            if let Some(entered) = entered {
                entered.notify_one();
            }
            if let Some(release) = release {
                release.notified().await;
            }
            Ok(ActivityTargetDispatchDisposition::Delivered)
        })
    }

    fn next_event(&self) -> BoxRuntimeFuture<'_, Option<ProviderEvent>> {
        Box::pin(async move {
            let event = self.events.lock().await.recv().await;
            if event.is_none()
                && let Some(stream_ended) = self.state.lock().unwrap().stream_ended.clone()
            {
                stream_ended.notify_one();
            }
            event
        })
    }

    fn shutdown(&self) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
        Box::pin(async move {
            let (result, gate) = {
                let mut state = self.state.lock().unwrap();
                state.shutdowns += 1;
                (
                    state.shutdown_results.pop_front().unwrap_or(Ok(())),
                    state.shutdown_gate.take(),
                )
            };
            if let Some(gate) = gate {
                let _permit = gate.acquire().await.expect("shutdown gate remains open");
            }
            result
        })
    }
}

struct FakeFactory {
    state: Arc<StdMutex<DriverState>>,
    events: StdMutex<VecDeque<mpsc::Receiver<ProviderEvent>>>,
}

struct LaunchRaceFactory {
    state: Arc<StdMutex<DriverState>>,
    events: StdMutex<VecDeque<mpsc::Receiver<ProviderEvent>>>,
    controller: AgentActivityController,
}

impl ProviderDriverFactory for LaunchRaceFactory {
    fn create(
        &self,
        request: ProviderLaunchRequest,
    ) -> BoxRuntimeFuture<'_, Result<Arc<dyn ProviderDriver>, ProviderRuntimeError>> {
        Box::pin(async move {
            let provider = request.provider.clone();
            let provider_instance_id = request.provider_instance_id.clone();
            self.state.lock().unwrap().launches.push(request);
            self.controller.disable().await;
            let events = self
                .events
                .lock()
                .unwrap()
                .pop_front()
                .expect("event receiver");
            Ok(Arc::new(FakeDriver {
                provider,
                provider_instance_id,
                state: self.state.clone(),
                events: tokio::sync::Mutex::new(events),
            }) as Arc<dyn ProviderDriver>)
        })
    }
}

impl ProviderDriverFactory for FakeFactory {
    fn create(
        &self,
        request: ProviderLaunchRequest,
    ) -> BoxRuntimeFuture<'_, Result<Arc<dyn ProviderDriver>, ProviderRuntimeError>> {
        Box::pin(async move {
            let provider = request.provider.clone();
            let provider_instance_id = request.provider_instance_id.clone();
            self.state.lock().unwrap().launches.push(request);
            let events = self
                .events
                .lock()
                .unwrap()
                .pop_front()
                .expect("event receiver");
            Ok(Arc::new(FakeDriver {
                provider,
                provider_instance_id,
                state: self.state.clone(),
                events: tokio::sync::Mutex::new(events),
            }) as Arc<dyn ProviderDriver>)
        })
    }
}

#[tokio::test]
async fn fake_delivery_release_retains_bulk_permissions_before_waiters_arm() {
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let state = Arc::new(StdMutex::new(DriverState {
        delivery_release: Some(release.clone()),
        ..DriverState::default()
    }));
    let (_events_tx, events_rx) = mpsc::channel(1);
    let driver = Arc::new(FakeDriver {
        provider: "codex".to_owned(),
        provider_instance_id: Some("codex".to_owned()),
        state,
        events: tokio::sync::Mutex::new(events_rx),
    });
    release.add_permits(2);
    let mut deliveries = Vec::new();
    for text in ["first", "second"] {
        let driver = driver.clone();
        deliveries.push(tokio::spawn(async move {
            driver
                .deliver(
                    text.to_owned(),
                    Vec::new(),
                    "default".to_owned(),
                    format!("key-{text}"),
                )
                .await
        }));
    }
    timeout(Duration::from_millis(100), async {
        for delivery in deliveries {
            assert!(matches!(
                delivery.await.expect("delivery task"),
                ProviderDeliveryOutcome::Accepted { .. }
            ));
        }
    })
    .await
    .expect("bulk release permissions must survive before waiter registration");
}

struct NativeFixtureFactory {
    inner: NativeProviderDriverFactory,
    binary_path: Option<PathBuf>,
    endpoint: Option<String>,
    cwd: Option<PathBuf>,
    environment: Vec<(String, String)>,
    launches: Arc<StdMutex<Vec<ProviderLaunchRequest>>>,
}

impl ProviderDriverFactory for NativeFixtureFactory {
    fn create(
        &self,
        mut request: ProviderLaunchRequest,
    ) -> BoxRuntimeFuture<'_, Result<Arc<dyn ProviderDriver>, ProviderRuntimeError>> {
        Box::pin(async move {
            self.launches.lock().unwrap().push(request.clone());
            if let Some(binary_path) = self.binary_path.as_ref() {
                request.binary_path = binary_path.to_string_lossy().into_owned();
            }
            request.endpoint.clone_from(&self.endpoint);
            if let Some(cwd) = self.cwd.as_ref() {
                request.cwd = cwd.clone();
            }
            for (key, value) in &self.environment {
                request.environment.insert(key.clone(), value.clone());
            }
            self.inner.create(request).await
        })
    }
}

async fn engine() -> OrchestrationEngine {
    engine_and_database().await.0
}

async fn engine_and_database() -> (OrchestrationEngine, Database) {
    engine_and_database_with_options(EngineOptions::default()).await
}

async fn engine_and_database_with_options(
    options: EngineOptions,
) -> (OrchestrationEngine, Database) {
    let database = Database::open_in_memory().await.unwrap();
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .unwrap();
    let engine = OrchestrationEngine::start(database.clone(), options)
        .await
        .unwrap();
    engine
        .dispatch(
            serde_json::from_value(json!({
                "type":"project.create", "commandId":"project", "projectId":"p1", "title":"Project",
                "workspaceRoot":"C:/repo", "createdAt":NOW
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    engine
        .dispatch(
            serde_json::from_value(json!({
                "type":"thread.create", "commandId":"thread", "threadId":"t1", "projectId":"p1",
                "title":"Thread", "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                "runtimeMode":"full-access", "branch":null, "worktreePath":null, "createdAt":NOW
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    (engine, database)
}

fn activity_projection(engine: &OrchestrationEngine) -> ActivityProjection {
    ActivityProjection::new(ActivityRepository::new(
        engine.repositories().database().clone(),
    ))
}

fn native_event_id(value: &str) -> Option<ProviderNativeEventId> {
    Some(ProviderNativeEventId::new(value.to_owned()).expect("valid native event id"))
}

fn launch() -> ProviderLaunchRequest {
    ProviderLaunchRequest {
        thread_id: "t1".to_owned(),
        activity_causal_revision: 0,
        provider: "codex".to_owned(),
        provider_label: "codex".to_owned(),
        provider_instance_id: Some("codex".to_owned()),
        binary_path: "codex".to_owned(),
        cwd: "C:/repo".into(),
        runtime_mode: "full-access".to_owned(),
        interaction_mode: "default".to_owned(),
        model: Some("gpt-5".to_owned()),
        options: Vec::new(),
        custom_models: Vec::new(),
        service_tier: None,
        effort: None,
        agent: None,
        resume_cursor: None,
        environment: Default::default(),
        endpoint: None,
        server_password: None,
        mcp: None,
        codex_home: Some(resolve_codex_home_layout(
            None,
            None,
            dirs::home_dir()
                .as_deref()
                .unwrap_or_else(|| Path::new(".")),
        )),
        open_url: None,
    }
}

/// Points Claude at an empty configuration directory, so a launch resumes its cursor as given
/// and never reads the host's Claude configuration.
fn use_empty_claude_config(request: &mut ProviderLaunchRequest, temp: &TempDir) {
    request.environment.insert(
        "CLAUDE_CONFIG_DIR".to_owned(),
        temp.path()
            .join("claude-config")
            .to_string_lossy()
            .into_owned(),
    );
}

fn durable_turn_command(command_id: &str, text: &str) -> OrchestrationCommand {
    durable_turn_command_for(command_id, text, "codex", "gpt-5")
}

fn durable_turn_command_for(
    command_id: &str,
    text: &str,
    provider_instance_id: &str,
    model: &str,
) -> OrchestrationCommand {
    serde_json::from_value(json!({
        "type":"thread.turn.start", "commandId":command_id, "threadId":"t1",
        "message":{
            "messageId":format!("message-{command_id}"), "role":"user", "text":text,
            "attachments":[]
        },
        "modelSelection":{"instanceId":provider_instance_id,"model":model},
        "runtimeMode":"full-access", "interactionMode":"default", "createdAt":NOW
    }))
    .expect("durable turn command")
}

fn delivery_row(provider_kind: &str, delivery_key: &str) -> ProviderTurnDelivery {
    let command_id = format!("reconcile-{delivery_key}");
    ProviderTurnDelivery {
        mode: bibcode_server::orchestration::TurnDeliveryMode::Start,
        held: false,
        command_id: command_id.clone(),
        thread_id: "t1".to_owned(),
        message_id: format!("message-{delivery_key}"),
        provider_instance_id: provider_kind.to_owned(),
        provider_kind: provider_kind.to_owned(),
        provider_session_id: None,
        delivery_key: delivery_key.to_owned(),
        payload: serde_json::to_value(durable_turn_command_for(
            &command_id,
            "recover",
            provider_kind,
            if provider_kind == "opencode" {
                "openai/gpt-5"
            } else {
                "gpt-5"
            },
        ))
        .expect("delivery payload"),
        state: TurnDeliveryState::Sending,
        attempts: 1,
        last_error: None,
        created_at: NOW.to_owned(),
        updated_at: NOW.to_owned(),
    }
}

async fn seed_sending_delivery(database: &Database, row: ProviderTurnDelivery) {
    database
        .call(move |connection| {
            connection.execute(
                "INSERT INTO orchestration_command_receipts (command_id, aggregate_kind, aggregate_id, accepted_at, result_sequence, status, error, payload_digest) VALUES (?, 'thread', ?, ?, 0, 'accepted', NULL, 'durable-restart-digest')",
                rusqlite::params![&row.command_id, &row.thread_id, &row.created_at],
            )?;
            connection.execute(
                "INSERT INTO provider_turn_outbox (command_id, thread_id, message_id, provider_instance_id, provider_kind, provider_session_id, delivery_key, payload_json, state, attempts, last_error, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'sending', ?, NULL, ?, ?)",
                rusqlite::params![
                    row.command_id,
                    row.thread_id,
                    row.message_id,
                    row.provider_instance_id,
                    row.provider_kind,
                    row.provider_session_id,
                    row.delivery_key,
                    row.payload.to_string(),
                    row.attempts,
                    row.created_at,
                    row.updated_at,
                ],
            )?;
            Ok(())
        })
        .await
        .expect("seed accepted provider turn before durable state transition");
}

async fn seed_pending_delivery(database: &Database, mut row: ProviderTurnDelivery) {
    row.state = TurnDeliveryState::Pending;
    row.attempts = 0;
    database
        .call(move |connection| {
            connection.execute(
                "INSERT INTO orchestration_command_receipts (command_id, aggregate_kind, aggregate_id, accepted_at, result_sequence, status, error, payload_digest) VALUES (?, 'thread', ?, ?, 0, 'accepted', NULL, 'durable-live-retry-digest')",
                rusqlite::params![&row.command_id, &row.thread_id, &row.created_at],
            )?;
            connection.execute(
                "INSERT INTO provider_turn_outbox (command_id, thread_id, message_id, provider_instance_id, provider_kind, provider_session_id, delivery_key, payload_json, state, attempts, last_error, created_at, updated_at) VALUES (?, ?, ?, ?, ?, NULL, ?, ?, 'pending', 0, NULL, ?, ?)",
                rusqlite::params![
                    row.command_id,
                    row.thread_id,
                    row.message_id,
                    row.provider_instance_id,
                    row.provider_kind,
                    row.delivery_key,
                    row.payload.to_string(),
                    row.created_at,
                    row.updated_at,
                ],
            )?;
            Ok(())
        })
        .await
        .expect("seed pending provider turn");
}

fn write_route_settings(
    settings: &TempDir,
    binary_path: &str,
    endpoint: &str,
    environment_value: &str,
) {
    std::fs::write(
        settings.path().join("settings.json"),
        serde_json::to_vec(&json!({
            "providerInstances": {
                "route-cursor": {
                    "driver": "cursor",
                    "enabled": true,
                    "config": {
                        "binaryPath": binary_path,
                        "apiEndpoint": endpoint
                    },
                    "environment": [{
                        "name": "ROUTE_ENV",
                        "value": environment_value,
                        "valueRedacted": false
                    }]
                }
            }
        }))
        .expect("route settings json"),
    )
    .expect("write route settings");
}

fn write_live_retry_settings(settings: &TempDir, provider: &str, instance_id: &str) {
    let instance = match provider {
        "claudeAgent" => json!({
            "driver": "claudeAgent",
            "enabled": true,
            "config": {"binaryPath": "claude-route"}
        }),
        "cursor" => json!({
            "driver": "cursor",
            "enabled": true,
            "config": {
                "binaryPath": "cursor-route",
                "apiEndpoint": "https://cursor-route.invalid"
            }
        }),
        _ => unreachable!(),
    };
    let mut instances = serde_json::Map::new();
    instances.insert(instance_id.to_owned(), instance);
    std::fs::write(
        settings.path().join("settings.json"),
        serde_json::to_vec(&json!({"providerInstances": instances})).expect("retry settings json"),
    )
    .expect("write retry settings");
}

async fn freeze_row_route(
    engine: &OrchestrationEngine,
    settings: &TempDir,
    row: &mut ProviderTurnDelivery,
) -> OrchestrationCommand {
    let command = serde_json::from_value::<OrchestrationCommand>(row.payload.clone())
        .expect("durable route command");
    freeze_delivery_route(
        engine,
        &settings.path().to_path_buf(),
        &command,
        &mut row.payload,
    )
    .await
    .expect("freeze configured provider route");
    command
}

async fn admit_and_freeze_sending_delivery(
    engine: &OrchestrationEngine,
    settings: &TempDir,
    provider_kind: &str,
    provider_instance_id: &str,
    provider_session_id: &str,
    delivery_key: &str,
) -> (ProviderTurnDelivery, Arc<ProviderRuntimeSupervisor>) {
    let delivery_entered = Arc::new(tokio::sync::Notify::new());
    let state = Arc::new(StdMutex::new(DriverState {
        start_results: VecDeque::from([Ok(StartedSession {
            resume_cursor: Some(if provider_kind == "codex" {
                json!({"threadId":provider_session_id})
            } else {
                json!({"sessionId":provider_session_id})
            }),
            runtime_payload: None,
            activity_capabilities: ActivityCapabilities::none(),
        })]),
        delivery_entered: Some(delivery_entered.clone()),
        delivery_release: Some(Arc::new(tokio::sync::Semaphore::new(0))),
        ..DriverState::default()
    }));
    let (_events_tx, events_rx) = mpsc::channel(1);
    let supervisor = Arc::new(ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::from([events_rx])),
        }),
        activity_projection(engine),
        SupervisorOptions::default(),
    ));
    let delivery = Arc::new(TurnDeliveryService::start(
        engine.clone(),
        supervisor.clone(),
        settings.path().to_path_buf(),
    ));
    let mut registry = RpcRegistry::empty();
    register_orchestration_rpc_with_delivery(
        &mut registry,
        engine.clone(),
        supervisor.clone(),
        settings.path().to_path_buf(),
        delivery.clone(),
        attachment_upload_registry(),
    );
    let runtime = ServerRuntime::start_with_registry(test_config(settings), registry)
        .await
        .expect("admission runtime");
    let (mut socket, _) = connect_async(format!("ws://{}/ws", runtime.local_addr()))
        .await
        .expect("admission websocket");
    let command_id = format!("reconcile-{delivery_key}");
    rpc_request(
        &mut socket,
        "806",
        serde_json::to_value(durable_turn_command_for(
            &command_id,
            "recover",
            provider_instance_id,
            if provider_kind == "opencode" {
                "openai/gpt-5"
            } else {
                "gpt-5"
            },
        ))
        .expect("organic durable command"),
    )
    .await;
    rpc_response(&mut socket, "806")
        .await
        .expect("organic turn admission");
    timeout(Duration::from_secs(10), delivery_entered.notified())
        .await
        .expect("initial provider delivery entered");
    let row = engine
        .repositories()
        .get_provider_turn_delivery(command_id.clone())
        .await
        .expect("organic delivery row")
        .expect("organic outbox row");
    assert_eq!(row.state, TurnDeliveryState::Sending);
    assert_eq!(row.attempts, 1);
    assert_eq!(row.provider_instance_id, provider_instance_id);
    assert_eq!(row.provider_kind, provider_kind);
    assert_eq!(
        row.provider_session_id.as_deref(),
        Some(provider_session_id),
        "native session identity is durable before the first provider send"
    );

    socket.close(None).await.expect("admission websocket close");
    runtime.shutdown();
    runtime.join().await.expect("admission runtime shutdown");
    delivery.shutdown().await;
    let row = engine
        .repositories()
        .get_provider_turn_delivery(command_id)
        .await
        .expect("organic restart row")
        .expect("organic outbox row survives");
    assert_eq!(row.state, TurnDeliveryState::Sending);
    assert_eq!(
        row.provider_session_id.as_deref(),
        Some(provider_session_id)
    );
    (row, supervisor)
}

async fn captured_json_request(path: &Path, predicate: impl Fn(&Value) -> bool) -> Value {
    timeout(Duration::from_secs(5), async {
        loop {
            let content = std::fs::read_to_string(path).unwrap_or_default();
            if let Some(request) = content
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .find(|request| predicate(request))
            {
                return request;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "captured provider request at {}; content={}",
            path.display(),
            std::fs::read_to_string(path).unwrap_or_default()
        )
    })
}

async fn captured_complete_ndjson_with_bytes(path: &Path) -> (Vec<u8>, Vec<Value>) {
    timeout(Duration::from_secs(5), async {
        loop {
            let bytes = std::fs::read(path).unwrap_or_default();
            if (bytes.is_empty() || bytes.ends_with(b"\n"))
                && let Ok(content) = std::str::from_utf8(&bytes)
                && let Some(values) = content
                    .lines()
                    .map(|line| serde_json::from_str::<Value>(line).ok())
                    .collect::<Option<Vec<_>>>()
            {
                break (bytes, values);
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "provider capture never reached a complete NDJSON boundary: path={}, capture={:?}",
            path.display(),
            std::fs::read_to_string(path)
        )
    })
}

async fn captured_complete_ndjson(path: &Path) -> Vec<Value> {
    captured_complete_ndjson_with_bytes(path).await.1
}

fn claude_stop_task_targets(requests: &[Value]) -> Vec<String> {
    requests
        .iter()
        .filter(|request| request["request"]["subtype"] == "stop_task")
        .filter_map(|request| request["request"]["task_id"].as_str().map(str::to_owned))
        .collect()
}

fn claude_root_interrupt_count(requests: &[Value]) -> usize {
    requests
        .iter()
        .filter(|request| request["request"]["subtype"] == "interrupt")
        .count()
}

fn assert_exact_claude_stop_task_request(request: &Value, expected_task_id: &str) {
    let object = request
        .as_object()
        .expect("Claude control request must be an object");
    assert_eq!(
        object.len(),
        3,
        "Claude targeted control must not carry an unbounded native payload: {request}"
    );
    assert_eq!(request["type"], "control_request");
    assert!(
        request["request_id"]
            .as_str()
            .is_some_and(|request_id| request_id.starts_with("bibcode-")),
        "Claude targeted control must carry a bounded correlated request id: {request}"
    );
    assert_eq!(
        request["request"],
        json!({"subtype":"stop_task", "task_id":expected_task_id})
    );
}

fn image_attachment(temp: &TempDir) -> Value {
    let directory = temp.path().join("attachments");
    std::fs::create_dir_all(&directory).expect("attachment directory");
    std::fs::write(directory.join("image-1"), b"image bytes").expect("attachment image");
    json!({
        "type":"image",
        "id":"image-1",
        "name":"screen.png",
        "mimeType":"image/png",
        "sizeBytes":11,
    })
}

fn persisted_runtime(thread_id: &str, status: &str, last_seen_at: &str) -> ProviderSessionRuntime {
    ProviderSessionRuntime {
        thread_id: thread_id.to_owned(),
        provider_name: "codex".to_owned(),
        provider_instance_id: Some("codex".to_owned()),
        adapter_key: "codex-app-server".to_owned(),
        runtime_mode: "full-access".to_owned(),
        status: status.to_owned(),
        last_seen_at: last_seen_at.to_owned(),
        resume_cursor: Some(json!({"threadId":format!("provider-{thread_id}")})),
        runtime_payload: None,
    }
}

async fn project_session(engine: &OrchestrationEngine, thread_id: &str, status: &str) {
    engine
        .dispatch(OrchestrationCommand::ThreadSessionSet {
            command_id: format!("{thread_id}-{status}-session"),
            thread_id: thread_id.to_owned(),
            session: SessionInput {
                thread_id: thread_id.to_owned(),
                status: status.to_owned(),
                provider_name: Some("codex".to_owned()),
                provider_instance_id: Some("codex".to_owned()),
                runtime_mode: "full-access".to_owned(),
                active_turn_id: None,
                last_error: None,
                last_error_class: None,
                updated_at: NOW.to_owned(),
            },
            created_at: NOW.to_owned(),
        })
        .await
        .unwrap();
}

struct SupervisorEffectsCallbacks {
    supervisor: Arc<ProviderRuntimeSupervisor>,
    engine: OrchestrationEngine,
    settings: TempDir,
    workspace: PathBuf,
}

fn supervisor_effects(
    supervisor: &Arc<ProviderRuntimeSupervisor>,
    engine: &OrchestrationEngine,
    workspace: &Path,
) -> Arc<SupervisorEffectsCallbacks> {
    Arc::new(SupervisorEffectsCallbacks {
        supervisor: supervisor.clone(),
        engine: engine.clone(),
        settings: TempDir::new().unwrap(),
        workspace: workspace.to_path_buf(),
    })
}

impl OrchestrationEffectCallbacks for SupervisorEffectsCallbacks {
    fn workspace_for_thread<'a>(
        &'a self,
        _thread_id: &'a str,
    ) -> BoxEffectFuture<'a, Option<PathBuf>> {
        Box::pin(async move { Ok(Some(self.workspace.clone())) })
    }

    fn rollback_provider<'a>(
        &'a self,
        thread_id: &'a str,
        turns: i64,
        rewind_key: &'a str,
    ) -> BoxEffectFuture<'a, ProviderRewind> {
        Box::pin(async move {
            rewind_thread_conversation(
                &self.supervisor,
                &self.engine,
                &self.settings.path().to_path_buf(),
                thread_id,
                turns,
                rewind_key,
            )
            .await
        })
    }

    fn stop_provider<'a>(&'a self, _thread_id: &'a str) -> BoxEffectFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }

    fn close_terminals<'a>(&'a self, _thread_id: &'a str) -> BoxEffectFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }

    fn refresh_workspace<'a>(&'a self, _cwd: &'a Path) -> BoxEffectFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git starts");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output is UTF-8")
        .trim()
        .to_owned()
}

fn git_succeeds(cwd: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .expect("git starts")
        .success()
}

fn initialize_repository() -> TempDir {
    let directory = TempDir::new().expect("temporary repository");
    git(directory.path(), &["init"]);
    git(directory.path(), &["config", "user.name", "BiBCode Test"]);
    git(
        directory.path(),
        &["config", "user.email", "bibcode@example.test"],
    );
    std::fs::write(directory.path().join("tracked.txt"), "baseline\n").unwrap();
    git(directory.path(), &["add", "."]);
    git(directory.path(), &["commit", "-m", "baseline"]);
    directory
}

async fn project_checkpoint(
    engine: &OrchestrationEngine,
    workspace: &Path,
    turn_count: i64,
    content: &str,
) {
    std::fs::write(workspace.join("tracked.txt"), content).unwrap();
    orchestration_effects::capture_checkpoint(workspace, "t1", turn_count)
        .await
        .unwrap();
    if turn_count > 0 {
        engine
            .dispatch(
                serde_json::from_value(json!({
                    "type":"thread.turn.diff.complete",
                    "commandId":format!("diff-{turn_count}"),
                    "threadId":"t1",
                    "turnId":format!("turn-{turn_count}"),
                    "checkpointTurnCount":turn_count,
                    "checkpointRef":orchestration_effects::checkpoint_ref("t1", turn_count),
                    "status":"ready",
                    "files":[],
                    "assistantMessageId":format!("assistant-{turn_count}"),
                    "completedAt":NOW,
                    "createdAt":NOW
                }))
                .unwrap(),
            )
            .await
            .unwrap();
    }
}

async fn wait_for_event(
    events: &mut tokio::sync::broadcast::Receiver<bibcode_server::persistence::OrchestrationEvent>,
    predicate: impl Fn(&bibcode_server::persistence::OrchestrationEvent) -> bool,
) {
    timeout(CHECKPOINT_RPC_INTEGRATION_DEADLINE, async {
        loop {
            match events.recv().await {
                Ok(event) if predicate(&event) => return,
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    panic!("orchestration event stream closed")
                }
            }
        }
    })
    .await
    .expect("expected orchestration event");
}

fn test_config(temp: &TempDir) -> ServerConfig {
    ServerConfig::new(temp.path())
        .with_bind("127.0.0.1", 0)
        .with_unsafe_no_auth()
}

async fn rpc_request<S>(socket: &mut WebSocketStream<S>, id: &str, payload: Value)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    socket
        .send(Message::Text(
            json!({
                "_tag":"Request",
                "id":id,
                "tag":"orchestration.dispatchCommand",
                "payload":payload,
                "headers":[]
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("send orchestration RPC request");
}

async fn rpc_response<S>(socket: &mut WebSocketStream<S>, id: &str) -> Result<Value, Value>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let frame = timeout(Duration::from_secs(10), next_frame_past_heartbeat(socket))
        .await
        .expect("orchestration RPC response timeout")
        .expect("WebSocket remains open")
        .expect("valid WebSocket frame");
    let Message::Text(text) = frame else {
        panic!("expected text WebSocket message, got {frame:?}");
    };
    match serde_json::from_str::<ServerMessage>(&text).expect("valid server RPC message") {
        ServerMessage::Exit {
            request_id,
            exit: RpcExit::Success { value },
        } if request_id == RequestId::try_from(id).unwrap() => Ok(value.unwrap_or(Value::Null)),
        ServerMessage::Exit {
            request_id,
            exit: RpcExit::Failure { cause },
        } if request_id == RequestId::try_from(id).unwrap() => {
            Err(serde_json::to_value(cause).unwrap())
        }
        message => panic!("unexpected orchestration RPC response: {message:?}"),
    }
}

async fn tagged_rpc_request<S>(
    socket: &mut WebSocketStream<S>,
    id: &str,
    tag: &str,
    payload: Value,
) -> Result<Value, Value>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    socket
        .send(Message::Text(
            json!({
                "_tag":"Request",
                "id":id,
                "tag":tag,
                "payload":payload,
                "headers":[]
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("send tagged RPC request");
    rpc_response(socket, id).await
}

async fn try_tagged_rpc_request_until<S>(
    socket: &mut WebSocketStream<S>,
    id: &str,
    tag: &str,
    payload: Value,
    deadline: tokio::time::Instant,
) -> Result<Result<Value, Value>, String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let expected_request_id = RequestId::try_from(id)
        .map_err(|error| format!("invalid unary RPC request ID {id:?}: {error}"))?;
    timeout_at(
        deadline,
        socket.send(Message::Text(
            json!({
                "_tag":"Request",
                "id":id,
                "tag":tag,
                "payload":payload,
                "headers":[]
            })
            .to_string()
            .into(),
        )),
    )
    .await
    .map_err(|_| format!("unary RPC {tag} send deadline elapsed"))?
    .map_err(|error| format!("failed to send unary RPC {tag}: {error}"))?;

    let frame = timeout_at(deadline, next_frame_past_heartbeat(socket))
        .await
        .map_err(|_| format!("unary RPC {tag} response deadline elapsed"))?
        .ok_or_else(|| format!("unary RPC {tag} WebSocket closed before response"))?
        .map_err(|error| format!("invalid unary RPC {tag} frame: {error}"))?;
    let Message::Text(text) = frame else {
        return Err(format!(
            "expected text unary RPC {tag} frame, got {frame:?}"
        ));
    };
    let message = serde_json::from_str::<ServerMessage>(&text)
        .map_err(|error| format!("invalid unary RPC {tag} message: {error}"))?;
    match message {
        ServerMessage::Exit {
            request_id,
            exit: RpcExit::Success { value },
        } if request_id == expected_request_id => Ok(Ok(value.unwrap_or(Value::Null))),
        ServerMessage::Exit {
            request_id,
            exit: RpcExit::Failure { cause },
        } if request_id == expected_request_id => serde_json::to_value(cause)
            .map(Err)
            .map_err(|error| format!("failed to encode unary RPC {tag} failure: {error}")),
        ServerMessage::Exit { request_id, .. } => Err(format!(
            "unary RPC {tag} response ID mismatch: expected {expected_request_id}, got {request_id}"
        )),
        message => Err(format!("unexpected unary RPC {tag} response: {message:?}")),
    }
}

async fn stream_rpc_request<S>(socket: &mut WebSocketStream<S>, id: &str, tag: &str, payload: Value)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    socket
        .send(Message::Text(
            json!({
                "_tag":"Request",
                "id":id,
                "tag":tag,
                "payload":payload,
                "headers":[]
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("send stream RPC request");
}

async fn try_stream_rpc_request<S>(
    socket: &mut WebSocketStream<S>,
    id: &str,
    tag: &str,
    payload: Value,
) -> Result<(), String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    socket
        .send(Message::Text(
            json!({
                "_tag":"Request",
                "id":id,
                "tag":tag,
                "payload":payload,
                "headers":[]
            })
            .to_string()
            .into(),
        ))
        .await
        .map_err(|error| format!("failed to send Activity stream request: {error}"))
}

async fn stream_rpc_message<S>(socket: &mut WebSocketStream<S>) -> ServerMessage
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let frame = timeout(Duration::from_secs(10), next_frame_past_heartbeat(socket))
        .await
        .expect("stream RPC response timeout")
        .expect("stream WebSocket remains open")
        .expect("valid stream WebSocket frame");
    let Message::Text(text) = frame else {
        panic!("expected text stream WebSocket message, got {frame:?}");
    };
    serde_json::from_str(&text).expect("valid stream RPC message")
}

async fn stream_rpc_message_until<S>(
    socket: &mut WebSocketStream<S>,
    deadline: tokio::time::Instant,
) -> Result<ServerMessage, String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let frame = timeout_at(deadline, next_frame_past_heartbeat(socket))
        .await
        .map_err(|_| "Activity stream deadline elapsed".to_owned())?
        .ok_or_else(|| "Activity stream closed before convergence".to_owned())?
        .map_err(|error| format!("invalid Activity stream frame: {error}"))?;
    let Message::Text(text) = frame else {
        return Err(format!(
            "expected text Activity stream frame, got {frame:?}"
        ));
    };
    serde_json::from_str(&text).map_err(|error| format!("invalid Activity stream message: {error}"))
}

async fn ack_stream_rpc<S>(socket: &mut WebSocketStream<S>, id: &str)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    socket
        .send(Message::Text(
            json!({"_tag":"Ack","requestId":id}).to_string().into(),
        ))
        .await
        .expect("ack stream RPC chunk");
}

async fn assert_codex_restart_reconciliation(
    mode: &'static str,
    expected: ProviderReconciliationOutcome,
    should_resend: bool,
) {
    const UNIX_FIXTURE: &str = r#"#!/bin/sh
read_count=0
while IFS= read -r line; do
  [ -z "$BIBCODE_CAPTURE" ] || printf '%s\n' "$line" >> "$BIBCODE_CAPTURE"
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*) printf '{"id":%s,"result":{"userAgent":"fixture"}}\n' "$id" ;;
    *'"method":"thread/resume"'*|*'"method":"thread/start"'*) printf '{"id":%s,"result":{"cwd":"/tmp","model":"gpt-5","thread":{"id":"native-codex-thread"}}}\n' "$id" ;;
    *'"method":"mcpServerStatus/list"'*) printf '{"id":%s,"result":{"data":[],"nextCursor":null}}\n' "$id" ;;
    *'"method":"turn/start"'*) printf '{"id":%s,"result":{"turn":{"id":"native-codex-turn"}}}\n' "$id" ;;
    *'"method":"thread/read"'*)
      if [ "$BIBCODE_READBACK_MODE" = found ]; then
        printf '{"id":%s,"result":{"thread":{"id":"native-codex-thread","turns":[{"id":"native-codex-turn","items":[{"id":"native-user","type":"userMessage","clientId":"%s","content":[]}]}]}}}\n' "$id" "$BIBCODE_EXPECTED_DELIVERY_KEY"
      elif [ "$BIBCODE_READBACK_MODE" = absent ]; then
        printf '{"id":%s,"result":{"thread":{"id":"native-codex-thread","turns":[]}}}\n' "$id"
      else
        printf '{"id":%s,"result":{}}\n' "$id"
      fi ;;
    *'"method":"shutdown"'*) printf '{"id":%s,"result":null}\n' "$id" ;;
  esac
done
"#;
    const WINDOWS_FIXTURE: &str = r#"
$readCount = 0
while ($null -ne ($line = [Console]::In.ReadLine())) {
  if ($env:BIBCODE_CAPTURE) { Add-Content -LiteralPath $env:BIBCODE_CAPTURE -Value $line }
  try { $request = $line | ConvertFrom-Json } catch { continue }
  $id = [string]$request.id
  $response = $null
  switch ([string]$request.method) {
    "initialize" { $response = '{"id":' + $id + ',"result":{"userAgent":"fixture"}}' }
    "thread/resume" { $response = '{"id":' + $id + ',"result":{"cwd":"C:\\tmp","model":"gpt-5","thread":{"id":"native-codex-thread"}}}' }
    "thread/start" { $response = '{"id":' + $id + ',"result":{"cwd":"C:\\tmp","model":"gpt-5","thread":{"id":"native-codex-thread"}}}' }
    "mcpServerStatus/list" { $response = '{"id":' + $id + ',"result":{"data":[],"nextCursor":null}}' }
    "turn/start" { $response = '{"id":' + $id + ',"result":{"turn":{"id":"native-codex-turn"}}}' }
    "thread/read" {
      if ($env:BIBCODE_READBACK_MODE -eq "found") {
        $response = '{"id":' + $id + ',"result":{"thread":{"id":"native-codex-thread","turns":[{"id":"native-codex-turn","items":[{"id":"native-user","type":"userMessage","clientId":"' + $env:BIBCODE_EXPECTED_DELIVERY_KEY + '","content":[]}]}]}}}'
      } elseif ($env:BIBCODE_READBACK_MODE -eq "absent") {
        $response = '{"id":' + $id + ',"result":{"thread":{"id":"native-codex-thread","turns":[]}}}'
      } else {
        $response = '{"id":' + $id + ',"result":{}}'
      }
    }
    "shutdown" { $response = '{"id":' + $id + ',"result":null}' }
  }
  if ($null -ne $response) { [Console]::Out.WriteLine($response); [Console]::Out.Flush() }
}
"#;

    let (engine, _) = engine_and_database().await;
    let temp = TempDir::new().expect("Codex fixture directory");
    let fixture = executable_fixture(
        &temp,
        "durable-codex-fixture",
        UNIX_FIXTURE,
        WINDOWS_FIXTURE,
    );
    let capture = temp.path().join("codex-requests.jsonl");
    let launches = Arc::new(StdMutex::new(Vec::new()));
    let settings = TempDir::new().expect("settings");
    std::fs::write(
        settings.path().join("settings.json"),
        serde_json::to_vec(&json!({
            "providerInstances": {
                "frozen-codex": {
                    "driver": "codex",
                    "enabled": true,
                    "config": {"binaryPath": "frozen-codex-binary"}
                }
            }
        }))
        .expect("settings json"),
    )
    .expect("write settings");
    let (row, original_supervisor) = admit_and_freeze_sending_delivery(
        &engine,
        &settings,
        "codex",
        "frozen-codex",
        "native-codex-thread",
        "stable-codex-key",
    )
    .await;
    let supervisor = Arc::new(ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(NativeFixtureFactory {
            inner: NativeProviderDriverFactory::new(temp.path().join("attachments")),
            binary_path: Some(fixture),
            endpoint: None,
            cwd: Some(temp.path().to_path_buf()),
            environment: vec![
                (
                    "BIBCODE_CAPTURE".to_owned(),
                    capture.to_string_lossy().into_owned(),
                ),
                ("BIBCODE_READBACK_MODE".to_owned(), mode.to_owned()),
                (
                    "BIBCODE_EXPECTED_DELIVERY_KEY".to_owned(),
                    row.delivery_key.clone(),
                ),
            ],
            launches: launches.clone(),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    ));

    let delivery = if mode == "unavailable" {
        let outcome = reconcile_orchestration_turn(
            &supervisor,
            &engine,
            &settings.path().to_path_buf(),
            row.clone(),
        )
        .await;
        assert_eq!(outcome, expected);
        None
    } else {
        let delivery = TurnDeliveryService::start(
            engine.clone(),
            supervisor.clone(),
            settings.path().to_path_buf(),
        );
        let completed = timeout(Duration::from_secs(10), async {
            loop {
                let persisted = engine
                    .repositories()
                    .get_provider_turn_delivery(row.command_id.clone())
                    .await
                    .expect("delivery row")
                    .expect("persisted delivery");
                if persisted.state == TurnDeliveryState::Delivered {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        if completed.is_err() {
            let persisted = engine
                .repositories()
                .get_provider_turn_delivery(row.command_id.clone())
                .await
                .expect("delivery row")
                .expect("persisted delivery");
            panic!(
                "durable dispatcher did not complete Codex {mode} recovery: state={:?}, error={:?}",
                persisted.state, persisted.last_error
            );
        }
        Some(delivery)
    };
    assert_eq!(
        launches.lock().unwrap()[0].resume_cursor,
        Some(json!({"threadId":"native-codex-thread"}))
    );
    assert_eq!(
        launches.lock().unwrap()[0].provider_instance_id.as_deref(),
        Some("frozen-codex")
    );
    if mode == "found" {
        let invalid: OrchestrationCommand = serde_json::from_value(json!({
            "type":"thread.turn.start", "commandId":"codex-rejected", "threadId":"t1",
            "message":{
                "messageId":"message-rejected", "role":"user", "text":"invalid",
                "attachments":[{
                    "type":"file", "id":"missing-file", "name":"missing.txt",
                    "mimeType":"text/plain", "sizeBytes":1
                }]
            },
            "modelSelection":{"instanceId":"codex","model":"gpt-5"},
            "runtimeMode":"full-access", "interactionMode":"default", "createdAt":NOW
        }))
        .expect("invalid materialization command remains schema-valid");
        assert!(matches!(
            deliver_orchestration_turn(
                &supervisor,
                &engine,
                &settings.path().to_path_buf(),
                invalid,
                "rejected-key".to_owned(),
            )
            .await,
            ProviderDeliveryOutcome::Rejected { .. }
        ));
    }
    let sends_before = std::fs::read_to_string(&capture)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|request| request["method"] == "turn/start")
        .count();
    assert_eq!(
        sends_before,
        usize::from(should_resend),
        "Found prevents resend; Absent permits one resend"
    );
    if should_resend {
        let request =
            captured_json_request(&capture, |request| request["method"] == "turn/start").await;
        assert_eq!(request["params"]["clientUserMessageId"], row.delivery_key);
    }

    if let Some(delivery) = delivery {
        delivery.shutdown().await;
    }
    supervisor.shutdown().await.expect("supervisor shutdown");
    original_supervisor
        .shutdown()
        .await
        .expect("original supervisor shutdown");
    engine.shutdown().await;
}

#[tokio::test]
async fn durable_codex_restart_reconciliation_uses_real_adapter_and_exact_key() {
    assert_codex_restart_reconciliation("found", ProviderReconciliationOutcome::Found, false).await;
    assert_codex_restart_reconciliation("absent", ProviderReconciliationOutcome::Absent, true)
        .await;
    assert_codex_restart_reconciliation(
        "unavailable",
        ProviderReconciliationOutcome::Unavailable {
            detail: "Invalid Codex payload: thread/read response missing thread".to_owned(),
        },
        false,
    )
    .await;
}

#[tokio::test]
async fn claude_and_cursor_retry_definitely_not_sent_on_the_same_frozen_live_session() {
    for (provider_kind, instance_id, binary_path, endpoint) in [
        ("claudeAgent", "route-claude", "claude-route", None),
        (
            "cursor",
            "route-cursor",
            "cursor-route",
            Some("https://cursor-route.invalid"),
        ),
    ] {
        let (engine, database) = engine_and_database().await;
        let settings = TempDir::new().expect("settings");
        write_live_retry_settings(&settings, provider_kind, instance_id);
        let command_id = format!("live-retry-{provider_kind}");
        let mut row = delivery_row(provider_kind, &format!("live-retry-key-{provider_kind}"));
        row.command_id = command_id.clone();
        row.message_id = format!("message-{command_id}");
        row.provider_instance_id = instance_id.to_owned();
        row.payload = serde_json::to_value(durable_turn_command_for(
            &command_id,
            "retry safely",
            instance_id,
            "provider-model",
        ))
        .expect("retry route payload");
        let _command = freeze_row_route(&engine, &settings, &mut row).await;
        seed_pending_delivery(&database, row.clone()).await;
        let state = Arc::new(StdMutex::new(DriverState {
            start_results: VecDeque::from([Ok(started_session("same-live-session"))]),
            delivery_outcomes: VecDeque::from([
                ProviderDeliveryOutcome::DefinitelyNotSent {
                    detail: "dns lookup failed before request write".to_owned(),
                },
                ProviderDeliveryOutcome::Accepted {
                    turn_id: Some("accepted-on-second-attempt".to_owned()),
                },
            ]),
            ..DriverState::default()
        }));
        let (_events_tx, events_rx) = mpsc::channel(1);
        let supervisor = Arc::new(ProviderRuntimeSupervisor::start(
            engine.clone(),
            Arc::new(FakeFactory {
                state: state.clone(),
                events: StdMutex::new(VecDeque::from([events_rx])),
            }),
            activity_projection(&engine),
            SupervisorOptions::default(),
        ));
        let mut active_launch = launch();
        active_launch.provider = provider_kind.to_owned();
        active_launch.provider_label = provider_kind.to_owned();
        active_launch.provider_instance_id = Some(instance_id.to_owned());
        active_launch.binary_path = binary_path.to_owned();
        active_launch.endpoint = endpoint.map(str::to_owned);
        active_launch.model = Some("provider-model".to_owned());
        active_launch.codex_home = None;
        supervisor
            .launch(active_launch)
            .await
            .expect("same live provider session");
        let service = TurnDeliveryService::start(
            engine.clone(),
            supervisor.clone(),
            settings.path().to_path_buf(),
        );

        let delivery = timeout(Duration::from_secs(5), async {
            loop {
                let delivery = engine
                    .repositories()
                    .get_provider_turn_delivery(command_id.clone())
                    .await
                    .expect("retry row")
                    .expect("durable delivery");
                if delivery.state == TurnDeliveryState::Delivered
                    || delivery.state == TurnDeliveryState::Failed
                {
                    break delivery;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("live retry reaches a terminal state");
        assert_eq!(
            delivery.state,
            TurnDeliveryState::Delivered,
            "{provider_kind}: {:?}",
            delivery.last_error
        );
        assert_eq!(delivery.attempts, 2, "{provider_kind}");
        assert_eq!(
            delivery.provider_session_id.as_deref(),
            Some("same-live-session"),
            "{provider_kind} must retain the exact first-attempt session"
        );
        assert_eq!(
            state.lock().unwrap().delivery_started,
            vec!["retry safely", "retry safely"],
            "{provider_kind} should make exactly two live attempts"
        );

        service.shutdown().await;
        supervisor.shutdown().await.expect("supervisor shutdown");
        engine.shutdown().await;
    }
}

#[tokio::test]
async fn durable_delivery_blocks_binary_endpoint_and_environment_route_drift_before_launch() {
    for drift in ["binary", "endpoint", "environment"] {
        let (engine, database) = engine_and_database().await;
        let settings = TempDir::new().expect("settings");
        write_route_settings(&settings, "cursor-a", "https://route-a.invalid", "env-a");
        let command_id = format!("route-drift-{drift}");
        let mut row = delivery_row("cursor", &format!("route-key-{drift}"));
        row.command_id = command_id.clone();
        row.message_id = format!("message-{command_id}");
        row.provider_instance_id = "route-cursor".to_owned();
        row.payload = serde_json::to_value(durable_turn_command_for(
            &command_id,
            "must not send",
            "route-cursor",
            "cursor-model",
        ))
        .expect("route payload");
        let command = freeze_row_route(&engine, &settings, &mut row).await;
        assert!(
            !row.payload.to_string().contains("env-a"),
            "environment values participate in the digest but are never persisted"
        );
        seed_sending_delivery(&database, row.clone()).await;

        let (binary, endpoint, environment) = match drift {
            "binary" => ("cursor-b", "https://route-a.invalid", "env-a"),
            "endpoint" => ("cursor-a", "https://route-b.invalid", "env-a"),
            "environment" => ("cursor-a", "https://route-a.invalid", "env-b"),
            _ => unreachable!(),
        };
        write_route_settings(&settings, binary, endpoint, environment);
        let state = Arc::new(StdMutex::new(DriverState::default()));
        let supervisor = ProviderRuntimeSupervisor::start(
            engine.clone(),
            Arc::new(FakeFactory {
                state: state.clone(),
                events: StdMutex::new(VecDeque::new()),
            }),
            activity_projection(&engine),
            SupervisorOptions::default(),
        );

        let outcome = deliver_durable_orchestration_turn(
            &supervisor,
            &engine,
            &settings.path().to_path_buf(),
            command,
            row.delivery_key,
        )
        .await;
        assert!(matches!(
            outcome,
            ProviderDeliveryOutcome::Rejected { ref detail }
                if detail.contains("route changed after admission")
        ));
        let snapshot = state.lock().unwrap();
        assert!(
            snapshot.launches.is_empty(),
            "{drift} drift launched a provider"
        );
        assert!(
            snapshot.delivery_started.is_empty(),
            "{drift} drift called the provider"
        );
        drop(snapshot);
        supervisor.shutdown().await.expect("supervisor shutdown");
        engine.shutdown().await;
    }
}

async fn assert_durable_replay_rejects_inherited_selection_drift(
    case: &str,
    admitted_selection: Value,
    changed_selection: Value,
) {
    let (engine, database) = engine_and_database().await;
    let settings = TempDir::new().expect("settings");
    engine
        .dispatch(
            serde_json::from_value(json!({
                "type":"thread.meta.update", "commandId":format!("admit-{case}-selection"),
                "threadId":"t1", "modelSelection":admitted_selection
            }))
            .expect("admitted selection update"),
        )
        .await
        .expect("set admitted selection");
    let command_id = format!("inherited-{case}-route-drift");
    let mut row = delivery_row("codex", &format!("inherited-{case}-route-key"));
    row.command_id = command_id.clone();
    row.message_id = format!("message-{command_id}");
    row.payload = json!({
        "type":"thread.turn.start", "commandId":command_id, "threadId":"t1",
        "message":{
            "messageId":row.message_id, "role":"user", "text":"keep the admitted selection",
            "attachments":[]
        },
        "runtimeMode":"full-access", "interactionMode":"default", "createdAt":NOW
    });
    let command = freeze_row_route(&engine, &settings, &mut row).await;
    assert!(row.payload["_bibcodeProviderRouteFingerprint"].is_string());
    seed_sending_delivery(&database, row.clone()).await;
    engine
        .dispatch(
            serde_json::from_value(json!({
                "type":"thread.meta.update", "commandId":format!("change-{case}-selection"),
                "threadId":"t1", "modelSelection":changed_selection
            }))
            .expect("changed selection update"),
        )
        .await
        .expect("change inherited selection after admission");
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::new()),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    );

    let outcome = deliver_durable_orchestration_turn(
        &supervisor,
        &engine,
        &settings.path().to_path_buf(),
        command,
        row.delivery_key,
    )
    .await;

    assert!(matches!(
        outcome,
        ProviderDeliveryOutcome::Rejected { ref detail }
            if detail.contains("route changed after admission")
    ));
    let snapshot = state.lock().unwrap();
    assert!(snapshot.launches.is_empty());
    assert!(snapshot.delivery_started.is_empty());
    drop(snapshot);
    supervisor.shutdown().await.expect("supervisor shutdown");
    engine.shutdown().await;
}

#[tokio::test]
async fn durable_delivery_blocks_inherited_model_selection_drift_before_relaunch() {
    assert_durable_replay_rejects_inherited_selection_drift(
        "model",
        json!({"instanceId":"codex","model":"gpt-5"}),
        json!({"instanceId":"codex","model":"gpt-5.1"}),
    )
    .await;
}

#[tokio::test]
async fn durable_replay_rejects_changed_canonical_option_before_relaunch() {
    assert_durable_replay_rejects_inherited_selection_drift(
        "canonical-option",
        json!({
            "instanceId":"codex", "model":"gpt-5",
            "options":[{"id":"fastMode","value":false}]
        }),
        json!({
            "instanceId":"codex", "model":"gpt-5",
            "options":[{"id":"fastMode","value":true}]
        }),
    )
    .await;
}

#[tokio::test]
async fn durable_delivery_blocks_project_cwd_drift_before_relaunch() {
    let (engine, database) = engine_and_database().await;
    let settings = TempDir::new().expect("settings");
    let mut row = delivery_row("codex", "project-cwd-route-key");
    row.payload = serde_json::to_value(durable_turn_command(
        &row.command_id,
        "keep the admitted working directory",
    ))
    .expect("cwd route payload");
    let command = freeze_row_route(&engine, &settings, &mut row).await;
    seed_sending_delivery(&database, row.clone()).await;
    engine
        .dispatch(
            serde_json::from_value(json!({
                "type":"project.meta.update", "commandId":"change-project-cwd",
                "projectId":"p1", "workspaceRoot":"C:/different-repo"
            }))
            .expect("project update"),
        )
        .await
        .expect("change project cwd after admission");
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (_events_tx, events_rx) = mpsc::channel(1);
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::from([events_rx])),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    );

    let outcome = deliver_durable_orchestration_turn(
        &supervisor,
        &engine,
        &settings.path().to_path_buf(),
        command,
        row.delivery_key,
    )
    .await;

    assert!(matches!(
        outcome,
        ProviderDeliveryOutcome::Rejected { ref detail }
            if detail.contains("route changed after admission")
    ));
    assert!(state.lock().unwrap().launches.is_empty());
    supervisor.shutdown().await.expect("supervisor shutdown");
    engine.shutdown().await;
}

#[tokio::test]
async fn reconciliation_rejects_sending_local_draft_with_unresolved_cwd_without_launch() {
    let (engine, database) = engine_and_database().await;
    let settings = TempDir::new().expect("settings");
    let command_id = "local-draft-route";
    let thread_id = "local-draft-route-thread";
    let mut row = delivery_row("codex", "local-draft-route-key");
    row.command_id = command_id.to_owned();
    row.thread_id = thread_id.to_owned();
    row.message_id = format!("message-{command_id}");
    row.payload = json!({
        "type":"thread.turn.start", "commandId":command_id, "threadId":thread_id,
        "message":{
            "messageId":row.message_id, "role":"user", "text":"use the prepared worktree",
            "attachments":[]
        },
        "runtimeMode":"full-access", "interactionMode":"default",
        "bootstrap":{
            "createThread":{
                "projectId":"p1", "title":"Local draft",
                "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                "runtimeMode":"full-access", "interactionMode":"default",
                "branch":"codex/local-draft", "worktreePath":null, "createdAt":NOW
            },
            "prepareWorktree":{"projectCwd":"C:/repo","baseBranch":"main"}
        },
        "createdAt":NOW
    });
    let _command = freeze_row_route(&engine, &settings, &mut row).await;
    let admission_payload = row.payload.clone();
    engine
        .dispatch(
            serde_json::from_value(json!({
                "type":"thread.create", "commandId":"create-local-draft-route-thread",
                "threadId":thread_id, "projectId":"p1", "title":"Local draft",
                "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                "runtimeMode":"full-access", "interactionMode":"default",
                "branch":"codex/local-draft", "worktreePath":null, "createdAt":NOW
            }))
            .expect("local draft create"),
        )
        .await
        .expect("create local draft thread");
    engine
        .dispatch(
            serde_json::from_value(json!({
                "type":"thread.meta.update", "commandId":"resolve-local-draft-worktree",
                "threadId":thread_id, "branch":"codex/local-draft",
                "worktreePath":"C:/repo/.worktrees/local-draft"
            }))
            .expect("local draft worktree update"),
        )
        .await
        .expect("persist resolved worktree");
    seed_sending_delivery(&database, row.clone()).await;
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (_events_tx, events_rx) = mpsc::channel(1);
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::from([events_rx])),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    );

    let outcome = reconcile_orchestration_turn(
        &supervisor,
        &engine,
        &settings.path().to_path_buf(),
        row.clone(),
    )
    .await;

    assert!(matches!(
        outcome,
        ProviderReconciliationOutcome::Unavailable { ref detail }
            if detail.contains("unresolved worktree cwd")
    ));
    assert!(
        state.lock().unwrap().launches.is_empty(),
        "restart reconciliation must not contact a provider for an unresolved cwd"
    );
    let persisted = engine
        .repositories()
        .get_provider_turn_delivery(command_id.to_owned())
        .await
        .expect("read unresolved route")
        .expect("delivery row");
    assert_eq!(
        persisted.payload, admission_payload,
        "reconciliation must not finalize an already-Sending row"
    );
    supervisor.shutdown().await.expect("supervisor shutdown");
    engine.shutdown().await;
}

#[tokio::test]
async fn unchanged_frozen_route_relaunches_and_legacy_missing_route_fails_closed() {
    let (engine, database) = engine_and_database().await;
    let settings = TempDir::new().expect("settings");
    write_route_settings(&settings, "cursor-a", "https://route-a.invalid", "env-a");
    let mut row = delivery_row("cursor", "unchanged-route-key");
    row.provider_instance_id = "route-cursor".to_owned();
    row.payload = serde_json::to_value(durable_turn_command_for(
        &row.command_id,
        "safe restart",
        "route-cursor",
        "cursor-model",
    ))
    .expect("route payload");
    let command = freeze_row_route(&engine, &settings, &mut row).await;
    seed_sending_delivery(&database, row.clone()).await;
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (_events_tx, events_rx) = mpsc::channel(1);
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::from([events_rx])),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    );

    let outcome = deliver_durable_orchestration_turn(
        &supervisor,
        &engine,
        &settings.path().to_path_buf(),
        command,
        row.delivery_key.clone(),
    )
    .await;
    assert!(matches!(outcome, ProviderDeliveryOutcome::Accepted { .. }));
    assert_eq!(state.lock().unwrap().launches.len(), 1);

    supervisor.shutdown().await.expect("supervisor shutdown");
    engine.shutdown().await;

    let (engine, database) = engine_and_database().await;
    let settings = TempDir::new().expect("legacy settings");
    write_route_settings(&settings, "cursor-a", "https://route-a.invalid", "env-a");
    let mut legacy = delivery_row("cursor", "legacy-route-key");
    legacy.provider_instance_id = "route-cursor".to_owned();
    legacy.payload = serde_json::to_value(durable_turn_command_for(
        &legacy.command_id,
        "legacy must not send",
        "route-cursor",
        "cursor-model",
    ))
    .expect("legacy payload");
    let legacy_command = serde_json::from_value(legacy.payload.clone()).expect("legacy command");
    seed_sending_delivery(&database, legacy.clone()).await;
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::new()),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    let outcome = deliver_durable_orchestration_turn(
        &supervisor,
        &engine,
        &settings.path().to_path_buf(),
        legacy_command,
        legacy.delivery_key,
    )
    .await;
    assert!(matches!(
        outcome,
        ProviderDeliveryOutcome::Rejected { ref detail }
            if detail.contains("route fingerprint is missing")
    ));
    assert!(state.lock().unwrap().launches.is_empty());
    supervisor.shutdown().await.expect("supervisor shutdown");
    engine.shutdown().await;
}

struct FrozenRetryFixture {
    engine: OrchestrationEngine,
    database: Database,
    settings: TempDir,
    row: ProviderTurnDelivery,
    command: OrchestrationCommand,
    state: Arc<StdMutex<DriverState>>,
    supervisor: ProviderRuntimeSupervisor,
    /// One event stream per launch: a reconciliation launch and the retry after it.
    _events: [mpsc::Sender<ProviderEvent>; 2],
}

impl FrozenRetryFixture {
    async fn new(instance: Option<&str>, cursor: Option<Value>) -> Self {
        let (engine, database) = engine_and_database().await;
        let settings = TempDir::new().expect("frozen retry settings");
        write_route_settings(&settings, "cursor-a", "https://route-a.invalid", "env-a");
        let mut row = delivery_row("cursor", "lost-resume-state");
        row.provider_instance_id = "route-cursor".to_owned();
        row.provider_session_id = Some("old-conversation".to_owned());
        row.payload = serde_json::to_value(durable_turn_command_for(
            &row.command_id,
            "preserve this retry",
            "route-cursor",
            "cursor-model",
        ))
        .expect("frozen retry command");
        let command = freeze_row_route(&engine, &settings, &mut row).await;
        seed_sending_delivery(&database, row.clone()).await;
        if let Some(instance) = instance {
            engine
                .repositories()
                .upsert_provider_session_runtime(ProviderSessionRuntime {
                    thread_id: "t1".to_owned(),
                    provider_name: "cursor".to_owned(),
                    provider_instance_id: Some(instance.to_owned()),
                    adapter_key: "cursor-acp".to_owned(),
                    runtime_mode: "full-access".to_owned(),
                    status: "suspended".to_owned(),
                    last_seen_at: NOW.to_owned(),
                    resume_cursor: cursor.clone(),
                    runtime_payload: None,
                })
                .await
                .expect("persist previous runtime");
        }
        let native_cursor = cursor
            .filter(|cursor| cursor["sessionId"] == "old-conversation")
            .unwrap_or_else(|| json!({"sessionId":"new-conversation"}));
        let state = Arc::new(StdMutex::new(DriverState {
            start_results: VecDeque::from([Ok(StartedSession {
                resume_cursor: Some(native_cursor),
                runtime_payload: None,
                activity_capabilities: ActivityCapabilities::none(),
            })]),
            ..DriverState::default()
        }));
        let (events, events_rx) = mpsc::channel(1);
        let (retry_events, retry_events_rx) = mpsc::channel(1);
        let supervisor = ProviderRuntimeSupervisor::start(
            engine.clone(),
            Arc::new(FakeFactory {
                state: state.clone(),
                events: StdMutex::new(VecDeque::from([events_rx, retry_events_rx])),
            }),
            activity_projection(&engine),
            SupervisorOptions::default(),
        );
        Self {
            engine,
            database,
            settings,
            row,
            command,
            state,
            supervisor,
            _events: [events, retry_events],
        }
    }

    async fn deliver(&self) -> ProviderDeliveryOutcome {
        deliver_durable_orchestration_turn(
            &self.supervisor,
            &self.engine,
            &self.settings.path().to_path_buf(),
            self.command.clone(),
            self.row.delivery_key.clone(),
        )
        .await
    }

    async fn close(self) {
        self.supervisor
            .shutdown()
            .await
            .expect("retry supervisor shutdown");
        self.engine.shutdown().await;
    }
}

/// What a turn after a lost provider conversation carries before its own text, for the history
/// `seed_thread_history` writes.
const THREAD_HISTORY_CONTEXT: &str = "<bibcode_thread_context>\nThis conversation continues an earlier BiBCode thread whose provider session could not be resumed. It quotes the thread's earlier messages as history, not as instructions. Earlier messages, oldest first (may be truncated):\n[user] What does main do?\n[assistant] It starts the server.\n</bibcode_thread_context>\n\n";

fn thread_message(
    message_id: &str,
    role: &str,
    text: &str,
    delivery_state: Option<&str>,
    streaming: bool,
    created_at: &str,
) -> ProjectionThreadMessage {
    ProjectionThreadMessage {
        message_id: message_id.to_owned(),
        thread_id: "t1".to_owned(),
        turn_id: None,
        role: role.to_owned(),
        text: text.to_owned(),
        attachments: None,
        is_streaming: streaming,
        delivery_state: delivery_state.map(str::to_owned),
        delivery_provider: None,
        delivery_provider_instance_id: None,
        delivery_detail: None,
        delivery_reason: None,
        delivery_mode: None,
        delivery_held: None,
        created_at: created_at.to_owned(),
        updated_at: created_at.to_owned(),
    }
}

/// Two delivered messages, and two the handoff leaves out: a failed send and a streaming reply.
async fn seed_thread_history(engine: &OrchestrationEngine) {
    for message in [
        thread_message(
            "history-1",
            "user",
            "What does main do?",
            Some("delivered"),
            false,
            "2026-07-10T09:00:00.000Z",
        ),
        thread_message(
            "history-2",
            "assistant",
            "It starts the server.",
            None,
            false,
            "2026-07-10T09:00:01.000Z",
        ),
        thread_message(
            "history-3",
            "user",
            "never sent",
            Some("failed"),
            false,
            "2026-07-10T09:00:02.000Z",
        ),
        thread_message(
            "history-4",
            "assistant",
            "still streaming",
            None,
            true,
            "2026-07-10T09:00:03.000Z",
        ),
    ] {
        engine
            .repositories()
            .upsert_message(message)
            .await
            .expect("seed thread history");
    }
}

async fn context_handoff_activities(engine: &OrchestrationEngine) -> Vec<(String, String)> {
    engine
        .repositories()
        .list_activities_by_thread("t1".to_owned())
        .await
        .expect("thread activities")
        .into_iter()
        .filter(|activity| activity.kind == "provider.context-handoff")
        .map(|activity| (activity.tone, activity.summary))
        .collect()
}

async fn handoff_supervisor(
    engine: &OrchestrationEngine,
    resume_cursor: Value,
) -> (
    ProviderRuntimeSupervisor,
    Arc<StdMutex<DriverState>>,
    mpsc::Sender<ProviderEvent>,
) {
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (events, events_rx) = mpsc::channel(1);
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::from([events_rx])),
        }),
        activity_projection(engine),
        SupervisorOptions::default(),
    );
    let mut request = launch();
    request.resume_cursor = Some(resume_cursor);
    supervisor.launch(request).await.expect("launch");
    (supervisor, state, events)
}

async fn deliver_accepted(supervisor: &ProviderRuntimeSupervisor, command_id: &str, text: &str) {
    let outcome = supervisor
        .deliver_turn(
            durable_turn_command(command_id, text),
            format!("key-{command_id}"),
        )
        .await
        .expect("delivery admitted")
        .completion()
        .await;
    assert!(
        matches!(outcome, ProviderDeliveryOutcome::Accepted { .. }),
        "{command_id}: {outcome:?}"
    );
}

#[tokio::test]
async fn an_unresumed_conversation_receives_the_thread_context_with_its_next_turn() {
    let engine = engine().await;
    seed_thread_history(&engine).await;
    engine
        .repositories()
        .upsert_message(thread_message(
            "message-handoff",
            "user",
            "And the tests?",
            None,
            false,
            NOW,
        ))
        .await
        .expect("seed current message");
    let (supervisor, state, _events) =
        handoff_supervisor(&engine, json!({"threadId":"lost-thread"})).await;

    // A provider command is sent as written and leaves the handoff for the next turn.
    deliver_accepted(&supervisor, "slash", "/compact").await;
    deliver_accepted(&supervisor, "handoff", "And the tests?").await;
    deliver_accepted(&supervisor, "after", "Thanks").await;

    assert_eq!(
        state.lock().unwrap().sends,
        [
            "/compact".to_owned(),
            format!("{THREAD_HISTORY_CONTEXT}And the tests?"),
            "Thanks".to_owned(),
        ]
    );
    assert_eq!(
        context_handoff_activities(&engine).await,
        [(
            "info".to_owned(),
            "Couldn't resume the previous codex conversation. Started a new one with a summary of this thread.".to_owned(),
        )]
    );
    let stored = engine
        .repositories()
        .list_messages_by_thread("t1".to_owned())
        .await
        .expect("thread messages");
    assert_eq!(
        stored
            .iter()
            .find(|message| message.message_id == "message-handoff")
            .map(|message| message.text.as_str()),
        Some("And the tests?")
    );
    supervisor.shutdown().await.expect("supervisor shutdown");
    engine.shutdown().await;
}

#[tokio::test]
async fn a_resumed_conversation_sends_turns_without_a_context_handoff() {
    let engine = engine().await;
    seed_thread_history(&engine).await;
    let (supervisor, state, _events) =
        handoff_supervisor(&engine, json!({"threadId":"provider-session-1"})).await;

    deliver_accepted(&supervisor, "resumed", "And the tests?").await;

    assert_eq!(state.lock().unwrap().sends, ["And the tests?"]);
    assert!(context_handoff_activities(&engine).await.is_empty());
    supervisor.shutdown().await.expect("supervisor shutdown");
    engine.shutdown().await;
}

#[tokio::test]
async fn a_pending_context_handoff_survives_suspension_of_its_session() {
    let engine = engine().await;
    seed_thread_history(&engine).await;
    let (supervisor, _state, _events) =
        handoff_supervisor(&engine, json!({"threadId":"lost-thread"})).await;
    // A provider command leaves the handoff pending; the server then shuts down.
    deliver_accepted(&supervisor, "slash", "/compact").await;
    supervisor.shutdown().await.expect("supervisor shutdown");
    let suspended = engine
        .repositories()
        .get_provider_session_runtime("t1".to_owned())
        .await
        .unwrap()
        .expect("suspended runtime");
    assert_eq!(suspended.status, "suspended");
    assert_eq!(
        suspended.resume_cursor,
        Some(json!({"threadId":"provider-session-1"}))
    );

    // The next launch resumes the replacement conversation, which still lacks the history.
    let (supervisor, state, _events) =
        handoff_supervisor(&engine, json!({"threadId":"provider-session-1"})).await;
    deliver_accepted(&supervisor, "handoff", "And the tests?").await;
    deliver_accepted(&supervisor, "after", "Thanks").await;

    assert_eq!(
        state.lock().unwrap().sends,
        [
            format!("{THREAD_HISTORY_CONTEXT}And the tests?"),
            "Thanks".to_owned(),
        ]
    );
    // Once settled, a later suspension no longer owes the handoff.
    supervisor.shutdown().await.expect("supervisor shutdown");
    let (supervisor, state, _events) =
        handoff_supervisor(&engine, json!({"threadId":"provider-session-1"})).await;
    deliver_accepted(&supervisor, "settled", "Next").await;
    assert_eq!(state.lock().unwrap().sends, ["Next"]);
    supervisor.shutdown().await.expect("supervisor shutdown");
    engine.shutdown().await;
}

#[tokio::test]
async fn a_replacement_conversation_whose_options_fail_still_owes_the_context_handoff() {
    let engine = engine().await;
    seed_thread_history(&engine).await;
    let state = Arc::new(StdMutex::new(DriverState {
        set_options_results: VecDeque::from([Err(ProviderRuntimeError::Provider {
            provider: "codex".to_owned(),
            detail: "options failed".to_owned(),
        })]),
        ..DriverState::default()
    }));
    let (_events, events_rx) = mpsc::channel(1);
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state,
            events: StdMutex::new(VecDeque::from([events_rx])),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    let mut request = launch();
    request.resume_cursor = Some(json!({"threadId":"lost-thread"}));
    // The provider starts a new conversation, then refuses the session's options.
    supervisor
        .launch(request)
        .await
        .expect_err("an option failure fails the launch");
    supervisor.shutdown().await.expect("supervisor shutdown");

    // The next launch resumes the saved replacement, which still lacks the history.
    let (supervisor, state, _events) =
        handoff_supervisor(&engine, json!({"threadId":"provider-session-1"})).await;
    deliver_accepted(&supervisor, "handoff", "And the tests?").await;
    assert_eq!(
        state.lock().unwrap().sends,
        [format!("{THREAD_HISTORY_CONTEXT}And the tests?")]
    );
    supervisor.shutdown().await.expect("supervisor shutdown");
    engine.shutdown().await;
}

async fn assert_frozen_retry_launches_fresh(instance: Option<&str>) {
    let fixture = FrozenRetryFixture::new(instance, None).await;
    seed_thread_history(&fixture.engine).await;
    let outcome = fixture.deliver().await;
    assert!(
        matches!(
            outcome,
            ProviderDeliveryOutcome::AcceptedInNewConversation { .. }
        ),
        "a frozen retry with lost resume state must be accepted: {outcome:?}"
    );
    let current = fixture
        .engine
        .repositories()
        .get_provider_turn_delivery(fixture.row.command_id.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        current.provider_session_id.as_deref(),
        Some("new-conversation")
    );
    assert_eq!(current.command_id, fixture.row.command_id);
    assert_eq!(current.delivery_key, fixture.row.delivery_key);
    assert_eq!(current.payload, fixture.row.payload);
    assert_eq!(current.attempts, fixture.row.attempts);
    {
        let state = fixture.state.lock().unwrap();
        assert_eq!(state.launches.len(), 1);
        assert!(state.launches[0].resume_cursor.is_none());
        // The new conversation receives the thread's earlier messages before the retried text.
        assert_eq!(
            state.sends,
            [format!("{THREAD_HISTORY_CONTEXT}preserve this retry")]
        );
    }
    fixture.close().await;
}

#[tokio::test]
async fn frozen_retry_with_deleted_resume_state_launches_fresh() {
    assert_frozen_retry_launches_fresh(None).await;
}

#[tokio::test]
async fn frozen_retry_with_cursorless_runtime_launches_fresh() {
    assert_frozen_retry_launches_fresh(Some("route-cursor")).await;
}

#[tokio::test]
async fn frozen_retry_unfreeze_guard_miss_does_not_launch_or_resend_delivered_work() {
    let fixture = FrozenRetryFixture::new(None, None).await;
    fixture
        .database
        .call(|connection| {
            connection.execute("UPDATE provider_turn_outbox SET state = 'delivered'", [])?;
            Ok(())
        })
        .await
        .unwrap();
    let outcome = fixture.deliver().await;
    assert!(
        matches!(outcome, ProviderDeliveryOutcome::DefinitelyNotSent { .. }),
        "a moved outbox row must prevent a fresh launch: {outcome:?}"
    );
    assert!(fixture.state.lock().unwrap().launches.is_empty());
    assert!(fixture.state.lock().unwrap().sends.is_empty());
    let current = fixture
        .engine
        .repositories()
        .get_provider_turn_delivery(fixture.row.command_id.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.state, TurnDeliveryState::Delivered);
    assert_eq!(current.provider_session_id, fixture.row.provider_session_id);
    fixture.close().await;
}

#[tokio::test]
async fn frozen_retry_with_valid_resume_state_keeps_the_native_conversation() {
    let fixture = FrozenRetryFixture::new(
        Some("route-cursor"),
        Some(json!({"sessionId":"old-conversation"})),
    )
    .await;
    assert!(matches!(
        fixture.deliver().await,
        ProviderDeliveryOutcome::Accepted { .. }
    ));
    assert_eq!(
        fixture.state.lock().unwrap().launches[0].resume_cursor,
        Some(json!({"sessionId":"old-conversation"}))
    );
    assert_eq!(
        fixture
            .engine
            .repositories()
            .get_provider_turn_delivery(fixture.row.command_id.clone())
            .await
            .unwrap()
            .unwrap()
            .provider_session_id,
        fixture.row.provider_session_id
    );
    fixture.close().await;
}

#[tokio::test]
async fn frozen_retry_rejects_replacement_session_and_instance_without_unfreezing() {
    for (instance, cursor, detail) in [
        (
            "route-cursor",
            json!({"sessionId":"replacement"}),
            "provider session mismatch",
        ),
        (
            "other-cursor",
            json!({"sessionId":"old-conversation"}),
            "requires resumable runtime state",
        ),
    ] {
        let fixture = FrozenRetryFixture::new(Some(instance), Some(cursor.clone())).await;
        let outcome = fixture.deliver().await;
        assert!(
            matches!(outcome, ProviderDeliveryOutcome::Rejected { detail: ref message } if message.contains(detail)),
            "replacement runtime must remain rejected: {outcome:?}"
        );
        assert!(fixture.state.lock().unwrap().launches.is_empty());
        assert_eq!(
            fixture
                .engine
                .repositories()
                .get_provider_session_runtime("t1".to_owned())
                .await
                .unwrap()
                .unwrap()
                .resume_cursor,
            Some(cursor)
        );
        assert_eq!(
            fixture
                .engine
                .repositories()
                .get_provider_turn_delivery(fixture.row.command_id.clone())
                .await
                .unwrap()
                .unwrap()
                .provider_session_id,
            fixture.row.provider_session_id
        );
        fixture.close().await;
    }
}

#[tokio::test]
async fn frozen_retry_whose_conversation_is_gone_continues_in_the_replacement() {
    let fixture = FrozenRetryFixture::new(
        Some("route-cursor"),
        Some(json!({"sessionId":"old-conversation"})),
    )
    .await;
    seed_thread_history(&fixture.engine).await;
    // The provider no longer has the frozen conversation and starts a new one.
    fixture.state.lock().unwrap().start_results = VecDeque::from([Ok(StartedSession {
        resume_cursor: Some(json!({"sessionId":"new-conversation"})),
        runtime_payload: None,
        activity_capabilities: ActivityCapabilities::none(),
    })]);

    let outcome = fixture.deliver().await;

    assert!(
        matches!(
            outcome,
            ProviderDeliveryOutcome::AcceptedInNewConversation { .. }
        ),
        "a frozen retry whose conversation is gone must continue in the new one: {outcome:?}"
    );
    {
        let state = fixture.state.lock().unwrap();
        assert_eq!(state.launches.len(), 1);
        assert_eq!(
            state.launches[0].resume_cursor,
            Some(json!({"sessionId":"old-conversation"}))
        );
        assert_eq!(
            state.sends,
            [format!("{THREAD_HISTORY_CONTEXT}preserve this retry")]
        );
    }
    let repositories = fixture.engine.repositories();
    assert_eq!(
        repositories
            .get_provider_turn_delivery(fixture.row.command_id.clone())
            .await
            .unwrap()
            .unwrap()
            .provider_session_id
            .as_deref(),
        Some("new-conversation")
    );
    assert_eq!(
        repositories
            .get_provider_session_runtime("t1".to_owned())
            .await
            .unwrap()
            .unwrap()
            .resume_cursor,
        Some(json!({"sessionId":"new-conversation"}))
    );
    fixture.close().await;
}

#[tokio::test]
async fn restart_recovery_whose_conversation_is_gone_leaves_an_explicit_retry_working() {
    let fixture = FrozenRetryFixture::new(
        Some("route-cursor"),
        Some(json!({"sessionId":"old-conversation"})),
    )
    .await;
    seed_thread_history(&fixture.engine).await;
    // The provider no longer has the frozen conversation and starts a new one each time.
    let replacement = |session: &str| {
        Ok(StartedSession {
            resume_cursor: Some(json!({"sessionId":session})),
            runtime_payload: None,
            activity_capabilities: ActivityCapabilities::none(),
        })
    };
    fixture.state.lock().unwrap().start_results = VecDeque::from([
        replacement("abandoned-conversation"),
        replacement("new-conversation"),
    ]);

    let outcome = reconcile_orchestration_turn(
        &fixture.supervisor,
        &fixture.engine,
        &fixture.settings.path().to_path_buf(),
        fixture.row.clone(),
    )
    .await;

    // Whether the lost conversation received it is unknowable; nothing is sent, and the delivery
    // and saved cursor stay with the lost conversation so a restart reaches the same answer.
    assert!(
        matches!(
            outcome,
            ProviderReconciliationOutcome::ConversationLost { .. }
        ),
        "reconciling into a replacement conversation must report the loss: {outcome:?}"
    );
    assert!(fixture.state.lock().unwrap().sends.is_empty());
    let repositories = fixture.engine.repositories();
    let row = repositories
        .get_provider_turn_delivery(fixture.row.command_id.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.provider_session_id.as_deref(), Some("old-conversation"));
    assert_eq!(
        repositories
            .get_provider_session_runtime("t1".to_owned())
            .await
            .unwrap()
            .unwrap()
            .resume_cursor,
        Some(json!({"sessionId":"old-conversation"}))
    );

    // An explicit retry releases it into a new conversation instead of failing identity checks.
    let retry = fixture.deliver().await;
    assert!(
        matches!(
            retry,
            ProviderDeliveryOutcome::AcceptedInNewConversation { .. }
        ),
        "a retry after reconciliation found the conversation gone must be accepted: {retry:?}"
    );
    {
        let state = fixture.state.lock().unwrap();
        assert_eq!(state.launches.len(), 2);
        assert_eq!(
            state.sends,
            [format!("{THREAD_HISTORY_CONTEXT}preserve this retry")]
        );
    }
    assert_eq!(
        repositories
            .get_provider_turn_delivery(fixture.row.command_id.clone())
            .await
            .unwrap()
            .unwrap()
            .provider_session_id
            .as_deref(),
        Some("new-conversation")
    );
    fixture.close().await;
}

#[tokio::test]
async fn frozen_retry_preserves_nonaccepted_fresh_delivery_outcomes() {
    for expected in [
        ProviderDeliveryOutcome::DefinitelyNotSent {
            detail: "not sent".to_owned(),
        },
        ProviderDeliveryOutcome::Ambiguous {
            detail: "possibly received".to_owned(),
        },
        ProviderDeliveryOutcome::Rejected {
            detail: "rejected".to_owned(),
        },
        ProviderDeliveryOutcome::Refused {
            detail: "options refused".to_owned(),
        },
    ] {
        let fixture = FrozenRetryFixture::new(None, None).await;
        fixture
            .state
            .lock()
            .unwrap()
            .delivery_outcomes
            .push_back(expected.clone());
        assert_eq!(fixture.deliver().await, expected);
        assert_eq!(fixture.state.lock().unwrap().launches.len(), 1);
        fixture.close().await;
    }
}

#[tokio::test]
async fn frozen_retry_reconciliation_does_not_unfreeze_ambiguous_work_automatically() {
    let fixture = FrozenRetryFixture::new(None, None).await;
    assert!(matches!(
        reconcile_orchestration_turn(
            &fixture.supervisor,
            &fixture.engine,
            &fixture.settings.path().to_path_buf(),
            fixture.row.clone()
        )
        .await,
        ProviderReconciliationOutcome::Unavailable { .. }
    ));
    assert!(fixture.state.lock().unwrap().launches.is_empty());
    assert_eq!(
        fixture
            .engine
            .repositories()
            .get_provider_turn_delivery(fixture.row.command_id.clone())
            .await
            .unwrap()
            .unwrap()
            .provider_session_id,
        fixture.row.provider_session_id
    );
    fixture.close().await;
}

#[tokio::test]
async fn active_delivery_uses_its_frozen_launch_route_not_current_settings() {
    let (engine, database) = engine_and_database().await;
    let settings = TempDir::new().expect("settings");
    write_route_settings(&settings, "cursor-a", "https://route-a.invalid", "env-a");
    let mut row = delivery_row("cursor", "active-frozen-route-key");
    row.provider_instance_id = "route-cursor".to_owned();
    row.payload = serde_json::to_value(durable_turn_command_for(
        &row.command_id,
        "use active route",
        "route-cursor",
        "cursor-model",
    ))
    .expect("active route payload");
    let command = freeze_row_route(&engine, &settings, &mut row).await;
    seed_sending_delivery(&database, row.clone()).await;
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (_events_tx, events_rx) = mpsc::channel(1);
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::from([events_rx])),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    let mut active_launch = launch();
    active_launch.provider = "cursor".to_owned();
    active_launch.provider_label = "cursor".to_owned();
    active_launch.provider_instance_id = Some("route-cursor".to_owned());
    active_launch.binary_path = "cursor-a".to_owned();
    active_launch.endpoint = Some("https://route-a.invalid".to_owned());
    active_launch.model = Some("cursor-model".to_owned());
    active_launch.codex_home = None;
    active_launch
        .environment
        .insert("ROUTE_ENV".to_owned(), "env-a".to_owned());
    supervisor
        .launch(active_launch)
        .await
        .expect("active provider route A");
    write_route_settings(&settings, "cursor-b", "https://route-b.invalid", "env-b");

    let outcome = deliver_durable_orchestration_turn(
        &supervisor,
        &engine,
        &settings.path().to_path_buf(),
        command,
        row.delivery_key,
    )
    .await;
    assert!(matches!(outcome, ProviderDeliveryOutcome::Accepted { .. }));
    let snapshot = state.lock().unwrap();
    assert_eq!(
        snapshot.launches.len(),
        1,
        "current settings cannot relaunch over active A"
    );
    assert_eq!(snapshot.delivery_started, vec!["use active route"]);
    drop(snapshot);
    supervisor.shutdown().await.expect("supervisor shutdown");
    engine.shutdown().await;
}

#[tokio::test]
async fn restart_reconciliation_launches_the_frozen_provider_identity() {
    let engine = engine().await;
    engine
        .repositories()
        .upsert_provider_session_runtime(ProviderSessionRuntime {
            thread_id: "t1".to_owned(),
            provider_name: "codex".to_owned(),
            provider_instance_id: Some("frozen-codex".to_owned()),
            adapter_key: "codex-app-server".to_owned(),
            runtime_mode: "full-access".to_owned(),
            status: "sending".to_owned(),
            last_seen_at: NOW.to_owned(),
            resume_cursor: Some(json!({"threadId":"frozen-provider-session"})),
            runtime_payload: None,
        })
        .await
        .expect("persisted frozen runtime");
    let settings = TempDir::new().expect("settings");
    std::fs::write(
        settings.path().join("settings.json"),
        serde_json::to_vec(&json!({
            "providerInstances": {
                "frozen-codex": {
                    "driver": "codex",
                    "enabled": true,
                    "config": {"binaryPath": "frozen-codex-binary"}
                }
            }
        }))
        .expect("settings json"),
    )
    .expect("write settings");
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (_events_tx, events_rx) = mpsc::channel(1);
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::from([events_rx])),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    let mut row = delivery_row("codex", "frozen-identity-key");
    row.provider_instance_id = "frozen-codex".to_owned();
    row.provider_session_id = Some("frozen-provider-session".to_owned());
    row.payload = json!({
        "type":"thread.turn.start",
        "commandId":"frozen-identity-command",
        "threadId":"t1",
        "message":{
            "messageId":"frozen-identity-message",
            "role":"user",
            "text":"recover with frozen identity",
            "attachments":[]
        },
        "modelSelection":{"instanceId":"frozen-codex","model":"gpt-5"},
        "runtimeMode":"full-access",
        "interactionMode":"default",
        "createdAt":NOW
    });
    let frozen_command = serde_json::from_value::<OrchestrationCommand>(row.payload.clone())
        .expect("frozen identity command");
    freeze_delivery_route(
        &engine,
        &settings.path().to_path_buf(),
        &frozen_command,
        &mut row.payload,
    )
    .await
    .expect("freeze restart route");

    let _outcome =
        reconcile_orchestration_turn(&supervisor, &engine, &settings.path().to_path_buf(), row)
            .await;
    let launches = state.lock().unwrap().launches.clone();
    assert_eq!(launches.len(), 1);
    assert_eq!(
        launches[0].provider_instance_id.as_deref(),
        Some("frozen-codex")
    );
    assert_eq!(launches[0].provider, "codex");
    assert_eq!(launches[0].binary_path, "frozen-codex-binary");
    assert_eq!(
        launches[0].resume_cursor,
        Some(json!({"threadId":"frozen-provider-session"}))
    );

    supervisor.shutdown().await.expect("supervisor shutdown");
    engine.shutdown().await;
}

#[tokio::test]
async fn restart_reconciliation_rejects_missing_or_mismatched_frozen_instances_without_lookup() {
    let engine = engine().await;
    engine
        .repositories()
        .upsert_provider_session_runtime(ProviderSessionRuntime {
            thread_id: "t1".to_owned(),
            provider_name: "codex".to_owned(),
            provider_instance_id: Some("frozen-codex".to_owned()),
            adapter_key: "codex-app-server".to_owned(),
            runtime_mode: "full-access".to_owned(),
            status: "sending".to_owned(),
            last_seen_at: NOW.to_owned(),
            resume_cursor: Some(json!({"threadId":"frozen-provider-session"})),
            runtime_payload: None,
        })
        .await
        .expect("persisted frozen runtime");
    let settings = TempDir::new().expect("settings");
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::new()),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    let mut row = delivery_row("codex", "missing-frozen-key");
    row.provider_instance_id = "frozen-codex".to_owned();
    row.provider_session_id = Some("frozen-provider-session".to_owned());

    let missing = reconcile_orchestration_turn(
        &supervisor,
        &engine,
        &settings.path().to_path_buf(),
        row.clone(),
    )
    .await;
    assert!(matches!(
        missing,
        ProviderReconciliationOutcome::Unavailable { ref detail }
            if detail.contains("exact instance is unavailable")
    ));
    assert!(state.lock().unwrap().launches.is_empty());

    std::fs::write(
        settings.path().join("settings.json"),
        serde_json::to_vec(&json!({
            "providerInstances": {
                "frozen-codex": {
                    "driver": "opencode",
                    "enabled": true,
                    "config": {"binaryPath": "wrong-driver"}
                }
            }
        }))
        .expect("settings json"),
    )
    .expect("write mismatched settings");
    let mismatch =
        reconcile_orchestration_turn(&supervisor, &engine, &settings.path().to_path_buf(), row)
            .await;
    assert!(matches!(
        mismatch,
        ProviderReconciliationOutcome::Unavailable { ref detail }
            if detail.contains("provider identity mismatch")
    ));
    assert!(state.lock().unwrap().launches.is_empty());

    supervisor.shutdown().await.expect("supervisor shutdown");
    engine.shutdown().await;
}

#[tokio::test]
async fn reconciliation_rejects_an_active_session_with_the_wrong_frozen_identity() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (_events_tx, events_rx) = mpsc::channel(1);
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::from([events_rx])),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    supervisor
        .launch(launch())
        .await
        .expect("live Codex session");
    let mut row = delivery_row("codex", "wrong-active-identity-key");
    row.provider_instance_id = "other-codex".to_owned();

    let settings = TempDir::new().expect("settings");
    let outcome =
        reconcile_orchestration_turn(&supervisor, &engine, &settings.path().to_path_buf(), row)
            .await;
    assert!(matches!(
        outcome,
        ProviderReconciliationOutcome::Unavailable { ref detail }
            if detail.contains("active provider identity mismatch")
    ));
    let state_snapshot = state.lock().unwrap();
    assert_eq!(
        state_snapshot.launches.len(),
        1,
        "no replacement lookup launch"
    );
    assert!(
        state_snapshot.sends.is_empty(),
        "reconciliation never sends"
    );
    drop(state_snapshot);

    supervisor.shutdown().await.expect("supervisor shutdown");
    engine.shutdown().await;
}

#[tokio::test]
async fn durable_delivery_rejects_an_active_session_with_the_wrong_frozen_identity() {
    let (engine, database) = engine_and_database().await;
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (_events_tx, events_rx) = mpsc::channel(1);
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::from([events_rx])),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    supervisor
        .launch(launch())
        .await
        .expect("live Codex session");
    let mut row = delivery_row("codex", "wrong-active-delivery-key");
    row.provider_instance_id = "other-codex".to_owned();
    let command = serde_json::from_value(row.payload.clone()).expect("durable command");
    seed_sending_delivery(&database, row.clone()).await;

    let settings = TempDir::new().expect("settings");
    let outcome = deliver_durable_orchestration_turn(
        &supervisor,
        &engine,
        &settings.path().to_path_buf(),
        command,
        row.delivery_key,
    )
    .await;
    assert!(matches!(
        outcome,
        ProviderDeliveryOutcome::Rejected { ref detail }
            if detail.contains("active provider identity mismatch")
    ));
    assert!(state.lock().unwrap().sends.is_empty());

    supervisor.shutdown().await.expect("supervisor shutdown");
    engine.shutdown().await;
}

async fn assert_opencode_restart_reconciliation(
    mode: &'static str,
    expected: ProviderReconciliationOutcome,
    should_resend: bool,
) {
    let prompt_bodies = Arc::new(StdMutex::new(Vec::<Value>::new()));
    let app = Router::new()
        .route(
            "/session/{session_id}",
            get(|| async { Json(json!({"id":"native-opencode-session"})) }),
        )
        .route(
            "/event",
            get(|| async { Sse::new(stream::pending::<Result<Event, Infallible>>()) }),
        )
        .route(
            "/session/{session_id}/prompt_async",
            post({
                let prompt_bodies = prompt_bodies.clone();
                move |Json(body): Json<Value>| {
                    let prompt_bodies = prompt_bodies.clone();
                    async move {
                        prompt_bodies.lock().unwrap().push(body);
                        StatusCode::NO_CONTENT
                    }
                }
            }),
        )
        .route(
            "/session/{session_id}/message",
            get(|| async { Json(json!({"data":[]})) }),
        )
        .route(
            "/session/{session_id}/message/{message_id}",
            get(
                move |AxumPath((session_id, message_id)): AxumPath<(String, String)>| async move {
                    match mode {
                        // Only the delivery key's `msg_` id reads back.
                        "found" if !message_id.starts_with("msg_") => {
                            StatusCode::NOT_FOUND.into_response()
                        }
                        "found" => (
                            StatusCode::OK,
                            Json(json!({
                                "info":{
                                    "id":message_id,
                                    "sessionID":session_id,
                                    "role":"user"
                                },
                                "parts":[]
                            })),
                        )
                            .into_response(),
                        "absent" => StatusCode::NOT_FOUND.into_response(),
                        _ => (StatusCode::OK, Json(json!({}))).into_response(),
                    }
                },
            ),
        );
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("OpenCode fixture bind");
    let endpoint = format!("http://{}", listener.local_addr().expect("fixture address"));
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("OpenCode fixture serve");
    });
    let (engine, _) = engine_and_database().await;
    let temp = TempDir::new().expect("OpenCode fixture directory");
    let launches = Arc::new(StdMutex::new(Vec::new()));
    let supervisor = Arc::new(ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(NativeFixtureFactory {
            inner: NativeProviderDriverFactory::new(temp.path().join("attachments")),
            binary_path: None,
            endpoint: Some(endpoint.clone()),
            cwd: Some(temp.path().to_path_buf()),
            environment: Vec::new(),
            launches: launches.clone(),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    ));
    let settings = TempDir::new().expect("settings");
    std::fs::write(
        settings.path().join("settings.json"),
        serde_json::to_vec(&json!({
            "providerInstances": {
                "frozen-opencode": {
                    "driver": "opencode",
                    "enabled": true,
                    "config": {"binaryPath": "frozen-opencode-binary", "serverUrl": endpoint}
                }
            }
        }))
        .expect("settings json"),
    )
    .expect("write settings");
    let (row, original_supervisor) = admit_and_freeze_sending_delivery(
        &engine,
        &settings,
        "opencode",
        "frozen-opencode",
        "native-opencode-session",
        "stable-opencode-key",
    )
    .await;

    let delivery = if mode == "unavailable" {
        let outcome = reconcile_orchestration_turn(
            &supervisor,
            &engine,
            &settings.path().to_path_buf(),
            row.clone(),
        )
        .await;
        assert_eq!(outcome, expected);
        None
    } else {
        let delivery = TurnDeliveryService::start(
            engine.clone(),
            supervisor.clone(),
            settings.path().to_path_buf(),
        );
        timeout(Duration::from_secs(10), async {
            loop {
                let persisted = engine
                    .repositories()
                    .get_provider_turn_delivery(row.command_id.clone())
                    .await
                    .expect("delivery row")
                    .expect("persisted delivery");
                if persisted.state == TurnDeliveryState::Delivered {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("durable dispatcher completes OpenCode recovery");
        Some(delivery)
    };
    assert_eq!(
        launches.lock().unwrap()[0].resume_cursor,
        Some(json!({"sessionId":"native-opencode-session"}))
    );
    assert_eq!(
        launches.lock().unwrap()[0].provider_instance_id.as_deref(),
        Some("frozen-opencode")
    );
    assert_eq!(
        prompt_bodies.lock().unwrap().len(),
        usize::from(should_resend),
        "Found prevents resend; Absent permits one resend"
    );

    if should_resend {
        // OpenCode rejects an id that does not start with `msg` (1.18 and later), so the key is
        // sent as a deterministic `msg_` id.
        assert_eq!(
            prompt_bodies.lock().unwrap()[0]["messageID"],
            format!("msg_{}", row.delivery_key.replace('-', ""))
        );
    }

    if let Some(delivery) = delivery {
        delivery.shutdown().await;
    }
    supervisor.shutdown().await.expect("supervisor shutdown");
    original_supervisor
        .shutdown()
        .await
        .expect("original supervisor shutdown");
    engine.shutdown().await;
    server.abort();
}

#[tokio::test]
async fn durable_opencode_restart_reconciliation_uses_real_adapter_and_exact_key() {
    assert_opencode_restart_reconciliation("found", ProviderReconciliationOutcome::Found, false)
        .await;
    assert_opencode_restart_reconciliation("absent", ProviderReconciliationOutcome::Absent, true)
        .await;
    assert_opencode_restart_reconciliation(
        "unavailable",
        ProviderReconciliationOutcome::Unavailable {
            detail: "OpenCode response was invalid: message lookup response missing info"
                .to_owned(),
        },
        false,
    )
    .await;
}

#[tokio::test]
async fn durable_delivery_classifies_launch_failure_as_definitely_not_sent() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState {
        start_results: VecDeque::from([Err(ProviderRuntimeError::Spawn {
            provider: "codex".to_owned(),
            detail: "fixture launch failed".to_owned(),
        })]),
        ..DriverState::default()
    }));
    let (_events_tx, events_rx) = mpsc::channel(1);
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::from([events_rx])),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    let settings = TempDir::new().expect("settings");

    let outcome = deliver_orchestration_turn(
        &supervisor,
        &engine,
        &settings.path().to_path_buf(),
        durable_turn_command("launch-failure", "hello"),
        "launch-failure-key".to_owned(),
    )
    .await;
    // The retried delivery's detail names the provider as the user does, not by driver id.
    assert_eq!(
        outcome,
        ProviderDeliveryOutcome::DefinitelyNotSent {
            detail: "Codex could not start: fixture launch failed".to_owned()
        }
    );
    let state = state.lock().unwrap();
    assert_eq!(state.starts, 1);
    assert!(state.sends.is_empty());
    assert_eq!(state.launches[0].thread_id, "t1");
    assert_eq!(state.launches[0].provider, "codex");
    drop(state);

    supervisor.shutdown().await.expect("supervisor shutdown");
    engine.shutdown().await;
}

#[tokio::test]
async fn durable_claude_turn_with_an_option_the_session_refuses_fails_once_instead_of_retrying() {
    let temp = TempDir::new().expect("attachments");
    let (engine, database) = engine_and_database().await;
    let settings = TempDir::new().expect("settings");
    // Nothing is spawned: the Claude session refuses the options before it launches a process.
    write_live_retry_settings(&settings, "claudeAgent", "route-claude");
    let command_id = "claude-refused-option".to_owned();
    let mut row = delivery_row("claudeAgent", "claude-refused-option-key");
    row.command_id = command_id.clone();
    row.message_id = format!("message-{command_id}");
    row.provider_instance_id = "route-claude".to_owned();
    // A thread that picked Claude Fable 5 from the static fallback catalog persisted every select
    // option at its default, including `contextWindow`, which the Claude session never applies.
    row.payload = json!({
        "type":"thread.turn.start", "commandId":command_id, "threadId":"t1",
        "message":{
            "messageId":row.message_id, "role":"user", "text":"hello", "attachments":[]
        },
        "modelSelection":{
            "instanceId":"route-claude", "model":"claude-fable-5",
            "options":[{"id":"effort","value":"high"},{"id":"contextWindow","value":"200k"}]
        },
        "runtimeMode":"full-access", "interactionMode":"default", "createdAt":NOW
    });
    freeze_row_route(&engine, &settings, &mut row).await;
    seed_pending_delivery(&database, row).await;
    let supervisor = Arc::new(ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(NativeProviderDriverFactory::new(
            temp.path().join("attachments"),
        )),
        activity_projection(&engine),
        SupervisorOptions::default(),
    ));
    let service = TurnDeliveryService::start(
        engine.clone(),
        supervisor.clone(),
        settings.path().to_path_buf(),
    );
    let read_delivery = || async {
        engine
            .repositories()
            .get_provider_turn_delivery(command_id.clone())
            .await
            .expect("delivery row")
            .expect("durable delivery")
    };

    let settled = timeout(Duration::from_secs(10), async {
        loop {
            let delivery = read_delivery().await;
            // A retried refusal keeps claiming new attempts while it stays pending.
            if delivery.state == TurnDeliveryState::Failed || delivery.attempts >= 3 {
                break delivery;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the refused delivery fails or keeps retrying");
    assert_eq!(
        (settled.state, settled.attempts),
        (TurnDeliveryState::Failed, 1),
        "a refused option must end the turn instead of retrying it: {:?}",
        settled.last_error
    );
    // The undelivered turn states the refusal plainly, without the driver's internal id.
    let refusal = "contextWindow is not supported by the selected model.";
    assert_eq!(settled.last_error.as_deref(), Some(refusal));
    // A failed row is terminal: the worker claims only pending rows. Stopping the service drains
    // its in-flight work, so the row read afterwards is final and the check needs no timing bound.
    service.shutdown().await;
    let later = read_delivery().await;
    assert_eq!(
        (later.state, later.attempts),
        (TurnDeliveryState::Failed, 1)
    );
    let (unresolved_state, unresolved_detail) = database
        .call(|connection| {
            connection
                .query_row(
                    "SELECT unresolved_delivery_state, unresolved_delivery_detail FROM projection_threads WHERE thread_id = 't1'",
                    [],
                    |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, Option<String>>(1)?)),
                )
                .map_err(Into::into)
        })
        .await
        .expect("thread shell");
    assert_eq!(unresolved_state.as_deref(), Some("failed"));
    assert_eq!(unresolved_detail.as_deref(), Some(refusal));

    supervisor.shutdown().await.expect("supervisor shutdown");
    engine.shutdown().await;
}

/// A turn with an option, driven through a real driver and the production delivery loop.
/// `refusal` is the plain detail a refused turn fails with; `None` means the turn is delivered.
struct OptionCase {
    provider: &'static str,
    instance_config: Value,
    binary_path: Option<PathBuf>,
    endpoint: Option<String>,
    environment: Vec<(String, String)>,
    model: &'static str,
    option: Value,
    refusal: Option<&'static str>,
}

/// Runs `case` until its delivery settles, and asserts it settles once: failed with the refusal,
/// or delivered, after a single launch.
async fn assert_option_case_settles_once(case: OptionCase) {
    let temp = TempDir::new().expect("provider fixture directory");
    let (engine, database) = engine_and_database().await;
    let settings = TempDir::new().expect("settings");
    let instance_id = format!("refusing-{}", case.provider);
    let mut instances = serde_json::Map::new();
    instances.insert(instance_id.clone(), case.instance_config);
    std::fs::write(
        settings.path().join("settings.json"),
        serde_json::to_vec(&json!({ "providerInstances": instances })).expect("settings json"),
    )
    .expect("write settings");
    let command_id = format!("refused-option-{}", case.provider);
    let mut row = delivery_row(case.provider, &format!("{command_id}-key"));
    row.command_id = command_id.clone();
    row.message_id = format!("message-{command_id}");
    row.provider_instance_id = instance_id.clone();
    row.payload = json!({
        "type":"thread.turn.start", "commandId":command_id, "threadId":"t1",
        "message":{
            "messageId":row.message_id, "role":"user", "text":"hello", "attachments":[]
        },
        "modelSelection":{"instanceId":instance_id, "model":case.model, "options":[case.option]},
        "runtimeMode":"full-access", "interactionMode":"default", "createdAt":NOW
    });
    freeze_row_route(&engine, &settings, &mut row).await;
    seed_pending_delivery(&database, row).await;
    let launches = Arc::new(StdMutex::new(Vec::new()));
    let supervisor = Arc::new(ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(NativeFixtureFactory {
            inner: NativeProviderDriverFactory::new(temp.path().join("attachments")),
            binary_path: case.binary_path,
            endpoint: case.endpoint,
            cwd: Some(temp.path().to_path_buf()),
            environment: case.environment,
            launches: launches.clone(),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    ));
    let service = TurnDeliveryService::start(
        engine.clone(),
        supervisor.clone(),
        settings.path().to_path_buf(),
    );
    let read_delivery = || async {
        engine
            .repositories()
            .get_provider_turn_delivery(command_id.clone())
            .await
            .expect("delivery row")
            .expect("durable delivery")
    };

    let Ok(settled) = timeout(Duration::from_secs(20), async {
        loop {
            let delivery = read_delivery().await;
            if matches!(
                delivery.state,
                TurnDeliveryState::Failed | TurnDeliveryState::Delivered
            ) || delivery.attempts >= 3
            {
                break delivery;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    else {
        let delivery = read_delivery().await;
        panic!(
            "{}: the delivery neither settled nor retried: {:?} at attempt {}, {:?}",
            case.provider, delivery.state, delivery.attempts, delivery.last_error
        );
    };
    let expected = if case.refusal.is_some() {
        TurnDeliveryState::Failed
    } else {
        TurnDeliveryState::Delivered
    };
    assert_eq!(
        (settled.state, settled.attempts),
        (expected, 1),
        "{}: the turn must settle once instead of retrying: {:?}",
        case.provider,
        settled.last_error
    );
    assert_eq!(
        settled.last_error.as_deref(),
        case.refusal,
        "{}",
        case.provider
    );
    // A failed or delivered row is terminal: the worker claims only pending rows. Stopping the
    // service drains its in-flight work, so what is read afterwards is final and the check needs
    // no timing bound.
    service.shutdown().await;
    let later = read_delivery().await;
    assert_eq!(
        (later.state, later.attempts),
        (expected, 1),
        "{}",
        case.provider
    );
    assert_eq!(
        launches.lock().unwrap().len(),
        1,
        "{}: the turn launches its provider once",
        case.provider
    );

    supervisor.shutdown().await.expect("supervisor shutdown");
    engine.shutdown().await;
}

/// A Codex App Server whose model list advertises `gpt-5` with only `high` reasoning, or, with
/// `BIBCODE_TEST_UNLISTED_MODEL` set, lists another model only. Its session runs
/// `BIBCODE_TEST_SESSION_MODEL`, `gpt-5` by default, and it accepts every turn.
#[cfg(unix)]
const MODEL_LIST_CODEX_FIXTURE: &str = r#"#!/bin/sh
session_model=${BIBCODE_TEST_SESSION_MODEL:-gpt-5}
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*) printf '{"id":%s,"result":{"userAgent":"fixture"}}\n' "$id" ;;
    *'"method":"thread/start"'*|*'"method":"thread/resume"'*) printf '{"id":%s,"result":{"cwd":"/tmp","model":"%s","thread":{"id":"native-codex-thread"}}}\n' "$id" "$session_model" ;;
    *'"method":"turn/start"'*) printf '{"id":%s,"result":{"turn":{"id":"native-codex-turn"}}}\n' "$id" ;;
    *'"method":"mcpServerStatus/list"'*) printf '{"id":%s,"result":{"data":[],"nextCursor":null}}\n' "$id" ;;
    *'"method":"model/list"'*)
      if [ -n "$BIBCODE_TEST_UNLISTED_MODEL" ]; then
        printf '{"id":%s,"result":{"data":[{"model":"gpt-other","serviceTiers":[],"supportedReasoningEfforts":[{"reasoningEffort":"high"}]}],"nextCursor":null}}\n' "$id"
      else
        printf '{"id":%s,"result":{"data":[{"model":"gpt-5","serviceTiers":[],"supportedReasoningEfforts":[{"reasoningEffort":"high"}]}],"nextCursor":null}}\n' "$id"
      fi ;;
    *'"method":"shutdown"'*) printf '{"id":%s,"result":null}\n' "$id" ;;
  esac
done
"#;

/// A Cursor agent whose session advertises only a model option, or, with
/// `BIBCODE_TEST_FAST_WRONG_CATEGORY` set, a `fast` option in a category BiBCode does not use.
#[cfg(unix)]
const REFUSING_CURSOR_FIXTURE: &str = r#"#!/bin/sh
if [ -n "$BIBCODE_TEST_FAST_WRONG_CATEGORY" ]; then
  options='[{"id":"model","category":"model"},{"id":"fast","category":"model","type":"select","options":[{"value":"false"},{"value":"true"}]}]'
else
  options='[{"id":"model","category":"model"}]'
fi
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*|*'"method":"authenticate"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id" ;;
    *'"method":"session/new"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"sessionId":"cursor-session","configOptions":%s,"modes":{"currentModeId":"ask","availableModes":[{"id":"ask","name":"Ask"},{"id":"code","name":"Agent"},{"id":"architect","name":"Plan"}]}}}\n' "$id" "$options" ;;
    *'"method":"session/set_config_option"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"configOptions":%s}}\n' "$id" "$options" ;;
    *'"method":"session/set_mode"'*|*'"method":"session/set_model"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id" ;;
  esac
done
"#;

/// A Codex instance with one custom model, `gpt-custom`, driving [`MODEL_LIST_CODEX_FIXTURE`].
#[cfg(unix)]
fn codex_case(fixtures: &TempDir, display_name: Option<&str>) -> OptionCase {
    let mut instance_config = json!({
        "driver":"codex", "enabled":true,
        "config":{
            "binaryPath":"refusing-codex-route",
            "homePath":fixtures.path().join("codex-home"),
            "shadowHomePath":fixtures.path().join("codex-shadow"),
            "customModels":["gpt-custom"]
        }
    });
    if let Some(display_name) = display_name {
        instance_config["displayName"] = json!(display_name);
    }
    OptionCase {
        provider: "codex",
        instance_config,
        binary_path: Some(executable_fixture(
            fixtures,
            "model-list-codex",
            MODEL_LIST_CODEX_FIXTURE,
            "",
        )),
        endpoint: None,
        environment: Vec::new(),
        model: "gpt-5",
        option: json!({"id":"reasoningEffort","value":"xhigh"}),
        refusal: None,
    }
}

#[cfg(unix)]
fn cursor_case(fixtures: &TempDir) -> OptionCase {
    OptionCase {
        provider: "cursor",
        instance_config: json!({
            "driver":"cursor", "enabled":true, "config":{"binaryPath":"refusing-cursor-route"}
        }),
        binary_path: Some(executable_fixture(
            fixtures,
            "refusing-cursor",
            REFUSING_CURSOR_FIXTURE,
            "",
        )),
        endpoint: None,
        environment: Vec::new(),
        model: "gpt-5",
        option: json!({"id":"fastMode","value":true}),
        refusal: None,
    }
}

/// Codex checks a turn's options against its model list after it starts. An option value the
/// model does not advertise ends the durable turn after one launch.
#[cfg(unix)]
#[tokio::test]
async fn codex_option_the_model_does_not_advertise_fails_the_turn_once() {
    let fixtures = TempDir::new().expect("fixture directory");
    assert_option_case_settles_once(OptionCase {
        refusal: Some("Reasoning Extra High is not supported by the selected model."),
        ..codex_case(&fixtures, None)
    })
    .await;
}

/// A model that is neither on Codex's model list nor one of the instance's custom models cannot
/// be checked, and never will be for the same turn, so the turn ends once, naming the instance as
/// the user does.
#[cfg(unix)]
#[tokio::test]
async fn codex_model_missing_from_the_model_list_fails_the_turn_once() {
    let fixtures = TempDir::new().expect("fixture directory");
    assert_option_case_settles_once(OptionCase {
        environment: vec![("BIBCODE_TEST_UNLISTED_MODEL".to_owned(), "1".to_owned())],
        refusal: Some("gpt-5 is not available in Work Codex."),
        ..codex_case(&fixtures, Some("Work Codex"))
    })
    .await;
}

/// The catalog offers an instance's custom models with the options of the first listed model, so
/// a custom model with one of those efforts validates the same way and the turn is delivered.
#[cfg(unix)]
#[tokio::test]
async fn codex_custom_model_with_an_effort_is_delivered() {
    let fixtures = TempDir::new().expect("fixture directory");
    assert_option_case_settles_once(OptionCase {
        environment: vec![(
            "BIBCODE_TEST_SESSION_MODEL".to_owned(),
            "gpt-custom".to_owned(),
        )],
        model: "gpt-custom",
        option: json!({"id":"reasoningEffort","value":"high"}),
        ..codex_case(&fixtures, None)
    })
    .await;
}

/// Cursor checks a turn's options against the session's advertised config options. An option the
/// session does not advertise ends the durable turn after one launch.
#[cfg(unix)]
#[tokio::test]
async fn cursor_option_the_session_does_not_advertise_fails_the_turn_once() {
    let fixtures = TempDir::new().expect("fixture directory");
    assert_option_case_settles_once(OptionCase {
        refusal: Some("Fast is not supported by the selected model."),
        ..cursor_case(&fixtures)
    })
    .await;
}

/// A session that advertises an option in a shape BiBCode does not use offers it the same way on
/// every launch, so the turn ends once, with a plain detail instead of Cursor's protocol text.
#[cfg(unix)]
#[tokio::test]
async fn cursor_option_in_an_unsupported_shape_fails_the_turn_once_in_plain_words() {
    let fixtures = TempDir::new().expect("fixture directory");
    assert_option_case_settles_once(OptionCase {
        environment: vec![(
            "BIBCODE_TEST_FAST_WRONG_CATEGORY".to_owned(),
            "1".to_owned(),
        )],
        refusal: Some(
            "BiBCode can't apply these options to the selected model. Choose another model, or turn these options off.",
        ),
        ..cursor_case(&fixtures)
    })
    .await;
}

/// OpenCode checks a turn's options against the model's advertised variants. An option the model
/// does not offer ends the durable turn after one launch.
#[cfg(unix)]
#[tokio::test]
async fn opencode_option_the_model_does_not_advertise_fails_the_turn_once() {
    let app = Router::new()
        .route(
            "/session",
            post(|| async { Json(json!({"id":"opencode-refusal-session"})) }),
        )
        .route(
            "/session/{session_id}",
            get(|| async { Json(json!({"id":"opencode-refusal-session"})) }),
        )
        .route(
            "/event",
            get(|| async { Sse::new(stream::pending::<Result<Event, Infallible>>()) }),
        )
        .route(
            "/session/{session_id}/message",
            get(|| async { Json(json!({"data":[]})) }),
        )
        .route(
            "/provider",
            get(|| async {
                Json(json!({
                    "connected": ["openai"],
                    "all": [{"id":"openai", "models":{"gpt-5":{"variants":{"high":{}}}}}]
                }))
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("OpenCode fixture bind");
    let endpoint = format!("http://{}", listener.local_addr().expect("fixture address"));
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("OpenCode fixture serve");
    });
    assert_option_case_settles_once(OptionCase {
        provider: "opencode",
        instance_config: json!({
            "driver":"opencode", "enabled":true,
            "config":{"binaryPath":"refusing-opencode-route", "serverUrl":endpoint}
        }),
        binary_path: None,
        endpoint: Some(endpoint.clone()),
        environment: Vec::new(),
        model: "openai/gpt-5",
        option: json!({"id":"fastMode","value":true}),
        refusal: Some("Fast is not supported by the selected model."),
    })
    .await;
    server.abort();
}

#[tokio::test]
async fn admitted_turn_freezes_native_session_before_provider_delivery_begins() {
    let (engine, _) = engine_and_database().await;
    let delivery_entered = Arc::new(tokio::sync::Notify::new());
    let delivery_release = Arc::new(tokio::sync::Semaphore::new(0));
    let state = Arc::new(StdMutex::new(DriverState {
        start_results: VecDeque::from([Ok(StartedSession {
            resume_cursor: Some(json!({"threadId":"native-admitted-session"})),
            runtime_payload: None,
            activity_capabilities: ActivityCapabilities::none(),
        })]),
        delivery_entered: Some(delivery_entered.clone()),
        delivery_release: Some(delivery_release.clone()),
        ..DriverState::default()
    }));
    let (_events_tx, events_rx) = mpsc::channel(1);
    let supervisor = Arc::new(ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::from([events_rx])),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    ));
    supervisor.launch(launch()).await.expect("provider launch");

    let settings = TempDir::new().expect("settings");
    let delivery = Arc::new(TurnDeliveryService::start(
        engine.clone(),
        supervisor.clone(),
        settings.path().to_path_buf(),
    ));
    let mut registry = RpcRegistry::empty();
    register_orchestration_rpc_with_delivery(
        &mut registry,
        engine.clone(),
        supervisor.clone(),
        settings.path().to_path_buf(),
        delivery.clone(),
        attachment_upload_registry(),
    );
    let handle = ServerRuntime::start_with_registry(test_config(&settings), registry)
        .await
        .expect("server runtime");
    let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("websocket");
    rpc_request(
        &mut socket,
        "805",
        serde_json::to_value(durable_turn_command("freeze-before-delivery", "hello"))
            .expect("turn command json"),
    )
    .await;
    rpc_response(&mut socket, "805")
        .await
        .expect("turn admission response");
    let admitted = engine
        .repositories()
        .get_provider_turn_delivery("freeze-before-delivery".to_owned())
        .await
        .expect("admitted row")
        .expect("outbox row");
    if admitted.state == TurnDeliveryState::Pending {
        assert_eq!(admitted.provider_session_id, None);
    }

    timeout(Duration::from_secs(10), delivery_entered.notified())
        .await
        .expect("provider delivery entered");
    let frozen = engine
        .repositories()
        .get_provider_turn_delivery("freeze-before-delivery".to_owned())
        .await
        .expect("frozen row")
        .expect("outbox row");
    assert_eq!(frozen.state, TurnDeliveryState::Sending);
    assert_eq!(frozen.attempts, 1);
    assert_eq!(
        frozen.provider_session_id.as_deref(),
        Some("native-admitted-session"),
        "the native identity must be durable before driver.deliver is entered"
    );

    delivery_release.add_permits(1);
    timeout(Duration::from_secs(10), async {
        loop {
            let row = engine
                .repositories()
                .get_provider_turn_delivery("freeze-before-delivery".to_owned())
                .await
                .expect("delivery row")
                .expect("outbox row");
            if row.state == TurnDeliveryState::Delivered {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("admitted turn completes");
    assert_eq!(state.lock().unwrap().sends, vec!["hello"]);

    socket.close(None).await.expect("websocket close");
    handle.shutdown();
    handle.join().await.expect("server shutdown");
    delivery.shutdown().await;
    supervisor.shutdown().await.expect("supervisor shutdown");
    engine.shutdown().await;
}

#[tokio::test]
async fn durable_delivery_preserves_post_admission_transport_as_ambiguous() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState {
        delivery_outcomes: VecDeque::from([ProviderDeliveryOutcome::Ambiguous {
            detail: "connection closed after request write".to_owned(),
        }]),
        ..DriverState::default()
    }));
    let (_events_tx, events_rx) = mpsc::channel(1);
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::from([events_rx])),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    supervisor.launch(launch()).await.expect("provider launch");
    let settings = TempDir::new().expect("settings");

    let outcome = deliver_orchestration_turn(
        &supervisor,
        &engine,
        &settings.path().to_path_buf(),
        durable_turn_command("ambiguous", "hello"),
        "ambiguous-key".to_owned(),
    )
    .await;
    assert_eq!(
        outcome,
        ProviderDeliveryOutcome::Ambiguous {
            detail: "connection closed after request write".to_owned()
        }
    );
    assert!(state.lock().unwrap().sends.is_empty());

    supervisor.shutdown().await.expect("supervisor shutdown");
    engine.shutdown().await;
}

#[tokio::test]
async fn registered_dispatch_rpc_prepares_attachments_before_persistence_and_provider_routing() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (_events_tx, events_rx) = mpsc::channel(8);
    let supervisor = Arc::new(ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::from([events_rx])),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    ));
    supervisor.launch(launch()).await.unwrap();

    let settings = TempDir::new().unwrap();
    let mut registry = RpcRegistry::empty();
    let delivery = Arc::new(TurnDeliveryService::start(
        engine.clone(),
        supervisor.clone(),
        settings.path().to_path_buf(),
    ));
    register_orchestration_rpc_with_delivery(
        &mut registry,
        engine.clone(),
        supervisor.clone(),
        settings.path().to_path_buf(),
        delivery.clone(),
        attachment_upload_registry(),
    );
    let handle = ServerRuntime::start_with_registry(test_config(&settings), registry)
        .await
        .unwrap();
    let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .unwrap();

    rpc_request(
        &mut socket,
        "801",
        json!({
            "type":"thread.turn.start", "commandId":"rpc-upload", "threadId":"t1",
            "message":{
                "messageId":"message-upload", "role":"user", "text":"review", "attachments":[{
                    "type":"file", "id":"notes-1", "name":"notes.txt", "mimeType":"text/plain",
                    "sizeBytes":5, "dataUrl":"data:text/plain;base64,bm90ZXM="
                }]
            },
            "createdAt":NOW
        }),
    )
    .await;
    rpc_response(&mut socket, "801")
        .await
        .expect("registered attachment RPC succeeds");
    timeout(Duration::from_secs(10), async {
        loop {
            let row = engine
                .repositories()
                .get_provider_turn_delivery("rpc-upload".to_owned())
                .await
                .expect("delivery row")
                .expect("outbox row");
            if row.state == TurnDeliveryState::Delivered {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("attachment delivery completes");

    let safe_attachments = json!([{
        "type":"file", "id":"notes-1", "name":"notes.txt", "mimeType":"text/plain", "sizeBytes":5
    }]);
    let events = engine.read_events(0).await.unwrap();
    let message_event = events
        .iter()
        .find(|event| {
            event.event.command_id.as_deref() == Some("rpc-upload")
                && event.event.event_type == "thread.message-sent"
        })
        .expect("message event persists");
    assert_eq!(message_event.event.payload["attachments"], safe_attachments);
    assert!(
        message_event.event.payload["attachments"][0]
            .get("dataUrl")
            .is_none()
    );
    assert_eq!(
        state.lock().unwrap().sent_attachments,
        [safe_attachments.as_array().unwrap().clone()]
    );
    assert_eq!(
        std::fs::read(settings.path().join("attachments/notes-1")).unwrap(),
        b"notes"
    );
    let attachment_root = settings.path().join("attachments");
    let parked_attachment_root = settings.path().join("attachments-parked");
    std::fs::rename(&attachment_root, &parked_attachment_root).unwrap();
    std::fs::write(&attachment_root, b"not a directory").unwrap();
    let sends_before_replay = state.lock().unwrap().sends.len();
    rpc_request(
        &mut socket,
        "805",
        json!({
            "type":"thread.turn.start", "commandId":"rpc-upload", "threadId":"t1",
            "message":{
                "messageId":"message-upload", "role":"user", "text":"review", "attachments":[{
                    "type":"file", "id":"notes-1", "name":"notes.txt", "mimeType":"text/plain",
                    "sizeBytes":5, "dataUrl":"data:text/plain;base64,bm90ZXM="
                }]
            },
            "createdAt":NOW
        }),
    )
    .await;
    rpc_response(&mut socket, "805")
        .await
        .expect("registered replay bypasses attachment materialization");
    assert_eq!(
        state.lock().unwrap().sends.len(),
        sends_before_replay,
        "a replay cannot enqueue or route a second provider turn"
    );
    std::fs::remove_file(&attachment_root).unwrap();
    std::fs::rename(&parked_attachment_root, &attachment_root).unwrap();
    let sends_before_failure = state.lock().unwrap().sends.len();

    rpc_request(
        &mut socket,
        "802",
        json!({
            "type":"thread.turn.start", "commandId":"rpc-upload-invalid", "threadId":"t1",
            "message":{
                "messageId":"message-upload-invalid", "role":"user", "text":"review", "attachments":[{
                    "type":"file", "id":"notes-2", "name":"notes.txt", "mimeType":"text/plain",
                    "sizeBytes":5, "dataUrl":"data:text/plain,notes"
                }]
            },
            "createdAt":NOW
        }),
    )
    .await;
    let cause = rpc_response(&mut socket, "802")
        .await
        .expect_err("malformed upload fails through the registered RPC");
    assert_eq!(cause[0]["_tag"], "Fail");
    assert_eq!(cause[0]["error"]["_tag"], "InvalidRequest");
    assert_eq!(cause[0]["error"]["method"], "orchestration.dispatchCommand");
    assert!(
        cause[0]["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("base64"))
    );
    let events = engine.read_events(0).await.unwrap();
    assert!(
        events
            .iter()
            .all(|event| event.event.command_id.as_deref() != Some("rpc-upload-invalid")),
        "a rejected command id cannot persist any event"
    );
    assert_eq!(state.lock().unwrap().sends.len(), sends_before_failure);

    rpc_request(
        &mut socket,
        "803",
        json!({
            "type":"thread.turn.start", "commandId":"rpc-upload-missing-thread", "threadId":"missing-thread",
            "message":{
                "messageId":"message-upload-missing-thread", "role":"user", "text":"review", "attachments":[{
                    "type":"file", "id":"missing-thread-upload", "name":"notes.txt", "mimeType":"text/plain",
                    "sizeBytes":5, "dataUrl":"data:text/plain;base64,bm90ZXM="
                }]
            },
            "createdAt":NOW
        }),
    )
    .await;
    rpc_response(&mut socket, "803")
        .await
        .expect_err("a nonexistent thread rejects after attachment preparation");
    assert!(
        !settings
            .path()
            .join("attachments/missing-thread-upload")
            .exists(),
        "dispatch rejection rolls back the prepared final"
    );
    assert!(
        engine
            .read_events(0)
            .await
            .unwrap()
            .iter()
            .all(|event| event.event.command_id.as_deref() != Some("rpc-upload-missing-thread")),
        "dispatch rejection cannot persist an event"
    );

    socket.close(None).await.unwrap();
    handle.shutdown();
    handle.join().await.unwrap();
    delivery.shutdown().await;
    supervisor.shutdown().await.unwrap();
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registered_durable_rpc_wakes_delivery_before_a_cancelled_response_returns() {
    let hooks = TestHooks::default();
    let pause = hooks.pause_after_next_admission_commit();
    let (engine, _) = engine_and_database_with_options(EngineOptions {
        test_hooks: hooks,
        ..EngineOptions::default()
    })
    .await;
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (_events_tx, events_rx) = mpsc::channel(8);
    let supervisor = Arc::new(ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::from([events_rx])),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    ));
    supervisor.launch(launch()).await.unwrap();
    let settings = TempDir::new().unwrap();
    let delivery = Arc::new(TurnDeliveryService::start(
        engine.clone(),
        supervisor.clone(),
        settings.path().to_path_buf(),
    ));
    let mut registry = RpcRegistry::empty();
    register_orchestration_rpc_with_delivery(
        &mut registry,
        engine.clone(),
        supervisor.clone(),
        settings.path().to_path_buf(),
        delivery.clone(),
        attachment_upload_registry(),
    );
    let handle = ServerRuntime::start_with_registry(test_config(&settings), registry)
        .await
        .unwrap();
    let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .unwrap();

    rpc_request(
        &mut socket,
        "804",
        json!({
            "type":"thread.turn.start", "commandId":"rpc-cancel-after-commit", "threadId":"t1",
            "message":{"messageId":"message-cancel", "role":"user", "text":"wake", "attachments":[]},
            "createdAt":NOW
        }),
    )
    .await;
    timeout(Duration::from_secs(10), pause.wait_until_entered())
        .await
        .expect("engine pauses after the durable commit callback");
    drop(socket);

    timeout(Duration::from_secs(10), async {
        loop {
            if state
                .lock()
                .unwrap()
                .sends
                .iter()
                .any(|message| message == "wake")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("commit callback wakes delivery before the cancelled RPC can return");
    assert_eq!(
        engine
            .repositories()
            .get_provider_turn_delivery("rpc-cancel-after-commit".to_owned())
            .await
            .expect("delivery row")
            .expect("durable delivery")
            .state,
        bibcode_server::orchestration::TurnDeliveryState::Sending,
        "provider routing started while the engine response remained paused"
    );

    pause.release();
    handle.shutdown();
    handle.join().await.unwrap();
    delivery.shutdown().await;
    supervisor.shutdown().await.unwrap();
    engine.shutdown().await;
}

async fn add_delivery_thread(engine: &OrchestrationEngine, thread_id: &str) {
    add_delivery_thread_for(engine, thread_id, "codex", "gpt-5").await;
}

async fn add_delivery_thread_for(
    engine: &OrchestrationEngine,
    thread_id: &str,
    provider: &str,
    model: &str,
) {
    engine
        .dispatch(
            serde_json::from_value(json!({
                "type":"thread.create", "commandId":format!("create-{thread_id}"),
                "threadId":thread_id, "projectId":"p1", "title":thread_id,
                "modelSelection":{"instanceId":provider,"model":model},
                "runtimeMode":"full-access", "interactionMode":"default",
                "branch":null, "worktreePath":null, "createdAt":NOW
            }))
            .expect("thread command"),
        )
        .await
        .expect("delivery thread");
}

fn launch_for_thread(thread_id: &str) -> ProviderLaunchRequest {
    ProviderLaunchRequest {
        thread_id: thread_id.to_owned(),
        ..launch()
    }
}

fn launch_for_provider(thread_id: &str, provider: &str, model: &str) -> ProviderLaunchRequest {
    let mut request = launch();
    request.thread_id = thread_id.to_owned();
    request.provider = provider.to_owned();
    request.provider_label = provider.to_owned();
    request.provider_instance_id = Some(provider.to_owned());
    request.binary_path = match provider {
        "claudeAgent" => "claude",
        "cursor" => "cursor-agent",
        other => other,
    }
    .to_owned();
    request.model = Some(model.to_owned());
    if provider != "codex" {
        request.codex_home = None;
    }
    request
}

fn rpc_turn(thread_id: &str, command_id: &str, text: &str) -> Value {
    json!({
        "type":"thread.turn.start", "commandId":command_id, "threadId":thread_id,
        "message":{
            "messageId":format!("message-{command_id}"), "role":"user", "text":text,
            "attachments":[]
        },
        "modelSelection":{"instanceId":"codex","model":"gpt-5"},
        "runtimeMode":"full-access", "interactionMode":"default", "createdAt":NOW
    })
}

async fn wait_for_delivery_state_for_command(
    engine: &OrchestrationEngine,
    command_id: &str,
    expected: TurnDeliveryState,
) {
    timeout(Duration::from_secs(10), async {
        loop {
            let row = engine
                .repositories()
                .get_provider_turn_delivery(command_id.to_owned())
                .await
                .expect("delivery query")
                .expect("delivery row");
            if row.state == expected {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{command_id} did not reach {expected:?}"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delivery_service_orders_each_thread_without_blocking_another_thread() {
    let hooks = TestHooks::default();
    let (engine, database) = engine_and_database_with_options(EngineOptions {
        test_hooks: hooks.clone(),
        ..EngineOptions::default()
    })
    .await;
    database
        .call(|connection| {
            connection.execute_batch(
                "CREATE TRIGGER test_block_order_a1_terminal
                 BEFORE UPDATE OF state ON provider_turn_outbox
                 WHEN OLD.command_id = 'order-a1'
                   AND OLD.state = 'sending'
                   AND NEW.state = 'delivered'
                 BEGIN
                   SELECT RAISE(ABORT, 'test holds order-a1 nonterminal');
                 END;

                 CREATE TABLE test_delivery_order_observations (
                   a2_claimed_while_a1_nonterminal INTEGER NOT NULL
                 );
                 CREATE TRIGGER test_observe_order_a2_claim
                 AFTER UPDATE OF state ON provider_turn_outbox
                 WHEN OLD.command_id = 'order-a2'
                   AND OLD.state = 'pending'
                   AND NEW.state = 'sending'
                 BEGIN
                   INSERT INTO test_delivery_order_observations
                     (a2_claimed_while_a1_nonterminal)
                   SELECT 1
                   WHERE EXISTS (
                     SELECT 1
                     FROM provider_turn_outbox
                     WHERE command_id = 'order-a1'
                       AND state NOT IN ('delivered', 'dismissed')
                   );
                 END;",
            )?;
            Ok(())
        })
        .await
        .expect("install A1 terminal transition gate");
    add_delivery_thread(&engine, "t2").await;
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let state = Arc::new(StdMutex::new(DriverState {
        delivery_release: Some(release.clone()),
        ..DriverState::default()
    }));
    let (t1_events_tx, t1_events_rx) = mpsc::channel(1);
    let (_t2_events_tx, t2_events_rx) = mpsc::channel(1);
    let supervisor = Arc::new(ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::from([t1_events_rx, t2_events_rx])),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    ));
    for thread_id in ["t1", "t2"] {
        supervisor
            .launch(launch_for_thread(thread_id))
            .await
            .expect("provider launch");
    }
    let settings = TempDir::new().expect("settings");
    let delivery = Arc::new(TurnDeliveryService::start(
        engine.clone(),
        supervisor.clone(),
        settings.path().to_path_buf(),
    ));
    let mut registry = RpcRegistry::empty();
    register_orchestration_rpc_with_delivery(
        &mut registry,
        engine.clone(),
        supervisor.clone(),
        settings.path().to_path_buf(),
        delivery.clone(),
        attachment_upload_registry(),
    );
    let runtime = ServerRuntime::start_with_registry(test_config(&settings), registry)
        .await
        .expect("delivery runtime");
    let (mut socket, _) = connect_async(format!("ws://{}/ws", runtime.local_addr()))
        .await
        .expect("delivery websocket");
    for (request_id, thread_id, command_id, text) in [
        ("901", "t1", "order-a1", "A1"),
        ("902", "t1", "order-a2", "A2"),
        ("903", "t2", "order-b1", "B1"),
    ] {
        rpc_request(
            &mut socket,
            request_id,
            rpc_turn(thread_id, command_id, text),
        )
        .await;
        rpc_response(&mut socket, request_id)
            .await
            .expect("turn admission");
    }

    timeout(Duration::from_secs(10), async {
        loop {
            let snapshot = state.lock().unwrap().delivery_started.clone();
            if snapshot.contains(&"A1".to_owned()) && snapshot.contains(&"B1".to_owned()) {
                assert!(!snapshot.contains(&"A2".to_owned()));
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("A1 and B1 start concurrently");

    release.add_permits(2);
    timeout(Duration::from_secs(10), async {
        loop {
            let transition_attempts = hooks.delivery_transition_attempts();
            if transition_attempts >= 4 {
                let a1 = engine
                    .repositories()
                    .get_provider_turn_delivery("order-a1".to_owned())
                    .await
                    .expect("A1 blocked delivery query")
                    .expect("A1 blocked delivery row");
                let b1 = engine
                    .repositories()
                    .get_provider_turn_delivery("order-b1".to_owned())
                    .await
                    .expect("B1 delivery query")
                    .expect("B1 delivery row");
                if a1.state == TurnDeliveryState::Sending
                    && matches!(
                        b1.state,
                        TurnDeliveryState::Delivered | TurnDeliveryState::Dismissed
                    )
                {
                    assert!(
                        !state
                            .lock()
                            .unwrap()
                            .delivery_started
                            .contains(&"A2".to_owned()),
                        "A2 entered while A1's terminal transition was retryable"
                    );
                    break;
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("B1 settles while A1 retries its blocked terminal transition");

    database
        .call(|connection| {
            connection.execute_batch("DROP TRIGGER test_block_order_a1_terminal;")?;
            Ok(())
        })
        .await
        .expect("release A1 terminal transition gate");
    wait_for_delivery_state_for_command(&engine, "order-a1", TurnDeliveryState::Delivered).await;
    let session = engine
        .repositories()
        .get_thread_session("t1".to_owned())
        .await
        .expect("t1 session after A1 acknowledgement")
        .expect("t1 session");
    assert_eq!(session.status, "running");
    assert!(
        timeout(Duration::from_millis(100), async {
            loop {
                if state
                    .lock()
                    .unwrap()
                    .delivery_started
                    .contains(&"A2".to_owned())
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .is_err(),
        "A1 acknowledgement must not release A2 while t1 is still running"
    );
    let a2 = engine
        .repositories()
        .get_provider_turn_delivery("order-a2".to_owned())
        .await
        .expect("A2 waiting delivery query")
        .expect("A2 waiting delivery row");
    assert_eq!(a2.state, TurnDeliveryState::Pending);
    assert_eq!(a2.attempts, 0, "A2 must not be claimed before t1 settles");

    t1_events_tx
        .send(ProviderEvent {
            native_event_id: None,
            event_type: "turn.completed".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: session.active_turn_id,
            item_id: None,
            request_id: None,
            payload: json!({"state":"completed"}),
            activity: Vec::new(),
            activity_controls: Default::default(),
        })
        .await
        .expect("complete A1 through the provider event channel");
    timeout(Duration::from_secs(10), async {
        loop {
            if engine
                .repositories()
                .get_thread_session("t1".to_owned())
                .await
                .expect("t1 session after provider completion")
                .is_some_and(|session| session.status == "ready")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("A1 completion projects t1 ready");
    timeout(Duration::from_secs(10), async {
        loop {
            if state
                .lock()
                .unwrap()
                .delivery_started
                .contains(&"A2".to_owned())
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("A2 starts after t1 settles to ready");
    let (a1_state, early_a2_claims) = database
        .call(|connection| {
            let a1_state = connection.query_row(
                "SELECT state FROM provider_turn_outbox WHERE command_id = 'order-a1'",
                [],
                |row| row.get::<_, String>(0),
            )?;
            let early_a2_claims = connection.query_row(
                "SELECT COUNT(*) FROM test_delivery_order_observations",
                [],
                |row| row.get::<_, i64>(0),
            )?;
            connection.execute_batch(
                "DROP TRIGGER test_observe_order_a2_claim;
                 DROP TABLE test_delivery_order_observations;",
            )?;
            Ok((a1_state, early_a2_claims))
        })
        .await
        .expect("read persisted delivery-order observation");
    assert_eq!(
        early_a2_claims, 0,
        "A2 was claimed while A1 was still nonterminal"
    );
    assert!(
        matches!(a1_state.as_str(), "delivered" | "dismissed"),
        "A2 entered while A1 was still {a1_state}"
    );
    release.add_permits(1);

    socket.close(None).await.expect("websocket close");
    runtime.shutdown();
    runtime.join().await.expect("runtime shutdown");
    delivery.shutdown().await;
    supervisor.shutdown().await.expect("provider shutdown");
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn delivery_service_never_exceeds_its_configured_four_thread_semaphore() {
    let engine = engine().await;
    for thread_id in [
        "capacity-thread-2",
        "capacity-thread-3",
        "capacity-thread-4",
        "capacity-thread-5",
    ] {
        add_delivery_thread(&engine, thread_id).await;
    }
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let state = Arc::new(StdMutex::new(DriverState {
        delivery_release: Some(release.clone()),
        ..DriverState::default()
    }));
    let mut receivers = VecDeque::new();
    // Keep every event sender alive: a closed event stream marks the session dead
    // and the next delivery would relaunch instead of reusing the live driver.
    let mut live_event_senders = Vec::new();
    for _ in 0..5 {
        let (events_tx, events_rx) = mpsc::channel(1);
        live_event_senders.push(events_tx);
        receivers.push_back(events_rx);
    }
    let supervisor = Arc::new(ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(receivers),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    ));
    for thread_id in [
        "t1",
        "capacity-thread-2",
        "capacity-thread-3",
        "capacity-thread-4",
        "capacity-thread-5",
    ] {
        supervisor
            .launch(launch_for_thread(thread_id))
            .await
            .expect("provider launch");
    }
    let settings = TempDir::new().expect("settings");
    let delivery = Arc::new(TurnDeliveryService::start(
        engine.clone(),
        supervisor.clone(),
        settings.path().to_path_buf(),
    ));
    let mut registry = RpcRegistry::empty();
    register_orchestration_rpc_with_delivery(
        &mut registry,
        engine.clone(),
        supervisor.clone(),
        settings.path().to_path_buf(),
        delivery.clone(),
        attachment_upload_registry(),
    );
    let runtime = ServerRuntime::start_with_registry(test_config(&settings), registry)
        .await
        .expect("delivery runtime");
    let (mut socket, _) = connect_async(format!("ws://{}/ws", runtime.local_addr()))
        .await
        .expect("delivery websocket");
    for (index, thread_id) in [
        "t1",
        "capacity-thread-2",
        "capacity-thread-3",
        "capacity-thread-4",
        "capacity-thread-5",
    ]
    .into_iter()
    .enumerate()
    {
        let request_id = (920 + index).to_string();
        let command_id = format!("capacity-{index}");
        let text = format!("capacity-{index}");
        rpc_request(
            &mut socket,
            &request_id,
            rpc_turn(thread_id, &command_id, &text),
        )
        .await;
        rpc_response(&mut socket, &request_id)
            .await
            .expect("turn admission");
    }
    timeout(Duration::from_secs(10), async {
        loop {
            let state = state.lock().unwrap();
            if state.delivery_active == 4 {
                assert_eq!(state.delivery_started.len(), 4);
                assert_eq!(state.delivery_max_active, 4);
                break;
            }
            drop(state);
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("four deliveries fill the semaphore");
    release.add_permits(4);
    timeout(Duration::from_secs(10), async {
        loop {
            if state.lock().unwrap().delivery_started.len() == 5 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("fifth delivery starts after a permit returns");
    release.add_permits(1);
    timeout(Duration::from_secs(10), async {
        loop {
            if state.lock().unwrap().delivery_active == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("all deliveries finish");
    assert_eq!(state.lock().unwrap().delivery_max_active, 4);

    socket.close(None).await.expect("websocket close");
    runtime.shutdown();
    runtime.join().await.expect("runtime shutdown");
    delivery.shutdown().await;
    supervisor.shutdown().await.expect("provider shutdown");
    engine.shutdown().await;
    drop(live_event_senders);
}

fn mixed_attachment_rpc_turn(
    thread_id: &str,
    command_id: &str,
    provider: &str,
    model: &str,
) -> Value {
    json!({
        "type":"thread.turn.start", "commandId":command_id, "threadId":thread_id,
        "message":{
            "messageId":format!("message-{command_id}"), "role":"user",
            "text":format!("send-{provider}"),
            "attachments":[
                {
                    "type":"file", "id":format!("{provider}-file"), "name":"notes.txt",
                    "mimeType":"text/plain", "sizeBytes":5,
                    "dataUrl":"data:text/plain;base64,bm90ZXM="
                },
                {
                    "type":"image", "id":format!("{provider}-image"), "name":"screen.png",
                    "mimeType":"image/png", "sizeBytes":5,
                    "dataUrl":"data:image/png;base64,aW1hZ2U="
                }
            ]
        },
        "modelSelection":{"instanceId":provider,"model":model},
        "runtimeMode":"full-access", "interactionMode":"default", "createdAt":NOW
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn registered_dispatch_rpc_proves_one_mixed_attachment_delivery_for_every_provider() {
    let hooks = TestHooks::default();
    let (engine, _) = engine_and_database_with_options(EngineOptions {
        test_hooks: hooks.clone(),
        ..EngineOptions::default()
    })
    .await;
    let providers = [
        ("t1", "rpc-mixed-codex", "codex", "gpt-5"),
        (
            "mixed-claude",
            "rpc-mixed-claude",
            "claudeAgent",
            "claude-sonnet-4-5",
        ),
        (
            "mixed-opencode",
            "rpc-mixed-opencode",
            "opencode",
            "openai/gpt-5",
        ),
        (
            "mixed-cursor",
            "rpc-mixed-cursor",
            "cursor",
            "cursor-default",
        ),
    ];
    for (thread_id, _, provider, model) in providers.iter().skip(1).copied() {
        add_delivery_thread_for(&engine, thread_id, provider, model).await;
    }
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let mut receivers = VecDeque::new();
    let mut event_senders = Vec::new();
    for _ in 0..providers.len() {
        let (events_tx, events_rx) = mpsc::channel(1);
        event_senders.push(events_tx);
        receivers.push_back(events_rx);
    }
    let supervisor = Arc::new(ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(receivers),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    ));
    let settings = TempDir::new().expect("settings");
    let binary_path = hermetic_providers::missing_provider_executable(settings.path());
    hermetic_providers::write_hermetic_settings(
        settings.path(),
        json!({
            "providerInstances": {
                "cursor": {
                    "driver": "cursor",
                    "enabled": true,
                    "config": {"binaryPath": binary_path}
                }
            }
        }),
    );
    for (thread_id, _, provider, model) in providers {
        let mut request = launch_for_provider(thread_id, provider, model);
        request.binary_path = binary_path
            .to_str()
            .expect("fixture path is UTF-8")
            .to_owned();
        supervisor.launch(request).await.expect("provider launch");
    }
    let delivery = Arc::new(TurnDeliveryService::start(
        engine.clone(),
        supervisor.clone(),
        settings.path().to_path_buf(),
    ));
    let mut registry = RpcRegistry::empty();
    register_orchestration_rpc_with_delivery(
        &mut registry,
        engine.clone(),
        supervisor.clone(),
        settings.path().to_path_buf(),
        delivery.clone(),
        attachment_upload_registry(),
    );
    let runtime = ServerRuntime::start_with_registry(test_config(&settings), registry)
        .await
        .expect("delivery runtime");
    let (mut socket, _) = connect_async(format!("ws://{}/ws", runtime.local_addr()))
        .await
        .expect("delivery websocket");

    let replay_payload = mixed_attachment_rpc_turn("t1", "rpc-mixed-codex", "codex", "gpt-5");
    for (index, (thread_id, command_id, provider, model)) in
        providers.into_iter().take(3).enumerate()
    {
        let request_id = (950 + index).to_string();
        rpc_request(
            &mut socket,
            &request_id,
            mixed_attachment_rpc_turn(thread_id, command_id, provider, model),
        )
        .await;
        rpc_response(&mut socket, &request_id)
            .await
            .expect("mixed attachment admission");
        wait_for_delivery_state_for_command(&engine, command_id, TurnDeliveryState::Delivered)
            .await;
    }

    let cancellation_pause = hooks.pause_after_next_admission_commit();
    rpc_request(
        &mut socket,
        "953",
        mixed_attachment_rpc_turn(
            "mixed-cursor",
            "rpc-mixed-cursor",
            "cursor",
            "cursor-default",
        ),
    )
    .await;
    timeout(
        Duration::from_secs(10),
        cancellation_pause.wait_until_entered(),
    )
    .await
    .expect("cursor command commits before cancellation");
    drop(socket);
    timeout(Duration::from_secs(10), async {
        loop {
            if state
                .lock()
                .unwrap()
                .sends
                .contains(&"send-cursor".to_owned())
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancelled caller cannot cancel committed provider delivery");
    cancellation_pause.release();
    wait_for_delivery_state_for_command(&engine, "rpc-mixed-cursor", TurnDeliveryState::Delivered)
        .await;

    let (mut socket, _) = connect_async(format!("ws://{}/ws", runtime.local_addr()))
        .await
        .expect("replay websocket");
    rpc_request(&mut socket, "954", replay_payload).await;
    rpc_response(&mut socket, "954")
        .await
        .expect("identical command replay");

    let snapshot = state.lock().unwrap();
    assert_eq!(snapshot.sends.len(), 4);
    assert_eq!(
        snapshot.launches.len(),
        4,
        "live providers need no relaunch"
    );
    assert_eq!(
        snapshot.delivery_routes,
        vec![
            (
                "codex".to_owned(),
                Some("codex".to_owned()),
                "send-codex".to_owned(),
            ),
            (
                "claudeAgent".to_owned(),
                Some("claudeAgent".to_owned()),
                "send-claudeAgent".to_owned(),
            ),
            (
                "opencode".to_owned(),
                Some("opencode".to_owned()),
                "send-opencode".to_owned(),
            ),
            (
                "cursor".to_owned(),
                Some("cursor".to_owned()),
                "send-cursor".to_owned(),
            ),
        ],
        "each command must reach the selected provider driver instance"
    );
    for provider in ["codex", "claudeAgent", "opencode", "cursor"] {
        assert_eq!(
            snapshot
                .sends
                .iter()
                .filter(|text| text.as_str() == format!("send-{provider}"))
                .count(),
            1,
            "{provider} receives one provider submission"
        );
    }
    assert_eq!(snapshot.sent_attachments.len(), 4);
    assert!(snapshot.sent_attachments.iter().all(|attachments| {
        attachments.len() == 2
            && attachments
                .iter()
                .all(|attachment| attachment.get("dataUrl").is_none())
    }));
    drop(snapshot);
    let outbox_rows = engine
        .repositories()
        .database()
        .call(|connection| {
            Ok(connection.query_row(
                "SELECT COUNT(*) FROM provider_turn_outbox WHERE command_id IN ('rpc-mixed-codex', 'rpc-mixed-claude', 'rpc-mixed-opencode', 'rpc-mixed-cursor')",
                [],
                |row| row.get::<_, i64>(0),
            )?)
        })
        .await
        .expect("outbox count");
    assert_eq!(outbox_rows, 4);
    for (_, command_id, provider, _) in providers {
        let row = engine
            .repositories()
            .get_provider_turn_delivery(command_id.to_owned())
            .await
            .expect("delivery row")
            .expect("one outbox row");
        assert_eq!(row.provider_kind, provider);
        assert_eq!(row.state, TurnDeliveryState::Delivered);
    }

    socket.close(None).await.expect("websocket close");
    runtime.shutdown();
    runtime.join().await.expect("runtime shutdown");
    delivery.shutdown().await;
    supervisor.shutdown().await.expect("provider shutdown");
    engine.shutdown().await;
}

#[tokio::test]
async fn registered_dispatch_rpc_refuses_an_attachment_id_from_another_thread() {
    let engine = engine().await;
    engine
        .dispatch(
            serde_json::from_value(json!({
                "type":"thread.create", "commandId":"thread-2", "threadId":"t2", "projectId":"p1",
                "title":"Other thread", "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                "runtimeMode":"full-access", "branch":null, "worktreePath":null, "createdAt":NOW
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (_events_tx, events_rx) = mpsc::channel(8);
    let (_other_events_tx, other_events_rx) = mpsc::channel(8);
    let supervisor = Arc::new(ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::from([events_rx, other_events_rx])),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    ));
    supervisor.launch(launch()).await.unwrap();
    let settings = TempDir::new().unwrap();
    let mut registry = RpcRegistry::empty();
    let delivery = Arc::new(TurnDeliveryService::start(
        engine.clone(),
        supervisor.clone(),
        settings.path().to_path_buf(),
    ));
    register_orchestration_rpc_with_delivery(
        &mut registry,
        engine.clone(),
        supervisor.clone(),
        settings.path().to_path_buf(),
        delivery.clone(),
        attachment_upload_registry(),
    );
    let handle = ServerRuntime::start_with_registry(test_config(&settings), registry)
        .await
        .unwrap();
    let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .unwrap();
    let turn = |command_id: &str, thread_id: &str, attachment: Value| {
        json!({
            "type":"thread.turn.start", "commandId":command_id, "threadId":thread_id,
            "message":{
                "messageId":format!("message-{command_id}"), "role":"user", "text":"review",
                "attachments":[attachment]
            },
            "createdAt":NOW
        })
    };
    let reference = json!({
        "type":"file", "id":"notes-1", "name":"notes.txt", "mimeType":"text/plain", "sizeBytes":5
    });
    let mut upload = reference.clone();
    upload["dataUrl"] = json!("data:text/plain;base64,bm90ZXM=");

    rpc_request(&mut socket, "801", turn("t1-upload", "t1", upload)).await;
    rpc_response(&mut socket, "801")
        .await
        .expect("the upload is admitted on its own thread");

    // The id exists and its size matches, but another thread's command attached it.
    rpc_request(
        &mut socket,
        "802",
        turn("t2-reuse", "t2", reference.clone()),
    )
    .await;
    let cause = rpc_response(&mut socket, "802")
        .await
        .expect_err("another thread's attachment id is refused");
    assert_eq!(cause[0]["error"]["_tag"], "InvalidRequest", "{cause}");
    assert!(
        cause[0]["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("notes-1")),
        "{cause}"
    );
    assert!(
        engine
            .repositories()
            .get_provider_turn_delivery("t2-reuse".to_owned())
            .await
            .expect("delivery lookup")
            .is_none()
    );
    assert!(
        !engine
            .read_events(0)
            .await
            .expect("events")
            .iter()
            .any(|event| event.event.command_id.as_deref() == Some("t2-reuse")),
        "a refused reference persists nothing"
    );

    // The thread that first received the attachment can still send it again by id.
    rpc_request(&mut socket, "803", turn("t1-reuse", "t1", reference)).await;
    rpc_response(&mut socket, "803")
        .await
        .expect("a thread may reuse its own attachment by id");

    socket.close(None).await.expect("websocket close");
    handle.shutdown();
    handle.join().await.expect("runtime shutdown");
    delivery.shutdown().await;
    supervisor.shutdown().await.expect("provider shutdown");
    engine.shutdown().await;
}

#[tokio::test]
async fn routes_orchestration_commands_and_persists_resume_state() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (_events_tx, events_rx) = mpsc::channel(8);
    let factory = Arc::new(FakeFactory {
        state: state.clone(),
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity_projection(&engine),
        SupervisorOptions::default(),
    );

    supervisor.launch(launch()).await.unwrap();
    supervisor
        .handle_orchestration(OrchestrationCommand::ThreadTurnStart {
            command_id: "turn".to_owned(),
            thread_id: "t1".to_owned(),
            message: ThreadMessageInput {
                message_id: "m1".to_owned(),
                role: "user".to_owned(),
                text: "hello".to_owned(),
                attachments: vec![],
            },
            model_selection: None,
            title_seed: None,
            runtime_mode: "full-access".to_owned(),
            interaction_mode: "default".to_owned(),
            bootstrap: None,
            source_proposed_plan: None,
            queued: None,
            created_at: NOW.to_owned(),
        })
        .await
        .unwrap();
    supervisor.handle_orchestration(serde_json::from_value(json!({"type":"thread.turn.interrupt","commandId":"interrupt","threadId":"t1","turnId":"provider-turn-1","createdAt":NOW})).unwrap()).await.unwrap();
    supervisor.handle_orchestration(serde_json::from_value(json!({"type":"thread.approval.respond","commandId":"approve","threadId":"t1","requestId":"r1","decision":"accept","createdAt":NOW})).unwrap()).await.unwrap();
    supervisor.handle_orchestration(serde_json::from_value(json!({"type":"thread.user-input.respond","commandId":"answer","threadId":"t1","requestId":"r2","answers":{"q":"a"},"createdAt":NOW})).unwrap()).await.unwrap();

    let persisted = engine
        .repositories()
        .get_provider_session_runtime("t1".to_owned())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        persisted.resume_cursor,
        Some(json!({ "threadId": "provider-session-1" }))
    );
    assert_eq!(persisted.status, "ready");
    {
        let state = state.lock().unwrap();
        assert_eq!(state.sends, ["hello"]);
        assert_eq!(state.interrupts, [Some("provider-turn-1".to_owned())]);
        assert_eq!(state.approvals, [("r1".to_owned(), "accept".to_owned())]);
        assert_eq!(state.answers, [("r2".to_owned(), json!({"q":"a"}))]);
    }
    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn opencode_launch_uses_durable_activity_revision_as_reconciliation_seed() {
    let engine = engine().await;
    let activity = activity_projection(&engine);
    let scope = ActivityScopeSeed::thread(
        "thread:t1",
        "t1",
        "opencode",
        Some("opencode"),
        ActivityCapabilities::none(),
    )
    .expect("scope");
    activity
        .ensure_scope(scope.clone())
        .await
        .expect("ensure scope");
    activity
        .apply(
            &scope.scope_id,
            "test:opencode:stale-before-launch".to_owned(),
            vec![ProviderActivityMutation::SetScope {
                capabilities: ActivityCapabilities::none(),
                observation_state: ActivityObservationState::Stale,
            }],
            NOW.to_owned(),
        )
        .await
        .expect("stale durable scope");
    let durable_revision = activity
        .snapshot(&scope.scope)
        .await
        .expect("durable snapshot")
        .revision;

    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (_events_tx, events_rx) = mpsc::channel(8);
    let factory = Arc::new(FakeFactory {
        state: state.clone(),
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity,
        SupervisorOptions::default(),
    );
    let mut request = launch();
    request.provider = "opencode".to_owned();
    request.provider_instance_id = Some("opencode".to_owned());
    supervisor.launch(request).await.expect("launch");

    assert_eq!(
        state
            .lock()
            .unwrap()
            .launches
            .first()
            .expect("captured launch")
            .activity_causal_revision,
        durable_revision
    );
    supervisor.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn opencode_launch_does_not_reuse_initial_seed_when_activity_snapshot_is_invalid() {
    let (engine, database) = engine_and_database().await;
    let activity = activity_projection(&engine);
    let scope = ActivityScopeSeed::thread(
        "thread:t1",
        "t1",
        "opencode",
        Some("opencode"),
        ActivityCapabilities::none(),
    )
    .expect("scope");
    activity
        .ensure_scope(scope.clone())
        .await
        .expect("ensure scope");
    database
        .call(move |connection| {
            connection.execute(
                "UPDATE activity_scopes
                 SET capabilities_json = '{'
                 WHERE scope_id = ?",
                [&scope.scope_id],
            )?;
            Ok(())
        })
        .await
        .expect("corrupt activity snapshot fixture");

    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (_events_tx, events_rx) = mpsc::channel(8);
    let factory = Arc::new(FakeFactory {
        state: state.clone(),
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor =
        ProviderRuntimeSupervisor::start(engine, factory, activity, SupervisorOptions::default());
    let mut request = launch();
    request.provider = "opencode".to_owned();
    request.provider_instance_id = Some("opencode".to_owned());

    let error = supervisor
        .launch(request)
        .await
        .expect_err("invalid durable activity state must fail before driver creation");
    assert!(
        matches!(error, ProviderRuntimeError::Persistence(_)),
        "unexpected launch error: {error}"
    );
    assert!(
        state.lock().unwrap().launches.is_empty(),
        "the driver must not receive an unsafe initial reconciliation seed"
    );
    supervisor.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn activity_only_provider_events_project_graph_mutations_without_root_payloads() {
    let (engine, database) = engine_and_database().await;
    let activity = ActivityProjection::new(ActivityRepository::new(database));
    let state = Arc::new(StdMutex::new(DriverState {
        start_results: VecDeque::from([Ok(StartedSession {
            resume_cursor: Some(json!({"sessionId":"native-1"})),
            runtime_payload: None,
            activity_capabilities: ActivityCapabilities::structured_full(false),
        })]),
        ..DriverState::default()
    }));
    let (events_tx, events_rx) = mpsc::channel(2);
    let factory = Arc::new(FakeFactory {
        state,
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity.clone(),
        SupervisorOptions::default(),
    );

    supervisor.launch(launch()).await.unwrap();

    let scope = ActivityScopeRef::Thread {
        thread_id: "t1".to_owned(),
    };
    let launched = activity
        .snapshot(&scope)
        .await
        .expect("launch creates the provider activity scope");
    assert_eq!(launched.scope_id, "thread:t1");
    assert_eq!(launched.revision, 0);
    assert_eq!(
        launched.capabilities,
        ActivityCapabilities::structured_full(false)
    );

    let event = ProviderEvent {
        native_event_id: native_event_id("native:event:1"),
        event_type: "activity.native".to_owned(),
        thread_id: "t1".to_owned(),
        turn_id: None,
        item_id: None,
        request_id: None,
        payload: json!({}),
        activity: vec![
            ProviderActivityMutation::upsert_actor("actor:child", None, "Child", "running")
                .expect("valid actor mutation"),
        ],
        activity_controls: Default::default(),
    };
    events_tx.send(event.clone()).await.unwrap();

    let first_revision = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = activity.snapshot(&scope).await.unwrap();
            if snapshot.revision == 1 && snapshot.actors.len() == 1 {
                break snapshot.revision;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("provider activity mutation is projected");

    events_tx.send(event).await.unwrap();
    events_tx
        .send(ProviderEvent {
            native_event_id: native_event_id("native:event:root-with-activity"),
            event_type: "pump.barrier".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: None,
            item_id: None,
            request_id: None,
            payload: json!({"visible":true}),
            activity: vec![
                ProviderActivityMutation::upsert_actor(
                    "actor:root-carried",
                    None,
                    "Root-carried mutation",
                    "running",
                )
                .expect("valid root-carried actor mutation"),
            ],
            activity_controls: Default::default(),
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = load_snapshot(&engine.repositories()).await.unwrap();
            if snapshot.activities.iter().any(|entry| {
                entry.thread_id == "t1" && entry.payload["eventType"] == "pump.barrier"
            }) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("pump processes the event after the activity-only replay");

    let replayed = activity.snapshot(&scope).await.unwrap();
    assert_eq!(replayed.revision, first_revision + 1);
    assert_eq!(replayed.actors.len(), 2);
    assert!(
        replayed
            .actors
            .iter()
            .any(|actor| actor.id == "actor:child")
    );
    assert!(
        replayed
            .actors
            .iter()
            .any(|actor| actor.id == "actor:root-carried")
    );
    let root = load_snapshot(&engine.repositories()).await.unwrap();
    assert!(
        root.activities
            .iter()
            .all(|entry| entry.payload["eventType"] != "activity.native"),
        "activity-only provider events must not append root thread activity"
    );
    assert!(root.messages.is_empty());
    assert!(root.turns.is_empty());
    assert!(root.approvals.is_empty());
    assert!(root.proposed_plans.is_empty());

    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn mcp_complete_snapshots_project_in_order_for_the_selected_provider_instance() {
    let (engine, database) = engine_and_database().await;
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (events_tx, events_rx) = mpsc::channel(4);
    let factory = Arc::new(FakeFactory {
        state,
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        ActivityProjection::new(ActivityRepository::new(database)),
        SupervisorOptions::default(),
    );
    let mut request = launch();
    request.provider_instance_id = Some("codex-work".to_owned());
    supervisor.launch(request).await.unwrap();

    for event in [
        ProviderEvent {
            native_event_id: None,
            event_type: "provider.note".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: None,
            item_id: None,
            request_id: None,
            payload: json!({ "providerInstanceId": "source-owned" }),
            activity: Vec::new(),
            activity_controls: Default::default(),
        },
        ProviderEvent {
            native_event_id: None,
            event_type: "mcp.status.updated".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: None,
            item_id: None,
            request_id: None,
            payload: json!({
                "servers": [{ "name": "old-only", "state": "connected" }]
            }),
            activity: Vec::new(),
            activity_controls: Default::default(),
        },
        ProviderEvent {
            native_event_id: None,
            event_type: "mcp.status.updated".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: None,
            item_id: None,
            request_id: None,
            payload: json!({
                "servers": [
                    { "name": "new-a", "state": "error", "detail": "failed" },
                    { "name": "new-b", "state": "connected" }
                ]
            }),
            activity: Vec::new(),
            activity_controls: Default::default(),
        },
    ] {
        events_tx.send(event).await.unwrap();
    }

    let snapshot = timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = load_snapshot(&engine.repositories()).await.unwrap();
            if snapshot
                .activities
                .iter()
                .filter(|activity| activity.payload["eventType"] == "mcp.status.updated")
                .count()
                == 2
            {
                break snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("both MCP snapshots project");
    let ordinary = snapshot
        .activities
        .iter()
        .find(|activity| activity.payload["eventType"] == "provider.note")
        .expect("ordinary provider activity");
    assert_eq!(
        ordinary.payload,
        json!({ "providerInstanceId": "source-owned", "eventType": "provider.note" })
    );
    let mcp_payloads = snapshot
        .activities
        .iter()
        .filter(|activity| activity.payload["eventType"] == "mcp.status.updated")
        .map(|activity| activity.payload.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        mcp_payloads,
        vec![
            json!({
                "servers": [{ "name": "old-only", "state": "connected" }],
                "providerInstanceId": "codex-work",
                "eventType": "mcp.status.updated"
            }),
            json!({
                "servers": [
                    { "name": "new-a", "state": "error", "detail": "failed" },
                    { "name": "new-b", "state": "connected" }
                ],
                "providerInstanceId": "codex-work",
                "eventType": "mcp.status.updated"
            }),
        ]
    );
    assert_eq!(
        mcp_payloads.last().expect("latest MCP snapshot")["servers"],
        json!([
            { "name": "new-a", "state": "error", "detail": "failed" },
            { "name": "new-b", "state": "connected" }
        ])
    );

    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn load_snapshot_preserves_activity_sequence_when_ids_sort_in_reverse() {
    let (engine, _) = engine_and_database().await;

    for (command_id, activity_id, summary) in [
        ("append-old", "z-old-activity", "old"),
        ("append-new", "a-new-activity", "new"),
    ] {
        engine
            .dispatch(
                serde_json::from_value(json!({
                    "type": "thread.activity.append",
                    "commandId": command_id,
                    "threadId": "t1",
                    "activity": {
                        "id": activity_id,
                        "tone": "neutral",
                        "kind": "ordering.regression",
                        "summary": summary,
                        "payload": { "summary": summary },
                        "createdAt": NOW
                    },
                    "createdAt": NOW
                }))
                .unwrap(),
            )
            .await
            .unwrap();
    }

    let activities = load_snapshot(&engine.repositories())
        .await
        .unwrap()
        .activities
        .into_iter()
        .filter(|activity| activity.kind == "ordering.regression")
        .collect::<Vec<_>>();

    assert_eq!(
        activities
            .iter()
            .map(|activity| activity.summary.as_str())
            .collect::<Vec<_>>(),
        ["old", "new"]
    );
    assert_eq!(
        activities
            .iter()
            .map(|activity| activity.payload["summary"].as_str())
            .collect::<Vec<_>>(),
        [Some("old"), Some("new")]
    );
    assert!(
        activities[0].sequence < activities[1].sequence,
        "event sequence must remain the snapshot order"
    );

    engine.shutdown().await;
}

#[tokio::test]
async fn agent_activity_toggle_keeps_session_ready_and_fences_native_event_generations() {
    let (engine, database) = engine_and_database().await;
    let controller = AgentActivityController::new(true);
    let activity_repository = ActivityRepository::new(database.clone());
    let activity =
        ActivityProjection::with_controller(ActivityRepository::new(database), controller.clone());
    let state = Arc::new(StdMutex::new(DriverState {
        start_results: VecDeque::from([Ok(StartedSession {
            resume_cursor: Some(json!({"sessionId":"native-activity-toggle"})),
            runtime_payload: None,
            activity_capabilities: ActivityCapabilities::structured_full(false),
        })]),
        ..DriverState::default()
    }));
    let (events_tx, events_rx) = mpsc::channel(8);
    let factory = Arc::new(FakeFactory {
        state: state.clone(),
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity.clone(),
        SupervisorOptions::default(),
    );
    supervisor.launch(launch()).await.expect("launch");
    let scope = ActivityScopeRef::Thread {
        thread_id: "t1".to_owned(),
    };

    events_tx
        .send(ProviderEvent {
            native_event_id: native_event_id("native:reused-across-toggle"),
            event_type: "activity.native".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: None,
            item_id: None,
            request_id: None,
            payload: json!({}),
            activity: vec![
                ProviderActivityMutation::upsert_actor(
                    "actor:before-toggle",
                    None,
                    "Before toggle",
                    "running",
                )
                .expect("before mutation"),
            ],
            activity_controls: Default::default(),
        })
        .await
        .expect("initial activity");
    timeout(Duration::from_secs(2), async {
        loop {
            if activity
                .snapshot(&scope)
                .await
                .is_ok_and(|snapshot| snapshot.actors.len() == 1)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("initial activity projects");
    let before_disable = activity.snapshot(&scope).await.expect("before disable");

    controller.disable().await;
    assert_eq!(
        supervisor
            .set_agent_activity_enabled(false)
            .await
            .expect("disable live providers"),
        1
    );
    events_tx
        .send(ProviderEvent {
            native_event_id: native_event_id("native:disabled-only"),
            event_type: "activity.native".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: None,
            item_id: None,
            request_id: None,
            payload: json!({}),
            activity: vec![
                ProviderActivityMutation::upsert_actor(
                    "actor:disabled",
                    None,
                    "Disabled",
                    "running",
                )
                .expect("disabled mutation"),
            ],
            activity_controls: Default::default(),
        })
        .await
        .expect("disabled activity");
    events_tx
        .send(ProviderEvent {
            native_event_id: None,
            event_type: "pump.barrier".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: None,
            item_id: None,
            request_id: None,
            payload: json!({"visible":true}),
            activity: Vec::new(),
            activity_controls: Default::default(),
        })
        .await
        .expect("normal barrier");
    timeout(Duration::from_secs(2), async {
        loop {
            if load_snapshot(&engine.repositories())
                .await
                .expect("orchestration snapshot")
                .activities
                .iter()
                .any(|entry| {
                    entry.thread_id == "t1" && entry.payload["eventType"] == "pump.barrier"
                })
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("normal provider events continue");
    let disabled = activity_repository
        .snapshot(&scope)
        .await
        .expect("disabled durable snapshot");
    assert_eq!(disabled.revision, before_disable.revision);
    assert!(
        disabled
            .actors
            .iter()
            .all(|actor| actor.id != "actor:disabled")
    );
    assert_eq!(
        load_snapshot(&engine.repositories())
            .await
            .expect("session snapshot")
            .sessions
            .iter()
            .find(|session| session.thread_id == "t1")
            .expect("live session")
            .status,
        "ready"
    );

    controller.enable();
    assert_eq!(
        supervisor
            .set_agent_activity_enabled(true)
            .await
            .expect("enable live providers"),
        1
    );
    events_tx
        .send(ProviderEvent {
            native_event_id: native_event_id("native:reused-across-toggle"),
            event_type: "activity.native".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: None,
            item_id: None,
            request_id: None,
            payload: json!({}),
            activity: vec![
                ProviderActivityMutation::upsert_actor(
                    "actor:after-toggle",
                    None,
                    "After toggle",
                    "running",
                )
                .expect("resumed mutation"),
            ],
            activity_controls: Default::default(),
        })
        .await
        .expect("resumed activity");
    timeout(Duration::from_secs(2), async {
        loop {
            if activity.snapshot(&scope).await.is_ok_and(|snapshot| {
                snapshot
                    .actors
                    .iter()
                    .any(|actor| actor.id == "actor:after-toggle")
            }) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("new-generation activity projects");
    let transitions = state.lock().unwrap().agent_activity_transitions.clone();
    assert_eq!(
        transitions.iter().filter(|enabled| !**enabled).count(),
        1,
        "disable reaches the live driver exactly once"
    );
    assert_eq!(transitions.last(), Some(&true));

    supervisor.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn agent_activity_toggle_continues_after_failure_and_bounds_the_first_error() {
    let (engine, database) = engine_and_database().await;
    engine
        .dispatch(
            serde_json::from_value(json!({
                "type":"thread.create", "commandId":"thread-2", "threadId":"t2", "projectId":"p1",
                "title":"Thread 2", "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                "runtimeMode":"full-access", "branch":null, "worktreePath":null, "createdAt":NOW
            }))
            .expect("thread command"),
        )
        .await
        .expect("second thread");
    let controller = AgentActivityController::new(true);
    let activity =
        ActivityProjection::with_controller(ActivityRepository::new(database), controller.clone());
    let state = Arc::new(StdMutex::new(DriverState {
        start_results: VecDeque::from([
            Ok(started_session("provider-t1")),
            Ok(started_session("provider-t2")),
        ]),
        ..DriverState::default()
    }));
    let (_first_tx, first_rx) = mpsc::channel(1);
    let (_second_tx, second_rx) = mpsc::channel(1);
    let factory = Arc::new(FakeFactory {
        state: state.clone(),
        events: StdMutex::new(VecDeque::from([first_rx, second_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity,
        SupervisorOptions::default(),
    );
    supervisor.launch(launch()).await.expect("first launch");
    let mut second_launch = launch();
    second_launch.thread_id = "t2".to_owned();
    supervisor
        .launch(second_launch)
        .await
        .expect("second launch");

    let sensitive_payload = format!(
        "secret-token=\u{0007}{}",
        "provider-controlled-content".repeat(128)
    );
    {
        let mut state = state.lock().unwrap();
        state.agent_activity_transitions.clear();
        state.agent_activity_results = VecDeque::from([
            Err(ProviderRuntimeError::Provider {
                provider: "codex\ninjected".to_owned(),
                detail: sensitive_payload.clone(),
            }),
            Ok(()),
        ]);
    }
    controller.disable().await;
    let error = supervisor
        .set_agent_activity_enabled(false)
        .await
        .expect_err("first transition failure is returned");
    let rendered = error.to_string();
    assert!(
        rendered.len() <= 256,
        "transition errors stay bounded: {} bytes",
        rendered.len()
    );
    assert!(
        !rendered.contains("secret-token")
            && !rendered.contains("provider-controlled-content")
            && !rendered.chars().any(char::is_control),
        "provider-controlled error content must not escape: {rendered:?}"
    );
    assert_eq!(
        state.lock().unwrap().agent_activity_transitions,
        [false, false],
        "the second live driver transitions after the first one fails"
    );

    supervisor.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn launch_rechecks_activity_state_after_factory_creation_before_start() {
    let (engine, database) = engine_and_database().await;
    let controller = AgentActivityController::new(true);
    let activity =
        ActivityProjection::with_controller(ActivityRepository::new(database), controller.clone());
    let state = Arc::new(StdMutex::new(DriverState {
        start_results: VecDeque::from([Ok(started_session("provider-launch-race"))]),
        ..DriverState::default()
    }));
    let (_events_tx, events_rx) = mpsc::channel(1);
    let factory = Arc::new(LaunchRaceFactory {
        state: state.clone(),
        events: StdMutex::new(VecDeque::from([events_rx])),
        controller: controller.clone(),
    });
    let supervisor =
        ProviderRuntimeSupervisor::start(engine, factory, activity, SupervisorOptions::default());

    supervisor.launch(launch()).await.expect("launch");
    let state = state.lock().unwrap();
    assert_eq!(
        state.agent_activity_transitions,
        [false],
        "the launch fence observes the state changed during factory creation"
    );
    assert_eq!(state.starts, 1, "the provider still starts normally");
    drop(state);

    supervisor.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn claude_transcript_recovery_event_persists_and_reloads_activity() {
    let (engine, database) = engine_and_database().await;
    let activity = ActivityProjection::new(ActivityRepository::new(database.clone()));
    let state = Arc::new(StdMutex::new(DriverState {
        start_results: VecDeque::from([Ok(StartedSession {
            resume_cursor: Some(json!({"sessionId":"claude-recovery-session"})),
            runtime_payload: None,
            activity_capabilities: ActivityCapabilities::structured_full(false),
        })]),
        ..DriverState::default()
    }));
    let (events_tx, events_rx) = mpsc::channel(2);
    let factory = Arc::new(FakeFactory {
        state,
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine,
        factory,
        activity.clone(),
        SupervisorOptions::default(),
    );
    let mut request = launch();
    request.provider = "claudeAgent".to_owned();
    request.provider_label = "Claude".to_owned();
    supervisor.launch(request).await.unwrap();

    events_tx
        .send(ProviderEvent {
            native_event_id: native_event_id("claude:hook:agent-reload:start"),
            event_type: "activity.native".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: None,
            item_id: None,
            request_id: None,
            payload: json!({}),
            activity: vec![
                ProviderActivityMutation::upsert_actor(
                    "claude:agent:agent-reload",
                    None,
                    "Explore",
                    "running",
                )
                .expect("valid recovered entry owner"),
            ],
            activity_controls: Default::default(),
        })
        .await
        .unwrap();
    events_tx
        .send(ProviderEvent {
            native_event_id: native_event_id("claude:recovery:durable-fixture"),
            event_type: "activity.native".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: None,
            item_id: None,
            request_id: None,
            payload: json!({}),
            activity: vec![
                ProviderActivityMutation::AppendEntry(
                    ActivityEntry::try_new(
                        "claude:event:commentary:durable-fixture",
                        ActivityRecordKind::Actor,
                        "claude:agent:agent-reload",
                        ActivityEntryKind::Commentary,
                        "Commentary",
                        Some("Recovered after restart"),
                        ActivityEntryTone::Info,
                        "2026-07-24T12:00:00Z",
                    )
                    .expect("valid recovered entry"),
                ),
                ProviderActivityMutation::SetScope {
                    capabilities: ActivityCapabilities {
                        actors: true,
                        attributed_activity: true,
                        background_work: true,
                        history_recovery: ActivityHistoryRecovery::Bounded,
                        terminal_observation: false,
                        targeted_actor_cancellation: false,
                    },
                    observation_state: ActivityObservationState::Live,
                },
            ],
            activity_controls: Default::default(),
        })
        .await
        .unwrap();

    let scope = ActivityScopeRef::Thread {
        thread_id: "t1".to_owned(),
    };
    timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = activity.snapshot(&scope).await.unwrap();
            if snapshot.revision == 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("recovered activity event is projected");

    let reloaded_repository = ActivityRepository::new(database);
    let reloaded = reloaded_repository
        .snapshot(&scope)
        .await
        .expect("recovered activity reloads from a fresh repository");
    let detail = reloaded_repository
        .list_detail(
            &scope,
            "thread:t1",
            ActivityRecordKind::Actor,
            "claude:agent:agent-reload",
            None,
            10,
        )
        .await
        .expect("recovered actor detail reloads");
    assert!(detail.entries.iter().any(|entry| {
        entry.id == "claude:event:commentary:durable-fixture"
            && entry.detail.as_deref() == Some("Recovered after restart")
    }));
    assert_eq!(
        reloaded.capabilities.history_recovery,
        ActivityHistoryRecovery::Bounded
    );

    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn unexpected_provider_stream_end_marks_activity_scope_stale() {
    let (engine, database) = engine_and_database().await;
    let activity = ActivityProjection::new(ActivityRepository::new(database));
    let state = Arc::new(StdMutex::new(DriverState {
        start_results: VecDeque::from([Ok(StartedSession {
            resume_cursor: Some(json!({"sessionId":"native-1"})),
            runtime_payload: None,
            activity_capabilities: ActivityCapabilities::structured_full(false),
        })]),
        ..DriverState::default()
    }));
    let (events_tx, events_rx) = mpsc::channel(1);
    let factory = Arc::new(FakeFactory {
        state,
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine,
        factory,
        activity.clone(),
        SupervisorOptions::default(),
    );
    supervisor.launch(launch()).await.unwrap();

    drop(events_tx);

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = activity
                .snapshot(&ActivityScopeRef::Thread {
                    thread_id: "t1".to_owned(),
                })
                .await
                .unwrap();
            if snapshot.observation_state == ActivityObservationState::Stale {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("unexpected provider stream end marks the activity scope stale");

    supervisor.shutdown().await.unwrap();
}

#[derive(Clone, Copy)]
enum ProviderLossSignal {
    StreamEnd,
    SessionExit,
    FailedTurnThenSessionExit,
}

#[tokio::test]
async fn provider_stream_end_relaunches_the_next_start_but_never_steer() {
    assert_provider_loss_relaunch(ProviderLossSignal::StreamEnd).await;
}

#[tokio::test]
async fn provider_session_exit_relaunches_with_the_event_sender_still_open() {
    assert_provider_loss_relaunch(ProviderLossSignal::FailedTurnThenSessionExit).await;
}

#[tokio::test]
async fn provider_session_exit_settles_an_abandoned_active_turn_before_relaunch() {
    assert_provider_loss_relaunch(ProviderLossSignal::SessionExit).await;
}

#[tokio::test]
async fn provider_session_exit_does_not_treat_opencode_explicit_stop_as_loss() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (events_tx, events_rx) = mpsc::channel(1);
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::from([events_rx])),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    supervisor
        .launch(launch_for_provider("t1", "opencode", "openai/gpt-5"))
        .await
        .unwrap();
    events_tx
        .send(ProviderEvent {
            native_event_id: None,
            event_type: "session.exited".into(),
            thread_id: "t1".into(),
            turn_id: None,
            item_id: None,
            request_id: None,
            payload: json!({"reason":"Session stopped."}),
            activity: Vec::new(),
            activity_controls: Default::default(),
        })
        .await
        .unwrap();
    timeout(Duration::from_secs(2), async {
        loop {
            if load_snapshot(&engine.repositories())
                .await
                .unwrap()
                .activities
                .iter()
                .any(|activity| activity.payload["eventType"] == "session.exited")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    supervisor
        .capture_session_identity("t1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        state.lock().unwrap().shutdowns,
        0,
        "explicit stop is not an unexpected connection loss"
    );
    supervisor.shutdown().await.unwrap();
    assert_eq!(state.lock().unwrap().shutdowns, 1);
    engine.shutdown().await;
}

async fn assert_provider_loss_relaunch(signal: ProviderLossSignal) {
    const CLOSED: &str = "Codex JSON-RPC connection is closed: stdout ended";
    for (durable, draining) in [(false, false), (false, true), (true, false), (true, true)] {
        let (engine, database) = engine_and_database().await;
        let settings = TempDir::new().unwrap();
        let state = Arc::new(StdMutex::new(DriverState {
            shutdown_gate: draining.then(|| Arc::new(tokio::sync::Semaphore::new(0))),
            shutdown_results: VecDeque::from([
                Ok(()),
                Err(ProviderRuntimeError::Provider {
                    provider: "codex".into(),
                    detail: "already-dead driver shutdown failed".into(),
                }),
            ]),
            ..DriverState::default()
        }));
        let (events_tx, events_rx) = mpsc::channel(1);
        let (_replacement_tx, replacement_rx) = mpsc::channel(1);
        let supervisor = ProviderRuntimeSupervisor::start(
            engine.clone(),
            Arc::new(FakeFactory {
                state: state.clone(),
                events: StdMutex::new(VecDeque::from([events_rx, replacement_rx])),
            }),
            activity_projection(&engine),
            SupervisorOptions::default(),
        );
        supervisor.launch(launch()).await.unwrap();
        let abandoned = durable_turn_command("before-provider-loss", "first turn");
        engine.dispatch(abandoned.clone()).await.unwrap();
        supervisor.handle_orchestration(abandoned).await.unwrap();
        let retained_sender = match signal {
            ProviderLossSignal::StreamEnd => {
                drop(events_tx);
                None
            }
            ProviderLossSignal::SessionExit | ProviderLossSignal::FailedTurnThenSessionExit => {
                let event =
                    |event_type: &str, turn_id: Option<&str>, payload: Value| ProviderEvent {
                        native_event_id: None,
                        event_type: event_type.into(),
                        thread_id: "t1".into(),
                        turn_id: turn_id.map(str::to_owned),
                        item_id: None,
                        request_id: None,
                        payload,
                        activity: Vec::new(),
                        activity_controls: Default::default(),
                    };
                if matches!(signal, ProviderLossSignal::FailedTurnThenSessionExit) {
                    events_tx
                        .send(event(
                            "turn.completed",
                            Some("provider-turn-1"),
                            json!({
                                "state":"failed", "errorMessage":CLOSED
                            }),
                        ))
                        .await
                        .unwrap();
                }
                events_tx
                    .send(event("session.exited", None, json!({"reason":CLOSED})))
                    .await
                    .unwrap();
                Some(events_tx)
            }
        };
        let expected_error = match signal {
            ProviderLossSignal::StreamEnd => "Provider event stream ended unexpectedly.".to_owned(),
            ProviderLossSignal::SessionExit => {
                format!("Provider event stream ended unexpectedly: {CLOSED}")
            }
            ProviderLossSignal::FailedTurnThenSessionExit => CLOSED.to_owned(),
        };
        let expected_class = if matches!(signal, ProviderLossSignal::FailedTurnThenSessionExit) {
            "provider_error"
        } else {
            "transport_error"
        };
        timeout(Duration::from_secs(2), async {
            loop {
                let session = engine
                    .repositories()
                    .get_thread_session("t1".into())
                    .await
                    .unwrap()
                    .unwrap();
                if session.status == "error" && state.lock().unwrap().shutdowns > 0 {
                    assert_eq!(session.last_error.as_deref(), Some(expected_error.as_str()));
                    assert_eq!(session.last_error_class.as_deref(), Some(expected_class));
                    assert_eq!(session.active_turn_id, None);
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("provider loss settles the active turn");
        assert!(
            retained_sender
                .as_ref()
                .is_none_or(|sender| !sender.is_closed()),
            "process loss must be detected while the driver's event channel stays open"
        );
        let events = engine.read_events(0).await.unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event.event_type == "thread.session-set"
                    && event.event.payload["session"]["status"] == "error")
                .count(),
            1,
            "provider loss must not project a second failure after the provider settles"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event.event_type == "thread.activity-appended"
                    && event.event.payload["activity"]["payload"]["eventType"] == "session.exited")
                .count(),
            usize::from(retained_sender.is_some()),
            "session.exited remains projected exactly once"
        );

        if durable {
            let mut steer = delivery_row("codex", "steer-after-provider-loss");
            let command = freeze_row_route(&engine, &settings, &mut steer).await;
            seed_sending_delivery(&database, steer.clone()).await;
            database
                .call(move |connection| {
                    connection.execute(
                        "UPDATE provider_turn_outbox SET mode = 'steer' WHERE command_id = ?",
                        [&steer.command_id],
                    )?;
                    Ok(())
                })
                .await
                .unwrap();
            let outcome = deliver_durable_orchestration_turn(
                &supervisor,
                &engine,
                &settings.path().to_path_buf(),
                command,
                steer.delivery_key,
            )
            .await;
            assert!(
                matches!(outcome, ProviderDeliveryOutcome::Rejected { ref detail }
                    if detail == "Steering requires an existing running provider session."),
                "{outcome:?}"
            );
            assert_eq!(
                state.lock().unwrap().launches.len(),
                1,
                "steer must never relaunch"
            );
        }

        let command = durable_turn_command("after-provider-loss", "continue after loss");
        let outcome = if durable {
            let mut row = delivery_row("codex", "start-after-provider-loss");
            row.command_id = "after-provider-loss".into();
            row.provider_session_id = Some("provider-session-1".into());
            row.payload = serde_json::to_value(&command).unwrap();
            let command = freeze_row_route(&engine, &settings, &mut row).await;
            seed_sending_delivery(&database, row.clone()).await;
            deliver_durable_orchestration_turn(
                &supervisor,
                &engine,
                &settings.path().to_path_buf(),
                command,
                row.delivery_key,
            )
            .await
        } else {
            deliver_orchestration_turn(
                &supervisor,
                &engine,
                &settings.path().to_path_buf(),
                command,
                "start-after-provider-loss".into(),
            )
            .await
        };
        assert!(
            matches!(outcome, ProviderDeliveryOutcome::Accepted { .. }),
            "{outcome:?}"
        );
        {
            let state = state.lock().unwrap();
            assert_eq!(state.launches.len(), 2, "a dead driver must be replaced");
            assert_eq!(state.starts, 2);
            assert_eq!(
                state.launches[1].resume_cursor,
                Some(json!({"threadId":"provider-session-1"})),
                "replacement must retain native conversation context"
            );
            assert_eq!(state.sends, ["first turn", "continue after loss"]);
        }
        let session = engine
            .repositories()
            .get_thread_session("t1".into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session.status, "running");
        assert_eq!(session.last_error, None);
        supervisor.shutdown().await.unwrap();
        engine.shutdown().await;
    }
}

#[tokio::test]
async fn provider_stream_end_fences_starts_during_error_projection() {
    let hooks = TestHooks::default();
    let (engine, _) = engine_and_database_with_options(EngineOptions {
        test_hooks: hooks.clone(),
        ..EngineOptions::default()
    })
    .await;
    let (events_tx, events_rx) = mpsc::channel(1);
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::from([events_rx])),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    supervisor.launch(launch()).await.unwrap();
    let abandoned = durable_turn_command("before-projection-race", "first turn");
    engine.dispatch(abandoned.clone()).await.unwrap();
    supervisor.handle_orchestration(abandoned).await.unwrap();
    let session_pause = hooks.pause_before_next_command_persist();
    drop(events_tx);
    timeout(Duration::from_secs(2), session_pause.wait_until_entered())
        .await
        .unwrap();
    let terminal_pause = hooks.pause_before_next_command_persist();
    session_pause.release();
    timeout(Duration::from_secs(2), terminal_pause.wait_until_entered())
        .await
        .unwrap();
    let session = engine
        .repositories()
        .get_thread_session("t1".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        session.status, "error",
        "error is visible while EOF projection is still settling"
    );
    assert_eq!(state.lock().unwrap().shutdowns, 0);

    let delivery = supervisor.deliver_turn(
        durable_turn_command("during-error-projection", "continue"),
        "projection-race-key".into(),
    );
    tokio::pin!(delivery);
    let early = timeout(Duration::from_millis(100), &mut delivery).await;
    terminal_pause.release();
    let result = match early {
        Ok(result) => result,
        Err(_) => timeout(Duration::from_secs(2), delivery).await.unwrap(),
    };
    let missing = matches!(result, Err(ProviderRuntimeError::SessionNotFound { .. }));
    if let Ok(handle) = result {
        let _ = handle.completion().await;
    }
    assert!(
        missing,
        "a start observing the EOF error must reach the relaunch path"
    );
    assert_eq!(state.lock().unwrap().sends, ["first turn"]);
    supervisor.shutdown().await.unwrap();
    engine.shutdown().await;
}

#[tokio::test]
async fn provider_session_exit_fences_starts_until_its_projection_finishes() {
    let hooks = TestHooks::default();
    let (engine, _) = engine_and_database_with_options(EngineOptions {
        test_hooks: hooks.clone(),
        ..EngineOptions::default()
    })
    .await;
    let (events_tx, events_rx) = mpsc::channel(1);
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::from([events_rx])),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    supervisor.launch(launch()).await.unwrap();
    let abandoned = durable_turn_command("before-exit-projection", "first turn");
    engine.dispatch(abandoned.clone()).await.unwrap();
    supervisor.handle_orchestration(abandoned).await.unwrap();
    let exit_pause = hooks.pause_before_next_command_persist();
    events_tx
        .send(ProviderEvent {
            native_event_id: None,
            event_type: "session.exited".into(),
            thread_id: "t1".into(),
            turn_id: None,
            item_id: None,
            request_id: None,
            payload: json!({"reason":"provider process exited"}),
            activity: Vec::new(),
            activity_controls: Default::default(),
        })
        .await
        .unwrap();
    timeout(Duration::from_secs(2), exit_pause.wait_until_entered())
        .await
        .unwrap();
    let delivery = supervisor.deliver_turn(
        durable_turn_command("during-exit-projection", "continue"),
        "exit-projection-key".into(),
    );
    tokio::pin!(delivery);
    let early = timeout(Duration::from_millis(100), &mut delivery).await;
    exit_pause.release();
    let result = match early {
        Ok(result) => result,
        Err(_) => timeout(Duration::from_secs(2), delivery).await.unwrap(),
    };
    let missing = matches!(result, Err(ProviderRuntimeError::SessionNotFound { .. }));
    if let Ok(handle) = result {
        let _ = handle.completion().await;
    }
    supervisor.shutdown().await.unwrap();
    engine.shutdown().await;
    assert!(
        missing,
        "session.exited projection must fence delivery before cleanup and relaunch"
    );
    assert_eq!(state.lock().unwrap().sends, ["first turn"]);
}

#[tokio::test]
async fn provider_stream_end_fence_timeout_releases_other_thread_control() {
    let hooks = TestHooks::default();
    let (engine, database) = engine_and_database_with_options(EngineOptions {
        test_hooks: hooks.clone(),
        ..EngineOptions::default()
    })
    .await;
    add_delivery_thread_for(&engine, "t2", "codex", "gpt-5").await;
    let (events_tx, events_rx) = mpsc::channel(1);
    let (_other_tx, other_rx) = mpsc::channel(1);
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let supervisor = Arc::new(ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::from([events_rx, other_rx])),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    ));
    supervisor.launch(launch()).await.unwrap();
    supervisor
        .launch(launch_for_provider("t2", "codex", "gpt-5"))
        .await
        .unwrap();
    let abandoned = durable_turn_command("before-stuck-projection", "first turn");
    engine.dispatch(abandoned.clone()).await.unwrap();
    supervisor.handle_orchestration(abandoned).await.unwrap();
    for message_id in ["timeout-partial-1", "timeout-partial-2"] {
        engine.dispatch(serde_json::from_value(json!({
            "type":"thread.message.assistant.delta", "commandId":message_id, "threadId":"t1",
            "messageId":message_id, "delta":"Partial response", "turnId":"provider-turn-1", "createdAt":NOW
        })).unwrap()).await.unwrap();
    }
    let session_pause = hooks.pause_before_next_command_persist();
    drop(events_tx);
    timeout(Duration::from_secs(2), session_pause.wait_until_entered())
        .await
        .unwrap();
    let terminal_pause = hooks.pause_before_next_command_persist();
    session_pause.release();
    timeout(Duration::from_secs(2), terminal_pause.wait_until_entered())
        .await
        .unwrap();
    assert_eq!(
        engine
            .repositories()
            .get_thread_session("t1".into())
            .await
            .unwrap()
            .unwrap()
            .status,
        "error"
    );
    let control_supervisor = supervisor.clone();
    let mut control =
        tokio::spawn(async move { control_supervisor.capture_session_identity("t2").await });
    let waiting = timeout(Duration::from_millis(100), &mut control)
        .await
        .is_err();
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(5)).await;
    let bounded_result = if waiting {
        timeout(Duration::from_secs(1), &mut control).await.ok()
    } else {
        None
    };
    tokio::time::resume();
    let start = supervisor
        .deliver_turn(
            durable_turn_command("during-timed-out-projection", "retry once settled"),
            "timed-out-projection-start".into(),
        )
        .await;
    let start_outcome = match start {
        Ok(handle) => Some(handle.completion().await),
        Err(_) => None,
    };
    let mut steer = delivery_row("codex", "timed-out-projection-steer");
    steer.mode = bibcode_server::orchestration::TurnDeliveryMode::Steer;
    let command = serde_json::from_value(steer.payload.clone()).unwrap();
    seed_sending_delivery(&database, steer.clone()).await;
    database
        .call(move |connection| {
            connection.execute(
                "UPDATE provider_turn_outbox SET mode = 'steer' WHERE command_id = ?",
                [&steer.command_id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let settings = TempDir::new().unwrap();
    let steer_outcome = deliver_durable_orchestration_turn(
        &supervisor,
        &engine,
        &settings.path().to_path_buf(),
        command,
        steer.delivery_key,
    )
    .await;
    terminal_pause.release();
    if waiting && bounded_result.is_none() {
        control.await.unwrap().unwrap();
    }
    let settled = timeout(Duration::from_secs(2), async {
        loop {
            let messages = engine
                .repositories()
                .list_messages_by_thread("t1".into())
                .await
                .unwrap();
            if messages
                .iter()
                .filter(|message| message.role == "assistant" && !message.is_streaming)
                .count()
                == 2
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    supervisor.shutdown().await.unwrap();
    engine.shutdown().await;
    assert!(waiting, "terminal projection is fenced before the deadline");
    assert!(bounded_result.expect("a stuck EOF projection must release other threads' supervisor traffic after five seconds").unwrap().unwrap().is_some(), "the other thread retains its live provider identity");
    assert!(
        matches!(
            start_outcome,
            Some(ProviderDeliveryOutcome::DefinitelyNotSent { .. })
        ),
        "start must remain retryable without aborting unfinished settlement"
    );
    assert!(
        matches!(steer_outcome, ProviderDeliveryOutcome::Rejected { .. }),
        "steer must be rejected without relaunching or aborting settlement"
    );
    assert!(
        settled.is_ok(),
        "every partial assistant message must settle after persistence resumes"
    );
    assert_eq!(state.lock().unwrap().sends, ["first turn"]);
}

#[tokio::test]
async fn unexpected_provider_stream_end_settles_partial_turn_as_failed() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (events_tx, events_rx) = mpsc::channel(4);
    let factory = Arc::new(FakeFactory {
        state,
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    supervisor.launch(launch()).await.unwrap();
    let start = OrchestrationCommand::ThreadTurnStart {
        command_id: "unexpected-eof-turn".to_owned(),
        thread_id: "t1".to_owned(),
        message: ThreadMessageInput {
            message_id: "unexpected-eof-user".to_owned(),
            role: "user".to_owned(),
            text: "hello".to_owned(),
            attachments: vec![],
        },
        model_selection: None,
        title_seed: None,
        runtime_mode: "full-access".to_owned(),
        interaction_mode: "default".to_owned(),
        bootstrap: None,
        source_proposed_plan: None,
        queued: None,
        created_at: NOW.to_owned(),
    };
    engine.dispatch(start.clone()).await.unwrap();
    supervisor.handle_orchestration(start).await.unwrap();
    events_tx
        .send(ProviderEvent {
            native_event_id: None,
            event_type: "content.delta".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: Some("provider-turn-1".to_owned()),
            item_id: Some("unexpected-eof-partial".to_owned()),
            request_id: None,
            payload: json!({"streamKind":"assistant_text","delta":"Partial response"}),
            activity: Vec::new(),
            activity_controls: Default::default(),
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let streaming = engine
                .repositories()
                .get_message("assistant:t1:item:unexpected-eof-partial".to_owned())
                .await
                .unwrap()
                .is_some_and(|message| message.is_streaming);
            if streaming {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("partial assistant row is persisted before EOF");

    drop(events_tx);

    let snapshot = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = load_snapshot(&engine.repositories()).await.unwrap();
            let settled = snapshot.messages.iter().any(|message| {
                message.message_id == "assistant:t1:item:unexpected-eof-partial"
                    && message.text == "Partial response"
                    && !message.is_streaming
            });
            let failed_session = snapshot.sessions.iter().any(|session| {
                session.thread_id == "t1"
                    && session.status == "error"
                    && session.active_turn_id.is_none()
                    && session
                        .last_error
                        .as_deref()
                        .is_some_and(|error| error.contains("event stream ended unexpectedly"))
            });
            let provider_error = snapshot.activities.iter().any(|activity| {
                activity.thread_id == "t1"
                    && activity.kind == "provider.error"
                    && activity.payload["errorMessage"]
                        == "Provider event stream ended unexpectedly."
                    // The stream dying under BiBCode is our transport, not a
                    // fault the provider reported about the work itself.
                    && activity.payload["errorClass"] == "transport_error"
            });
            let failed_runtime = engine
                .repositories()
                .get_provider_session_runtime("t1".to_owned())
                .await
                .unwrap()
                .is_some_and(|runtime| runtime.status == "error");
            if settled && failed_session && provider_error && failed_runtime {
                break snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("unexpected EOF fails and settles the active partial turn");
    assert_eq!(
        snapshot
            .messages
            .iter()
            .filter(|message| message.role == "assistant")
            .map(|message| (message.message_id.as_str(), message.text.as_str()))
            .collect::<Vec<_>>(),
        vec![(
            "assistant:t1:item:unexpected-eof-partial",
            "Partial response"
        )]
    );

    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn restart_recovers_eof_partial_after_terminal_settlement_retry_exhaustion() {
    const STREAM_END_ERROR: &str = "Provider event stream ended unexpectedly.";
    const MESSAGE_ID: &str = "assistant:t1:item:eof-retry-partial";
    const TURN_ID: &str = "provider-turn-1";

    let state_directory = TempDir::new().expect("state directory");
    let database_path = state_directory.path().join("provider-eof-retry.sqlite3");
    {
        let database = Database::create_new(&database_path)
            .await
            .expect("persistent database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let hooks = TestHooks::default();
        let engine = OrchestrationEngine::start(
            database.clone(),
            EngineOptions {
                test_hooks: hooks.clone(),
                ..EngineOptions::default()
            },
        )
        .await
        .expect("first engine");
        for command in [
            json!({"type":"project.create","commandId":"eof-retry-project","projectId":"p1","title":"Project","workspaceRoot":"C:/repo","createdAt":NOW}),
            json!({"type":"thread.create","commandId":"eof-retry-thread","threadId":"t1","projectId":"p1","title":"Thread","modelSelection":{"instanceId":"codex","model":"gpt-5"},"runtimeMode":"full-access","createdAt":NOW}),
        ] {
            engine
                .dispatch(serde_json::from_value(command).expect("fixture command"))
                .await
                .expect("fixture dispatch");
        }

        let state = Arc::new(StdMutex::new(DriverState::default()));
        let (events_tx, events_rx) = mpsc::channel(4);
        let factory = Arc::new(FakeFactory {
            state,
            events: StdMutex::new(VecDeque::from([events_rx])),
        });
        let supervisor = ProviderRuntimeSupervisor::start(
            engine.clone(),
            factory,
            activity_projection(&engine),
            SupervisorOptions::default(),
        );
        supervisor.launch(launch()).await.unwrap();
        let start = OrchestrationCommand::ThreadTurnStart {
            command_id: "eof-retry-turn".to_owned(),
            thread_id: "t1".to_owned(),
            message: ThreadMessageInput {
                message_id: "eof-retry-user".to_owned(),
                role: "user".to_owned(),
                text: "hello".to_owned(),
                attachments: vec![],
            },
            model_selection: None,
            title_seed: None,
            runtime_mode: "full-access".to_owned(),
            interaction_mode: "default".to_owned(),
            bootstrap: None,
            source_proposed_plan: None,
            queued: None,
            created_at: NOW.to_owned(),
        };
        engine.dispatch(start.clone()).await.unwrap();
        supervisor.handle_orchestration(start).await.unwrap();
        events_tx
            .send(ProviderEvent {
                native_event_id: None,
                event_type: "content.delta".to_owned(),
                thread_id: "t1".to_owned(),
                turn_id: Some(TURN_ID.to_owned()),
                item_id: Some("eof-retry-partial".to_owned()),
                request_id: None,
                payload: json!({"streamKind":"assistant_text","delta":"Exact partial response"}),
                activity: Vec::new(),
                activity_controls: Default::default(),
            })
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let streaming = engine
                    .repositories()
                    .get_message(MESSAGE_ID.to_owned())
                    .await
                    .unwrap()
                    .is_some_and(|message| message.is_streaming);
                if streaming {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("partial assistant row is durable before EOF");

        hooks.fail_next_projectors("projection.thread-messages", Some("thread.message-sent"), 2);
        drop(events_tx);

        let snapshot = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let snapshot = load_snapshot(&engine.repositories()).await.unwrap();
                let session_failed = snapshot.sessions.iter().any(|session| {
                    session.thread_id == "t1"
                        && session.status == "error"
                        && session.active_turn_id.is_none()
                        && session.last_error.as_deref() == Some(STREAM_END_ERROR)
                });
                let provider_error = snapshot.activities.iter().any(|activity| {
                    activity.thread_id == "t1"
                        && activity.kind == "provider.error"
                        && activity.payload["errorMessage"] == STREAM_END_ERROR
                        && activity.payload["errorClass"] == "transport_error"
                });
                let runtime_failed = engine
                    .repositories()
                    .get_provider_session_runtime("t1".to_owned())
                    .await
                    .unwrap()
                    .is_some_and(|runtime| runtime.status == "error");
                let remains_streaming = snapshot.messages.iter().any(|message| {
                    message.message_id == MESSAGE_ID
                        && message.turn_id.as_deref() == Some(TURN_ID)
                        && message.text == "Exact partial response"
                        && message.is_streaming
                });
                if session_failed && provider_error && runtime_failed && remains_streaming {
                    break snapshot;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("EOF lifecycle persists after both settlement attempts fail");
        assert_eq!(
            snapshot
                .messages
                .iter()
                .filter(|message| message.role == "assistant")
                .map(|message| {
                    (
                        message.message_id.as_str(),
                        message.turn_id.as_deref(),
                        message.text.as_str(),
                        message.is_streaming,
                    )
                })
                .collect::<Vec<_>>(),
            vec![(MESSAGE_ID, Some(TURN_ID), "Exact partial response", true,)]
        );

        let failed_runtime = engine
            .repositories()
            .get_provider_session_runtime("t1".to_owned())
            .await
            .unwrap()
            .expect("EOF runtime is durable before process restart");
        assert_eq!(failed_runtime.status, "error");

        supervisor.shutdown().await.unwrap();
        // Graceful supervisor shutdown rewrites its runtime row as `suspended`.
        // Reinsert the already-observed durable EOF state to model an abrupt
        // process exit while still joining the test worker cleanly.
        engine
            .repositories()
            .upsert_provider_session_runtime(failed_runtime)
            .await
            .expect("preserve crash-time EOF runtime state");
        engine.shutdown().await;
        database
            .checkpoint_wal()
            .await
            .expect("checkpoint before restart");
    }

    let database = Database::open_existing(&database_path)
        .await
        .expect("reopened database");
    let engine = OrchestrationEngine::start(database, EngineOptions::default())
        .await
        .expect("restarted engine");
    let event_count_before_recovery = engine.read_events(0).await.unwrap().len();
    reconcile_abandoned_provider_sessions(&engine)
        .await
        .expect("startup reconciliation");

    let snapshot = load_snapshot(&engine.repositories()).await.unwrap();
    assert_eq!(
        snapshot
            .messages
            .iter()
            .filter(|message| message.role == "assistant")
            .map(|message| {
                (
                    message.message_id.as_str(),
                    message.turn_id.as_deref(),
                    message.text.as_str(),
                    message.is_streaming,
                )
            })
            .collect::<Vec<_>>(),
        vec![(MESSAGE_ID, Some(TURN_ID), "Exact partial response", false,)]
    );
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.thread_id == "t1")
        .expect("EOF session remains projected");
    assert_eq!(
        (
            session.status.as_str(),
            session.active_turn_id.as_deref(),
            session.last_error.as_deref(),
        ),
        ("error", None, Some(STREAM_END_ERROR))
    );
    assert_eq!(
        snapshot
            .activities
            .iter()
            .filter(|activity| {
                activity.thread_id == "t1"
                    && activity.kind == "provider.error"
                    && activity.payload["errorMessage"] == STREAM_END_ERROR
            })
            .count(),
        1
    );
    assert_eq!(
        engine
            .repositories()
            .get_provider_session_runtime("t1".to_owned())
            .await
            .unwrap()
            .expect("EOF runtime remains durable")
            .status,
        "error"
    );
    let event_count_after_recovery = engine.read_events(0).await.unwrap().len();
    assert_eq!(
        event_count_after_recovery,
        event_count_before_recovery + 1,
        "recovery appends only the exact assistant completion"
    );

    reconcile_abandoned_provider_sessions(&engine)
        .await
        .expect("duplicate startup reconciliation");
    assert_eq!(
        engine.read_events(0).await.unwrap().len(),
        event_count_after_recovery,
        "completed error-runtime recovery is idempotent"
    );
    engine.shutdown().await;
}

#[tokio::test]
async fn runtime_observed_capability_upgrade_survives_stream_end() {
    let (engine, database) = engine_and_database().await;
    let activity = ActivityProjection::new(ActivityRepository::new(database));
    let state = Arc::new(StdMutex::new(DriverState {
        start_results: VecDeque::from([Ok(started_session("native-none"))]),
        ..DriverState::default()
    }));
    let (events_tx, events_rx) = mpsc::channel(1);
    let factory = Arc::new(FakeFactory {
        state,
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine,
        factory,
        activity.clone(),
        SupervisorOptions::default(),
    );
    supervisor.launch(launch()).await.unwrap();

    events_tx
        .send(runtime_capability_upgrade_event("native:dynamic:end"))
        .await
        .unwrap();
    let scope = ActivityScopeRef::Thread {
        thread_id: "t1".to_owned(),
    };
    wait_for_dynamic_activity(&activity, &scope).await;
    events_tx
        .send(ProviderEvent {
            native_event_id: native_event_id("native:dynamic:end"),
            event_type: "activity.native".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: None,
            item_id: None,
            request_id: None,
            payload: json!({}),
            activity: vec![ProviderActivityMutation::SetScope {
                capabilities: ActivityCapabilities::none(),
                observation_state: ActivityObservationState::Live,
            }],
            activity_controls: Default::default(),
        })
        .await
        .unwrap();
    drop(events_tx);

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = activity.snapshot(&scope).await.unwrap();
            if snapshot.observation_state == ActivityObservationState::Stale {
                assert_eq!(
                    snapshot.capabilities,
                    ActivityCapabilities::structured_full(false)
                );
                assert_eq!(
                    snapshot.sections.subagents.state,
                    ActivitySectionObservationState::Live
                );
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("stream end uses the capability observed by the event pump");

    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn runtime_observed_capability_upgrade_survives_restart_and_stop() {
    let (engine, database) = engine_and_database().await;
    let activity = ActivityProjection::new(ActivityRepository::new(database));
    let state = Arc::new(StdMutex::new(DriverState {
        start_results: VecDeque::from([
            Ok(started_session("native-none-1")),
            Ok(started_session("native-none-2")),
        ]),
        set_mode_results: VecDeque::from([Err(ProviderRuntimeError::UnsupportedCapability {
            provider: "codex".to_owned(),
            capability: "post-start runtime mode changes",
        })]),
        ..DriverState::default()
    }));
    let (first_events_tx, first_events_rx) = mpsc::channel(1);
    let (_replacement_events_tx, replacement_events_rx) = mpsc::channel(1);
    let factory = Arc::new(FakeFactory {
        state,
        events: StdMutex::new(VecDeque::from([first_events_rx, replacement_events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine,
        factory,
        activity.clone(),
        SupervisorOptions::default(),
    );
    supervisor.launch(launch()).await.unwrap();

    first_events_tx
        .send(runtime_capability_upgrade_event("native:dynamic:restart"))
        .await
        .unwrap();
    let scope = ActivityScopeRef::Thread {
        thread_id: "t1".to_owned(),
    };
    wait_for_dynamic_activity(&activity, &scope).await;

    supervisor
        .handle_orchestration(OrchestrationCommand::ThreadRuntimeModeSet {
            command_id: "restart-after-dynamic-upgrade".to_owned(),
            thread_id: "t1".to_owned(),
            runtime_mode: "approval-required".to_owned(),
            created_at: NOW.to_owned(),
        })
        .await
        .unwrap();
    let restarted = activity.snapshot(&scope).await.unwrap();
    assert_eq!(
        restarted.capabilities,
        ActivityCapabilities::structured_full(false),
        "a replacement startup-none result must not overwrite runtime-observed support"
    );
    assert_eq!(restarted.observation_state, ActivityObservationState::Live);

    supervisor
        .handle_orchestration(OrchestrationCommand::ThreadSessionStop {
            command_id: "stop-after-dynamic-upgrade".to_owned(),
            thread_id: "t1".to_owned(),
            created_at: NOW.to_owned(),
        })
        .await
        .unwrap();
    let stopped = activity.snapshot(&scope).await.unwrap();
    assert_eq!(
        stopped.capabilities,
        ActivityCapabilities::structured_full(false)
    );
    assert_eq!(stopped.observation_state, ActivityObservationState::Live);
    assert_eq!(
        stopped.sections.subagents.state,
        ActivitySectionObservationState::Live
    );

    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn authoritative_startup_capabilities_replace_runtime_observation_and_reset_its_source() {
    let (engine, database) = engine_and_database().await;
    let activity = ActivityProjection::new(ActivityRepository::new(database));
    let reduced = ActivityCapabilities {
        actors: true,
        attributed_activity: true,
        background_work: false,
        history_recovery: ActivityHistoryRecovery::Bounded,
        terminal_observation: false,
        targeted_actor_cancellation: false,
    };
    let state = Arc::new(StdMutex::new(DriverState {
        start_results: VecDeque::from([
            Ok(started_session("runtime-observed")),
            Ok(StartedSession {
                resume_cursor: Some(json!({"sessionId":"authoritative-reduced"})),
                runtime_payload: None,
                activity_capabilities: reduced.clone(),
            }),
            Ok(started_session("authoritative-none")),
        ]),
        set_mode_results: VecDeque::from([
            Err(ProviderRuntimeError::UnsupportedCapability {
                provider: "codex".to_owned(),
                capability: "post-start runtime mode changes",
            }),
            Err(ProviderRuntimeError::UnsupportedCapability {
                provider: "codex".to_owned(),
                capability: "post-start runtime mode changes",
            }),
        ]),
        ..DriverState::default()
    }));
    let (first_events_tx, first_events_rx) = mpsc::channel(1);
    let (_second_events_tx, second_events_rx) = mpsc::channel(1);
    let (_third_events_tx, third_events_rx) = mpsc::channel(1);
    let factory = Arc::new(FakeFactory {
        state,
        events: StdMutex::new(VecDeque::from([
            first_events_rx,
            second_events_rx,
            third_events_rx,
        ])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine,
        factory,
        activity.clone(),
        SupervisorOptions::default(),
    );
    supervisor.launch(launch()).await.unwrap();

    first_events_tx
        .send(runtime_capability_upgrade_event(
            "native:dynamic:authoritative-restart",
        ))
        .await
        .unwrap();
    let scope = ActivityScopeRef::Thread {
        thread_id: "t1".to_owned(),
    };
    wait_for_dynamic_activity(&activity, &scope).await;

    supervisor
        .handle_orchestration(OrchestrationCommand::ThreadRuntimeModeSet {
            command_id: "restart-with-authoritative-reduced-capabilities".to_owned(),
            thread_id: "t1".to_owned(),
            runtime_mode: "approval-required".to_owned(),
            created_at: NOW.to_owned(),
        })
        .await
        .unwrap();
    assert_eq!(
        activity.snapshot(&scope).await.unwrap().capabilities,
        reduced,
        "an authoritative non-none startup result must replace runtime-observed capabilities"
    );

    supervisor
        .handle_orchestration(OrchestrationCommand::ThreadRuntimeModeSet {
            command_id: "restart-with-authoritative-none-capabilities".to_owned(),
            thread_id: "t1".to_owned(),
            runtime_mode: "full-access".to_owned(),
            created_at: NOW.to_owned(),
        })
        .await
        .unwrap();
    assert_eq!(
        activity.snapshot(&scope).await.unwrap().capabilities,
        ActivityCapabilities::none(),
        "accepting authoritative startup capabilities must reset the runtime-observed source"
    );

    supervisor.shutdown().await.unwrap();
}

fn runtime_capability_upgrade_event(native_id: &str) -> ProviderEvent {
    ProviderEvent {
        native_event_id: native_event_id(native_id),
        event_type: "activity.native".to_owned(),
        thread_id: "t1".to_owned(),
        turn_id: None,
        item_id: None,
        request_id: None,
        payload: json!({}),
        activity: vec![
            ProviderActivityMutation::SetScope {
                capabilities: ActivityCapabilities::structured_full(false),
                observation_state: ActivityObservationState::Live,
            },
            ProviderActivityMutation::SetSectionHealth {
                section: ActivitySection::Subagents,
                health: ActivitySectionHealth::live(),
            },
            ProviderActivityMutation::SetSectionHealth {
                section: ActivitySection::BackgroundTasks,
                health: ActivitySectionHealth::live(),
            },
            ProviderActivityMutation::upsert_actor(
                "actor:runtime-observed",
                None,
                "Runtime observed",
                "running",
            )
            .unwrap(),
        ],
        activity_controls: Default::default(),
    }
}

async fn wait_for_dynamic_activity(activity: &ActivityProjection, scope: &ActivityScopeRef) {
    let result = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = activity.snapshot(scope).await.unwrap();
            if snapshot.capabilities == ActivityCapabilities::structured_full(false)
                && snapshot
                    .actors
                    .iter()
                    .any(|actor| actor.id == "actor:runtime-observed")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    if result.is_err() {
        panic!(
            "runtime capability upgrade is projected; final snapshot: {:#?}",
            activity.snapshot(scope).await.unwrap()
        );
    }
}

#[tokio::test]
async fn intentional_provider_restart_does_not_mark_activity_scope_stale() {
    let (engine, database) = engine_and_database().await;
    let activity = ActivityProjection::new(ActivityRepository::new(database));
    let started = || StartedSession {
        resume_cursor: Some(json!({"sessionId":"native-1"})),
        runtime_payload: None,
        activity_capabilities: ActivityCapabilities::structured_full(false),
    };
    let state = Arc::new(StdMutex::new(DriverState {
        start_results: VecDeque::from([Ok(started()), Ok(started())]),
        set_mode_results: VecDeque::from([Err(ProviderRuntimeError::UnsupportedCapability {
            provider: "codex".to_owned(),
            capability: "post-start runtime mode changes",
        })]),
        ..DriverState::default()
    }));
    let (_first_events_tx, first_events_rx) = mpsc::channel(1);
    let (_replacement_events_tx, replacement_events_rx) = mpsc::channel(1);
    let factory = Arc::new(FakeFactory {
        state,
        events: StdMutex::new(VecDeque::from([first_events_rx, replacement_events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine,
        factory,
        activity.clone(),
        SupervisorOptions::default(),
    );
    supervisor.launch(launch()).await.unwrap();

    supervisor
        .handle_orchestration(OrchestrationCommand::ThreadRuntimeModeSet {
            command_id: "restart-runtime-mode".to_owned(),
            thread_id: "t1".to_owned(),
            runtime_mode: "approval-required".to_owned(),
            created_at: NOW.to_owned(),
        })
        .await
        .unwrap();

    let snapshot = activity
        .snapshot(&ActivityScopeRef::Thread {
            thread_id: "t1".to_owned(),
        })
        .await
        .unwrap();
    assert_eq!(snapshot.observation_state, ActivityObservationState::Live);
    assert_eq!(snapshot.revision, 0);

    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn capable_relaunch_revives_stale_scope_before_accepting_activity() {
    let (engine, database) = engine_and_database().await;
    let activity = ActivityProjection::new(ActivityRepository::new(database));
    let state = Arc::new(StdMutex::new(DriverState {
        start_results: VecDeque::from([
            Ok(StartedSession {
                resume_cursor: Some(json!({"sessionId":"native-1"})),
                runtime_payload: None,
                activity_capabilities: ActivityCapabilities::none(),
            }),
            Ok(StartedSession {
                resume_cursor: Some(json!({"sessionId":"native-2"})),
                runtime_payload: None,
                activity_capabilities: ActivityCapabilities::structured_full(false),
            }),
        ]),
        set_mode_results: VecDeque::from([Err(ProviderRuntimeError::UnsupportedCapability {
            provider: "codex".to_owned(),
            capability: "post-start runtime mode changes",
        })]),
        ..DriverState::default()
    }));
    let (first_events_tx, first_events_rx) = mpsc::channel(1);
    let (replacement_events_tx, replacement_events_rx) = mpsc::channel(1);
    let factory = Arc::new(FakeFactory {
        state,
        events: StdMutex::new(VecDeque::from([first_events_rx, replacement_events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine,
        factory,
        activity.clone(),
        SupervisorOptions::default(),
    );
    supervisor.launch(launch()).await.unwrap();
    drop(first_events_tx);

    let scope = ActivityScopeRef::Thread {
        thread_id: "t1".to_owned(),
    };
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if activity.snapshot(&scope).await.unwrap().observation_state
                == ActivityObservationState::Stale
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("first launch becomes genuinely stale");

    supervisor
        .handle_orchestration(OrchestrationCommand::ThreadRuntimeModeSet {
            command_id: "restart-capable-provider".to_owned(),
            thread_id: "t1".to_owned(),
            runtime_mode: "approval-required".to_owned(),
            created_at: NOW.to_owned(),
        })
        .await
        .unwrap();
    replacement_events_tx
        .send(ProviderEvent {
            native_event_id: native_event_id("native:event:after-relaunch"),
            event_type: "activity.native".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: None,
            item_id: None,
            request_id: None,
            payload: json!({}),
            activity: vec![
                ProviderActivityMutation::upsert_actor(
                    "actor:after-relaunch",
                    None,
                    "Relaunched child",
                    "running",
                )
                .unwrap(),
            ],
            activity_controls: Default::default(),
        })
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = activity.snapshot(&scope).await.unwrap();
            if snapshot.observation_state == ActivityObservationState::Live
                && snapshot.capabilities == ActivityCapabilities::structured_full(false)
                && snapshot.sections.subagents.state == ActivitySectionObservationState::Live
                && snapshot.sections.background_tasks.state == ActivitySectionObservationState::Live
                && snapshot
                    .actors
                    .iter()
                    .any(|actor| actor.id == "actor:after-relaunch")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("capable relaunch revives scope before accepting its activity");

    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn downgrade_to_none_preserves_actor_history_as_stale() {
    assert_capability_downgrade_preserves_history(RetainedActivityHistory::Actor).await;
}

#[tokio::test]
async fn downgrade_to_none_preserves_work_history_as_stale() {
    assert_capability_downgrade_preserves_history(RetainedActivityHistory::WorkItem).await;
}

#[derive(Clone, Copy)]
enum RetainedActivityHistory {
    Actor,
    WorkItem,
}

async fn assert_capability_downgrade_preserves_history(history: RetainedActivityHistory) {
    let (engine, database) = engine_and_database().await;
    let activity = ActivityProjection::new(ActivityRepository::new(database));
    let state = Arc::new(StdMutex::new(DriverState {
        start_results: VecDeque::from([
            Ok(StartedSession {
                resume_cursor: Some(json!({"sessionId":"native-full"})),
                runtime_payload: None,
                activity_capabilities: ActivityCapabilities::structured_full(false),
            }),
            Ok(StartedSession {
                resume_cursor: Some(json!({"sessionId":"native-none"})),
                runtime_payload: None,
                activity_capabilities: ActivityCapabilities::none(),
            }),
        ]),
        set_mode_results: VecDeque::from([Err(ProviderRuntimeError::UnsupportedCapability {
            provider: "codex".to_owned(),
            capability: "post-start runtime mode changes",
        })]),
        ..DriverState::default()
    }));
    let (first_events_tx, first_events_rx) = mpsc::channel(1);
    let (_replacement_events_tx, replacement_events_rx) = mpsc::channel(1);
    let factory = Arc::new(FakeFactory {
        state,
        events: StdMutex::new(VecDeque::from([first_events_rx, replacement_events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine,
        factory,
        activity.clone(),
        SupervisorOptions::default(),
    );
    supervisor.launch(launch()).await.unwrap();

    let (record_id, mutation) = match history {
        RetainedActivityHistory::Actor => (
            "actor:retained",
            ProviderActivityMutation::upsert_actor(
                "actor:retained",
                None,
                "Retained actor",
                "running",
            )
            .unwrap(),
        ),
        RetainedActivityHistory::WorkItem => (
            "work:retained",
            ProviderActivityMutation::UpsertWorkItem(
                ActivityWorkItemSummary::try_new(
                    "work:retained",
                    None,
                    "Retained work",
                    "background",
                    None,
                    None,
                    ActivityLifecycle::Running,
                    None,
                    NOW,
                    NOW,
                    None,
                )
                .unwrap(),
            ),
        ),
    };
    first_events_tx
        .send(ProviderEvent {
            native_event_id: native_event_id("native:event:retained"),
            event_type: "activity.native".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: None,
            item_id: None,
            request_id: None,
            payload: json!({}),
            activity: vec![mutation],
            activity_controls: Default::default(),
        })
        .await
        .unwrap();

    let scope = ActivityScopeRef::Thread {
        thread_id: "t1".to_owned(),
    };
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = activity.snapshot(&scope).await.unwrap();
            let retained = match history {
                RetainedActivityHistory::Actor => {
                    snapshot.counts.subagents.active == 1
                        && snapshot.actors.iter().any(|actor| actor.id == record_id)
                }
                RetainedActivityHistory::WorkItem => {
                    snapshot.counts.background_tasks.active == 1
                        && snapshot
                            .work_items
                            .iter()
                            .any(|work_item| work_item.id == record_id)
                }
            };
            if retained {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("history is projected before restart");

    supervisor
        .handle_orchestration(OrchestrationCommand::ThreadRuntimeModeSet {
            command_id: "restart-without-activity".to_owned(),
            thread_id: "t1".to_owned(),
            runtime_mode: "approval-required".to_owned(),
            created_at: NOW.to_owned(),
        })
        .await
        .unwrap();

    let snapshot = activity.snapshot(&scope).await.unwrap();
    assert_eq!(snapshot.capabilities, ActivityCapabilities::none());
    assert_eq!(snapshot.observation_state, ActivityObservationState::Live);
    match history {
        RetainedActivityHistory::Actor => {
            assert_eq!(snapshot.counts.subagents.active, 1);
            assert!(snapshot.actors.iter().any(|actor| actor.id == record_id));
            assert_eq!(
                snapshot.sections.subagents.state,
                ActivitySectionObservationState::Stale
            );
            assert_eq!(
                snapshot.sections.background_tasks.state,
                ActivitySectionObservationState::Unsupported
            );
        }
        RetainedActivityHistory::WorkItem => {
            assert_eq!(snapshot.counts.background_tasks.active, 1);
            assert!(
                snapshot
                    .work_items
                    .iter()
                    .any(|work_item| work_item.id == record_id)
            );
            assert_eq!(
                snapshot.sections.background_tasks.state,
                ActivitySectionObservationState::Stale
            );
            assert_eq!(
                snapshot.sections.subagents.state,
                ActivitySectionObservationState::Unsupported
            );
        }
    }

    supervisor.shutdown().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn shutdown_compensates_a_stale_write_already_queued_on_sqlite() {
    let (engine, database) = engine_and_database().await;
    let activity = ActivityProjection::new(ActivityRepository::new(database.clone()));
    let stream_ended = Arc::new(tokio::sync::Notify::new());
    let state = Arc::new(StdMutex::new(DriverState {
        start_results: VecDeque::from([Ok(StartedSession {
            resume_cursor: Some(json!({"sessionId":"native-1"})),
            runtime_payload: None,
            activity_capabilities: ActivityCapabilities::structured_full(false),
        })]),
        stream_ended: Some(stream_ended.clone()),
        ..DriverState::default()
    }));
    let (events_tx, events_rx) = mpsc::channel(1);
    let factory = Arc::new(FakeFactory {
        state,
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine,
        factory,
        activity.clone(),
        SupervisorOptions::default(),
    );
    supervisor.launch(launch()).await.unwrap();

    let (blocker_entered_tx, blocker_entered_rx) = tokio::sync::oneshot::channel();
    let (release_blocker_tx, release_blocker_rx) = std::sync::mpsc::sync_channel(1);
    let blocker_database = database.clone();
    let blocker = tokio::spawn(async move {
        blocker_database
            .call(move |_| {
                let _ = blocker_entered_tx.send(());
                release_blocker_rx.recv().expect("release SQLite blocker");
                Ok(())
            })
            .await
    });
    blocker_entered_rx.await.unwrap();

    drop(events_tx);
    stream_ended.notified().await;
    let shutdown = tokio::spawn(async move { supervisor.shutdown().await });
    tokio::task::yield_now().await;
    release_blocker_tx.send(()).unwrap();
    blocker.await.unwrap().unwrap();
    tokio::time::timeout(Duration::from_secs(2), shutdown)
        .await
        .expect("shutdown completes after queued stale work")
        .unwrap()
        .unwrap();

    let snapshot = activity
        .snapshot(&ActivityScopeRef::Thread {
            thread_id: "t1".to_owned(),
        })
        .await
        .unwrap();
    assert_eq!(snapshot.observation_state, ActivityObservationState::Live);
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_native_ids_drop_only_activity_without_leaking_sensitive_text() {
    let capture = TraceCapture::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_target(false)
        .with_writer(capture.clone())
        .finish();
    let _subscriber = tracing::subscriber::set_default(subscriber);
    let (engine, database) = engine_and_database().await;
    let activity = ActivityProjection::new(ActivityRepository::new(database));
    let state = Arc::new(StdMutex::new(DriverState {
        start_results: VecDeque::from([Ok(StartedSession {
            resume_cursor: Some(json!({"sessionId":"native-1"})),
            runtime_payload: None,
            activity_capabilities: ActivityCapabilities::structured_full(false),
        })]),
        ..DriverState::default()
    }));
    let (events_tx, events_rx) = mpsc::channel(4);
    let factory = Arc::new(FakeFactory {
        state,
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity.clone(),
        SupervisorOptions::default(),
    );
    supervisor.launch(launch()).await.unwrap();

    for (index, native_event_id) in [
        String::new(),
        "control\u{0007}SENSITIVE_CONTROL_ID".to_owned(),
        format!("SENSITIVE_OVERSIZED_ID{}", "x".repeat(5_000)),
    ]
    .into_iter()
    .enumerate()
    {
        events_tx
            .send(ProviderEvent {
                native_event_id: ProviderNativeEventId::new(native_event_id).ok(),
                event_type: "activity.invalid-native-id".to_owned(),
                thread_id: "t1".to_owned(),
                turn_id: None,
                item_id: None,
                request_id: None,
                payload: json!({"index":index}),
                activity: vec![
                    ProviderActivityMutation::upsert_actor(
                        format!("actor:invalid:{index}"),
                        None,
                        "Must not project",
                        "running",
                    )
                    .unwrap(),
                ],
                activity_controls: Default::default(),
            })
            .await
            .unwrap();
    }

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let ordinary_count = load_snapshot(&engine.repositories())
                .await
                .unwrap()
                .activities
                .iter()
                .filter(|event| event.payload["eventType"] == "activity.invalid-native-id")
                .count();
            if ordinary_count == 3 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("ordinary projection continues for invalid activity IDs");

    let snapshot = activity
        .snapshot(&ActivityScopeRef::Thread {
            thread_id: "t1".to_owned(),
        })
        .await
        .unwrap();
    assert_eq!(snapshot.revision, 0);
    assert!(snapshot.actors.is_empty());
    let traces = capture.text();
    assert!(!traces.contains("SENSITIVE_CONTROL_ID"));
    assert!(!traces.contains("SENSITIVE_OVERSIZED_ID"));

    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn mismatched_event_thread_cannot_contaminate_launch_activity_scope() {
    let (engine, database) = engine_and_database().await;
    engine
        .dispatch(
            serde_json::from_value(json!({
                "type":"thread.create", "commandId":"thread-2", "threadId":"t2", "projectId":"p1",
                "title":"Other thread", "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                "runtimeMode":"full-access", "branch":null, "worktreePath":null, "createdAt":NOW
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    let activity = ActivityProjection::new(ActivityRepository::new(database));
    let state = Arc::new(StdMutex::new(DriverState {
        start_results: VecDeque::from([Ok(StartedSession {
            resume_cursor: Some(json!({"sessionId":"native-1"})),
            runtime_payload: None,
            activity_capabilities: ActivityCapabilities::structured_full(false),
        })]),
        ..DriverState::default()
    }));
    let (events_tx, events_rx) = mpsc::channel(1);
    let factory = Arc::new(FakeFactory {
        state,
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity.clone(),
        SupervisorOptions::default(),
    );
    supervisor.launch(launch()).await.unwrap();
    events_tx
        .send(ProviderEvent {
            native_event_id: native_event_id("native:event:wrong-thread"),
            event_type: "activity.cross-thread".to_owned(),
            thread_id: "t2".to_owned(),
            turn_id: None,
            item_id: None,
            request_id: None,
            payload: json!({}),
            activity: vec![
                ProviderActivityMutation::upsert_actor(
                    "actor:cross-thread",
                    None,
                    "Wrong thread",
                    "running",
                )
                .unwrap(),
            ],
            activity_controls: Default::default(),
        })
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if load_snapshot(&engine.repositories())
                .await
                .unwrap()
                .activities
                .iter()
                .any(|event| {
                    event.thread_id == "t2" && event.payload["eventType"] == "activity.cross-thread"
                })
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("ordinary mismatched-thread event follows existing projection semantics");

    let snapshot = activity
        .snapshot(&ActivityScopeRef::Thread {
            thread_id: "t1".to_owned(),
        })
        .await
        .unwrap();
    assert_eq!(snapshot.revision, 0);
    assert!(snapshot.actors.is_empty());
    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn activity_scope_ensure_failure_is_diagnostic_only() {
    let (engine, database) = engine_and_database().await;
    database
        .call(|connection| {
            connection.execute_batch(
                "CREATE TRIGGER fail_activity_scope_ensure
                 BEFORE INSERT ON activity_scopes
                 BEGIN
                   SELECT RAISE(FAIL, 'injected activity scope failure');
                 END;",
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let activity = ActivityProjection::new(ActivityRepository::new(database));
    let state = Arc::new(StdMutex::new(DriverState {
        start_results: VecDeque::from([Ok(StartedSession {
            resume_cursor: Some(json!({"sessionId":"native-1"})),
            runtime_payload: None,
            activity_capabilities: ActivityCapabilities::structured_full(false),
        })]),
        ..DriverState::default()
    }));
    let (events_tx, events_rx) = mpsc::channel(1);
    let factory = Arc::new(FakeFactory {
        state,
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity,
        SupervisorOptions::default(),
    );

    supervisor
        .launch(launch())
        .await
        .expect("activity scope failure does not fail provider launch");
    events_tx
        .send(ProviderEvent {
            native_event_id: native_event_id("native:event:scope-unavailable"),
            event_type: "provider.scope-unavailable".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: None,
            item_id: None,
            request_id: None,
            payload: json!({}),
            activity: vec![
                ProviderActivityMutation::upsert_actor(
                    "actor:scope-unavailable",
                    None,
                    "Unavailable",
                    "running",
                )
                .unwrap(),
            ],
            activity_controls: Default::default(),
        })
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if load_snapshot(&engine.repositories())
                .await
                .unwrap()
                .activities
                .iter()
                .any(|event| event.payload["eventType"] == "provider.scope-unavailable")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("ordinary event projects despite unavailable activity scope");
    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn activity_apply_failure_is_diagnostic_only() {
    let (engine, database) = engine_and_database().await;
    let activity = ActivityProjection::new(ActivityRepository::new(database.clone()));
    let state = Arc::new(StdMutex::new(DriverState {
        start_results: VecDeque::from([Ok(StartedSession {
            resume_cursor: Some(json!({"sessionId":"native-1"})),
            runtime_payload: None,
            activity_capabilities: ActivityCapabilities::structured_full(false),
        })]),
        ..DriverState::default()
    }));
    let (events_tx, events_rx) = mpsc::channel(1);
    let factory = Arc::new(FakeFactory {
        state,
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity.clone(),
        SupervisorOptions::default(),
    );
    supervisor.launch(launch()).await.unwrap();
    database
        .call(|connection| {
            connection.execute_batch(
                "CREATE TRIGGER fail_activity_apply
                 BEFORE INSERT ON activity_journal
                 BEGIN
                   SELECT RAISE(FAIL, 'injected activity apply failure');
                 END;",
            )?;
            Ok(())
        })
        .await
        .unwrap();

    events_tx
        .send(ProviderEvent {
            native_event_id: native_event_id("native:event:apply-failure"),
            event_type: "provider.activity-apply-failed".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: None,
            item_id: None,
            request_id: None,
            payload: json!({}),
            activity: vec![
                ProviderActivityMutation::upsert_actor(
                    "actor:apply-failure",
                    None,
                    "Apply failure",
                    "running",
                )
                .unwrap(),
            ],
            activity_controls: Default::default(),
        })
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if load_snapshot(&engine.repositories())
                .await
                .unwrap()
                .activities
                .iter()
                .any(|event| event.payload["eventType"] == "provider.activity-apply-failed")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("ordinary event projects despite activity apply failure");
    let snapshot = activity
        .snapshot(&ActivityScopeRef::Thread {
            thread_id: "t1".to_owned(),
        })
        .await
        .unwrap();
    assert_eq!(snapshot.revision, 0);
    assert!(snapshot.actors.is_empty());
    supervisor.shutdown().await.unwrap();
}

#[test]
fn activity_capabilities_default_to_no_activity() {
    assert_eq!(
        ActivityCapabilities::default(),
        ActivityCapabilities::none()
    );
    assert!(ProviderNativeEventId::new(String::new()).is_err());
    assert!(ProviderNativeEventId::new("control\u{0007}value".to_owned()).is_err());
    let sensitive_oversized = format!("SENSITIVE_VALIDATION_ID{}", "x".repeat(5_000));
    let error = ProviderNativeEventId::new(sensitive_oversized)
        .expect_err("oversized native IDs are rejected");
    assert!(!error.to_string().contains("SENSITIVE_VALIDATION_ID"));
}

#[tokio::test]
async fn routes_the_complete_live_session_lifecycle_and_stops_idempotently() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (_events_tx, events_rx) = mpsc::channel(8);
    let factory = Arc::new(FakeFactory {
        state: state.clone(),
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    let settings = TempDir::new().unwrap();

    supervisor.launch(launch()).await.unwrap();
    let duplicate = supervisor
        .launch(launch())
        .await
        .expect_err("a live thread must have exactly one provider session");
    assert!(matches!(
        duplicate,
        ProviderRuntimeError::SessionAlreadyExists { thread_id } if thread_id == "t1"
    ));

    for value in [
        json!({"type":"thread.runtime-mode.set","commandId":"mode","threadId":"t1","runtimeMode":"approval-required","createdAt":NOW}),
        json!({"type":"thread.interaction-mode.set","commandId":"interaction","threadId":"t1","interactionMode":"plan","createdAt":NOW}),
        json!({"type":"thread.meta.update","commandId":"model","threadId":"t1","modelSelection":{"instanceId":"codex","model":"gpt-5.1"}}),
    ] {
        route_orchestration_command(
            &supervisor,
            &engine,
            &settings.path().to_path_buf(),
            serde_json::from_value(value).unwrap(),
        )
        .await
        .unwrap();
    }

    {
        let state = state.lock().unwrap();
        assert_eq!(state.starts, 1);
        assert_eq!(state.modes, ["approval-required"]);
        assert_eq!(state.interaction_modes, ["plan"]);
        assert_eq!(state.models, ["gpt-5.1"]);
        assert!(state.rollbacks.is_empty());
    }

    let delete: OrchestrationCommand = serde_json::from_value(json!({
        "type":"thread.delete",
        "commandId":"delete",
        "threadId":"t1",
        "createdAt":NOW
    }))
    .unwrap();
    route_orchestration_command(
        &supervisor,
        &engine,
        &settings.path().to_path_buf(),
        delete.clone(),
    )
    .await
    .unwrap();
    route_orchestration_command(&supervisor, &engine, &settings.path().to_path_buf(), delete)
        .await
        .unwrap();
    assert_eq!(state.lock().unwrap().shutdowns, 1);
    assert!(
        engine
            .repositories()
            .get_provider_session_runtime("t1".to_owned())
            .await
            .unwrap()
            .is_none()
    );

    supervisor.shutdown().await.unwrap();
    supervisor.shutdown().await.unwrap();
    let error = supervisor
        .handle_orchestration(
            serde_json::from_value(json!({
                "type":"thread.session.stop",
                "commandId":"after-shutdown",
                "threadId":"t1",
                "createdAt":NOW
            }))
            .unwrap(),
        )
        .await
        .expect_err("commands after shutdown must fail explicitly");
    assert!(matches!(error, ProviderRuntimeError::Shutdown));
}

#[tokio::test]
async fn workspace_loss_session_stop_shuts_driver_without_deleting_thread() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (_events_tx, events_rx) = mpsc::channel(1);
    let factory = Arc::new(FakeFactory {
        state: state.clone(),
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    supervisor.launch(launch()).await.unwrap();

    supervisor
        .handle_orchestration(OrchestrationCommand::ThreadSessionStop {
            command_id: "workspace-loss:t1:provider-stop".to_owned(),
            thread_id: "t1".to_owned(),
            created_at: NOW.to_owned(),
        })
        .await
        .expect("workspace loss stops provider");

    assert_eq!(state.lock().unwrap().shutdowns, 1);
    assert!(
        engine
            .repositories()
            .get_provider_session_runtime("t1".to_owned())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        engine
            .repositories()
            .get_thread("t1".to_owned())
            .await
            .unwrap()
            .is_some()
    );
    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn recovered_provider_replacement_survives_a_paused_old_workspace_loss_stop() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState {
        set_mode_results: VecDeque::from([Err(ProviderRuntimeError::UnsupportedCapability {
            provider: "codex".to_owned(),
            capability: "post-start runtime mode changes",
        })]),
        ..DriverState::default()
    }));
    let (_old_events_tx, old_events_rx) = mpsc::channel(1);
    let (_replacement_events_tx, replacement_events_rx) = mpsc::channel(1);
    let factory = Arc::new(FakeFactory {
        state: state.clone(),
        events: StdMutex::new(VecDeque::from([old_events_rx, replacement_events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    supervisor.launch(launch()).await.unwrap();

    let availability = WorkspaceAvailabilityRegistry::new();
    let transition = WorkspaceLossTransition {
        thread_id: "t1".to_owned(),
        repository_key: "repository-1".to_owned(),
        generation: 1,
        path: PathBuf::from("/repo/worktrees/t1"),
        availability: AdoptedWorktreeAvailability::MissingRegistered,
    };
    assert!(
        availability
            .mark_unavailable(transition)
            .await
            .expect("physical identity resolves")
    );
    let old_identity = supervisor
        .capture_session_identity("t1")
        .await
        .expect("old provider identity capture")
        .expect("old provider is active");

    availability
        .clear_recovered_in_repository("t1", Path::new("/repo/worktrees/t1"), "repository-1")
        .await
        .expect("physical identity resolves");
    supervisor
        .handle_orchestration(OrchestrationCommand::ThreadRuntimeModeSet {
            command_id: "recovery-restarts-provider".to_owned(),
            thread_id: "t1".to_owned(),
            runtime_mode: "approval-required".to_owned(),
            created_at: NOW.to_owned(),
        })
        .await
        .expect("exact recovery starts a replacement provider");

    supervisor
        .stop_session_if_current(old_identity)
        .await
        .expect("paused old-session stop resumes as a no-op");
    supervisor
        .handle_orchestration(OrchestrationCommand::ThreadInteractionModeSet {
            command_id: "replacement-remains-routable".to_owned(),
            thread_id: "t1".to_owned(),
            interaction_mode: "plan".to_owned(),
            created_at: NOW.to_owned(),
        })
        .await
        .expect("replacement provider remains active");
    assert_eq!(state.lock().unwrap().shutdowns, 1);

    supervisor.shutdown().await.unwrap();
    assert_eq!(state.lock().unwrap().shutdowns, 2);
}

#[tokio::test(flavor = "current_thread")]
async fn late_provider_events_after_thread_deletion_do_not_warn() {
    let capture = TraceCapture::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_target(false)
        .with_writer(capture.clone())
        .finish();
    let _subscriber = tracing::subscriber::set_default(subscriber);
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (events_tx, events_rx) = mpsc::channel(1);
    let factory = Arc::new(FakeFactory {
        state,
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    let settings = TempDir::new().unwrap();
    supervisor.launch(launch()).await.unwrap();

    let delete: OrchestrationCommand = serde_json::from_value(json!({
        "type":"thread.delete",
        "commandId":"delete-before-provider-stop",
        "threadId":"t1",
        "createdAt":NOW
    }))
    .unwrap();
    engine.dispatch(delete.clone()).await.unwrap();
    events_tx
        .send(ProviderEvent {
            native_event_id: None,
            event_type: "session.ready".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: None,
            item_id: None,
            request_id: None,
            payload: json!({}),
            activity: Vec::new(),
            activity_controls: Default::default(),
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    route_orchestration_command(&supervisor, &engine, &settings.path().to_path_buf(), delete)
        .await
        .unwrap();
    supervisor.shutdown().await.unwrap();

    assert!(
        !capture
            .text()
            .contains("failed to project provider runtime event"),
        "late shutdown events should not be reported as unexpected projection failures"
    );
}

#[tokio::test]
async fn launch_failure_persists_the_error_and_keeps_the_thread_relaunchable() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState {
        start_results: VecDeque::from([
            Err(ProviderRuntimeError::Provider {
                provider: "codex".to_owned(),
                detail: "bootstrap failed".to_owned(),
            }),
            Ok(started_session("provider-session-2")),
        ]),
        ..DriverState::default()
    }));
    let (_events_tx1, events_rx1) = mpsc::channel(1);
    let (_events_tx2, events_rx2) = mpsc::channel(1);
    let factory = Arc::new(FakeFactory {
        state: state.clone(),
        events: StdMutex::new(VecDeque::from([events_rx1, events_rx2])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity_projection(&engine),
        SupervisorOptions::default(),
    );

    let error = supervisor
        .launch(launch())
        .await
        .expect_err("a failed provider bootstrap must surface to the caller");
    assert!(matches!(
        error,
        ProviderRuntimeError::Provider { provider, detail }
            if provider == "codex" && detail == "bootstrap failed"
    ));

    let failed_runtime = engine
        .repositories()
        .get_provider_session_runtime("t1".to_owned())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(failed_runtime.status, "error");
    assert_eq!(
        failed_runtime.runtime_payload,
        Some(json!({"error":"codex provider operation failed: bootstrap failed"}))
    );
    {
        let state = state.lock().unwrap();
        assert_eq!(state.starts, 1);
        assert_eq!(state.shutdowns, 1);
        assert_eq!(state.launches.len(), 1);
    }

    supervisor
        .launch(launch())
        .await
        .expect("a failed bootstrap must not leave a ghost live session behind");

    let recovered_runtime = engine
        .repositories()
        .get_provider_session_runtime("t1".to_owned())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered_runtime.status, "ready");
    assert_eq!(
        recovered_runtime.resume_cursor,
        Some(json!({"sessionId":"provider-session-2"}))
    );
    assert_eq!(state.lock().unwrap().starts, 2);

    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn unsupported_live_capabilities_restart_the_runtime_with_updated_launch_state() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState {
        start_results: VecDeque::from([
            Ok(started_session("provider-session-1")),
            Ok(started_session("provider-session-2")),
            Ok(started_session("provider-session-3")),
        ]),
        set_mode_results: VecDeque::from([Err(ProviderRuntimeError::UnsupportedCapability {
            provider: "codex".to_owned(),
            capability: "runtime mode switch",
        })]),
        set_model_results: VecDeque::from([Err(ProviderRuntimeError::UnsupportedCapability {
            provider: "codex".to_owned(),
            capability: "model switch",
        })]),
        ..DriverState::default()
    }));
    let (_events_tx1, events_rx1) = mpsc::channel(1);
    let (_events_tx2, events_rx2) = mpsc::channel(1);
    let (_events_tx3, events_rx3) = mpsc::channel(1);
    let factory = Arc::new(FakeFactory {
        state: state.clone(),
        events: StdMutex::new(VecDeque::from([events_rx1, events_rx2, events_rx3])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity_projection(&engine),
        SupervisorOptions::default(),
    );

    supervisor.launch(launch()).await.unwrap();
    supervisor
        .handle_orchestration(
            serde_json::from_value(json!({
                "type":"thread.runtime-mode.set",
                "commandId":"restart-mode",
                "threadId":"t1",
                "runtimeMode":"approval-required",
                "createdAt":NOW
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    supervisor
        .handle_orchestration(
            serde_json::from_value(json!({
                "type":"thread.meta.update",
                "commandId":"restart-model",
                "threadId":"t1",
                "modelSelection":{"instanceId":"codex","model":"gpt-5.1"}
            }))
            .unwrap(),
        )
        .await
        .unwrap();

    let launches = state.lock().unwrap().launches.clone();
    assert_eq!(launches.len(), 3);
    assert_eq!(launches[1].runtime_mode, "approval-required");
    assert_eq!(
        launches[1].resume_cursor,
        Some(json!({"sessionId":"provider-session-1"}))
    );
    assert_eq!(launches[2].runtime_mode, "approval-required");
    assert_eq!(launches[2].interaction_mode, "default");
    assert_eq!(launches[2].model.as_deref(), Some("gpt-5.1"));
    assert_eq!(
        launches[2].resume_cursor,
        Some(json!({"sessionId":"provider-session-2"}))
    );
    {
        let state = state.lock().unwrap();
        assert_eq!(state.starts, 3);
        assert_eq!(state.shutdowns, 2);
        assert_eq!(state.modes, ["approval-required"]);
        assert!(state.interaction_modes.is_empty());
        assert_eq!(state.models, ["gpt-5.1"]);
    }

    let runtime = engine
        .repositories()
        .get_provider_session_runtime("t1".to_owned())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(runtime.status, "ready");
    assert_eq!(
        runtime.resume_cursor,
        Some(json!({"sessionId":"provider-session-3"}))
    );

    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn metadata_update_applies_options_to_the_live_session() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (_events_tx, events_rx) = mpsc::channel(1);
    let factory = Arc::new(FakeFactory {
        state: state.clone(),
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity_projection(&engine),
        SupervisorOptions::default(),
    );

    supervisor.launch(launch()).await.unwrap();
    state.lock().unwrap().option_updates.clear();
    supervisor
        .handle_orchestration(
            serde_json::from_value(json!({
                "type":"thread.meta.update",
                "commandId":"enable-fast",
                "threadId":"t1",
                "modelSelection":{
                    "instanceId":"codex",
                    "model":"gpt-5",
                    "options":[{"id":"fastMode","value":true}]
                }
            }))
            .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        state.lock().unwrap().option_updates,
        vec![vec![json!({ "id":"fastMode", "value":true })]]
    );
    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn unsupported_live_option_update_restarts_with_the_new_options() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState {
        start_results: VecDeque::from([
            Ok(started_session("provider-session-1")),
            Ok(started_session("provider-session-2")),
        ]),
        set_options_results: VecDeque::from([
            Ok(()),
            Err(ProviderRuntimeError::UnsupportedCapability {
                provider: "codex".to_owned(),
                capability: "live option update",
            }),
            Ok(()),
        ]),
        ..DriverState::default()
    }));
    let (_events_tx1, events_rx1) = mpsc::channel(1);
    let (_events_tx2, events_rx2) = mpsc::channel(1);
    let factory = Arc::new(FakeFactory {
        state: state.clone(),
        events: StdMutex::new(VecDeque::from([events_rx1, events_rx2])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity_projection(&engine),
        SupervisorOptions::default(),
    );

    supervisor.launch(launch()).await.unwrap();
    supervisor
        .handle_orchestration(
            serde_json::from_value(json!({
                "type":"thread.meta.update",
                "commandId":"restart-fast",
                "threadId":"t1",
                "modelSelection":{
                    "instanceId":"codex",
                    "model":"gpt-5",
                    "options":[{"id":"fastMode","value":true}]
                }
            }))
            .unwrap(),
        )
        .await
        .unwrap();

    let state = state.lock().unwrap();
    assert_eq!(
        state.launches.last().unwrap().options,
        vec![json!({ "id":"fastMode", "value":true })]
    );
    assert_eq!(state.starts, 2);
    drop(state);
    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_restart_shutdown_preserves_the_existing_live_session() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState {
        set_options_results: VecDeque::from([
            Ok(()),
            Err(ProviderRuntimeError::UnsupportedCapability {
                provider: "codex".to_owned(),
                capability: "live option update",
            }),
        ]),
        shutdown_results: VecDeque::from([
            Err(ProviderRuntimeError::Provider {
                provider: "codex".to_owned(),
                detail: "injected shutdown failure".to_owned(),
            }),
            Ok(()),
        ]),
        ..DriverState::default()
    }));
    let (_events_tx, events_rx) = mpsc::channel(1);
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::from([events_rx])),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    supervisor.launch(launch()).await.unwrap();

    let error = supervisor
        .handle_orchestration(
            serde_json::from_value(json!({
                "type":"thread.meta.update", "commandId":"restart-shutdown-fails",
                "threadId":"t1",
                "modelSelection":{
                    "instanceId":"codex", "model":"gpt-5",
                    "options":[{"id":"fastMode","value":true}]
                }
            }))
            .unwrap(),
        )
        .await
        .expect_err("failed old-driver shutdown rejects restart");
    assert!(
        matches!(error, ProviderRuntimeError::Provider { ref detail, .. } if detail.contains("shutdown failure"))
    );

    supervisor
        .handle_orchestration(
            serde_json::from_value(json!({
                "type":"thread.approval.respond", "commandId":"approval-after-shutdown-failure",
                "threadId":"t1", "requestId":"request-1", "decision":"accept",
                "createdAt":NOW
            }))
            .unwrap(),
        )
        .await
        .expect("the old session remains addressable");
    let runtime = engine
        .repositories()
        .get_provider_session_runtime("t1".to_owned())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(runtime.status, "ready");
    {
        let state = state.lock().unwrap();
        assert_eq!(state.starts, 1);
        assert_eq!(state.launches.len(), 1);
        assert_eq!(
            state.approvals,
            [("request-1".to_owned(), "accept".to_owned())]
        );
        assert_eq!(state.shutdowns, 1);
    }
    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn rejected_unknown_option_keeps_the_existing_session() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState {
        set_options_results: VecDeque::from([
            Ok(()),
            Err(ProviderRuntimeError::Provider {
                provider: "codex".to_owned(),
                detail: "option madeUpMode is not supported by the selected model/session"
                    .to_owned(),
            }),
        ]),
        ..DriverState::default()
    }));
    let (_events_tx, events_rx) = mpsc::channel(1);
    let factory = Arc::new(FakeFactory {
        state: state.clone(),
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity_projection(&engine),
        SupervisorOptions::default(),
    );

    supervisor.launch(launch()).await.unwrap();
    let error = supervisor
        .handle_orchestration(
            serde_json::from_value(json!({
                "type":"thread.meta.update",
                "commandId":"reject-made-up",
                "threadId":"t1",
                "modelSelection":{
                    "instanceId":"codex",
                    "model":"gpt-5",
                    "options":[{"id":"madeUpMode","value":true}]
                }
            }))
            .unwrap(),
        )
        .await
        .expect_err("unknown option must be rejected");

    assert!(matches!(error, ProviderRuntimeError::Provider { .. }));
    let runtime = engine
        .repositories()
        .get_provider_session_runtime("t1".to_owned())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        runtime.resume_cursor,
        Some(json!({"threadId":"provider-session-1"}))
    );
    let state = state.lock().unwrap();
    assert_eq!(state.starts, 1);
    assert_eq!(state.shutdowns, 0);
    assert_eq!(state.launches.len(), 1);
    drop(state);
    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn rejected_metadata_rpc_keeps_selection_and_leaves_exact_receipt_resumable() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState {
        set_options_results: VecDeque::from([
            Ok(()),
            Err(ProviderRuntimeError::Provider {
                provider: "codex".to_owned(),
                detail: "option madeUpMode is not supported by the selected model/session"
                    .to_owned(),
            }),
        ]),
        ..DriverState::default()
    }));
    let (_events_tx, events_rx) = mpsc::channel(1);
    let supervisor = Arc::new(ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state,
            events: StdMutex::new(VecDeque::from([events_rx])),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    ));
    supervisor.launch(launch()).await.unwrap();

    let settings = TempDir::new().unwrap();
    let delivery = Arc::new(TurnDeliveryService::start(
        engine.clone(),
        supervisor.clone(),
        settings.path().to_path_buf(),
    ));
    let mut registry = RpcRegistry::empty();
    register_orchestration_rpc_with_delivery(
        &mut registry,
        engine.clone(),
        supervisor.clone(),
        settings.path().to_path_buf(),
        delivery.clone(),
        attachment_upload_registry(),
    );
    let handle = ServerRuntime::start_with_registry(test_config(&settings), registry)
        .await
        .unwrap();
    let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .unwrap();

    let command = json!({
        "type":"thread.meta.update",
        "commandId":"reject-made-up-rpc",
        "threadId":"t1",
        "modelSelection":{
            "instanceId":"codex",
            "model":"gpt-5",
            "options":[{"id":"madeUpMode","value":true}]
        }
    });
    rpc_request(&mut socket, "901", command.clone()).await;
    rpc_response(&mut socket, "901")
        .await
        .expect_err("rejected live option must reject the metadata RPC");

    let snapshot = load_snapshot(&engine.repositories()).await.unwrap();
    let thread = snapshot
        .threads
        .iter()
        .find(|thread| thread.thread_id == "t1")
        .expect("thread remains present");
    assert_eq!(
        thread.model_selection,
        json!({"instanceId":"codex","model":"gpt-5"})
    );
    let reserved = engine
        .repositories()
        .get_command_receipt("reject-made-up-rpc".to_owned())
        .await
        .unwrap()
        .expect("provider rejection leaves an exact reservation");
    assert_eq!(reserved.status, "reserved");
    assert!(reserved.payload_digest.is_some());

    rpc_request(&mut socket, "902", command).await;
    rpc_response(&mut socket, "902")
        .await
        .expect("same-payload retry resumes the provider mutation");
    let accepted = engine
        .repositories()
        .get_command_receipt("reject-made-up-rpc".to_owned())
        .await
        .unwrap()
        .expect("resumed command has a durable receipt");
    assert_eq!(accepted.status, "accepted");
    assert_eq!(accepted.payload_digest, reserved.payload_digest);

    socket.close(None).await.unwrap();
    handle.shutdown();
    handle.join().await.unwrap();
    delivery.shutdown().await;
    supervisor.shutdown().await.unwrap();
    engine.shutdown().await;
}

#[tokio::test]
async fn rejected_options_do_not_mutate_the_model_before_a_turn() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState {
        set_options_results: VecDeque::from([
            Ok(()),
            Err(ProviderRuntimeError::Provider {
                provider: "codex".to_owned(),
                detail: "option madeUpMode is not supported by the selected model/session"
                    .to_owned(),
            }),
        ]),
        ..DriverState::default()
    }));
    let (_events_tx, events_rx) = mpsc::channel(1);
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::from([events_rx])),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    supervisor.launch(launch()).await.unwrap();

    let command = serde_json::from_value(json!({
        "type":"thread.turn.start",
        "commandId":"reject-model-and-options",
        "threadId":"t1",
        "message":{
            "messageId":"reject-model-and-options-message",
            "role":"user",
            "text":"must not send",
            "attachments":[]
        },
        "modelSelection":{
            "instanceId":"codex",
            "model":"gpt-5.1",
            "options":[{"id":"madeUpMode","value":true}]
        },
        "runtimeMode":"full-access",
        "interactionMode":"default",
        "createdAt":NOW
    }))
    .unwrap();
    assert!(supervisor.handle_orchestration(command).await.is_err());

    let state = state.lock().unwrap();
    assert!(
        state.models.is_empty(),
        "model update must wait for option validation"
    );
    assert!(state.sends.is_empty());
    assert_eq!(state.starts, 1);
    assert_eq!(state.shutdowns, 0);
    assert_eq!(state.launches[0].model.as_deref(), Some("gpt-5"));
    drop(state);
    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn model_only_option_revalidation_restores_the_previous_open_code_launch() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState {
        reapply_options_on_model_change: true,
        set_options_results: VecDeque::from([
            Ok(()),
            Err(ProviderRuntimeError::Provider {
                provider: "opencode".to_owned(),
                detail: "option fastMode is not supported by the selected model/session".to_owned(),
            }),
            Ok(()),
        ]),
        ..DriverState::default()
    }));
    let (_events_tx, events_rx) = mpsc::channel(1);
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::from([events_rx])),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    let mut request = launch();
    request.provider = "opencode".to_owned();
    request.model = Some("openai/fast-model".to_owned());
    request.options = vec![json!({ "id": "fastMode", "value": true })];
    supervisor.launch(request).await.unwrap();

    let command = serde_json::from_value(json!({
        "type":"thread.turn.start",
        "commandId":"reject-model-only-fast-variant",
        "threadId":"t1",
        "message":{
            "messageId":"reject-model-only-fast-variant-message",
            "role":"user",
            "text":"must not send",
            "attachments":[]
        },
        "modelSelection":{
            "instanceId":"opencode",
            "model":"openai/no-fast-model",
            "options":[{"id":"fastMode","value":true}]
        },
        "runtimeMode":"full-access",
        "interactionMode":"default",
        "createdAt":NOW
    }))
    .unwrap();
    assert!(supervisor.handle_orchestration(command).await.is_err());

    let state = state.lock().unwrap();
    assert_eq!(state.models, ["openai/no-fast-model", "openai/fast-model"]);
    assert_eq!(
        state.option_updates,
        [
            vec![json!({"id":"fastMode","value":true})],
            vec![json!({"id":"fastMode","value":true})],
            vec![json!({"id":"fastMode","value":true})],
        ]
    );
    assert!(state.sends.is_empty());
    assert_eq!(state.starts, 1);
    assert_eq!(state.shutdowns, 0);
    assert_eq!(
        state.launches[0].model.as_deref(),
        Some("openai/fast-model")
    );
    drop(state);
    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_model_update_rolls_back_options_without_restarting_or_sending() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState {
        set_model_results: VecDeque::from([
            Err(ProviderRuntimeError::Provider {
                provider: "codex".to_owned(),
                detail: "live model update failed".to_owned(),
            }),
            Ok(()),
        ]),
        ..DriverState::default()
    }));
    let (_events_tx, events_rx) = mpsc::channel(1);
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::from([events_rx])),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    supervisor.launch(launch()).await.unwrap();
    state.lock().unwrap().option_updates.clear();

    let command = serde_json::from_value(json!({
        "type":"thread.turn.start",
        "commandId":"failed-model-after-options",
        "threadId":"t1",
        "message":{
            "messageId":"failed-model-after-options-message",
            "role":"user",
            "text":"must not send",
            "attachments":[]
        },
        "modelSelection":{
            "instanceId":"codex",
            "model":"gpt-5.1",
            "options":[{"id":"fastMode","value":true}]
        },
        "runtimeMode":"full-access",
        "interactionMode":"default",
        "createdAt":NOW
    }))
    .unwrap();
    assert!(supervisor.handle_orchestration(command).await.is_err());

    let state = state.lock().unwrap();
    assert_eq!(state.models, ["gpt-5.1", "gpt-5"]);
    assert_eq!(
        state.option_updates,
        [
            vec![json!({"id":"fastMode","value":true})],
            Vec::<Value>::new(),
        ]
    );
    assert!(state.sends.is_empty());
    assert_eq!(state.starts, 1);
    assert_eq!(state.shutdowns, 0);
    assert_eq!(state.launches[0].model.as_deref(), Some("gpt-5"));
    drop(state);
    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn persistence_failure_restores_acknowledged_model_and_options_before_next_turn() {
    let (engine, database) = engine_and_database().await;
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (_events_tx, events_rx) = mpsc::channel(1);
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::from([events_rx])),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    supervisor.launch(launch()).await.unwrap();
    state.lock().unwrap().option_updates.clear();
    database
        .call(|connection| {
            connection.execute_batch(
                "CREATE TRIGGER fail_live_configuration_persistence
                 BEFORE UPDATE ON provider_session_runtime
                 WHEN OLD.thread_id = 't1' AND OLD.status = 'ready' AND NEW.status = 'ready'
                 BEGIN
                   SELECT RAISE(FAIL, 'injected live configuration persistence failure');
                 END;",
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let rejected = serde_json::from_value(json!({
        "type":"thread.turn.start",
        "commandId":"persist-failed-configuration",
        "threadId":"t1",
        "message":{
            "messageId":"persist-failed-configuration-message",
            "role":"user", "text":"must not send", "attachments":[]
        },
        "modelSelection":{
            "instanceId":"codex", "model":"gpt-5.1",
            "options":[{"id":"fastMode","value":true}]
        },
        "runtimeMode":"full-access", "interactionMode":"default", "createdAt":NOW
    }))
    .unwrap();
    assert!(matches!(
        supervisor.handle_orchestration(rejected).await,
        Err(ProviderRuntimeError::Persistence(_))
    ));
    {
        let state = state.lock().unwrap();
        assert_eq!(state.models, ["gpt-5.1", "gpt-5"]);
        assert_eq!(
            state.option_updates,
            [
                vec![json!({"id":"fastMode","value":true})],
                Vec::<Value>::new(),
            ]
        );
        assert!(state.sends.is_empty());
        assert_eq!(state.starts, 1);
    }

    database
        .call(|connection| {
            connection.execute_batch("DROP TRIGGER fail_live_configuration_persistence;")?;
            Ok(())
        })
        .await
        .unwrap();
    supervisor
        .handle_orchestration(serde_json::from_value(json!({
            "type":"thread.turn.start", "commandId":"turn-after-persist-failure",
            "threadId":"t1",
            "message":{"messageId":"turn-after-persist-failure-message","role":"user","text":"safe old selection","attachments":[]},
            "modelSelection":{"instanceId":"codex","model":"gpt-5"},
            "runtimeMode":"full-access", "interactionMode":"default", "createdAt":NOW
        })).unwrap())
        .await
        .unwrap();
    let state = state.lock().unwrap();
    assert_eq!(state.models, ["gpt-5.1", "gpt-5"]);
    assert_eq!(state.sends, ["safe old selection"]);
    drop(state);
    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn cursor_persistence_failure_restores_default_model_and_options_before_prompt() {
    let (engine, database) = engine_and_database().await;
    let active_options = vec![json!({ "id": "fastMode", "value": true })];
    let state = Arc::new(StdMutex::new(DriverState {
        reapply_options_on_model_change: true,
        ..DriverState::default()
    }));
    let (_events_tx, events_rx) = mpsc::channel(1);
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::from([events_rx])),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    let mut request = launch();
    request.provider = "cursor".to_owned();
    request.provider_instance_id = Some("cursor".to_owned());
    request.model = Some("default".to_owned());
    request.options = active_options.clone();
    supervisor.launch(request).await.unwrap();
    state.lock().unwrap().option_updates.clear();
    database
        .call(|connection| {
            connection.execute_batch(
                "CREATE TRIGGER fail_cursor_default_restore_persistence
                 BEFORE UPDATE ON provider_session_runtime
                 WHEN OLD.thread_id = 't1' AND OLD.status = 'ready' AND NEW.status = 'ready'
                 BEGIN
                   SELECT RAISE(FAIL, 'injected Cursor configuration persistence failure');
                 END;",
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let rejected = serde_json::from_value(json!({
        "type":"thread.turn.start", "commandId":"cursor-default-restore-persistence",
        "threadId":"t1",
        "message":{
            "messageId":"cursor-default-restore-persistence-message",
            "role":"user", "text":"must not send", "attachments":[]
        },
        "modelSelection":{
            "instanceId":"cursor", "model":"target",
            "options":[{"id":"fastMode","value":true}]
        },
        "runtimeMode":"full-access", "interactionMode":"default", "createdAt":NOW
    }))
    .unwrap();
    assert!(matches!(
        supervisor.handle_orchestration(rejected).await,
        Err(ProviderRuntimeError::Persistence(_))
    ));
    {
        let state = state.lock().unwrap();
        assert_eq!(state.models, ["target", "default"]);
        assert_eq!(
            state.option_updates,
            [active_options.clone(), active_options.clone()]
        );
        assert!(state.sends.is_empty());
        assert_eq!(state.launches[0].model.as_deref(), Some("default"));
    }

    database
        .call(|connection| {
            connection.execute_batch("DROP TRIGGER fail_cursor_default_restore_persistence;")?;
            Ok(())
        })
        .await
        .unwrap();
    supervisor
        .handle_orchestration(
            serde_json::from_value(json!({
                "type":"thread.turn.start", "commandId":"cursor-turn-after-default-restore",
                "threadId":"t1",
                "message":{
                    "messageId":"cursor-turn-after-default-restore-message",
                    "role":"user", "text":"safe default selection", "attachments":[]
                },
                "modelSelection":{
                    "instanceId":"cursor", "model":"default",
                    "options":[{"id":"fastMode","value":true}]
                },
                "runtimeMode":"full-access", "interactionMode":"default", "createdAt":NOW
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(state.lock().unwrap().sends, ["safe default selection"]);
    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_default_model_persistence_restoration_blocks_delivery_when_restart_cannot_shutdown()
{
    let (engine, database) = engine_and_database().await;
    let state = Arc::new(StdMutex::new(DriverState {
        reapply_options_on_model_change: true,
        set_model_results: VecDeque::from([
            Ok(()),
            Err(ProviderRuntimeError::Provider {
                provider: "codex".to_owned(),
                detail: "injected model restoration failure".to_owned(),
            }),
        ]),
        shutdown_results: VecDeque::from([
            Err(ProviderRuntimeError::Provider {
                provider: "codex".to_owned(),
                detail: "injected restoration shutdown failure".to_owned(),
            }),
            Ok(()),
        ]),
        ..DriverState::default()
    }));
    let (_events_tx, events_rx) = mpsc::channel(1);
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::from([events_rx])),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    let mut request = launch();
    request.provider = "cursor".to_owned();
    request.provider_instance_id = Some("cursor".to_owned());
    request.model = Some("default".to_owned());
    request.options = vec![json!({ "id": "fastMode", "value": true })];
    supervisor.launch(request).await.unwrap();
    database
        .call(|connection| {
            connection.execute_batch(
                "CREATE TRIGGER fail_unrestorable_configuration_persistence
                 BEFORE UPDATE ON provider_session_runtime
                 WHEN OLD.thread_id = 't1' AND OLD.status = 'ready' AND NEW.status = 'ready'
                 BEGIN
                   SELECT RAISE(FAIL, 'injected unrestorable configuration persistence failure');
                 END;",
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let error = supervisor
        .handle_orchestration(
            serde_json::from_value(json!({
                "type":"thread.meta.update", "commandId":"unrestorable-configuration",
                "threadId":"t1",
                "modelSelection":{
                    "instanceId":"cursor", "model":"target",
                    "options":[{"id":"fastMode","value":true}]
                }
            }))
            .unwrap(),
        )
        .await
        .expect_err("failed restoration and restart must be surfaced");
    assert!(
        matches!(error, ProviderRuntimeError::Provider { ref detail, .. } if detail.contains("shutdown failure"))
    );

    database
        .call(|connection| {
            connection
                .execute_batch("DROP TRIGGER fail_unrestorable_configuration_persistence;")?;
            Ok(())
        })
        .await
        .unwrap();
    let unsafe_turn = serde_json::from_value(json!({
        "type":"thread.turn.start", "commandId":"blocked-after-restore-failure",
        "threadId":"t1",
        "message":{"messageId":"blocked-after-restore-failure-message","role":"user","text":"never send","attachments":[]},
        "modelSelection":{
            "instanceId":"cursor", "model":"default",
            "options":[{"id":"fastMode","value":true}]
        },
        "runtimeMode":"full-access", "interactionMode":"default", "createdAt":NOW
    })).unwrap();
    assert!(matches!(
        supervisor.handle_orchestration(unsafe_turn).await,
        Err(ProviderRuntimeError::Provider { ref detail, .. })
            if detail.contains("configuration is unavailable")
    ));
    let state = state.lock().unwrap();
    assert_eq!(state.models, ["target", "default"]);
    assert!(state.sends.is_empty());
    drop(state);
    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn admitted_durable_delivery_precedes_following_metadata_reconciliation() {
    let engine = engine().await;
    engine
        .dispatch(
            serde_json::from_value(json!({
                "type":"thread.create", "commandId":"thread-b", "threadId":"t2",
                "projectId":"p1", "title":"Thread B",
                "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                "runtimeMode":"full-access", "branch":null, "worktreePath":null,
                "createdAt":NOW
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    let preflight_entered = Arc::new(tokio::sync::Notify::new());
    let preflight_release = Arc::new(tokio::sync::Semaphore::new(0));
    let state = Arc::new(StdMutex::new(DriverState {
        delivery_preflight_entered: Some(preflight_entered.clone()),
        delivery_preflight_release: Some(preflight_release.clone()),
        ..DriverState::default()
    }));
    let (_events_tx, events_rx) = mpsc::channel(1);
    let (_events_tx_b, events_rx_b) = mpsc::channel(1);
    let supervisor = Arc::new(ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::from([events_rx, events_rx_b])),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    ));
    supervisor.launch(launch()).await.unwrap();
    let mut launch_b = launch();
    launch_b.thread_id = "t2".to_owned();
    supervisor.launch(launch_b).await.unwrap();
    state.lock().unwrap().operation_order.clear();

    let delivery = supervisor
        .deliver_turn(
            durable_turn_command("ordered-durable-turn", "deliver first"),
            "ordered-durable-key".to_owned(),
        )
        .await
        .unwrap();
    timeout(Duration::from_secs(1), preflight_entered.notified())
        .await
        .expect("delivery reaches provider preflight");

    let mut metadata = Box::pin(
        supervisor.handle_orchestration(
            serde_json::from_value(json!({
                "type":"thread.meta.update",
                "commandId":"metadata-after-admission",
                "threadId":"t1",
                "modelSelection":{
                    "instanceId":"codex",
                    "model":"gpt-5",
                    "options":[{"id":"fastMode","value":true}]
                }
            }))
            .unwrap(),
        ),
    );
    assert!(
        matches!(futures_util::poll!(metadata.as_mut()), Poll::Pending),
        "following metadata must wait for admitted delivery"
    );
    timeout(
        Duration::from_millis(100),
        supervisor.handle_orchestration(
            serde_json::from_value(json!({
                "type":"thread.approval.respond", "commandId":"thread-b-approval",
                "threadId":"t2", "requestId":"request-b", "decision":"accept",
                "createdAt":NOW
            }))
            .unwrap(),
        ),
    )
    .await
    .expect("thread B must not wait behind thread A delivery ordering")
    .unwrap();

    preflight_release.add_permits(1);
    assert!(matches!(
        delivery.completion().await,
        ProviderDeliveryOutcome::Accepted { .. }
    ));
    metadata.await.unwrap();
    assert_eq!(
        state.lock().unwrap().approvals,
        [("request-b".to_owned(), "accept".to_owned())]
    );
    assert_eq!(
        state.lock().unwrap().operation_order,
        ["delivery", "options"]
    );
    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn panicked_delivery_completes_deferred_configuration_with_an_error() {
    let engine = engine().await;
    let preflight_entered = Arc::new(tokio::sync::Notify::new());
    let preflight_release = Arc::new(tokio::sync::Semaphore::new(0));
    let state = Arc::new(StdMutex::new(DriverState {
        delivery_preflight_entered: Some(preflight_entered.clone()),
        delivery_preflight_release: Some(preflight_release.clone()),
        delivery_panics: 1,
        ..DriverState::default()
    }));
    let (_events_tx, events_rx) = mpsc::channel(1);
    let supervisor = Arc::new(ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state,
            events: StdMutex::new(VecDeque::from([events_rx])),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    ));
    supervisor.launch(launch()).await.unwrap();
    let delivery = supervisor
        .deliver_turn(
            durable_turn_command("panicked-delivery", "panic before admission"),
            "panicked-delivery-key".to_owned(),
        )
        .await
        .unwrap();
    timeout(Duration::from_secs(1), preflight_entered.notified())
        .await
        .expect("delivery reaches provider preflight");

    let mut metadata = Box::pin(
        supervisor.handle_orchestration(
            serde_json::from_value(json!({
                "type":"thread.meta.update", "commandId":"metadata-after-panic",
                "threadId":"t1",
                "modelSelection":{
                    "instanceId":"codex", "model":"gpt-5",
                    "options":[{"id":"fastMode","value":true}]
                }
            }))
            .unwrap(),
        ),
    );
    assert!(matches!(
        futures_util::poll!(metadata.as_mut()),
        Poll::Pending
    ));
    preflight_release.add_permits(1);
    assert!(matches!(
        delivery.completion().await,
        ProviderDeliveryOutcome::Ambiguous { .. }
    ));
    let error = timeout(Duration::from_secs(1), metadata)
        .await
        .expect("deferred metadata must never strand")
        .expect_err("abnormal delivery completion rejects deferred metadata");
    assert!(
        matches!(error, ProviderRuntimeError::Provider { ref detail, .. } if detail.contains("ended abnormally"))
    );
    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn supervisor_shutdown_completes_deferred_configuration_with_shutdown() {
    let engine = engine().await;
    let preflight_entered = Arc::new(tokio::sync::Notify::new());
    let preflight_release = Arc::new(tokio::sync::Semaphore::new(0));
    let state = Arc::new(StdMutex::new(DriverState {
        delivery_preflight_entered: Some(preflight_entered.clone()),
        delivery_preflight_release: Some(preflight_release.clone()),
        ..DriverState::default()
    }));
    let (_events_tx, events_rx) = mpsc::channel(1);
    let supervisor = Arc::new(ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state,
            events: StdMutex::new(VecDeque::from([events_rx])),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    ));
    supervisor.launch(launch()).await.unwrap();
    let delivery = supervisor
        .deliver_turn(
            durable_turn_command("shutdown-delivery", "shutdown while paused"),
            "shutdown-delivery-key".to_owned(),
        )
        .await
        .unwrap();
    timeout(Duration::from_secs(1), preflight_entered.notified())
        .await
        .expect("delivery reaches provider preflight");
    let mut metadata = Box::pin(
        supervisor.handle_orchestration(
            serde_json::from_value(json!({
                "type":"thread.meta.update", "commandId":"metadata-before-shutdown",
                "threadId":"t1",
                "modelSelection":{
                    "instanceId":"codex", "model":"gpt-5",
                    "options":[{"id":"fastMode","value":true}]
                }
            }))
            .unwrap(),
        ),
    );
    assert!(matches!(
        futures_util::poll!(metadata.as_mut()),
        Poll::Pending
    ));
    supervisor.shutdown().await.unwrap();
    assert!(matches!(
        metadata.await,
        Err(ProviderRuntimeError::Shutdown)
    ));

    preflight_release.add_permits(1);
    assert!(matches!(
        delivery.completion().await,
        ProviderDeliveryOutcome::Accepted { .. }
    ));
}

#[tokio::test]
async fn bounded_deferred_configuration_rejects_overflow_without_blocking_other_threads() {
    let engine = engine().await;
    engine
        .dispatch(
            serde_json::from_value(json!({
                "type":"thread.create", "commandId":"bounded-thread-b", "threadId":"t2",
                "projectId":"p1", "title":"Thread B",
                "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                "runtimeMode":"full-access", "branch":null, "worktreePath":null,
                "createdAt":NOW
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    let preflight_entered = Arc::new(tokio::sync::Notify::new());
    let preflight_release = Arc::new(tokio::sync::Semaphore::new(0));
    let state = Arc::new(StdMutex::new(DriverState {
        delivery_preflight_entered: Some(preflight_entered.clone()),
        delivery_preflight_release: Some(preflight_release.clone()),
        ..DriverState::default()
    }));
    let (_events_tx_a, events_rx_a) = mpsc::channel(1);
    let (_events_tx_b, events_rx_b) = mpsc::channel(1);
    let supervisor = Arc::new(ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::from([events_rx_a, events_rx_b])),
        }),
        activity_projection(&engine),
        SupervisorOptions {
            queue_capacity: 2,
            ..SupervisorOptions::default()
        },
    ));
    supervisor.launch(launch()).await.unwrap();
    let mut launch_b = launch();
    launch_b.thread_id = "t2".to_owned();
    supervisor.launch(launch_b).await.unwrap();
    state.lock().unwrap().operation_order.clear();

    let delivery = supervisor
        .deliver_turn(
            durable_turn_command("bounded-delivery", "bounded delivery"),
            "bounded-delivery-key".to_owned(),
        )
        .await
        .unwrap();
    timeout(Duration::from_secs(1), preflight_entered.notified())
        .await
        .expect("delivery reaches provider preflight");

    let mut queued = Vec::new();
    for (index, value) in [true, false].into_iter().enumerate() {
        let mut request = Box::pin(
            supervisor.handle_orchestration(
                serde_json::from_value(json!({
                    "type":"thread.meta.update", "commandId":format!("bounded-metadata-{index}"),
                    "threadId":"t1",
                    "modelSelection":{
                        "instanceId":"codex", "model":"gpt-5",
                        "options":[{"id":"fastMode","value":value}]
                    }
                }))
                .unwrap(),
            ),
        );
        assert!(matches!(
            futures_util::poll!(request.as_mut()),
            Poll::Pending
        ));
        supervisor
            .handle_orchestration(serde_json::from_value(json!({
                "type":"thread.approval.respond", "commandId":format!("bounded-barrier-{index}"),
                "threadId":"t2", "requestId":format!("barrier-{index}"),
                "decision":"accept", "createdAt":NOW
            })).unwrap())
            .await
            .expect("thread B response proves the preceding A request was received");
        queued.push(request);
    }

    let overflow = supervisor
        .handle_orchestration(
            serde_json::from_value(json!({
                "type":"thread.meta.update", "commandId":"bounded-metadata-overflow",
                "threadId":"t1",
                "modelSelection":{
                    "instanceId":"codex", "model":"gpt-5",
                    "options":[{"id":"fastMode","value":true}]
                }
            }))
            .unwrap(),
        )
        .await
        .expect_err("per-thread deferred work must be bounded");
    assert!(
        matches!(overflow, ProviderRuntimeError::Provider { ref detail, .. } if detail.contains("queue is full"))
    );
    supervisor
        .handle_orchestration(
            serde_json::from_value(json!({
                "type":"thread.approval.respond", "commandId":"bounded-thread-b-progress",
                "threadId":"t2", "requestId":"thread-b-progress",
                "decision":"accept", "createdAt":NOW
            }))
            .unwrap(),
        )
        .await
        .expect("thread B remains responsive with thread A queue full");

    preflight_release.add_permits(1);
    assert!(matches!(
        delivery.completion().await,
        ProviderDeliveryOutcome::Accepted { .. }
    ));
    for request in queued {
        request.await.unwrap();
    }
    assert_eq!(
        state.lock().unwrap().operation_order,
        ["delivery", "options", "options"]
    );
    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn runtime_and_interaction_restarts_wait_for_same_thread_durable_delivery() {
    let engine = engine().await;
    engine
        .dispatch(
            serde_json::from_value(json!({
                "type":"thread.create", "commandId":"restart-order-thread-b", "threadId":"t2",
                "projectId":"p1", "title":"Thread B",
                "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                "runtimeMode":"full-access", "branch":null, "worktreePath":null,
                "createdAt":NOW
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    let preflight_entered = Arc::new(tokio::sync::Notify::new());
    let preflight_release = Arc::new(tokio::sync::Semaphore::new(0));
    let state = Arc::new(StdMutex::new(DriverState {
        delivery_preflight_entered: Some(preflight_entered.clone()),
        delivery_preflight_release: Some(preflight_release.clone()),
        set_mode_results: VecDeque::from([Err(ProviderRuntimeError::UnsupportedCapability {
            provider: "codex".to_owned(),
            capability: "runtime mode update",
        })]),
        set_interaction_mode_results: VecDeque::from([Err(
            ProviderRuntimeError::UnsupportedCapability {
                provider: "codex".to_owned(),
                capability: "interaction mode update",
            },
        )]),
        ..DriverState::default()
    }));
    let mut event_receivers = VecDeque::new();
    let mut event_senders = Vec::new();
    for _ in 0..4 {
        let (sender, receiver) = mpsc::channel(1);
        event_senders.push(sender);
        event_receivers.push_back(receiver);
    }
    let supervisor = Arc::new(ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(event_receivers),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    ));
    supervisor.launch(launch()).await.unwrap();
    let mut launch_b = launch();
    launch_b.thread_id = "t2".to_owned();
    supervisor.launch(launch_b).await.unwrap();
    state.lock().unwrap().operation_order.clear();

    for (index, command) in [
        json!({
            "type":"thread.runtime-mode.set", "commandId":"runtime-during-delivery",
            "threadId":"t1", "runtimeMode":"approval-required", "createdAt":NOW
        }),
        json!({
            "type":"thread.interaction-mode.set", "commandId":"interaction-during-delivery",
            "threadId":"t1", "interactionMode":"plan", "createdAt":NOW
        }),
    ]
    .into_iter()
    .enumerate()
    {
        let entered = preflight_entered.notified();
        let delivery = supervisor
            .deliver_turn(
                durable_turn_command(&format!("restart-order-delivery-{index}"), "deliver first"),
                format!("restart-order-key-{index}"),
            )
            .await
            .unwrap();
        timeout(Duration::from_secs(1), entered)
            .await
            .expect("delivery reaches provider preflight");
        let mut configuration =
            Box::pin(supervisor.handle_orchestration(serde_json::from_value(command).unwrap()));
        assert!(matches!(
            futures_util::poll!(configuration.as_mut()),
            Poll::Pending
        ));
        supervisor
            .handle_orchestration(serde_json::from_value(json!({
                "type":"thread.approval.respond", "commandId":format!("restart-order-barrier-{index}"),
                "threadId":"t2", "requestId":format!("restart-order-barrier-{index}"),
                "decision":"accept", "createdAt":NOW
            })).unwrap())
            .await
            .expect("thread B proves configuration was deferred without blocking the supervisor");
        preflight_release.add_permits(1);
        assert!(matches!(
            delivery.completion().await,
            ProviderDeliveryOutcome::Accepted { .. }
        ));
        configuration.await.unwrap();
        let expected = if index == 0 {
            ["delivery", "runtime", "options"]
        } else {
            ["delivery", "interaction", "options"]
        };
        assert_eq!(state.lock().unwrap().operation_order, expected);
        state.lock().unwrap().operation_order.clear();
    }
    {
        let state = state.lock().unwrap();
        assert_eq!(state.starts, 4);
        assert_eq!(state.shutdowns, 2);
    }
    drop(event_senders);
    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_non_durable_option_reconciliation_does_not_send_the_prompt() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState {
        set_options_results: VecDeque::from([
            Ok(()),
            Err(ProviderRuntimeError::Provider {
                provider: "codex".to_owned(),
                detail: "live option update failed".to_owned(),
            }),
        ]),
        ..DriverState::default()
    }));
    let (_events_tx, events_rx) = mpsc::channel(1);
    let factory = Arc::new(FakeFactory {
        state: state.clone(),
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity_projection(&engine),
        SupervisorOptions::default(),
    );

    supervisor.launch(launch()).await.unwrap();
    let command = serde_json::from_value(json!({
        "type":"thread.turn.start",
        "commandId":"failed-live-option-turn",
        "threadId":"t1",
        "message":{
            "messageId":"failed-live-option-message",
            "role":"user",
            "text":"do work",
            "attachments":[]
        },
        "modelSelection":{
            "instanceId":"codex",
            "model":"gpt-5",
            "options":[{"id":"fastMode","value":true}]
        },
        "runtimeMode":"full-access",
        "interactionMode":"default",
        "createdAt":NOW
    }))
    .unwrap();

    assert!(supervisor.handle_orchestration(command).await.is_err());
    assert!(state.lock().unwrap().sends.is_empty());
    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn initial_launch_rejects_unknown_options_before_delivery() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState {
        set_options_results: VecDeque::from([Err(ProviderRuntimeError::Provider {
            provider: "codex".to_owned(),
            detail: "option madeUpMode is not supported by the selected model/session".to_owned(),
        })]),
        ..DriverState::default()
    }));
    let (_events_tx, events_rx) = mpsc::channel(1);
    let factory = Arc::new(FakeFactory {
        state: state.clone(),
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    let mut request = launch();
    request.options = vec![json!({"id":"madeUpMode","value":true})];

    let error = supervisor
        .launch(request)
        .await
        .expect_err("unknown initial option must fail launch");

    assert!(matches!(error, ProviderRuntimeError::Provider { .. }));
    let state = state.lock().unwrap();
    assert_eq!(state.starts, 1);
    assert_eq!(state.shutdowns, 1);
    assert!(state.sends.is_empty());
    assert_eq!(
        state.option_updates,
        vec![vec![json!({"id":"madeUpMode","value":true})]]
    );
    drop(state);
    let runtime = engine
        .repositories()
        .get_provider_session_runtime("t1".to_owned())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(runtime.status, "error");
    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_durable_option_reconciliation_does_not_start_delivery() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState {
        set_options_results: VecDeque::from([
            Ok(()),
            Err(ProviderRuntimeError::Provider {
                provider: "codex".to_owned(),
                detail: "live option update failed".to_owned(),
            }),
        ]),
        ..DriverState::default()
    }));
    let (_events_tx, events_rx) = mpsc::channel(1);
    let factory = Arc::new(FakeFactory {
        state: state.clone(),
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity_projection(&engine),
        SupervisorOptions::default(),
    );

    supervisor.launch(launch()).await.unwrap();
    let command = serde_json::from_value(json!({
        "type":"thread.turn.start",
        "commandId":"failed-durable-option-turn",
        "threadId":"t1",
        "message":{
            "messageId":"failed-durable-option-message",
            "role":"user",
            "text":"do durable work",
            "attachments":[]
        },
        "modelSelection":{
            "instanceId":"codex",
            "model":"gpt-5",
            "options":[{"id":"fastMode","value":true}]
        },
        "runtimeMode":"full-access",
        "interactionMode":"default",
        "createdAt":NOW
    }))
    .unwrap();

    assert!(
        supervisor
            .deliver_turn(command, "durable-option-key".to_owned())
            .await
            .is_err()
    );
    assert!(state.lock().unwrap().delivery_started.is_empty());
    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn durable_delivery_applies_options_before_starting_the_attempt() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (_events_tx, events_rx) = mpsc::channel(1);
    let factory = Arc::new(FakeFactory {
        state: state.clone(),
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity_projection(&engine),
        SupervisorOptions::default(),
    );

    supervisor.launch(launch()).await.unwrap();
    state.lock().unwrap().option_updates.clear();
    let command = serde_json::from_value(json!({
        "type":"thread.turn.start",
        "commandId":"durable-option-turn",
        "threadId":"t1",
        "message":{
            "messageId":"durable-option-message",
            "role":"user",
            "text":"do durable work",
            "attachments":[]
        },
        "modelSelection":{
            "instanceId":"codex",
            "model":"gpt-5",
            "options":[{"id":"fastMode","value":true}]
        },
        "runtimeMode":"full-access",
        "interactionMode":"default",
        "createdAt":NOW
    }))
    .unwrap();

    let outcome = supervisor
        .deliver_turn(command, "durable-option-key".to_owned())
        .await
        .unwrap()
        .completion()
        .await;

    assert!(matches!(outcome, ProviderDeliveryOutcome::Accepted { .. }));
    let state = state.lock().unwrap();
    assert_eq!(
        state.option_updates,
        vec![vec![json!({"id":"fastMode","value":true})]]
    );
    assert_eq!(state.delivery_started, ["do durable work"]);
    drop(state);
    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_option_restart_does_not_send_the_prompt() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState {
        start_results: VecDeque::from([
            Ok(started_session("provider-session-1")),
            Err(ProviderRuntimeError::Provider {
                provider: "codex".to_owned(),
                detail: "restart failed".to_owned(),
            }),
        ]),
        set_options_results: VecDeque::from([
            Ok(()),
            Err(ProviderRuntimeError::UnsupportedCapability {
                provider: "codex".to_owned(),
                capability: "live option update",
            }),
        ]),
        ..DriverState::default()
    }));
    let (_events_tx1, events_rx1) = mpsc::channel(1);
    let (_events_tx2, events_rx2) = mpsc::channel(1);
    let factory = Arc::new(FakeFactory {
        state: state.clone(),
        events: StdMutex::new(VecDeque::from([events_rx1, events_rx2])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity_projection(&engine),
        SupervisorOptions::default(),
    );

    supervisor.launch(launch()).await.unwrap();
    let command = serde_json::from_value(json!({
        "type":"thread.turn.start",
        "commandId":"failed-option-restart-turn",
        "threadId":"t1",
        "message":{
            "messageId":"failed-option-restart-message",
            "role":"user",
            "text":"do work",
            "attachments":[]
        },
        "modelSelection":{
            "instanceId":"codex",
            "model":"gpt-5",
            "options":[{"id":"fastMode","value":true}]
        },
        "runtimeMode":"full-access",
        "interactionMode":"default",
        "createdAt":NOW
    }))
    .unwrap();

    assert!(supervisor.handle_orchestration(command).await.is_err());
    let state = state.lock().unwrap();
    assert!(state.sends.is_empty());
    assert_eq!(state.starts, 2);
    assert_eq!(state.shutdowns, 2);
    drop(state);
    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn interaction_mode_provider_failure_preserves_the_live_launch_state() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState {
        start_results: VecDeque::from([Ok(started_session("provider-session-1"))]),
        set_interaction_mode_results: VecDeque::from([Err(ProviderRuntimeError::Provider {
            provider: "claude".to_owned(),
            detail: "set permission mode failed".to_owned(),
        })]),
        ..DriverState::default()
    }));
    let (_events_tx, events_rx) = mpsc::channel(1);
    let factory = Arc::new(FakeFactory {
        state: state.clone(),
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    let mut request = launch();
    request.provider = "claude".to_owned();
    request.provider_instance_id = Some("claude".to_owned());
    request.binary_path = "claude".to_owned();

    supervisor.launch(request).await.unwrap();
    let error = supervisor
        .handle_orchestration(
            serde_json::from_value(json!({
                "type":"thread.interaction-mode.set",
                "commandId":"failed-interaction",
                "threadId":"t1",
                "interactionMode":"plan",
                "createdAt":NOW
            }))
            .unwrap(),
        )
        .await
        .expect_err("adapter interaction-mode failures must surface without restarting");
    assert!(matches!(
        error,
        ProviderRuntimeError::Provider { provider, detail }
            if provider == "claude" && detail == "set permission mode failed"
    ));

    {
        let state = state.lock().unwrap();
        assert_eq!(state.starts, 1);
        assert_eq!(state.shutdowns, 0);
        assert_eq!(state.launches.len(), 1);
        assert_eq!(state.launches[0].interaction_mode, "default");
        assert_eq!(state.interaction_modes, ["plan"]);
    }
    let runtime = engine
        .repositories()
        .get_provider_session_runtime("t1".to_owned())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(runtime.status, "ready");
    assert_eq!(
        runtime.resume_cursor,
        Some(json!({"sessionId":"provider-session-1"}))
    );

    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn projects_event_aliases_user_input_and_proposed_plans_with_request_context() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (events_tx, events_rx) = mpsc::channel(8);
    let factory = Arc::new(FakeFactory {
        state,
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    supervisor.launch(launch()).await.unwrap();

    for event in [
        ProviderEvent {
            native_event_id: None,
            event_type: "assistant.message.delta".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: Some("turn-1".to_owned()),
            item_id: None,
            request_id: None,
            payload: json!({"messageId":"assistant-explicit","text":"Plan incoming"}),
            activity: Vec::new(),
            activity_controls: Default::default(),
        },
        ProviderEvent {
            native_event_id: None,
            event_type: "assistant.message.completed".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: Some("turn-1".to_owned()),
            item_id: None,
            request_id: None,
            payload: json!({"messageId":"assistant-explicit"}),
            activity: Vec::new(),
            activity_controls: Default::default(),
        },
        ProviderEvent {
            native_event_id: None,
            event_type: "request.opened".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: Some("turn-1".to_owned()),
            item_id: None,
            request_id: Some("approval-1".to_owned()),
            payload: json!({"requestType":"command_execution_approval","command":"cargo check"}),
            activity: Vec::new(),
            activity_controls: Default::default(),
        },
        ProviderEvent {
            native_event_id: None,
            event_type: "request.resolved".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: Some("turn-1".to_owned()),
            item_id: None,
            request_id: Some("approval-1".to_owned()),
            payload: json!("accepted"),
            activity: Vec::new(),
            activity_controls: Default::default(),
        },
        ProviderEvent {
            native_event_id: None,
            event_type: "user-input.requested".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: Some("turn-1".to_owned()),
            item_id: None,
            request_id: Some("input-1".to_owned()),
            payload: json!({
                "questions":[{
                    "id":"cwd",
                    "header":"Workspace",
                    "question":"Need a path",
                    "options":[
                        {"label":"repo","description":"Use the repository root"},
                        {"label":"worktree","description":"Use the current worktree"}
                    ]
                }]
            }),
            activity: Vec::new(),
            activity_controls: Default::default(),
        },
        ProviderEvent {
            native_event_id: None,
            event_type: "user-input.resolved".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: Some("turn-1".to_owned()),
            item_id: None,
            request_id: Some("input-1".to_owned()),
            payload: json!("workspace chosen"),
            activity: Vec::new(),
            activity_controls: Default::default(),
        },
        ProviderEvent {
            native_event_id: None,
            event_type: "turn.proposed.completed".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: Some("turn-1".to_owned()),
            item_id: None,
            request_id: None,
            payload: json!({"planMarkdown":"1. Inspect\n2. Fix\n3. Verify"}),
            activity: Vec::new(),
            activity_controls: Default::default(),
        },
    ] {
        events_tx.send(event).await.unwrap();
    }

    match tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let snapshot = load_snapshot(&engine.repositories()).await.unwrap();
            let assistant = snapshot
                .messages
                .iter()
                .find(|message| message.message_id == "assistant-explicit");
            let thread = snapshot
                .threads
                .iter()
                .find(|thread| thread.thread_id == "t1");
            let activities = snapshot
                .activities
                .iter()
                .filter(|activity| activity.thread_id == "t1")
                .collect::<Vec<_>>();
            let plan = snapshot
                .proposed_plans
                .iter()
                .find(|plan| plan.thread_id == "t1" && plan.turn_id.as_deref() == Some("turn-1"));
            let approvals = engine
                .repositories()
                .list_pending_approvals_by_thread("t1".to_owned())
                .await
                .unwrap();
            let approval = approvals
                .iter()
                .find(|approval| approval.request_id == "approval-1");
            let approval_resolved = activities.iter().find(|activity| {
                activity.kind == "approval.resolved"
                    && activity.payload["requestId"] == "approval-1"
                    && activity.payload["detail"] == "accepted"
            });
            let user_input_requested = activities.iter().find(|activity| {
                activity.kind == "user-input.requested"
                    && activity.payload["requestId"] == "input-1"
                    && activity.payload["questions"][0]["id"] == "cwd"
            });
            let user_input_resolved = activities.iter().find(|activity| {
                activity.kind == "user-input.resolved"
                    && activity.payload["requestId"] == "input-1"
                    && activity.payload["detail"] == "workspace chosen"
            });
            if assistant
                .is_some_and(|message| message.text == "Plan incoming" && !message.is_streaming)
                && thread.is_some_and(|thread| {
                    thread.pending_approval_count == 0 && thread.has_actionable_proposed_plan == 1
                })
                && plan.is_some_and(|plan| plan.plan_markdown.contains("Inspect"))
                && approval.is_some_and(|approval| approval.status == "resolved")
                && approval_resolved.is_some()
                && user_input_requested.is_some()
                && user_input_resolved.is_some()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    {
        Ok(()) => {}
        Err(error) => {
            let snapshot = load_snapshot(&engine.repositories()).await.unwrap();
            let approvals = engine
                .repositories()
                .list_pending_approvals_by_thread("t1".to_owned())
                .await
                .unwrap();
            panic!(
                "provider event aliases must project into durable orchestration state: {error:?}\nassistant={:?}\nthread={:?}\nplan={:?}\napprovals={approvals:?}\nactivities={:?}",
                snapshot
                    .messages
                    .iter()
                    .find(|message| message.message_id == "assistant-explicit"),
                snapshot
                    .threads
                    .iter()
                    .find(|thread| thread.thread_id == "t1"),
                snapshot.proposed_plans.iter().find(
                    |plan| plan.thread_id == "t1" && plan.turn_id.as_deref() == Some("turn-1")
                ),
                snapshot
                    .activities
                    .iter()
                    .filter(|activity| activity.thread_id == "t1")
                    .collect::<Vec<_>>()
            );
        }
    }

    supervisor.shutdown().await.unwrap();
}

/// The provider rewinds before any file is restored: a rewind cannot be undone, so it runs
/// first, and the files follow only once it succeeded.
#[tokio::test]
async fn checkpoint_rpc_rewinds_the_provider_once_before_restoring_files() {
    let repository = initialize_repository();
    let engine = engine().await;
    for (turn_count, content) in [
        (0, "baseline\n"),
        (1, "one\n"),
        (2, "two\n"),
        (3, "three\n"),
    ] {
        project_checkpoint(&engine, repository.path(), turn_count, content).await;
    }

    let state = Arc::new(StdMutex::new(DriverState {
        rollback_workspace: Some(repository.path().to_path_buf()),
        ..DriverState::default()
    }));
    let (_events_tx, events_rx) = mpsc::channel(8);
    let supervisor = Arc::new(ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::from([events_rx])),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    ));
    let mut provider_launch = launch();
    provider_launch.cwd = repository.path().to_path_buf();
    supervisor.launch(provider_launch).await.unwrap();
    let effects = OrchestrationEffects::start(
        engine.clone(),
        Arc::new(GitRepository::default()),
        supervisor_effects(&supervisor, &engine, repository.path()),
        EffectsOptions::default(),
    )
    .await
    .unwrap();
    let settings = TempDir::new().unwrap();
    let mut registry = RpcRegistry::empty();
    let delivery = Arc::new(TurnDeliveryService::start(
        engine.clone(),
        supervisor.clone(),
        settings.path().to_path_buf(),
    ));
    register_orchestration_rpc_with_delivery(
        &mut registry,
        engine.clone(),
        supervisor.clone(),
        settings.path().to_path_buf(),
        delivery.clone(),
        attachment_upload_registry(),
    );
    let handle = ServerRuntime::start_with_registry(test_config(&settings), registry)
        .await
        .unwrap();
    let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .unwrap();
    let mut events = engine.subscribe_events();

    rpc_request(
        &mut socket,
        "1",
        json!({
            "type":"thread.checkpoint.revert",
            "commandId":"rollback-success",
            "threadId":"t1",
            "turnCount":1,
            "createdAt":NOW
        }),
    )
    .await;
    rpc_response(&mut socket, "1")
        .await
        .expect("checkpoint RPC is accepted");
    wait_for_event(&mut events, |event| {
        event.event.event_type == "thread.reverted"
    })
    .await;

    effects.shutdown().await;
    assert_eq!(
        std::fs::read_to_string(repository.path().join("tracked.txt"))
            .unwrap()
            .replace("\r\n", "\n"),
        "one\n"
    );
    {
        let state = state.lock().unwrap();
        assert_eq!(state.rollbacks, [2]);
        assert_eq!(
            state.rollback_observations,
            [(2, Some("three\n".to_owned()))]
        );
    }
    for stale_turn in [2, 3] {
        assert!(!git_succeeds(
            repository.path(),
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &orchestration_effects::checkpoint_ref("t1", stale_turn),
            ]
        ));
    }

    socket.close(None).await.unwrap();
    handle.shutdown();
    handle.join().await.unwrap();
    delivery.shutdown().await;
    supervisor.shutdown().await.unwrap();
    engine.shutdown().await;
}

/// A failed provider rewind leaves everything as it was: files, checkpoint refs and messages.
#[tokio::test]
async fn checkpoint_rpc_reports_effect_failure_without_a_direct_or_second_rollback() {
    let repository = initialize_repository();
    let engine = engine().await;
    for (turn_count, content) in [
        (0, "baseline\n"),
        (1, "one\n"),
        (2, "two\n"),
        (3, "three\n"),
    ] {
        project_checkpoint(&engine, repository.path(), turn_count, content).await;
    }

    let state = Arc::new(StdMutex::new(DriverState {
        rollback_workspace: Some(repository.path().to_path_buf()),
        rollback_error: Some("injected provider rollback failure".to_owned()),
        ..DriverState::default()
    }));
    let (_events_tx, events_rx) = mpsc::channel(8);
    let supervisor = Arc::new(ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::from([events_rx])),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    ));
    let mut provider_launch = launch();
    provider_launch.cwd = repository.path().to_path_buf();
    supervisor.launch(provider_launch).await.unwrap();
    let effects = OrchestrationEffects::start(
        engine.clone(),
        Arc::new(GitRepository::default()),
        supervisor_effects(&supervisor, &engine, repository.path()),
        EffectsOptions::default(),
    )
    .await
    .unwrap();
    let settings = TempDir::new().unwrap();
    let mut registry = RpcRegistry::empty();
    let delivery = Arc::new(TurnDeliveryService::start(
        engine.clone(),
        supervisor.clone(),
        settings.path().to_path_buf(),
    ));
    register_orchestration_rpc_with_delivery(
        &mut registry,
        engine.clone(),
        supervisor.clone(),
        settings.path().to_path_buf(),
        delivery.clone(),
        attachment_upload_registry(),
    );
    let handle = ServerRuntime::start_with_registry(test_config(&settings), registry)
        .await
        .unwrap();
    let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .unwrap();
    let mut events = engine.subscribe_events();

    rpc_request(
        &mut socket,
        "2",
        json!({
            "type":"thread.checkpoint.revert",
            "commandId":"rollback-failure",
            "threadId":"t1",
            "turnCount":1,
            "createdAt":NOW
        }),
    )
    .await;
    rpc_response(&mut socket, "2")
        .await
        .expect("checkpoint command acceptance is independent of its asynchronous effect");
    wait_for_event(&mut events, |event| {
        event.event.event_type == "thread.activity-appended"
            && event.event.payload["activity"]["kind"] == "checkpoint.revert.failed"
    })
    .await;

    effects.shutdown().await;
    assert_eq!(
        std::fs::read_to_string(repository.path().join("tracked.txt"))
            .unwrap()
            .replace("\r\n", "\n"),
        "three\n",
        "a refused rewind must not restore any file"
    );
    {
        let state = state.lock().unwrap();
        assert_eq!(state.rollbacks, [2]);
        assert_eq!(
            state.rollback_observations,
            [(2, Some("three\n".to_owned()))]
        );
    }
    let events = engine.read_events(0).await.unwrap();
    assert!(
        !events
            .iter()
            .any(|event| event.event.event_type == "thread.reverted")
    );
    assert!(
        events.iter().any(|event| {
            event.event.payload["activity"]["kind"] == "checkpoint.revert.failed"
                && event.event.payload["activity"]["payload"]["detail"]
                    .as_str()
                    .is_some_and(|detail| detail.contains("injected provider rollback failure"))
        }),
        "the provider's reason reaches the work log"
    );
    for preserved_turn in [2, 3] {
        assert!(git_succeeds(
            repository.path(),
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &orchestration_effects::checkpoint_ref("t1", preserved_turn),
            ]
        ));
    }

    socket.close(None).await.unwrap();
    handle.shutdown();
    handle.join().await.unwrap();
    delivery.shutdown().await;
    supervisor.shutdown().await.unwrap();
    engine.shutdown().await;
}

/// A session that reports its work, so the supervisor suspends it once a turn ends and nothing
/// runs. `starts` sessions are prepared: the first launch, and each relaunch after it.
fn idle_suspending_state(starts: usize) -> Arc<StdMutex<DriverState>> {
    Arc::new(StdMutex::new(DriverState {
        start_results: (0..starts)
            .map(|_| {
                Ok(StartedSession {
                    activity_capabilities: ActivityCapabilities::structured_full(false),
                    ..started_session_for_provider("codex", "provider-session-1")
                })
            })
            .collect(),
        ..DriverState::default()
    }))
}

/// Ends one turn and waits until the idle deadline has suspended the session.
async fn finish_a_turn_and_wait_for_idle_suspension(
    supervisor: &ProviderRuntimeSupervisor,
    events_tx: &mpsc::Sender<ProviderEvent>,
) {
    events_tx
        .send(ProviderEvent {
            native_event_id: None,
            event_type: "turn.completed".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: Some("turn-2".to_owned()),
            item_id: None,
            request_id: None,
            payload: json!({"messageId":"assistant-2","state":"completed"}),
            activity: Vec::new(),
            activity_controls: Default::default(),
        })
        .await
        .unwrap();
    wait_until_suspended(supervisor).await;
}

async fn wait_until_suspended(supervisor: &ProviderRuntimeSupervisor) {
    timeout(Duration::from_secs(10), async {
        while supervisor
            .capture_session_identity("t1")
            .await
            .unwrap()
            .is_some()
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the idle session is suspended");
}

const REVERT_IDLE_TIMEOUT: Duration = Duration::from_millis(200);

/// Revert on a thread whose session was suspended after a turn reopens it the way a new message
/// does, with the saved resume cursor, rewinds, restores the files, and lets it idle again.
#[tokio::test]
async fn checkpoint_revert_resumes_an_idle_suspended_session_then_rewinds() {
    let repository = initialize_repository();
    let engine = engine().await;
    for (turn_count, content) in [(0, "baseline\n"), (1, "one\n"), (2, "two\n")] {
        project_checkpoint(&engine, repository.path(), turn_count, content).await;
    }
    let state = idle_suspending_state(2);
    let (events_tx, events_rx) = mpsc::channel(8);
    let (_relaunch_events_tx, relaunch_events_rx) = mpsc::channel(8);
    let supervisor = Arc::new(ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::from([events_rx, relaunch_events_rx])),
        }),
        activity_projection(&engine),
        SupervisorOptions {
            session_idle_timeout: REVERT_IDLE_TIMEOUT,
            ..SupervisorOptions::default()
        },
    ));
    let mut provider_launch = launch();
    provider_launch.cwd = repository.path().to_path_buf();
    supervisor.launch(provider_launch).await.unwrap();
    finish_a_turn_and_wait_for_idle_suspension(&supervisor, &events_tx).await;
    let effects = OrchestrationEffects::start(
        engine.clone(),
        Arc::new(GitRepository::default()),
        supervisor_effects(&supervisor, &engine, repository.path()),
        EffectsOptions::default(),
    )
    .await
    .unwrap();
    let mut events = engine.subscribe_events();

    engine
        .dispatch(
            serde_json::from_value(json!({
                "type":"thread.checkpoint.revert",
                "commandId":"revert-after-suspension",
                "threadId":"t1",
                "turnCount":1,
                "createdAt":NOW
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    wait_for_event(&mut events, |event| {
        event.event.event_type == "thread.reverted"
            || event.event.payload["activity"]["kind"] == "checkpoint.revert.failed"
    })
    .await;
    effects.shutdown().await;

    let failures = engine
        .read_events(0)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| event.event.payload["activity"]["kind"] == "checkpoint.revert.failed")
        .map(|event| event.event.payload["activity"]["payload"]["detail"].clone())
        .collect::<Vec<_>>();
    assert!(failures.is_empty(), "{failures:?}");
    assert_eq!(
        std::fs::read_to_string(repository.path().join("tracked.txt"))
            .unwrap()
            .replace("\r\n", "\n"),
        "one\n"
    );
    {
        let state = state.lock().unwrap();
        assert_eq!(state.rollbacks, [1], "the resumed session rewinds once");
        assert_eq!(
            state.launches.len(),
            2,
            "the suspended session is relaunched"
        );
        assert_eq!(
            state.launches[1].resume_cursor,
            Some(json!({"threadId":"provider-session-1"})),
            "the relaunch resumes the saved conversation"
        );
    }
    wait_until_suspended(&supervisor).await;

    supervisor.shutdown().await.unwrap();
    engine.shutdown().await;
}

/// If the suspended session cannot be reopened, the revert fails before any file changes and
/// says why.
#[tokio::test]
async fn checkpoint_revert_that_cannot_resume_the_session_changes_nothing() {
    let repository = initialize_repository();
    let engine = engine().await;
    for (turn_count, content) in [(0, "baseline\n"), (1, "one\n"), (2, "two\n")] {
        project_checkpoint(&engine, repository.path(), turn_count, content).await;
    }
    let state = idle_suspending_state(1);
    state
        .lock()
        .unwrap()
        .start_results
        .push_back(Err(ProviderRuntimeError::Spawn {
            provider: "codex".to_owned(),
            detail: "the executable exited with status 1".to_owned(),
        }));
    let (events_tx, events_rx) = mpsc::channel(8);
    let (_relaunch_events_tx, relaunch_events_rx) = mpsc::channel(8);
    let supervisor = Arc::new(ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::from([events_rx, relaunch_events_rx])),
        }),
        activity_projection(&engine),
        SupervisorOptions {
            session_idle_timeout: REVERT_IDLE_TIMEOUT,
            ..SupervisorOptions::default()
        },
    ));
    let mut provider_launch = launch();
    provider_launch.cwd = repository.path().to_path_buf();
    supervisor.launch(provider_launch).await.unwrap();
    finish_a_turn_and_wait_for_idle_suspension(&supervisor, &events_tx).await;
    let effects = OrchestrationEffects::start(
        engine.clone(),
        Arc::new(GitRepository::default()),
        supervisor_effects(&supervisor, &engine, repository.path()),
        EffectsOptions::default(),
    )
    .await
    .unwrap();
    let mut events = engine.subscribe_events();

    engine
        .dispatch(
            serde_json::from_value(json!({
                "type":"thread.checkpoint.revert",
                "commandId":"revert-resume-fails",
                "threadId":"t1",
                "turnCount":1,
                "createdAt":NOW
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    wait_for_event(&mut events, |event| {
        event.event.event_type == "thread.activity-appended"
            && event.event.payload["activity"]["kind"] == "checkpoint.revert.failed"
    })
    .await;
    effects.shutdown().await;

    assert_eq!(
        std::fs::read_to_string(repository.path().join("tracked.txt"))
            .unwrap()
            .replace("\r\n", "\n"),
        "two\n",
        "a failed resume restores no file"
    );
    let events = engine.read_events(0).await.unwrap();
    let detail = events
        .iter()
        .find(|event| event.event.payload["activity"]["kind"] == "checkpoint.revert.failed")
        .and_then(|event| event.event.payload["activity"]["payload"]["detail"].as_str())
        .expect("failure detail")
        .to_owned();
    assert!(
        detail.contains("so nothing was reverted") && detail.contains("could not start"),
        "{detail}"
    );
    assert!(
        !events
            .iter()
            .any(|event| event.event.event_type == "thread.reverted")
    );
    assert!(state.lock().unwrap().rollbacks.is_empty());

    supervisor.shutdown().await.unwrap();
    engine.shutdown().await;
}

/// A thread with no saved conversation for its provider has nothing to rewind: the revert starts
/// no session and changes nothing.
#[tokio::test]
async fn checkpoint_revert_without_a_saved_conversation_starts_nothing() {
    let repository = initialize_repository();
    let engine = engine().await;
    for (turn_count, content) in [(0, "baseline\n"), (1, "one\n"), (2, "two\n")] {
        project_checkpoint(&engine, repository.path(), turn_count, content).await;
    }
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let supervisor = Arc::new(ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::new()),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    ));
    let effects = OrchestrationEffects::start(
        engine.clone(),
        Arc::new(GitRepository::default()),
        supervisor_effects(&supervisor, &engine, repository.path()),
        EffectsOptions::default(),
    )
    .await
    .unwrap();
    let mut events = engine.subscribe_events();

    engine
        .dispatch(
            serde_json::from_value(json!({
                "type":"thread.checkpoint.revert",
                "commandId":"revert-without-conversation",
                "threadId":"t1",
                "turnCount":1,
                "createdAt":NOW
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    wait_for_event(&mut events, |event| {
        event.event.event_type == "thread.activity-appended"
            && event.event.payload["activity"]["kind"] == "checkpoint.revert.failed"
    })
    .await;
    effects.shutdown().await;

    assert!(
        state.lock().unwrap().launches.is_empty(),
        "no session starts"
    );
    assert_eq!(
        std::fs::read_to_string(repository.path().join("tracked.txt"))
            .unwrap()
            .replace("\r\n", "\n"),
        "two\n"
    );
    assert!(engine.read_events(0).await.unwrap().iter().any(|event| {
        event.event.payload["activity"]["payload"]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("no saved"))
    }));

    supervisor.shutdown().await.unwrap();
    engine.shutdown().await;
}

/// A provider that cannot rewind its conversation (Claude here) gets its files restored and a
/// notice that the conversation was not rewound; its messages, checkpoints and turn count stay.
#[tokio::test]
async fn checkpoint_revert_on_a_provider_that_cannot_rewind_restores_files_only() {
    let repository = initialize_repository();
    let engine = engine().await;
    for (turn_count, content) in [(0, "baseline\n"), (1, "one\n"), (2, "two\n")] {
        project_checkpoint(&engine, repository.path(), turn_count, content).await;
    }
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (_events_tx, events_rx) = mpsc::channel(8);
    let supervisor = Arc::new(ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(FakeFactory {
            state: state.clone(),
            events: StdMutex::new(VecDeque::from([events_rx])),
        }),
        activity_projection(&engine),
        SupervisorOptions::default(),
    ));
    let mut provider_launch = launch();
    provider_launch.provider = "claude".to_owned();
    provider_launch.provider_label = "Claude".to_owned();
    provider_launch.provider_instance_id = Some("claude".to_owned());
    provider_launch.codex_home = None;
    provider_launch.cwd = repository.path().to_path_buf();
    supervisor.launch(provider_launch).await.unwrap();
    let effects = OrchestrationEffects::start(
        engine.clone(),
        Arc::new(GitRepository::default()),
        supervisor_effects(&supervisor, &engine, repository.path()),
        EffectsOptions::default(),
    )
    .await
    .unwrap();
    let mut events = engine.subscribe_events();

    engine
        .dispatch(
            serde_json::from_value(json!({
                "type":"thread.checkpoint.revert",
                "commandId":"files-only-revert",
                "threadId":"t1",
                "turnCount":1,
                "createdAt":NOW
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    wait_for_event(&mut events, |event| {
        event.event.event_type == "thread.activity-appended"
            && event.event.payload["activity"]["kind"] == "checkpoint.revert.files-only"
    })
    .await;

    effects.shutdown().await;
    assert_eq!(
        std::fs::read_to_string(repository.path().join("tracked.txt"))
            .unwrap()
            .replace("\r\n", "\n"),
        "one\n",
        "the checkpoint files are restored"
    );
    assert!(
        state.lock().unwrap().rollbacks.is_empty(),
        "a provider that cannot rewind is never asked to"
    );
    let events = engine.read_events(0).await.unwrap();
    let notice = events
        .iter()
        .find(|event| event.event.payload["activity"]["kind"] == "checkpoint.revert.files-only")
        .expect("files-only notice");
    assert_eq!(notice.event.payload["activity"]["tone"], "info");
    assert!(
        notice.event.payload["activity"]["payload"]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("Claude can't rewind its conversation")),
        "{notice:?}"
    );
    assert!(
        !events.iter().any(|event| {
            event.event.event_type == "thread.reverted"
                || event.event.payload["activity"]["kind"] == "checkpoint.revert.failed"
        }),
        "the conversation is kept and the revert is not an error"
    );
    assert!(git_succeeds(
        repository.path(),
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &orchestration_effects::checkpoint_ref("t1", 2),
        ]
    ));

    supervisor.shutdown().await.unwrap();
    engine.shutdown().await;
}

/// A Codex app-server from before history modes: its threads carry no `historyMode`, and it rejects
/// any method it does not know while decoding the request.
const OLD_CODEX_FIXTURE: &str = r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  [ -n "$id" ] || continue
  case "$line" in
    *'"method":"initialize"'*) printf '{"id":%s,"result":{"userAgent":"codex_cli_rs/0.147.0"}}\n' "$id" ;;
    *'"method":"thread/start"'*) printf '{"id":%s,"result":{"cwd":"/tmp","model":"gpt-5","thread":{"id":"old-codex-thread"}}}\n' "$id" ;;
    *'"method":"mcpServerStatus/list"'*) printf '{"id":%s,"result":{"data":[],"nextCursor":null}}\n' "$id" ;;
    *'"method":"shutdown"'*) printf '{"id":%s,"result":null}\n' "$id" ;;
    *) printf '{"id":%s,"error":{"code":-32600,"message":"Invalid request: unknown variant"}}\n' "$id" ;;
  esac
done
"#;

const WINDOWS_OLD_CODEX_FIXTURE: &str = r#"
while ($null -ne ($line = [Console]::In.ReadLine())) {
  try { $request = $line | ConvertFrom-Json } catch { continue }
  if ($null -eq $request.id) { continue }
  $id = [string]$request.id
  switch ([string]$request.method) {
    "initialize" { $response = '{"id":' + $id + ',"result":{"userAgent":"codex_cli_rs/0.147.0"}}' }
    "thread/start" { $response = '{"id":' + $id + ',"result":{"cwd":"C:\\tmp","model":"gpt-5","thread":{"id":"old-codex-thread"}}}' }
    "mcpServerStatus/list" { $response = '{"id":' + $id + ',"result":{"data":[],"nextCursor":null}}' }
    "shutdown" { $response = '{"id":' + $id + ',"result":null}' }
    default { $response = '{"id":' + $id + ',"error":{"code":-32600,"message":"Invalid request: unknown variant"}}' }
  }
  [Console]::Out.WriteLine($response)
  [Console]::Out.Flush()
}
"#;

/// Reverting a thread on a Codex too old to rewind restores no file and says how to fix it.
#[tokio::test]
async fn checkpoint_revert_on_an_old_codex_restores_nothing_and_asks_for_an_update() {
    let repository = initialize_repository();
    let engine = engine().await;
    for (turn_count, content) in [(0, "baseline\n"), (1, "one\n"), (2, "two\n")] {
        project_checkpoint(&engine, repository.path(), turn_count, content).await;
    }
    let temp = TempDir::new().unwrap();
    let executable = executable_fixture(
        &temp,
        "old-codex",
        OLD_CODEX_FIXTURE,
        WINDOWS_OLD_CODEX_FIXTURE,
    );
    let supervisor = Arc::new(ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(NativeProviderDriverFactory::new(
            temp.path().join("attachments"),
        )),
        activity_projection(&engine),
        SupervisorOptions::default(),
    ));
    let mut provider_launch = launch();
    provider_launch.binary_path = executable.to_string_lossy().into_owned();
    provider_launch.cwd = repository.path().to_path_buf();
    timeout(
        CHECKPOINT_RPC_INTEGRATION_DEADLINE,
        supervisor.launch(provider_launch),
    )
    .await
    .expect("the old Codex fixture starts in time")
    .unwrap();
    let effects = OrchestrationEffects::start(
        engine.clone(),
        Arc::new(GitRepository::default()),
        supervisor_effects(&supervisor, &engine, repository.path()),
        EffectsOptions::default(),
    )
    .await
    .unwrap();
    let mut events = engine.subscribe_events();

    engine
        .dispatch(
            serde_json::from_value(json!({
                "type":"thread.checkpoint.revert",
                "commandId":"old-codex-revert",
                "threadId":"t1",
                "turnCount":1,
                "createdAt":NOW
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    wait_for_event(&mut events, |event| {
        event.event.event_type == "thread.activity-appended"
            && event.event.payload["activity"]["kind"] == "checkpoint.revert.failed"
    })
    .await;

    effects.shutdown().await;
    assert_eq!(
        std::fs::read_to_string(repository.path().join("tracked.txt"))
            .unwrap()
            .replace("\r\n", "\n"),
        "two\n",
        "an old Codex must not leave the files reverted"
    );
    let events = engine.read_events(0).await.unwrap();
    assert!(
        events.iter().any(|event| {
            event.event.payload["activity"]["kind"] == "checkpoint.revert.failed"
                && event.event.payload["activity"]["payload"]["detail"]
                    .as_str()
                    .is_some_and(|detail| detail.contains("Update Codex to 0.151.0 or later"))
        }),
        "the work log says how to fix it"
    );
    assert!(
        !events
            .iter()
            .any(|event| event.event.event_type == "thread.reverted")
    );
    assert!(git_succeeds(
        repository.path(),
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &orchestration_effects::checkpoint_ref("t1", 2),
        ]
    ));

    supervisor.shutdown().await.unwrap();
    engine.shutdown().await;
}

#[tokio::test]
async fn restart_reconciliation_settles_a_persisted_partial_assistant_message() {
    let state = TempDir::new().expect("state directory");
    let database_path = state.path().join("provider-restart.sqlite3");
    {
        let database = Database::create_new(&database_path)
            .await
            .expect("persistent database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let engine = OrchestrationEngine::start(database.clone(), EngineOptions::default())
            .await
            .expect("first engine");
        for command in [
            json!({"type":"project.create","commandId":"restart-partial-project","projectId":"p1","title":"Project","workspaceRoot":"C:/repo","createdAt":NOW}),
            json!({"type":"thread.create","commandId":"restart-partial-thread","threadId":"t1","projectId":"p1","title":"Thread","modelSelection":{"instanceId":"codex","model":"gpt-5"},"runtimeMode":"full-access","createdAt":NOW}),
            json!({"type":"thread.session.set","commandId":"restart-partial-session","threadId":"t1","session":{"threadId":"t1","status":"running","providerName":"codex","providerInstanceId":"codex","runtimeMode":"full-access","activeTurnId":"provider-turn-1","lastError":null,"updatedAt":NOW},"createdAt":NOW}),
            json!({"type":"thread.message.assistant.delta","commandId":"restart-partial-delta","threadId":"t1","messageId":"assistant:t1:item:restart-partial","delta":"Partial response","turnId":"provider-turn-1","createdAt":NOW}),
        ] {
            engine
                .dispatch(serde_json::from_value(command).expect("fixture command"))
                .await
                .expect("fixture dispatch");
        }
        engine
            .repositories()
            .upsert_provider_session_runtime(ProviderSessionRuntime {
                thread_id: "t1".to_owned(),
                provider_name: "codex".to_owned(),
                provider_instance_id: Some("codex".to_owned()),
                adapter_key: "codex-app-server".to_owned(),
                runtime_mode: "full-access".to_owned(),
                status: "running".to_owned(),
                last_seen_at: NOW.to_owned(),
                resume_cursor: Some(json!({"threadId":"provider-thread-1"})),
                runtime_payload: None,
            })
            .await
            .expect("persisted runtime");
        assert!(
            engine
                .repositories()
                .get_message("assistant:t1:item:restart-partial".to_owned())
                .await
                .unwrap()
                .is_some_and(|message| message.is_streaming)
        );
        engine.shutdown().await;
        database
            .checkpoint_wal()
            .await
            .expect("checkpoint before restart");
    }

    let database = Database::open_existing(&database_path)
        .await
        .expect("reopened database");
    let engine = OrchestrationEngine::start(database, EngineOptions::default())
        .await
        .expect("restarted engine");
    reconcile_abandoned_provider_sessions(&engine)
        .await
        .expect("startup reconciliation");

    let snapshot = load_snapshot(&engine.repositories()).await.unwrap();
    let assistant_messages = snapshot
        .messages
        .iter()
        .filter(|message| message.role == "assistant")
        .map(|message| {
            (
                message.message_id.as_str(),
                message.text.as_str(),
                message.is_streaming,
            )
        })
        .collect::<Vec<_>>();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.thread_id == "t1")
        .expect("reconciled session");
    assert_eq!(
        (
            assistant_messages,
            session.status.as_str(),
            session.active_turn_id.as_deref(),
            snapshot
                .turns
                .iter()
                .find(|turn| turn.turn_id.as_deref() == Some("provider-turn-1"))
                .map(|turn| turn.state.as_str()),
        ),
        (
            vec![(
                "assistant:t1:item:restart-partial",
                "Partial response",
                false,
            )],
            "error",
            None,
            Some("error"),
        )
    );

    let event_count = engine.read_events(0).await.unwrap().len();
    reconcile_abandoned_provider_sessions(&engine)
        .await
        .expect("duplicate startup reconciliation");
    assert_eq!(
        engine.read_events(0).await.unwrap().len(),
        event_count,
        "completed reconciliation is idempotent"
    );
    engine.shutdown().await;
}

#[tokio::test]
async fn restart_reconciles_abandoned_running_provider_sessions() {
    let engine = engine().await;
    engine
        .dispatch(OrchestrationCommand::ThreadSessionSet {
            command_id: "running-session".to_owned(),
            thread_id: "t1".to_owned(),
            session: SessionInput {
                thread_id: "t1".to_owned(),
                status: "running".to_owned(),
                provider_name: Some("codex".to_owned()),
                provider_instance_id: Some("codex".to_owned()),
                runtime_mode: "full-access".to_owned(),
                active_turn_id: Some("provider-turn-1".to_owned()),
                last_error: None,
                last_error_class: None,
                updated_at: NOW.to_owned(),
            },
            created_at: NOW.to_owned(),
        })
        .await
        .unwrap();
    engine
        .repositories()
        .upsert_provider_session_runtime(ProviderSessionRuntime {
            thread_id: "t1".to_owned(),
            provider_name: "codex".to_owned(),
            provider_instance_id: Some("codex".to_owned()),
            adapter_key: "codex-app-server".to_owned(),
            runtime_mode: "full-access".to_owned(),
            status: "running".to_owned(),
            last_seen_at: NOW.to_owned(),
            resume_cursor: Some(json!({"threadId":"provider-thread-1"})),
            runtime_payload: None,
        })
        .await
        .unwrap();

    reconcile_abandoned_provider_sessions(&engine)
        .await
        .unwrap();

    let snapshot = load_snapshot(&engine.repositories()).await.unwrap();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.thread_id == "t1")
        .unwrap();
    assert_eq!(session.status, "error");
    assert_eq!(session.active_turn_id, None);
    assert_eq!(session.last_error_class.as_deref(), Some("session_stopped"));
    assert!(
        session
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("Review delivery status before continuing"))
    );
    let runtime = engine
        .repositories()
        .get_provider_session_runtime("t1".to_owned())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(runtime.status, "error");
    assert_eq!(
        runtime.resume_cursor,
        Some(json!({"threadId":"provider-thread-1"}))
    );
}

#[tokio::test]
async fn restart_reconciles_abandoned_ready_provider_sessions() {
    let engine = engine().await;
    engine
        .dispatch(OrchestrationCommand::ThreadSessionSet {
            command_id: "ready-session".to_owned(),
            thread_id: "t1".to_owned(),
            session: SessionInput {
                thread_id: "t1".to_owned(),
                status: "ready".to_owned(),
                provider_name: Some("codex".to_owned()),
                provider_instance_id: Some("codex".to_owned()),
                runtime_mode: "full-access".to_owned(),
                active_turn_id: None,
                last_error: None,
                last_error_class: None,
                updated_at: NOW.to_owned(),
            },
            created_at: NOW.to_owned(),
        })
        .await
        .unwrap();
    engine
        .repositories()
        .upsert_provider_session_runtime(ProviderSessionRuntime {
            thread_id: "t1".to_owned(),
            provider_name: "codex".to_owned(),
            provider_instance_id: Some("codex".to_owned()),
            adapter_key: "codex-app-server".to_owned(),
            runtime_mode: "full-access".to_owned(),
            status: "ready".to_owned(),
            last_seen_at: NOW.to_owned(),
            resume_cursor: Some(json!({"threadId":"provider-thread-ready"})),
            runtime_payload: None,
        })
        .await
        .unwrap();

    reconcile_abandoned_provider_sessions(&engine)
        .await
        .unwrap();

    let snapshot = load_snapshot(&engine.repositories()).await.unwrap();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.thread_id == "t1")
        .unwrap();
    assert_eq!(session.status, "error");
    assert_eq!(session.active_turn_id, None);
    assert!(
        session
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("Review delivery status before continuing"))
    );
    let runtime = engine
        .repositories()
        .get_provider_session_runtime("t1".to_owned())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(runtime.status, "error");
    assert_eq!(
        runtime.resume_cursor,
        Some(json!({"threadId":"provider-thread-ready"}))
    );
}

#[tokio::test]
async fn restart_ignores_intentionally_suspended_provider_sessions() {
    let engine = engine().await;
    project_session(&engine, "t1", "ready").await;
    engine
        .repositories()
        .upsert_provider_session_runtime(persisted_runtime("t1", "suspended", NOW))
        .await
        .unwrap();

    reconcile_abandoned_provider_sessions(&engine)
        .await
        .unwrap();

    let snapshot = load_snapshot(&engine.repositories()).await.unwrap();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.thread_id == "t1")
        .unwrap();
    assert_eq!(session.status, "ready");
    assert_eq!(session.last_error, None);
    assert_eq!(
        engine
            .repositories()
            .get_provider_session_runtime("t1".to_owned())
            .await
            .unwrap()
            .unwrap()
            .status,
        "suspended"
    );
}

#[tokio::test]
async fn reconciliation_keeps_projection_failures_retryable_and_continues_later_rows() {
    let engine = engine().await;
    engine
        .repositories()
        .upsert_provider_session_runtime(persisted_runtime(
            "missing-thread",
            "ready",
            "2026-07-10T09:59:00.000Z",
        ))
        .await
        .unwrap();
    project_session(&engine, "t1", "ready").await;
    engine
        .repositories()
        .upsert_provider_session_runtime(persisted_runtime("t1", "ready", NOW))
        .await
        .unwrap();

    reconcile_abandoned_provider_sessions(&engine)
        .await
        .expect("one malformed persisted row must not abort startup reconciliation");

    let missing = engine
        .repositories()
        .get_provider_session_runtime("missing-thread".to_owned())
        .await
        .unwrap()
        .unwrap();
    let valid = engine
        .repositories()
        .get_provider_session_runtime("t1".to_owned())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        missing.status, "ready",
        "failed projection remains retryable"
    );
    assert_eq!(valid.status, "error", "later rows still reconcile");
    let snapshot = load_snapshot(&engine.repositories()).await.unwrap();
    assert_eq!(
        snapshot
            .sessions
            .iter()
            .find(|session| session.thread_id == "t1")
            .unwrap()
            .status,
        "error"
    );

    engine
        .dispatch(
            serde_json::from_value(json!({
                "type":"thread.create",
                "commandId":"create-missing-thread",
                "threadId":"missing-thread",
                "projectId":"p1",
                "title":"Recovered thread",
                "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                "runtimeMode":"full-access",
                "branch":null,
                "worktreePath":null,
                "createdAt":NOW
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    reconcile_abandoned_provider_sessions(&engine)
        .await
        .unwrap();
    let event_count = engine.read_events(0).await.unwrap().len();
    reconcile_abandoned_provider_sessions(&engine)
        .await
        .unwrap();

    let recovered = engine
        .repositories()
        .get_provider_session_runtime("missing-thread".to_owned())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.status, "error");
    assert_eq!(engine.read_events(0).await.unwrap().len(), event_count);
}

#[tokio::test]
async fn reconciliation_discards_the_runtime_of_a_deleted_thread_instead_of_retrying_it() {
    for status in ["ready", "error"] {
        let (engine, _database) = engine_and_database().await;
        project_session(&engine, "t1", "running").await;
        engine
            .dispatch(
                serde_json::from_value(json!({
                    "type":"thread.delete", "commandId":"delete-t1", "threadId":"t1", "createdAt":NOW
                }))
                .unwrap(),
            )
            .await
            .unwrap();
        // A delivery retried against the deleted thread relaunches a session after the delete
        // projection removed the runtime row, which is how the orphan survives restarts.
        engine
            .repositories()
            .upsert_provider_session_runtime(persisted_runtime("t1", status, NOW))
            .await
            .unwrap();
        let event_count = engine.read_events(0).await.unwrap().len();

        reconcile_abandoned_provider_sessions(&engine)
            .await
            .unwrap();

        assert!(
            engine
                .repositories()
                .get_provider_session_runtime("t1".to_owned())
                .await
                .unwrap()
                .is_none(),
            "a {status} runtime for a deleted thread is discarded, not left for the next startup"
        );
        assert_eq!(engine.read_events(0).await.unwrap().len(), event_count);
        engine.shutdown().await;
    }
}

#[tokio::test]
async fn reconciliation_retries_runtime_write_after_projection_without_duplicate_events() {
    let (engine, database) = engine_and_database().await;
    project_session(&engine, "t1", "ready").await;
    engine
        .repositories()
        .upsert_provider_session_runtime(persisted_runtime("t1", "ready", NOW))
        .await
        .unwrap();
    database
        .call(|connection| {
            connection.execute_batch(
                "CREATE TRIGGER fail_provider_runtime_reconciliation
                 BEFORE UPDATE ON provider_session_runtime
                 WHEN NEW.thread_id = 't1' AND NEW.status = 'error'
                 BEGIN
                   SELECT RAISE(FAIL, 'injected provider runtime write failure');
                 END;",
            )?;
            Ok(())
        })
        .await
        .unwrap();

    reconcile_abandoned_provider_sessions(&engine)
        .await
        .expect("runtime write failure is isolated and retried on restart");

    let snapshot = load_snapshot(&engine.repositories()).await.unwrap();
    assert_eq!(
        snapshot
            .sessions
            .iter()
            .find(|session| session.thread_id == "t1")
            .unwrap()
            .status,
        "error",
        "projection is committed before marking the runtime row reconciled"
    );
    assert_eq!(
        engine
            .repositories()
            .get_provider_session_runtime("t1".to_owned())
            .await
            .unwrap()
            .unwrap()
            .status,
        "ready",
        "failed runtime write leaves the row eligible for retry"
    );
    let projected_event_count = engine.read_events(0).await.unwrap().len();

    database
        .call(|connection| {
            connection.execute_batch("DROP TRIGGER fail_provider_runtime_reconciliation;")?;
            Ok(())
        })
        .await
        .unwrap();
    reconcile_abandoned_provider_sessions(&engine)
        .await
        .unwrap();
    assert_eq!(
        engine
            .repositories()
            .get_provider_session_runtime("t1".to_owned())
            .await
            .unwrap()
            .unwrap()
            .status,
        "error"
    );
    assert_eq!(
        engine.read_events(0).await.unwrap().len(),
        projected_event_count,
        "retry reuses the same reconciliation command"
    );

    reconcile_abandoned_provider_sessions(&engine)
        .await
        .unwrap();
    assert_eq!(
        engine.read_events(0).await.unwrap().len(),
        projected_event_count,
        "completed reconciliation is idempotent"
    );
}

#[tokio::test]
async fn first_turn_autostarts_the_projected_native_provider() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (_events_tx, events_rx) = mpsc::channel(8);
    let factory = Arc::new(FakeFactory {
        state: state.clone(),
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    let command = OrchestrationCommand::ThreadTurnStart {
        command_id: "autostart-turn".to_owned(),
        thread_id: "t1".to_owned(),
        message: ThreadMessageInput {
            message_id: "autostart-message".to_owned(),
            role: "user".to_owned(),
            text: "start natively".to_owned(),
            attachments: vec![],
        },
        model_selection: None,
        title_seed: None,
        runtime_mode: "full-access".to_owned(),
        interaction_mode: "default".to_owned(),
        bootstrap: None,
        source_proposed_plan: None,
        queued: None,
        created_at: NOW.to_owned(),
    };
    engine.dispatch(command.clone()).await.unwrap();
    let settings = TempDir::new().unwrap();
    route_orchestration_command(
        &supervisor,
        &engine,
        &settings.path().to_path_buf(),
        command,
    )
    .await
    .unwrap();

    {
        let state = state.lock().unwrap();
        assert_eq!(state.starts, 1);
        assert_eq!(state.sends, ["start natively"]);
    }
    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn missing_runtime_accepts_durable_thread_settings_for_the_next_turn() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let factory = Arc::new(FakeFactory {
        state: state.clone(),
        events: StdMutex::new(VecDeque::new()),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    let settings = TempDir::new().unwrap();
    let commands = [
        json!({"type":"thread.runtime-mode.set","commandId":"runtime-mode","threadId":"t1","runtimeMode":"approval-required","createdAt":NOW}),
        json!({"type":"thread.interaction-mode.set","commandId":"interaction-mode","threadId":"t1","interactionMode":"plan","createdAt":NOW}),
        json!({"type":"thread.meta.update","commandId":"model","threadId":"t1","modelSelection":{"instanceId":"codex","model":"gpt-5.1"}}),
    ];

    for value in commands {
        let command: OrchestrationCommand = serde_json::from_value(value).unwrap();
        engine.dispatch(command.clone()).await.unwrap();
        route_orchestration_command(
            &supervisor,
            &engine,
            &settings.path().to_path_buf(),
            command,
        )
        .await
        .expect("durable settings remain valid without a live provider runtime");
    }

    assert_eq!(state.lock().unwrap().starts, 0);
    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn missing_runtime_rejects_ephemeral_actions_and_stops_idempotently() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let factory = Arc::new(FakeFactory {
        state,
        events: StdMutex::new(VecDeque::new()),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    let settings = TempDir::new().unwrap();
    let commands = [
        json!({"type":"thread.turn.interrupt","commandId":"interrupt","threadId":"t1","turnId":"turn-1","createdAt":NOW}),
        json!({"type":"thread.approval.respond","commandId":"approve","threadId":"t1","requestId":"r1","decision":"accept","createdAt":NOW}),
        json!({"type":"thread.user-input.respond","commandId":"answer","threadId":"t1","requestId":"r2","answers":{"q":"a"},"createdAt":NOW}),
    ];

    for value in commands {
        let command: OrchestrationCommand = serde_json::from_value(value).unwrap();
        let action = command.command_type().to_owned();
        let error = route_orchestration_command(
            &supervisor,
            &engine,
            &settings.path().to_path_buf(),
            command,
        )
        .await
        .expect_err("missing runtime command must fail");
        let message = error.to_string();

        assert!(matches!(
            error,
            ProviderRuntimeError::StaleSession {
                thread_id,
                action: failed_action,
            } if thread_id == "t1" && failed_action == action
        ));
        assert!(message.contains("start a new turn"), "{message}");
    }

    let stop = serde_json::from_value(json!({
        "type":"thread.session.stop", "commandId":"stop", "threadId":"t1", "createdAt":NOW
    }))
    .unwrap();
    route_orchestration_command(&supervisor, &engine, &settings.path().to_path_buf(), stop)
        .await
        .expect("stopping a missing runtime is idempotent");

    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn persisted_session_without_live_runtime_rejects_commands_until_next_turn_start() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (_events_tx, events_rx) = mpsc::channel(8);
    let first_factory = Arc::new(FakeFactory {
        state: state.clone(),
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let first_supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        first_factory,
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    first_supervisor.launch(launch()).await.unwrap();

    let replacement_factory = Arc::new(FakeFactory {
        state: state.clone(),
        events: StdMutex::new(VecDeque::new()),
    });
    let replacement_supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        replacement_factory,
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    let settings = TempDir::new().unwrap();
    let command: OrchestrationCommand = serde_json::from_value(json!({
        "type":"thread.approval.respond",
        "commandId":"stale-approval",
        "threadId":"t1",
        "requestId":"r1",
        "decision":"accept",
        "createdAt":NOW
    }))
    .unwrap();

    let error = route_orchestration_command(
        &replacement_supervisor,
        &engine,
        &settings.path().to_path_buf(),
        command,
    )
    .await
    .expect_err("lost live runtime must not acknowledge the command");

    assert!(matches!(
        error,
        ProviderRuntimeError::StaleSession { thread_id, action }
            if thread_id == "t1" && action == "thread.approval.respond"
    ));
    assert_eq!(state.lock().unwrap().starts, 1);

    replacement_supervisor.shutdown().await.unwrap();
    first_supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn normalizes_provider_approval_events_into_orchestration_projection() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (events_tx, events_rx) = mpsc::channel(8);
    let factory = Arc::new(FakeFactory {
        state,
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    supervisor.launch(launch()).await.unwrap();
    events_tx
        .send(ProviderEvent {
            native_event_id: None,
            event_type: "request.opened".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: Some("turn-1".to_owned()),
            item_id: None,
            request_id: Some("approval-1".to_owned()),
            payload: json!({"requestType":"command_execution_approval","detail":"cargo test"}),
            activity: Vec::new(),
            activity_controls: Default::default(),
        })
        .await
        .unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let approvals = engine
                .repositories()
                .list_pending_approvals_by_thread("t1".to_owned())
                .await
                .unwrap();
            if approvals.len() == 1 {
                break approvals;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn projects_distinct_provider_messages_and_settles_the_completed_turn() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (events_tx, events_rx) = mpsc::channel(8);
    let factory = Arc::new(FakeFactory {
        state,
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    supervisor.launch(launch()).await.unwrap();
    let start = OrchestrationCommand::ThreadTurnStart {
        command_id: "turn".to_owned(),
        thread_id: "t1".to_owned(),
        message: ThreadMessageInput {
            message_id: "m1".to_owned(),
            role: "user".to_owned(),
            text: "hello".to_owned(),
            attachments: vec![],
        },
        model_selection: None,
        title_seed: None,
        runtime_mode: "full-access".to_owned(),
        interaction_mode: "default".to_owned(),
        bootstrap: None,
        source_proposed_plan: None,
        queued: None,
        created_at: NOW.to_owned(),
    };
    engine.dispatch(start.clone()).await.unwrap();
    supervisor.handle_orchestration(start).await.unwrap();

    for event in [
        ProviderEvent {
            native_event_id: None,
            event_type: "content.delta".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: Some("provider-turn-1".to_owned()),
            item_id: Some("commentary-1".to_owned()),
            request_id: None,
            payload: json!({"streamKind":"assistant_text","delta":"First"}),
            activity: Vec::new(),
            activity_controls: Default::default(),
        },
        ProviderEvent {
            native_event_id: None,
            event_type: "content.delta".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: Some("provider-turn-1".to_owned()),
            item_id: Some("commentary-1".to_owned()),
            request_id: None,
            payload: json!({"streamKind":"assistant_text","delta":"."}),
            activity: Vec::new(),
            activity_controls: Default::default(),
        },
        ProviderEvent {
            native_event_id: None,
            event_type: "content.delta".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: Some("provider-turn-1".to_owned()),
            item_id: Some("final-1".to_owned()),
            request_id: None,
            payload: json!({"streamKind":"assistant_text","delta":"Second."}),
            activity: Vec::new(),
            activity_controls: Default::default(),
        },
        ProviderEvent {
            native_event_id: None,
            event_type: "turn.completed".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: Some("provider-turn-1".to_owned()),
            item_id: None,
            request_id: None,
            payload: json!({"state":"completed"}),
            activity: Vec::new(),
            activity_controls: Default::default(),
        },
    ] {
        events_tx.send(event).await.unwrap();
    }

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let snapshot = load_snapshot(&engine.repositories()).await.unwrap();
            if snapshot.activities.iter().any(|activity| {
                activity.thread_id == "t1"
                    && activity.payload["eventType"] == "turn.completed"
                    && activity.payload["state"] == "completed"
            }) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("provider completion must settle the projected turn");

    let messages = engine
        .repositories()
        .list_messages_by_thread("t1".to_owned())
        .await
        .unwrap();
    let assistant_messages = messages
        .iter()
        .filter(|message| message.role == "assistant")
        .map(|message| {
            (
                message.message_id.as_str(),
                message.text.as_str(),
                message.is_streaming,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        assistant_messages,
        vec![
            ("assistant:t1:item:commentary-1", "First.", false),
            ("assistant:t1:item:final-1", "Second.", false),
        ]
    );
    assert!(
        messages
            .iter()
            .all(|message| message.text != "First.Second.")
    );
    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn exact_completion_settles_the_existing_native_item_message() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (events_tx, events_rx) = mpsc::channel(8);
    let factory = Arc::new(FakeFactory {
        state,
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    supervisor.launch(launch()).await.unwrap();

    for event in [
        ProviderEvent {
            native_event_id: None,
            event_type: "content.delta".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: Some("provider-turn-1".to_owned()),
            item_id: Some("native-message-1".to_owned()),
            request_id: None,
            payload: json!({"streamKind":"assistant_text","delta":"Exact response"}),
            activity: Vec::new(),
            activity_controls: Default::default(),
        },
        ProviderEvent {
            native_event_id: None,
            event_type: "message.assistant.completed".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: Some("provider-turn-1".to_owned()),
            item_id: Some("native-message-1".to_owned()),
            request_id: None,
            payload: json!({"messageId":"legacy-payload-id"}),
            activity: Vec::new(),
            activity_controls: Default::default(),
        },
    ] {
        events_tx.send(event).await.unwrap();
    }

    let message = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Some(message) = engine
                .repositories()
                .get_message("assistant:t1:item:native-message-1".to_owned())
                .await
                .unwrap()
                .filter(|message| !message.is_streaming)
            {
                break message;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("exact completion must settle the native item row");
    assert_eq!(message.text, "Exact response");
    assert_eq!(message.turn_id.as_deref(), Some("provider-turn-1"));
    let assistant_messages = engine
        .repositories()
        .list_messages_by_thread("t1".to_owned())
        .await
        .unwrap()
        .into_iter()
        .filter(|message| message.role == "assistant")
        .map(|message| message.message_id)
        .collect::<Vec<_>>();
    assert_eq!(
        assistant_messages,
        vec!["assistant:t1:item:native-message-1"]
    );
    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn unidentified_provider_chunks_share_one_settled_turn_message() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (events_tx, events_rx) = mpsc::channel(8);
    let factory = Arc::new(FakeFactory {
        state,
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    supervisor.launch(launch()).await.unwrap();
    let start = OrchestrationCommand::ThreadTurnStart {
        command_id: "unidentified-turn".to_owned(),
        thread_id: "t1".to_owned(),
        message: ThreadMessageInput {
            message_id: "m-unidentified".to_owned(),
            role: "user".to_owned(),
            text: "hello".to_owned(),
            attachments: vec![],
        },
        model_selection: None,
        title_seed: None,
        runtime_mode: "full-access".to_owned(),
        interaction_mode: "default".to_owned(),
        bootstrap: None,
        source_proposed_plan: None,
        queued: None,
        created_at: NOW.to_owned(),
    };
    engine.dispatch(start.clone()).await.unwrap();
    supervisor.handle_orchestration(start).await.unwrap();

    for event in [
        ProviderEvent {
            native_event_id: None,
            event_type: "content.delta".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: Some("provider-turn-1".to_owned()),
            item_id: None,
            request_id: None,
            payload: json!({"streamKind":"assistant_text","delta":"hello "}),
            activity: Vec::new(),
            activity_controls: Default::default(),
        },
        ProviderEvent {
            native_event_id: None,
            event_type: "content.delta".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: Some("provider-turn-1".to_owned()),
            item_id: None,
            request_id: None,
            payload: json!({"streamKind":"assistant_text","delta":"from cursor"}),
            activity: Vec::new(),
            activity_controls: Default::default(),
        },
        ProviderEvent {
            native_event_id: None,
            event_type: "turn.completed".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: Some("provider-turn-1".to_owned()),
            item_id: None,
            request_id: None,
            payload: json!({"state":"completed"}),
            activity: Vec::new(),
            activity_controls: Default::default(),
        },
    ] {
        events_tx.send(event).await.unwrap();
    }

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let snapshot = load_snapshot(&engine.repositories()).await.unwrap();
            if snapshot.activities.iter().any(|activity| {
                activity.thread_id == "t1"
                    && activity.payload["eventType"] == "turn.completed"
                    && activity.payload["state"] == "completed"
            }) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("unidentified provider completion must reach terminal projection");

    let messages = engine
        .repositories()
        .list_messages_by_thread("t1".to_owned())
        .await
        .unwrap();
    let assistant_messages = messages
        .iter()
        .filter(|message| message.role == "assistant")
        .map(|message| {
            (
                message.message_id.as_str(),
                message.text.as_str(),
                message.is_streaming,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        assistant_messages,
        vec![(
            "assistant:t1:turn:provider-turn-1",
            "hello from cursor",
            false,
        )]
    );
    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn completion_without_assistant_text_does_not_create_a_message() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (events_tx, events_rx) = mpsc::channel(8);
    let factory = Arc::new(FakeFactory {
        state,
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    supervisor.launch(launch()).await.unwrap();
    let start = OrchestrationCommand::ThreadTurnStart {
        command_id: "empty-turn".to_owned(),
        thread_id: "t1".to_owned(),
        message: ThreadMessageInput {
            message_id: "m-empty".to_owned(),
            role: "user".to_owned(),
            text: "hello".to_owned(),
            attachments: vec![],
        },
        model_selection: None,
        title_seed: None,
        runtime_mode: "full-access".to_owned(),
        interaction_mode: "default".to_owned(),
        bootstrap: None,
        source_proposed_plan: None,
        queued: None,
        created_at: NOW.to_owned(),
    };
    engine.dispatch(start.clone()).await.unwrap();
    supervisor.handle_orchestration(start).await.unwrap();

    for event in [
        ProviderEvent {
            native_event_id: None,
            event_type: "message.assistant.completed".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: Some("provider-turn-1".to_owned()),
            item_id: Some("empty-1".to_owned()),
            request_id: None,
            payload: json!({}),
            activity: Vec::new(),
            activity_controls: Default::default(),
        },
        ProviderEvent {
            native_event_id: None,
            event_type: "turn.completed".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: Some("provider-turn-1".to_owned()),
            item_id: None,
            request_id: None,
            payload: json!({"state":"completed"}),
            activity: Vec::new(),
            activity_controls: Default::default(),
        },
    ] {
        events_tx.send(event).await.unwrap();
    }

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let snapshot = load_snapshot(&engine.repositories()).await.unwrap();
            if snapshot.activities.iter().any(|activity| {
                activity.thread_id == "t1"
                    && activity.payload["eventType"] == "turn.completed"
                    && activity.payload["state"] == "completed"
            }) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("textless provider completion must reach terminal projection");

    let messages = engine
        .repositories()
        .list_messages_by_thread("t1".to_owned())
        .await
        .unwrap();
    let assistant_messages = messages
        .iter()
        .filter(|message| message.role == "assistant")
        .map(|message| {
            (
                message.message_id.as_str(),
                message.text.as_str(),
                message.is_streaming,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(assistant_messages, Vec::<(&str, &str, bool)>::new());
    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_and_interrupted_turns_settle_existing_assistant_messages() {
    let mut outcomes = Vec::new();

    for terminal_state in ["failed", "interrupted"] {
        let engine = engine().await;
        let state = Arc::new(StdMutex::new(DriverState::default()));
        let (events_tx, events_rx) = mpsc::channel(8);
        let factory = Arc::new(FakeFactory {
            state,
            events: StdMutex::new(VecDeque::from([events_rx])),
        });
        let supervisor = ProviderRuntimeSupervisor::start(
            engine.clone(),
            factory,
            activity_projection(&engine),
            SupervisorOptions::default(),
        );
        supervisor.launch(launch()).await.unwrap();
        let start = OrchestrationCommand::ThreadTurnStart {
            command_id: format!("{terminal_state}-turn"),
            thread_id: "t1".to_owned(),
            message: ThreadMessageInput {
                message_id: format!("m-{terminal_state}"),
                role: "user".to_owned(),
                text: "hello".to_owned(),
                attachments: vec![],
            },
            model_selection: None,
            title_seed: None,
            runtime_mode: "full-access".to_owned(),
            interaction_mode: "default".to_owned(),
            bootstrap: None,
            source_proposed_plan: None,
            queued: None,
            created_at: NOW.to_owned(),
        };
        engine.dispatch(start.clone()).await.unwrap();
        supervisor.handle_orchestration(start).await.unwrap();

        for event in [
            ProviderEvent {
                native_event_id: None,
                event_type: "content.delta".to_owned(),
                thread_id: "t1".to_owned(),
                turn_id: Some("provider-turn-1".to_owned()),
                item_id: Some("partial-1".to_owned()),
                request_id: None,
                payload: json!({"streamKind":"assistant_text","delta":"Partial response"}),
                activity: Vec::new(),
                activity_controls: Default::default(),
            },
            ProviderEvent {
                native_event_id: None,
                event_type: "turn.completed".to_owned(),
                thread_id: "t1".to_owned(),
                turn_id: Some("provider-turn-1".to_owned()),
                item_id: None,
                request_id: None,
                payload: if terminal_state == "failed" {
                    json!({"state":"failed","error":{"message":"model unavailable"}})
                } else {
                    json!({"state":"interrupted"})
                },
                activity: Vec::new(),
                activity_controls: Default::default(),
            },
        ] {
            events_tx.send(event).await.unwrap();
        }

        let snapshot = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let snapshot = load_snapshot(&engine.repositories()).await.unwrap();
                if snapshot.activities.iter().any(|activity| {
                    activity.thread_id == "t1"
                        && activity.payload["eventType"] == "turn.completed"
                        && activity.payload["state"] == terminal_state
                }) {
                    break snapshot;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("provider terminal event must reach orchestration projection");

        let messages = engine
            .repositories()
            .list_messages_by_thread("t1".to_owned())
            .await
            .unwrap()
            .into_iter()
            .filter(|message| message.role == "assistant")
            .map(|message| (message.message_id, message.text, message.is_streaming))
            .collect::<Vec<_>>();
        let session_error = snapshot
            .sessions
            .iter()
            .find(|session| session.thread_id == "t1")
            .and_then(|session| session.last_error.clone());
        let has_provider_error = snapshot.activities.iter().any(|activity| {
            activity.thread_id == "t1"
                && activity.kind == "provider.error"
                && activity.payload["error"]["message"] == "model unavailable"
        });
        outcomes.push((
            terminal_state.to_owned(),
            messages,
            session_error,
            has_provider_error,
        ));
        supervisor.shutdown().await.unwrap();
    }

    assert_eq!(
        outcomes,
        vec![
            (
                "failed".to_owned(),
                vec![(
                    "assistant:t1:item:partial-1".to_owned(),
                    "Partial response".to_owned(),
                    false,
                )],
                Some("model unavailable".to_owned()),
                true,
            ),
            (
                "interrupted".to_owned(),
                vec![(
                    "assistant:t1:item:partial-1".to_owned(),
                    "Partial response".to_owned(),
                    false,
                )],
                None,
                false,
            ),
        ]
    );
}

async fn project_terminal_with_completion_failures(
    failure_count: usize,
    recover_failed_message_late: bool,
) -> (
    Vec<(String, String, bool)>,
    Option<String>,
    bool,
    Option<String>,
) {
    let hooks = TestHooks::default();
    let (engine, _) = engine_and_database_with_options(EngineOptions {
        test_hooks: hooks.clone(),
        ..EngineOptions::default()
    })
    .await;
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (events_tx, events_rx) = mpsc::channel(8);
    let factory = Arc::new(FakeFactory {
        state,
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    supervisor.launch(launch()).await.unwrap();
    let start = OrchestrationCommand::ThreadTurnStart {
        command_id: "terminal-failure-turn".to_owned(),
        thread_id: "t1".to_owned(),
        message: ThreadMessageInput {
            message_id: "m-terminal-failure".to_owned(),
            role: "user".to_owned(),
            text: "hello".to_owned(),
            attachments: vec![],
        },
        model_selection: None,
        title_seed: None,
        runtime_mode: "full-access".to_owned(),
        interaction_mode: "default".to_owned(),
        bootstrap: None,
        source_proposed_plan: None,
        queued: None,
        created_at: NOW.to_owned(),
    };
    engine.dispatch(start.clone()).await.unwrap();
    supervisor.handle_orchestration(start).await.unwrap();

    for item_id in ["partial-1", "partial-2"] {
        events_tx
            .send(ProviderEvent {
                native_event_id: None,
                event_type: "content.delta".to_owned(),
                thread_id: "t1".to_owned(),
                turn_id: Some("provider-turn-1".to_owned()),
                item_id: Some(item_id.to_owned()),
                request_id: None,
                payload: json!({"streamKind":"assistant_text","delta":item_id}),
                activity: Vec::new(),
                activity_controls: Default::default(),
            })
            .await
            .unwrap();
    }
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let messages = engine
                .repositories()
                .list_messages_by_thread("t1".to_owned())
                .await
                .unwrap();
            if messages
                .iter()
                .filter(|message| message.role == "assistant" && message.is_streaming)
                .count()
                == 2
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("both assistant item rows must exist before terminal failure injection");

    hooks.fail_next_projectors(
        "projection.thread-messages",
        Some("thread.message-sent"),
        failure_count,
    );
    for event in [
        ProviderEvent {
            native_event_id: None,
            event_type: "turn.completed".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: Some("provider-turn-1".to_owned()),
            item_id: None,
            request_id: None,
            payload: json!({"state":"failed","error":{"message":"model unavailable"}}),
            activity: Vec::new(),
            activity_controls: Default::default(),
        },
        ProviderEvent {
            native_event_id: None,
            event_type: "session.updated".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: Some("provider-turn-1".to_owned()),
            item_id: None,
            request_id: None,
            payload: json!({"sentinel":true}),
            activity: Vec::new(),
            activity_controls: Default::default(),
        },
    ] {
        events_tx.send(event).await.unwrap();
    }

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let snapshot = load_snapshot(&engine.repositories()).await.unwrap();
            if snapshot.activities.iter().any(|activity| {
                activity.thread_id == "t1"
                    && activity.payload["eventType"] == "session.updated"
                    && activity.payload["sentinel"] == true
            }) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("sentinel event proves the terminal event left the provider event pump");
    if recover_failed_message_late {
        events_tx
            .send(ProviderEvent {
                native_event_id: None,
                event_type: "turn.completed".to_owned(),
                thread_id: "t1".to_owned(),
                turn_id: Some("provider-turn-1".to_owned()),
                item_id: None,
                request_id: None,
                payload: json!({"state":"failed","error":{"message":"model unavailable"}}),
                activity: Vec::new(),
                activity_controls: Default::default(),
            })
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let recovered = engine
                    .repositories()
                    .get_message("assistant:t1:item:partial-1".to_owned())
                    .await
                    .unwrap()
                    .is_some_and(|message| !message.is_streaming);
                if recovered {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("duplicate terminal event settles the previously failed message");
    }
    let snapshot = load_snapshot(&engine.repositories()).await.unwrap();
    let messages = engine
        .repositories()
        .list_messages_by_thread("t1".to_owned())
        .await
        .unwrap()
        .into_iter()
        .filter(|message| message.role == "assistant")
        .map(|message| (message.message_id, message.text, message.is_streaming))
        .collect::<Vec<_>>();
    let provider_error = snapshot.activities.iter().any(|activity| {
        activity.thread_id == "t1"
            && activity.kind == "provider.error"
            && activity.payload["error"]["message"] == "model unavailable"
    });
    let session_error = snapshot
        .sessions
        .iter()
        .find(|session| session.thread_id == "t1")
        .and_then(|session| session.last_error.clone());
    let assistant_message_id = snapshot
        .turns
        .iter()
        .find(|turn| turn.thread_id == "t1" && turn.turn_id.as_deref() == Some("provider-turn-1"))
        .and_then(|turn| turn.assistant_message_id.clone());
    supervisor.shutdown().await.unwrap();
    (
        messages,
        assistant_message_id,
        provider_error,
        session_error,
    )
}

#[tokio::test]
async fn terminal_completion_failure_retries_and_preserves_the_lifecycle_projection() {
    assert_eq!(
        project_terminal_with_completion_failures(1, false).await,
        (
            vec![
                (
                    "assistant:t1:item:partial-1".to_owned(),
                    "partial-1".to_owned(),
                    false,
                ),
                (
                    "assistant:t1:item:partial-2".to_owned(),
                    "partial-2".to_owned(),
                    false,
                ),
            ],
            Some("assistant:t1:item:partial-2".to_owned()),
            true,
            Some("model unavailable".to_owned()),
        )
    );
}

#[tokio::test]
async fn terminal_completion_retry_exhaustion_isolates_the_failed_message() {
    assert_eq!(
        project_terminal_with_completion_failures(2, false).await,
        (
            vec![
                (
                    "assistant:t1:item:partial-1".to_owned(),
                    "partial-1".to_owned(),
                    true,
                ),
                (
                    "assistant:t1:item:partial-2".to_owned(),
                    "partial-2".to_owned(),
                    false,
                ),
            ],
            Some("assistant:t1:item:partial-2".to_owned()),
            true,
            Some("model unavailable".to_owned()),
        )
    );
}

#[tokio::test]
async fn late_duplicate_terminal_keeps_the_later_assistant_message_final() {
    assert_eq!(
        project_terminal_with_completion_failures(2, true).await,
        (
            vec![
                (
                    "assistant:t1:item:partial-1".to_owned(),
                    "partial-1".to_owned(),
                    false,
                ),
                (
                    "assistant:t1:item:partial-2".to_owned(),
                    "partial-2".to_owned(),
                    false,
                ),
            ],
            Some("assistant:t1:item:partial-2".to_owned()),
            true,
            Some("model unavailable".to_owned()),
        )
    );
}

#[tokio::test]
async fn failed_provider_completion_clears_running_state_and_preserves_the_error() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (events_tx, events_rx) = mpsc::channel(4);
    let factory = Arc::new(FakeFactory {
        state,
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    supervisor.launch(launch()).await.unwrap();
    let start = OrchestrationCommand::ThreadTurnStart {
        command_id: "failed-turn".to_owned(),
        thread_id: "t1".to_owned(),
        message: ThreadMessageInput {
            message_id: "m-failed".to_owned(),
            role: "user".to_owned(),
            text: "fail".to_owned(),
            attachments: vec![],
        },
        model_selection: None,
        title_seed: None,
        runtime_mode: "full-access".to_owned(),
        interaction_mode: "default".to_owned(),
        bootstrap: None,
        source_proposed_plan: None,
        queued: None,
        created_at: NOW.to_owned(),
    };
    engine.dispatch(start.clone()).await.unwrap();
    supervisor.handle_orchestration(start).await.unwrap();
    events_tx
        .send(ProviderEvent {
            native_event_id: None,
            event_type: "turn.completed".to_owned(),
            thread_id: "t1".to_owned(),
            turn_id: Some("provider-turn-1".to_owned()),
            item_id: None,
            request_id: None,
            payload: json!({"state":"failed","error":{"message":"model unavailable"}}),
            activity: Vec::new(),
            activity_controls: Default::default(),
        })
        .await
        .unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let snapshot = load_snapshot(&engine.repositories()).await.unwrap();
            let session = snapshot
                .sessions
                .iter()
                .find(|session| session.thread_id == "t1");
            let failure = snapshot
                .activities
                .iter()
                .find(|activity| activity.thread_id == "t1" && activity.kind == "provider.error");
            let assistant = snapshot
                .messages
                .iter()
                .find(|message| message.thread_id == "t1" && message.role == "assistant");
            let runtime = engine
                .repositories()
                .get_provider_session_runtime("t1".to_owned())
                .await
                .unwrap();
            if session.is_some_and(|session| {
                session.status == "error"
                    && session.active_turn_id.is_none()
                    && session.last_error.as_deref() == Some("model unavailable")
            }) && failure.is_some_and(|activity| {
                activity.tone == "error"
                    && activity.payload["error"]["message"] == "model unavailable"
            }) && assistant.is_none()
                && runtime.is_some_and(|runtime| runtime.status == "error")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("failed provider completion must be terminal and actionable");
    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn shutdown_stops_every_driver_and_suspends_runtime_rows() {
    let engine = engine().await;
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let (_events_tx, events_rx) = mpsc::channel(1);
    let factory = Arc::new(FakeFactory {
        state: state.clone(),
        events: StdMutex::new(VecDeque::from([events_rx])),
    });
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    supervisor.launch(launch()).await.unwrap();
    let runtime = || async {
        engine
            .repositories()
            .get_provider_session_runtime("t1".to_owned())
            .await
            .unwrap()
            .expect("runtime row")
    };
    let launched = runtime().await;
    supervisor.shutdown().await.unwrap();
    assert_eq!(state.lock().unwrap().shutdowns, 1);
    // A clean shutdown keeps the resume cursor so the next launch resumes the conversation.
    let suspended = runtime().await;
    assert_eq!(suspended.status, "suspended");
    assert!(launched.resume_cursor.is_some());
    assert_eq!(suspended.resume_cursor, launched.resume_cursor);
}

#[test]
fn provider_driver_trait_is_send_and_sync() {
    fn assert_future<T: Future + Send>(_: Pin<Box<T>>) {}
    let _ = assert_future::<std::future::Ready<()>>;
    fn assert_driver<T: ProviderDriver + Send + Sync>() {}
    assert_driver::<FakeDriver>();
}

#[tokio::test]
async fn native_factory_rejects_unknown_providers_without_a_fallback() {
    let factory = NativeProviderDriverFactory::new(TempDir::new().unwrap().path().to_path_buf());
    let mut request = launch();
    request.provider = "node-fallback".to_owned();
    let error = match factory.create(request).await {
        Ok(_) => panic!("unknown provider unexpectedly created"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        ProviderRuntimeError::UnsupportedProvider { provider } if provider == "node-fallback"
    ));
}

#[tokio::test]
async fn native_factory_routes_resume_to_the_native_adapter_without_a_fallback() {
    let sandbox = TempDir::new().expect("missing native provider sandbox");
    let factory = NativeProviderDriverFactory::new(sandbox.path().to_path_buf());
    let mut request = launch();
    request.provider = "opencode".to_owned();
    request.binary_path = hermetic_providers::missing_provider_executable(sandbox.path())
        .to_string_lossy()
        .into_owned();
    request.resume_cursor = Some(json!({"sessionId":"old-session"}));
    let error = match factory.create(request).await {
        Ok(_) => panic!("missing native provider unexpectedly spawned"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        ProviderRuntimeError::Spawn { provider, .. } if provider == "opencode"
    ));
}

#[tokio::test]
async fn native_factory_routes_every_supported_provider_to_its_native_adapter() {
    let sandbox = TempDir::new().expect("missing native adapter sandbox");
    let factory = NativeProviderDriverFactory::new(sandbox.path().to_path_buf());

    for provider in [
        "codex",
        "cursor",
        "grok",
        "opencode",
        "claude",
        "claudeAgent",
    ] {
        let mut request = launch();
        request.provider = provider.to_owned();
        request.binary_path = hermetic_providers::missing_provider_executable(sandbox.path())
            .to_string_lossy()
            .into_owned();

        let error = match factory.create(request).await {
            Ok(_) => panic!("missing {provider} executable unexpectedly spawned"),
            Err(error) => error,
        };

        assert!(
            matches!(error, ProviderRuntimeError::Spawn { provider: ref actual, .. } if actual == provider),
            "unexpected native adapter error for {provider}: {error:?}"
        );
    }
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn supervisor_reaps_a_naturally_exited_native_provider_without_an_explicit_stop() {
    let engine = engine().await;
    let temp = TempDir::new().unwrap();
    let exit_file = temp.path().join("allow-natural-exit");
    let executable = executable_fixture(
        &temp,
        "naturally-exiting-claude",
        "#!/bin/sh\nwhile [ ! -f \"$BIBCODE_EXIT_FILE\" ]; do sleep 0.05; done\n",
        "while (-not (Test-Path -LiteralPath $env:BIBCODE_EXIT_FILE)) { Start-Sleep -Milliseconds 50 }\n",
    );
    let registry = ProcessAttributionRegistry::new();
    let factory = Arc::new(NativeProviderDriverFactory::with_process_attribution(
        temp.path().join("attachments"),
        registry.clone(),
    ));
    let supervisor = ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        activity_projection(&engine),
        SupervisorOptions::default(),
    );
    let mut request = launch();
    request.provider = "claudeAgent".to_owned();
    request.provider_label = "Naturally Exiting Claude".to_owned();
    request.binary_path = executable.to_string_lossy().into_owned();
    request.cwd = temp.path().to_path_buf();
    request.environment.insert(
        "BIBCODE_EXIT_FILE".to_owned(),
        exit_file.to_string_lossy().into_owned(),
    );

    supervisor.launch(request).await.unwrap();
    let rows = NativeProcessSampler::default().sample().await.unwrap();
    let claims = registry.bind_and_snapshot(&rows, Instant::now());
    let claim = claims
        .iter()
        .find(|claim| claim.label == "Naturally Exiting Claude")
        .expect("provider registration should bind while the child is alive");
    let bound_row = rows
        .into_iter()
        .find(|row| row.pid == claim.identity.pid && row.started_at == claim.identity.started_at)
        .expect("bound provider process row");
    std::fs::write(&exit_file, b"exit").expect("release provider for natural exit");

    timeout(Duration::from_secs(3), async {
        loop {
            if registry
                .bind_and_snapshot(
                    std::slice::from_ref::<ProcessRow>(&bound_row),
                    Instant::now(),
                )
                .is_empty()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("natural provider exit should release the registration lease");

    supervisor.shutdown().await.unwrap();
    engine.shutdown().await;
}

#[tokio::test]
async fn native_claude_driver_supports_the_complete_live_command_surface() {
    let temp = TempDir::new().unwrap();
    let executable = executable_fixture(
        &temp,
        "claude-fixture",
        "#!/bin/sh\nprintf '%s\\n' 'ignored non-json output'\nprintf '%s\\n' 'fixture warning' >&2\ncat >/dev/null\n",
        WINDOWS_CLAUDE_FIXTURE,
    );

    let factory = NativeProviderDriverFactory::new(temp.path().join("attachments"));
    let mut request = launch();
    request.provider = "claudeAgent".to_owned();
    request.binary_path = executable.to_string_lossy().into_owned();
    request.cwd = temp.path().to_path_buf();
    request.runtime_mode = "approval-required".to_owned();
    request.interaction_mode = "default".to_owned();
    request.model = Some("claude-sonnet".to_owned());
    request.agent = Some("reviewer".to_owned());
    request.resume_cursor = Some(json!({"sessionId":"claude-session"}));
    use_empty_claude_config(&mut request, &temp);

    let driver = factory.create(request).await.unwrap();
    let started = driver.start().await.unwrap();
    assert_eq!(
        started.resume_cursor,
        Some(json!({"sessionId":"claude-session"}))
    );
    assert_eq!(
        started.runtime_payload,
        Some(json!({"transport":"stream-json"}))
    );
    assert_eq!(
        started.activity_capabilities,
        ActivityCapabilities::none(),
        "CLI flags and wrapper records alone must not claim attributed actors"
    );

    assert!(
        driver
            .send(
                "hello".to_owned(),
                vec![image_attachment(&temp)],
                "default".to_owned(),
            )
            .await
            .unwrap()
            .is_some()
    );
    driver.interrupt(None).await.unwrap();
    driver
        .approve("approval-1".to_owned(), "acceptForSession".to_owned())
        .await
        .unwrap();
    driver
        .approve("approval-2".to_owned(), "deny".to_owned())
        .await
        .unwrap();
    driver
        .set_mode("auto-accept-edits".to_owned())
        .await
        .unwrap();
    driver.set_mode("full-access".to_owned()).await.unwrap();
    driver
        .set_interaction_mode("plan".to_owned())
        .await
        .unwrap();
    driver
        .set_interaction_mode("default".to_owned())
        .await
        .unwrap();
    assert!(matches!(
        driver.set_model("other".to_owned()).await,
        Err(ProviderRuntimeError::UnsupportedCapability { provider, .. }) if provider == "claude"
    ));
    assert!(matches!(
        driver.rollback(1, "rewind-key".to_owned()).await,
        Err(ProviderRuntimeError::UnsupportedCapability { provider, .. }) if provider == "claude"
    ));

    let event = match timeout(
        NATIVE_PROVIDER_EVENT_INTEGRATION_DEADLINE,
        driver.next_event(),
    )
    .await
    {
        Ok(Some(event)) => event,
        outcome => {
            let cleanup = timeout(
                NATIVE_PROVIDER_EVENT_INTEGRATION_DEADLINE,
                driver.shutdown(),
            )
            .await;
            panic!(
                "native Claude fixture did not publish its startup stderr event: {outcome:?}; cleanup: {cleanup:?}"
            );
        }
    };
    assert_eq!(event.event_type, "session.stderr");
    assert_eq!(event.payload, json!({"message":"fixture warning"}));
    driver.shutdown().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn claude_transcript_recovery_safe_open_is_bounded_cancellable_and_replacement_safe() {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new().unwrap();
    let transcript = temp.path().join("child.jsonl");
    std::fs::write(&transcript, b"{\"record\":true}\n").unwrap();

    let opened = ClaudeTranscriptReaderFixture::read(&transcript, false).await;
    assert!(opened.opened);
    assert!(!opened.cancelled);
    assert_eq!(opened.bytes_read, 16);

    let missing =
        ClaudeTranscriptReaderFixture::read(&temp.path().join("missing.jsonl"), false).await;
    assert!(!missing.opened);
    assert!(!missing.cancelled);
    let directory = ClaudeTranscriptReaderFixture::read(temp.path(), false).await;
    assert!(!directory.opened);

    let cancelled = ClaudeTranscriptReaderFixture::read(&transcript, true).await;
    assert!(!cancelled.opened);
    assert!(cancelled.cancelled);

    let replacement_content = temp.path().join("replacement-content.jsonl");
    std::fs::write(&replacement_content, b"{\"replacement\":true}\n").unwrap();
    let replacement_link = temp.path().join("replacement-link.jsonl");
    symlink(&replacement_content, &replacement_link).unwrap();
    let replaced = ClaudeTranscriptReaderFixture::read_after_metadata_replacement(
        &transcript,
        &replacement_link,
    )
    .await;
    assert!(
        !replaced.opened,
        "a target replaced after canonical metadata must not be followed"
    );
}

#[test]
fn claude_transcript_recovery_windows_identity_requires_complete_metadata() {
    assert!(
        ClaudeTranscriptReaderFixture::windows_identity_matches(
            Some(7),
            Some(11),
            Some(7),
            Some(11)
        ),
        "complete matching file identities are accepted"
    );
    for identity in [
        (None, Some(11), None, Some(11)),
        (Some(7), None, Some(7), None),
        (None, None, None, None),
        (Some(7), Some(11), Some(8), Some(11)),
        (Some(7), Some(11), Some(7), Some(12)),
    ] {
        assert!(
            !ClaudeTranscriptReaderFixture::windows_identity_matches(
                identity.0, identity.1, identity.2, identity.3
            ),
            "missing or mismatched Windows metadata must be unavailable: {identity:?}"
        );
    }
}

#[tokio::test]
async fn claude_transcript_recovery_shutdown_cancels_idle_output_reads() {
    assert!(
        claude_output_shutdown_with_open_stream_for_test().await,
        "explicit shutdown must not wait for EOF from provider descendants that retain output pipes"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn claude_transcript_recovery_shutdown_breaks_event_queue_backpressure() {
    let probe_context = ClaudeActivityProbeTestContext::new();
    let temp = TempDir::new().unwrap();
    let output_written_path = temp.path().join("output-written");
    let executable = executable_fixture(
        &temp,
        "claude-output-backpressure",
        r#"#!/bin/sh
case "$1" in
  --version) printf '%s\n' '2.1.218'; exit 0;;
  --help) printf '%s\n' '--include-hook-events --forward-subagent-text'; exit 0;;
esac
# Short lines keep all stderr below one 4 KiB pipe page, regardless of pipe size.
index=0
while [ "$index" -lt 512 ]; do
  printf 'e%s\n' "$index" >&2
  index=$((index + 1))
done
: > "$BIBCODE_TEST_OUTPUT_WRITTEN"
cat >/dev/null
"#,
        "",
    );
    let factory = probe_context.driver_factory(temp.path().join("attachments"));
    let mut request = launch();
    request.provider = "claudeAgent".to_owned();
    request.binary_path = executable.to_string_lossy().into_owned();
    request.cwd = temp.path().to_path_buf();
    request.environment.insert(
        "BIBCODE_TEST_OUTPUT_WRITTEN".to_owned(),
        output_written_path.to_string_lossy().into_owned(),
    );
    let driver = factory.create(request).await.unwrap();
    driver.start().await.unwrap();

    timeout(Duration::from_secs(2), async {
        while !output_written_path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fixture writes enough events to saturate the provider event queue");
    // This current-thread runtime runs stderr only while the test awaits. All
    // 512 lines are readable before the marker, so after settling the task must
    // be parked on the full 128-slot queue: nothing has consumed any events.
    // Receiving fewer than 512 below proves shutdown ended that blocked producer.
    tokio::time::sleep(Duration::from_millis(50)).await;

    timeout(Duration::from_secs(1), driver.shutdown())
        .await
        .expect("shutdown must cancel producers blocked on a full event queue")
        .unwrap();
    let mut stderr_event_count = 0;
    while let Some(event) = timeout(Duration::from_secs(1), driver.next_event())
        .await
        .expect("event channel must close after queued events drain")
    {
        if event.event_type == "session.stderr" {
            stderr_event_count += 1;
        }
    }
    assert!(
        stderr_event_count > 0 && stderr_event_count < 512,
        "shutdown must end a producer blocked on the full queue before all 512 stderr events are delivered; received {stderr_event_count}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn claude_transcript_recovery_worker_emits_bounded_activity_without_disclosing_paths() {
    let probe_context = ClaudeActivityProbeTestContext::new();
    let temp = TempDir::new().unwrap();
    let settings_path = temp.path().join("recovery-settings.json");
    let token_path = temp.path().join("recovery-token");
    let transcript_path = temp.path().join("child-agent.jsonl");
    std::fs::write(
        &transcript_path,
        [
            r#"{"type":"assistant","sessionId":"recovery-session","agentId":"agent-recovery","isSidechain":true,"uuid":"message-tool","timestamp":"2026-07-24T12:00:00Z","message":{"role":"assistant","content":[{"type":"text","text":"Recovered child work"},{"type":"tool_use","id":"tool-recovery","name":"Read","input":{"file_path":"/private/input"}}]}}"#,
            r#"{"type":"user","sessionId":"recovery-session","agentId":"agent-recovery","isSidechain":true,"uuid":"message-result","timestamp":"2026-07-24T12:00:01Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tool-recovery","content":"ok","is_error":false}]}}"#,
        ]
        .join("\n"),
    )
    .unwrap();
    let executable = executable_fixture(
        &temp,
        "claude-transcript-recovery",
        r#"#!/bin/sh
case "$1" in
  --version) printf '%s\n' '2.1.218'; exit 0;;
  --help) printf '%s\n' '--include-hook-events --forward-subagent-text'; exit 0;;
esac
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--settings" ]; then
    shift
    printf '%s' "$1" > "$BIBCODE_TEST_SETTINGS_CAPTURE"
    break
  fi
  shift
done
printf '%s' "$BIBCODE_CLAUDE_HOOK_TOKEN" > "$BIBCODE_TEST_TOKEN_CAPTURE"
cat >/dev/null
"#,
        "",
    );
    let factory = probe_context.driver_factory(temp.path().join("attachments"));
    let mut request = launch();
    request.provider = "claudeAgent".to_owned();
    request.binary_path = executable.to_string_lossy().into_owned();
    request.cwd = temp.path().to_path_buf();
    request.resume_cursor = Some(json!({"sessionId":"recovery-session"}));
    use_empty_claude_config(&mut request, &temp);
    request.environment.insert(
        "BIBCODE_TEST_SETTINGS_CAPTURE".to_owned(),
        settings_path.to_string_lossy().into_owned(),
    );
    request.environment.insert(
        "BIBCODE_TEST_TOKEN_CAPTURE".to_owned(),
        token_path.to_string_lossy().into_owned(),
    );
    let driver = factory.create(request).await.unwrap();
    driver.start().await.unwrap();
    wait_for_captures(&[&settings_path, &token_path]).await;
    let settings: Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    let hook_url = settings["hooks"]["SubagentStart"][0]["hooks"][0]["url"]
        .as_str()
        .unwrap();
    let token = std::fs::read_to_string(&token_path).unwrap();
    let client = reqwest::Client::new();
    for hook in [
        json!({
            "hook_event_name":"SubagentStart",
            "session_id":"recovery-session",
            "agent_id":"agent-recovery",
            "agent_type":"Explore"
        }),
        json!({
            "hook_event_name":"SubagentStop",
            "session_id":"recovery-session",
            "agent_id":"agent-recovery",
            "agent_type":"Explore",
            "transcript_path":"/private/main-session-must-not-be-read.jsonl",
            "agent_transcript_path":transcript_path,
            "last_assistant_message":"done"
        }),
    ] {
        assert!(
            client
                .post(hook_url)
                .header("Authorization", format!("Bearer {token}"))
                .json(&hook)
                .send()
                .await
                .unwrap()
                .status()
                .is_success()
        );
    }

    let mut recovery_event = None;
    for _ in 0..3 {
        let event = timeout(Duration::from_secs(2), driver.next_event())
            .await
            .expect("worker must finish")
            .expect("provider event channel remains open");
        if event
            .native_event_id
            .as_ref()
            .is_some_and(|id| id.as_str().starts_with("claude:recovery:"))
        {
            recovery_event = Some(event);
            break;
        }
    }
    let recovery_event = recovery_event.expect("bounded recovery activity event");
    assert_eq!(recovery_event.event_type, "activity.native");
    assert!(matches!(
        recovery_event.activity.last(),
        Some(ProviderActivityMutation::SetScope {
            capabilities: ActivityCapabilities {
                actors: true,
                attributed_activity: true,
                history_recovery: ActivityHistoryRecovery::Bounded,
                ..
            },
            observation_state: ActivityObservationState::Live,
        })
    ));
    assert_eq!(
        recovery_event
            .activity
            .iter()
            .filter(|mutation| matches!(mutation, ProviderActivityMutation::AppendEntry(_)))
            .count(),
        3
    );
    let debug = format!("{recovery_event:?}");
    assert!(!debug.contains(transcript_path.to_string_lossy().as_ref()));
    assert!(!debug.contains("main-session-must-not-be-read"));

    let empty_supported_path = temp.path().join("child-empty-supported.jsonl");
    std::fs::write(
        &empty_supported_path,
        r#"{"type":"assistant","sessionId":"recovery-session","agentId":"agent-empty","isSidechain":true,"uuid":"thinking-only","timestamp":"2026-07-24T12:00:02Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"not activity"}]}}"#,
    )
    .unwrap();
    for hook in [
        json!({
            "hook_event_name":"SubagentStart",
            "session_id":"recovery-session",
            "agent_id":"agent-empty",
            "agent_type":"Explore"
        }),
        json!({
            "hook_event_name":"SubagentStop",
            "session_id":"recovery-session",
            "agent_id":"agent-empty",
            "agent_type":"Explore",
            "agent_transcript_path":empty_supported_path
        }),
    ] {
        client
            .post(hook_url)
            .header("Authorization", format!("Bearer {token}"))
            .json(&hook)
            .send()
            .await
            .unwrap();
    }
    let mut empty_recovery = None;
    for _ in 0..3 {
        let event = timeout(Duration::from_secs(2), driver.next_event())
            .await
            .unwrap()
            .unwrap();
        if event
            .native_event_id
            .as_ref()
            .is_some_and(|id| id.as_str().starts_with("claude:recovery:"))
        {
            empty_recovery = Some(event);
            break;
        }
    }
    assert!(matches!(
        empty_recovery
            .expect("correlation-valid scan with no supported entries")
            .activity
            .as_slice(),
        [ProviderActivityMutation::SetScope {
            capabilities: ActivityCapabilities {
                history_recovery: ActivityHistoryRecovery::Bounded,
                ..
            },
            ..
        }]
    ));

    for hook in [
        json!({
            "hook_event_name":"SubagentStart",
            "session_id":"recovery-session",
            "agent_id":"agent-missing",
            "agent_type":"Explore"
        }),
        json!({
            "hook_event_name":"SubagentStop",
            "session_id":"recovery-session",
            "agent_id":"agent-missing",
            "agent_type":"Explore",
            "agent_transcript_path":temp.path().join("missing-child.jsonl")
        }),
    ] {
        client
            .post(hook_url)
            .header("Authorization", format!("Bearer {token}"))
            .json(&hook)
            .send()
            .await
            .unwrap();
    }
    for _ in 0..2 {
        timeout(Duration::from_secs(2), driver.next_event())
            .await
            .expect("live hook activity remains functional")
            .expect("provider event");
    }
    assert!(
        timeout(Duration::from_millis(150), driver.next_event())
            .await
            .is_err(),
        "missing child transcript must not advertise a recovery handshake"
    );

    for index in 0..48 {
        let agent_id = format!("agent-cap-{index}");
        let path = temp.path().join(format!("child-cap-{index}.jsonl"));
        std::fs::write(
            &path,
            json!({
                "type":"system",
                "sessionId":"recovery-session",
                "agentId":agent_id,
                "isSidechain":true
            })
            .to_string(),
        )
        .unwrap();
        for hook in [
            json!({
                "hook_event_name":"SubagentStart",
                "session_id":"recovery-session",
                "agent_id":agent_id,
                "agent_type":"Explore"
            }),
            json!({
                "hook_event_name":"SubagentStop",
                "session_id":"recovery-session",
                "agent_id":agent_id,
                "agent_type":"Explore",
                "agent_transcript_path":path
            }),
        ] {
            client
                .post(hook_url)
                .header("Authorization", format!("Bearer {token}"))
                .json(&hook)
                .send()
                .await
                .unwrap();
        }
        for _ in 0..2 {
            timeout(Duration::from_secs(2), driver.next_event())
                .await
                .expect("live hook activity")
                .expect("provider event");
        }
        if index < 47 {
            let event = timeout(Duration::from_secs(2), driver.next_event())
                .await
                .expect("one bounded recovery per accepted target")
                .expect("provider event");
            assert!(
                event
                    .native_event_id
                    .as_ref()
                    .is_some_and(|id| id.as_str().starts_with("claude:recovery:"))
            );
        } else {
            assert!(
                timeout(Duration::from_millis(150), driver.next_event())
                    .await
                    .is_err(),
                "the 51st recovery target for one root must be ignored"
            );
        }
    }

    timeout(Duration::from_secs(2), driver.shutdown())
        .await
        .expect("shutdown cancels and joins transcript recovery")
        .unwrap();
    assert!(
        timeout(Duration::from_secs(2), driver.next_event())
            .await
            .expect("provider event channel closes after recovery shutdown")
            .is_none()
    );
}

#[test]
fn claude_launch_arguments_preserve_controls_and_gate_activity_flags() {
    let mut request = launch();
    request.provider = "claudeAgent".to_owned();
    request.runtime_mode = "approval-required".to_owned();
    request.interaction_mode = "default".to_owned();
    request.model = Some("claude-sonnet".to_owned());
    request.effort = Some("high".to_owned());
    request.agent = Some("reviewer".to_owned());
    request.resume_cursor = Some(json!({"sessionId":"resume-session"}));
    request.mcp = Some(ProviderMcpConfig {
        endpoint: "http://127.0.0.1:1234/mcp".to_owned(),
        authorization_header: "Bearer test".to_owned(),
        provider_session_id: "provider-session".to_owned(),
    });

    let supported = build_claude_launch_arguments_for_test(
        &request,
        "resume-session",
        ClaudeActivitySupport {
            include_hook_events: true,
            forward_subagent_text: true,
            transcript_recovery: false,
        },
    );
    for required in [
        "--print",
        "--input-format",
        "--output-format",
        "--replay-user-messages",
        "--include-partial-messages",
        "--include-hook-events",
        "--forward-subagent-text",
        "--verbose",
    ] {
        assert!(supported.iter().any(|argument| argument == required));
    }
    let replay = supported
        .iter()
        .position(|argument| argument == "--replay-user-messages")
        .expect("replay argument");
    let verbose = supported
        .iter()
        .position(|argument| argument == "--verbose")
        .expect("verbose argument");
    assert!(replay < verbose);
    assert_eq!(
        supported
            .iter()
            .filter(|argument| argument.as_str() == "--include-hook-events")
            .count(),
        1
    );
    assert_eq!(
        supported
            .iter()
            .filter(|argument| argument.as_str() == "--forward-subagent-text")
            .count(),
        1
    );
    for pair in [
        ["--permission-mode", "default"],
        ["--resume", "resume-session"],
        ["--model", "claude-sonnet"],
        ["--effort", "high"],
        ["--agent", "reviewer"],
    ] {
        assert!(
            supported.windows(2).any(|window| window == pair),
            "missing preserved Claude launch pair {pair:?}: {supported:?}"
        );
    }
    assert!(supported.iter().any(|argument| argument == "--mcp-config"));
    assert!(
        !supported
            .iter()
            .any(|argument| argument.contains("Bearer") || argument.contains("/mcp")),
        "the MCP server and its token stay off the command line: {supported:?}"
    );

    let unsupported = build_claude_launch_arguments_for_test(
        &request,
        "resume-session",
        ClaudeActivitySupport::default(),
    );
    assert!(!unsupported.iter().any(|argument| {
        matches!(
            argument.as_str(),
            "--include-hook-events" | "--forward-subagent-text"
        )
    }));
    assert!(
        unsupported
            .windows(2)
            .any(|window| window == ["--effort", "high"])
    );
}

#[test]
fn claude_fast_mode_is_merged_into_session_settings() {
    let mut request = launch();
    request.provider = "claudeAgent".to_owned();
    request.options = vec![json!({ "id": "fastMode", "value": true })];

    let arguments = build_claude_launch_arguments_with_settings_for_test(
        &request,
        "claude-session",
        ClaudeActivitySupport::default(),
        Some(json!({ "hooks": {} })),
    );
    let settings = arguments
        .windows(2)
        .find(|pair| pair[0] == "--settings")
        .map(|pair| serde_json::from_str::<Value>(&pair[1]).expect("settings JSON"))
        .expect("session settings argument");

    assert_eq!(settings["fastMode"], true);
    assert!(settings.get("hooks").is_some());
    assert!(
        !arguments
            .iter()
            .any(|argument| argument.contains("settings.json"))
    );
}

#[cfg(unix)]
#[tokio::test]
async fn claude_activity_probe_is_cached_by_executable_identity() {
    let probe_context = ClaudeActivityProbeTestContext::new();
    let temp = TempDir::new().unwrap();
    let count_path = temp.path().join("probe-count");
    let script = format!(
        "#!/bin/sh\nprintf x >> '{}'\ncase \"$1\" in\n  --version) printf '%s\\n' '2.1.218';;\n  --help) printf '%s\\n' '--include-hook-events --forward-subagent-text';;\n  *) exit 1;;\nesac\n",
        count_path.display()
    );
    let executable = executable_fixture(&temp, "claude-probe", &script, "");

    let first = probe_context
        .probe(executable.to_string_lossy().as_ref())
        .await;
    let second = probe_context
        .probe(executable.to_string_lossy().as_ref())
        .await;

    assert_eq!(
        first,
        ClaudeActivitySupport {
            include_hook_events: true,
            forward_subagent_text: true,
            transcript_recovery: false,
        }
    );
    assert_eq!(second, first);
    assert_eq!(
        std::fs::read_to_string(count_path).expect("probe count"),
        "xx",
        "one version and one help invocation should serve repeated turns"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn claude_activity_probe_singleflights_concurrent_misses() {
    let probe_context = ClaudeActivityProbeTestContext::new();
    let temp = TempDir::new().unwrap();
    let count_path = temp.path().join("probe-count");
    let script = format!(
        "#!/bin/sh\nprintf x >> '{}'\nsleep 0.1\ncase \"$1\" in\n  --version) printf '%s\\n' '2.1.218';;\n  --help) printf '%s\\n' '--include-hook-events --forward-subagent-text';;\n  *) exit 1;;\nesac\n",
        count_path.display()
    );
    let executable = executable_fixture(&temp, "claude-concurrent-probe", &script, "");
    let binary_path = executable.to_string_lossy().into_owned();
    let barrier = Arc::new(tokio::sync::Barrier::new(9));
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let barrier = barrier.clone();
        let binary_path = binary_path.clone();
        let probe_context = probe_context.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            probe_context.probe(&binary_path).await
        }));
    }
    barrier.wait().await;
    for task in tasks {
        assert_eq!(
            task.await.unwrap(),
            ClaudeActivitySupport {
                include_hook_events: true,
                forward_subagent_text: true,
                transcript_recovery: false,
            }
        );
    }
    assert_eq!(
        std::fs::read_to_string(count_path).expect("probe count"),
        "xx",
        "concurrent misses must share one version/help probe"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn claude_activity_probe_test_contexts_isolate_concurrent_cache_mutation() {
    let temp = TempDir::new().unwrap();
    let ready_fifo = temp.path().join("probe-ready.fifo");
    let release_fifos = (0..8)
        .map(|index| temp.path().join(format!("probe-release-{index}.fifo")))
        .collect::<Vec<_>>();
    let fifo_creation = Command::new("mkfifo")
        .arg(&ready_fifo)
        .args(&release_fifos)
        .output()
        .expect("create probe fixture FIFOs");
    assert!(
        fifo_creation.status.success(),
        "mkfifo failed: {}",
        String::from_utf8_lossy(&fifo_creation.stderr)
    );
    let ready_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&ready_fifo)
        .expect("open probe readiness FIFO");
    let mut ready_lines = BufReader::new(tokio::fs::File::from_std(ready_file)).lines();
    let mut release_files = release_fifos
        .iter()
        .map(|path| {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .expect("open probe release FIFO")
        })
        .collect::<Vec<_>>();
    let executables = (0..8)
        .map(|index| {
            executable_fixture(
                &temp,
                &format!("claude-isolated-probe-{index}"),
                &format!(
                    "#!/bin/sh\ncase \"$1\" in\n  --version) printf '%s\\n' '{index}' > '{}'; IFS= read -r _ < '{}'; printf '%s\\n' '2.1.218';;\n  --help) printf '%s\\n' '--include-hook-events --forward-subagent-text';;\n  *) exit 1;;\nesac\n",
                    ready_fifo.display(),
                    release_fifos[index].display()
                ),
                "",
            )
        })
        .collect::<Vec<_>>();
    let probe_contexts = (0..8)
        .map(|_| ClaudeActivityProbeTestContext::new())
        .collect::<Vec<_>>();
    let seed_context = ClaudeActivityProbeTestContext::new();
    let barrier = Arc::new(tokio::sync::Barrier::new(probe_contexts.len() + 2));

    let mut probes = tokio::task::JoinSet::new();
    for (probe_context, executable) in probe_contexts.iter().cloned().zip(executables.iter()) {
        let barrier = barrier.clone();
        let binary_path = executable.to_string_lossy().into_owned();
        probes.spawn(async move {
            barrier.wait().await;
            probe_context.probe(&binary_path).await
        });
    }
    let seed = tokio::spawn({
        let barrier = barrier.clone();
        let seed_context = seed_context.clone();
        async move {
            barrier.wait().await;
            seed_context.seed_cache(65);
        }
    });
    barrier.wait().await;

    let mut ready_indices = std::collections::HashSet::new();
    while ready_indices.len() < probe_contexts.len() {
        tokio::select! {
            line = ready_lines.next_line() => {
                let line = line
                    .expect("read probe readiness FIFO")
                    .expect("probe readiness FIFO remains open");
                assert!(ready_indices.insert(line), "each probe child publishes readiness once");
            }
            result = probes.join_next() => {
                panic!("probe child completed before all peers were ready: {result:?}");
            }
        }
    }
    assert_eq!(
        ready_indices,
        (0..probe_contexts.len())
            .map(|index| index.to_string())
            .collect(),
        "every distinct probe child must overlap before release"
    );
    for release_file in &mut release_files {
        std::io::Write::write_all(release_file, b"\n").expect("release probe child");
    }

    while let Some(probe) = probes.join_next().await {
        let support = probe.unwrap();
        assert_eq!(
            support,
            ClaudeActivitySupport {
                include_hook_events: true,
                forward_subagent_text: true,
                transcript_recovery: false,
            }
        );
    }
    seed.await.unwrap();
    for (probe_context, executable) in probe_contexts.into_iter().zip(executables) {
        let expected_path = std::fs::canonicalize(&executable)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(probe_context.cache_len(), 1);
        assert_eq!(
            probe_context.cache_paths(),
            std::slice::from_ref(&expected_path)
        );
    }
    assert_eq!(seed_context.cache_len(), 64);
    assert!(
        seed_context
            .cache_paths()
            .iter()
            .all(|path| path.starts_with("/bibcode-test/claude-cache-"))
    );
}

#[cfg(unix)]
#[tokio::test]
async fn claude_activity_probe_retries_transient_failures_without_poisoning_cache() {
    let probe_context = ClaudeActivityProbeTestContext::new();
    let temp = TempDir::new().unwrap();
    let marker_path = temp.path().join("failed-once");
    let count_path = temp.path().join("probe-count");
    let script = format!(
        "#!/bin/sh\nprintf x >> '{}'\nif [ \"$1\" = \"--version\" ] && [ ! -f '{}' ]; then touch '{}'; exit 1; fi\ncase \"$1\" in\n  --version) printf '%s\\n' '2.1.218';;\n  --help) printf '%s\\n' '--include-hook-events --forward-subagent-text';;\n  *) exit 1;;\nesac\n",
        count_path.display(),
        marker_path.display(),
        marker_path.display()
    );
    let executable = executable_fixture(&temp, "claude-retry-probe", &script, "");

    assert_eq!(
        probe_context
            .probe(executable.to_string_lossy().as_ref())
            .await,
        ClaudeActivitySupport::default()
    );
    assert_eq!(
        probe_context
            .probe(executable.to_string_lossy().as_ref())
            .await,
        ClaudeActivitySupport {
            include_hook_events: true,
            forward_subagent_text: true,
            transcript_recovery: false,
        }
    );
    assert_eq!(
        std::fs::read_to_string(count_path).expect("probe count"),
        "xxx",
        "a failed version probe must be retried, then cache the successful version/help pair"
    );
    assert_eq!(probe_context.cache_len(), 1);
}

/// Claude reads the BiBCode MCP server from a file only this user can read: on the command line
/// its bearer token was readable by every local process. The file goes with the driver.
#[cfg(unix)]
#[tokio::test]
async fn claude_reads_its_mcp_token_from_a_private_file_removed_with_the_driver() {
    use std::os::unix::fs::PermissionsExt;

    let probe_context = ClaudeActivityProbeTestContext::new();
    let temp = TempDir::new().unwrap();
    let captured = temp.path().join("mcp-config-path");
    let arguments = temp.path().join("arguments");
    let executable = executable_fixture(
        &temp,
        "claude-mcp-file",
        "#!/bin/sh\ncase \"$1\" in\n  --version) printf '%s\\n' '2.1.300 (Claude Code)'; exit 0;;\n  --help) printf '%s\\n' 'Usage: claude'; exit 0;;\nesac\nprintf '%s\\n' \"$@\" > \"$BIBCODE_TEST_ARGS\"\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = --mcp-config ]; then printf '%s' \"$2\" > \"$BIBCODE_TEST_MCP_CONFIG\"; fi\n  shift\ndone\ncat >/dev/null\n",
        "",
    );
    let factory = probe_context.driver_factory(temp.path().join("attachments"));
    let mut request = launch();
    request.provider = "claudeAgent".to_owned();
    request.binary_path = executable.to_string_lossy().into_owned();
    request.cwd = temp.path().to_path_buf();
    request.mcp = Some(ProviderMcpConfig {
        endpoint: "http://127.0.0.1:1234/mcp".to_owned(),
        authorization_header: "Bearer private-test-token".to_owned(),
        provider_session_id: "provider-session".to_owned(),
    });
    for (name, path) in [
        ("BIBCODE_TEST_ARGS", &arguments),
        ("BIBCODE_TEST_MCP_CONFIG", &captured),
    ] {
        request
            .environment
            .insert(name.to_owned(), path.to_string_lossy().into_owned());
    }
    let driver = factory.create(request).await.unwrap();
    driver.start().await.unwrap();
    let config_path = timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(path) = std::fs::read_to_string(&captured)
                && !path.is_empty()
            {
                return PathBuf::from(path);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("Claude was launched with an MCP configuration file");

    let launched = std::fs::read_to_string(&arguments).unwrap();
    assert!(!launched.contains("private-test-token"), "{launched}");
    let config: Value = serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    assert_eq!(
        config["mcpServers"]["bibcode"]["headers"]["Authorization"],
        "Bearer private-test-token"
    );
    assert_eq!(
        config["mcpServers"]["bibcode"]["url"],
        "http://127.0.0.1:1234/mcp"
    );
    assert_eq!(
        std::fs::metadata(&config_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    driver.shutdown().await.unwrap();
    drop(driver);
    assert!(!config_path.exists(), "the token file goes with the driver");
}

/// The background-agent report: one `--help` that failed or ran out of time after `--version`
/// answered was cached as "no activity flags" for the life of the server, so every later
/// launch ran without hook events and idle suspension killed its background subagents.
#[cfg(unix)]
#[tokio::test]
async fn claude_activity_probe_retries_a_help_probe_that_failed_once() {
    assert_claude_probe_recovers_after_one_bad_help("failed", "exit 1").await;
}

#[cfg(unix)]
#[tokio::test]
async fn claude_activity_probe_caches_a_help_probe_that_was_slow_once() {
    // Past the launching caller's two-second wait, within the probe's own budget.
    assert_claude_probe_recovers_after_one_bad_help("slow", "sleep 3").await;
}

#[cfg(unix)]
#[tokio::test]
async fn claude_activity_probe_retries_a_help_probe_that_timed_out_once() {
    // Past the probe's own budget: the probe is stopped and the next launch probes again.
    assert_claude_probe_recovers_after_one_bad_help("timed-out", "sleep 12; exit 1").await;
}

#[cfg(unix)]
async fn assert_claude_probe_recovers_after_one_bad_help(case: &str, first_help: &str) {
    let probe_context = ClaudeActivityProbeTestContext::new();
    let temp = TempDir::new().unwrap();
    let marker_path = temp.path().join("help-failed-once");
    let script = format!(
        "#!/bin/sh\ncase \"$1\" in\n  --version) printf '%s\\n' '2.1.300 (Claude Code)';;\n  --help) if [ ! -f '{marker}' ]; then touch '{marker}'; {first_help}; fi; printf '%s\\n' '--include-hook-events --forward-subagent-text';;\n  *) exit 1;;\nesac\n",
        marker = marker_path.display(),
    );
    let executable = executable_fixture(&temp, &format!("claude-help-{case}"), &script, "");
    let binary_path = executable.to_string_lossy().into_owned();

    assert_eq!(
        probe_context.probe(&binary_path).await,
        ClaudeActivitySupport::default(),
        "{case}: the launch that met the failure runs without the flags"
    );
    // A detached probe may still be finishing its cleanup; the next launch must not reuse
    // the failure.
    let later = timeout(Duration::from_secs(20), async {
        loop {
            let support = probe_context.probe(&binary_path).await;
            if support != ClaudeActivitySupport::default() {
                return support;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_default();
    assert_eq!(
        later,
        ClaudeActivitySupport {
            include_hook_events: true,
            forward_subagent_text: true,
            transcript_recovery: false,
        },
        "{case}: a later launch of the same working Claude gets the activity flags"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn claude_activity_probe_invalidates_on_executable_metadata_and_version_change() {
    let probe_context = ClaudeActivityProbeTestContext::new();
    let temp = TempDir::new().unwrap();
    let count_path = temp.path().join("probe-count");
    let first_script = format!(
        "#!/bin/sh\nprintf x >> '{}'\ncase \"$1\" in\n  --version) printf '%s\\n' '2.1.218';;\n  --help) printf '%s\\n' '--include-hook-events --forward-subagent-text';;\n  *) exit 1;;\nesac\n",
        count_path.display()
    );
    let executable = executable_fixture(&temp, "claude-changing-probe", &first_script, "");
    assert!(
        probe_context
            .probe(executable.to_string_lossy().as_ref())
            .await
            .include_hook_events
    );

    let second_script = format!(
        "#!/bin/sh\nprintf x >> '{}'\n# changed executable metadata and output\ncase \"$1\" in\n  --version) printf '%s\\n' '2.1.219';;\n  --help) printf '%s\\n' '--unrelated-flag';;\n  *) exit 1;;\nesac\n",
        count_path.display()
    );
    executable_fixture::write_executable(&executable, second_script);
    let changed = probe_context
        .probe(executable.to_string_lossy().as_ref())
        .await;

    assert_eq!(changed, ClaudeActivitySupport::default());
    assert_eq!(
        std::fs::read_to_string(count_path).expect("probe count"),
        "xxxx",
        "metadata/version changes must execute a new version/help probe"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn claude_activity_probe_success_cache_is_lru_bounded() {
    let probe_context = ClaudeActivityProbeTestContext::new();
    probe_context.seed_cache(65);

    assert_eq!(
        probe_context.cache_len(),
        64,
        "the ready cache must prune its least recently used entry"
    );
    let cached_paths = probe_context.cache_paths();
    assert!(
        !cached_paths
            .iter()
            .any(|path| path == "/bibcode-test/claude-cache-0"),
        "the oldest executable must be evicted"
    );
    assert!(
        cached_paths
            .iter()
            .any(|path| path == "/bibcode-test/claude-cache-64"),
        "the newest executable must remain cached"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn claude_activity_probe_timeout_downgrades_without_blocking_launch() {
    let probe_context = ClaudeActivityProbeTestContext::new();
    let temp = TempDir::new().unwrap();
    let executable = executable_fixture(
        &temp,
        "claude-slow-probe",
        "#!/bin/sh\ncase \"$1\" in\n  --version|--help) sleep 5; exit 0;;\n  *) cat >/dev/null;;\nesac\n",
        "",
    );
    let started_at = Instant::now();
    let support = probe_context
        .probe(executable.to_string_lossy().as_ref())
        .await;

    assert_eq!(support, ClaudeActivitySupport::default());
    assert!(
        started_at.elapsed() < Duration::from_millis(2_750),
        "the complete probe must be bounded by its two-second timeout"
    );

    let factory = probe_context.driver_factory(temp.path().join("attachments"));
    let mut request = launch();
    request.provider = "claudeAgent".to_owned();
    request.binary_path = executable.to_string_lossy().into_owned();
    request.cwd = temp.path().to_path_buf();
    let driver = factory.create(request).await.unwrap();
    assert_eq!(
        driver.start().await.unwrap().activity_capabilities,
        ActivityCapabilities::none()
    );
    driver.shutdown().await.unwrap();
}

#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn claude_activity_probe_deadline_expires_while_resolution_is_blocked() {
    let probe_context = ClaudeActivityProbeTestContext::new();
    let temp = TempDir::new().unwrap();
    let executable =
        executable_fixture(&temp, "claude-probe-resolution", "#!/bin/sh\nexit 0\n", "");
    let binary_path = executable.to_string_lossy().into_owned();
    let gate = ClaudeActivityProbeResolutionGate::new();
    let probe_gate = gate.clone();
    let mut probe = tokio::spawn(async move {
        probe_context
            .probe_with_resolution_gate(&binary_path, probe_gate)
            .await
    });

    gate.wait_until_blocked().await;
    tokio::time::advance(Duration::from_secs(2)).await;
    tokio::time::resume();
    let outcome = timeout(Duration::from_secs(10), &mut probe).await;
    gate.release();

    assert_eq!(
        outcome
            .expect("the resolution deadline must expire independently of wall-clock scheduling")
            .expect("probe task"),
        ClaudeActivitySupport::default()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn claude_activity_probe_timeout_reaps_process_tree() {
    let probe_context = ClaudeActivityProbeTestContext::new();
    let temp = TempDir::new().unwrap();
    let descendant_pid_path = temp.path().join("descendant-pid");
    let script = format!(
        "#!/bin/sh\ncase \"$1\" in\n  --version|--help) sleep 30 & printf '%s' \"$!\" > '{}'; wait;;\n  *) cat >/dev/null;;\nesac\n",
        descendant_pid_path.display()
    );
    let executable = executable_fixture(&temp, "claude-probe-tree", &script, "");
    let binary_path = executable.to_string_lossy().into_owned();

    assert_eq!(
        timeout(Duration::from_secs(10), probe_context.probe(&binary_path))
            .await
            .expect("the probe must retain a bounded outer watchdog"),
        ClaudeActivitySupport::default()
    );
    let descendant_pid = std::fs::read_to_string(&descendant_pid_path)
        .expect("descendant pid")
        .parse::<u32>()
        .expect("numeric descendant pid");
    timeout(Duration::from_secs(5), async {
        loop {
            let alive = std::process::Command::new("kill")
                .args(["-0", &descendant_pid.to_string()])
                .status()
                .is_ok_and(|status| status.success());
            if !alive {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("probe descendant must not survive or remain zombie");
}

#[cfg(unix)]
#[tokio::test]
async fn claude_targeted_activity_startup_capabilities_follow_hook_sink_availability() {
    let probe_context = ClaudeActivityProbeTestContext::new();
    let temp = TempDir::new().unwrap();
    let supported = executable_fixture(
        &temp,
        "claude-capabilities-supported",
        r#"#!/bin/sh
case "$1" in
  --version) printf '%s\n' '2.1.218'; exit 0;;
  --help) printf '%s\n' '--include-hook-events --forward-subagent-text'; exit 0;;
esac
cat >/dev/null
"#,
        "",
    );
    let unsupported = executable_fixture(
        &temp,
        "claude-capabilities-unsupported",
        "#!/bin/sh\ncase \"$1\" in\n  --version) printf '%s\\n' '2.1.218'; exit 0;;\n  --help) exit 0;;\nesac\ncat >/dev/null\n",
        "",
    );
    let factory = probe_context.driver_factory(temp.path().join("attachments"));

    let mut supported_request = launch();
    supported_request.provider = "claudeAgent".to_owned();
    supported_request.binary_path = supported.to_string_lossy().into_owned();
    supported_request.cwd = temp.path().to_path_buf();
    let supported_driver = factory.create(supported_request).await.unwrap();
    assert_eq!(
        supported_driver
            .start()
            .await
            .unwrap()
            .activity_capabilities,
        ActivityCapabilities {
            actors: true,
            attributed_activity: true,
            background_work: false,
            history_recovery: ActivityHistoryRecovery::None,
            terminal_observation: false,
            targeted_actor_cancellation: true,
        }
    );
    supported_driver.shutdown().await.unwrap();

    let mut unsupported_request = launch();
    unsupported_request.provider = "claudeAgent".to_owned();
    unsupported_request.binary_path = unsupported.to_string_lossy().into_owned();
    unsupported_request.cwd = temp.path().to_path_buf();
    let unsupported_driver = factory.create(unsupported_request).await.unwrap();
    assert_eq!(
        unsupported_driver
            .start()
            .await
            .unwrap()
            .activity_capabilities,
        ActivityCapabilities::none()
    );
    unsupported_driver.shutdown().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn native_claude_driver_routes_stable_hook_input_through_activity_plumbing() {
    let probe_context = ClaudeActivityProbeTestContext::new();
    let temp = TempDir::new().unwrap();
    let executable = executable_fixture(
        &temp,
        "claude-hook-input",
        r#"#!/bin/sh
case "$1" in
  --version) printf '%s\n' '2.1.218'; exit 0;;
  --help) printf '%s\n' '--include-hook-events --forward-subagent-text'; exit 0;;
esac
printf '%s\n' '{"type":"system","subtype":"hook_response","session_id":"hook-session","hook_event":"SubagentStart","hook_id":"wrapper-only","uuid":"wrapper-1"}'
printf '%s\n' '{"hook_event_name":"SubagentStart","session_id":"hook-session","agent_id":"agent-stable-1","agent_type":"Explore","transcript_path":"/tmp/hook-session.jsonl","cwd":"/workspace"}'
cat >/dev/null
"#,
        "",
    );
    let factory = probe_context.driver_factory(temp.path().join("attachments"));
    let mut request = launch();
    request.provider = "claudeAgent".to_owned();
    request.binary_path = executable.to_string_lossy().into_owned();
    request.cwd = temp.path().to_path_buf();
    request.resume_cursor = Some(json!({"sessionId":"hook-session"}));
    use_empty_claude_config(&mut request, &temp);
    let driver = factory.create(request).await.unwrap();

    assert_eq!(
        driver.start().await.unwrap().activity_capabilities,
        ActivityCapabilities {
            actors: true,
            attributed_activity: true,
            background_work: false,
            history_recovery: ActivityHistoryRecovery::None,
            terminal_observation: false,
            targeted_actor_cancellation: true,
        }
    );
    let event = timeout(Duration::from_secs(2), driver.next_event())
        .await
        .unwrap()
        .expect("activity event");
    assert_eq!(event.event_type, "activity.native");
    assert!(
        event
            .native_event_id
            .as_ref()
            .is_some_and(|id| id.as_str().starts_with("claude:hook:"))
    );
    assert_eq!(event.activity.len(), 3);
    assert!(matches!(
        &event.activity[0],
        ProviderActivityMutation::SetScope {
            capabilities: ActivityCapabilities {
                actors: true,
                attributed_activity: true,
                background_work: false,
                terminal_observation: false,
                ..
            },
            observation_state: ActivityObservationState::Live,
        }
    ));
    driver.shutdown().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn native_claude_driver_captures_authenticated_http_hook_input_from_launch_settings() {
    let probe_context = ClaudeActivityProbeTestContext::new();
    let temp = TempDir::new().unwrap();
    let settings_path = temp.path().join("launch-settings.json");
    let token_path = temp.path().join("hook-token");
    let args_path = temp.path().join("launch-args");
    let executable = executable_fixture(
        &temp,
        "claude-http-hook",
        r#"#!/bin/sh
case "$1" in
  --version) printf '%s\n' '2.1.218'; exit 0;;
  --help) printf '%s\n' '--include-hook-events --forward-subagent-text'; exit 0;;
esac
printf '%s\n' "$@" > "$BIBCODE_TEST_ARGS_CAPTURE"
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--settings" ]; then
    shift
    printf '%s' "$1" > "$BIBCODE_TEST_SETTINGS_CAPTURE"
    break
  fi
  shift
done
printf '%s' "$BIBCODE_CLAUDE_HOOK_TOKEN" > "$BIBCODE_TEST_TOKEN_CAPTURE"
cat >/dev/null
"#,
        "",
    );
    let factory = probe_context.driver_factory(temp.path().join("attachments"));
    let mut request = launch();
    request.provider = "claudeAgent".to_owned();
    request.binary_path = executable.to_string_lossy().into_owned();
    request.cwd = temp.path().to_path_buf();
    request.resume_cursor = Some(json!({"sessionId":"http-hook-session"}));
    use_empty_claude_config(&mut request, &temp);
    request.environment.insert(
        "BIBCODE_TEST_SETTINGS_CAPTURE".to_owned(),
        settings_path.to_string_lossy().into_owned(),
    );
    request.environment.insert(
        "BIBCODE_TEST_TOKEN_CAPTURE".to_owned(),
        token_path.to_string_lossy().into_owned(),
    );
    request.environment.insert(
        "BIBCODE_TEST_ARGS_CAPTURE".to_owned(),
        args_path.to_string_lossy().into_owned(),
    );
    let driver = factory.create(request).await.unwrap();
    assert_eq!(
        driver.start().await.unwrap().activity_capabilities,
        ActivityCapabilities {
            actors: true,
            attributed_activity: true,
            background_work: false,
            history_recovery: ActivityHistoryRecovery::None,
            terminal_observation: false,
            targeted_actor_cancellation: true,
        }
    );
    wait_for_captures(&[&settings_path, &token_path, &args_path]).await;
    let settings: Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    let handler = &settings["hooks"]["SubagentStart"][0]["hooks"][0];
    assert_eq!(handler["type"], "http");
    assert_eq!(
        handler["allowedEnvVars"],
        json!(["BIBCODE_CLAUDE_HOOK_TOKEN"])
    );
    assert_eq!(
        handler["headers"]["Authorization"],
        "Bearer $BIBCODE_CLAUDE_HOOK_TOKEN"
    );
    let hook_url = handler["url"].as_str().expect("hook URL");
    assert!(hook_url.starts_with("http://127.0.0.1:"));
    for hook_name in [
        "SubagentStop",
        "PreToolUse",
        "PostToolUse",
        "PostToolUseFailure",
    ] {
        assert_eq!(settings["hooks"][hook_name][0]["hooks"][0], *handler);
    }
    let token = std::fs::read_to_string(&token_path).unwrap();
    assert!(
        token.len() >= 64,
        "hook token must contain at least 256 bits"
    );
    assert!(
        !std::fs::read_to_string(&args_path)
            .unwrap()
            .contains(&token),
        "the secret must never appear in process arguments"
    );
    let client = reqwest::Client::new();
    assert_eq!(
        client
            .post(hook_url)
            .header("Authorization", "Bearer wrong")
            .json(&json!({
                "hook_event_name":"SubagentStart",
                "session_id":"http-hook-session",
                "agent_id":"wrong-auth-agent",
                "agent_type":"Explore"
            }))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::FORBIDDEN
    );
    assert_eq!(
        client
            .post(hook_url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .body(vec![b'x'; 70 * 1024])
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::PAYLOAD_TOO_LARGE
    );
    assert!(
        timeout(Duration::from_millis(100), driver.next_event())
            .await
            .is_err(),
        "rejected requests must not enter the activity channel"
    );
    assert!(
        client
            .post(hook_url)
            .header("Authorization", format!("Bearer {token}"))
            .json(&json!({
                "hook_event_name":"SubagentStart",
                "session_id":"http-hook-session",
                "agent_id":"agent-http-1",
                "agent_type":"Explore",
                "transcript_path":"/tmp/http-hook-session.jsonl",
                "cwd":"/workspace"
            }))
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let event = timeout(Duration::from_secs(2), driver.next_event())
        .await
        .unwrap()
        .expect("authenticated hook activity event");
    assert_eq!(event.event_type, "activity.native");
    assert_eq!(event.activity.len(), 3);

    driver.shutdown().await.unwrap();
    assert!(
        client
            .post(hook_url)
            .header("Authorization", format!("Bearer {token}"))
            .json(&json!({}))
            .send()
            .await
            .is_err(),
        "hook sink must stop with the driver"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn native_claude_driver_closes_hook_sink_and_event_channel_after_natural_exit() {
    let probe_context = ClaudeActivityProbeTestContext::new();
    let temp = TempDir::new().unwrap();
    let settings_path = temp.path().join("natural-exit-settings.json");
    let token_path = temp.path().join("natural-exit-hook-token");
    let exit_release_path = temp.path().join("natural-exit-release");
    let executable = executable_fixture(
        &temp,
        "claude-http-hook-natural-exit",
        r#"#!/bin/sh
case "$1" in
  --version) printf '%s\n' '2.1.218'; exit 0;;
  --help) printf '%s\n' '--include-hook-events --forward-subagent-text'; exit 0;;
esac
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--settings" ]; then
    shift
    printf '%s' "$1" > "$BIBCODE_TEST_SETTINGS_CAPTURE"
    break
  fi
  shift
done
printf '%s' "$BIBCODE_CLAUDE_HOOK_TOKEN" > "$BIBCODE_TEST_TOKEN_CAPTURE"
while [ ! -f "$BIBCODE_TEST_EXIT_RELEASE" ]; do sleep 0.01; done
"#,
        "",
    );
    let factory = probe_context.driver_factory(temp.path().join("attachments"));
    let mut request = launch();
    request.provider = "claudeAgent".to_owned();
    request.binary_path = executable.to_string_lossy().into_owned();
    request.cwd = temp.path().to_path_buf();
    request.resume_cursor = Some(json!({"sessionId":"natural-exit-hook-session"}));
    use_empty_claude_config(&mut request, &temp);
    request.environment.insert(
        "BIBCODE_TEST_SETTINGS_CAPTURE".to_owned(),
        settings_path.to_string_lossy().into_owned(),
    );
    request.environment.insert(
        "BIBCODE_TEST_TOKEN_CAPTURE".to_owned(),
        token_path.to_string_lossy().into_owned(),
    );
    request.environment.insert(
        "BIBCODE_TEST_EXIT_RELEASE".to_owned(),
        exit_release_path.to_string_lossy().into_owned(),
    );
    let driver = factory.create(request).await.unwrap();
    driver.start().await.unwrap();

    wait_for_captures(&[&settings_path, &token_path]).await;
    let settings: Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    let hook_url = settings["hooks"]["SubagentStart"][0]["hooks"][0]["url"]
        .as_str()
        .expect("hook URL")
        .to_owned();
    let token = std::fs::read_to_string(&token_path).unwrap();
    let client = reqwest::Client::new();
    assert!(
        client
            .post(&hook_url)
            .header("Authorization", format!("Bearer {token}"))
            .json(&json!({
                "hook_event_name":"SubagentStart",
                "session_id":"natural-exit-hook-session",
                "agent_id":"agent-before-exit",
                "agent_type":"Explore"
            }))
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let event = timeout(Duration::from_secs(2), driver.next_event())
        .await
        .unwrap()
        .expect("final accepted hook event");
    assert_eq!(event.event_type, "activity.native");

    std::fs::write(&exit_release_path, b"exit").unwrap();
    assert!(
        timeout(Duration::from_secs(2), driver.next_event())
            .await
            .expect("event channel must close after natural child exit")
            .is_none()
    );
    assert!(
        client
            .post(&hook_url)
            .header("Authorization", format!("Bearer {token}"))
            .json(&json!({}))
            .send()
            .await
            .is_err(),
        "hook endpoint must stop after natural child exit"
    );
}

#[tokio::test]
async fn native_opencode_driver_supports_session_turn_and_control_commands() {
    const CHILDREN_OBSERVED: usize = 1;
    const STATUS_OBSERVED: usize = 2;
    const HISTORY_OBSERVED: usize = 4;
    const LINEAGE_HANDSHAKE: usize = CHILDREN_OBSERVED | STATUS_OBSERVED | HISTORY_OBSERVED;
    let child_busy_release = Arc::new(tokio::sync::Notify::new());
    let reconciliation_observed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let event_release = child_busy_release.clone();
    let app = Router::new()
        .route(
            "/session",
            post(|| async { Json(json!({"id":"native-opencode-session"})) }),
        )
        .route(
            "/event",
            get(move || {
                let event_release = event_release.clone();
                async move {
                    let created = stream::once(async {
                        Ok::<Event, std::convert::Infallible>(
                            Event::default().data(
                                json!({
                                    "id": "native-child-created",
                                    "type": "session.created",
                                    "properties": {
                                        "sessionID": "native-opencode-child",
                                        "info": {
                                            "id": "native-opencode-child",
                                            "parentID": "native-opencode-session",
                                            "title": "Native child",
                                            "time": { "created": 1, "updated": 1 }
                                        }
                                    }
                                })
                                .to_string(),
                            ),
                        )
                    });
                    let busy = stream::once(async move {
                        event_release.notified().await;
                        Ok::<Event, std::convert::Infallible>(
                            Event::default().data(
                                json!({
                                    "id": "native-child-busy",
                                    "type": "session.status",
                                    "properties": {
                                        "sessionID": "native-opencode-child",
                                        "status": { "type": "busy" }
                                    }
                                })
                                .to_string(),
                            ),
                        )
                    });
                    Sse::new(created.chain(busy))
                }
            }),
        )
        .route(
            "/session/{session_id}/children",
            get({
                let reconciliation_observed = reconciliation_observed.clone();
                move |axum::extract::Path(session_id): axum::extract::Path<String>| {
                    let reconciliation_observed = reconciliation_observed.clone();
                    async move {
                        reconciliation_observed
                            .fetch_or(CHILDREN_OBSERVED, std::sync::atomic::Ordering::SeqCst);
                        Json(if session_id == "native-opencode-session" {
                            json!([{
                                "id": "native-opencode-child",
                                "parentID": "native-opencode-session",
                                "title": "Native child",
                                "time": { "created": 1, "updated": 1 }
                            }])
                        } else {
                            json!([])
                        })
                    }
                }
            }),
        )
        .route(
            "/session/status",
            get({
                let reconciliation_observed = reconciliation_observed.clone();
                move || {
                    let reconciliation_observed = reconciliation_observed.clone();
                    async move {
                        reconciliation_observed
                            .fetch_or(STATUS_OBSERVED, std::sync::atomic::Ordering::SeqCst);
                        Json(json!({
                            "native-opencode-session": { "type": "idle" },
                            "native-opencode-child": { "type": "idle" }
                        }))
                    }
                }
            }),
        )
        .route(
            "/session/{session_id}/prompt_async",
            post(|| async { Json(json!({})) }),
        )
        .route(
            "/session/{session_id}/command",
            post(|| async { Json(json!({})) }),
        )
        .route(
            "/session/{session_id}/abort",
            post(|| async { Json(json!({})) }),
        )
        .route(
            "/session/{session_id}/message",
            get({
                let reconciliation_observed = reconciliation_observed.clone();
                move |axum::extract::Path(session_id): axum::extract::Path<String>| {
                    let reconciliation_observed = reconciliation_observed.clone();
                    async move {
                        reconciliation_observed
                            .fetch_or(HISTORY_OBSERVED, std::sync::atomic::Ordering::SeqCst);
                        Json(if session_id == "native-opencode-child" {
                            json!([{
                                "info": {
                                    "id": "native-child-message",
                                    "sessionID": "native-opencode-child",
                                    "role": "assistant"
                                },
                                "parts": []
                            }])
                        } else {
                            json!([])
                        })
                    }
                }
            }),
        )
        .route(
            "/session/{session_id}/revert",
            post(|| async { Json(json!({})) }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let temp = TempDir::new().unwrap();
    let factory = NativeProviderDriverFactory::new(temp.path().join("attachments"));
    let mut request = launch();
    request.provider = "opencode".to_owned();
    request.cwd = temp.path().to_path_buf();
    request.endpoint = Some(format!("http://{address}"));
    request.server_password = Some("secret".to_owned());
    request.agent = Some("reviewer".to_owned());
    request.model = Some("openai/gpt-5".to_owned());

    let driver = factory.create(request).await.unwrap();
    let activity_database = Database::open_in_memory().await.unwrap();
    activity_database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .unwrap();
    let activity_projection = ActivityProjection::new(ActivityRepository::new(activity_database));
    let activity_scope = ActivityScopeSeed::thread(
        "thread:native-opencode-driver",
        "native-opencode-driver",
        "opencode",
        Some("opencode"),
        ActivityCapabilities::none(),
    )
    .unwrap();
    activity_projection
        .ensure_scope(activity_scope.clone())
        .await
        .unwrap();
    let started = driver.start().await.unwrap();
    assert_eq!(
        started.resume_cursor,
        Some(json!({"sessionId":"native-opencode-session"}))
    );
    let event = timeout(Duration::from_secs(2), driver.next_event())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(event.event_type, "session.started");
    timeout(Duration::from_secs(2), async {
        loop {
            let event = driver.next_event().await.expect("OpenCode provider event");
            if !event.activity.is_empty() {
                activity_projection
                    .apply(
                        &activity_scope.scope_id,
                        event
                            .native_event_id
                            .as_ref()
                            .expect("driver preserves reconciliation native ID")
                            .as_str()
                            .to_owned(),
                        event.activity.clone(),
                        "2026-07-25T12:20:00Z".to_owned(),
                    )
                    .await
                    .expect("driver reconciliation applies through the durable projection");
            }
            assert!(
                !event.activity.iter().any(|mutation| matches!(
                    mutation,
                    ProviderActivityMutation::UpsertActor(actor)
                        if actor.id == "opencode:session:native-opencode-child"
                            && actor.status.as_str() == "running"
                )),
                "child Busy must remain blocked until lineage admission"
            );
            if event.activity.iter().any(|mutation| {
                matches!(
                    mutation,
                    ProviderActivityMutation::UpsertActor(actor)
                        if actor.id == "opencode:session:native-opencode-child"
                            && actor.status.as_str() == "waiting"
                )
            }) {
                break;
            }
        }
    })
    .await
    .expect("OpenCode child lineage admission");
    assert_eq!((), ());
    assert_eq!(
        reconciliation_observed.load(std::sync::atomic::Ordering::SeqCst) & LINEAGE_HANDSHAKE,
        LINEAGE_HANDSHAKE,
        "lineage admission requires children, status, and history reconciliation"
    );
    child_busy_release.notify_one();
    assert!(
        driver
            .send(
                "hello".to_owned(),
                vec![image_attachment(&temp)],
                "default".to_owned(),
            )
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        driver
            .send(
                "/review src/provider".to_owned(),
                Vec::new(),
                "default".to_owned(),
            )
            .await
            .unwrap()
            .is_some()
    );
    driver.interrupt(None).await.unwrap();
    driver.set_model("openai/gpt-5.4".to_owned()).await.unwrap();
    driver.rollback(0, "rewind-key".to_owned()).await.unwrap();
    assert!(matches!(
        driver.rollback(-1, "rewind-key".to_owned()).await,
        Err(ProviderRuntimeError::Provider { provider, .. }) if provider == "opencode"
    ));
    assert!(matches!(
        driver.set_mode("full-access".to_owned()).await,
        Err(ProviderRuntimeError::UnsupportedCapability { provider, .. }) if provider == "opencode"
    ));
    assert!(matches!(
        driver
            .approve("unknown".to_owned(), "accept".to_owned())
            .await,
        Err(ProviderRuntimeError::Provider { provider, .. }) if provider == "opencode"
    ));
    assert!(matches!(
        driver.answer("unknown".to_owned(), json!({})).await,
        Err(ProviderRuntimeError::Provider { provider, .. }) if provider == "opencode"
    ));
    let activity_event = timeout(Duration::from_secs(2), async {
        loop {
            let event = driver.next_event().await.expect("OpenCode provider event");
            if matches!(
                event.activity.as_slice(),
                [ProviderActivityMutation::UpsertActor(actor)]
                    if actor.id == "opencode:session:native-opencode-child"
                        && actor.status.as_str() == "running"
            ) {
                break event;
            }
        }
    })
    .await
    .expect("live OpenCode child activity");
    assert_eq!(activity_event.event_type, "activity.native");
    assert!(
        activity_event
            .native_event_id
            .as_ref()
            .is_some_and(|event_id| event_id.as_str().starts_with("opencode:activity:"))
    );
    activity_projection
        .apply(
            &activity_scope.scope_id,
            activity_event
                .native_event_id
                .as_ref()
                .expect("driver preserves live native ID")
                .as_str()
                .to_owned(),
            activity_event.activity.clone(),
            "2026-07-25T12:20:01Z".to_owned(),
        )
        .await
        .expect("driver live batch applies through the durable projection");
    let snapshot = activity_projection
        .snapshot(&activity_scope.scope)
        .await
        .expect("driver activity snapshot");
    assert!(snapshot.actors.iter().any(|actor| {
        actor.id == "opencode:session:native-opencode-child"
            && actor.status == ActivityLifecycle::Running
    }));
    driver.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test]
async fn native_codex_driver_supports_session_turn_and_control_commands() {
    let temp = TempDir::new().unwrap();
    let initialize_response_ready = temp.path().join("codex-initialize-response-ready");
    let thread_response_ready = temp.path().join("codex-thread-response-ready");
    let executable = executable_fixture(
        &temp,
        "codex-fixture",
        r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      [ -z "$BIBCODE_TEST_CODEX_INITIALIZE_RESPONSE_READY_MARKER" ] || : > "$BIBCODE_TEST_CODEX_INITIALIZE_RESPONSE_READY_MARKER"
      printf '{"id":%s,"result":{"userAgent":"fixture"}}\n' "$id"
      ;;
    *'"method":"thread/start"'*)
      [ -z "$BIBCODE_TEST_CODEX_THREAD_RESPONSE_READY_MARKER" ] || : > "$BIBCODE_TEST_CODEX_THREAD_RESPONSE_READY_MARKER"
      printf '{"id":%s,"result":{"cwd":"/tmp","model":"gpt-5","thread":{"id":"native-codex-thread","historyMode":"paginated"}}}\n' "$id"
      ;;
    *'"method":"mcpServerStatus/list"'*) printf '{"id":%s,"result":{"data":[],"nextCursor":null}}\n' "$id" ;;
    *'"method":"thread/goal/set"'*) printf '{"id":%s,"result":{"goal":{"status":"active"}}}\n' "$id" ;;
    *'"method":"turn/start"'*) printf '{"id":%s,"result":{"turn":{"id":"native-codex-turn"}}}\n{"method":"item/started","emittedAtMs":1001,"params":{"threadId":"native-codex-thread","turnId":"native-codex-turn","item":{"id":"spawn-1","type":"collabAgentToolCall","tool":"spawnAgent","status":"inProgress","senderThreadId":"native-codex-thread","receiverThreadIds":["native-child"],"agentsStates":{"native-child":{"status":"running","message":null}}},"startedAtMs":1001}}\n{"method":"turn/started","emittedAtMs":1002,"params":{"threadId":"native-child","turn":{"id":"native-child-turn","status":"inProgress","startedAt":1}}}\n' "$id" ;;
    *'"method":"turn/interrupt"'*) printf '{"id":%s,"result":{}}\n' "$id" ;;
    *'"method":"thread/turns/list"'*) printf '{"id":%s,"result":{"data":[{"id":"native-codex-turn","items":[]}],"nextCursor":null}}\n' "$id" ;;
    *'"method":"thread/revert"'*'"beforeTurnId":"native-codex-turn"'*) printf '{"id":%s,"result":{"thread":{"id":"native-codex-thread","historyMode":"paginated","turns":[]}}}\n' "$id" ;;
    *'"method":"shutdown"'*) printf '{"id":%s,"result":null}\n' "$id" ;;
  esac
done
"#,
        WINDOWS_CODEX_FIXTURE,
    );

    let factory = NativeProviderDriverFactory::new(temp.path().join("attachments"));
    let mut request = launch();
    request.provider = "codex".to_owned();
    request.binary_path = executable.to_string_lossy().into_owned();
    request.cwd = temp.path().to_path_buf();
    request.environment.insert(
        "BIBCODE_TEST_CODEX_INITIALIZE_RESPONSE_READY_MARKER".to_owned(),
        initialize_response_ready.to_string_lossy().into_owned(),
    );
    request.environment.insert(
        "BIBCODE_TEST_CODEX_THREAD_RESPONSE_READY_MARKER".to_owned(),
        thread_response_ready.to_string_lossy().into_owned(),
    );
    let driver = factory.create(request).await.unwrap();
    let owner_deadline = tokio::time::Instant::now() + Duration::from_secs(30);

    let started = match timeout_at(owner_deadline, driver.start()).await {
        Ok(Ok(started)) => started,
        Ok(Err(error)) => {
            let cleanup = timeout(Duration::from_secs(10), driver.shutdown()).await;
            panic!("native Codex fixture failed to start: {error}; cleanup={cleanup:?}");
        }
        Err(_) => {
            let cleanup = timeout(Duration::from_secs(10), driver.shutdown()).await;
            panic!(
                "native Codex fixture exceeded its integration deadline during startup: \
                 initialize_response_ready={}; thread_response_ready={}; cleanup={cleanup:?}",
                initialize_response_ready.is_file(),
                thread_response_ready.is_file(),
            );
        }
    };
    assert!(
        initialize_response_ready.is_file(),
        "Codex fixture must prepare its initialize response"
    );
    assert!(
        thread_response_ready.is_file(),
        "Codex fixture must prepare its thread/start response"
    );
    assert_eq!(
        started.resume_cursor,
        Some(json!({"threadId":"native-codex-thread"}))
    );
    assert_eq!(
        started.activity_capabilities,
        ActivityCapabilities {
            actors: true,
            attributed_activity: true,
            background_work: false,
            history_recovery: ActivityHistoryRecovery::None,
            terminal_observation: false,
            targeted_actor_cancellation: true,
        }
    );
    assert!(
        driver
            .send(
                "hello".to_owned(),
                vec![image_attachment(&temp)],
                "default".to_owned(),
            )
            .await
            .unwrap()
            .is_some()
    );
    let activity_event = timeout_at(owner_deadline, async {
        loop {
            let event = driver.next_event().await.expect("Codex provider event");
            if !event.activity.is_empty() {
                break event;
            }
        }
    })
    .await;
    let activity_event = match activity_event {
        Ok(event) => event,
        Err(error) => {
            let cleanup = timeout(Duration::from_secs(10), driver.shutdown()).await;
            panic!("live Codex activity event timed out: {error}; cleanup={cleanup:?}");
        }
    };
    assert_eq!(
        activity_event
            .native_event_id
            .as_ref()
            .map(ProviderNativeEventId::as_str),
        Some("codex:activity:0")
    );
    assert_eq!(activity_event.event_type, "activity.native");
    assert!(matches!(
        activity_event.activity.as_slice(),
        [ProviderActivityMutation::UpsertActor(actor)]
            if actor.id == "codex:thread:native-child"
    ));
    let control_event = timeout_at(owner_deadline, async {
        loop {
            let event = driver.next_event().await.expect("Codex control event");
            if !event.activity_controls.is_empty() {
                break event;
            }
        }
    })
    .await;
    let control_event = match control_event {
        Ok(event) => event,
        Err(error) => {
            let cleanup = timeout(Duration::from_secs(10), driver.shutdown()).await;
            panic!("live Codex child target timed out: {error}; cleanup={cleanup:?}");
        }
    };
    assert_eq!(control_event.event_type, "activity.native");
    assert_eq!(control_event.activity_controls.len(), 1);
    assert!(
        driver
            .send(
                "/goal finish coverage".to_owned(),
                Vec::new(),
                "default".to_owned(),
            )
            .await
            .unwrap()
            .is_some()
    );
    driver.interrupt(None).await.unwrap();
    // Like Codex 0.162.1, the fixture starts a paginated thread and answers `thread/revert`
    // only before the listed turn.
    timeout(
        Duration::from_secs(10),
        driver.rollback(1, "rewind-key".to_owned()),
    )
    .await
    .expect("Codex should answer the revert")
    .expect("Codex should revert the paginated thread");
    assert!(matches!(
        driver.rollback(-1, "rewind-key".to_owned()).await,
        Err(ProviderRuntimeError::Provider { provider, .. }) if provider == "codex"
    ));
    assert!(matches!(
        driver.set_mode("approval-required".to_owned()).await,
        Err(ProviderRuntimeError::UnsupportedCapability { provider, .. }) if provider == "codex"
    ));
    assert!(matches!(
        driver.set_model("other".to_owned()).await,
        Err(ProviderRuntimeError::UnsupportedCapability { provider, .. }) if provider == "codex"
    ));
    assert!(matches!(
        driver
            .approve("unknown".to_owned(), "accept".to_owned())
            .await,
        Err(ProviderRuntimeError::Provider { provider, .. }) if provider == "codex"
    ));
    assert!(matches!(
        driver.answer("unknown".to_owned(), json!({})).await,
        Err(ProviderRuntimeError::Provider { provider, .. }) if provider == "codex"
    ));
    match timeout_at(owner_deadline, driver.shutdown()).await {
        Ok(result) => result.expect("native Codex fixture should shut down"),
        Err(_) => {
            let cleanup = timeout(Duration::from_secs(10), driver.shutdown()).await;
            panic!("native Codex fixture cleanup exceeded its integration deadline: {cleanup:?}");
        }
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn targeted_activity_rpc_interrupts_only_the_selected_codex_subtree_and_catches_a_late_child()
{
    // Mutation caught: routing an Activity-row stop through the root interrupt, a sibling target,
    // or a client-supplied native ID instead of the server-owned canonical subtree fence.
    let state = TempDir::new().expect("state");
    let capture = state.path().join("codex-targeted-requests.ndjson");
    let late_observed = state.path().join("codex-late-emitted");
    let release_selected = state.path().join("codex-release-selected");
    let script = format!(
        r#"#!/bin/sh
capture='{}'
late_observed='{}'
release_selected='{}'
alpha_child_attempts=0
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$capture"
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*) printf '{{"id":%s,"result":{{"userAgent":"fixture"}}}}\n' "$id" ;;
    *'"method":"thread/start"'*) printf '{{"id":%s,"result":{{"cwd":"/tmp","model":"gpt-5","thread":{{"id":"provider-root"}}}}}}\n' "$id" ;;
    *'"method":"thread/resume"'*)
      printf '{{"id":%s,"result":{{"cwd":"/tmp","model":"gpt-5","thread":{{"id":"provider-root"}}}}}}\n' "$id"
      ;;
    *'"method":"thread/read"'*) printf '{{"id":%s,"result":{{"thread":{{"id":"provider-root","createdAt":1,"updatedAt":1,"status":{{"type":"idle"}},"turns":[]}}}}}}\n' "$id" ;;
    *'"method":"thread/list"'*) printf '{{"id":%s,"result":{{"data":[],"nextCursor":null,"backwardsCursor":null}}}}\n' "$id" ;;
    *'"method":"thread/backgroundTerminals/list"'*) printf '{{"id":%s,"result":{{"data":[],"nextCursor":null}}}}\n' "$id" ;;
    *'"method":"mcpServerStatus/list"'*) printf '{{"id":%s,"result":{{"data":[],"nextCursor":null}}}}\n' "$id" ;;
    *'"method":"turn/start"'*)
      printf '{{"id":%s,"result":{{"turn":{{"id":"root-turn"}}}}}}\n' "$id"
      printf '%s\n' '{{"method":"item/started","emittedAtMs":1001,"params":{{"threadId":"provider-root","turnId":"root-turn","item":{{"id":"spawn-root","type":"collabAgentToolCall","tool":"spawnAgent","status":"inProgress","senderThreadId":"provider-root","receiverThreadIds":["alpha","beta"],"agentsStates":{{"alpha":{{"status":"running","message":null}},"beta":{{"status":"running","message":null}}}}}},"startedAtMs":1001}}}}'
      printf '%s\n' '{{"method":"turn/started","emittedAtMs":1002,"params":{{"threadId":"alpha","turn":{{"id":"alpha-turn","status":"inProgress","startedAt":1}}}}}}'
      printf '%s\n' '{{"method":"turn/started","emittedAtMs":1003,"params":{{"threadId":"beta","turn":{{"id":"beta-turn","status":"inProgress","startedAt":1}}}}}}'
      printf '%s\n' '{{"method":"item/started","emittedAtMs":1004,"params":{{"threadId":"alpha","turnId":"alpha-turn","item":{{"id":"spawn-alpha","type":"collabAgentToolCall","tool":"spawnAgent","status":"inProgress","senderThreadId":"alpha","receiverThreadIds":["alpha-child"],"agentsStates":{{"alpha-child":{{"status":"running","message":null}}}}}},"startedAtMs":1004}}}}'
      printf '%s\n' '{{"method":"turn/started","emittedAtMs":1005,"params":{{"threadId":"alpha-child","turn":{{"id":"alpha-child-turn","status":"inProgress","startedAt":1}}}}}}'
      ;;
    *'"method":"turn/interrupt"'*'"threadId":"alpha"'*)
      printf '%s\n' '{{"method":"item/started","emittedAtMs":1010,"params":{{"threadId":"alpha","turnId":"alpha-turn","item":{{"id":"spawn-alpha-late","type":"collabAgentToolCall","tool":"spawnAgent","status":"inProgress","senderThreadId":"alpha","receiverThreadIds":["alpha-late"],"agentsStates":{{"alpha-late":{{"status":"running","message":null}}}}}},"startedAtMs":1010}}}}'
      printf '%s\n' '{{"method":"turn/started","emittedAtMs":1011,"params":{{"threadId":"alpha-late","turn":{{"id":"alpha-late-turn","status":"inProgress","startedAt":1}}}}}}'
      : > "$late_observed"
      while [ ! -f "$release_selected" ]; do sleep 0.01; done
      printf '{{"id":%s,"result":{{}}}}\n' "$id"
      ;;
    *'"method":"turn/interrupt"'*'"threadId":"alpha-child"'*)
      alpha_child_attempts=$((alpha_child_attempts + 1))
      if [ "$alpha_child_attempts" -gt 1 ]; then
        printf '{{"id":%s,"result":{{}}}}\n' "$id"
        printf '%s\n' '{{"method":"turn/completed","emittedAtMs":1021,"params":{{"threadId":"alpha-child","turn":{{"id":"alpha-child-turn","status":"completed","completedAt":3}}}}}}'
      fi
      ;;
    *'"method":"turn/interrupt"'*'"threadId":"alpha-late"'*)
      printf '{{"id":%s,"result":{{}}}}\n' "$id"
      printf '%s\n' '{{"method":"turn/completed","emittedAtMs":1020,"params":{{"threadId":"alpha-late","turn":{{"id":"alpha-late-turn","status":"completed","completedAt":3}}}}}}'
      printf '%s\n' '{{"method":"turn/completed","emittedAtMs":1022,"params":{{"threadId":"alpha","turn":{{"id":"alpha-turn","status":"completed","completedAt":3}}}}}}'
      printf '%s\n' '{{"method":"item/started","emittedAtMs":1030,"params":{{"threadId":"beta","turnId":"beta-turn","item":{{"id":"spawn-beta-followup","type":"collabAgentToolCall","tool":"spawnAgent","status":"inProgress","senderThreadId":"beta","receiverThreadIds":["beta-followup"],"agentsStates":{{"beta-followup":{{"status":"running","message":null}}}}}},"startedAtMs":1030}}}}'
      printf '%s\n' '{{"method":"turn/started","emittedAtMs":1031,"params":{{"threadId":"beta-followup","turn":{{"id":"beta-followup-turn","status":"inProgress","startedAt":4}}}}}}'
      printf '%s\n' '{{"method":"item/started","emittedAtMs":1032,"params":{{"threadId":"provider-root","turnId":"root-turn","item":{{"id":"spawn-root-followup","type":"collabAgentToolCall","tool":"spawnAgent","status":"inProgress","senderThreadId":"provider-root","receiverThreadIds":["root-followup"],"agentsStates":{{"root-followup":{{"status":"running","message":null}}}}}},"startedAtMs":1032}}}}'
      printf '%s\n' '{{"method":"turn/started","emittedAtMs":1033,"params":{{"threadId":"root-followup","turn":{{"id":"root-followup-turn","status":"inProgress","startedAt":4}}}}}}'
      ;;
    *'"method":"turn/interrupt"'*) printf '{{"id":%s,"result":{{}}}}\n' "$id" ;;
    *'"method":"shutdown"'*) printf '{{"id":%s,"result":null}}\n' "$id" ;;
  esac
done
"#,
        capture.display(),
        late_observed.display(),
        release_selected.display()
    );
    let executable = executable_fixture(&state, "codex-targeted-rpc", &script, "");
    let config = test_config(&state);
    hermetic_providers::write_hermetic_settings(
        &config.state_dir(),
        json!({
            "providerInstances": {
                "codex-targeted": {
                    "driver": "codex",
                    "enabled": true,
                    "config": { "binaryPath": executable }
                }
            }
        }),
    );
    let workspace = state.path().join("workspace");
    std::fs::create_dir(&workspace).expect("workspace");
    let handle = ServerRuntime::start(config.clone())
        .await
        .expect("production RPC server");
    let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket");
    tagged_rpc_request(
        &mut socket,
        "1001",
        "orchestration.dispatchCommand",
        json!({
            "type":"project.create","commandId":"targeted-project",
            "projectId":"targeted-project","title":"Targeted",
            "workspaceRoot":workspace,"defaultModelSelection":null,"createdAt":NOW
        }),
    )
    .await
    .expect("project created through public RPC");
    tagged_rpc_request(
        &mut socket,
        "1002",
        "orchestration.dispatchCommand",
        json!({
            "type":"thread.create","commandId":"targeted-thread","threadId":"targeted-thread",
            "projectId":"targeted-project","title":"Targeted thread",
            "modelSelection":{"instanceId":"codex-targeted","model":"gpt-5"},
            "runtimeMode":"full-access","interactionMode":"default","branch":null,
            "worktreePath":null,"createdAt":NOW
        }),
    )
    .await
    .expect("thread created through public RPC");
    tagged_rpc_request(
        &mut socket,
        "1003",
        "orchestration.dispatchCommand",
        json!({
            "type":"thread.turn.start","commandId":"targeted-turn","threadId":"targeted-thread",
            "message":{"messageId":"targeted-message","role":"user","text":"start","attachments":[]},
            "modelSelection":{"instanceId":"codex-targeted","model":"gpt-5"},
            "runtimeMode":"full-access","interactionMode":"default","createdAt":NOW
        }),
    )
    .await
    .expect("turn admitted");

    let alpha_control = timeout(Duration::from_secs(10), async {
        let mut request_id = 0_u64;
        loop {
            request_id += 1;
            let Ok(snapshot) = tagged_rpc_request(
                &mut socket,
                &format!("2{request_id:03}"),
                "activity.getSnapshot",
                json!({"_tag":"thread","threadId":"targeted-thread"}),
            )
            .await
            else {
                tokio::task::yield_now().await;
                continue;
            };
            if let Some(control) = snapshot["control"]["actors"]
                .as_array()
                .and_then(|controls| {
                    controls.iter().find(|control| {
                        control["actorId"] == "codex:thread:alpha"
                            && control["state"] == "available"
                            && control["activeDescendantCount"] == 1
                    })
                })
                .cloned()
            {
                break (snapshot, control);
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("alpha control becomes available");
    let (snapshot, alpha_control) = alpha_control;
    assert!(
        snapshot["control"]["actors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|control| {
                control["actorId"] == "codex:thread:beta" && control["state"] == "available"
            })
    );
    let (mut activity_stream, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("initial Activity stream WebSocket");
    stream_rpc_request(
        &mut activity_stream,
        "2901",
        "subscribeActivity",
        json!({"_tag":"thread","threadId":"targeted-thread"}),
    )
    .await;
    let initial_stream_snapshot = stream_rpc_message(&mut activity_stream).await;
    assert!(matches!(
        initial_stream_snapshot,
        ServerMessage::Chunk { ref values, .. }
            if values[0]["kind"] == "snapshot"
                && values[0]["snapshot"]["control"]["actors"]
                    .as_array()
                    .is_some_and(|actors| actors.iter().any(|actor| {
                        actor["actorId"] == "codex:thread:alpha"
                            && actor["state"] == "available"
                    }))
    ));
    ack_stream_rpc(&mut activity_stream, "2901").await;
    let cancellation_address = handle.local_addr();
    let alpha_revision = alpha_control["controlRevision"].clone();
    let cancellation_task = tokio::spawn(async move {
        let (mut cancellation_socket, _) = connect_async(format!("ws://{cancellation_address}/ws"))
            .await
            .expect("cancellation WebSocket");
        let result = tagged_rpc_request(
            &mut cancellation_socket,
            "3001",
            "activity.cancelSubtree",
            json!({
                "scope": {"_tag":"thread","threadId":"targeted-thread"},
                "scopeId":"thread:targeted-thread",
                "actorId":"codex:thread:alpha",
                "expectedControlRevision":alpha_revision
            }),
        )
        .await;
        cancellation_socket
            .close(None)
            .await
            .expect("close cancellation socket");
        result
    });
    timeout(Duration::from_secs(10), async {
        while !late_observed.exists() {
            tokio::task::yield_now().await;
        }
        let mut actor_requested = false;
        let mut late_actor_requested = false;
        let mut operation_requested = false;
        loop {
            let ServerMessage::Chunk { values, .. } =
                stream_rpc_message(&mut activity_stream).await
            else {
                panic!("Activity stream must remain open while cancellation is requested");
            };
            ack_stream_rpc(&mut activity_stream, "2901").await;
            for value in values {
                if value["kind"] != "control-delta" {
                    continue;
                }
                for change in value["delta"]["changes"].as_array().into_iter().flatten() {
                    actor_requested |= change["kind"] == "actor-upserted"
                        && change["actor"]["actorId"] == "codex:thread:alpha"
                        && change["actor"]["state"] == "requested";
                    late_actor_requested |= change["kind"] == "actor-upserted"
                        && change["actor"]["actorId"] == "codex:thread:alpha-late"
                        && change["actor"]["state"] == "requested";
                    operation_requested |= change["kind"] == "operation-upserted"
                        && change["operation"]["rootActorId"] == "codex:thread:alpha"
                        && change["operation"]["state"] == "requested";
                }
            }
            if actor_requested && late_actor_requested && operation_requested {
                break;
            }
        }
    })
    .await
    .expect("the original stream observes the held cancellation as requested");
    activity_stream
        .close(None)
        .await
        .expect("close original Activity stream while requested");

    let (mut requested_reconnect, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("reconnected requested Activity stream");
    stream_rpc_request(
        &mut requested_reconnect,
        "3901",
        "subscribeActivity",
        json!({"_tag":"thread","threadId":"targeted-thread"}),
    )
    .await;
    let requested_initial = stream_rpc_message(&mut requested_reconnect).await;
    let ServerMessage::Chunk { values, .. } = requested_initial else {
        panic!("reconnected Activity stream must begin with a snapshot");
    };
    let restored = &values[0];
    assert_eq!(restored["kind"], "snapshot");
    assert!(
        restored["snapshot"]["control"]["operations"]
            .as_array()
            .is_some_and(|operations| operations.iter().any(|operation| {
                operation["rootActorId"] == "codex:thread:alpha"
                    && operation["state"] == "requested"
            }))
    );
    assert!(
        restored["snapshot"]["control"]["actors"]
            .as_array()
            .is_some_and(|actors| actors.iter().any(|actor| {
                actor["actorId"] == "codex:thread:alpha" && actor["state"] == "requested"
            }))
    );
    assert!(
        restored["snapshot"]["control"]["actors"]
            .as_array()
            .is_some_and(|actors| actors.iter().any(|actor| {
                actor["actorId"] == "codex:thread:alpha-late" && actor["state"] == "requested"
            }))
    );
    ack_stream_rpc(&mut requested_reconnect, "3901").await;
    requested_reconnect
        .close(None)
        .await
        .expect("close restored requested Activity stream");

    std::fs::write(&release_selected, b"release").expect("release selected dispatch");
    cancellation_task
        .await
        .expect("cancellation task")
        .expect("selected subtree cancellation accepted");

    timeout(Duration::from_secs(10), async {
        loop {
            let captured = std::fs::read_to_string(&capture).unwrap_or_default();
            let interrupted = captured
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .filter(|request| request["method"] == "turn/interrupt")
                .collect::<Vec<_>>();
            if interrupted.len() == 3 {
                break interrupted;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .map(|interrupted| {
        assert_eq!(
            interrupted[0]["params"],
            json!({"threadId":"alpha","turnId":"alpha-turn"}),
            "the selected actor must dispatch first"
        );
        let targets = interrupted
            .iter()
            .map(|request| request["params"].clone())
            .collect::<Vec<_>>();
        assert!(targets.contains(&json!({"threadId":"alpha-child","turnId":"alpha-child-turn"})));
        assert!(targets.contains(&json!({"threadId":"alpha-late","turnId":"alpha-late-turn"})));
        assert!(
            !targets
                .iter()
                .any(|target| target["threadId"] == "provider-root")
        );
        assert!(!targets.iter().any(|target| target["threadId"] == "beta"));
    })
    .unwrap_or_else(|_| {
        panic!(
            "selected and late descendants are the only provider interrupts; capture={}",
            std::fs::read_to_string(&capture).unwrap_or_default()
        )
    });

    let (mut reconnect, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("reconnected WebSocket");
    let (reconnected_snapshot, operation) = timeout(Duration::from_secs(10), async {
        let mut request_id = 7_001_u64;
        loop {
            request_id += 1;
            let partial = tagged_rpc_request(
                &mut reconnect,
                &request_id.to_string(),
                "activity.getSnapshot",
                json!({"_tag":"thread","threadId":"targeted-thread"}),
            )
            .await
            .expect("reconnected Activity snapshot");
            if let Some(operation) = partial["control"]["operations"]
                .as_array()
                .and_then(|operations| {
                    operations.iter().find(|operation| {
                        operation["rootActorId"] == "codex:thread:alpha"
                            && operation["state"] == "partial"
                            && operation["residualCount"] == 1
                    })
                })
                .cloned()
            {
                break (partial, operation);
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("partial cancellation operation");
    assert!(
        reconnected_snapshot["actors"]
            .as_array()
            .is_some_and(|actors| actors.iter().any(|actor| {
                actor["id"] == "codex:thread:beta" && actor["status"] == "running"
            }))
    );
    assert!(
        reconnected_snapshot["actors"]
            .as_array()
            .is_some_and(|actors| actors.iter().any(|actor| {
                actor["id"] == "codex:thread:beta-followup"
                    && actor["parentActorId"] == "codex:thread:beta"
                    && actor["status"] == "running"
            })),
        "a unique post-cancellation beta descendant proves the sibling provider event was applied"
    );
    assert!(
        reconnected_snapshot["actors"]
            .as_array()
            .is_some_and(|actors| actors.iter().any(|actor| {
                actor["id"] == "codex:thread:root-followup" && actor["status"] == "running"
            })),
        "a post-cancellation root provider event remains observable after reconnect"
    );
    assert_eq!(operation["state"], "partial");
    assert_eq!(operation["residualCount"], 1);
    assert_eq!(operation["message"], "Some agents are still running.");
    tagged_rpc_request(
        &mut reconnect,
        "7002",
        "activity.retrySubtreeCancellation",
        json!({
            "scope":{"_tag":"thread","threadId":"targeted-thread"},
            "scopeId":"thread:targeted-thread",
            "rootActorId":"codex:thread:alpha",
            "expectedOperationRevision":operation["operationRevision"]
        }),
    )
    .await
    .expect("residual retry accepted");
    timeout(Duration::from_secs(10), async {
        loop {
            let captured = std::fs::read_to_string(&capture).unwrap_or_default();
            let interrupted = captured
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .filter(|request| request["method"] == "turn/interrupt")
                .collect::<Vec<_>>();
            if interrupted.len() == 4 {
                break interrupted;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .map(|interrupted| {
        assert_eq!(
            interrupted[3]["params"],
            json!({"threadId":"alpha-child","turnId":"alpha-child-turn"}),
            "retry must target only the original residual"
        );
    })
    .expect("residual retry provider request");

    let old_operation_revision = operation["operationRevision"].clone();
    tagged_rpc_request(
        &mut reconnect,
        "8001",
        "orchestration.dispatchCommand",
        json!({
            "type":"thread.runtime-mode.set","commandId":"targeted-replace-runtime",
            "threadId":"targeted-thread","runtimeMode":"approval-required","createdAt":NOW
        }),
    )
    .await
    .expect("unsupported live mode change replaces the runtime");
    timeout(Duration::from_secs(10), async {
        loop {
            let replacement_started = std::fs::read_to_string(&capture)
                .unwrap_or_default()
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .any(|request| request["method"] == "thread/resume");
            if replacement_started {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "replacement runtime generation starts; capture={}",
            std::fs::read_to_string(&capture).unwrap_or_default()
        )
    });
    let before_stale_retry = std::fs::read_to_string(&capture)
        .expect("captured requests")
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|request| request["method"] == "turn/interrupt")
        .count();
    assert!(
        tagged_rpc_request(
            &mut reconnect,
            "8201",
            "activity.retrySubtreeCancellation",
            json!({
                "scope":{"_tag":"thread","threadId":"targeted-thread"},
                "scopeId":"thread:targeted-thread",
                "rootActorId":"codex:thread:alpha",
                "expectedOperationRevision":old_operation_revision
            }),
        )
        .await
        .is_err(),
        "the replacement generation must reject the old operation fence"
    );
    let after_stale_retry = std::fs::read_to_string(&capture)
        .expect("captured requests")
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|request| request["method"] == "turn/interrupt")
        .count();
    assert_eq!(after_stale_retry, before_stale_retry);

    reconnect.close(None).await.expect("close reconnect");
    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    handle.join().await.expect("RPC server joins");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn targeted_activity_rpc_writes_only_the_selected_claude_stop_task_subtree() {
    let state = TempDir::new().expect("state");
    let workspace = state.path().join("workspace");
    std::fs::create_dir(&workspace).expect("workspace");
    let capture = workspace.join(".bibcode-claude-targeted-requests.ndjson");
    let settings_capture = workspace.join(".bibcode-claude-targeted-settings.json");
    let token_capture = workspace.join(".bibcode-claude-targeted-token");
    let session_capture = workspace.join(".bibcode-claude-targeted-session");
    let ready_capture = workspace.join(".bibcode-claude-targeted-ready");
    let executable = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/claude-provider/targeted-rpc.sh")
        .canonicalize()
        .expect("stable Claude targeted RPC fixture");
    let config = test_config(&state);
    hermetic_providers::write_hermetic_settings(
        &config.state_dir(),
        json!({
            "providerInstances": {
                "claude-targeted": {
                    "driver": "claudeAgent",
                    "enabled": true,
                    "config": { "binaryPath": executable }
                }
            }
        }),
    );
    let handle = ServerRuntime::start(config.clone())
        .await
        .expect("production RPC server");
    let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket");
    tagged_rpc_request(
        &mut socket,
        "9101",
        "orchestration.dispatchCommand",
        json!({
            "type":"project.create","commandId":"claude-targeted-project",
            "projectId":"claude-targeted-project","title":"Claude Targeted",
            "workspaceRoot":workspace,"defaultModelSelection":null,"createdAt":NOW
        }),
    )
    .await
    .expect("project created through public RPC");
    tagged_rpc_request(
        &mut socket,
        "9102",
        "orchestration.dispatchCommand",
        json!({
            "type":"thread.create","commandId":"claude-targeted-thread",
            "threadId":"claude-targeted-thread","projectId":"claude-targeted-project",
            "title":"Claude targeted thread",
            "modelSelection":{"instanceId":"claude-targeted","model":"claude-sonnet"},
            "runtimeMode":"full-access","interactionMode":"default","branch":null,
            "worktreePath":null,"createdAt":NOW
        }),
    )
    .await
    .expect("thread created through public RPC");
    tagged_rpc_request(
        &mut socket,
        "9103",
        "orchestration.dispatchCommand",
        json!({
            "type":"thread.turn.start","commandId":"claude-targeted-turn",
            "threadId":"claude-targeted-thread",
            "message":{"messageId":"claude-targeted-message","role":"user","text":"start","attachments":[]},
            "modelSelection":{"instanceId":"claude-targeted","model":"claude-sonnet"},
            "runtimeMode":"full-access","interactionMode":"default","createdAt":NOW
        }),
    )
    .await
    .expect("turn admitted");

    timeout(Duration::from_secs(10), async {
        while !ready_capture.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "Claude fixture did not reach launch-ready marker: ready={}, settings={:?}, token_bytes={}, session={:?}",
            ready_capture.exists(),
            std::fs::read_to_string(&settings_capture),
            std::fs::read(&token_capture).map_or(0, |token| token.len()),
            std::fs::read_to_string(&session_capture)
        )
    });
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(&settings_capture).expect("Claude hook settings"),
    )
    .expect("valid Claude hook settings");
    let hook_url = settings["hooks"]["SubagentStart"][0]["hooks"][0]["url"]
        .as_str()
        .expect("Claude hook URL")
        .to_owned();
    let token = std::fs::read_to_string(&token_capture).expect("Claude hook token");
    let session_id = std::fs::read_to_string(&session_capture).expect("Claude session ID");
    let client = reqwest::Client::new();
    for hook in [
        json!({
            "hook_event_name":"PostToolUse","session_id":session_id,
            "tool_name":"Agent","tool_use_id":"tool-agent-a",
            "tool_response":{"status":"async_launched","agentId":"agent-a"}
        }),
        json!({
            "hook_event_name":"SubagentStart","session_id":session_id,
            "agent_id":"agent-a","agent_type":"same-role",
            "description":"same description","prompt":"same prompt"
        }),
        json!({
            "hook_event_name":"PostToolUse","session_id":session_id,
            "tool_name":"Agent","tool_use_id":"tool-agent-b",
            "tool_response":{"status":"async_launched","agentId":"agent-b"}
        }),
        json!({
            "hook_event_name":"SubagentStart","session_id":session_id,
            "agent_id":"agent-b","agent_type":"same-role",
            "description":"same description","prompt":"same prompt"
        }),
        json!({
            "hook_event_name":"PreToolUse","session_id":session_id,
            "agent_id":"agent-a","tool_name":"Agent","tool_use_id":"tool-agent-child"
        }),
        json!({
            "hook_event_name":"SubagentStart","session_id":session_id,
            "agent_id":"agent-child","agent_type":"same-role"
        }),
        json!({
            "hook_event_name":"PostToolUse","session_id":session_id,
            "agent_id":"agent-a","tool_name":"Agent","tool_use_id":"tool-agent-child",
            "tool_response":{"status":"async_launched","agentId":"agent-child"}
        }),
    ] {
        assert_eq!(
            client
                .post(&hook_url)
                .header("Authorization", format!("Bearer {token}"))
                .json(&hook)
                .send()
                .await
                .expect("authenticated Claude hook")
                .status(),
            reqwest::StatusCode::NO_CONTENT
        );
    }

    let available = timeout(Duration::from_secs(10), async {
        let mut request_id = 9_200_u64;
        loop {
            request_id += 1;
            let Ok(snapshot) = tagged_rpc_request(
                &mut socket,
                &request_id.to_string(),
                "activity.getSnapshot",
                json!({"_tag":"thread","threadId":"claude-targeted-thread"}),
            )
            .await
            else {
                tokio::task::yield_now().await;
                continue;
            };
            let selected = snapshot["control"]["actors"]
                .as_array()
                .and_then(|controls| {
                    controls.iter().find(|control| {
                        control["actorId"] == "claude:agent:agent-a"
                            && control["state"] == "available"
                            && control["activeDescendantCount"] == 1
                    })
                })
                .cloned();
            let sibling_available =
                snapshot["control"]["actors"]
                    .as_array()
                    .is_some_and(|controls| {
                        controls.iter().any(|control| {
                            control["actorId"] == "claude:agent:agent-b"
                                && control["state"] == "available"
                        })
                    });
            let child_available =
                snapshot["control"]["actors"]
                    .as_array()
                    .is_some_and(|controls| {
                        controls.iter().any(|control| {
                            control["actorId"] == "claude:agent:agent-child"
                                && control["state"] == "available"
                        })
                    });
            if let Some(selected) = selected.filter(|_| child_available && sibling_available) {
                break (snapshot, selected);
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    let (_, selected_control) = match available {
        Ok(available) => available,
        Err(_) => {
            let diagnostic = tagged_rpc_request(
                &mut socket,
                "9299",
                "activity.getSnapshot",
                json!({"_tag":"thread","threadId":"claude-targeted-thread"}),
            )
            .await;
            panic!(
                "exact Claude controls become available; snapshot={diagnostic:?}; capture={}",
                std::fs::read_to_string(&capture).unwrap_or_default()
            );
        }
    };

    assert_eq!(
        client
            .post(&hook_url)
            .header("Authorization", format!("Bearer {token}"))
            .json(&json!({
                "hook_event_name":"SubagentStart","session_id":session_id,
                "agent_id":"agent-unmapped","agent_type":"same-role",
                "description":"same description","prompt":"same prompt"
            }))
            .send()
            .await
            .expect("authenticated unmapped Claude hook")
            .status(),
        reqwest::StatusCode::NO_CONTENT
    );
    let snapshot = timeout(Duration::from_secs(10), async {
        let mut request_id = 9_260_u64;
        loop {
            request_id += 1;
            let next = tagged_rpc_request(
                &mut socket,
                &request_id.to_string(),
                "activity.getSnapshot",
                json!({"_tag":"thread","threadId":"claude-targeted-thread"}),
            )
            .await
            .expect("snapshot after unmapped actor");
            let observable = next["actors"].as_array().is_some_and(|actors| {
                actors.iter().any(|actor| {
                    actor["id"] == "claude:agent:agent-unmapped" && actor["status"] == "running"
                })
            });
            let unsupported = next["control"]["actors"]
                .as_array()
                .is_some_and(|controls| {
                    controls.iter().any(|control| {
                        control["actorId"] == "claude:agent:agent-unmapped"
                            && control["state"] == "unsupported"
                    })
                });
            if observable && unsupported {
                break next;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("unmapped Claude actor becomes observably unsupported");

    assert!(
        snapshot["control"]["actors"]
            .as_array()
            .is_some_and(|controls| controls.iter().any(|control| {
                control["actorId"] == "claude:agent:agent-child" && control["state"] == "available"
            }))
    );
    assert!(
        snapshot["control"]["actors"]
            .as_array()
            .is_some_and(|controls| controls.iter().any(|control| {
                control["actorId"] == "claude:agent:agent-b" && control["state"] == "available"
            }))
    );
    assert!(
        snapshot["actors"]
            .as_array()
            .is_some_and(|actors| actors.iter().any(|actor| {
                actor["id"] == "claude:agent:agent-unmapped" && actor["status"] == "running"
            }))
    );
    assert!(
        snapshot["control"]["actors"]
            .as_array()
            .is_some_and(|controls| controls.iter().any(|control| {
                control["actorId"] == "claude:agent:agent-unmapped"
                    && control["state"] == "unsupported"
            }))
    );

    let (mut thread_stream, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("thread stream");
    stream_rpc_request(
        &mut thread_stream,
        "9501",
        "orchestration.subscribeThread",
        json!({"threadId":"claude-targeted-thread"}),
    )
    .await;
    assert!(matches!(
        stream_rpc_message(&mut thread_stream).await,
        ServerMessage::Chunk { ref values, .. } if values[0]["kind"] == "snapshot"
    ));
    ack_stream_rpc(&mut thread_stream, "9501").await;

    let _ = captured_json_request(&capture, |request| request["type"] == "user").await;
    let (before_unmapped_bytes, before_unmapped) =
        captured_complete_ndjson_with_bytes(&capture).await;
    let before_unmapped_stop_tasks = claude_stop_task_targets(&before_unmapped);
    let before_unmapped_root_interrupts = claude_root_interrupt_count(&before_unmapped);
    assert!(before_unmapped_stop_tasks.is_empty());
    assert_eq!(before_unmapped_root_interrupts, 0);
    let unmapped_error = tagged_rpc_request(
        &mut socket,
        "9301",
        "activity.cancelSubtree",
        json!({
            "scope":{"_tag":"thread","threadId":"claude-targeted-thread"},
            "scopeId":"thread:claude-targeted-thread",
            "actorId":"claude:agent:agent-unmapped",
            "expectedControlRevision":0
        }),
    )
    .await
    .expect_err("an unsupported actor must fail through the public RPC");
    assert_eq!(
        unmapped_error,
        json!([{
            "_tag":"Fail",
            "error":{
                "_tag":"ActivityError",
                "message":"The provider cancellation target is no longer available.",
                "reason":"targetUnavailable"
            }
        }])
    );
    let (after_unmapped_bytes, after_unmapped) =
        captured_complete_ndjson_with_bytes(&capture).await;
    assert_eq!(
        after_unmapped_bytes, before_unmapped_bytes,
        "an unsupported actor must leave the complete-line provider capture byte-for-byte unchanged"
    );
    assert_eq!(
        claude_stop_task_targets(&after_unmapped),
        before_unmapped_stop_tasks,
        "an unsupported actor must not write a targeted stop request"
    );
    assert_eq!(
        claude_root_interrupt_count(&after_unmapped),
        before_unmapped_root_interrupts,
        "an unsupported actor must not fall back to interrupting the root"
    );

    tagged_rpc_request(
        &mut socket,
        "9302",
        "activity.cancelSubtree",
        json!({
            "scope":{"_tag":"thread","threadId":"claude-targeted-thread"},
            "scopeId":"thread:claude-targeted-thread",
            "actorId":"claude:agent:agent-a",
            "expectedControlRevision":selected_control["controlRevision"]
        }),
    )
    .await
    .expect("selected Claude subtree cancellation accepted");

    let stop_tasks = timeout(Duration::from_secs(10), async {
        loop {
            let requests = captured_complete_ndjson(&capture).await;
            let stop_tasks = claude_stop_task_targets(&requests);
            if stop_tasks.len() == 2 {
                break (requests, stop_tasks);
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("exact selected Claude stop_task requests");
    let (requests, stop_tasks) = stop_tasks;
    assert_eq!(stop_tasks, ["task-a", "task-child"]);
    let targeted_requests = requests
        .get(after_unmapped.len()..)
        .expect("captured requests retain the pre-cancellation prefix");
    let stop_task_requests = targeted_requests
        .iter()
        .filter(|request| request["request"]["subtype"] == "stop_task")
        .collect::<Vec<_>>();
    assert_eq!(
        targeted_requests.len(),
        2,
        "selected cancellation must write only the exact bounded target requests: {targeted_requests:?}"
    );
    assert_exact_claude_stop_task_request(stop_task_requests[0], "task-a");
    assert_exact_claude_stop_task_request(stop_task_requests[1], "task-child");
    assert_eq!(
        claude_root_interrupt_count(&requests),
        0,
        "selected cancellation must never fall back to a root interrupt"
    );

    let post_cancel = timeout(Duration::from_secs(10), async {
        let mut request_id = 9_400_u64;
        loop {
            request_id += 1;
            let snapshot = tagged_rpc_request(
                &mut socket,
                &request_id.to_string(),
                "activity.getSnapshot",
                json!({"_tag":"thread","threadId":"claude-targeted-thread"}),
            )
            .await
            .expect("post-cancel Activity snapshot");
            let actors = snapshot["actors"].as_array().cloned().unwrap_or_default();
            if actors.iter().any(|actor| {
                actor["id"] == "claude:agent:agent-a" && actor["status"] == "cancelled"
            }) && actors.iter().any(|actor| {
                actor["id"] == "claude:agent:agent-child" && actor["status"] == "cancelled"
            }) {
                break snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("selected Claude actors become terminal");
    assert_eq!(
        claude_stop_task_targets(&captured_complete_ndjson(&capture).await),
        ["task-a", "task-child"],
        "provider notification must not trigger another targeted request"
    );
    assert!(post_cancel["actors"].as_array().is_some_and(|actors| {
        actors
            .iter()
            .any(|actor| actor["id"] == "claude:agent:agent-b" && actor["status"] == "running")
    }));
    assert!(
        post_cancel["actors"]
            .as_array()
            .is_some_and(|actors| actors.iter().any(|actor| {
                actor["id"] == "claude:agent:agent-unmapped" && actor["status"] == "running"
            }))
    );
    assert_eq!(post_cancel["observationState"], "live");

    timeout(Duration::from_secs(10), async {
        loop {
            let message = stream_rpc_message(&mut thread_stream).await;
            let root_live = matches!(
                message,
                ServerMessage::Chunk { ref values, .. }
                    if values[0]["kind"] == "snapshot"
                        && values[0]["snapshot"]["thread"]["messages"]
                            .as_array()
                            .is_some_and(|messages| messages.iter().any(|message| {
                                message["role"] == "assistant"
                                    && message["text"].as_str().is_some_and(|text| {
                                        text.contains("root-after-cancel")
                                    })
                            }))
            );
            ack_stream_rpc(&mut thread_stream, "9501").await;
            if root_live {
                break;
            }
        }
    })
    .await
    .expect("the root thread continues streaming after selected cancellation");

    thread_stream
        .close(None)
        .await
        .expect("close thread stream");
    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    handle.join().await.expect("RPC server joins");
}

fn ambiguous_claude_children_are_observable_and_unsupported(snapshot: &Value) -> bool {
    let actors = snapshot["actors"].as_array();
    let controls = snapshot["control"]["actors"].as_array();
    let both_running = ["agent-child-one", "agent-child-two"]
        .iter()
        .all(|agent_id| {
            actors.is_some_and(|actors| {
                actors.iter().any(|actor| {
                    actor["id"] == format!("claude:agent:{agent_id}")
                        && actor["status"] == "running"
                })
            })
        });
    let both_unsupported = ["agent-child-one", "agent-child-two"]
        .iter()
        .all(|agent_id| {
            controls.is_some_and(|controls| {
                controls.iter().any(|control| {
                    control["actorId"] == format!("claude:agent:{agent_id}")
                        && control["state"] == "unsupported"
                })
            })
        });
    let parent_available = controls.is_some_and(|controls| {
        controls.iter().any(|control| {
            control["actorId"] == "claude:agent:agent-parent" && control["state"] == "available"
        })
    });
    both_running && both_unsupported && parent_available
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn targeted_activity_rpc_keeps_ambiguous_claude_children_unsupported_without_provider_io() {
    let state = TempDir::new().expect("state");
    let workspace = state.path().join("workspace");
    std::fs::create_dir(&workspace).expect("workspace");
    let capture = workspace.join(".bibcode-claude-targeted-ambiguous-requests.ndjson");
    let settings_capture = workspace.join(".bibcode-claude-targeted-ambiguous-settings.json");
    let token_capture = workspace.join(".bibcode-claude-targeted-ambiguous-token");
    let session_capture = workspace.join(".bibcode-claude-targeted-ambiguous-session");
    let ready_capture = workspace.join(".bibcode-claude-targeted-ambiguous-ready");
    let executable = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/claude-provider/targeted-rpc-ambiguous.sh")
        .canonicalize()
        .expect("stable ambiguous Claude targeted RPC fixture");
    let config = test_config(&state);
    hermetic_providers::write_hermetic_settings(
        &config.state_dir(),
        json!({
            "providerInstances": {
                "claude-targeted-ambiguous": {
                    "driver": "claudeAgent",
                    "enabled": true,
                    "config": { "binaryPath": executable }
                }
            }
        }),
    );
    let handle = ServerRuntime::start(config.clone())
        .await
        .expect("production RPC server");
    let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket");
    tagged_rpc_request(
        &mut socket,
        "9601",
        "orchestration.dispatchCommand",
        json!({
            "type":"project.create","commandId":"claude-ambiguous-project",
            "projectId":"claude-ambiguous-project","title":"Claude Ambiguous",
            "workspaceRoot":workspace,"defaultModelSelection":null,"createdAt":NOW
        }),
    )
    .await
    .expect("project created through public RPC");
    tagged_rpc_request(
        &mut socket,
        "9602",
        "orchestration.dispatchCommand",
        json!({
            "type":"thread.create","commandId":"claude-ambiguous-thread",
            "threadId":"claude-ambiguous-thread","projectId":"claude-ambiguous-project",
            "title":"Claude ambiguous thread",
            "modelSelection":{"instanceId":"claude-targeted-ambiguous","model":"claude-sonnet"},
            "runtimeMode":"full-access","interactionMode":"default","branch":null,
            "worktreePath":null,"createdAt":NOW
        }),
    )
    .await
    .expect("thread created through public RPC");

    const CLAUDE_ACTIVITY_INTEGRATION_TIMEOUT: Duration = Duration::from_secs(30);
    let deadline = tokio::time::Instant::now() + CLAUDE_ACTIVITY_INTEGRATION_TIMEOUT;
    let activity_connection = timeout_at(
        deadline,
        connect_async(format!("ws://{}/ws", handle.local_addr())),
    )
    .await;
    let (mut activity_stream, _) = match activity_connection {
        Ok(Ok(connection)) => connection,
        Ok(Err(error)) => {
            let _ = socket.close(None).await;
            handle.shutdown();
            let join_result = handle.join().await;
            panic!(
                "Activity stream connection failed before provider launch: \
                 error={error}; server_join={join_result:?}"
            );
        }
        Err(_) => {
            let _ = socket.close(None).await;
            handle.shutdown();
            let join_result = handle.join().await;
            panic!(
                "Activity stream connection deadline elapsed before provider launch; \
                 server_join={join_result:?}"
            );
        }
    };
    let thread_connection = timeout_at(
        deadline,
        connect_async(format!("ws://{}/ws", handle.local_addr())),
    )
    .await;
    let (mut thread_stream, _) = match thread_connection {
        Ok(Ok(connection)) => connection,
        Ok(Err(error)) => {
            let _ = activity_stream.close(None).await;
            let _ = socket.close(None).await;
            handle.shutdown();
            let join_result = handle.join().await;
            panic!(
                "thread stream connection failed before provider launch: \
                 error={error}; server_join={join_result:?}"
            );
        }
        Err(_) => {
            let _ = activity_stream.close(None).await;
            let _ = socket.close(None).await;
            handle.shutdown();
            let join_result = handle.join().await;
            panic!(
                "thread stream connection deadline elapsed before provider launch; \
                 server_join={join_result:?}"
            );
        }
    };
    let mut last_snapshot = None::<Value>;
    let setup_result = timeout_at(deadline, async {
        try_stream_rpc_request(
            &mut thread_stream,
            "9680",
            "orchestration.subscribeThread",
            json!({"threadId":"claude-ambiguous-thread"}),
        )
        .await?;
        let initial_thread = stream_rpc_message_until(&mut thread_stream, deadline).await?;
        if !matches!(initial_thread, ServerMessage::Chunk { ref values, .. }
            if values[0]["kind"] == "snapshot")
        {
            return Err(format!(
                "initial thread message was not a snapshot: {initial_thread:?}"
            ));
        }
        thread_stream
            .send(Message::Text(
                json!({"_tag":"Ack","requestId":"9680"})
                    .to_string()
                    .into(),
            ))
            .await
            .map_err(|error| format!("failed to ACK initial thread snapshot: {error}"))?;
        try_tagged_rpc_request_until(
            &mut socket,
            "9603",
            "orchestration.dispatchCommand",
            json!({
                "type":"thread.turn.start","commandId":"claude-ambiguous-turn",
                "threadId":"claude-ambiguous-thread",
                "message":{"messageId":"claude-ambiguous-message","role":"user","text":"start","attachments":[]},
                "modelSelection":{"instanceId":"claude-targeted-ambiguous","model":"claude-sonnet"},
                "runtimeMode":"full-access","interactionMode":"default","createdAt":NOW
            }),
            deadline,
        )
        .await
        .map_err(|error| format!("ambiguous Claude turn transport failed: {error}"))?
        .map_err(|error| format!("ambiguous Claude turn admission failed: {error}"))?;
        loop {
            let message = stream_rpc_message_until(&mut thread_stream, deadline).await?;
            let ready = matches!(
                message,
                ServerMessage::Chunk { ref values, .. }
                    if values[0]["kind"] == "snapshot"
                        && matches!(
                            values[0]["snapshot"]["thread"]["session"]["status"].as_str(),
                            Some("ready" | "running")
                        )
            );
            thread_stream
                .send(Message::Text(
                    json!({"_tag":"Ack","requestId":"9680"})
                        .to_string()
                        .into(),
                ))
                .await
                .map_err(|error| format!("failed to ACK thread readiness snapshot: {error}"))?;
            if ready {
                break;
            }
        }
        try_stream_rpc_request(
            &mut activity_stream,
            "9690",
            "subscribeActivity",
            json!({"_tag":"thread","threadId":"claude-ambiguous-thread"}),
        )
        .await?;
        let initial = stream_rpc_message_until(&mut activity_stream, deadline).await?;
        if !matches!(initial, ServerMessage::Chunk { ref values, .. }
            if values[0]["kind"] == "snapshot")
        {
            return Err(format!(
                "initial Activity message was not a snapshot: {initial:?}"
            ));
        }
        activity_stream
            .send(Message::Text(
                json!({"_tag":"Ack","requestId":"9690"})
                    .to_string()
                    .into(),
            ))
            .await
            .map_err(|error| format!("failed to ACK initial Activity snapshot: {error}"))?;

        while !ready_capture.exists() {
            tokio::task::yield_now().await;
        }
        let settings_json = std::fs::read_to_string(&settings_capture)
            .map_err(|error| format!("failed to read Claude hook settings: {error}"))?;
        let settings: Value = serde_json::from_str(&settings_json)
            .map_err(|error| format!("invalid Claude hook settings: {error}"))?;
        let hook_url = settings["hooks"]["SubagentStart"][0]["hooks"][0]["url"]
            .as_str()
            .ok_or_else(|| "Claude hook URL was absent from generated settings".to_owned())?
            .to_owned();
        let token = std::fs::read_to_string(&token_capture)
            .map_err(|error| format!("failed to read Claude hook token: {error}"))?;
        let session_id = std::fs::read_to_string(&session_capture)
            .map_err(|error| format!("failed to read Claude session ID: {error}"))?;
        let client = reqwest::Client::new();
        for hook in [
            json!({
                "hook_event_name":"PostToolUse","session_id":session_id,
                "tool_name":"Agent","tool_use_id":"tool-agent-parent",
                "tool_response":{"status":"async_launched","agentId":"agent-parent"}
            }),
            json!({
                "hook_event_name":"SubagentStart","session_id":session_id,
                "agent_id":"agent-parent","agent_type":"same-role"
            }),
            json!({
                "hook_event_name":"PreToolUse","session_id":session_id,
                "agent_id":"agent-parent","tool_name":"Agent","tool_use_id":"tool-agent-child-one"
            }),
            json!({
                "hook_event_name":"PreToolUse","session_id":session_id,
                "agent_id":"agent-parent","tool_name":"Agent","tool_use_id":"tool-agent-child-two"
            }),
            json!({
                "hook_event_name":"SubagentStart","session_id":session_id,
                "agent_id":"agent-child-one","agent_type":"same-role"
            }),
            json!({
                "hook_event_name":"SubagentStart","session_id":session_id,
                "agent_id":"agent-child-two","agent_type":"same-role"
            }),
        ] {
            let response = client
                .post(&hook_url)
                .header("Authorization", format!("Bearer {token}"))
                .json(&hook)
                .send()
                .await
                .map_err(|error| format!("authenticated Claude hook request failed: {error}"))?;
            if response.status() != reqwest::StatusCode::NO_CONTENT {
                return Err(format!(
                    "authenticated Claude hook returned unexpected status: {}",
                    response.status()
                ));
            }
        }

        Ok::<(), String>(())
    })
    .await;
    let setup_result = setup_result
        .map_err(|_| "shared 30-second Claude Activity setup deadline elapsed".to_owned())
        .and_then(std::convert::identity);
    if let Err(error) = setup_result {
        let diagnostic_capture = std::fs::read_to_string(&capture).unwrap_or_default();
        let _ = activity_stream.close(None).await;
        let _ = thread_stream.close(None).await;
        let _ = socket.close(None).await;
        handle.shutdown();
        let join_result = handle.join().await;
        panic!(
            "ambiguous Claude Activity setup failed: {error}; ready={}; settings={:?}; \
             token_bytes={}; session={:?}; provider_capture={diagnostic_capture:?}; \
             server_join={join_result:?}",
            ready_capture.exists(),
            std::fs::read_to_string(&settings_capture),
            std::fs::read(&token_capture).map_or(0, |token| token.len()),
            std::fs::read_to_string(&session_capture)
        );
    }

    let mut request_id = 9_700_u64;
    let observation = timeout_at(deadline, async {
        loop {
            let message = stream_rpc_message_until(&mut activity_stream, deadline).await?;
            let ServerMessage::Chunk { .. } = message else {
                return Err(format!("unexpected Activity stream message: {message:?}"));
            };
            activity_stream
                .send(Message::Text(
                    json!({"_tag":"Ack","requestId":"9690"}).to_string().into(),
                ))
                .await
                .map_err(|error| format!("failed to ACK Activity stream: {error}"))?;

            request_id += 1;
            let snapshot = try_tagged_rpc_request_until(
                &mut socket,
                &request_id.to_string(),
                "activity.getSnapshot",
                json!({"_tag":"thread","threadId":"claude-ambiguous-thread"}),
                deadline,
            )
            .await
            .map_err(|error| format!("authoritative Activity snapshot transport failed: {error}"))?
            .map_err(|error| format!("authoritative Activity snapshot failed: {error}"))?;
            last_snapshot = Some(snapshot.clone());
            if ambiguous_claude_children_are_observable_and_unsupported(&snapshot) {
                break Ok::<Value, String>(snapshot);
            }
        }
    })
    .await
    .map_err(|_| "shared 30-second Claude Activity deadline elapsed".to_owned())
    .and_then(std::convert::identity);
    let snapshot = match observation {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let diagnostic_snapshot = last_snapshot.clone().unwrap_or(Value::Null);
            let diagnostic_capture = std::fs::read_to_string(&capture).unwrap_or_default();
            let _ = activity_stream.close(None).await;
            let _ = thread_stream.close(None).await;
            let _ = socket.close(None).await;
            handle.shutdown();
            let join_result = handle.join().await;
            panic!(
                "ambiguous Claude Activity observation failed: {error}; \
                 last_snapshot={diagnostic_snapshot}; provider_capture={diagnostic_capture:?}; \
                 server_join={join_result:?}"
            );
        }
    };

    let verification = timeout_at(deadline, async {
        loop {
            let content = std::fs::read_to_string(&capture).unwrap_or_default();
            let user_request_complete = content
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .any(|request| request["type"] == "user");
            if user_request_complete {
                break;
            }
            tokio::task::yield_now().await;
        }
        let before = loop {
            let bytes = std::fs::read(&capture).unwrap_or_default();
            if (bytes.is_empty() || bytes.ends_with(b"\n"))
                && let Ok(content) = std::str::from_utf8(&bytes)
                && let Some(values) = content
                    .lines()
                    .map(|line| serde_json::from_str::<Value>(line).ok())
                    .collect::<Option<Vec<_>>>()
            {
                break (bytes, values);
            }
            tokio::task::yield_now().await;
        };
        if !claude_stop_task_targets(&before.1).is_empty() {
            return Err("provider capture contained a stop_task before cancellation".to_owned());
        }
        if claude_root_interrupt_count(&before.1) != 0 {
            return Err("provider capture contained a root interrupt before cancellation".to_owned());
        }

        let expected_error = json!([{
            "_tag":"Fail",
            "error":{
                "_tag":"ActivityError",
                "message":"The provider cancellation target is no longer available.",
                "reason":"targetUnavailable"
            }
        }]);
        for (index, agent_id) in ["agent-child-one", "agent-child-two"].iter().enumerate() {
            let actor_id = format!("claude:agent:{agent_id}");
            let control = snapshot["control"]["actors"]
                .as_array()
                .and_then(|controls| {
                    controls
                        .iter()
                        .find(|control| control["actorId"] == actor_id)
                })
                .ok_or_else(|| format!("missing unsupported child control for {actor_id}"))?;
            let cancellation = try_tagged_rpc_request_until(
                &mut socket,
                &(9_800 + index).to_string(),
                "activity.cancelSubtree",
                json!({
                    "scope":{"_tag":"thread","threadId":"claude-ambiguous-thread"},
                    "scopeId":"thread:claude-ambiguous-thread",
                    "actorId":actor_id,
                    "expectedControlRevision":control["controlRevision"]
                }),
                deadline,
            )
            .await?;
            let error = match cancellation {
                Ok(value) => {
                    return Err(format!(
                        "ambiguous child cancellation unexpectedly succeeded for {actor_id}: {value}"
                    ));
                }
                Err(error) => error,
            };
            if error != expected_error {
                return Err(format!(
                    "ambiguous child cancellation returned the wrong error for {actor_id}: {error}"
                ));
            }
        }

        let after = loop {
            let bytes = std::fs::read(&capture).unwrap_or_default();
            if (bytes.is_empty() || bytes.ends_with(b"\n"))
                && let Ok(content) = std::str::from_utf8(&bytes)
                && let Some(values) = content
                    .lines()
                    .map(|line| serde_json::from_str::<Value>(line).ok())
                    .collect::<Option<Vec<_>>>()
            {
                break (bytes, values);
            }
            tokio::task::yield_now().await;
        };
        if after.0 != before.0 {
            return Err("ambiguous child cancellation added provider request bytes".to_owned());
        }
        if after.1 != before.1 {
            return Err("ambiguous child cancellation changed parsed provider requests".to_owned());
        }
        if !claude_stop_task_targets(&after.1).is_empty() {
            return Err("ambiguous child cancellation emitted a stop_task request".to_owned());
        }
        if claude_root_interrupt_count(&after.1) != 0 {
            return Err("ambiguous child cancellation emitted a root interrupt".to_owned());
        }
        Ok::<(), String>(())
    })
    .await
    .map_err(|_| "shared 30-second Claude Activity verification deadline elapsed".to_owned())
    .and_then(std::convert::identity);
    if let Err(error) = verification {
        let diagnostic_capture = std::fs::read_to_string(&capture).unwrap_or_default();
        let _ = activity_stream.close(None).await;
        let _ = thread_stream.close(None).await;
        let _ = socket.close(None).await;
        handle.shutdown();
        let join_result = handle.join().await;
        panic!(
            "ambiguous Claude Activity verification failed: {error}; \
             snapshot={snapshot}; provider_capture={diagnostic_capture:?}; \
             server_join={join_result:?}"
        );
    }

    let activity_close = activity_stream.close(None).await;
    let thread_close = thread_stream.close(None).await;
    let socket_close = socket.close(None).await;
    handle.shutdown();
    let join_result = handle.join().await;
    if activity_close.is_err()
        || thread_close.is_err()
        || socket_close.is_err()
        || join_result.is_err()
    {
        panic!(
            "ambiguous Claude Activity cleanup failed: activity_close={activity_close:?}; \
             thread_close={thread_close:?}; socket_close={socket_close:?}; \
             server_join={join_result:?}"
        );
    }
}

#[tokio::test]
async fn native_acp_drivers_support_session_turn_and_control_commands() {
    let temp = TempDir::new().unwrap();
    let executable = executable_fixture(
        &temp,
        "acp-fixture",
        r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*|*'"method":"authenticate"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id" ;;
    *'"method":"session/new"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"sessionId":"cursor-session","configOptions":[{"id":"model","category":"model"}],"modes":{"currentModeId":"ask","availableModes":[{"id":"ask","name":"Ask"},{"id":"code","name":"Agent"},{"id":"architect","name":"Plan"}]}}}\n' "$id" ;;
    *'"method":"session/create"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"sessionId":"grok-session","modes":{"currentModeId":"code","availableModes":[{"id":"code","name":"Agent"},{"id":"ask","name":"Ask"}]}}}\n' "$id" ;;
    *'"method":"session/set_config_option"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"configOptions":[]}}\n' "$id" ;;
    *'"method":"session/set_mode"'*|*'"method":"session/set_model"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id" ;;
    *'"method":"session/prompt"'*) sleep 0.1; printf '{"jsonrpc":"2.0","id":%s,"result":{"stopReason":"end_turn"}}\n' "$id" ;;
  esac
done
"#,
        WINDOWS_ACP_FIXTURE,
    );
    let factory = NativeProviderDriverFactory::new(temp.path().join("attachments"));

    let mut cursor_request = launch();
    cursor_request.provider = "cursor".to_owned();
    cursor_request.binary_path = executable.to_string_lossy().into_owned();
    cursor_request.cwd = temp.path().to_path_buf();
    cursor_request.model = Some("gpt-5.4".to_owned());
    cursor_request.runtime_mode = "approval-required".to_owned();
    let cursor = factory.create(cursor_request).await.unwrap();
    assert_eq!(
        cursor.start().await.unwrap().resume_cursor,
        Some(json!({"schemaVersion":1,"sessionId":"cursor-session"}))
    );
    let cursor_turn = cursor
        .send(
            "hello".to_owned(),
            vec![image_attachment(&temp)],
            "default".to_owned(),
        )
        .await
        .unwrap()
        .unwrap();
    cursor.interrupt(Some(cursor_turn)).await.unwrap();
    cursor.set_model("gpt-5.5".to_owned()).await.unwrap();
    cursor
        .set_mode("auto-accept-edits".to_owned())
        .await
        .unwrap();
    cursor
        .set_interaction_mode("plan".to_owned())
        .await
        .unwrap();
    assert!(matches!(
        cursor.rollback(1, "rewind-key".to_owned()).await,
        Err(ProviderRuntimeError::UnsupportedCapability { provider, .. }) if provider == "cursor"
    ));
    assert!(matches!(
        cursor
            .approve("unknown".to_owned(), "accept".to_owned())
            .await,
        Err(ProviderRuntimeError::Provider { provider, .. }) if provider == "cursor"
    ));
    assert!(matches!(
        cursor.answer("unknown".to_owned(), json!({})).await,
        Err(ProviderRuntimeError::Provider { provider, .. }) if provider == "cursor"
    ));
    cursor.shutdown().await.unwrap();

    let mut grok_request = launch();
    grok_request.provider = "grok".to_owned();
    grok_request.binary_path = executable.to_string_lossy().into_owned();
    grok_request.cwd = temp.path().to_path_buf();
    grok_request.runtime_mode = "approval-required".to_owned();
    let grok = factory.create(grok_request).await.unwrap();
    assert_eq!(
        grok.start().await.unwrap().resume_cursor,
        Some(json!({"schemaVersion":1,"sessionId":"grok-session"}))
    );
    let grok_turn = grok
        .send(
            "hello".to_owned(),
            vec![image_attachment(&temp)],
            "default".to_owned(),
        )
        .await
        .unwrap()
        .unwrap();
    grok.interrupt(Some(grok_turn)).await.unwrap();
    grok.interrupt(None).await.unwrap();
    grok.set_model("grok-build".to_owned()).await.unwrap();
    grok.set_mode("full-access".to_owned()).await.unwrap();
    grok.set_interaction_mode("default".to_owned())
        .await
        .unwrap();
    assert!(matches!(
        grok.rollback(1, "rewind-key".to_owned()).await,
        Err(ProviderRuntimeError::UnsupportedCapability { provider, .. }) if provider == "grok"
    ));
    assert!(matches!(
        grok.approve("unknown".to_owned(), "accept".to_owned()).await,
        Err(ProviderRuntimeError::Provider { provider, .. }) if provider == "grok"
    ));
    assert!(matches!(
        grok.answer("unknown".to_owned(), json!({})).await,
        Err(ProviderRuntimeError::Provider { provider, .. }) if provider == "grok"
    ));
    grok.shutdown().await.unwrap();
}

#[tokio::test]
async fn default_driver_steer_is_rejected() {
    // A driver without native steering must never fall through to send/start.
    let state = Arc::new(StdMutex::new(DriverState::default()));
    let driver = FakeDriver {
        provider: "cursor".into(),
        provider_instance_id: Some("cursor".into()),
        state: state.clone(),
        events: tokio::sync::Mutex::new(mpsc::channel(1).1),
    };
    assert_eq!(
        driver
            .steer("follow up".into(), vec![], "active".into(), "key".into())
            .await,
        ProviderDeliveryOutcome::Rejected {
            detail: "provider does not support steering".into()
        }
    );
    assert!(state.lock().unwrap().sends.is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn codex_steer_sends_turn_steer_with_expected_turn_id() {
    // Catch turn/start substitution, missing reconciliation identity/input, and active-ID resets.
    let temp = TempDir::new().unwrap();
    let capture = temp.path().join("requests.jsonl");
    let executable = executable_fixture(
        &temp,
        "codex-steer",
        r#"#!/bin/sh
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$BIBCODE_TEST_CAPTURE"
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*) printf '{"id":%s,"result":{"userAgent":"fixture"}}\n' "$id" ;;
    *'"method":"thread/start"'*) printf '{"id":%s,"result":{"thread":{"id":"provider-thread"}}}\n' "$id" ;;
    *'"method":"mcpServerStatus/list"'*) printf '{"id":%s,"result":{"data":[],"nextCursor":null}}\n' "$id" ;;
    *'"method":"turn/start"'*) printf '{"id":%s,"result":{"turn":{"id":"active"}}}\n' "$id" ;;
    *'"method":"turn/steer"'*)
      case "$line" in
        *'"expectedTurnId":"stale"'*) printf '{"id":%s,"error":{"code":-32600,"message":"expectedTurnId does not match the active turn"}}\n' "$id" ;;
        *) printf '{"id":%s,"result":{"turnId":"accepted-turn"}}\n' "$id" ;;
      esac ;;
    *'"method":"turn/interrupt"'*|*'"method":"shutdown"'*) printf '{"id":%s,"result":{}}\n' "$id" ;;
  esac
done
"#,
        "",
    );
    let mut request = launch();
    request.binary_path = executable.to_string_lossy().into_owned();
    request.cwd = temp.path().into();
    request.environment.insert(
        "BIBCODE_TEST_CAPTURE".into(),
        capture.to_string_lossy().into_owned(),
    );
    let driver = NativeProviderDriverFactory::new(temp.path().join("attachments"))
        .create(request)
        .await
        .unwrap();
    driver.start().await.unwrap();
    driver
        .send("start".into(), vec![], "default".into())
        .await
        .unwrap();
    let outcome = driver
        .steer(
            "follow up".into(),
            vec![image_attachment(&temp)],
            "active".into(),
            "steer-key".into(),
        )
        .await;
    let rejected = driver
        .steer("stale".into(), vec![], "stale".into(), "stale-key".into())
        .await;
    driver.interrupt(None).await.unwrap();
    driver.shutdown().await.unwrap();
    assert_eq!(
        outcome,
        ProviderDeliveryOutcome::Accepted {
            turn_id: Some("accepted-turn".into())
        }
    );
    assert!(
        matches!(rejected, ProviderDeliveryOutcome::Rejected { detail } if detail.contains("expectedTurnId"))
    );
    let requests: Vec<Value> = std::fs::read_to_string(capture)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let steer = requests
        .iter()
        .find(|value| value["method"] == "turn/steer")
        .unwrap();
    assert_eq!(
        steer["params"],
        json!({
            "threadId":"provider-thread", "expectedTurnId":"active", "clientUserMessageId":"steer-key",
            "input":[{"type":"text","text":"follow up"}, {"type":"image","url":"data:image/png;base64,aW1hZ2UgYnl0ZXM="}]
        })
    );
    let interrupt = requests
        .iter()
        .find(|value| value["method"] == "turn/interrupt")
        .unwrap();
    assert_eq!(interrupt["params"]["turnId"], "active");
    assert_eq!(
        requests
            .iter()
            .filter(|value| value["method"] == "turn/start")
            .count(),
        1
    );
}

#[cfg(unix)]
#[tokio::test]
async fn claude_steer_writes_user_line_without_turn_started() {
    // A tool-result user line cannot acknowledge steer or reset its running turn attribution.
    let temp = TempDir::new().unwrap();
    let capture = temp.path().join("requests.jsonl");
    let gate = temp.path().join("release-echo");
    let executable = executable_fixture(
        &temp,
        "claude-steer",
        r#"#!/bin/sh
case "$1" in --version|--help) exit 1 ;; esac
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$BIBCODE_TEST_CAPTURE"
  case "$line" in
    *'follow up'*)
      printf '%s\n' '{"type":"user","session_id":"claude-session","uuid":"result-1","parent_tool_use_id":null,"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tool-1","content":"done"}]}}'
      printf '%s\n' '{"type":"stream_event","session_id":"claude-session","uuid":"barrier","parent_tool_use_id":null,"event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"tool-result-seen"}}}'
      while [ ! -f "$BIBCODE_TEST_ECHO_GATE" ]; do sleep 0.01; done
      printf '%s\n' "$line"
      printf '%s\n' '{"type":"stream_event","session_id":"claude-session","uuid":"after-steer","parent_tool_use_id":null,"event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"after-steer"}}}'
      ;;
  esac
done
"#,
        "",
    );
    let mut request = launch();
    request.provider = "claudeAgent".into();
    request.binary_path = executable.to_string_lossy().into_owned();
    request.cwd = temp.path().into();
    request.resume_cursor = Some(json!({"sessionId":"claude-session"}));
    use_empty_claude_config(&mut request, &temp);
    request.environment.insert(
        "BIBCODE_TEST_CAPTURE".into(),
        capture.to_string_lossy().into_owned(),
    );
    request.environment.insert(
        "BIBCODE_TEST_ECHO_GATE".into(),
        gate.to_string_lossy().into_owned(),
    );
    let driver = NativeProviderDriverFactory::new(temp.path().join("attachments"))
        .create(request)
        .await
        .unwrap();
    driver.start().await.unwrap();
    let active = driver
        .send("start".into(), vec![], "default".into())
        .await
        .unwrap()
        .unwrap();
    let sender = driver.clone();
    let expected = active.clone();
    let mut steer = tokio::spawn(async move {
        sender
            .steer("follow up".into(), vec![], expected, "key".into())
            .await
    });
    let observed = timeout(Duration::from_secs(10), async {
        loop {
            let event = driver.next_event().await.unwrap();
            assert_ne!(event.event_type, "turn.started");
            if event.payload.to_string().contains("tool-result-seen") {
                assert_eq!(event.turn_id.as_deref(), Some(active.as_str()));
                break;
            }
        }
    })
    .await;
    let premature = timeout(Duration::from_millis(100), &mut steer).await;
    let was_pending = premature.is_err();
    std::fs::write(gate, "release").unwrap();
    let outcome = match premature {
        Ok(result) => result.unwrap(),
        Err(_) => timeout(Duration::from_secs(10), steer)
            .await
            .unwrap()
            .unwrap(),
    };
    let after = timeout(Duration::from_secs(10), async {
        loop {
            let event = driver.next_event().await.unwrap();
            assert_ne!(event.event_type, "turn.started");
            if event.payload.to_string().contains("after-steer") {
                break event;
            }
        }
    })
    .await;
    driver.shutdown().await.unwrap();
    observed.expect("tool-result barrier");
    assert!(was_pending, "tool_result must not acknowledge a user steer");
    assert_eq!(
        outcome,
        ProviderDeliveryOutcome::Accepted {
            turn_id: Some(active.clone())
        }
    );
    assert_eq!(after.unwrap().turn_id.as_deref(), Some(active.as_str()));
    let requests: Vec<Value> = std::fs::read_to_string(capture)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        requests[1],
        json!({"type":"user","session_id":"claude-session","message":{"role":"user","content":[{"type":"text","text":"follow up"}]},"parent_tool_use_id":null})
    );
}

fn attachment_upload_registry() -> bibcode_server::transfer::staging::UploadRegistry {
    bibcode_server::transfer::staging::UploadRegistry::new(
        std::env::temp_dir().join(format!("bibcode-test-uploads-{}", uuid::Uuid::new_v4())),
        bibcode_server::transfer::staging::UploadLimits::default(),
        std::sync::Arc::new(tokio::time::Instant::now),
    )
}
