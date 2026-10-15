//! What agent turns leave on the heap while the shell and thread streams deliver them (#101):
//! a thread subscription held the whole thread as JSON for as long as it stayed open, so an
//! open thread grew the server by about twice its content. Its own binary, since it counts
//! every allocation the process makes.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering},
    },
    time::Duration,
};

use bibcode_server::{
    RpcRegistry, ServerConfig, ServerMessage, ServerRuntime,
    orchestration::{EngineOptions, OrchestrationEngine},
    persistence::{Database, run_migrations},
    production::orchestration_rpc::register_orchestration_rpc,
};
use futures_util::SinkExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio_tungstenite::{connect_async, tungstenite::Message};

#[path = "support/websocket_frames.rs"]
mod websocket_frames;
use websocket_frames::next_frame_past_heartbeat;

struct Counting;

static LIVE_BYTES: AtomicIsize = AtomicIsize::new(0);

// SAFETY: forwards to the system allocator unchanged and only counts sizes.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LIVE_BYTES.fetch_add(layout.size() as isize, Ordering::Relaxed);
        // SAFETY: the caller's layout contract carries over.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE_BYTES.fetch_sub(layout.size() as isize, Ordering::Relaxed);
        // SAFETY: `ptr` came from `alloc` with this layout.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        LIVE_BYTES.fetch_add(
            new_size as isize - layout.size() as isize,
            Ordering::Relaxed,
        );
        // SAFETY: the caller's contract carries over.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const NOW: &str = "2026-10-14T10:00:00.000Z";
const DELTAS_PER_TURN: usize = 400;
const DELTA_TEXT: usize = 120;
const ACTIVITIES_PER_TURN: usize = 40;
/// Until the connections' buffers have grown to the largest update they carry.
const WARM_UP_TURNS: usize = 25;
const MEASURED_TURNS: usize = 10;

/// Reads a stream's chunks and acknowledges each, as the web client does, until `stop`. A
/// stream that fails or ends first fails the test: a subscription that is gone holds nothing.
async fn subscribe(
    address: std::net::SocketAddr,
    tag: &'static str,
    payload: Value,
    chunks: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
) {
    let (mut socket, _) = connect_async(format!("ws://{address}/ws")).await.unwrap();
    let request =
        json!({ "_tag": "Request", "id": "1", "tag": tag, "payload": payload, "headers": [] });
    socket
        .send(Message::Text(request.to_string().into()))
        .await
        .unwrap();
    while !stop.load(Ordering::Relaxed) {
        let frame = match tokio::time::timeout(
            Duration::from_millis(100),
            next_frame_past_heartbeat(&mut socket),
        )
        .await
        {
            Err(_) => continue,
            Ok(Some(Ok(Message::Text(text)))) => text,
            Ok(other) => panic!("{tag} closed: {other:?}"),
        };
        match serde_json::from_str::<ServerMessage>(&frame) {
            Ok(ServerMessage::Chunk { values, .. }) => {
                let first = chunks.fetch_add(1, Ordering::Relaxed) == 0;
                assert!(
                    !first
                        || values
                            .first()
                            .is_some_and(|value| value["kind"] == "snapshot"),
                    "{tag} starts with a snapshot: {values:?}"
                );
                let ack = json!({ "_tag": "Ack", "requestId": "1" });
                socket
                    .send(Message::Text(ack.to_string().into()))
                    .await
                    .unwrap();
            }
            other => panic!("{tag} ended: {other:?}"),
        }
    }
}

async fn dispatch(engine: &OrchestrationEngine, command: Value) {
    engine
        .dispatch(serde_json::from_value(command).expect("command decodes"))
        .await
        .expect("command dispatches");
}

/// One agent turn: streamed assistant text and tool activities, then the completion.
async fn turn(engine: &OrchestrationEngine, n: usize) {
    let message = format!("message-{n}");
    let turn = format!("turn-{n}");
    for delta in 0..DELTAS_PER_TURN {
        dispatch(
            engine,
            json!({
                "type": "thread.message.assistant.delta",
                "commandId": format!("delta-{n}-{delta}"),
                "threadId": "thread",
                "messageId": message,
                "delta": "x".repeat(DELTA_TEXT),
                "turnId": turn,
                "createdAt": NOW,
            }),
        )
        .await;
        if delta % (DELTAS_PER_TURN / ACTIVITIES_PER_TURN) == 0 {
            let id = format!("activity-{n}-{delta}");
            dispatch(
                engine,
                json!({
                    "type": "thread.activity.append",
                    "commandId": format!("append-{id}"),
                    "threadId": "thread",
                    "activity": {
                        "id": id,
                        "tone": "tool",
                        "kind": "tool.completed",
                        "summary": "Ran a command",
                        "payload": { "detail": "y".repeat(400) },
                        "turnId": turn,
                        "createdAt": NOW,
                    },
                    "createdAt": NOW,
                }),
            )
            .await;
        }
    }
    dispatch(
        engine,
        json!({
            "type": "thread.message.assistant.complete",
            "commandId": format!("complete-{n}"),
            "threadId": "thread",
            "messageId": message,
            "turnId": turn,
            "createdAt": NOW,
        }),
    )
    .await;
}

