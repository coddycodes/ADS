use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::{fs, io::AsyncWriteExt, sync::Mutex};
use uuid::Uuid;

pub(crate) mod policy;

/// The one source both settings readers fill missing built-in provider fields from.
pub(crate) fn built_in_provider_defaults() -> Value {
    serde_json::json!({
        "codex": { "enabled": false, "binaryPath": "codex", "homePath": "", "shadowHomePath": "", "customModels": [] },
        "claudeAgent": { "enabled": false, "binaryPath": "claude", "homePath": "", "customModels": [], "launchArgs": "" },
        "coddy": { "enabled": true, "binaryPath": "coddy", "serverUrl": "", "dataDir": "" },
        "cursor": { "enabled": false, "binaryPath": "cursor-agent", "apiEndpoint": "", "customModels": [] },
        "grok": { "enabled": false, "binaryPath": "grok", "customModels": [] },
        "opencode": { "enabled": false, "binaryPath": "opencode", "serverUrl": "", "serverPassword": "", "customModels": [] },
    })
}

/// Recursively fills missing keys in `target` from `defaults` without overwriting existing values.
pub(crate) fn merge_missing(target: &mut Value, defaults: &Value) {
    if let (Some(target), Some(defaults)) = (target.as_object_mut(), defaults.as_object()) {
        for (key, default) in defaults {
            match target.get_mut(key) {
                Some(value) if value.is_object() && default.is_object() => {
                    merge_missing(value, default)
                }
                Some(_) => {}
                None => {
                    target.insert(key.clone(), default.clone());
                }
            }
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderBinarySettingsState {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub binary_path: String,
    #[serde(default)]
    pub server_url: String,
    #[serde(default)]
    pub server_password: String,
    /// Optional data root for drivers whose CLI accepts one (Coddy's `-D`).
    #[serde(default)]
    pub data_dir: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CoddyProviderSettingsState {
    pub enabled: bool,
    pub binary_path: String,
    pub server_url: String,
    pub data_dir: String,
}

impl Default for CoddyProviderSettingsState {
    fn default() -> Self {
        Self {
            enabled: true,
            binary_path: "coddy".to_owned(),
            server_url: String::new(),
            data_dir: String::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ProvidersState {
    pub codex: ProviderBinarySettingsState,
    pub claude_agent: ProviderBinarySettingsState,
    pub coddy: CoddyProviderSettingsState,
    pub cursor: ProviderBinarySettingsState,
    pub grok: ProviderBinarySettingsState,
    pub opencode: ProviderBinarySettingsState,
}

impl ProvidersState {
    fn with_defaults() -> Self {
        let mut defaults = built_in_provider_defaults();
        // Decode entries separately: ProvidersState's container-level serde
        // default would recurse back here even for a complete document.
        let coddy = serde_json::from_value(defaults["coddy"].take())
            .expect("built-in coddy defaults must decode");
        let mut provider = |driver: &str| {
            serde_json::from_value(defaults[driver].take())
                .expect("built-in provider defaults must decode")
        };
        Self {
            codex: provider("codex"),
            claude_agent: provider("claudeAgent"),
            coddy,
            cursor: provider("cursor"),
            grok: provider("grok"),
            opencode: provider("opencode"),
        }
    }
}

impl Default for ProvidersState {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderEnvironmentVariableState {
    pub name: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub sensitive: bool,
    #[serde(default)]
    pub value_redacted: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInstanceState {
    pub driver: String,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub environment: Vec<ProviderEnvironmentVariableState>,
    #[serde(default)]
    pub config: Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProviderOptionSelectionValueState {
    String(String),
    Boolean(bool),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderOptionSelectionState {
    pub id: String,
    pub value: ProviderOptionSelectionValueState,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSessionDefaultState {
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<ProviderOptionSelectionState>>,
}

const fn enabled_by_default() -> bool {
    true
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ProviderSettingsState {
    #[serde(default = "enabled_by_default")]
    pub enable_chat_agent_activity: bool,
    #[serde(default)]
    pub enable_terminal_agent_activity: bool,
    #[serde(default = "default_git_fetch_interval")]
    pub automatic_git_fetch_interval: u64,
    #[serde(default)]
    pub add_project_base_directory: String,
    #[serde(default)]
    pub worktree_base_directory: String,
    #[serde(default)]
    pub providers: ProvidersState,
    #[serde(default)]
    pub provider_instances: BTreeMap<String, ProviderInstanceState>,
    #[serde(default)]
    pub provider_session_defaults: BTreeMap<String, ProviderSessionDefaultState>,
}

impl Default for ProviderSettingsState {
    fn default() -> Self {
        Self {
            enable_chat_agent_activity: true,
            enable_terminal_agent_activity: false,
            automatic_git_fetch_interval: default_git_fetch_interval(),
            add_project_base_directory: String::new(),
            worktree_base_directory: String::new(),
            providers: ProvidersState::default(),
            provider_instances: BTreeMap::new(),
            provider_session_defaults: BTreeMap::new(),
        }
    }
}

const fn default_git_fetch_interval() -> u64 {
    180_000
}

#[derive(Clone, Debug, Default)]
pub struct ProviderSettingsPatch {
    pub enabled: Option<bool>,
    pub binary_path: Option<String>,
    pub server_url: Option<String>,
    pub server_password: Option<String>,
    pub data_dir: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ProvidersPatch {
    pub codex: Option<ProviderSettingsPatch>,
    pub claude_agent: Option<ProviderSettingsPatch>,
    pub coddy: Option<ProviderSettingsPatch>,
    pub cursor: Option<ProviderSettingsPatch>,
    pub grok: Option<ProviderSettingsPatch>,
    pub opencode: Option<ProviderSettingsPatch>,
}

#[derive(Clone, Debug, Default)]
pub struct ProviderEnvironmentVariableInput {
    pub name: String,
    pub value: String,
    pub sensitive: bool,
    pub value_redacted: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ProviderInstanceInput {
    pub driver: String,
    pub enabled: bool,
    pub display_name: Option<String>,
    pub environment: Vec<ProviderEnvironmentVariableInput>,
    pub config: Value,
}

#[derive(Clone, Debug, Default)]
pub struct ServerSettingsPatch {
    pub enable_chat_agent_activity: Option<bool>,
    pub enable_terminal_agent_activity: Option<bool>,
    pub automatic_git_fetch_interval_ms: Option<u64>,
    pub add_project_base_directory: Option<String>,
    pub worktree_base_directory: Option<String>,
    pub providers: Option<ProvidersPatch>,
    pub provider_instances: Option<BTreeMap<String, ProviderInstanceInput>>,
    pub provider_session_defaults: Option<BTreeMap<String, ProviderSessionDefaultState>>,
}

#[derive(Debug, Error)]
pub enum ServerSettingsReadError {
    #[error("failed to read settings from {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to decode settings from {path}")]
    Decode {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to persist settings to {path}")]
    Persist {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "missing secret for provider instance {instance_id} and environment variable {environment_variable}"
    )]
    MissingSecret {
        instance_id: String,
        environment_variable: String,
        path: PathBuf,
    },
}

#[derive(Clone)]
pub struct ProviderSettingsStore {
    root: PathBuf,
    lock: ArcMutex,
}

type ArcMutex = std::sync::Arc<Mutex<()>>;

impl ProviderSettingsStore {
    #[must_use]
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
            lock: std::sync::Arc::new(Mutex::new(())),
        }
    }

    pub async fn get(&self) -> Result<ProviderSettingsState, ServerSettingsReadError> {
        let _guard = self.lock.lock().await;
        let persisted = self.read_persisted().await?;
        self.materialize_secrets(persisted).await
    }

    pub async fn update(
        &self,
        patch: ServerSettingsPatch,
    ) -> Result<ProviderSettingsState, ServerSettingsReadError> {
        let _guard = self.lock.lock().await;
        let current = self.read_persisted().await?;
        let next = apply_patch(current, patch);
        let materialized = self.materialize_and_persist(next).await?;
        Ok(materialized)
    }

    fn settings_path(&self) -> PathBuf {
        self.root.join("settings.json")
    }

    fn secrets_dir(&self) -> PathBuf {
        self.root.join("secrets")
    }

    /// The settings document as persisted, read without its secrets: a sensitive environment
    /// value stays redacted. It also keeps the fields the typed state leaves out, such as a
    /// provider's custom models. A missing file reads as an empty document.
    pub(crate) async fn get_document(&self) -> Result<Value, ServerSettingsReadError> {
        let _guard = self.lock.lock().await;
        let Some(bytes) = self.read_bytes().await? else {
            return Ok(Value::Object(serde_json::Map::new()));
        };
        serde_json::from_slice(&bytes).map_err(|source| ServerSettingsReadError::Decode {
            path: self.settings_path(),
            source,
        })
    }

    /// The settings file's bytes, or `None` when it does not exist.
    async fn read_bytes(&self) -> Result<Option<Vec<u8>>, ServerSettingsReadError> {
        let path = self.settings_path();
        match fs::read(&path).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(ServerSettingsReadError::Read { path, source }),
        }
    }

    async fn read_persisted(&self) -> Result<ProviderSettingsState, ServerSettingsReadError> {
        let path = self.settings_path();
        let Some(bytes) = self.read_bytes().await? else {
            return Ok(ProviderSettingsState::default());
        };
        let mut settings =
            serde_json::from_slice(&bytes).map_err(|source| ServerSettingsReadError::Decode {
                path: path.clone(),
                source,
            })?;
        normalize_agent_activity_settings(&mut settings);
        if let Some(providers) = settings.get_mut("providers") {
            merge_missing(providers, &built_in_provider_defaults());
        }
        let mut settings: ProviderSettingsState = serde_json::from_value(settings)
            .map_err(|source| ServerSettingsReadError::Decode { path, source })?;
        let policy = policy::StudioPolicy::load(&self.root);
        policy.apply_enablement(&mut settings.providers);
        policy::init(policy);
        Ok(settings)
    }

    async fn materialize_and_persist(
        &self,
        mut settings: ProviderSettingsState,
    ) -> Result<ProviderSettingsState, ServerSettingsReadError> {
        fs::create_dir_all(self.secrets_dir())
            .await
            .map_err(|source| ServerSettingsReadError::Persist {
                path: self.secrets_dir(),
                source,
            })?;

        let mut persisted = settings.clone();
        for (instance_id, instance) in &mut settings.provider_instances {
            let Some(persisted_instance) = persisted.provider_instances.get_mut(instance_id) else {
                continue;
            };
            for (index, variable) in instance.environment.iter_mut().enumerate() {
                let persisted_variable = &mut persisted_instance.environment[index];
                if !variable.sensitive {
                    persisted_variable.value_redacted = false;
                    variable.value_redacted = false;
                    continue;
                }
                if !variable.value.is_empty() {
                    let secret_path = self.secret_path(instance_id, &variable.name);
                    write_bytes_atomically(&secret_path, variable.value.as_bytes())
                        .await
                        .map_err(|source| ServerSettingsReadError::Persist {
                            path: secret_path.clone(),
                            source,
                        })?;
                    persisted_variable.value.clear();
                    persisted_variable.value_redacted = true;
                    variable.value_redacted = false;
                    continue;
                }
                if variable.value_redacted {
                    let secret_path = self.secret_path(instance_id, &variable.name);
                    let secret = fs::read_to_string(&secret_path).await.map_err(|source| {
                        match source.kind() {
                            std::io::ErrorKind::NotFound => {
                                ServerSettingsReadError::MissingSecret {
                                    instance_id: instance_id.clone(),
                                    environment_variable: variable.name.clone(),
                                    path: secret_path.clone(),
                                }
                            }
                            _ => ServerSettingsReadError::Read {
                                path: secret_path.clone(),
                                source,
                            },
                        }
                    })?;
                    variable.value = secret;
                    variable.value_redacted = false;
                    persisted_variable.value.clear();
                    persisted_variable.value_redacted = true;
                }
            }
        }

        let persisted_value =
            serde_json::to_value(&persisted).map_err(|source| ServerSettingsReadError::Decode {
                path: self.settings_path(),
                source,
            })?;
        let defaults =
            serde_json::to_value(ProviderSettingsState::default()).map_err(|source| {
                ServerSettingsReadError::Decode {
                    path: self.settings_path(),
                    source,
                }
            })?;
        let stripped = strip_defaults(&persisted_value, &defaults)
            .unwrap_or(Value::Object(Default::default()));
        write_json_atomically(&self.settings_path(), &stripped).await?;
        Ok(settings)
    }

    async fn materialize_secrets(
        &self,
        mut settings: ProviderSettingsState,
    ) -> Result<ProviderSettingsState, ServerSettingsReadError> {
        for (instance_id, instance) in &mut settings.provider_instances {
            for variable in &mut instance.environment {
                if variable.sensitive && variable.value_redacted {
                    let secret_path = self.secret_path(instance_id, &variable.name);
                    variable.value = fs::read_to_string(&secret_path).await.map_err(|source| {
                        match source.kind() {
                            std::io::ErrorKind::NotFound => {
                                ServerSettingsReadError::MissingSecret {
                                    instance_id: instance_id.clone(),
                                    environment_variable: variable.name.clone(),
                                    path: secret_path.clone(),
                                }
                            }
                            _ => ServerSettingsReadError::Read {
                                path: secret_path.clone(),
                                source,
                            },
                        }
                    })?;
                    variable.value_redacted = false;
                }
            }
        }
        Ok(settings)
    }

    fn secret_path(&self, instance_id: &str, name: &str) -> PathBuf {
        self.secrets_dir().join(format!(
            "provider-env-{}-{}",
            URL_SAFE_NO_PAD.encode(instance_id),
            URL_SAFE_NO_PAD.encode(name)
        ))
    }
}

fn apply_patch(
    mut current: ProviderSettingsState,
    patch: ServerSettingsPatch,
) -> ProviderSettingsState {
    if let Some(value) = patch.enable_chat_agent_activity {
        current.enable_chat_agent_activity = value;
    }
    if let Some(value) = patch.enable_terminal_agent_activity {
        current.enable_terminal_agent_activity = value;
    }
    if let Some(value) = patch.automatic_git_fetch_interval_ms {
        current.automatic_git_fetch_interval = value;
    }
    if let Some(value) = patch.add_project_base_directory {
        current.add_project_base_directory = value.trim().to_owned();
    }
    if let Some(value) = patch.worktree_base_directory {
        current.worktree_base_directory = value.trim().to_owned();
    }
    if let Some(providers) = patch.providers {
        apply_provider_patch(&mut current.providers.codex, providers.codex, "codex");
        apply_provider_patch(
            &mut current.providers.claude_agent,
            providers.claude_agent,
            "claude",
        );
        apply_coddy_patch(&mut current.providers.coddy, providers.coddy);
        apply_provider_patch(
            &mut current.providers.cursor,
            providers.cursor,
            "cursor-agent",
        );
        apply_provider_patch(&mut current.providers.grok, providers.grok, "grok");
        apply_provider_patch(
            &mut current.providers.opencode,
            providers.opencode,
            "opencode",
        );
    }
    if let Some(instances) = patch.provider_instances {
        current.provider_instances = instances
            .into_iter()
            .map(|(instance_id, instance)| {
                (
                    instance_id,
                    ProviderInstanceState {
                        driver: instance.driver,
                        enabled: instance.enabled,
                        display_name: instance.display_name,
                        environment: instance
                            .environment
                            .into_iter()
                            .map(|variable| ProviderEnvironmentVariableState {
                                name: variable.name,
                                value: variable.value,
                                sensitive: variable.sensitive,
                                value_redacted: variable.value_redacted,
                            })
                            .collect(),
                        config: instance.config,
                    },
                )
            })
            .collect();
    }
    if let Some(defaults) = patch.provider_session_defaults {
        current.provider_session_defaults = defaults;
    }
    current
}

pub(crate) fn normalize_agent_activity_settings(settings: &mut Value) {
    let Some(object) = settings.as_object_mut() else {
        return;
    };
    let legacy = object.remove("enableAgentActivity");
    if !object.contains_key("enableChatAgentActivity") {
        object.insert(
            "enableChatAgentActivity".to_owned(),
            legacy.unwrap_or(Value::Bool(true)),
        );
    }
    object
        .entry("enableTerminalAgentActivity")
        .or_insert(Value::Bool(false));
}

fn apply_provider_patch(
    current: &mut ProviderBinarySettingsState,
    patch: Option<ProviderSettingsPatch>,
    default_binary: &str,
) {    let Some(patch) = patch else {
        return;
    };
    if let Some(enabled) = patch.enabled {
        current.enabled = enabled;
    }
    if let Some(binary_path) = patch.binary_path {
        let trimmed = binary_path.trim();
        current.binary_path = if trimmed.is_empty() {
            default_binary.to_owned()
        } else {
            trimmed.to_owned()
        };
    }
    if let Some(server_url) = patch.server_url {
        current.server_url = server_url.trim().to_owned();
    }
    if let Some(server_password) = patch.server_password {
        current.server_password = server_password.trim().to_owned();
    }
}

fn apply_coddy_patch(
    current: &mut CoddyProviderSettingsState,
    patch: Option<ProviderSettingsPatch>,
) {
    let Some(patch) = patch else {
        return;
    };
    if let Some(enabled) = patch.enabled {
        current.enabled = enabled;
    }
    if let Some(binary_path) = patch.binary_path {
        let trimmed = binary_path.trim();
        current.binary_path = if trimmed.is_empty() {
            "coddy".to_owned()
        } else {
            trimmed.to_owned()
        };
    }
    if let Some(server_url) = patch.server_url {
        current.server_url = server_url.trim().to_owned();
    }
    if let Some(data_dir) = patch.data_dir {
        current.data_dir = data_dir.trim().to_owned();
    }
}

fn strip_defaults(current: &Value, defaults: &Value) -> Option<Value> {
    match (current, defaults) {
        (Value::Object(current), Value::Object(defaults)) => {
            let mut next = serde_json::Map::new();
            for (key, value) in current {
                let stripped = strip_defaults(value, defaults.get(key).unwrap_or(&Value::Null));
                if let Some(value) = stripped {
                    next.insert(key.clone(), value);
                }
            }
            (!next.is_empty()).then_some(Value::Object(next))
        }
        (Value::Array(_), Value::Array(_)) if current == defaults => None,
        _ if current == defaults => None,
        _ => Some(current.clone()),
    }
}

async fn write_json_atomically(path: &Path, value: &Value) -> Result<(), ServerSettingsReadError> {
    let mut encoded =
        serde_json::to_vec_pretty(value).map_err(|source| ServerSettingsReadError::Decode {
            path: path.to_path_buf(),
            source,
        })?;
    encoded.push(b'\n');
    write_bytes_atomically(path, &encoded)
        .await
        .map_err(|source| ServerSettingsReadError::Persist {
            path: path.to_path_buf(),
            source,
        })
}

async fn write_bytes_atomically(path: &Path, contents: &[u8]) -> Result<(), std::io::Error> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).await?;
    let temporary_dir = parent.join(format!(
        "{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("settings"),
        Uuid::new_v4()
    ));
    fs::create_dir(&temporary_dir).await?;
    let temporary_path = temporary_dir.join("contents.tmp");
    let result = async {
        let mut file = fs::File::create(&temporary_path).await?;
        file.write_all(contents).await?;
        file.sync_all().await?;
        drop(file);
        if fs::rename(&temporary_path, path).await.is_err() {
            let _ = fs::remove_file(path).await;
            fs::rename(&temporary_path, path).await?;
        }
        Ok(())
    }
    .await;
    let _ = fs::remove_dir_all(&temporary_dir).await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUILTIN_DEFAULTS: &[(&str, bool, &str)] = &[
        ("codex", false, "codex"),
        ("claudeAgent", false, "claude"),
        ("coddy", true, "coddy"),
        ("cursor", false, "cursor-agent"),
        ("grok", false, "grok"),
        ("opencode", false, "opencode"),
    ];

    #[tokio::test]
    async fn partial_builtin_provider_objects_preserve_enabled_defaults_and_custom_paths() {
        let temp = tempfile::tempdir().expect("settings root");
        let store = ProviderSettingsStore::new(temp.path());
        let mut actual = Vec::new();
        let mut expected = Vec::new();
        for &(driver, enabled, _) in BUILTIN_DEFAULTS {
            let binary = temp.path().join(format!("custom-{driver}"));
            fs::write(
                temp.path().join("settings.json"),
                serde_json::to_vec(&serde_json::json!({
                    "providers": { driver: { "binaryPath": binary } }
                }))
                .expect("partial settings JSON"),
            )
            .await
            .expect("partial settings file");

            let settings = store.get().await.expect("partial settings load");
            let providers = serde_json::to_value(settings.providers).expect("providers JSON");
            actual.push((driver, providers[driver]["enabled"].clone()));
            expected.push((driver, Value::Bool(enabled)));
            assert_eq!(providers[driver]["binaryPath"], serde_json::json!(binary));
        }
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn absent_settings_file_uses_all_builtin_provider_defaults() {
        let temp = tempfile::tempdir().expect("settings root");
        let settings = ProviderSettingsStore::new(temp.path())
            .get()
            .await
            .expect("absent settings load");
        let providers = serde_json::to_value(settings.providers).expect("providers JSON");
        for &(driver, enabled, binary) in BUILTIN_DEFAULTS {
            assert_eq!(providers[driver]["enabled"], enabled, "{driver}");
            assert_eq!(providers[driver]["binaryPath"], binary, "{driver}");
        }
        assert!(!temp.path().join("settings.json").exists());
    }

    #[tokio::test]
    async fn explicit_builtin_provider_disable_preserves_default_binary_paths() {
        let temp = tempfile::tempdir().expect("settings root");
        let store = ProviderSettingsStore::new(temp.path());
        for &(driver, _, binary) in BUILTIN_DEFAULTS {
            fs::write(
                temp.path().join("settings.json"),
                serde_json::to_vec(&serde_json::json!({
                    "providers": { driver: { "enabled": false } }
                }))
                .expect("disabled settings JSON"),
            )
            .await
            .expect("disabled settings file");

            let settings = store.get().await.expect("disabled settings load");
            let providers = serde_json::to_value(settings.providers).expect("providers JSON");
            assert_eq!(providers[driver]["enabled"], false, "{driver}");
            assert_eq!(providers[driver]["binaryPath"], binary, "{driver}");
        }
    }

    #[tokio::test]
    async fn codex_custom_path_update_roundtrip_preserves_enabled_default() {
        let temp = tempfile::tempdir().expect("settings root");
        let store = ProviderSettingsStore::new(temp.path());
        let updated = store
            .update(ServerSettingsPatch {
                providers: Some(ProvidersPatch {
                    codex: Some(ProviderSettingsPatch {
                        binary_path: Some("custom-codex".to_owned()),
                        ..ProviderSettingsPatch::default()
                    }),
                    ..ProvidersPatch::default()
                }),
                ..ServerSettingsPatch::default()
            })
            .await
            .expect("update Codex binary");
        assert!(updated.providers.codex.enabled);

        let reloaded = store.get().await.expect("reload Codex settings");
        assert!(reloaded.providers.codex.enabled);
        assert_eq!(reloaded.providers.codex.binary_path, "custom-codex");
        assert_eq!(reloaded, updated);
    }

    #[test]
    fn grok_is_disabled_by_default() {
        assert!(!ProviderSettingsState::default().providers.grok.enabled);
    }

    #[test]
    fn cursor_is_enabled_by_default_and_explicit_disable_is_preserved() {
        let defaults = ProviderSettingsState::default();
        assert!(defaults.providers.cursor.enabled);
        assert_eq!(defaults.providers.cursor.binary_path, "cursor-agent");

        let explicit: ProviderSettingsState = serde_json::from_value(serde_json::json!({
            "providers": {
                "cursor": {
                    "enabled": false,
                    "binaryPath": "cursor-agent"
                }
            }
        }))
        .expect("explicit Cursor settings");
        assert!(!explicit.providers.cursor.enabled);
    }

    #[tokio::test]
    async fn filesystem_and_patch_boundaries_cover_settings_failure_contracts() {
        assert_eq!(
            ProviderSettingsState::default()
                .providers
                .cursor
                .binary_path,
            "cursor-agent"
        );

        let temporary = tempfile::tempdir().unwrap();
        let unreadable_settings = temporary.path().join("unreadable");
        fs::create_dir_all(unreadable_settings.join("settings.json"))
            .await
            .unwrap();
        assert!(matches!(
            ProviderSettingsStore::new(&unreadable_settings)
                .read_persisted()
                .await,
            Err(ServerSettingsReadError::Read { .. })
        ));

        let malformed_root = temporary.path().join("malformed-settings");
        fs::create_dir_all(&malformed_root).await.unwrap();
        let malformed_path = malformed_root.join("settings.json");
        fs::write(&malformed_path, b"{not-json").await.unwrap();
        let malformed_store = ProviderSettingsStore::new(&malformed_root);
        assert!(matches!(
            malformed_store
                .update(ServerSettingsPatch {
                    add_project_base_directory: Some("/should-not-persist".to_owned()),
                    ..ServerSettingsPatch::default()
                })
                .await,
            Err(ServerSettingsReadError::Decode { .. })
        ));
        assert_eq!(fs::read(&malformed_path).await.unwrap(), b"{not-json");

        let blocked_root = temporary.path().join("blocked-root");
        fs::write(&blocked_root, "file").await.unwrap();
        assert!(matches!(
            ProviderSettingsStore::new(&blocked_root)
                .materialize_and_persist(ProviderSettingsState::default())
                .await,
            Err(ServerSettingsReadError::Persist { .. })
        ));
        assert!(
            write_json_atomically(&blocked_root.join("settings.json"), &Value::Null)
                .await
                .is_err()
        );

        let secret_root = temporary.path().join("secret-root");
        let store = ProviderSettingsStore::new(&secret_root);
        fs::create_dir_all(store.secret_path("instance", "TOKEN"))
            .await
            .unwrap();
        let mut settings = ProviderSettingsState::default();
        settings.provider_instances.insert(
            "instance".to_owned(),
            ProviderInstanceState {
                driver: "codex".to_owned(),
                enabled: true,
                display_name: None,
                environment: vec![ProviderEnvironmentVariableState {
                    name: "TOKEN".to_owned(),
                    value: String::new(),
                    sensitive: true,
                    value_redacted: true,
                }],
                config: Value::Null,
            },
        );
        assert!(matches!(
            store.materialize_secrets(settings).await,
            Err(ServerSettingsReadError::Read { .. })
        ));

        let mut provider = ProviderBinarySettingsState::default();
        apply_provider_patch(
            &mut provider,
            Some(ProviderSettingsPatch {
                enabled: Some(true),
                binary_path: Some(" custom ".to_owned()),
                server_url: Some(" http://localhost ".to_owned()),
                server_password: Some(" secret ".to_owned()),
                data_dir: None,
            }),
            "default",
        );
        assert!(provider.enabled);
        assert_eq!(provider.binary_path, "custom");
        assert_eq!(provider.server_url, "http://localhost");
        assert_eq!(provider.server_password, "secret");
        assert_eq!(strip_defaults(&Value::Null, &Value::Null), None);
        assert_eq!(
            strip_defaults(&Value::String("value".to_owned()), &Value::Null),
            Some(Value::String("value".to_owned()))
        );
    }

    #[tokio::test]
    async fn coddy_defaults_fill_partial_documents() {
        let temp = tempfile::tempdir().expect("settings root");
        fs::write(
            temp.path().join("settings.json"),
            serde_json::to_vec(&serde_json::json!({
                "providers": { "coddy": { "binaryPath": "/opt/coddy" } }
            }))
            .expect("partial coddy settings JSON"),
        )
        .await
        .expect("partial coddy settings file");

        let settings = ProviderSettingsStore::new(temp.path())
            .get()
            .await
            .expect("partial coddy settings load");

        assert_eq!(settings.providers.coddy.binary_path, "/opt/coddy");
        assert_eq!(settings.providers.coddy.data_dir, "");
        assert_eq!(settings.providers.coddy.server_url, "");
        assert!(settings.providers.coddy.enabled);
    }
}
