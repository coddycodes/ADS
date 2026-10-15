//! `OpenCodeSessionRuntime` on the OpenCode 2 wire, against a server that replays event streams
//! and responses captured from a real OpenCode 2.0.29 server (`tests/fixtures/opencode-v2`).

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{OriginalUri, Path, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use futures_util::{StreamExt, stream};
use serde_json::{Value, json};
use tokio::sync::Notify;

use super::runtime::{OpenCodeRuntimeEventStableView, OpenCodeSessionRuntime};

fn fixture(name: &str) -> String {
    let path = format!(
        "{}/tests/fixtures/opencode-v2/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"))
}

fn events(scenario: &str) -> Vec<Value> {
    fixture(&format!("{scenario}.events.jsonl"))
        .lines()
        .map(|line| serde_json::from_str(line).expect("fixture event"))
        .collect()
}

/// The captured response to `method *path_suffix` in a scenario's exchanges.
fn exchange(scenario: &str, method: &str, path_suffix: &str) -> (u16, Value) {
    let exchanges: Value =
        serde_json::from_str(&fixture(&format!("{scenario}.exchanges.json"))).expect("exchanges");
    let exchange = exchanges["exchanges"]
        .as_array()
        .expect("exchanges")
        .iter()
        .find(|exchange| {
            exchange["method"] == method
                && exchange["path"]
                    .as_str()
                    .is_some_and(|path| path.split('?').next().unwrap().ends_with(path_suffix))
        })
        .unwrap_or_else(|| panic!("{scenario}: no {method} *{path_suffix}"));
    (
        u16::try_from(exchange["status"].as_u64().expect("status")).expect("HTTP status"),
        exchange["response"].clone(),
    )
}

enum Step {
    Send(Value),
    /// Wait until BiBCode replies to an ask, or interrupts.
    AwaitReply,
}

/// Replays one captured scenario. The event stream opens with `server.connected`, waits for the
/// prompt, then sends the captured events, pausing after a permission ask or a form until BiBCode
/// replies, and after the first text delta of the interrupt scenario until BiBCode interrupts.
struct Replay {
    scenario: &'static str,
    root: String,
    requests: StdMutex<Vec<(String, String, Value)>>,
    prompted: Notify,
    replied: Notify,
    /// Fail the next reply with HTTP 500, as a transient server error would.
    fail_next_reply: AtomicBool,
    /// Accept the next reply (OpenCode settles it) but answer HTTP 500, as a lost response would.
    lose_next_reply_response: AtomicBool,
    /// `/api/session/active` lists the root session.
    active: AtomicBool,
}

impl Replay {
    fn requests_to(&self, path_suffix: &str) -> Vec<(String, String, Value)> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, uri, _)| uri.split('?').next().unwrap().ends_with(path_suffix))
            .cloned()
            .collect()
    }
}

