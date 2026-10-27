use std::{sync::mpsc as std_mpsc, time::Duration};

use coddy_studio_server::{
    ACTIVE_RPC_METHODS, MethodMode, RpcRegistry, ServerConfig, ServerMessage, ServerRuntime,
    activity::{
        ActivityActorSummary, ActivityCapabilities, ActivityEntry, ActivityEntryKind,
        ActivityEntryTone, ActivityLifecycle, ActivityProjection, ActivityProjections,
        ActivityRecordKind, ActivityRepository, ActivityScopeSeed, ActivityWorkItemSummary,
        AgentActivityController, ProviderActivityMutation,
        register_activity_rpc_for_integration_test,
    },
    persistence::{Database, run_migrations},
};
use futures_util::SinkExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::time::timeout;
use tokio_tungstenite::{WebSocketStream, connect_async, tungstenite::Message};

#[path = "support/activity_time.rs"]
mod activity_time;
use activity_time::{activity_timestamp, recent_activity_anchor};
#[path = "support/websocket_frames.rs"]
mod websocket_frames;
use websocket_frames::next_frame_past_heartbeat;

#[tokio::test]
async fn activity_unary_rpc_pages_rosters_and_detail_and_bounds_scope_errors() {
    let timeline_start = recent_activity_anchor();
    let completed_at = activity_timestamp(timeline_start, time::Duration::seconds(1));
    let fixture = Fixture::start(16).await;
    let scope = thread_scope("thread:rpc", "rpc");
    fixture
        .chat_projection
        .ensure_scope(scope.clone())
        .await
        .expect("scope");
    fixture
        .chat_projection
        .apply(
            &scope.scope_id,
            "event:seed".to_owned(),
            vec![
                ProviderActivityMutation::UpsertActor(
                    actor(
                        "actor:active",
                        ActivityLifecycle::Running,
                        "2026-07-22T12:00:00Z",
                        None,
                    )
                    .expect("active actor"),
                ),
                ProviderActivityMutation::UpsertActor(
                    actor(
                        "actor:done",
                        ActivityLifecycle::Completed,
                        &completed_at,
                        Some(&completed_at),
                    )
                    .expect("done actor"),
                ),
                ProviderActivityMutation::UpsertWorkItem(
                    work_item("work:active", "actor:active").expect("active work item"),
                ),
                ProviderActivityMutation::AppendEntry(
                    entry("entry:older", "actor:active", "2026-07-22T12:00:02Z")
                        .expect("older entry"),
                ),
                ProviderActivityMutation::AppendEntry(
                    entry("entry:newer", "actor:active", "2026-07-22T12:00:03Z")
                        .expect("newer entry"),
                ),
            ],
            "2026-07-22T12:00:03Z".to_owned(),
        )
        .await
        .expect("seed mutation");

    let mut socket = fixture.connect().await;
    let snapshot = unary(
        &mut socket,
        "1",
        "activity.getSnapshot",
        json!({ "_tag": "thread", "threadId": "rpc" }),
    )
    .await
    .expect("snapshot");
    assert_eq!(snapshot["scopeId"], "thread:rpc");
    assert_eq!(snapshot["protocolVersion"], 2);
    assert_eq!(snapshot["control"]["scopeId"], "thread:rpc");
    assert_eq!(snapshot["control"]["actors"].as_array().unwrap().len(), 2);
    assert_eq!(snapshot["control"]["operations"], json!([]));
    assert_eq!(
        snapshot["counts"]["subagents"],
        json!({ "active": 1, "done": 1 })
    );

    let active = unary(
        &mut socket,
        "2",
        "activity.listRoster",
        json!({
            "scope": { "_tag": "thread", "threadId": "rpc" },
            "scopeId": "thread:rpc",
            "section": "subagents",
            "bucket": "active",
            "limit": 1
        }),
    )
    .await
    .expect("active roster");
    assert_eq!(active["records"][0]["id"], "actor:active");
    assert_eq!(active["actorControls"].as_array().unwrap().len(), 1);
    assert_eq!(active["actorControls"][0]["actorId"], "actor:active");
    assert_eq!(active["actorControls"][0]["state"], "unsupported");
    assert!(active["nextCursor"].is_null());

    let missing_scope = unary(
        &mut socket,
        "20",
        "activity.listRoster",
        json!({
            "scopeId": "thread:rpc",
            "section": "subagents",
            "bucket": "active",
            "limit": 1
        }),
    )
    .await
    .expect_err("activity v1 roster paging requires its root descriptor");
    assert_eq!(
        missing_scope,
        json!({
            "_tag": "ActivityError",
            "message": "The requested activity scope is invalid.",
            "reason": "invalidScope"
        })
    );

    let done = unary(
        &mut socket,
        "3",
        "activity.listRoster",
        json!({
            "scope": { "_tag": "thread", "threadId": "rpc" },
            "scopeId": "thread:rpc",
            "section": "subagents",
            "bucket": "done",
            "limit": 1
        }),
    )
    .await
    .expect("done roster");
    assert_eq!(done["records"][0]["id"], "actor:done");
    assert!(done["nextCursor"].is_null());

    let detail = unary(
        &mut socket,
        "4",
        "activity.listDetail",
        json!({
            "scope": { "_tag": "thread", "threadId": "rpc" },
            "scopeId": "thread:rpc",
            "recordKind": "actor",
            "recordId": "actor:active",
            "limit": 1
        }),
    )
    .await
    .expect("detail");
    assert_eq!(detail["entries"][0]["id"], "entry:newer");
    assert_eq!(detail["actorControl"]["actorId"], "actor:active");
    assert_eq!(detail["actorControl"]["state"], "unsupported");
    let cursor = detail["nextCursor"].as_str().expect("detail cursor");
    let next_detail = unary(
        &mut socket,
        "5",
        "activity.listDetail",
        json!({
            "scope": { "_tag": "thread", "threadId": "rpc" },
            "scopeId": "thread:rpc",
            "recordKind": "actor",
            "recordId": "actor:active",
            "cursor": cursor,
            "limit": 1
        }),
    )
    .await
    .expect("next detail");
    assert_eq!(next_detail["entries"][0]["id"], "entry:older");

    let work_detail = unary(
        &mut socket,
        "50",
        "activity.listDetail",
        json!({
            "scope": { "_tag": "thread", "threadId": "rpc" },
            "scopeId": "thread:rpc",
            "recordKind": "workItem",
            "recordId": "work:active",
            "limit": 1
        }),
    )
    .await
    .expect("work-item detail");
    assert!(work_detail["actorControl"].is_null());

    let invalid_cursor = unary(
        &mut socket,
        "6",
        "activity.listDetail",
        json!({
            "scope": { "_tag": "thread", "threadId": "rpc" },
            "scopeId": "thread:rpc",
            "recordKind": "actor",
            "recordId": "actor:active",
            "cursor": "not-a-cursor"
        }),
    )
    .await
    .expect_err("invalid cursor");
    assert_eq!(
        invalid_cursor,
        json!({
            "_tag": "ActivityError",
            "message": "The activity cursor is invalid.",
            "reason": "invalidCursor"
        })
    );

    socket.close(None).await.expect("close socket");
    fixture.shutdown().await;
}

