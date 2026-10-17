use bibcode_server::orchestration::engine::{EngineOptions, TestHooks};
use bibcode_server::orchestration::{
    OrchestrationCommand, OrchestrationEngine, TurnDeliveryState, load_snapshot,
};
use serde_json::{Value, json};
#[path = "support/turn_queue.rs"]
mod turn_queue;
use turn_queue::{queue_engine, seed_queued_message};

const TIME: &str = "2026-07-10T10:01:00.000Z";
fn decode(value: Value) -> OrchestrationCommand {
    serde_json::from_value(value).unwrap()
}
async fn session(engine: &OrchestrationEngine, status: &str, id: &str) {
    engine.dispatch(decode(json!({"type":"thread.session.set", "commandId":id, "threadId":"t1", "session":{"threadId":"t1", "status":status, "providerName":"codex", "activeTurnId":if status == "running" {Some("active")} else {None}, "lastError":null, "updatedAt":TIME}, "createdAt":TIME}))).await.unwrap();
}
async fn unhold_fixture(engine: &OrchestrationEngine) {
    engine
        .repositories()
        .database()
        .call(|connection| {
            connection.execute("UPDATE provider_turn_outbox SET held = 0", [])?;
            connection.execute(
                "UPDATE projection_thread_messages SET delivery_held = 0",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}
fn promote(id: &str, message: &str) -> OrchestrationCommand {
    decode(
        json!({"type":"thread.turn.promote", "commandId":id, "threadId":"t1", "messageId":message, "createdAt":TIME}),
    )
}

#[tokio::test]
async fn promotion_retimes_the_message_after_the_previous_reply_and_replays() {
    const REPLY_TIME: &str = "2026-07-10T10:00:30.000Z";
    let engine = queue_engine(TestHooks::default()).await;
    for id in ["head", "second", "third"] {
        seed_queued_message(&engine, id).await;
    }
    engine.dispatch(decode(json!({"type":"thread.message.assistant.delta", "commandId":"previous-reply", "threadId":"t1", "messageId":"assistant", "delta":"SECOND SEEN.", "turnId":"previous", "createdAt":REPLY_TIME}))).await.unwrap();
    engine.dispatch(decode(json!({"type":"thread.message.assistant.complete", "commandId":"previous-reply-complete", "threadId":"t1", "messageId":"assistant", "turnId":"previous", "createdAt":REPLY_TIME}))).await.unwrap();
    session(&engine, "ready", "settled").await;
    let before = load_snapshot(&engine.repositories()).await.unwrap();
    let tail_times: Vec<_> = before
        .messages
        .iter()
        .filter(|message| matches!(message.message_id.as_str(), "second" | "third"))
        .map(|message| {
            (
                message.message_id.clone(),
                message.created_at.clone(),
                message.updated_at.clone(),
            )
        })
        .collect();
    engine
        .dispatch(promote("promote-head", "head"))
        .await
        .unwrap();
    let events = engine.read_events(0).await.unwrap();
    let requested = events
        .iter()
        .find(|event| event.event.event_type == "thread.turn-start-requested")
        .unwrap();
    assert_eq!(requested.event.payload["createdAt"], TIME);
    let snapshot = load_snapshot(&engine.repositories()).await.unwrap();
    let head = snapshot
        .messages
        .iter()
        .find(|message| message.message_id == "head")
        .unwrap();
    assert_eq!(head.created_at, TIME);
    assert_eq!(head.updated_at, TIME);
    assert_eq!(
        snapshot
            .messages
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<Vec<_>>(),
        ["assistant", "head", "second", "third"]
    );
    assert_eq!(
        snapshot
            .messages
            .iter()
            .filter(|message| matches!(message.message_id.as_str(), "second" | "third"))
            .map(|message| (
                message.message_id.clone(),
                message.created_at.clone(),
                message.updated_at.clone()
            ))
            .collect::<Vec<_>>(),
        tail_times
    );
    assert_eq!(
        engine
            .repositories()
            .get_provider_turn_delivery("head".into())
            .await
            .unwrap()
            .unwrap()
            .created_at,
        "2026-07-10T10:00:00.000Z"
    );

    let database = engine.repositories().database().clone();
    engine.shutdown().await;
    database
        .call(|connection| {
            connection.execute("DELETE FROM projection_thread_messages", [])?;
            connection.execute(
                "DELETE FROM projection_state WHERE projector = 'projection.thread-messages'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let engine = OrchestrationEngine::start(database, EngineOptions::default())
        .await
        .unwrap();
    let replayed = load_snapshot(&engine.repositories()).await.unwrap();
    assert_eq!(
        replayed
            .messages
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<Vec<_>>(),
        ["assistant", "head", "second", "third"]
    );
    let head = replayed
        .messages
        .iter()
        .find(|message| message.message_id == "head")
        .unwrap();
    assert_eq!(head.created_at, TIME);
    assert_eq!(head.updated_at, TIME);
    engine.shutdown().await;
}

#[tokio::test]
async fn interrupt_holds_queued_rows_and_ready_does_not_promote_held() {
    let engine = queue_engine(TestHooks::default()).await;
    for id in ["head", "tail"] {
        seed_queued_message(&engine, id).await;
    }
    unhold_fixture(&engine).await;
    session(&engine, "running", "running").await;
    let before = engine
        .read_events(0)
        .await
        .unwrap()
        .last()
        .unwrap()
        .sequence;
    engine.dispatch(decode(json!({"type":"thread.turn.interrupt", "commandId":"interrupt", "threadId":"t1", "createdAt":TIME}))).await.unwrap();
    let events = engine.read_events(before).await.unwrap();
    assert_eq!(events.len(), 3);
    for id in ["head", "tail"] {
        let row = engine
            .repositories()
            .get_provider_turn_delivery(id.into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, TurnDeliveryState::Queued);
        assert!(row.held);
        let event = events
            .iter()
            .find(|event| event.event.payload["messageId"] == id)
            .unwrap();
        assert_eq!(event.event.payload["held"], true);
        assert_eq!(
            event.event.payload["delivery"],
            json!({"state":"queued", "provider":"codex", "providerInstanceId":"codex", "mode":"start", "held":true})
        );
    }
    session(&engine, "ready", "ready").await;
    assert!(
        engine
            .dispatch(promote("server:held-promote", "head"))
            .await
            .is_err()
    );
    engine.dispatch(promote("send-now", "head")).await.unwrap();
    assert!(
        !engine
            .repositories()
            .get_provider_turn_delivery("head".into())
            .await
            .unwrap()
            .unwrap()
            .held
    );
    assert!(
        engine
            .repositories()
            .get_provider_turn_delivery("tail".into())
            .await
            .unwrap()
            .unwrap()
            .held
    );
    engine.shutdown().await;
}

#[tokio::test]
async fn error_settle_holds_queued_rows() {
    let engine = queue_engine(TestHooks::default()).await;
    seed_queued_message(&engine, "head").await;
    unhold_fixture(&engine).await;
    session(&engine, "error", "error").await;
    let row = engine
        .repositories()
        .get_provider_turn_delivery("head".into())
        .await
        .unwrap()
        .unwrap();
    assert!(row.held);
    assert_eq!(row.state, TurnDeliveryState::Queued);
    session(&engine, "ready", "ready").await;
    assert!(
        engine
            .dispatch(promote("server:error-promote", "head"))
            .await
            .is_err()
    );
    engine.shutdown().await;
}

#[tokio::test]
async fn promote_requires_head_and_rejects_running_or_starting_sessions() {
    let engine = queue_engine(TestHooks::default()).await;
    for id in ["head", "tail"] {
        seed_queued_message(&engine, id).await;
    }
    assert!(
        engine
            .dispatch(promote("tail-promote", "tail"))
            .await
            .is_err()
    );
    for status in ["running", "starting"] {
        session(&engine, status, status).await;
        assert!(
            engine
                .dispatch(promote(&format!("{status}-promote"), "head"))
                .await
                .is_err()
        );
    }
    for status in ["idle", "stopped", "interrupted", "error"] {
        session(&engine, status, status).await;
        assert!(
            engine
                .dispatch(promote(&format!("server:{status}-promote"), "head"))
                .await
                .is_err()
        );
    }
    engine.shutdown().await;
}

#[tokio::test]
async fn queue_mutations_roll_back_outbox_events_and_projection_together() {
    for action in ["promote", "cancel", "interrupt", "error"] {
        let hooks = TestHooks::default();
        let engine = queue_engine(hooks.clone()).await;
        seed_queued_message(&engine, "head").await;
        unhold_fixture(&engine).await;
        let before = engine.read_events(0).await.unwrap().len();
        hooks.fail_next_projector(
            "projection.thread-messages",
            Some("thread.turn-delivery-updated"),
        );
        let command = match action {
            "promote" => promote("mutation", "head"),
            "cancel" => decode(
                json!({"type":"thread.turn-delivery.resolve", "commandId":"mutation", "threadId":"t1", "messageId":"head", "action":"cancel", "createdAt":TIME}),
            ),
            "interrupt" => decode(
                json!({"type":"thread.turn.interrupt", "commandId":"mutation", "threadId":"t1", "createdAt":TIME}),
            ),
            _ => decode(
                json!({"type":"thread.session.set", "commandId":"mutation", "threadId":"t1", "session":{"threadId":"t1", "status":"error", "providerName":"codex", "activeTurnId":null, "lastError":null, "updatedAt":TIME}, "createdAt":TIME}),
            ),
        };
        assert!(
            engine.dispatch(command).await.is_err(),
            "{action} must reach the delivery projector"
        );
        assert_eq!(engine.read_events(0).await.unwrap().len(), before);
        let row = engine
            .repositories()
            .get_provider_turn_delivery("head".into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, TurnDeliveryState::Queued);
        assert!(!row.held);
        let message = engine
            .repositories()
            .get_message("head".into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message.created_at, "2026-07-10T10:00:00.000Z");
        assert_eq!(message.updated_at, "2026-07-10T10:00:00.000Z");
        engine.shutdown().await;
    }
}

#[tokio::test]
async fn hold_and_withdrawal_replay_without_resurrecting_messages() {
    let engine = queue_engine(TestHooks::default()).await;
    for id in ["head", "tail"] {
        seed_queued_message(&engine, id).await;
    }
    unhold_fixture(&engine).await;
    session(&engine, "error", "error").await;
    engine.dispatch(decode(json!({"type":"thread.turn-delivery.resolve", "commandId":"cancel", "threadId":"t1", "messageId":"head", "action":"cancel", "createdAt":TIME}))).await.unwrap();
    let database = engine.repositories().database().clone();
    engine.shutdown().await;
    database
        .call(|connection| {
            connection.execute("DELETE FROM projection_thread_messages", [])?;
            connection.execute(
                "DELETE FROM projection_state WHERE projector = 'projection.thread-messages'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let engine = OrchestrationEngine::start(database, EngineOptions::default())
        .await
        .unwrap();
    let snapshot = load_snapshot(&engine.repositories()).await.unwrap();
    assert_eq!(snapshot.messages.len(), 1);
    assert_eq!(snapshot.messages[0].message_id, "tail");
    assert_eq!(snapshot.messages[0].delivery_held, Some(true));
    assert_eq!(snapshot.messages[0].delivery_mode.as_deref(), Some("start"));
    engine.shutdown().await;
}

#[tokio::test]
async fn interrupted_settle_holds_the_queue_through_a_later_ready() {
    let engine = queue_engine(TestHooks::default()).await;
    seed_queued_message(&engine, "head").await;
    unhold_fixture(&engine).await;
    session(&engine, "interrupted", "provider-interrupted").await;
    session(&engine, "ready", "provider-ready").await;
    assert!(
        engine
            .repositories()
            .get_provider_turn_delivery("head".into())
            .await
            .unwrap()
            .unwrap()
            .held
    );
    assert!(
        engine
            .dispatch(promote("server:after-interruption", "head"))
            .await
            .is_err()
    );
    engine.shutdown().await;
}

#[tokio::test]
async fn queue_heads_follow_admission_order_with_tied_or_skewed_client_timestamps() {
    for skew in [false, true] {
        let engine = queue_engine(TestHooks::default()).await;
        seed_queued_message(&engine, "z-first").await;
        seed_queued_message(&engine, "a-second").await;
        if skew {
            engine.repositories().database().call(|connection| {
                connection.execute("UPDATE provider_turn_outbox SET created_at = '2025-01-01T00:00:00Z' WHERE command_id = 'a-second'", [])?;
                Ok(())
            }).await.unwrap();
        }
        let heads = engine
            .repositories()
            .list_queued_provider_turn_heads()
            .await
            .unwrap();
        assert_eq!(heads.len(), 1);
        assert_eq!(heads[0].command_id, "z-first");
        engine
            .dispatch(promote("promote-first", "z-first"))
            .await
            .unwrap();
        engine.shutdown().await;
    }
}

#[tokio::test]
async fn dismissing_an_ambiguous_head_does_not_auto_start_the_tail() {
    for state in ["sending", "uncertain"] {
        let engine = queue_engine(TestHooks::default()).await;
        seed_queued_message(&engine, "head").await;
        engine
            .dispatch(promote("start-head", "head"))
            .await
            .unwrap();
        seed_queued_message(&engine, "tail").await;
        unhold_fixture(&engine).await;
        session(&engine, "ready", "ready").await;
        engine
            .repositories()
            .claim_provider_turn("head".into(), TIME.into())
            .await
            .unwrap()
            .unwrap();
        let state = state.to_owned();
        engine
            .repositories()
            .database()
            .call(move |connection| {
                connection.execute(
                    "UPDATE provider_turn_outbox SET state = ? WHERE command_id = 'head'",
                    [state],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        engine.dispatch(decode(json!({"type":"thread.turn-delivery.resolve", "commandId":"dismiss-head", "threadId":"t1", "messageId":"head", "action":"dismiss", "createdAt":TIME}))).await.unwrap();
        assert!(
            !engine
                .repositories()
                .can_promote_queued_provider_turn("t1".into(), "tail".into(), true)
                .await
                .unwrap(),
            "dismissal does not prove an in-flight provider accepted nothing"
        );
        assert!(
            engine
                .dispatch(promote("server:tail", "tail"))
                .await
                .is_err()
        );
        engine.shutdown().await;
    }
}

use bibcode_server::activity::{ActivityCapabilities, ActivityProjection, ActivityRepository};
use bibcode_server::production::provider_runtime::{
    BoxRuntimeFuture, ProviderDeliveryOutcome, ProviderDriver, ProviderDriverFactory,
    ProviderEvent, ProviderLaunchRequest, ProviderRuntimeError, ProviderRuntimeSupervisor,
    StartedSession, SupervisorOptions, deliver_orchestration_turn, freeze_delivery_route,
};
use bibcode_server::production::turn_delivery::TurnDeliveryService;
use std::sync::{
    Arc, Mutex as StdMutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;
use tempfile::TempDir;

#[derive(Default)]
struct SteerDriver {
    calls: StdMutex<Vec<(String, String, String)>>,
    reject: AtomicBool,
    creates: AtomicUsize,
    steer_entered: tokio::sync::Notify,
    steer_release: StdMutex<Option<Arc<tokio::sync::Semaphore>>>,
}
struct SteerFactory(Arc<SteerDriver>);
impl ProviderDriverFactory for SteerFactory {
    fn create(
        &self,
        _: ProviderLaunchRequest,
    ) -> BoxRuntimeFuture<'_, Result<Arc<dyn ProviderDriver>, ProviderRuntimeError>> {
        self.0.creates.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(self.0.clone() as Arc<dyn ProviderDriver>) })
    }
}
impl ProviderDriver for SteerDriver {
    fn start(&self) -> BoxRuntimeFuture<'_, Result<StartedSession, ProviderRuntimeError>> {
        Box::pin(async {
            Ok(StartedSession {
                resume_cursor: Some(json!({"threadId":"provider-session"})),
                runtime_payload: None,
                activity_capabilities: ActivityCapabilities::none(),
            })
        })
    }
    fn send(
        &self,
        _: String,
        _: Vec<Value>,
        _: String,
    ) -> BoxRuntimeFuture<'_, Result<Option<String>, ProviderRuntimeError>> {
        Box::pin(async { panic!("durable delivery must choose deliver or steer") })
    }
    fn deliver(
        &self,
        text: String,
        _: Vec<Value>,
        _: String,
        key: String,
    ) -> BoxRuntimeFuture<'_, ProviderDeliveryOutcome> {
        self.calls.lock().unwrap().push(("start".into(), text, key));
        Box::pin(async {
            ProviderDeliveryOutcome::Accepted {
                turn_id: Some("active".into()),
            }
        })
    }
    fn steer(
        &self,
        text: String,
        _: Vec<Value>,
        expected: String,
        key: String,
    ) -> BoxRuntimeFuture<'_, ProviderDeliveryOutcome> {
        self.calls
            .lock()
            .unwrap()
            .push(("steer".into(), expected.clone(), key));
        assert_eq!(text, "saved prompt");
        let release = self.steer_release.lock().unwrap().clone();
        self.steer_entered.notify_one();
        Box::pin(async move {
            if let Some(release) = release {
                release.acquire().await.unwrap().forget();
            }
            if self.reject.load(Ordering::SeqCst) {
                ProviderDeliveryOutcome::Rejected {
                    detail: "expected turn is no longer active".into(),
                }
            } else {
                ProviderDeliveryOutcome::Accepted {
                    turn_id: Some(expected),
                }
            }
        })
    }
    fn interrupt(
        &self,
        _: Option<String>,
    ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
        Box::pin(async { Ok(()) })
    }
    fn approve(
        &self,
        _: String,
        _: String,
    ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
        Box::pin(async { Ok(()) })
    }
    fn answer(
        &self,
        _: String,
        _: Value,
    ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
        Box::pin(async { Ok(()) })
    }
    fn set_mode(&self, _: String) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
        Box::pin(async { Ok(()) })
    }
    fn set_model(&self, _: String) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
        Box::pin(async { Ok(()) })
    }
    fn set_options(&self, _: Vec<Value>) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
        Box::pin(async { Ok(()) })
    }
    fn rollback(
        &self,
        _: i64,
        _: String,
    ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
        Box::pin(async { Ok(()) })
    }
    fn next_event(&self) -> BoxRuntimeFuture<'_, Option<ProviderEvent>> {
        Box::pin(std::future::pending())
    }
    fn shutdown(&self) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
        Box::pin(async { Ok(()) })
    }
}

/// Refuses every launch as an incompatible provider version while `incompatible` is set, as the
/// OpenCode driver does for a version BiBCode cannot drive, and launches `driver` otherwise.
struct VersionGateFactory {
    driver: Arc<SteerDriver>,
    incompatible: AtomicBool,
    creates: AtomicUsize,
}
const UNSUPPORTED_VERSION: &str = "OpenCode 3.0.0 isn't supported yet. In Settings → Providers, set Binary path to an OpenCode 1.x executable.";
impl ProviderDriverFactory for VersionGateFactory {
    fn create(
        &self,
        request: ProviderLaunchRequest,
    ) -> BoxRuntimeFuture<'_, Result<Arc<dyn ProviderDriver>, ProviderRuntimeError>> {
        self.creates.fetch_add(1, Ordering::SeqCst);
        let incompatible = self.incompatible.load(Ordering::SeqCst);
        Box::pin(async move {
            if incompatible {
                return Err(ProviderRuntimeError::IncompatibleProvider {
                    provider: request.provider,
                    detail: UNSUPPORTED_VERSION.to_owned(),
                });
            }
            Ok(self.driver.clone() as Arc<dyn ProviderDriver>)
        })
    }
}

