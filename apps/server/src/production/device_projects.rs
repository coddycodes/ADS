//! First-run alignment with the Coddy Device Agent's project registry.
//!
//! The agent owns the device's project list (`/api/v1/servcortex/projects`,
//! unauthenticated on loopback). Studio's project store is separate, so a fresh
//! Studio install imports the agent's projects once. Existing project state is
//! never rewritten or pruned; the imported rows are ordinary projects from then
//! on.

use std::{
    collections::HashSet,
    path::Path,
    time::Duration,
};

use serde_json::Value;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

use crate::{
    orchestration::{OrchestrationCommand, OrchestrationEngine, engine::OptionalNullable},
    server_settings::policy,
};

const DEVICE_AGENT_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_IMPORTED_PROJECTS: usize = 200;

pub(crate) async fn sync_device_projects(engine: &OrchestrationEngine, state_dir: &Path) {
    // Test runtimes use temporary state roots; never let a developer's running
    // agent inject projects into their databases.
    if state_dir.starts_with(std::env::temp_dir()) {
        return;
    }
    let policy = policy::current();
    let base = policy.device_agent.url.trim_end_matches('/');
    let Ok(client) = reqwest::Client::builder()
        .timeout(DEVICE_AGENT_TIMEOUT)
        .build()
    else {
        return;
    };

    let health = client
        .get(format!("{base}/api/v1/agent/health"))
        .send()
        .await;
    if !matches!(health, Ok(response) if response.status().is_success()) {
        tracing::warn!(
            url = %base,
            "Coddy Device Agent is not reachable; Coddy turns will be refused until it is installed and running"
        );
        return;
    }

    let Ok(response) = client
        .get(format!("{base}/api/v1/servcortex/projects"))
        .send()
        .await
    else {
        return;
    };
    if !response.status().is_success() {
        return;
    }
    let Ok(body) = response.json::<Value>().await else {
        return;
    };
    let Some(projects) = body.get("projects").and_then(Value::as_array) else {
        return;
    };
    let Ok(existing) = engine.repositories().list_projects().await else {
        return;
    };
    if !existing.is_empty() {
        return;
    }

    let created_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned());
    let mut seen = HashSet::new();

    for project in projects.iter().take(MAX_IMPORTED_PROJECTS) {
        let Some(path) = project
            .get("path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|path| !path.is_empty())
        else {
            continue;
        };
        if !Path::new(path).is_dir() || !seen.insert(path.to_owned()) {
            continue;
        }
        let title = project
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                Path::new(path)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| path.to_owned());

        let command = OrchestrationCommand::ProjectCreate {
            command_id: format!("device-project-{}", Uuid::new_v4()),
            project_id: Uuid::new_v4().to_string(),
            title,
            workspace_root: path.to_owned(),
            create_workspace_root_if_missing: Some(false),
            initialize_git: Some(false),
            default_model_selection: OptionalNullable::Missing,
            created_at: created_at.clone(),
        };
        if let Err(error) = engine.dispatch(command).await {
            tracing::debug!(%error, %path, "device agent project import skipped");
        }
    }
}