#[tokio::test]
async fn cancellation_rpc_fails_closed_without_an_exact_runtime_target_or_thread_scope() {
    // Mutation caught: accepting a terminal scope or falling back to a root interruption when the
    // current provider runtime has not registered an exact private actor target.
    let fixture = Fixture::start(16).await;
    let scope = thread_scope("thread:cancellation-rpc", "cancellation-rpc");
    fixture
        .chat_projection
        .ensure_scope(scope.clone())
        .await
        .expect("scope");
    fixture
        .chat_projection
        .apply(
            &scope.scope_id,
            "event:cancellation-rpc".to_owned(),
            vec![ProviderActivityMutation::UpsertActor(
                actor(
                    "actor:cancellation-rpc",
                    ActivityLifecycle::Running,
                    "2026-08-11T20:01:00Z",
                    None,
                )
                .expect("actor"),
            )],
            "2026-08-11T20:01:00Z".to_owned(),
        )
        .await
        .expect("actor projection");
    let mut socket = fixture.connect().await;

    let unavailable = unary(
        &mut socket,
        "90",
        "activity.cancelSubtree",
        json!({
            "scope":{"_tag":"thread","threadId":"cancellation-rpc"},
            "scopeId":"thread:cancellation-rpc",
            "actorId":"actor:cancellation-rpc",
            "expectedControlRevision":0
        }),
    )
    .await
    .expect_err("actor without an exact native target remains read-only");
    assert_eq!(
        unavailable,
        json!({
            "_tag":"ActivityError",
            "message":"The activity scope has changed. Refresh and try again.",
            "reason":"staleScope"
        })
    );

    let terminal = unary(
        &mut socket,
        "91",
        "activity.cancelSubtree",
        json!({
            "scope":{
                "_tag":"terminal",
                "threadId":"cancellation-rpc",
                "terminalId":"terminal-1"
            },
            "scopeId":"terminal:cancellation-rpc",
            "actorId":"actor:cancellation-rpc",
            "expectedControlRevision":0
        }),
    )
    .await
    .expect_err("terminal Activity cancellation must fail at the typed RPC boundary");
    assert_eq!(
        terminal,
        json!({
            "_tag":"ActivityError",
            "message":"The requested activity scope is invalid.",
            "reason":"invalidScope"
        })
    );

    socket.close(None).await.expect("close socket");
    fixture.shutdown().await;
}

#[tokio::test]
async fn activity_stream_starts_with_snapshot_and_filters_deltas_to_exact_scope() {
    let fixture = Fixture::start(16).await;
    let first = thread_scope("thread:first", "first");
    let second = thread_scope("thread:second", "second");
    fixture
        .chat_projection
        .ensure_scope(first.clone())
        .await
        .expect("first scope");
    fixture
        .chat_projection
        .ensure_scope(second.clone())
        .await
        .expect("second scope");

    let mut socket = fixture.connect().await;
    request(
        &mut socket,
        "10",
        "subscribeActivity",
        json!({ "_tag": "thread", "threadId": "first" }),
    )
    .await;
    let snapshot = next_message(&mut socket).await;
    assert!(
        matches!(
            snapshot,
            ServerMessage::Chunk { ref values, .. }
                if values[0]["kind"] == "snapshot"
                    && values[0]["snapshot"]["scopeId"] == "thread:first"
        ),
        "unexpected first stream message: {snapshot:?}"
    );
    ack(&mut socket, "10").await;

    fixture
        .chat_projection
        .apply(
            &second.scope_id,
            "event:other".to_owned(),
            vec![
                ProviderActivityMutation::upsert_actor("actor:other", None, "Other", "running")
                    .expect("other actor"),
            ],
            "2026-07-22T12:00:00Z".to_owned(),
        )
        .await
        .expect("other delta");
    assert!(
        timeout(Duration::from_millis(100), next_message(&mut socket))
            .await
            .is_err(),
        "unrelated scope delta leaked into stream"
    );

    fixture
        .chat_projection
        .apply(
            &first.scope_id,
            "event:first".to_owned(),
            vec![
                ProviderActivityMutation::upsert_actor("actor:first", None, "First", "running")
                    .expect("first actor"),
            ],
            "2026-07-22T12:00:01Z".to_owned(),
        )
        .await
        .expect("first delta");
    let delta = next_message(&mut socket).await;
    assert!(
        matches!(
            delta,
            ServerMessage::Chunk { ref values, .. }
                if values[0]["kind"] == "delta"
                    && values[0]["delta"]["scopeId"] == "thread:first"
        ),
        "unexpected delta message: {delta:?}"
    );
    ack(&mut socket, "10").await;
    assert!(
        fixture
            .chat_projection
            .apply(
                &first.scope_id,
                "event:first".to_owned(),
                vec![
                    ProviderActivityMutation::remove_actor("actor:first")
                        .expect("valid duplicate payload"),
                ],
                "2026-07-22T12:00:02Z".to_owned(),
            )
            .await
            .expect("duplicate event")
            .is_empty()
    );
    assert!(
        timeout(Duration::from_millis(100), next_message(&mut socket))
            .await
            .is_err(),
        "duplicate/no-op repository result was broadcast"
    );

    socket.close(None).await.expect("close socket");
    fixture.shutdown().await;
}

