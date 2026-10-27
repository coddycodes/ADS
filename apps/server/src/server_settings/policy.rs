use std::{collections::BTreeMap, path::Path, sync::OnceLock};

use serde::Deserialize;

use super::ProvidersState;

/// `~/.coddy/studio/config.yaml`: the product policy an operator writes by hand.
///
/// Missing file means the Coddy-only defaults: the Coddy driver is enabled, the
/// other built-in drivers stay off, and only `coddy` models are offered. A file
/// can re-enable a driver and widen the model allowlist; it never has to restate
/// the defaults.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct StudioPolicy {
    pub providers: BTreeMap<String, ProviderPolicy>,
    pub models: ModelsPolicy,
    pub device_agent: DeviceAgentPolicy,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ProviderPolicy {
    pub enabled: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ModelsPolicy {
    /// Provider prefixes the model catalog may offer. Empty means the Coddy
    /// provider only, matching the product default.
    pub providers: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DeviceAgentPolicy {
    /// Coddy Studio refuses Coddy turns when the device agent is unreachable.
    pub required: bool,
    pub url: String,
}

impl Default for DeviceAgentPolicy {
    fn default() -> Self {
        Self {
            required: true,
            url: "http://127.0.0.1:9098".to_owned(),
        }
    }
}

impl StudioPolicy {
    /// The product default when no `config.yaml` exists.
    pub fn production_defaults() -> Self {
        Self {
            providers: BTreeMap::new(),
            models: ModelsPolicy {
                providers: vec!["coddy".to_owned()],
            },
            device_agent: DeviceAgentPolicy::default(),
        }
    }

    /// Used before the server has loaded its data root (and by tests that do not
    /// exercise the policy): no driver override, every model provider allowed,
    /// and no device-agent gate.
    pub fn permissive() -> Self {
        Self {
            providers: BTreeMap::new(),
            models: ModelsPolicy::default(),
            device_agent: DeviceAgentPolicy {
                required: false,
                url: DeviceAgentPolicy::default().url,
            },
        }
    }

    /// Reads `<root>/config.yaml`. A missing or malformed file falls back to the
    /// Coddy-only defaults; the malformed case is reported on stderr so the
    /// operator can fix it without the server refusing to start.
    pub fn load(root: &Path) -> Self {
        let path = root.join("config.yaml");
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Self::production_defaults();
            }
            Err(error) => {
                eprintln!("studio config {}: {error}", path.display());
                return Self::production_defaults();
            }
        };
        match serde_yaml::from_str::<Self>(&text) {
            Ok(mut policy) => {
                if policy.models.providers.is_empty() {
                    policy.models.providers = vec!["coddy".to_owned()];
                }
                policy
            }
            Err(error) => {
                eprintln!("studio config {}: {error}", path.display());
                Self::production_defaults()
            }
        }
    }

    /// Applies explicit `providers.<driver>.enabled` values from the file. A
    /// driver the file does not mention keeps the value already in settings.
    pub fn apply_enablement(&self, providers: &mut ProvidersState) {
        if let Some(enabled) = self.driver_enabled("codex") {
            providers.codex.enabled = enabled;
        }
        if let Some(enabled) = self.driver_enabled("claudeAgent") {
            providers.claude_agent.enabled = enabled;
        }
        if let Some(enabled) = self.driver_enabled("coddy") {
            providers.coddy.enabled = enabled;
        }
        if let Some(enabled) = self.driver_enabled("cursor") {
            providers.cursor.enabled = enabled;
        }
        if let Some(enabled) = self.driver_enabled("grok") {
            providers.grok.enabled = enabled;
        }
        if let Some(enabled) = self.driver_enabled("opencode") {
            providers.opencode.enabled = enabled;
        }
    }

    pub fn model_provider_allowed(&self, provider: &str) -> bool {
        self.models.providers.is_empty() || self.models.providers.iter().any(|id| id == provider)
    }

    pub fn driver_enabled(&self, driver: &str) -> Option<bool> {
        self.providers.get(driver).map(|provider| provider.enabled)
    }
}

static CURRENT: OnceLock<StudioPolicy> = OnceLock::new();

/// Publishes the loaded policy for the inventory, model catalog and Coddy
/// driver. The first root wins; the server has exactly one.
pub fn init(policy: StudioPolicy) {
    let _ = CURRENT.set(policy);
}

pub fn current() -> &'static StudioPolicy {
    CURRENT.get_or_init(StudioPolicy::permissive)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn missing_file_is_coddy_only_with_a_required_device_agent() {
        let temp = tempfile::tempdir().expect("policy root");
        let policy = StudioPolicy::load(temp.path());

        assert!(policy.model_provider_allowed("coddy"));
        assert!(!policy.model_provider_allowed("openai"));
        assert!(policy.device_agent.required);
        assert_eq!(policy.device_agent.url, "http://127.0.0.1:9098");
        assert!(policy.providers.is_empty());
    }

    #[test]
    fn file_enables_a_driver_and_widens_models() {
        let temp = tempfile::tempdir().expect("policy root");
        std::fs::write(
            temp.path().join("config.yaml"),
            "providers:\n  codex:\n    enabled: true\nmodels:\n  providers:\n    - coddy\n    - openai\n",
        )
        .expect("policy file");

        let policy = StudioPolicy::load(temp.path());

        assert_eq!(policy.driver_enabled("codex"), Some(true));
        assert_eq!(policy.driver_enabled("claudeAgent"), None);
        assert!(policy.model_provider_allowed("openai"));
    }

    #[test]
    fn file_can_turn_the_device_agent_gate_off() {
        let temp = tempfile::tempdir().expect("policy root");
        std::fs::write(
            temp.path().join("config.yaml"),
            "deviceAgent:\n  required: false\n  url: http://127.0.0.1:19098\n",
        )
        .expect("policy file");

        let policy = StudioPolicy::load(temp.path());

        assert!(!policy.device_agent.required);
        assert_eq!(policy.device_agent.url, "http://127.0.0.1:19098");
        assert!(policy.model_provider_allowed("coddy"));
    }

    #[test]
    fn apply_enablement_only_touches_named_drivers() {
        let mut providers = ProvidersState::default();
        providers.codex.enabled = true;
        providers.claude_agent.enabled = false;
        let policy = StudioPolicy {
            providers: BTreeMap::from([(
                "claudeAgent".to_owned(),
                ProviderPolicy { enabled: true },
            )]),
            ..StudioPolicy::production_defaults()
        };

        policy.apply_enablement(&mut providers);

        assert!(providers.codex.enabled);
        assert!(providers.claude_agent.enabled);
    }

    #[test]
    fn permissive_policy_keeps_tests_unblocked() {
        let policy = StudioPolicy::permissive();

        assert!(policy.model_provider_allowed("anything"));
        assert!(!policy.device_agent.required);
        let _ = json!({});
    }
}