async fn record(
    State(replay): State<Arc<Replay>>,
    method: Method,
    uri: OriginalUri,
    body: Bytes,
) -> Response {
    let body = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let path = uri.0.path().to_owned();
    replay
        .requests
        .lock()
        .unwrap()
        .push((method.to_string(), uri.0.to_string(), body));
    if path == "/api/session" {
        let (_, response) = exchange(replay.scenario, "POST", "/api/session");
        return Json(response).into_response();
    }
    if path.ends_with("/prompt") {
        replay.prompted.notify_one();
        let (status, response) = exchange(replay.scenario, "POST", "/prompt");
        return (StatusCode::from_u16(status).unwrap(), Json(response)).into_response();
    }
    if path.ends_with("/command") {
        replay.prompted.notify_one();
    }
    if path.ends_with("/reply") {
        if replay.fail_next_reply.swap(false, Ordering::SeqCst) {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        replay.replied.notify_one();
        if replay
            .lose_next_reply_response
            .swap(false, Ordering::SeqCst)
        {
            tokio::time::sleep(Duration::from_millis(200)).await;
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }
    if path.ends_with("/interrupt") {
        replay.replied.notify_one();
        return Json(json!({"interrupted": true})).into_response();
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn session(State(replay): State<Arc<Replay>>, Path(session_id): Path<String>) -> Response {
    if session_id == replay.root {
        let (_, response) = exchange(
            replay.scenario,
            "GET",
            &format!("/api/session/{session_id}"),
        );
        Json(response).into_response()
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(json!({"_tag":"SessionNotFoundError","message":"Session not found"})),
        )
            .into_response()
    }
}

async fn message(
    State(replay): State<Arc<Replay>>,
    Path((_, message_id)): Path<(String, String)>,
) -> Response {
    let exchanges: Value =
        serde_json::from_str(&fixture(&format!("{}.exchanges.json", replay.scenario))).unwrap();
    let found = exchanges["exchanges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|exchange| {
            exchange["method"] == "GET"
                && exchange["path"]
                    .as_str()
                    .is_some_and(|path| path.ends_with(&format!("/message/{message_id}")))
        })
        .cloned();
    match found {
        Some(exchange) => (
            StatusCode::from_u16(u16::try_from(exchange["status"].as_u64().unwrap()).unwrap())
                .unwrap(),
            Json(exchange["response"].clone()),
        )
            .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"_tag":"MessageNotFoundError","message":"Message not found"})),
        )
            .into_response(),
    }
}

/// A captured GET response, or an empty list when the scenario did not read it.
async fn captured_list(State(replay): State<Arc<Replay>>, uri: OriginalUri) -> Json<Value> {
    let exchanges: Value =
        serde_json::from_str(&fixture(&format!("{}.exchanges.json", replay.scenario))).unwrap();
    let path = uri.0.path().to_owned();
    Json(
        exchanges["exchanges"]
            .as_array()
            .unwrap()
            .iter()
            .find(|exchange| {
                exchange["method"] == "GET"
                    && exchange["path"]
                        .as_str()
                        .is_some_and(|captured| captured.split('?').next() == Some(path.as_str()))
            })
            .map_or_else(
                || json!({ "data": [] }),
                |exchange| exchange["response"].clone(),
            ),
    )
}

async fn models() -> Json<Value> {
    Json(exchange("catalog", "GET", "/api/model").1)
}

async fn subscribe(State(replay): State<Arc<Replay>>) -> Response {
    let mut steps = VecDeque::new();
    let mut paused_for_interrupt = false;
    for event in events(replay.scenario) {
        let kind = event["type"].as_str().unwrap_or_default().to_owned();
        if kind == "server.connected" {
            continue;
        }
        steps.push_back(Step::Send(event));
        if matches!(kind.as_str(), "permission.asked" | "form.created")
            || (replay.scenario == "interrupt"
                && kind == "session.text.delta"
                && !std::mem::replace(&mut paused_for_interrupt, true))
        {
            steps.push_back(Step::AwaitReply);
        }
    }
    let connected = stream::once(async {
        Ok::<_, std::convert::Infallible>(Bytes::from_static(
            b"data: {\"id\":\"evt_connected\",\"type\":\"server.connected\",\"data\":{}}\n\n",
        ))
    });
    let replayed = stream::unfold(
        (replay, steps, true),
        |(replay, mut steps, first)| async move {
            if first {
                replay.prompted.notified().await;
            }
            loop {
                match steps.pop_front()? {
                    Step::AwaitReply => replay.replied.notified().await,
                    Step::Send(event) => {
                        let chunk = format!("data: {event}\n\n");
                        return Some((
                            Ok::<_, std::convert::Infallible>(Bytes::from(chunk)),
                            (replay, steps, false),
                        ));
                    }
                }
            }
        },
    );
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .body(Body::from_stream(
            connected.chain(replayed).chain(stream::pending()),
        ))
        .expect("SSE response")
}