#[tokio::test]
async fn feature_disabled_stream_emits_one_error_completes_and_unary_reads_reserve_no_database_job()
{
    // Mutation caught: continuing an admitted stream or reserving a database job after disablement.
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let controller = AgentActivityController::new(true);
    let projections = ActivityProjections::new(
        ActivityRepository::new(database.clone()),
        controller.clone(),
        AgentActivityController::new(true),
    );
    let projection = projections.chat();
    let scope = thread_scope("thread:feature-disabled", "feature-disabled");
    projection.ensure_scope(scope).await.expect("scope");
    let mut registry = RpcRegistry::empty();
    register_activity_rpc_for_integration_test(&mut registry, projections);
    let directory = tempfile::tempdir().expect("server directory");
    let handle = ServerRuntime::start_with_registry(
        ServerConfig::new(directory.path())
            .with_bind("127.0.0.1", 0)
            .with_unsafe_no_auth(),
        registry,
    )
    .await
    .expect("server");
    let mut socket = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket")
        .0;
    request(
        &mut socket,
        "101",
        "subscribeActivity",
        json!({ "_tag": "thread", "threadId": "feature-disabled" }),
    )
    .await;
    let initial = next_message(&mut socket).await;
    assert!(
        matches!(
            initial,
            ServerMessage::Chunk { ref values, .. } if values[0]["kind"] == "snapshot"
        ),
        "unexpected initial stream message: {initial:?}"
    );
    ack(&mut socket, "101").await;

    let observer = database
        .enable_queue_backpressure_observation_for_integration_test()
        .expect("queue observer");
    controller.disable().await;
    assert!(matches!(
        next_message(&mut socket).await,
        ServerMessage::Exit {
            exit: coddy_studio_server::RpcExit::Failure { ref cause }, ..
        } if cause == &vec![coddy_studio_server::CauseItem::Fail {
            error: json!({
                "_tag": "ActivityError",
                "reason": "featureDisabled",
                "message": "Agent activity is disabled for this environment.",
            }),
        }]
    ));

    let error = unary(
        &mut socket,
        "102",
        "activity.getSnapshot",
        json!({ "_tag": "thread", "threadId": "feature-disabled" }),
    )
    .await
    .expect_err("disabled reads must fail");
    assert_eq!(
        error,
        json!({
            "_tag": "ActivityError",
            "reason": "featureDisabled",
            "message": "Agent activity is disabled for this environment.",
        })
    );

    request(
        &mut socket,
        "103",
        "subscribeActivity",
        json!({ "_tag": "thread", "threadId": "feature-disabled" }),
    )
    .await;
    assert!(matches!(
        next_message(&mut socket).await,
        ServerMessage::Exit {
            exit: coddy_studio_server::RpcExit::Failure { ref cause }, ..
        } if cause == &vec![coddy_studio_server::CauseItem::Fail {
            error: json!({
                "_tag": "ActivityError",
                "reason": "featureDisabled",
                "message": "Agent activity is disabled for this environment.",
            }),
        }]
    ));
    assert_eq!(
        database
            .queue_backpressure_snapshot_for_integration_test()
            .max_reserved_or_queued_jobs,
        0
    );

    socket.close(None).await.expect("close socket");
    handle.shutdown();
    handle.join().await.expect("server joins");
    drop(observer);
}

#[tokio::test]
async fn source_specific_chat_disable_leaves_terminal_rpc_and_stream_running() {
    // Mutation caught: selecting the Chat controller/projection for terminal requests.
    let fixture = SourceSpecificFixture::start().await;
    let mut chat_socket = fixture.connect().await;
    let mut terminal_socket = fixture.connect().await;

    request(
        &mut chat_socket,
        "201",
        "subscribeActivity",
        json!({ "_tag": "thread", "threadId": "rpc" }),
    )
    .await;
    let chat_initial = next_message(&mut chat_socket).await;
    assert!(
        matches!(
            chat_initial,
            ServerMessage::Chunk { ref values, .. } if values[0]["kind"] == "snapshot"
        ),
        "unexpected Chat initial message: {chat_initial:?}"
    );
    ack(&mut chat_socket, "201").await;
    request(
        &mut terminal_socket,
        "301",
        "subscribeActivity",
        json!({ "_tag": "terminal", "threadId": "rpc", "terminalId": "terminal-rpc" }),
    )
    .await;
    let terminal_initial = next_message(&mut terminal_socket).await;
    assert!(
        matches!(
            terminal_initial,
            ServerMessage::Chunk { ref values, .. } if values[0]["kind"] == "snapshot"
        ),
        "unexpected terminal initial message: {terminal_initial:?}"
    );
    ack(&mut terminal_socket, "301").await;

    fixture.chat_controller.disable().await;
    assert!(matches!(
        next_message(&mut chat_socket).await,
        ServerMessage::Exit {
            exit: coddy_studio_server::RpcExit::Failure { .. },
            ..
        }
    ));
    assert!(
        tokio::time::timeout(
            Duration::from_millis(50),
            next_message(&mut terminal_socket),
        )
        .await
        .is_err()
    );
    assert!(
        unary(
            &mut terminal_socket,
            "302",
            "activity.getSnapshot",
            json!({ "_tag": "terminal", "threadId": "rpc", "terminalId": "terminal-rpc" }),
        )
        .await
        .is_ok()
    );
    assert_eq!(
        unary(
            &mut chat_socket,
            "202",
            "activity.getSnapshot",
            json!({ "_tag": "thread", "threadId": "rpc" }),
        )
        .await
        .expect_err("Chat disabled")["reason"],
        "featureDisabled",
    );

    fixture
        .projections
        .terminal()
        .apply(
            "terminal:rpc",
            "event:terminal".to_owned(),
            vec![
                ProviderActivityMutation::upsert_actor(
                    "actor:terminal",
                    None,
                    "Terminal actor",
                    "running",
                )
                .expect("terminal actor"),
            ],
            "2026-08-04T12:00:00Z".to_owned(),
        )
        .await
        .expect("terminal delta");
    let delta = next_message(&mut terminal_socket).await;
    assert!(matches!(
        delta,
        ServerMessage::Chunk { ref values, .. }
            if values[0]["kind"] == "delta"
                && values[0]["delta"]["scopeId"] == "terminal:rpc"
    ));

    chat_socket.close(None).await.expect("close chat socket");
    terminal_socket
        .close(None)
        .await
        .expect("close terminal socket");
    fixture.shutdown().await;
}

