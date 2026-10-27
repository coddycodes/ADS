use std::{
    io,
    sync::{Arc, Mutex as StdMutex},
};

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::State,
    http::{HeaderMap, StatusCode, header::CONTENT_TYPE},
    response::IntoResponse,
    routing::post,
};
use serde_json::{Value, json};
use subtle::ConstantTimeEq;
use tokio::{net::TcpListener, sync::mpsc, task::JoinHandle};
use tokio_util::sync::CancellationToken;

pub(crate) const CLAUDE_HOOK_TOKEN_ENV: &str = "CODDY_STUDIO_CLAUDE_HOOK_TOKEN";
const CLAUDE_HOOK_BODY_LIMIT: usize = 64 * 1024;
const CLAUDE_HOOK_CHANNEL_CAPACITY: usize = 128;
const CLAUDE_HOOK_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Clone)]
struct HookState {
    token: Arc<String>,
    sender: mpsc::Sender<Value>,
}

pub(crate) struct ClaudeHookSinkLaunch {
    pub(crate) endpoint: String,
    pub(crate) token: String,
    pub(crate) receiver: mpsc::Receiver<Value>,
    pub(crate) handle: ClaudeHookSinkHandle,
}

pub(crate) struct ClaudeHookSinkHandle {
    cancellation: CancellationToken,
    task: StdMutex<Option<JoinHandle<()>>>,
}

impl ClaudeHookSinkHandle {
    pub(crate) async fn shutdown(&self) {
        self.cancellation.cancel();
        let task = self
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }
}

impl Drop for ClaudeHookSinkHandle {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Some(task) = self
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            task.abort();
        }
    }
}

pub(crate) async fn start_claude_hook_sink() -> io::Result<ClaudeHookSinkLaunch> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let token = random_token()?;
    let (sender, receiver) = mpsc::channel(CLAUDE_HOOK_CHANNEL_CAPACITY);
    let state = HookState {
        token: Arc::new(token.clone()),
        sender,
    };
    let cancellation = CancellationToken::new();
    let server_cancellation = cancellation.clone();
    let router = Router::new()
        .route("/claude-hook", post(capture_hook))
        .with_state(state);
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(server_cancellation.cancelled_owned())
            .await;
    });
    Ok(ClaudeHookSinkLaunch {
        endpoint: format!("http://{address}/claude-hook"),
        token,
        receiver,
        handle: ClaudeHookSinkHandle {
            cancellation,
            task: StdMutex::new(Some(task)),
        },
    })
}

pub(crate) fn claude_hook_settings(endpoint: &str) -> Value {
    let handler = json!({
        "type": "http",
        "url": endpoint,
        "timeout": 1,
        "headers": {
            "Authorization": format!("Bearer ${CLAUDE_HOOK_TOKEN_ENV}")
        },
        "allowedEnvVars": [CLAUDE_HOOK_TOKEN_ENV]
    });
    let hook = || json!([{ "hooks": [handler.clone()] }]);
    json!({
        "hooks": {
            "SubagentStart": hook(),
            "SubagentStop": hook(),
            "PreToolUse": hook(),
            "PostToolUse": hook(),
            "PostToolUseFailure": hook()
        }
    })
}

async fn capture_hook(
    State(state): State<HookState>,
    headers: HeaderMap,
    body: Body,
) -> impl IntoResponse {
    if !authorized(&headers, &state.token) {
        return StatusCode::FORBIDDEN;
    }
    if !headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
    {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE;
    }
    let Ok(Ok(bytes)) = tokio::time::timeout(
        CLAUDE_HOOK_REQUEST_TIMEOUT,
        to_bytes(body, CLAUDE_HOOK_BODY_LIMIT),
    )
    .await
    else {
        return StatusCode::PAYLOAD_TOO_LARGE;
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return StatusCode::BAD_REQUEST;
    };
    if !value.is_object() {
        return StatusCode::BAD_REQUEST;
    }
    match state.sender.try_send(value) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(mpsc::error::TrySendError::Full(_)) => StatusCode::SERVICE_UNAVAILABLE,
        Err(mpsc::error::TrySendError::Closed(_)) => StatusCode::GONE,
    }
}

fn authorized(headers: &HeaderMap, expected_token: &str) -> bool {
    let Some(provided) = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return false;
    };
    provided.len() == expected_token.len()
        && bool::from(provided.as_bytes().ct_eq(expected_token.as_bytes()))
}

fn random_token() -> io::Result<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(io::Error::other)?;
    Ok(bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>())
}