async fn serve(scenario: &'static str) -> (String, Arc<Replay>) {
    let root = events(scenario)
        .iter()
        .find(|event| event["type"] == "session.created" && event["data"].get("parentID").is_none())
        .and_then(|event| event["data"]["sessionID"].as_str())
        .expect("root session")
        .to_owned();
    let replay = Arc::new(Replay {
        scenario,
        root,
        requests: StdMutex::new(Vec::new()),
        prompted: Notify::new(),
        replied: Notify::new(),
        fail_next_reply: AtomicBool::new(false),
        lose_next_reply_response: AtomicBool::new(false),
        active: AtomicBool::new(false),
    });
    let app = Router::new()
        .route("/api/session", post(record).get(captured_list))
        .route("/api/session/{session_id}/permission", get(captured_list))
        .route("/api/session/{session_id}/form", get(captured_list))
        .route(
            "/api/session/active",
            get(|State(replay): State<Arc<Replay>>| async move {
                if replay.active.load(Ordering::SeqCst) {
                    Json(json!({ "data": { replay.root.clone(): { "type": "running" } } }))
                } else {
                    Json(json!({ "data": {} }))
                }
            }),
        )
        .route("/api/session/{session_id}", get(session).patch(record))
        .route(
            "/api/session/{session_id}/message/{message_id}",
            get(message),
        )
        .route("/api/session/{session_id}/prompt", post(record))
        .route("/api/session/{session_id}/command", post(record))
        .route("/api/session/{session_id}/model", post(record))
        .route("/api/session/{session_id}/agent", post(record))
        .route("/api/session/{session_id}/interrupt", post(record))
        .route(
            "/api/session/{session_id}/permission/{request_id}/reply",
            post(record),
        )
        .route(
            "/api/session/{session_id}/form/{form_id}/reply",
            post(record),
        )
        .route("/api/experimental/mcp/{name}", put(record))
        .route("/api/model", get(models))
        .route(
            "/api/agent",
            get(|| async { Json(exchange("catalog", "GET", "/api/agent").1) }),
        )
        .route(
            "/api/info",
            get(|| async {
                Json(json!({
                    "version": "2.0.29",
                    "pid": 4242,
                    "urls": ["http://127.0.0.1:4096"],
                    "paths": { "tmp": "/tmp/opencode" },
                    "capabilities": { "persistentPty": true },
                }))
            }),
        )
        .route("/api/event", get(subscribe))
        .with_state(replay.clone());
    let (endpoint, _server) = super::server_fixtures::serve(app).await;
    (endpoint, replay)
}

fn v2_runtime(endpoint: &str) -> OpenCodeSessionRuntime {
    let runtime =
        OpenCodeSessionRuntime::new_with_options_reconciliation_revision_and_agent_activity(
            endpoint,
            "thread-1",
            "/work/project",
            Some("mock/mock-model"),
            Some("secret"),
            None,
            0,
            true,
        )
        .expect("runtime");
    runtime.use_v2_api();
    runtime
}

/// Collects stable events until `turn.completed` (`count` of them).
async fn until_turns_complete(
    runtime: &OpenCodeSessionRuntime,
    count: usize,
) -> Vec<OpenCodeRuntimeEventStableView> {
    let mut seen = Vec::new();
    let mut completed = 0;
    tokio::time::timeout(Duration::from_secs(10), async {
        while completed < count {
            let event = runtime.next_event().await.expect("event stream open");
            if event.event_type == "turn.completed" {
                completed += 1;
            }
            seen.push(event.stable_view());
        }
    })
    .await
    .unwrap_or_else(|_| panic!("turns did not complete: {seen:#?}"));
    seen
}