#[tokio::test]
async fn source_specific_terminal_disable_leaves_chat_rpc_and_stream_running() {
    // Mutation caught: selecting the Terminal controller/projection for thread requests.
    let fixture = SourceSpecificFixture::start().await;
    let mut chat_socket = fixture.connect().await;
    let mut terminal_socket = fixture.connect().await;

    request(
        &mut chat_socket,
        "401",
        "subscribeActivity",
        json!({ "_tag": "thread", "threadId": "rpc" }),
    )
    .await;
    let chat_initial = next_message(&mut chat_socket).await;
    assert!(
        matches!(
            chat_initial,
            ServerMessage::Chunk { ref values, .. } if values[0]["kind"] == "snapshot"
        ),
        "unexpected Chat initial message: {chat_initial:?}"
    );
    ack(&mut chat_socket, "401").await;
    request(
        &mut terminal_socket,
        "501",
        "subscribeActivity",
        json!({ "_tag": "terminal", "threadId": "rpc", "terminalId": "terminal-rpc" }),
    )
    .await;
    let terminal_initial = next_message(&mut terminal_socket).await;
    assert!(
        matches!(
            terminal_initial,
            ServerMessage::Chunk { ref values, .. } if values[0]["kind"] == "snapshot"
        ),
        "unexpected terminal initial message: {terminal_initial:?}"
    );
    ack(&mut terminal_socket, "501").await;

    fixture.terminal_controller.disable().await;
    assert!(matches!(
        next_message(&mut terminal_socket).await,
        ServerMessage::Exit {
            exit: coddy_studio_server::RpcExit::Failure { .. },
            ..
        }
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), next_message(&mut chat_socket))
            .await
            .is_err()
    );
    assert!(
        unary(
            &mut chat_socket,
            "402",
            "activity.getSnapshot",
            json!({ "_tag": "thread", "threadId": "rpc" }),
        )
        .await
        .is_ok()
    );
    assert_eq!(
        unary(
            &mut terminal_socket,
            "502",
            "activity.getSnapshot",
            json!({ "_tag": "terminal", "threadId": "rpc", "terminalId": "terminal-rpc" }),
        )
        .await
        .expect_err("Terminal disabled")["reason"],
        "featureDisabled",
    );

    fixture
        .projections
        .chat()
        .apply(
            "thread:rpc",
            "event:chat".to_owned(),
            vec![
                ProviderActivityMutation::upsert_actor("actor:chat", None, "Chat actor", "running")
                    .expect("chat actor"),
            ],
            "2026-08-04T12:00:00Z".to_owned(),
        )
        .await
        .expect("chat delta");
    let delta = next_message(&mut chat_socket).await;
    assert!(matches!(
        delta,
        ServerMessage::Chunk { ref values, .. }
            if values[0]["kind"] == "delta"
                && values[0]["delta"]["scopeId"] == "thread:rpc"
    ));

    chat_socket.close(None).await.expect("close chat socket");
    terminal_socket
        .close(None)
        .await
        .expect("close terminal socket");
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unary_activity_reads_are_drained_and_fenced_when_disable_wins() {
    // Mutations caught:
    // - omitting unary reads from the controller's admitted in-flight work;
    // - publishing an old-generation success after disable closes admission;
    // - starting repository work for a request made after disable completes.
    let cases = [
        (
            "activity.getSnapshot",
            json!({ "_tag": "thread", "threadId": "unary-disable-race" }),
        ),
        (
            "activity.listRoster",
            json!({
                "scope": { "_tag": "thread", "threadId": "unary-disable-race" },
                "scopeId": "thread:unary-disable-race",
                "section": "subagents",
                "bucket": "active",
                "limit": 1
            }),
        ),
        (
            "activity.listDetail",
            json!({
                "scope": { "_tag": "thread", "threadId": "unary-disable-race" },
                "scopeId": "thread:unary-disable-race",
                "recordKind": "actor",
                "recordId": "actor:unary-disable-race",
                "limit": 1
            }),
        ),
    ];

    for (index, (method, payload)) in cases.into_iter().enumerate() {
        assert_unary_activity_read_is_drained_and_fenced(index, method, payload).await;
    }
}