/// Waits until neither stream has delivered anything for a while.
async fn settle(chunks: &[Arc<AtomicU64>]) {
    let total = || {
        chunks
            .iter()
            .map(|c| c.load(Ordering::Relaxed))
            .sum::<u64>()
    };
    let mut last = total();
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let now = total();
        if now == last {
            return;
        }
        last = now;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delivered_turns_leave_only_their_content_on_the_heap() {
    let temp = TempDir::new().unwrap();
    let database = Database::open_in_memory().await.unwrap();
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .unwrap();
    let engine = OrchestrationEngine::start(database, EngineOptions::default())
        .await
        .unwrap();
    let mut registry = RpcRegistry::empty();
    register_orchestration_rpc(&mut registry, engine.clone());
    let config = ServerConfig::new(temp.path())
        .with_bind("127.0.0.1", 0)
        .with_unsafe_no_auth();
    let handle = ServerRuntime::start_with_registry(config, registry)
        .await
        .unwrap();
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    dispatch(
        &engine,
        json!({
            "type": "project.create", "commandId": "create-project", "projectId": "project",
            "title": "Project", "workspaceRoot": workspace.to_string_lossy().replace('\\', "/"),
            "defaultModelSelection": null, "createdAt": NOW,
        }),
    )
    .await;
    dispatch(
        &engine,
        json!({
            "type": "thread.create", "commandId": "create-thread", "threadId": "thread",
            "projectId": "project", "title": "Thread",
            "modelSelection": {"instanceId": "codex", "model": "gpt-5"},
            "runtimeMode": "full-access", "interactionMode": "default", "createdAt": NOW,
        }),
    )
    .await;

    let stop = Arc::new(AtomicBool::new(false));
    let chunks = [Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0))];
    let address = handle.local_addr();
    // As the web client subscribes: the shell, and the open thread with patches.
    let subscribers = [
        tokio::spawn(subscribe(
            address,
            "orchestration.subscribeShell",
            json!({}),
            chunks[0].clone(),
            stop.clone(),
        )),
        tokio::spawn(subscribe(
            address,
            "orchestration.subscribeThread",
            json!({ "threadId": "thread", "patches": true }),
            chunks[1].clone(),
            stop.clone(),
        )),
    ];

    // Warm-up: stream buffers grow to the largest update they carry, then stay.
    for n in 0..WARM_UP_TURNS {
        turn(&engine, n).await;
    }
    settle(&chunks).await;
    let warm = LIVE_BYTES.load(Ordering::Relaxed);
    let thread_updates = chunks[1].load(Ordering::Relaxed);
    for n in WARM_UP_TURNS..WARM_UP_TURNS + MEASURED_TURNS {
        turn(&engine, n).await;
    }
    settle(&chunks).await;
    let per_turn = (LIVE_BYTES.load(Ordering::Relaxed) - warm) / MEASURED_TURNS as isize;
    assert!(
        chunks[1].load(Ordering::Relaxed) > thread_updates,
        "the thread stream delivered the measured turns"
    );
    assert!(
        subscribers
            .iter()
            .all(|subscriber| !subscriber.is_finished()),
        "both subscriptions are open while measured"
    );
    eprintln!(
        "{per_turn} B per turn kept; {} shell and {} thread updates",
        chunks[0].load(Ordering::Relaxed),
        chunks[1].load(Ordering::Relaxed),
    );

    stop.store(true, Ordering::Relaxed);
    for subscriber in subscribers {
        subscriber.await.unwrap();
    }
    handle.shutdown();
    handle.join().await.unwrap();
    engine.shutdown().await;

    // A turn's 40 activities and one message now cost 8 bytes each to track; holding the
    // thread cost about 120 KB.
    assert!(
        per_turn < 4 * 1024,
        "each delivered turn keeps {per_turn} B while its thread stays open"
    );
}