async fn wait_for(
    runtime: &OpenCodeSessionRuntime,
    event_type: &str,
) -> OpenCodeRuntimeEventStableView {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = runtime.next_event().await.expect("event stream open");
            if event.event_type == event_type {
                return event.stable_view();
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no {event_type}"))
}

/// The events already emitted or emitted within a short grace, such as a reply's resolution that
/// the event pump can overtake.
async fn drain(runtime: &OpenCodeSessionRuntime) -> Vec<OpenCodeRuntimeEventStableView> {
    let mut seen = Vec::new();
    while let Ok(Some(event)) =
        tokio::time::timeout(Duration::from_millis(200), runtime.next_event()).await
    {
        seen.push(event.stable_view());
    }
    seen
}

fn text(events: &[OpenCodeRuntimeEventStableView]) -> String {
    events
        .iter()
        .filter(|event| event.event_type == "content.delta")
        .filter_map(|event| event.payload["delta"].as_str())
        .collect()
}

fn of_type<'a>(
    events: &'a [OpenCodeRuntimeEventStableView],
    event_type: &str,
) -> Vec<&'a OpenCodeRuntimeEventStableView> {
    events
        .iter()
        .filter(|event| event.event_type == event_type)
        .collect()
}

#[tokio::test]
async fn a_prompt_creates_the_session_and_streams_one_turn() {
    let (endpoint, replay) = serve("hello").await;
    let runtime = v2_runtime(&endpoint);
    let session_id = runtime.start().await.expect("session starts");
    assert_eq!(session_id, replay.root);

    let turn_id = runtime
        .send_turn(
            Some("SAY_HELLO please"),
            Vec::new(),
            Some("3f2a1b9c-0000-4000-8000-000000000000"),
        )
        .await
        .expect("prompt is accepted");
    let events = until_turns_complete(&runtime, 1).await;

    let created = replay.requests_to("/api/session");
    assert_eq!(created.len(), 1);
    let body = &created[0].2;
    assert_eq!(body["location"], json!({ "directory": "/work/project" }));
    assert_eq!(
        body["model"],
        json!({ "providerID": "mock", "id": "mock-model" })
    );
    assert!(
        body["permissions"]
            .as_array()
            .unwrap()
            .contains(&json!({"action":"shell","resource":"*","effect":"ask"}))
    );
    let prompts = replay.requests_to("/prompt");
    assert_eq!(
        prompts[0].2,
        json!({
            "id": "msg_3f2a1b9c000040008000000000000000",
            "text": "SAY_HELLO please",
            "delivery": "steer",
        })
    );
    assert!(
        replay.requests_to("/model").is_empty(),
        "the create request already carried the model"
    );
    assert_eq!(text(&events), "Hello from mock.");
    assert_eq!(of_type(&events, "message.assistant.completed").len(), 1);
    let completed = of_type(&events, "turn.completed");
    assert_eq!(completed[0].turn_id.as_deref(), Some(turn_id.as_str()));
    assert_eq!(
        completed[0].payload,
        json!({ "state": "completed", "stopReason": "completed" })
    );
    runtime.stop().await.expect("stop");
}

#[tokio::test]
async fn a_shell_permission_is_answered_on_the_asking_session() {
    let (endpoint, replay) = serve("shell").await;
    let runtime = v2_runtime(&endpoint);
    runtime.start().await.expect("session starts");
    runtime
        .send_turn(Some("RUN_SHELL please"), Vec::new(), None)
        .await
        .expect("prompt is accepted");
    let opened = wait_for(&runtime, "request.opened").await;
    assert_eq!(
        opened.payload,
        json!({ "requestType": "exec_command_approval", "detail": "shell: echo bibcode-v2" })
    );
    let request_id = opened.request_id.clone().expect("request id");
    replay.fail_next_reply.store(true, Ordering::SeqCst);
    runtime
        .respond_to_permission(&request_id, "accept")
        .await
        .expect_err("a failed reply is reported");
    runtime
        .respond_to_permission(&request_id, "accept")
        .await
        .expect("the ask stays pending, so the reply can be retried");
    let mut events = until_turns_complete(&runtime, 1).await;
    events.extend(drain(&runtime).await);

    let replies = replay.requests_to("/reply");
    assert_eq!(
        replies[1].1.split('?').next().unwrap(),
        format!("/api/session/{}/permission/{request_id}/reply", replay.root)
    );
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[1].2, json!({ "decision": "once" }));
    let resolved = of_type(&events, "request.resolved");
    assert_eq!(
        resolved.len(),
        1,
        "OpenCode's permission.replied must not resolve the request twice"
    );
    assert_eq!(resolved[0].payload["decision"], "accept");
    assert_eq!(text(&events), "Done after tool.");
}