async fn assert_unary_activity_read_is_drained_and_fenced(
    index: usize,
    method: &str,
    payload: Value,
) {
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let controller = AgentActivityController::new(true);
    let projections = ActivityProjections::new(
        ActivityRepository::new(database.clone()),
        controller.clone(),
        AgentActivityController::new(true),
    );
    let projection = projections.chat();
    let scope = thread_scope("thread:unary-disable-race", "unary-disable-race");
    projection.ensure_scope(scope.clone()).await.expect("scope");
    projection
        .apply(
            &scope.scope_id,
            "event:unary-disable-race".to_owned(),
            vec![
                ProviderActivityMutation::upsert_actor(
                    "actor:unary-disable-race",
                    None,
                    "Unary disable race",
                    "running",
                )
                .expect("actor"),
            ],
            "2026-07-31T12:00:00Z".to_owned(),
        )
        .await
        .expect("seed activity");

    let mut registry = RpcRegistry::empty();
    register_activity_rpc_for_integration_test(&mut registry, projections);
    let directory = tempfile::tempdir().expect("server directory");
    let handle = ServerRuntime::start_with_registry(
        ServerConfig::new(directory.path())
            .with_bind("127.0.0.1", 0)
            .with_unsafe_no_auth(),
        registry,
    )
    .await
    .expect("server");
    let mut socket = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket")
        .0;

    let observer = database
        .enable_queue_backpressure_observation_for_integration_test()
        .expect("queue observer");
    let (worker_entered_sender, worker_entered_receiver) = std_mpsc::sync_channel(1);
    let (release_worker_sender, release_worker_receiver) = std_mpsc::sync_channel(1);
    let blocked_database = database.clone();
    let blocker = tokio::spawn(async move {
        blocked_database
            .call(move |_connection| {
                worker_entered_sender
                    .send(())
                    .expect("report blocked database worker");
                let _ = release_worker_receiver.recv();
                Ok(())
            })
            .await
    });
    worker_entered_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("database worker blocker starts");

    let request_id = (1_000 + index * 2).to_string();
    request(&mut socket, &request_id, method, payload.clone()).await;
    timeout(Duration::from_secs(1), async {
        while database
            .queue_backpressure_snapshot_for_integration_test()
            .reserved_or_queued_jobs
            != 1
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{method} did not start repository work while enabled"));

    let disable_controller = controller.clone();
    let mut disable = tokio::spawn(async move { disable_controller.disable().await });
    timeout(Duration::from_secs(1), async {
        while controller.snapshot().enabled {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("disable closes admission");
    assert!(
        timeout(Duration::from_millis(50), &mut disable)
            .await
            .is_err(),
        "{method} was not retained as in-flight work through response enqueue"
    );

    release_worker_sender
        .send(())
        .expect("release database worker");
    blocker
        .await
        .expect("database blocker task")
        .expect("database blocker");
    disable.await.expect("disable task");
    let raced = next_message(&mut socket).await;
    assert_activity_feature_disabled(&raced, &request_id, method);

    drop(observer);
    let post_disable_observer = database
        .enable_queue_backpressure_observation_for_integration_test()
        .expect("post-disable queue observer");
    let post_disable_request_id = (1_001 + index * 2).to_string();
    let post_disable = unary(&mut socket, &post_disable_request_id, method, payload)
        .await
        .expect_err("activity unary unexpectedly succeeded after disable");
    assert_eq!(
        post_disable,
        json!({
            "_tag": "ActivityError",
            "reason": "featureDisabled",
            "message": "Agent activity is disabled for this environment.",
        }),
        "{method} post-disable error"
    );
    assert_eq!(
        database
            .queue_backpressure_snapshot_for_integration_test()
            .max_reserved_or_queued_jobs,
        0,
        "{method} started repository work after disable"
    );

    socket.close(None).await.expect("close socket");
    handle.shutdown();
    handle.join().await.expect("server joins");
    drop(post_disable_observer);
}

fn assert_activity_feature_disabled(message: &ServerMessage, request_id: &str, method: &str) {
    assert!(
        matches!(
            message,
            ServerMessage::Exit {
                request_id: actual_request_id,
                exit: coddy_studio_server::RpcExit::Failure { cause },
            } if actual_request_id.as_str() == request_id
                && cause == &vec![coddy_studio_server::CauseItem::Fail {
                    error: json!({
                        "_tag": "ActivityError",
                        "reason": "featureDisabled",
                        "message": "Agent activity is disabled for this environment.",
                    }),
                }]
        ),
        "{method} returned an old-generation response after disable: {message:?}"
    );
}

#[tokio::test]
async fn hardening_generation_scope_stream_replaces_snapshot_when_terminal_generation_changes() {
    let fixture = Fixture::start(16).await;
    let first = ActivityScopeSeed::terminal(
        "terminal:generation-1",
        "generation-1",
        "thread-generation",
        "terminal-1",
        "codex",
        Some("codex"),
        ActivityCapabilities::structured_full(true),
    )
    .expect("first terminal scope");
    fixture
        .terminal_projection
        .ensure_scope(first.clone())
        .await
        .expect("first scope");
    fixture
        .terminal_projection
        .apply(
            &first.scope_id,
            "generation-1:event".to_owned(),
            vec![
                ProviderActivityMutation::upsert_actor(
                    "generation-1:actor:old",
                    None,
                    "Old worker",
                    "running",
                )
                .expect("old actor"),
            ],
            "2026-07-22T12:00:00Z".to_owned(),
        )
        .await
        .expect("old activity");

    let mut socket = fixture.connect().await;
    request(
        &mut socket,
        "40",
        "subscribeActivity",
        json!({
            "_tag": "terminal",
            "threadId": "thread-generation",
            "terminalId": "terminal-1"
        }),
    )
    .await;
    let initial = next_message(&mut socket).await;
    assert!(
        matches!(
            initial,
            ServerMessage::Chunk { ref values, .. }
                if values[0]["kind"] == "snapshot"
                    && values[0]["snapshot"]["scopeId"] == "terminal:generation-1"
        ),
        "unexpected initial stream message: {initial:?}"
    );
    ack(&mut socket, "40").await;

    let second = ActivityScopeSeed::terminal(
        "terminal:generation-2",
        "generation-2",
        "thread-generation",
        "terminal-1",
        "codex",
        Some("codex"),
        ActivityCapabilities::structured_full(true),
    )
    .expect("second terminal scope");
    fixture
        .terminal_projection
        .ensure_scope(second)
        .await
        .expect("replacement scope");

    let replacement = timeout(Duration::from_secs(1), next_message(&mut socket))
        .await
        .expect("logical terminal stream must receive a replacement snapshot");
    assert!(matches!(
        replacement,
        ServerMessage::Chunk { ref values, .. }
            if values[0]["kind"] == "snapshot"
                && values[0]["snapshot"]["scopeId"] == "terminal:generation-2"
                && values[0]["snapshot"]["actors"].as_array().is_some_and(Vec::is_empty)
    ));
    ack(&mut socket, "40").await;

    socket.close(None).await.expect("close socket");
    fixture.shutdown().await;
}

#[tokio::test]
async fn lag_replaces_the_stream_from_a_fresh_snapshot_and_interrupt_cancels_it() {
    let fixture = Fixture::start(2).await;
    let scope = thread_scope("thread:lag", "lag");
    fixture
        .chat_projection
        .ensure_scope(scope.clone())
        .await
        .expect("scope");

    let mut socket = fixture.connect().await;
    request(
        &mut socket,
        "20",
        "subscribeActivity",
        json!({ "_tag": "thread", "threadId": "lag" }),
    )
    .await;
    assert!(matches!(
        next_message(&mut socket).await,
        ServerMessage::Chunk { ref values, .. } if values[0]["kind"] == "snapshot"
    ));

    for index in 0..12 {
        fixture
            .chat_projection
            .apply(
                &scope.scope_id,
                format!("event:{index:02}"),
                vec![
                    ProviderActivityMutation::upsert_actor(
                        format!("actor:{index:02}"),
                        None,
                        format!("Actor {index}"),
                        "running",
                    )
                    .expect("actor"),
                ],
                format!("2026-07-22T12:00:{index:02}Z"),
            )
            .await
            .expect("delta");
    }

    ack(&mut socket, "20").await;
    let mut replacement = None;
    for _ in 0..4 {
        let message = next_message(&mut socket).await;
        if let ServerMessage::Chunk { values, .. } = &message
            && values[0]["kind"] == "snapshot"
        {
            replacement = Some(values[0].clone());
            break;
        }
        ack(&mut socket, "20").await;
    }
    let replacement = replacement.expect("lag must produce a replacement snapshot");
    assert_eq!(replacement["snapshot"]["revision"], 12);
    assert_eq!(replacement["snapshot"]["counts"]["subagents"]["active"], 12);

    ack(&mut socket, "20").await;
    assert!(
        timeout(Duration::from_millis(100), next_message(&mut socket))
            .await
            .is_err(),
        "stream emitted a delta already covered by the replacement snapshot"
    );
    socket
        .send(Message::Text(
            json!({ "_tag": "Interrupt", "requestId": "20" })
                .to_string()
                .into(),
        ))
        .await
        .expect("interrupt stream");
    assert!(
        matches!(
            next_message(&mut socket).await,
            ServerMessage::Exit { ref request_id, .. } if request_id.as_str() == "20"
        ),
        "interrupt must terminate the stream"
    );

    socket.close(None).await.expect("close socket");
    fixture.shutdown().await;
}

#[tokio::test]
async fn terminal_scope_cannot_be_requested_through_a_different_thread() {
    let fixture = Fixture::start(16).await;
    fixture
        .terminal_projection
        .ensure_scope(
            ActivityScopeSeed::terminal(
                "terminal:generation",
                "generation",
                "owner-thread",
                "terminal-1",
                "codex",
                Some("codex"),
                ActivityCapabilities::structured_full(true),
            )
            .expect("terminal scope"),
        )
        .await
        .expect("scope");

    let mut socket = fixture.connect().await;
    let error = unary(
        &mut socket,
        "30",
        "activity.getSnapshot",
        json!({
            "_tag": "terminal",
            "threadId": "different-thread",
            "terminalId": "terminal-1"
        }),
    )
    .await
    .expect_err("cross-thread terminal scope");
    assert_eq!(
        error,
        json!({
            "_tag": "ActivityError",
            "message": "The requested activity scope is invalid.",
            "reason": "invalidScope"
        })
    );

    let missing = unary(
        &mut socket,
        "31",
        "activity.getSnapshot",
        json!({
            "_tag": "terminal",
            "threadId": "owner-thread",
            "terminalId": "terminal-missing"
        }),
    )
    .await
    .expect_err("missing terminal scope");
    assert_eq!(
        missing,
        json!({
            "_tag": "ActivityError",
            "message": "The activity scope was not found.",
            "reason": "notFound"
        })
    );

    socket.close(None).await.expect("close socket");
    fixture.shutdown().await;
}

#[tokio::test]
async fn activity_paging_binds_a_current_scope_id_to_its_terminal_root() {
    let fixture = Fixture::start(16).await;
    let stale = ActivityScopeSeed::terminal(
        "terminal:stale-generation",
        "generation:stale",
        "owner-thread",
        "terminal:paging-boundary",
        "codex",
        Some("codex"),
        ActivityCapabilities::structured_full(true),
    )
    .expect("stale terminal scope");
    let current = ActivityScopeSeed::terminal(
        "terminal:current-generation",
        "generation:current",
        "owner-thread",
        "terminal:paging-boundary",
        "codex",
        Some("codex"),
        ActivityCapabilities::structured_full(true),
    )
    .expect("current terminal scope");
    fixture
        .terminal_projection
        .ensure_scope(stale.clone())
        .await
        .expect("stale scope");
    fixture
        .terminal_projection
        .ensure_scope(current.clone())
        .await
        .expect("current scope");

    let current_root = json!({
        "_tag": "terminal",
        "threadId": "owner-thread",
        "terminalId": "terminal:paging-boundary"
    });
    let mut socket = fixture.connect().await;
    let valid = unary(
        &mut socket,
        "34",
        "activity.listRoster",
        json!({
            "scope": current_root,
            "scopeId": current.scope_id,
            "section": "subagents",
            "bucket": "active",
            "limit": 1
        }),
    )
    .await
    .expect("current terminal root can page");
    assert!(valid["records"].is_array());

    let stale_error = unary(
        &mut socket,
        "35",
        "activity.listRoster",
        json!({
            "scope": current_root,
            "scopeId": stale.scope_id,
            "section": "subagents",
            "bucket": "active",
            "limit": 1
        }),
    )
    .await
    .expect_err("stale generation must not page");
    let stale_detail_error = unary(
        &mut socket,
        "36",
        "activity.listDetail",
        json!({
            "scope": current_root,
            "scopeId": stale.scope_id,
            "recordKind": "actor",
            "recordId": "actor:unreachable",
            "limit": 1
        }),
    )
    .await
    .expect_err("stale generation must not page record detail");
    let mismatched_error = unary(
        &mut socket,
        "37",
        "activity.listRoster",
        json!({
            "scope": {
                "_tag": "terminal",
                "threadId": "different-thread",
                "terminalId": "terminal:paging-boundary"
            },
            "scopeId": current.scope_id,
            "section": "subagents",
            "bucket": "active",
            "limit": 1
        }),
    )
    .await
    .expect_err("terminal scope ID must be bound to its owning thread");
    let expected = json!({
        "_tag": "ActivityError",
        "message": "The requested activity scope is invalid.",
        "reason": "invalidScope"
    });
    assert_eq!(stale_error, expected);
    assert_eq!(stale_detail_error, expected);
    assert_eq!(mismatched_error, expected);

    socket.close(None).await.expect("close socket");
    fixture.shutdown().await;
}

#[tokio::test]
async fn foreign_activity_detail_is_indistinguishable_from_a_missing_record() {
    let fixture = Fixture::start(16).await;
    let foreign_scope = thread_scope("thread:foreign-detail", "foreign-detail");
    let requested_scope = thread_scope("thread:requested-detail", "requested-detail");
    fixture
        .chat_projection
        .ensure_scope(foreign_scope.clone())
        .await
        .expect("foreign scope");
    fixture
        .chat_projection
        .ensure_scope(requested_scope.clone())
        .await
        .expect("requested scope");
    fixture
        .chat_projection
        .apply(
            &foreign_scope.scope_id,
            "event:foreign-record".to_owned(),
            vec![
                ProviderActivityMutation::upsert_actor(
                    "actor:foreign-only",
                    None,
                    "Foreign actor",
                    "running",
                )
                .expect("foreign actor"),
            ],
            "2026-07-22T12:00:00Z".to_owned(),
        )
        .await
        .expect("foreign activity");

    let mut socket = fixture.connect().await;
    let foreign = unary(
        &mut socket,
        "32",
        "activity.listDetail",
        json!({
            "scope": { "_tag": "thread", "threadId": "requested-detail" },
            "scopeId": requested_scope.scope_id,
            "recordKind": "actor",
            "recordId": "actor:foreign-only",
            "limit": 1
        }),
    )
    .await
    .expect_err("foreign record must not be visible through another scope");
    let missing = unary(
        &mut socket,
        "33",
        "activity.listDetail",
        json!({
            "scope": { "_tag": "thread", "threadId": "requested-detail" },
            "scopeId": requested_scope.scope_id,
            "recordKind": "actor",
            "recordId": "actor:does-not-exist",
            "limit": 1
        }),
    )
    .await
    .expect_err("missing record must report the same public boundary");

    assert_eq!(foreign, missing);
    assert_eq!(
        foreign,
        json!({
            "_tag": "ActivityError",
            "message": "The activity scope was not found.",
            "reason": "notFound"
        })
    );
    socket.close(None).await.expect("close socket");
    fixture.shutdown().await;
}

#[test]
fn activity_methods_are_in_the_authenticated_rpc_inventory() {
    let activity_methods = ACTIVE_RPC_METHODS
        .iter()
        .filter(|method| method.name.starts_with("activity.") || method.name == "subscribeActivity")
        .map(|method| (method.name, method.mode))
        .collect::<Vec<_>>();

    assert_eq!(
        activity_methods,
        [
            ("activity.cancelSubtree", MethodMode::Unary),
            ("activity.getSnapshot", MethodMode::Unary),
            ("activity.listDetail", MethodMode::Unary),
            ("activity.listRoster", MethodMode::Unary),
            ("activity.retrySubtreeCancellation", MethodMode::Unary,),
            ("subscribeActivity", MethodMode::Stream),
        ]
    );
}

#[tokio::test]
async fn activity_authorization_rejects_unauthenticated_websocket_before_unary_or_stream_dispatch()
{
    let fixture = AuthRequiredFixture::start(16).await;
    let activity_methods = ACTIVE_RPC_METHODS
        .iter()
        .filter(|method| method.name.starts_with("activity.") || method.name == "subscribeActivity")
        .collect::<Vec<_>>();
    assert_eq!(
        activity_methods
            .iter()
            .map(|method| method.name)
            .collect::<Vec<_>>(),
        [
            "activity.cancelSubtree",
            "activity.getSnapshot",
            "activity.listDetail",
            "activity.listRoster",
            "activity.retrySubtreeCancellation",
            "subscribeActivity",
        ]
    );
    assert!(
        connect_async(format!("ws://{}/ws", fixture.handle.local_addr()))
            .await
            .is_err(),
        "authentication must reject the connection before any activity unary or stream handler runs"
    );
    fixture.shutdown().await;
}

struct Fixture {
    _directory: TempDir,
    chat_projection: ActivityProjection,
    terminal_projection: ActivityProjection,
    handle: coddy_studio_server::ServerHandle,
}

struct SourceSpecificFixture {
    _directory: TempDir,
    projections: ActivityProjections,
    chat_controller: AgentActivityController,
    terminal_controller: AgentActivityController,
    handle: coddy_studio_server::ServerHandle,
}

struct AuthRequiredFixture {
    _directory: TempDir,
    handle: coddy_studio_server::ServerHandle,
}

impl AuthRequiredFixture {
    async fn start(capacity: usize) -> Self {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let projections = ActivityProjections::with_capacity(
            ActivityRepository::new(database),
            AgentActivityController::new(true),
            AgentActivityController::new(true),
            capacity,
        );
        let mut registry = RpcRegistry::empty();
        register_activity_rpc_for_integration_test(&mut registry, projections);
        let directory = tempfile::tempdir().expect("temporary server directory");
        let handle = ServerRuntime::start_with_registry(
            ServerConfig::new(directory.path()).with_bind("127.0.0.1", 0),
            registry,
        )
        .await
        .expect("authenticated server");
        Self {
            _directory: directory,
            handle,
        }
    }

    async fn shutdown(self) {
        self.handle.shutdown();
        self.handle.join().await.expect("server joins");
    }
}

impl Fixture {
    async fn start(capacity: usize) -> Self {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let projections = ActivityProjections::with_capacity(
            ActivityRepository::new(database),
            AgentActivityController::new(true),
            AgentActivityController::new(true),
            capacity,
        );
        let chat_projection = projections.chat();
        let terminal_projection = projections.terminal();
        let mut registry = RpcRegistry::empty();
        register_activity_rpc_for_integration_test(&mut registry, projections);
        let directory = tempfile::tempdir().expect("temporary server directory");
        let handle = ServerRuntime::start_with_registry(
            ServerConfig::new(directory.path())
                .with_bind("127.0.0.1", 0)
                .with_unsafe_no_auth(),
            registry,
        )
        .await
        .expect("server");
        Self {
            _directory: directory,
            chat_projection,
            terminal_projection,
            handle,
        }
    }

    async fn connect(
        &self,
    ) -> WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
        connect_async(format!("ws://{}/ws", self.handle.local_addr()))
            .await
            .expect("WebSocket")
            .0
    }

    async fn shutdown(self) {
        self.handle.shutdown();
        self.handle.join().await.expect("server joins");
    }
}

impl SourceSpecificFixture {
    async fn start() -> Self {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let chat_controller = AgentActivityController::new(true);
        let terminal_controller = AgentActivityController::new(true);
        let projections = ActivityProjections::new(
            ActivityRepository::new(database),
            chat_controller.clone(),
            terminal_controller.clone(),
        );
        projections
            .chat()
            .ensure_scope(thread_scope("thread:rpc", "rpc"))
            .await
            .expect("chat scope");
        projections
            .terminal()
            .ensure_scope(
                ActivityScopeSeed::terminal(
                    "terminal:rpc",
                    "terminal-generation:rpc",
                    "rpc",
                    "terminal-rpc",
                    "codex",
                    Some("codex"),
                    ActivityCapabilities::structured_full(true),
                )
                .expect("terminal scope"),
            )
            .await
            .expect("terminal scope persistence");
        let mut registry = RpcRegistry::empty();
        register_activity_rpc_for_integration_test(&mut registry, projections.clone());
        let directory = tempfile::tempdir().expect("temporary server directory");
        let handle = ServerRuntime::start_with_registry(
            ServerConfig::new(directory.path())
                .with_bind("127.0.0.1", 0)
                .with_unsafe_no_auth(),
            registry,
        )
        .await
        .expect("server");
        Self {
            _directory: directory,
            projections,
            chat_controller,
            terminal_controller,
            handle,
        }
    }

    async fn connect(
        &self,
    ) -> WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
        connect_async(format!("ws://{}/ws", self.handle.local_addr()))
            .await
            .expect("WebSocket")
            .0
    }

    async fn shutdown(self) {
        self.handle.shutdown();
        self.handle.join().await.expect("server joins");
    }
}

fn thread_scope(scope_id: &str, thread_id: &str) -> ActivityScopeSeed {
    ActivityScopeSeed::thread(
        scope_id,
        thread_id,
        "codex",
        Some("codex"),
        ActivityCapabilities::structured_full(false),
    )
    .expect("thread scope")
}

fn actor(
    id: &str,
    status: ActivityLifecycle,
    updated_at: &str,
    terminal_at: Option<&str>,
) -> Result<ActivityActorSummary, coddy_studio_server::activity::ActivityModelError> {
    ActivityActorSummary::try_new(
        id,
        None,
        id,
        None,
        None,
        status,
        None,
        "2026-07-22T12:00:00Z",
        updated_at,
        terminal_at,
    )
}

fn work_item(
    id: &str,
    owner_actor_id: &str,
) -> Result<ActivityWorkItemSummary, coddy_studio_server::activity::ActivityModelError> {
    ActivityWorkItemSummary::try_new(
        id,
        Some(owner_actor_id),
        id,
        "task",
        None,
        None,
        ActivityLifecycle::Running,
        None,
        "2026-07-22T12:00:00Z",
        "2026-07-22T12:00:00Z",
        None,
    )
}

fn entry(
    id: &str,
    owner_id: &str,
    created_at: &str,
) -> Result<ActivityEntry, coddy_studio_server::activity::ActivityModelError> {
    ActivityEntry::try_new(
        id,
        ActivityRecordKind::Actor,
        owner_id,
        ActivityEntryKind::Commentary,
        id,
        None,
        ActivityEntryTone::Info,
        created_at,
    )
}

async fn unary<S>(
    socket: &mut WebSocketStream<S>,
    id: &str,
    tag: &str,
    payload: Value,
) -> Result<Value, Value>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    request(socket, id, tag, payload).await;
    match next_message(socket).await {
        ServerMessage::Exit {
            exit: coddy_studio_server::RpcExit::Success { value: Some(value) },
            ..
        } => Ok(value),
        ServerMessage::Exit {
            exit: coddy_studio_server::RpcExit::Failure { cause },
            ..
        } => Err(cause
            .into_iter()
            .find_map(|item| match item {
                coddy_studio_server::CauseItem::Fail { error } => Some(error),
                _ => None,
            })
            .expect("typed RPC error")),
        message => panic!("unexpected unary message: {message:?}"),
    }
}

async fn request<S>(socket: &mut WebSocketStream<S>, id: &str, tag: &str, payload: Value)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    socket
        .send(Message::Text(
            json!({
                "_tag": "Request",
                "id": id,
                "tag": tag,
                "payload": payload,
                "headers": []
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("send request");
}

async fn ack<S>(socket: &mut WebSocketStream<S>, request_id: &str)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    socket
        .send(Message::Text(
            json!({ "_tag": "Ack", "requestId": request_id })
                .to_string()
                .into(),
        ))
        .await
        .expect("ack stream chunk");
}

async fn next_message<S>(socket: &mut WebSocketStream<S>) -> ServerMessage
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let frame = timeout(Duration::from_secs(2), next_frame_past_heartbeat(socket))
        .await
        .expect("server message timeout")
        .expect("WebSocket open")
        .expect("WebSocket frame");
    let Message::Text(text) = frame else {
        panic!("unexpected WebSocket frame: {frame:?}");
    };
    serde_json::from_str(&text).expect("server message")
}