async fn settled_row(
    engine: &OrchestrationEngine,
    id: &str,
) -> bibcode_server::orchestration::ProviderTurnDelivery {
    let settled = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let row = engine
                .repositories()
                .get_provider_turn_delivery(id.into())
                .await
                .unwrap()
                .unwrap();
            if !matches!(
                row.state,
                TurnDeliveryState::Pending | TurnDeliveryState::Sending | TurnDeliveryState::Queued
            ) {
                break row;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    match settled {
        Ok(row) => row,
        Err(_) => {
            let row = engine
                .repositories()
                .get_provider_turn_delivery(id.into())
                .await
                .unwrap();
            panic!("delivery {id} did not settle: {row:?}")
        }
    }
}

async fn seed_frozen_message(engine: &OrchestrationEngine, settings: &TempDir, id: &str) {
    let mut payload = seed_queued_message(engine, id).await;
    let command = decode(payload.clone());
    freeze_delivery_route(
        engine,
        &settings.path().to_path_buf(),
        &command,
        &mut payload,
    )
    .await
    .unwrap();
    let id = id.to_owned();
    engine
        .repositories()
        .database()
        .call(move |connection| {
            connection.execute(
                "UPDATE provider_turn_outbox SET payload_json = ? WHERE command_id = ?",
                [payload.to_string(), id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

/// A launch refused for an unsupported provider version fails its delivery once, visibly, and
/// does not relaunch the provider in a retry loop. Once the provider is fixed, retrying the
/// message launches it normally.
#[tokio::test]
async fn an_incompatible_provider_fails_the_delivery_once_and_a_retry_after_the_fix_launches() {
    let engine = queue_engine(TestHooks::default()).await;
    let settings = TempDir::new().unwrap();
    let factory = Arc::new(VersionGateFactory {
        driver: Arc::new(SteerDriver::default()),
        incompatible: AtomicBool::new(true),
        creates: AtomicUsize::new(0),
    });
    let supervisor = Arc::new(ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory.clone(),
        ActivityProjection::new(ActivityRepository::new(
            engine.repositories().database().clone(),
        )),
        SupervisorOptions::default(),
    ));
    seed_frozen_message(&engine, &settings, "head").await;
    engine
        .dispatch(promote("start-head", "head"))
        .await
        .unwrap();
    unhold_fixture(&engine).await;
    let delivery = TurnDeliveryService::start(
        engine.clone(),
        supervisor.clone(),
        settings.path().to_path_buf(),
    );

    let failed = settled_row(&engine, "head").await;
    assert_eq!(failed.state, TurnDeliveryState::Failed);
    assert_eq!(failed.last_error.as_deref(), Some(UNSUPPORTED_VERSION));
    let message = engine
        .repositories()
        .get_message("head".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(message.delivery_state.as_deref(), Some("failed"));
    // The delivery loop's retry backoff is capped at one second; outlast it.
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    assert_eq!(
        factory.creates.load(Ordering::SeqCst),
        1,
        "an incompatible provider must be launched exactly once"
    );
    assert!(factory.driver.calls.lock().unwrap().is_empty());

    // A failed delivery holds its thread's later messages until the user resolves it. The user
    // points the provider at a supported version and retries: the provider launches normally.
    // Only the RPC layer can wake a running delivery service, so a fresh one claims the retry.
    delivery.shutdown().await;
    factory.incompatible.store(false, Ordering::SeqCst);
    engine.dispatch(decode(json!({"type":"thread.turn-delivery.resolve", "commandId":"retry-head", "threadId":"t1", "messageId":"head", "action":"retry", "createdAt":TIME}))).await.unwrap();
    let delivery = TurnDeliveryService::start(
        engine.clone(),
        supervisor.clone(),
        settings.path().to_path_buf(),
    );
    let delivered = settled_row(&engine, "head").await;
    assert_eq!(delivered.state, TurnDeliveryState::Delivered);
    assert_eq!(factory.creates.load(Ordering::SeqCst), 2);
    assert_eq!(
        factory.driver.calls.lock().unwrap().clone(),
        vec![("start".into(), "saved prompt".into(), "head".into())]
    );
    delivery.shutdown().await;
    supervisor.shutdown().await.unwrap();
    engine.shutdown().await;
}

struct SteerFixture {
    engine: OrchestrationEngine,
    supervisor: Arc<ProviderRuntimeSupervisor>,
    driver: Arc<SteerDriver>,
    settings: TempDir,
}
impl SteerFixture {
    async fn new() -> Self {
        let engine = queue_engine(TestHooks::default()).await;
        let settings = TempDir::new().unwrap();
        let driver = Arc::new(SteerDriver::default());
        let supervisor = Arc::new(ProviderRuntimeSupervisor::start(
            engine.clone(),
            Arc::new(SteerFactory(driver.clone())),
            ActivityProjection::new(ActivityRepository::new(
                engine.repositories().database().clone(),
            )),
            SupervisorOptions::default(),
        ));
        let mut payload = seed_queued_message(&engine, "head").await;
        let command = decode(payload.clone());
        assert!(matches!(
            deliver_orchestration_turn(
                &supervisor,
                &engine,
                &settings.path().to_path_buf(),
                command.clone(),
                "initial".into()
            )
            .await,
            ProviderDeliveryOutcome::Accepted { .. }
        ));
        driver.calls.lock().unwrap().clear();
        freeze_delivery_route(
            &engine,
            &settings.path().to_path_buf(),
            &command,
            &mut payload,
        )
        .await
        .unwrap();
        engine
            .repositories()
            .database()
            .call(move |connection| {
                connection.execute(
                    "UPDATE provider_turn_outbox SET payload_json = ? WHERE command_id = 'head'",
                    [payload.to_string()],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        engine.dispatch(decode(json!({"type":"thread.turn.steer","commandId":"steer","threadId":"t1","messageId":"head","createdAt":TIME}))).await.unwrap();
        Self {
            engine,
            supervisor,
            driver,
            settings,
        }
    }
    fn delivery(&self) -> TurnDeliveryService {
        TurnDeliveryService::start(
            self.engine.clone(),
            self.supervisor.clone(),
            self.settings.path().to_path_buf(),
        )
    }
    async fn settled_delivery(&self) -> bibcode_server::orchestration::ProviderTurnDelivery {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let row = self
                    .engine
                    .repositories()
                    .get_provider_turn_delivery("head".into())
                    .await
                    .unwrap()
                    .unwrap();
                if !matches!(
                    row.state,
                    TurnDeliveryState::Pending | TurnDeliveryState::Sending
                ) {
                    break row;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("delivery settles")
    }
    async fn shutdown(self, delivery: TurnDeliveryService) {
        delivery.shutdown().await;
        self.supervisor.shutdown().await.unwrap();
        self.engine.shutdown().await;
    }
}

#[tokio::test]
async fn steer_row_is_delivered_through_driver_steer_and_attributed_to_the_turn() {
    let fixture = SteerFixture::new().await;
    let delivery = fixture.delivery();
    let row = fixture.settled_delivery().await;
    let calls = fixture.driver.calls.lock().unwrap().clone();
    let message = fixture
        .engine
        .repositories()
        .get_message("head".into())
        .await
        .unwrap()
        .unwrap();
    let events = fixture.engine.read_events(0).await.unwrap();
    let accepted = events
        .iter()
        .find(|event| {
            event.event.event_type == "thread.turn-delivery-updated"
                && event.event.payload["state"] == "delivered"
        })
        .unwrap();
    assert_eq!(row.state, TurnDeliveryState::Delivered);
    assert_eq!(
        calls,
        vec![("steer".into(), "active".into(), "head".into())]
    );
    assert_eq!(message.turn_id.as_deref(), Some("active"));
    assert_eq!(message.created_at, "2026-07-10T10:00:00.000Z");
    assert_eq!(accepted.event.payload["turnId"], "active");
    assert_eq!(
        accepted.event.payload["delivery"],
        json!({"state":"delivered","provider":"codex","providerInstanceId":"codex","mode":"steer","held":false})
    );
    assert_eq!(fixture.driver.creates.load(Ordering::SeqCst), 1);
    fixture.shutdown(delivery).await;
}

#[tokio::test]
async fn steer_claimed_after_settle_requeues_as_start() {
    let fixture = SteerFixture::new().await;
    session(&fixture.engine, "ready", "ready").await;
    let delivery = fixture.delivery();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if fixture.settled_delivery().await.state == TurnDeliveryState::Delivered {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let events = fixture.engine.read_events(0).await.unwrap();
    let queued = events
        .iter()
        .position(|event| {
            event.event.event_type == "thread.turn-delivery-updated"
                && event.event.payload["state"] == "queued"
                && event.event.payload["mode"] == "start"
        })
        .expect("steer is first requeued as start");
    let promoted = events
        .iter()
        .position(|event| event.event.event_type == "thread.turn-start-requested")
        .expect("normal promotion starts the next turn");
    assert!(queued < promoted);
    assert_eq!(
        fixture.driver.calls.lock().unwrap().as_slice(),
        &[("start".into(), "saved prompt".into(), "head".into())]
    );
    fixture.shutdown(delivery).await;
}

#[tokio::test]
async fn steer_rejected_by_provider_requeues() {
    let fixture = SteerFixture::new().await;
    fixture.driver.reject.store(true, Ordering::SeqCst);
    let delivery = fixture.delivery();
    let row = fixture.settled_delivery().await;
    assert_eq!(row.state, TurnDeliveryState::Queued);
    assert_eq!(
        row.mode,
        bibcode_server::orchestration::TurnDeliveryMode::Start
    );
    assert_eq!(row.attempts, 0);
    assert_eq!(fixture.driver.calls.lock().unwrap().len(), 1);
    let message = fixture
        .engine
        .repositories()
        .get_message("head".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(message.delivery_state.as_deref(), Some("queued"));
    assert_eq!(message.delivery_mode.as_deref(), Some("start"));
    assert_eq!(message.turn_id, None);
    fixture.shutdown(delivery).await;
}

#[tokio::test]
async fn steer_acceptance_after_settle_does_not_restore_running_session() {
    let fixture = SteerFixture::new().await;
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    *fixture.driver.steer_release.lock().unwrap() = Some(release.clone());
    let delivery = fixture.delivery();
    tokio::time::timeout(
        Duration::from_secs(5),
        fixture.driver.steer_entered.notified(),
    )
    .await
    .expect("steer enters native driver");
    session(&fixture.engine, "ready", "ready-during-steer").await;
    release.add_permits(1);
    assert_eq!(
        fixture.settled_delivery().await.state,
        TurnDeliveryState::Delivered
    );
    let session = fixture
        .engine
        .repositories()
        .get_thread_session("t1".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.status, "ready");
    assert_eq!(session.active_turn_id, None);
    assert_eq!(
        fixture
            .engine
            .repositories()
            .get_message("head".into())
            .await
            .unwrap()
            .unwrap()
            .turn_id
            .as_deref(),
        Some("active")
    );
    fixture.shutdown(delivery).await;
}

#[tokio::test]
async fn steer_target_changed_before_delivery_requeues_without_provider_io() {
    let fixture = SteerFixture::new().await;
    fixture.engine.dispatch(decode(json!({"type":"thread.session.set","commandId":"replacement-turn","threadId":"t1","createdAt":TIME,
        "session":{"threadId":"t1","status":"running","providerName":"codex","activeTurnId":"different","lastError":null,"updatedAt":TIME}}))).await.unwrap();
    let delivery = fixture.delivery();
    assert_eq!(
        fixture.settled_delivery().await.state,
        TurnDeliveryState::Queued
    );
    assert!(fixture.driver.calls.lock().unwrap().is_empty());
    fixture.shutdown(delivery).await;
}

#[tokio::test]
async fn steer_without_live_runtime_never_launches_a_replacement() {
    let fixture = SteerFixture::new().await;
    let identity = fixture
        .supervisor
        .capture_session_identity("t1")
        .await
        .unwrap()
        .unwrap();
    fixture
        .supervisor
        .stop_session_if_current(identity)
        .await
        .unwrap();
    session(&fixture.engine, "running", "stale-projected-running").await;
    let delivery = fixture.delivery();
    assert_eq!(
        fixture.settled_delivery().await.state,
        TurnDeliveryState::Queued
    );
    assert_eq!(fixture.driver.creates.load(Ordering::SeqCst), 1);
    assert!(fixture.driver.calls.lock().unwrap().is_empty());
    fixture.shutdown(delivery).await;
}

#[tokio::test]
async fn steer_rejected_after_interrupt_or_error_remains_held() {
    for trigger in ["interrupt", "error", "interrupted"] {
        let fixture = SteerFixture::new().await;
        fixture.driver.reject.store(true, Ordering::SeqCst);
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        *fixture.driver.steer_release.lock().unwrap() = Some(release.clone());
        let delivery = fixture.delivery();
        tokio::time::timeout(
            Duration::from_secs(5),
            fixture.driver.steer_entered.notified(),
        )
        .await
        .unwrap();
        if trigger == "interrupt" {
            fixture.engine.dispatch(decode(json!({"type":"thread.turn.interrupt","commandId":"interrupt","threadId":"t1","createdAt":TIME}))).await.unwrap();
        } else {
            session(&fixture.engine, trigger, trigger).await;
        }
        let held_during_steer = fixture
            .engine
            .repositories()
            .get_provider_turn_delivery("head".into())
            .await
            .unwrap()
            .unwrap()
            .held;
        session(&fixture.engine, "ready", "ready-after-interruption").await;
        release.add_permits(1);
        let row = fixture.settled_delivery().await;
        let message = fixture
            .engine
            .repositories()
            .get_message("head".into())
            .await
            .unwrap()
            .unwrap();
        fixture.shutdown(delivery).await;
        assert!(
            held_during_steer,
            "{trigger} must protect a rejected steer from auto-send"
        );
        assert_eq!(row.state, TurnDeliveryState::Queued);
        assert!(row.held);
        assert_eq!(message.delivery_held, Some(true));
    }
}

#[tokio::test]
async fn steer_held_before_native_dispatch_requeues_without_provider_io() {
    let fixture = SteerFixture::new().await;
    fixture.engine.dispatch(decode(json!({"type":"thread.turn.interrupt","commandId":"stop-before-claim","threadId":"t1","createdAt":TIME}))).await.unwrap();
    let delivery = fixture.delivery();
    let row = fixture.settled_delivery().await;
    let calls = fixture.driver.calls.lock().unwrap().clone();
    fixture.shutdown(delivery).await;
    assert_eq!(row.state, TurnDeliveryState::Queued);
    assert!(row.held);
    assert!(
        calls.is_empty(),
        "Stop must fence a steer that has not reached the provider"
    );
}