#[tokio::test]
async fn a_question_form_is_asked_and_answered_by_field_key() {
    let (endpoint, replay) = serve("question").await;
    let runtime = v2_runtime(&endpoint);
    runtime.start().await.expect("session starts");
    runtime
        .send_turn(Some("ASK_QUESTION please"), Vec::new(), None)
        .await
        .expect("prompt is accepted");
    let requested = wait_for(&runtime, "user-input.requested").await;
    assert_eq!(requested.payload["questions"][0]["id"], "q0");
    assert_eq!(requested.payload["questions"][0]["header"], "Color");
    let form_id = requested.request_id.clone().expect("form id");
    runtime
        .respond_to_user_input(&form_id, json!({ "q0": ["Blue"] }))
        .await
        .expect("answer is sent");
    let mut events = until_turns_complete(&runtime, 1).await;
    events.extend(drain(&runtime).await);

    let replies = replay.requests_to("/reply");
    assert_eq!(
        replies[0].1.split('?').next().unwrap(),
        format!("/api/session/{}/form/{form_id}/reply", replay.root)
    );
    assert_eq!(replies[0].2, json!({ "answer": { "q0": "Blue" } }));
    assert_eq!(of_type(&events, "user-input.resolved").len(), 1);
}

#[tokio::test]
async fn a_background_result_is_reported_as_a_turn_opencode_started() {
    let (endpoint, _replay) = serve("background").await;
    let runtime = v2_runtime(&endpoint);
    runtime.start().await.expect("session starts");
    let first = runtime
        .send_turn(Some("SPAWN_BACKGROUND please"), Vec::new(), None)
        .await
        .expect("prompt is accepted");
    let events = until_turns_complete(&runtime, 2).await;

    let started = of_type(&events, "turn.started");
    let completed = of_type(&events, "turn.completed");
    assert_eq!(completed.len(), 2);
    assert_eq!(completed[0].turn_id.as_deref(), Some(first.as_str()));
    assert_eq!(
        started.len(),
        2,
        "BiBCode's turn and OpenCode's: {events:#?}"
    );
    assert_eq!(started[0].turn_id.as_deref(), Some(first.as_str()));
    assert_eq!(started[1].turn_id, completed[1].turn_id);
    assert_ne!(completed[1].turn_id.as_deref(), Some(first.as_str()));
    assert!(text(&events).ends_with("Background result noted."));
}

#[tokio::test]
async fn provider_failures_and_interrupts_complete_the_turn_with_their_state() {
    let (endpoint, _replay) = serve("fail").await;
    let runtime = v2_runtime(&endpoint);
    runtime.start().await.expect("session starts");
    runtime
        .send_turn(Some("FAIL_PROVIDER please"), Vec::new(), None)
        .await
        .expect("prompt is accepted");
    let events = until_turns_complete(&runtime, 1).await;
    assert_eq!(
        of_type(&events, "turn.completed")[0].payload,
        json!({
            "state": "failed",
            "stopReason": "error",
            "errorMessage": "mock provider rejected the request (HTTP 400)",
            "errorClass": "provider_error",
        })
    );

    let (endpoint, replay) = serve("interrupt").await;
    let runtime = v2_runtime(&endpoint);
    runtime.start().await.expect("session starts");
    runtime
        .send_turn(Some("SLOW please"), Vec::new(), None)
        .await
        .expect("prompt is accepted");
    wait_for(&runtime, "content.delta").await;
    runtime.interrupt_turn().await.expect("interrupt is sent");
    let events = until_turns_complete(&runtime, 1).await;
    assert_eq!(replay.requests_to("/interrupt").len(), 1);
    assert_eq!(
        of_type(&events, "turn.completed")[0].payload["state"],
        "cancelled"
    );
}

