use coddy_studio_server::orchestration::engine::{
    EngineOptions, OrchestrationCommand, OrchestrationEngine, TestHooks,
};
use coddy_studio_server::persistence::{Database, Repositories, run_migrations};
use serde_json::{Value, json};
const CREATED_AT: &str = "2026-07-10T10:00:00.000Z";
fn decode(value: Value) -> OrchestrationCommand {
    serde_json::from_value(value).unwrap()
}

pub async fn queue_engine(hooks: TestHooks) -> OrchestrationEngine {
    let database = Database::open_in_memory().await.unwrap();
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .unwrap();
    let repositories = Repositories::new(database);
    let engine = OrchestrationEngine::start(
        repositories.database().clone(),
        EngineOptions {
            test_hooks: hooks,
            ..EngineOptions::default()
        },
    )
    .await
    .unwrap();
    for value in [
        json!({"type":"project.create", "commandId":"queue-project", "projectId":"p1", "title":"Queue", "workspaceRoot":"C:/repo", "createdAt":CREATED_AT}),
        json!({"type":"thread.create", "commandId":"queue-thread", "threadId":"t1", "projectId":"p1", "title":"Queue", "runtimeMode":"full-access", "modelSelection":{"instanceId":"codex", "model":"saved-model"}, "createdAt":CREATED_AT}),
    ] {
        engine.dispatch(decode(value)).await.unwrap();
    }
    engine
}

pub async fn seed_queued_message(engine: &OrchestrationEngine, id: &str) -> Value {
    let payload = json!({
        "type":"thread.turn.start", "commandId":id, "threadId":"t1",
        "message":{"messageId":id, "role":"user", "text":"saved prompt", "attachments":[]},
        "modelSelection":{"instanceId":"codex", "model":"saved-model", "options":{"reasoningEffort":"high"}},
        "runtimeMode":"full-access", "interactionMode":"default", "titleSeed":"Saved title",
        "queued":true, "createdAt":CREATED_AT
    });
    engine.dispatch(decode(payload.clone())).await.unwrap();
    let stored = payload.clone();
    let command_id = id.to_owned();
    engine.repositories().database().call(move |connection| {
        connection.execute(
            "INSERT INTO provider_turn_outbox (command_id, thread_id, message_id, provider_instance_id, provider_kind, delivery_key, payload_json, state, mode, held, created_at, updated_at) VALUES (?, 't1', ?, 'codex', 'codex', ?, ?, 'queued', 'start', 1, ?, ?)",
            rusqlite::params![command_id, command_id, command_id, stored.to_string(), CREATED_AT, CREATED_AT],
        )?;
        Ok(())
    }).await.unwrap();
    let mut message = engine
        .repositories()
        .get_message(id.to_owned())
        .await
        .unwrap()
        .unwrap();
    message.delivery_state = Some("queued".into());
    message.delivery_provider = Some("codex".into());
    message.delivery_mode = Some("start".into());
    message.delivery_held = Some(true);
    engine.repositories().upsert_message(message).await.unwrap();
    payload
}