#[tokio::test]
async fn delivery_lookup_mcp_registration_variants_and_selection_use_v2_routes() {
    let (endpoint, replay) = serve("hello").await;
    let runtime = v2_runtime(&endpoint);
    runtime
        .add_mcp_server("bibcode", "http://127.0.0.1:9/mcp", "Bearer <REDACTED>")
        .await
        .expect("MCP registration");
    let mcp = replay.requests_to("/api/experimental/mcp/bibcode");
    assert!(
        mcp[0]
            .1
            .contains("location%5Bdirectory%5D=%2Fwork%2Fproject"),
        "{}",
        mcp[0].1
    );
    assert_eq!(
        mcp[0].2,
        json!({"config":{"type":"remote","url":"http://127.0.0.1:9/mcp","headers":{"Authorization":"Bearer <REDACTED>"},"oauth":false}})
    );

    runtime.start().await.expect("session starts");
    let (_, captured) = exchange("hello", "POST", "/prompt");
    let delivered = captured["data"]["id"].as_str().expect("delivered id");
    assert!(runtime.message_exists(delivered).await.expect("lookup"));
    assert!(
        !runtime
            .message_exists("msg_doesnotexist000000000000")
            .await
            .expect("lookup")
    );

    runtime
        .set_options(vec![json!({ "id": "variant", "value": "high" })])
        .await
        .expect("the mock model advertises high");
    assert!(
        runtime
            .set_options(vec![json!({ "id": "variant", "value": "turbo" })])
            .await
            .is_err()
    );
    runtime
        .send_turn(Some("SAY_HELLO please"), Vec::new(), None)
        .await
        .expect("prompt is accepted");
    let selection = replay.requests_to("/model");
    assert_eq!(
        selection[0].2,
        json!({"model":{"providerID":"mock","id":"mock-model","variant":"high"}})
    );
    until_turns_complete(&runtime, 1).await;

    let error = runtime.rollback_thread(1).await.expect_err("not built yet");
    assert_eq!(
        error.to_string(),
        "Rewinding isn't supported with OpenCode 2 yet."
    );
}

#[tokio::test]
async fn resume_falls_back_to_a_new_session_only_when_opencode_lost_it() {
    let (endpoint, replay) = serve("hello").await;
    let runtime = v2_runtime(&endpoint);
    assert_eq!(
        runtime.resume(&replay.root).await.expect("resumes"),
        replay.root
    );
    assert!(replay.requests_to("/api/session").is_empty());
    let root_suffix = format!("/api/session/{}", replay.root);
    assert_eq!(
        replay.requests_to(&root_suffix)[0].2["permissions"],
        json!(super::v2::permission_rules("approval-required")),
        "a resumed session gets the permission rules of BiBCode's runtime mode at once"
    );
    runtime
        .send_turn(Some("SAY_HELLO please"), Vec::new(), None)
        .await
        .expect("prompt is accepted");
    assert_eq!(
        replay.requests_to("/agent")[0].2,
        json!({ "agent": "build" }),
        "a resumed session without a selected agent goes back to OpenCode's default agent"
    );
    assert_eq!(
        replay.requests_to("/model")[0].2,
        json!({ "model": { "providerID": "mock", "id": "mock-model" } }),
        "a resumed session is told BiBCode's model"
    );
    assert_eq!(
        replay.requests_to(&root_suffix).len(),
        1,
        "the rules are not sent again with the prompt"
    );

    let (endpoint, replay) = serve("hello").await;
    let runtime = v2_runtime(&endpoint);
    assert_eq!(
        runtime
            .resume("ses_doesnotexist0000000000000")
            .await
            .expect("starts fresh"),
        replay.root
    );
    assert_eq!(replay.requests_to("/api/session").len(), 1);
}

/// A configured Server URL that answers `/api/info` with 2.x gets the V2 wire end to end through
/// the production driver: session, durable delivery, events and the full-access ruleset.
#[tokio::test]
async fn the_driver_speaks_v2_to_an_opencode_2_server_url() {
    use crate::production::provider_runtime::{
        NativeProviderDriverFactory, ProviderDeliveryOutcome, ProviderDriverFactory,
        ProviderLaunchRequest,
    };
    let (endpoint, replay) = serve("hello").await;
    let temp = tempfile::TempDir::new().expect("attachment root");
    let factory = NativeProviderDriverFactory::new(temp.path().join("attachments"));
    let driver = factory
        .create(ProviderLaunchRequest {
            thread_id: "thread-1".to_owned(),
            activity_causal_revision: 0,
            provider: "opencode".to_owned(),
            provider_label: "OpenCode".to_owned(),
            provider_instance_id: Some("opencode".to_owned()),
            binary_path: "opencode".to_owned(),
            cwd: "/work/project".into(),
            runtime_mode: "full-access".to_owned(),
            interaction_mode: "default".to_owned(),
            model: Some("mock/mock-model".to_owned()),
            options: Vec::new(),
            custom_models: Vec::new(),
            service_tier: None,
            effort: None,
            agent: None,
            resume_cursor: None,
            environment: Default::default(),
            endpoint: Some(endpoint),
            server_password: Some("secret".to_owned()),
            mcp: None,
            codex_home: None,
            open_url: None,
        })
        .await
        .unwrap_or_else(|error| panic!("driver: {error}"));
    let started = driver.start().await.expect("session starts");
    assert_eq!(
        started.resume_cursor,
        Some(json!({ "sessionId": replay.root }))
    );
    let outcome = driver
        .deliver(
            "SAY_HELLO please".to_owned(),
            Vec::new(),
            "default".to_owned(),
            "3f2a1b9c-0000-4000-8000-000000000000".to_owned(),
        )
        .await;
    assert!(
        matches!(outcome, ProviderDeliveryOutcome::Accepted { .. }),
        "{outcome:?}"
    );
    let completed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = driver.next_event().await.expect("event stream open");
            if event.event_type == "turn.completed" {
                return event;
            }
        }
    })
    .await
    .expect("turn completes");
    assert_eq!(completed.payload["state"], "completed");
    assert_eq!(
        replay.requests_to("/api/session")[0].2["permissions"],
        json!([{ "action": "*", "resource": "*", "effect": "allow" }])
    );
    assert_eq!(
        replay.requests_to("/prompt")[0].2["id"],
        "msg_3f2a1b9c000040008000000000000000"
    );
    driver.shutdown().await.expect("shutdown");
}

/// Drives a real OpenCode 2 server through a text turn, a shell permission and a question form.
/// Run it against a server whose default model is a scripted endpoint (no provider credits), as
/// in the capture harness described in the OpenCode V2 client design:
/// `BIBCODE_OPENCODE_V2_LIVE=http://127.0.0.1:<port> BIBCODE_OPENCODE_V2_PASSWORD=<password>
/// BIBCODE_OPENCODE_V2_DIRECTORY=<dir> BIBCODE_OPENCODE_V2_MODEL=<provider/model>
/// cargo test -p bibcode-server --lib -- --ignored live_opencode_2`.
#[tokio::test]
#[ignore = "needs a live OpenCode 2 server; see the doc comment"]
async fn live_opencode_2_round_trip() {
    let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"));
    let runtime =
        OpenCodeSessionRuntime::new_with_options_reconciliation_revision_and_agent_activity(
            &var("BIBCODE_OPENCODE_V2_LIVE"),
            "live-thread",
            &var("BIBCODE_OPENCODE_V2_DIRECTORY"),
            Some(&var("BIBCODE_OPENCODE_V2_MODEL")),
            Some(&var("BIBCODE_OPENCODE_V2_PASSWORD")),
            None,
            0,
            true,
        )
        .expect("runtime");
    runtime.use_v2_api();
    runtime.start().await.expect("session starts");

    let key = format!("live-{}", uuid::Uuid::new_v4());
    runtime
        .send_turn(Some("SAY_HELLO please"), Vec::new(), Some(&key))
        .await
        .expect("prompt");
    let events = until_turns_complete(&runtime, 1).await;
    assert_eq!(text(&events), "Hello from mock.");
    assert!(runtime.message_exists(&key).await.expect("lookup"));

    runtime
        .send_turn(Some("RUN_SHELL please"), Vec::new(), None)
        .await
        .expect("prompt");
    let opened = wait_for(&runtime, "request.opened").await;
    runtime
        .respond_to_permission(opened.request_id.as_deref().unwrap(), "accept")
        .await
        .expect("permission reply");
    let events = until_turns_complete(&runtime, 1).await;
    assert_eq!(
        of_type(&events, "turn.completed")[0].payload["state"],
        "completed"
    );

    runtime
        .send_turn(Some("ASK_QUESTION please"), Vec::new(), None)
        .await
        .expect("prompt");
    let requested = wait_for(&runtime, "user-input.requested").await;
    runtime
        .respond_to_user_input(
            requested.request_id.as_deref().unwrap(),
            json!({ "q0": ["Blue"] }),
        )
        .await
        .expect("form reply");
    let events = until_turns_complete(&runtime, 1).await;
    assert_eq!(
        of_type(&events, "turn.completed")[0].payload["state"],
        "completed"
    );
    runtime.stop().await.expect("stop");
}

/// OpenCode can settle a reply whose HTTP response is then lost; the settlement it already sent
/// counts, so the request does not stay pending forever.
#[tokio::test]
async fn a_reply_whose_response_is_lost_still_settles_the_question() {
    let (endpoint, replay) = serve("question").await;
    let runtime = v2_runtime(&endpoint);
    runtime.start().await.expect("session starts");
    runtime
        .send_turn(Some("ASK_QUESTION please"), Vec::new(), None)
        .await
        .expect("prompt is accepted");
    let requested = wait_for(&runtime, "user-input.requested").await;
    replay
        .lose_next_reply_response
        .store(true, Ordering::SeqCst);
    runtime
        .respond_to_user_input(
            requested.request_id.as_deref().unwrap(),
            json!({ "q0": ["Blue"] }),
        )
        .await
        .expect("OpenCode settled the form, so the reply succeeded");
    let mut events = until_turns_complete(&runtime, 1).await;
    events.extend(drain(&runtime).await);
    let resolved = of_type(&events, "user-input.resolved");
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].payload["answers"], json!({ "q0": ["Blue"] }));
}

/// After a restart, replies to asks OpenCode is still waiting on are routed to the session that
/// asked, although the event stream does not replay them.
#[tokio::test]
async fn a_resumed_session_can_answer_the_asks_it_was_waiting_on() {
    for (scenario, reply) in [("shell", "permission"), ("question", "form")] {
        let (endpoint, replay) = serve(scenario).await;
        let runtime = v2_runtime(&endpoint);
        runtime.resume(&replay.root).await.expect("resumes");
        let pending = exchange(scenario, "GET", &format!("/{reply}")).1;
        let ask = pending["data"][0]["id"]
            .as_str()
            .expect("pending ask")
            .to_owned();
        if reply == "permission" {
            runtime
                .respond_to_permission(&ask, "accept")
                .await
                .expect("the restored ask can be answered");
        } else {
            runtime
                .respond_to_user_input(&ask, json!({ "q0": ["Red"] }))
                .await
                .expect("the restored form can be answered");
        }
        let replies = replay.requests_to("/reply");
        assert_eq!(
            replies[0].1.split('?').next().unwrap(),
            format!("/api/session/{}/{reply}/{ask}/reply", replay.root),
            "{scenario}"
        );
    }
}

/// A session OpenCode kept executing while BiBCode was away is reported as a running turn on
/// resume, so its completion is not lost.
#[tokio::test]
async fn a_resumed_session_that_is_still_executing_gets_a_turn() {
    let (endpoint, replay) = serve("hello").await;
    replay.active.store(true, Ordering::SeqCst);
    let runtime = v2_runtime(&endpoint);
    runtime.resume(&replay.root).await.expect("resumes");
    let started = wait_for(&runtime, "turn.started").await;
    assert!(started.turn_id.is_some());
}
