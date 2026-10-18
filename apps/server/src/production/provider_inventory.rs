use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    path::Path,
    process::Stdio,
    time::Duration,
};

use process_wrap::tokio::{ChildWrapper, CommandWrap};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::Command,
    time::{sleep, timeout},
};
use tokio_util::sync::CancellationToken;

use crate::{
    git::{OutputPolicy, ProcessRequest, ProcessRunner},
    process::{
        Platform, configure_supervised_background_command_wrap,
        supervised::{log_cleanup_failures, terminate_and_wait},
    },
    production::provider_maintenance::{ProviderMaintenance, ProviderMaintenanceTarget},
    production::provider_runtime::{
        normalize_provider_environment, prepare_provider_launch,
        resolve_provider_executable_with_environment,
    },
    provider::{
        claude, codex, cursor, environment::sanitize_provider_subprocess_environment, grok,
        opencode,
    },
};

mod capabilities;
pub(crate) use capabilities::discover_capabilities;

const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const CURSOR_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);
const CLAUDE_CAPABILITIES_TIMEOUT: Duration = Duration::from_secs(45);
const CLAUDE_SKILLS_TIMEOUT: Duration = Duration::from_secs(10);
const CLAUDE_CAPABILITY_PROBE_ARGS: [&str; 12] = [
    "--print",
    "--output-format",
    "stream-json",
    "--verbose",
    "--input-format",
    "stream-json",
    "--setting-sources=user,project,local",
    "--settings",
    r#"{"disableAllHooks":true}"#,
    "--permission-mode",
    "default",
    "--no-session-persistence",
];
const LOCAL_OPENCODE_HEALTH_TIMEOUT: Duration = Duration::from_millis(250);
const LOCAL_OPENCODE_INVENTORY_TIMEOUT: Duration = Duration::from_secs(30);
const LOCAL_OPENCODE_STARTUP_ATTEMPTS: usize = 50;
/// A configured OpenCode 2 server answered, but its catalogs could not be read.
const OPENCODE_2_CATALOG_UNREADABLE: &str = "BiBCode couldn't read this OpenCode server's models, agents and commands, so chats use its default model. They are read again at the next provider check.";
const PROBE_OUTPUT_LIMIT: usize = 256 * 1024;

#[derive(Clone)]
struct ProviderDefinition {
    instance_id: String,
    driver: String,
    display_name: Option<String>,
    enabled: bool,
    binary_path: String,
    available: bool,
    custom_models: Vec<String>,
    configured_model: Option<String>,
    configured_effort: Option<String>,
    configured_service_tier: Option<String>,
    endpoint: Option<String>,
    server_password: Option<String>,
    environment: Vec<(OsString, OsString)>,
}

struct ProviderSessionDefaults {
    model: Option<String>,
    effort: Option<String>,
    service_tier: Option<String>,
}

#[derive(Clone, Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ProviderCapabilities {
    slash_commands: Vec<Value>,
    skills: Vec<Value>,
    agents: Vec<Value>,
    issues: Vec<String>,
}

struct ClaudeProbeMetadata {
    capabilities: ProviderCapabilities,
    models: Option<Vec<Value>>,
    skills_loaded: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RichMetadataOutcome {
    NotRequested,
    Succeeded,
    Failed,
}

#[derive(Clone, Debug)]
pub(crate) struct ProviderProbeResult {
    pub(crate) snapshot: Value,
    pub(crate) rich_metadata: RichMetadataOutcome,
    pub(crate) models_authoritative: bool,
}

impl ProviderProbeResult {
    fn new(snapshot: Value, rich_metadata: RichMetadataOutcome) -> Self {
        Self {
            snapshot,
            rich_metadata,
            models_authoritative: rich_metadata == RichMetadataOutcome::Succeeded,
        }
    }

    fn with_authoritative_models(mut self) -> Self {
        self.models_authoritative = true;
        self
    }
}

pub(crate) async fn probe(
    settings: &Value,
    selected: Option<&str>,
    cwd: &Path,
    maintenance: &ProviderMaintenance,
) -> Vec<ProviderProbeResult> {
    probe_inner(settings, selected, cwd, false, maintenance).await
}

pub(crate) async fn probe_full(
    settings: &Value,
    selected: Option<&str>,
    cwd: &Path,
    maintenance: &ProviderMaintenance,
) -> Vec<ProviderProbeResult> {
    probe_inner(settings, selected, cwd, true, maintenance).await
}

async fn probe_inner(
    settings: &Value,
    selected: Option<&str>,
    cwd: &Path,
    include_slow_capabilities: bool,
    maintenance: &ProviderMaintenance,
) -> Vec<ProviderProbeResult> {
    let checks_enabled = settings
        .get("enableProviderUpdateChecks")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    futures_util::future::join_all(
        definitions(settings)
            .into_iter()
            .filter(|definition| selected.is_none_or(|id| id == definition.instance_id))
            .map(|definition| {
                probe_one(
                    definition,
                    cwd,
                    include_slow_capabilities,
                    checks_enabled,
                    maintenance,
                )
            }),
    )
    .await
}

fn provider_is_enabled(driver: &str, configured: bool) -> bool {
    configured && driver != "grok"
}

fn default_provider_binary(driver: &str) -> &str {
    if driver == "cursor" {
        "cursor-agent"
    } else {
        driver
    }
}

/// The custom models a provider instance offers, read the way the catalog reads them: the
/// instance's own `customModels`, otherwise its driver's legacy provider settings. A launch
/// validates a turn's options with this list, so a custom model the catalog offers is accepted
/// the same way.
pub(crate) fn instance_custom_models(settings: &Value, instance_id: &str) -> Vec<String> {
    definitions(settings)
        .into_iter()
        .find(|definition| definition.instance_id == instance_id)
        .map(|definition| definition.custom_models)
        .unwrap_or_default()
}

fn definitions(settings: &Value) -> Vec<ProviderDefinition> {
    let legacy = settings.get("providers").and_then(Value::as_object);
    let instances = settings.get("providerInstances").and_then(Value::as_object);
    let mut definitions = instances
        .filter(|instances| !instances.is_empty())
        .map(|instances| {
            instances
                .iter()
                .map(|(instance_id, instance)| {
                    let driver = instance
                        .get("driver")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown");
                    let legacy_settings = legacy.and_then(|providers| providers.get(driver));
                    let session_defaults = provider_session_defaults(settings, driver);
                    let config = instance.get("config");
                    ProviderDefinition {
                        instance_id: instance_id.clone(),
                        driver: driver.to_owned(),
                        display_name: instance
                            .get("displayName")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        enabled: provider_is_enabled(
                            driver,
                            instance
                                .get("enabled")
                                .and_then(Value::as_bool)
                                .or_else(|| {
                                    legacy_settings
                                        .and_then(|value| value.get("enabled"))
                                        .and_then(Value::as_bool)
                                })
                                .unwrap_or(true),
                        ),
                        binary_path: string_setting(config, "binaryPath")
                            .or_else(|| string_setting(legacy_settings, "binaryPath"))
                            .unwrap_or_else(|| default_provider_binary(driver))
                            .to_owned(),
                        available: is_builtin_driver(driver),
                        custom_models: string_array(config, "customModels")
                            .or_else(|| string_array(legacy_settings, "customModels"))
                            .unwrap_or_default(),
                        configured_model: session_defaults.model,
                        configured_effort: session_defaults.effort,
                        configured_service_tier: session_defaults.service_tier,
                        endpoint: string_setting(config, "serverUrl")
                            .or_else(|| string_setting(config, "apiEndpoint"))
                            .or_else(|| string_setting(legacy_settings, "serverUrl"))
                            .or_else(|| string_setting(legacy_settings, "apiEndpoint"))
                            .filter(|value| !value.trim().is_empty())
                            .map(str::to_owned),
                        server_password: string_setting(config, "serverPassword")
                            .or_else(|| string_setting(legacy_settings, "serverPassword"))
                            .filter(|value| !value.is_empty())
                            .map(str::to_owned),
                        environment: provider_environment(instance),
                    }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    for driver in ["codex", "claudeAgent", "cursor", "grok", "opencode"] {
        if definitions
            .iter()
            .any(|definition| definition.driver == driver)
        {
            continue;
        }
        let driver_settings = legacy.and_then(|providers| providers.get(driver));
        let session_defaults = provider_session_defaults(settings, driver);
        definitions.push(ProviderDefinition {
            instance_id: driver.to_owned(),
            driver: driver.to_owned(),
            display_name: None,
            enabled: provider_is_enabled(
                driver,
                driver_settings
                    .and_then(|value| value.get("enabled"))
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
            ),
            binary_path: string_setting(driver_settings, "binaryPath")
                .unwrap_or_else(|| default_provider_binary(driver))
                .to_owned(),
            available: true,
            custom_models: string_array(driver_settings, "customModels").unwrap_or_default(),
            configured_model: session_defaults.model,
            configured_effort: session_defaults.effort,
            configured_service_tier: session_defaults.service_tier,
            endpoint: string_setting(driver_settings, "serverUrl")
                .or_else(|| string_setting(driver_settings, "apiEndpoint"))
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned),
            server_password: string_setting(driver_settings, "serverPassword")
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
            environment: Vec::new(),
        });
    }
    definitions
}

fn provider_session_defaults(settings: &Value, driver: &str) -> ProviderSessionDefaults {
    let session_default = settings
        .get("providerSessionDefaults")
        .and_then(Value::as_object)
        .and_then(|defaults| defaults.get(driver));
    let effort = ["reasoningEffort", "effort", "reasoning"]
        .into_iter()
        .find_map(|id| provider_option_value(session_default, id))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let service_tier = provider_option_value(session_default, "serviceTier")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            provider_option_value(session_default, "fastMode")
                .and_then(Value::as_bool)
                .map(|fast| if fast { "fast" } else { "default" }.to_owned())
        });
    ProviderSessionDefaults {
        model: session_default
            .and_then(|value| value.get("model"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        effort,
        service_tier,
    }
}

fn provider_option_value<'a>(session_default: Option<&'a Value>, id: &str) -> Option<&'a Value> {
    let options = session_default?.get("options")?;
    if let Some(selections) = options.as_array() {
        return selections
            .iter()
            .find(|selection| selection.get("id").and_then(Value::as_str) == Some(id))
            .and_then(|selection| selection.get("value"));
    }
    options.as_object()?.get(id)
}

fn string_setting<'a>(value: Option<&'a Value>, name: &str) -> Option<&'a str> {
    value?.get(name)?.as_str()
}

fn string_array(value: Option<&Value>, name: &str) -> Option<Vec<String>> {
    Some(
        value?
            .get(name)?
            .as_array()?
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

fn provider_environment(instance: &Value) -> Vec<(OsString, OsString)> {
    let environment = instance
        .get("environment")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|entry| {
            !entry
                .get("valueRedacted")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .filter_map(|entry| {
            let name = entry.get("name").and_then(Value::as_str)?.trim();
            if name.is_empty() {
                return None;
            }
            Some((
                OsString::from(name),
                OsString::from(
                    entry
                        .get("value")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                ),
            ))
        })
        .collect::<Vec<_>>();
    normalize_provider_environment(
        environment
            .iter()
            .map(|(name, value)| (name.as_os_str(), value.as_os_str())),
    )
}

async fn probe_one(
    definition: ProviderDefinition,
    cwd: &Path,
    include_slow_capabilities: bool,
    checks_enabled: bool,
    maintenance: &ProviderMaintenance,
) -> ProviderProbeResult {
    let target = maintenance_target_from_definition(&definition);
    let mut result = probe_one_snapshot(&definition, cwd, include_slow_capabilities).await;
    maintenance
        .enrich_snapshot(&target, &mut result.snapshot, checks_enabled)
        .await;
    result
}

fn maintenance_target_from_definition(
    definition: &ProviderDefinition,
) -> ProviderMaintenanceTarget {
    ProviderMaintenanceTarget {
        instance_id: definition.instance_id.clone(),
        driver: definition.driver.clone(),
        binary_path: definition.binary_path.clone(),
        environment: definition.environment.clone(),
    }
}

#[allow(dead_code)] // Used by Task 2 before running a provider update command.
pub(crate) fn maintenance_target(
    settings: &Value,
    provider: &str,
    instance_id: Option<&str>,
) -> Option<ProviderMaintenanceTarget> {
    definitions(settings)
        .into_iter()
        .find(|definition| {
            definition.driver == provider
                && instance_id.is_none_or(|id| id == definition.instance_id)
        })
        .as_ref()
        .map(maintenance_target_from_definition)
}

pub(crate) async fn probe_maintenance_target_version(
    target: &ProviderMaintenanceTarget,
    cwd: &Path,
) -> Option<String> {
    let executable = resolve_provider_executable_with_environment(
        &target.binary_path,
        target
            .environment
            .iter()
            .map(|(name, value)| (name.as_os_str(), value.as_os_str())),
    )?;
    let version_probe = run_command(&executable, &["--version"], cwd, &target.environment);
    let about_probe = async {
        if target.driver == "cursor" {
            run_command(&executable, &["about"], cwd, &target.environment).await
        } else {
            None
        }
    };
    let (version_output, about_output) = tokio::join!(version_probe, about_probe);
    let version = normalize_provider_version(
        &target.driver,
        version_output
            .as_ref()
            .filter(|output| output.success)
            .and_then(|output| first_line(&output.stdout, &output.stderr)),
    );
    if target.driver != "cursor" {
        return version;
    }
    let about = about_output
        .filter(|output| output.success)
        .map(|output| cursor::parse_about_output(output.code, &output.stdout, &output.stderr));
    about.and_then(|about| about.version).or(version)
}

async fn probe_one_snapshot(
    definition: &ProviderDefinition,
    cwd: &Path,
    include_slow_capabilities: bool,
) -> ProviderProbeResult {
    let checked_at = super::control::now_iso();
    let default_models = provider_models_without_version(definition);
    if !definition.available {
        return ProviderProbeResult::new(
            snapshot(
                definition,
                false,
                None,
                "disabled",
                json!({ "status": "unknown" }),
                default_models,
                ProviderCapabilities::default(),
                Some("Provider driver is unavailable in this build."),
                checked_at,
                "unavailable",
            ),
            RichMetadataOutcome::NotRequested,
        );
    }
    if !definition.enabled {
        return ProviderProbeResult::new(
            snapshot(
                definition,
                false,
                None,
                "disabled",
                json!({ "status": "unknown" }),
                default_models,
                ProviderCapabilities::default(),
                None,
                checked_at,
                "available",
            ),
            RichMetadataOutcome::NotRequested,
        );
    }
    if !include_slow_capabilities
        && definition.driver == "opencode"
        && definition.endpoint.is_some()
    {
        return ProviderProbeResult::new(
            snapshot(
                definition,
                true,
                None,
                "ready",
                json!({ "status": "unknown" }),
                default_models,
                ProviderCapabilities::default(),
                None,
                checked_at,
                "available",
            ),
            RichMetadataOutcome::NotRequested,
        );
    }
    if include_slow_capabilities
        && definition.driver == "opencode"
        && let Some(endpoint) = definition.endpoint.as_deref()
    {
        let protocol =
            match opencode_server_protocol(endpoint, definition.server_password.as_deref()).await {
                Ok(protocol) => protocol,
                Err(version) => {
                    return ProviderProbeResult::new(
                        snapshot(
                            definition,
                            true,
                            None,
                            "error",
                            json!({ "status": "unknown" }),
                            default_models,
                            ProviderCapabilities::default(),
                            Some(&opencode::unsupported_server_message(&version, false)),
                            checked_at,
                            "available",
                        ),
                        RichMetadataOutcome::Failed,
                    );
                }
            };
        if let Some(inventory) = probe_opencode(
            endpoint,
            definition.server_password.as_deref(),
            &definition.custom_models,
            protocol,
            cwd,
        )
        .await
        {
            let models = serde_json::to_value(inventory.models)
                .ok()
                .and_then(|value| value.as_array().cloned())
                .unwrap_or_else(|| custom_models(&definition.custom_models));
            let capabilities = ProviderCapabilities {
                slash_commands: inventory.commands,
                skills: inventory.skills,
                agents: inventory.agents,
                ..ProviderCapabilities::default()
            };
            return ProviderProbeResult::new(
                snapshot(
                    definition,
                    true,
                    None,
                    "ready",
                    inventory.auth,
                    models,
                    capabilities,
                    None,
                    checked_at,
                    "available",
                ),
                RichMetadataOutcome::Succeeded,
            );
        }
        if protocol == opencode::OpenCodeProtocol::V2 {
            // The server answered as OpenCode 2, so chats work without its catalogs; a local CLI
            // says nothing about it.
            return ProviderProbeResult::new(
                snapshot(
                    definition,
                    true,
                    None,
                    "ready",
                    json!({ "status": "unknown" }),
                    default_models,
                    ProviderCapabilities::default(),
                    Some(OPENCODE_2_CATALOG_UNREADABLE),
                    checked_at,
                    "available",
                ),
                RichMetadataOutcome::Failed,
            );
        }
    }
    let mut rich_metadata = if include_slow_capabilities {
        RichMetadataOutcome::Failed
    } else {
        RichMetadataOutcome::NotRequested
    };
    let Some(executable) = resolve_provider_executable_with_environment(
        &definition.binary_path,
        definition
            .environment
            .iter()
            .map(|(name, value)| (name.as_os_str(), value.as_os_str())),
    ) else {
        return ProviderProbeResult::new(
            snapshot(
                definition,
                false,
                None,
                "error",
                json!({ "status": "unknown" }),
                default_models,
                ProviderCapabilities::default(),
                Some("Provider executable was not found."),
                checked_at,
                "available",
            ),
            rich_metadata,
        );
    };
    if !include_slow_capabilities {
        // A version BiBCode cannot drive must not read Ready until the full probe runs.
        if definition.driver == "opencode" {
            let output =
                run_command(&executable, &["--version"], cwd, &definition.environment).await;
            let version = normalize_provider_version(
                &definition.driver,
                output
                    .as_ref()
                    .and_then(|output| first_line(&output.stdout, &output.stderr)),
            );
            if let Some(opencode::OpenCodeCliSupport::Unsupported(unsupported)) =
                version.as_deref().map(opencode::cli_support)
            {
                return ProviderProbeResult::new(
                    snapshot(
                        definition,
                        true,
                        version,
                        "error",
                        json!({ "status": "unknown" }),
                        default_models,
                        ProviderCapabilities::default(),
                        Some(unsupported),
                        checked_at,
                        "available",
                    ),
                    rich_metadata,
                );
            }
        }
        return ProviderProbeResult::new(
            snapshot(
                definition,
                true,
                None,
                "ready",
                json!({ "status": "unknown" }),
                default_models,
                ProviderCapabilities {
                    slash_commands: built_in_slash_commands(&definition.driver),
                    ..ProviderCapabilities::default()
                },
                None,
                checked_at,
                "available",
            ),
            rich_metadata,
        );
    }

    let version_output =
        run_command(&executable, &["--version"], cwd, &definition.environment).await;
    let version = version_output
        .as_ref()
        .and_then(|output| first_line(&output.stdout, &output.stderr));
    let version = normalize_provider_version(&definition.driver, version);
    let mut installed = version_output.is_some();
    let mut status = match version_output.as_ref() {
        Some(output) if output.success => "ready",
        Some(_) => "warning",
        None => "error",
    };
    let mut auth = json!({ "status": "unknown" });
    let mut models = default_models;
    let mut capabilities = ProviderCapabilities {
        slash_commands: built_in_slash_commands(&definition.driver),
        ..ProviderCapabilities::default()
    };
    let mut message = match version_output.as_ref() {
        Some(output) if !output.success => Some("Provider executable returned a non-zero status."),
        None => Some("Provider executable could not be started."),
        Some(_) => None,
    };

    let mut versioned_model_catalog = false;
    match definition.driver.as_str() {
        "codex" => {
            if include_slow_capabilities
                && let Some(inventory) = probe_codex(
                    &executable,
                    cwd,
                    &definition.custom_models,
                    &definition.environment,
                )
                .await
            {
                installed = true;
                rich_metadata = RichMetadataOutcome::Succeeded;
                (status, auth, message) = codex_probe_health(&inventory.account);
                models = serde_json::to_value(inventory.models)
                    .ok()
                    .and_then(|value| value.as_array().cloned())
                    .unwrap_or(models);
                capabilities.skills = codex_skill_inventory(&inventory.skills);
            }
        }
        "cursor" => {
            if let Some(output) =
                run_command(&executable, &["about"], cwd, &definition.environment).await
            {
                let about = cursor::parse_about_output(output.code, &output.stdout, &output.stderr);
                installed = true;
                status = if about.status == "ready" {
                    "ready"
                } else {
                    "error"
                };
                auth = about.auth;
                let workspace_capabilities =
                    cursor::discover_workspace_capabilities_with_environment(
                        cwd,
                        &definition.environment,
                    )
                    .await;
                capabilities.slash_commands = merge_slash_commands(
                    workspace_capabilities.slash_commands,
                    capabilities.slash_commands,
                );
                capabilities.skills = workspace_capabilities.skills;
                capabilities.agents = workspace_capabilities.agents;
                if include_slow_capabilities
                    && let Some(discovered) = probe_cursor_models(
                        &executable,
                        definition.endpoint.as_deref(),
                        &definition.custom_models,
                        cwd,
                        &definition.environment,
                    )
                    .await
                {
                    models = discovered;
                    rich_metadata = RichMetadataOutcome::Succeeded;
                }
                return ProviderProbeResult::new(
                    snapshot_owned_message(
                        definition,
                        installed,
                        about.version.or(version),
                        status,
                        auth,
                        models,
                        capabilities,
                        about.message,
                        checked_at,
                    ),
                    rich_metadata,
                );
            }
        }
        "claudeAgent" => {
            let mut model_catalog_complete = false;
            if version_output.as_ref().is_some_and(|output| output.success)
                && let Some(version) = version.as_deref()
            {
                models = claude::model::models_for_version(version, &definition.custom_models);
                model_catalog_complete = true;
                versioned_model_catalog = true;
            }
            if let Some(output) = run_command(
                &executable,
                &["auth", "status", "--json"],
                cwd,
                &definition.environment,
            )
            .await
            {
                auth = claude_auth(&output.stdout);
                if auth["status"] == "unauthenticated" {
                    status = "warning";
                    message = Some("Claude is installed but not authenticated.");
                }
            }
            if include_slow_capabilities
                && let Some(mut discovered) = probe_claude_metadata(
                    &executable,
                    cwd,
                    &definition.environment,
                    &definition.custom_models,
                )
                .await
            {
                if let Some(discovered_models) = discovered.models.take() {
                    models = discovered_models;
                    model_catalog_complete = true;
                    versioned_model_catalog = true;
                }
                capabilities = discovered.capabilities;
                capabilities.slash_commands = merge_slash_commands(
                    capabilities.slash_commands,
                    built_in_slash_commands(&definition.driver),
                );
                apply_agent_options(&mut models, &capabilities.agents);
                if model_catalog_complete && discovered.skills_loaded {
                    rich_metadata = RichMetadataOutcome::Succeeded;
                }
            }
        }
        "opencode" => {
            // A version BiBCode cannot drive is reported without starting its server.
            if let Some(opencode::OpenCodeCliSupport::Unsupported(unsupported)) =
                version.as_deref().map(opencode::cli_support)
            {
                status = "error";
                message = Some(unsupported);
            } else if include_slow_capabilities
                && let Some(inventory) = probe_local_opencode(
                    &executable,
                    cwd,
                    &definition.custom_models,
                    &definition.environment,
                )
                .await
            {
                rich_metadata = RichMetadataOutcome::Succeeded;
                status = "ready";
                auth = inventory.auth;
                models = serde_json::to_value(inventory.models)
                    .ok()
                    .and_then(|value| value.as_array().cloned())
                    .unwrap_or(models);
                capabilities.slash_commands = inventory.commands;
                capabilities.agents = inventory.agents;
                capabilities.skills = inventory.skills;
                message = None;
            }
        }
        "grok" if version_output.as_ref().is_some_and(|output| output.success) => {
            rich_metadata = RichMetadataOutcome::Succeeded;
        }
        _ => {}
    }

    let result = ProviderProbeResult::new(
        snapshot(
            definition,
            installed,
            version,
            status,
            auth,
            models,
            capabilities,
            message,
            checked_at,
            "available",
        ),
        rich_metadata,
    );
    if versioned_model_catalog {
        result.with_authoritative_models()
    } else {
        result
    }
}

fn provider_models_without_version(definition: &ProviderDefinition) -> Vec<Value> {
    match definition.driver.as_str() {
        "codex" => codex::model::fallback_models(
            definition.configured_model.as_deref(),
            definition.configured_effort.as_deref(),
            definition.configured_service_tier.as_deref(),
            &definition.custom_models,
        ),
        "claudeAgent" => claude::model::all_models(&definition.custom_models),
        "grok" => serde_json::to_value(grok::model::default_models(&definition.custom_models))
            .ok()
            .and_then(|value| value.as_array().cloned())
            .unwrap_or_else(|| custom_models(&definition.custom_models)),
        _ => custom_models(&definition.custom_models),
    }
}

fn codex_skill_inventory(skills: &[codex::CodexProviderSkill]) -> Vec<Value> {
    skills
        .iter()
        .filter_map(|skill| serde_json::to_value(skill).ok())
        .map(|mut skill| {
            skill["invocation"] = json!("dollar");
            skill
        })
        .collect()
}

fn built_in_slash_commands(driver: &str) -> Vec<Value> {
    match driver {
        "codex" => vec![slash_command(
            "goal",
            "Set a completion condition and keep working until it is met",
            Some("<condition>"),
        )],
        "claudeAgent" => vec![
            slash_command(
                "goal",
                "Set a completion condition and keep working until it is met",
                Some("[condition|clear]"),
            ),
            slash_command(
                "loop",
                "Run a prompt repeatedly while the session stays open",
                Some("[interval] [prompt]"),
            ),
        ],
        "cursor" => vec![
            slash_command("models", "List and switch Cursor models", None),
            slash_command(
                "auto-run",
                "Configure automatic command execution",
                Some("[on|off|status]"),
            ),
            slash_command("new-chat", "Start a new Cursor chat", None),
            slash_command("vim", "Toggle Vim keys", None),
            slash_command("help", "Show Cursor command help", Some("[command]")),
            slash_command("feedback", "Send feedback to Cursor", Some("<message>")),
            slash_command("resume", "Resume a Cursor chat", Some("<chat>")),
            slash_command("copy-req-id", "Copy the last Cursor request ID", None),
            slash_command("rules", "Create or edit Cursor rules", None),
            slash_command("commands", "Create or edit Cursor commands", None),
            slash_command("mcp", "Manage Cursor MCP servers", Some("[enable|disable]")),
            slash_command("max-mode", "Toggle Cursor max mode", Some("[on|off]")),
            slash_command("compress", "Compress the current Cursor context", None),
            slash_command("add-plugin", "Install a Cursor plugin", None),
            slash_command("logout", "Sign out from Cursor", None),
            slash_command("quit", "Exit the Cursor session", None),
        ],
        "grok" => vec![
            slash_command("loop", "Run a prompt repeatedly", Some("[prompt]")),
            slash_command("agents", "List and manage Grok agents", None),
            slash_command("skills", "List available Grok skills", None),
        ],
        _ => Vec::new(),
    }
}

fn slash_command(name: &str, description: &str, hint: Option<&str>) -> Value {
    let mut command = json!({
        "name": name,
        "description": description,
    });
    if let Some(hint) = hint.filter(|value| !value.trim().is_empty()) {
        command["input"] = json!({ "hint": hint });
    }
    command
}

fn parse_claude_initialization_response(
    response: &Value,
    custom_models: &[String],
) -> ClaudeProbeMetadata {
    ClaudeProbeMetadata {
        capabilities: ProviderCapabilities {
            slash_commands: parse_command_values(response.get("commands")),
            skills: Vec::new(),
            agents: response
                .get("agents")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|agent| {
                    let name = agent.get("name")?.as_str()?.trim();
                    if name.is_empty() {
                        return None;
                    }
                    let mut result = json!({ "name": name });
                    copy_non_empty_string(agent, &mut result, "description");
                    copy_non_empty_string(agent, &mut result, "model");
                    Some(result)
                })
                .collect(),
            ..ProviderCapabilities::default()
        },
        models: claude::model::models_from_initialization(response, custom_models),
        skills_loaded: false,
    }
}

fn parse_claude_skills_response(response: &Value, initialization: &Value) -> Option<Vec<Value>> {
    // reload_skills includes model-only skills; initialize is the native user-visible menu.
    let mut visible = HashMap::new();
    let mut aliases = HashMap::new();
    for command in initialization.get("commands")?.as_array()? {
        let Some(name) = command.get("name").and_then(Value::as_str) else {
            continue;
        };
        let name = name.trim().trim_start_matches('/');
        if name.is_empty() {
            continue;
        }
        visible.entry(name.to_ascii_lowercase()).or_insert(name);
        for alias in command
            .get("aliases")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            aliases
                .entry(alias.trim().trim_start_matches('/').to_ascii_lowercase())
                .or_insert(name);
        }
    }
    let mut seen = HashSet::new();
    Some(
        response
            .get("skills")?
            .as_array()?
            .iter()
            .filter_map(|skill| {
                let id = skill.get("name")?.as_str()?.trim().trim_start_matches('/');
                let key = id.to_ascii_lowercase();
                // Native command lookup prefers every primary name over any alias.
                let name = visible.get(&key).or_else(|| aliases.get(&key))?;
                if !seen.insert(name.to_ascii_lowercase()) {
                    return None;
                }
                let mut result = json!({
                    "name": name,
                    "path": format!("claude://skill/{id}"),
                    "scope": "provider",
                    "enabled": true,
                    "invocation": "slash",
                });
                copy_non_empty_string(skill, &mut result, "description");
                Some(result)
            })
            .collect(),
    )
}

fn parse_command_values(value: Option<&Value>) -> Vec<Value> {
    let mut seen = HashSet::new();
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|command| {
            let name = command
                .get("name")?
                .as_str()?
                .trim()
                .trim_start_matches('/');
            if name.is_empty() || !seen.insert(name.to_ascii_lowercase()) {
                return None;
            }
            let mut result = json!({ "name": name });
            copy_non_empty_string(command, &mut result, "description");
            if let Some(hint) = command
                .get("argumentHint")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|hint| !hint.is_empty())
            {
                result["input"] = json!({ "hint": hint });
            }
            Some(result)
        })
        .collect()
}

fn merge_slash_commands(mut commands: Vec<Value>, fallbacks: Vec<Value>) -> Vec<Value> {
    let mut seen = commands
        .iter()
        .filter_map(|command| command.get("name").and_then(Value::as_str))
        .map(str::to_ascii_lowercase)
        .collect::<HashSet<_>>();
    commands.extend(fallbacks.into_iter().filter(|command| {
        command
            .get("name")
            .and_then(Value::as_str)
            .is_some_and(|name| seen.insert(name.to_ascii_lowercase()))
    }));
    commands
}

fn copy_non_empty_string(source: &Value, target: &mut Value, key: &str) {
    if let Some(value) = source
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        target[key] = json!(value);
    }
}

fn apply_agent_options(models: &mut [Value], agents: &[Value]) {
    let default_agent = agents
        .iter()
        .find(|agent| agent["name"] == "claude")
        .or_else(|| agents.first())
        .and_then(|agent| agent.get("name"))
        .and_then(Value::as_str);
    let options = agents
        .iter()
        .filter_map(|agent| {
            let name = agent.get("name")?.as_str()?;
            let mut option = json!({
                "id": name,
                "label": name,
            });
            if Some(name) == default_agent {
                option["isDefault"] = json!(true);
            }
            Some(option)
        })
        .collect::<Vec<_>>();
    if options.is_empty() {
        return;
    }
    for model in models {
        let capabilities = model
            .as_object_mut()
            .and_then(|model| model.get_mut("capabilities"))
            .and_then(Value::as_object_mut);
        let Some(capabilities) = capabilities else {
            continue;
        };
        let descriptors = capabilities
            .entry("optionDescriptors")
            .or_insert_with(|| json!([]));
        let Some(descriptors) = descriptors.as_array_mut() else {
            continue;
        };
        descriptors.retain(|descriptor| descriptor["id"] != "agent");
        descriptors.push(json!({
            "id": "agent",
            "label": "Agent",
            "type": "select",
            "options": options.clone(),
            "currentValue": default_agent,
        }));
    }
}

async fn probe_claude_metadata(
    executable: &Path,
    cwd: &Path,
    environment: &[(OsString, OsString)],
    custom_models: &[String],
) -> Option<ClaudeProbeMetadata> {
    probe_claude_metadata_with_cancellation(
        executable,
        cwd,
        environment,
        custom_models,
        &CancellationToken::new(),
    )
    .await
}

async fn probe_claude_metadata_with_cancellation(
    executable: &Path,
    cwd: &Path,
    environment: &[(OsString, OsString)],
    custom_models: &[String],
    cancellation: &CancellationToken,
) -> Option<ClaudeProbeMetadata> {
    let launch = prepare_provider_launch(executable, CLAUDE_CAPABILITY_PROBE_ARGS).ok()?;
    let mut command = Command::new(launch.program);
    command
        .args(launch.args)
        .current_dir(cwd)
        .envs(environment.iter().cloned())
        .env("CLAUDE_CODE_ENTRYPOINT", "sdk-rust")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    sanitize_provider_subprocess_environment(&mut command);
    let mut child = supervised_command(command).spawn().ok()?;
    let probe = async {
        let mut stdin = child.stdin().take()?;
        let stdout = child.stdout().take()?;
        let mut lines = BufReader::new(stdout).lines();

        write_claude_control_request(&mut stdin, "bibcode-inventory", "initialize")
            .await
            .ok()?;
        let initialization = timeout(
            CLAUDE_CAPABILITIES_TIMEOUT,
            read_claude_control_response(&mut lines, "bibcode-inventory"),
        )
        .await
        .ok()
        .flatten()?;
        let mut metadata = parse_claude_initialization_response(&initialization, custom_models);

        let skills = if write_claude_control_request(&mut stdin, "bibcode-skills", "reload_skills")
            .await
            .is_ok()
        {
            timeout(
                CLAUDE_SKILLS_TIMEOUT,
                read_claude_control_response(&mut lines, "bibcode-skills"),
            )
            .await
            .ok()
            .flatten()
            .and_then(|response| parse_claude_skills_response(&response, &initialization))
        } else {
            None
        };
        let _ = stdin.shutdown().await;
        if let Some(skills) = skills {
            metadata.capabilities.skills = skills;
            metadata.skills_loaded = true;
        }
        Some(metadata)
    };
    let result = tokio::select! {
        biased;
        () = cancellation.cancelled() => None,
        result = probe => result,
    };
    stop_supervised_child(&mut *child).await;
    result
}

async fn write_claude_control_request(
    stdin: &mut tokio::process::ChildStdin,
    request_id: &str,
    subtype: &str,
) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec(&json!({
        "type": "control_request",
        "request_id": request_id,
        "request": { "subtype": subtype },
    }))
    .map_err(std::io::Error::other)?;
    bytes.push(b'\n');
    stdin.write_all(&bytes).await?;
    stdin.flush().await
}

async fn read_claude_control_response<R: AsyncBufRead + Unpin>(
    lines: &mut Lines<R>,
    request_id: &str,
) -> Option<Value> {
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(response) = value.get("response") else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) == Some("control_response")
            && response.get("request_id").and_then(Value::as_str) == Some(request_id)
            && response.get("subtype").and_then(Value::as_str) == Some("success")
        {
            return response.get("response").cloned();
        }
    }
    None
}

struct CommandOutput {
    success: bool,
    code: i32,
    stdout: String,
    stderr: String,
}

async fn run_command(
    executable: &Path,
    args: &[&str],
    cwd: &Path,
    environment: &[(OsString, OsString)],
) -> Option<CommandOutput> {
    let launch = prepare_provider_launch(executable, args).ok()?;
    let environment = normalize_provider_environment(
        environment
            .iter()
            .map(|(name, value)| (name.as_os_str(), value.as_os_str())),
    );
    let output = ProcessRunner
        .run(
            ProcessRequest {
                operation: "provider.inventory.probe".to_owned(),
                command: launch.program,
                args: launch.args,
                cwd: cwd.to_path_buf(),
                env: environment,
                stdin: None,
                timeout: PROBE_TIMEOUT,
                max_output_bytes: PROBE_OUTPUT_LIMIT,
                output_policy: OutputPolicy::Truncate,
                append_truncation_marker: true,
                allow_non_zero_exit: true,
            },
            &CancellationToken::new(),
        )
        .await
        .ok()?;
    Some(CommandOutput {
        success: output.exit_code == 0,
        code: output.exit_code,
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

async fn probe_codex(
    executable: &Path,
    cwd: &Path,
    custom_models: &[String],
    environment: &[(OsString, OsString)],
) -> Option<codex::CodexProviderSnapshot> {
    let launch = prepare_provider_launch(executable, ["app-server"]).ok()?;
    let mut command = Command::new(launch.program);
    command.current_dir(cwd).envs(environment.iter().cloned());
    sanitize_provider_subprocess_environment(&mut command);
    command
        .args(launch.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = supervised_command(command).spawn().ok()?;
    let stdout = child.stdout().take()?;
    let stdin = child.stdin().take()?;
    let stderr = child.stderr().take()?;
    let (connection, _incoming) =
        codex::JsonRpcConnection::spawn(stdout, stdin, stderr, codex::ConnectionConfig::default());
    let cwd = cwd.to_string_lossy().into_owned();
    let result = timeout(
        PROBE_TIMEOUT,
        codex::probe_provider(&connection, env!("CARGO_PKG_VERSION"), &cwd, custom_models),
    )
    .await
    .ok()
    .and_then(Result::ok);
    connection.close().await;
    stop_supervised_child(&mut *child).await;
    result
}

async fn probe_cursor_models(
    executable: &Path,
    endpoint: Option<&str>,
    custom_models: &[String],
    cwd: &Path,
    environment: &[(OsString, OsString)],
) -> Option<Vec<Value>> {
    let mut arguments = Vec::new();
    if let Some(endpoint) = endpoint.filter(|value| !value.trim().is_empty()) {
        arguments.extend([OsString::from("-e"), OsString::from(endpoint)]);
    }
    arguments.push(OsString::from("acp"));
    let launch = prepare_provider_launch(executable, &arguments).ok()?;
    let mut command = Command::new(launch.program);
    command
        .args(launch.args)
        .current_dir(cwd)
        .envs(environment.iter().cloned());
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    sanitize_provider_subprocess_environment(&mut command);
    let mut child = supervised_command(command).spawn().ok()?;
    let stdout = child.stdout().take()?;
    let stdin = child.stdin().take()?;
    let stderr = child.stderr().take()?;
    let (connection, _incoming) = cursor::AcpJsonRpcConnection::spawn(
        stdout,
        stdin,
        stderr,
        cursor::AcpConnectionConfig::default(),
    );
    let response = timeout(CURSOR_DISCOVERY_TIMEOUT, async {
        connection
            .request(
                "initialize",
                json!({
                    "protocolVersion": 1,
                    "clientCapabilities": {
                        "fs": { "readTextFile": false, "writeTextFile": false },
                        "terminal": false,
                    },
                    "clientInfo": { "name": "bibcode-rust", "version": env!("CARGO_PKG_VERSION") },
                }),
            )
            .await
            .ok()?;
        connection
            .request("authenticate", json!({ "methodId": "cursor_login" }))
            .await
            .ok()?;
        connection
            .request("cursor/list_available_models", json!({}))
            .await
            .ok()
    })
    .await
    .ok()
    .flatten();
    stop_supervised_child(&mut *child).await;
    let models =
        cursor::discover_models_from_list_available_models(&response?, custom_models).ok()?;
    serde_json::to_value(models)
        .ok()
        .and_then(|value| value.as_array().cloned())
}

/// The wire a configured OpenCode server speaks, or the version it reports when BiBCode cannot
/// drive it. A server that does not answer is read as OpenCode 1, as before.
pub(super) async fn opencode_server_protocol(
    endpoint: &str,
    server_password: Option<&str>,
) -> Result<opencode::OpenCodeProtocol, String> {
    let Ok(client) = reqwest::Client::builder()
        .timeout(opencode::API_PROBE_TIMEOUT)
        .build()
    else {
        return Ok(opencode::OpenCodeProtocol::V1);
    };
    match opencode::probe_server_api(&client, endpoint, server_password).await {
        opencode::OpenCodeServerApi::V2 { .. } => Ok(opencode::OpenCodeProtocol::V2),
        opencode::OpenCodeServerApi::Unsupported { version } => Err(version),
        opencode::OpenCodeServerApi::V1 | opencode::OpenCodeServerApi::Unavailable => {
            Ok(opencode::OpenCodeProtocol::V1)
        }
    }
}

async fn probe_opencode(
    endpoint: &str,
    server_password: Option<&str>,
    custom_models: &[String],
    protocol: opencode::OpenCodeProtocol,
    cwd: &Path,
) -> Option<opencode::OpenCodeInventorySnapshot> {
    probe_opencode_with_timeout(
        endpoint,
        server_password,
        custom_models,
        PROBE_TIMEOUT,
        protocol,
        cwd,
    )
    .await
}

/// OpenCode 2 catalogs are read for `cwd`, the directory BiBCode's sessions use; OpenCode 1
/// inventory reads stay unscoped, as they always were.
async fn probe_opencode_with_timeout(
    endpoint: &str,
    server_password: Option<&str>,
    custom_models: &[String],
    request_timeout: Duration,
    protocol: opencode::OpenCodeProtocol,
    cwd: &Path,
) -> Option<opencode::OpenCodeInventorySnapshot> {
    let client = reqwest::Client::builder()
        .timeout(request_timeout)
        .build()
        .ok()?;
    let scope = (protocol == opencode::OpenCodeProtocol::V2).then_some(cwd);
    let (providers, agents, commands) =
        fetch_opencode_catalog(&client, endpoint, server_password, scope, protocol).await?;
    Some(opencode::build_inventory_snapshot(
        &providers,
        &agents,
        &commands,
        custom_models,
    ))
}

/// An OpenCode server's provider list, agents and commands (skills included) in the OpenCode 1
/// shapes `build_inventory_snapshot` reads. `cwd` scopes the agent and command catalogs.
async fn fetch_opencode_catalog(
    client: &reqwest::Client,
    endpoint: &str,
    server_password: Option<&str>,
    cwd: Option<&Path>,
    protocol: opencode::OpenCodeProtocol,
) -> Option<(Value, Value, Value)> {
    let endpoint = endpoint.trim_end_matches('/');
    match protocol {
        opencode::OpenCodeProtocol::V1 => {
            let (providers, agents, commands) = tokio::join!(
                get_opencode_json(client, endpoint, "/provider", server_password, None),
                get_opencode_json(client, endpoint, "/agent", server_password, cwd),
                get_opencode_json(client, endpoint, "/command", server_password, cwd),
            );
            Some((providers?, agents?, commands?))
        }
        opencode::OpenCodeProtocol::V2 => {
            let [providers, models, agents, commands, skills] = read_opencode_v2_catalogs(
                client,
                endpoint,
                server_password,
                cwd,
                ["provider", "model", "agent", "command", "skill"],
            )
            .await?;
            use opencode::v2::catalog;
            Some((
                catalog::provider_list(&providers, &models),
                catalog::agents(&agents),
                catalog::commands(&commands, &skills),
            ))
        }
    }
}

/// Waits before each re-read of a just-started OpenCode 2 server's catalogs: 1.5 s in all.
const OPENCODE_2_CATALOG_RETRY_DELAYS: [Duration; 4] = [
    Duration::from_millis(100),
    Duration::from_millis(200),
    Duration::from_millis(400),
    Duration::from_millis(800),
];

/// Reads OpenCode 2 catalogs together (`routes` must include `agent`). A just-started server
/// answers before it has loaded them: its agent list, which always holds the built-in agents,
/// reads empty for about 100 ms (seen on 2.0.29). So an empty agent list is read again, after a
/// bounded backoff, rather than published as no agents.
pub(super) async fn read_opencode_v2_catalogs<const N: usize>(
    client: &reqwest::Client,
    endpoint: &str,
    server_password: Option<&str>,
    cwd: Option<&Path>,
    routes: [&'static str; N],
) -> Option<[Value; N]> {
    let agent = routes.iter().position(|route| *route == "agent");
    let mut delays = OPENCODE_2_CATALOG_RETRY_DELAYS.iter();
    loop {
        let reads = futures_util::future::join_all(
            routes.map(|route| get_opencode_v2_json(client, endpoint, route, server_password, cwd)),
        )
        .await;
        let catalogs: [Value; N] = reads
            .into_iter()
            .collect::<Option<Vec<_>>>()?
            .try_into()
            .ok()?;
        let agents_loaded = agent.is_none_or(|agent| {
            catalogs[agent]
                .get("data")
                .and_then(Value::as_array)
                .is_none_or(|agents| !agents.is_empty())
        });
        match delays.next() {
            Some(delay) if !agents_loaded => tokio::time::sleep(*delay).await,
            _ => return Some(catalogs),
        }
    }
}

async fn get_opencode_v2_json(
    client: &reqwest::Client,
    endpoint: &str,
    route: &str,
    server_password: Option<&str>,
    cwd: Option<&Path>,
) -> Option<Value> {
    let mut url = reqwest::Url::parse(&format!("{endpoint}/api/{route}")).ok()?;
    if let Some(cwd) = cwd {
        url.query_pairs_mut()
            .append_pair("location[directory]", &cwd.to_string_lossy());
    }
    let request = client.get(url);
    let request = match server_password.filter(|value| !value.is_empty()) {
        Some(password) => request.basic_auth("opencode", Some(password)),
        None => request,
    };
    let value = request
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json::<Value>()
        .await
        .ok()?;
    value.get("data").is_some().then_some(value)
}

async fn get_opencode_json(
    client: &reqwest::Client,
    endpoint: &str,
    path: &str,
    server_password: Option<&str>,
    cwd: Option<&Path>,
) -> Option<Value> {
    let mut url = reqwest::Url::parse(&format!("{endpoint}{path}")).ok()?;
    if let Some(cwd) = cwd {
        url.query_pairs_mut()
            .append_pair("directory", &cwd.to_string_lossy());
    }
    let request = client.get(url);
    let request = match server_password.filter(|value| !value.is_empty()) {
        Some(password) => request.basic_auth("opencode", Some(password)),
        None => request,
    };
    request
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json::<Value>()
        .await
        .ok()
}

async fn local_opencode_api(
    endpoint: &str,
    server_password: Option<&str>,
) -> opencode::OpenCodeServerApi {
    let Ok(client) = reqwest::Client::builder()
        .timeout(LOCAL_OPENCODE_HEALTH_TIMEOUT)
        .build()
    else {
        return opencode::OpenCodeServerApi::Unavailable;
    };
    opencode::probe_server_api(&client, endpoint, server_password).await
}

async fn probe_local_opencode(
    executable: &Path,
    cwd: &Path,
    custom_models: &[String],
    environment: &[(OsString, OsString)],
) -> Option<opencode::OpenCodeInventorySnapshot> {
    with_local_opencode(
        executable,
        cwd,
        environment,
        &CancellationToken::new(),
        |endpoint, password, protocol| async move {
            probe_opencode_with_timeout(
                &endpoint,
                Some(&password),
                custom_models,
                LOCAL_OPENCODE_INVENTORY_TIMEOUT,
                protocol,
                cwd,
            )
            .await
        },
    )
    .await
}

async fn with_local_opencode<T, F, Fut>(
    executable: &Path,
    cwd: &Path,
    environment: &[(OsString, OsString)],
    cancellation: &CancellationToken,
    discover: F,
) -> Option<T>
where
    F: FnOnce(String, String, opencode::OpenCodeProtocol) -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let executable = opencode::resolve_owned_executable(Platform::current(), executable);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.ok()?;
    let port = listener.local_addr().ok()?.port();
    drop(listener);
    let endpoint = format!("http://127.0.0.1:{port}");
    let port_argument = format!("--port={port}");
    let launch = prepare_provider_launch(
        &executable,
        ["serve", "--hostname=127.0.0.1", port_argument.as_str()],
    )
    .ok()?;
    let mut command = Command::new(launch.program);
    command
        .args(launch.args)
        .current_dir(cwd)
        .envs(environment.iter().cloned())
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let local_password = uuid::Uuid::new_v4().to_string();
    // OpenCode 2 prefers an inherited `OPENCODE_PASSWORD` to `OPENCODE_SERVER_PASSWORD`.
    command.env("OPENCODE_SERVER_PASSWORD", &local_password);
    command.env("OPENCODE_PASSWORD", &local_password);
    sanitize_provider_subprocess_environment(&mut command);
    let mut child = supervised_command(command).spawn().ok()?;
    let inventory = tokio::select! {
        biased;
        () = cancellation.cancelled() => None,
        result = async {
            for _ in 0..LOCAL_OPENCODE_STARTUP_ATTEMPTS {
                if child.try_wait().ok().flatten().is_some() { return None; }
                let protocol = match local_opencode_api(&endpoint, Some(&local_password)).await {
                    opencode::OpenCodeServerApi::V1 => opencode::OpenCodeProtocol::V1,
                    opencode::OpenCodeServerApi::V2 { .. } => opencode::OpenCodeProtocol::V2,
                    // BiBCode cannot read these catalogs: stop now instead of waiting it out.
                    opencode::OpenCodeServerApi::Unsupported { .. } => return None,
                    opencode::OpenCodeServerApi::Unavailable => {
                        sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let snapshot = discover(endpoint, local_password, protocol).await;
                return child.try_wait().ok().flatten().is_none().then_some(snapshot).flatten();
            }
            None
        } => result,
    };
    stop_supervised_child(&mut *child).await;
    inventory
}

fn supervised_command(command: Command) -> CommandWrap {
    let mut command = CommandWrap::from(command);
    configure_supervised_background_command_wrap(&mut command);
    command
}

async fn stop_supervised_child(child: &mut dyn ChildWrapper) {
    let report = terminate_and_wait(child).await;
    log_cleanup_failures("provider inventory probe", &report);
}

fn first_line(stdout: &str, stderr: &str) -> Option<String> {
    stdout
        .lines()
        .chain(stderr.lines())
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

fn normalize_provider_version(driver: &str, version: Option<String>) -> Option<String> {
    let version = version?;
    if driver != "codex" {
        return Some(version);
    }
    version
        .split_whitespace()
        .find(|part| {
            part.chars()
                .next()
                .is_some_and(|character| character.is_ascii_digit())
                && part.contains('.')
        })
        .map(str::to_owned)
        .or(Some(version))
}

fn codex_probe_health(account: &Value) -> (&'static str, Value, Option<&'static str>) {
    let auth = codex_auth(account);
    let requires_openai_auth = account
        .get("requiresOpenaiAuth")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let has_account = account.get("account").is_some_and(|value| !value.is_null());
    if requires_openai_auth && !has_account {
        return (
            "error",
            auth,
            Some("Codex is installed but requires authentication."),
        );
    }
    ("ready", auth, None)
}

fn codex_auth(account: &Value) -> Value {
    let Some(account) = account.get("account").and_then(Value::as_object) else {
        return json!({ "status": "unauthenticated" });
    };
    let mut auth = json!({ "status": "authenticated" });
    if let Some(account_type) = account.get("type").and_then(Value::as_str) {
        auth["type"] = json!(account_type);
    }
    if let Some(email) = account.get("email").and_then(Value::as_str) {
        auth["email"] = json!(email);
    }
    auth
}

fn claude_auth(stdout: &str) -> Value {
    let Ok(value) = serde_json::from_str::<Value>(stdout) else {
        return json!({ "status": "unknown" });
    };
    let authenticated = value
        .get("loggedIn")
        .and_then(Value::as_bool)
        .or_else(|| value.get("authenticated").and_then(Value::as_bool));
    let mut auth = json!({
        "status": match authenticated {
            Some(true) => "authenticated",
            Some(false) => "unauthenticated",
            None => "unknown",
        }
    });
    if let Some(method) = value
        .get("authMethod")
        .or_else(|| value.get("subscriptionType"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        auth["type"] = json!(method);
    }
    if let Some(email) = value
        .get("email")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        auth["email"] = json!(email);
    }
    auth
}

fn custom_models(slugs: &[String]) -> Vec<Value> {
    slugs
        .iter()
        .map(|slug| {
            json!({
                "slug": slug,
                "name": slug,
                "isCustom": true,
                "capabilities": null,
            })
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn snapshot(
    definition: &ProviderDefinition,
    installed: bool,
    version: Option<String>,
    status: &str,
    auth: Value,
    models: Vec<Value>,
    capabilities: ProviderCapabilities,
    message: Option<&str>,
    checked_at: String,
    availability: &str,
) -> Value {
    let mut result = snapshot_owned_message(
        definition,
        installed,
        version,
        status,
        auth,
        models,
        capabilities,
        message.map(str::to_owned),
        checked_at,
    );
    result["availability"] = json!(availability);
    if availability == "unavailable" {
        result["unavailableReason"] = json!("Provider driver is unavailable in this build.");
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn snapshot_owned_message(
    definition: &ProviderDefinition,
    installed: bool,
    version: Option<String>,
    status: &str,
    auth: Value,
    models: Vec<Value>,
    capabilities: ProviderCapabilities,
    message: Option<String>,
    checked_at: String,
) -> Value {
    let mut result = json!({
        "instanceId": definition.instance_id,
        "driver": definition.driver,
        "displayName": super::provider_runtime::instance_label(
            &definition.instance_id,
            definition.display_name.as_deref(),
            &definition.driver,
        ),
        "enabled": definition.enabled && definition.available,
        "installed": installed,
        "version": version,
        "status": status,
        "auth": auth,
        "checkedAt": checked_at,
        "availability": "available",
        "models": models,
        "slashCommands": capabilities.slash_commands,
        "skills": capabilities.skills,
        "agents": capabilities.agents,
    });
    if let Some(message) = message {
        result["message"] = json!(message);
    }
    if crate::provider::supports_turn_steer(&definition.driver) {
        result["supportsTurnSteer"] = json!(true);
    }
    // Always present: a client reads an absent flag (an older server) as unknown.
    result["supportsConversationRewind"] = json!(crate::provider::supports_conversation_rewind(
        &definition.driver
    ));
    if matches!(definition.driver.as_str(), "codex" | "claudeAgent") {
        result["supportsMcpStatus"] = json!(true);
    }
    if matches!(definition.driver.as_str(), "codex" | "claudeAgent") {
        result["supportsContextWindowUsage"] = json!(true);
    }
    result
}

fn is_builtin_driver(driver: &str) -> bool {
    matches!(
        driver,
        "codex" | "claudeAgent" | "cursor" | "grok" | "opencode"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use crate::test_support::TestSandbox;
    use axum::{Json, Router, routing::get};

    #[cfg(target_os = "linux")]
    fn check_inventory_probe_appimage_environment<F, R>(test_name: &str, run_probe: F)
    where
        F: FnOnce(std::path::PathBuf, std::path::PathBuf, Vec<(OsString, OsString)>) -> R,
        R: std::future::Future<Output = bool>,
    {
        use crate::test_support::{
            ISOLATING_AND_NO_OP_CASES,
            appimage_environment::{assert_child_environment, check_inherited_environment},
        };

        check_inherited_environment(test_name, ISOLATING_AND_NO_OP_CASES, |expected| {
            let sandbox = TestSandbox::new("inventory-appimage-environment");
            let output_path = sandbox.path("environment");
            // Read a protocol request before closing the pipes. OpenCode uses
            // null stdin, so its read completes immediately without a request.
            let executable = sandbox.executable_script(
                "inventory-probe",
                "IFS= read -r request\n/usr/bin/env -0 > \"$BIBCODE_TEST_PROVIDER_ENVIRONMENT_FILE\"",
                "",
            );
            let environment = vec![(
                OsString::from("BIBCODE_TEST_PROVIDER_ENVIRONMENT_FILE"),
                output_path.as_os_str().to_owned(),
            )];
            // The fixture records the launched environment, then closes the
            // native discovery protocol without advertising provider data.
            let discovered = crate::test_support::run_on_current_thread(run_probe(
                executable,
                sandbox.root().to_owned(),
                environment,
            ));
            assert!(!discovered, "fixture cannot supply inventory: {test_name}");
            let environment = std::fs::read(&output_path)
                .unwrap_or_else(|error| panic!("inventory probe did not run: {error}"));
            assert_child_environment(&environment, expected);
        });
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn claude_inventory_probe_ignores_appimage_environment() {
        check_inventory_probe_appimage_environment(
            "production::provider_inventory::tests::claude_inventory_probe_ignores_appimage_environment",
            |executable, cwd, environment| async move {
                probe_claude_metadata(&executable, &cwd, &environment, &[])
                    .await
                    .is_some()
            },
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn codex_inventory_probe_ignores_appimage_environment() {
        check_inventory_probe_appimage_environment(
            "production::provider_inventory::tests::codex_inventory_probe_ignores_appimage_environment",
            |executable, cwd, environment| async move {
                probe_codex(&executable, &cwd, &[], &environment)
                    .await
                    .is_some()
            },
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cursor_inventory_probe_ignores_appimage_environment() {
        check_inventory_probe_appimage_environment(
            "production::provider_inventory::tests::cursor_inventory_probe_ignores_appimage_environment",
            |executable, cwd, environment| async move {
                probe_cursor_models(&executable, None, &[], &cwd, &environment)
                    .await
                    .is_some()
            },
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn opencode_inventory_probe_ignores_appimage_environment() {
        check_inventory_probe_appimage_environment(
            "production::provider_inventory::tests::opencode_inventory_probe_ignores_appimage_environment",
            |executable, cwd, environment| async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                    .await
                    .unwrap_or_else(|error| {
                        panic!("OpenCode inventory probe requires a loopback listener: {error:?}")
                    });
                drop(listener);
                probe_local_opencode(&executable, &cwd, &[], &environment)
                    .await
                    .is_some()
            },
        );
    }

    #[cfg(unix)]
    fn write_version_fixture(directory: &Path, name: &str, version: &str) -> std::path::PathBuf {
        let executable = directory.join(name);
        crate::test_support::executable_fixture::write_executable(
            &executable,
            format!(
                "#!/bin/sh\nif [ \"$1\" = about ]; then printf '%s\\n' '{{\"cliVersion\":\"{version}\"}}'; else printf '%s\\n' '{version}'; fi\n"
            ),
        );
        executable
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn inventory_instance_path_outranks_ambient_path_in_an_isolated_process() {
        const CASE: &str = "provider-inventory-path-precedence";
        const TEST_NAME: &str = "production::provider_inventory::tests::inventory_instance_path_outranks_ambient_path_in_an_isolated_process";
        const SENTINEL: &str = "BIBCODE_TEST_ISOLATED_CASE_DONE=provider-inventory-path-precedence";

        if TestSandbox::is_isolated_case(CASE, TEST_NAME) {
            let instance = std::path::PathBuf::from(
                std::env::var_os("BIBCODE_TEST_INSTANCE_PATH").expect("isolated instance PATH"),
            );
            let root = std::path::PathBuf::from(
                std::env::var_os("BIBCODE_TEST_ROOT").expect("isolated inventory root"),
            );
            let ambient_executable = std::path::PathBuf::from(
                std::env::var_os("BIBCODE_TEST_AMBIENT_EXECUTABLE")
                    .expect("isolated ambient executable"),
            );
            assert_eq!(
                resolve_provider_executable_with_environment(
                    "cursor-agent",
                    std::iter::empty::<(&std::ffi::OsStr, &std::ffi::OsStr)>(),
                ),
                Some(ambient_executable),
                "the child must have a competing ambient provider executable"
            );
            let settings = json!({
                "enableProviderUpdateChecks": false,
                "providerInstances": {
                    "cursor-work": {
                        "driver": "cursor",
                        "enabled": true,
                        "config": { "binaryPath": "cursor-agent" },
                        "environment": [
                            { "name": "pAtH", "value": instance, "valueRedacted": false }
                        ]
                    }
                }
            });

            let providers = probe_full(
                &settings,
                Some("cursor-work"),
                &root,
                &ProviderMaintenance::new(),
            )
            .await;

            assert_eq!(providers[0].snapshot["version"], "2026.03.04-instance");
            println!("{SENTINEL}");
            return;
        }

        let sandbox = TestSandbox::new("provider-inventory-path");
        let ambient = sandbox.path("ambient");
        let instance = sandbox.path("instance");
        std::fs::create_dir(&ambient).expect("ambient executable directory");
        std::fs::create_dir(&instance).expect("instance executable directory");
        let ambient_executable =
            write_version_fixture(&ambient, "cursor-agent", "2026.01.02-ambient");
        write_version_fixture(&instance, "cursor-agent", "2026.03.04-instance");
        let output = sandbox.run_isolated_case(
            CASE,
            TEST_NAME,
            &[
                ("PATH", ambient.as_os_str()),
                ("BIBCODE_TEST_INSTANCE_PATH", instance.as_os_str()),
                ("BIBCODE_TEST_ROOT", sandbox.root().as_os_str()),
                (
                    "BIBCODE_TEST_AMBIENT_EXECUTABLE",
                    ambient_executable.as_os_str(),
                ),
            ],
        );
        assert!(
            output.status.success(),
            "isolated inventory PATH precedence case failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains(SENTINEL));
    }

    /// Reads the catalogs of a real OpenCode 2 server (see `live_opencode_2_round_trip` in
    /// `provider::opencode` for the environment).
    #[tokio::test]
    #[ignore = "needs a live OpenCode 2 server"]
    async fn live_opencode_2_inventory() {
        let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"));
        let endpoint = var("BIBCODE_OPENCODE_V2_LIVE");
        let password = var("BIBCODE_OPENCODE_V2_PASSWORD");
        assert_eq!(
            opencode_server_protocol(&endpoint, Some(&password)).await,
            Ok(opencode::OpenCodeProtocol::V2)
        );
        let inventory = probe_opencode(
            &endpoint,
            Some(&password),
            &[],
            opencode::OpenCodeProtocol::V2,
            Path::new(&var("BIBCODE_OPENCODE_V2_DIRECTORY")),
        )
        .await
        .expect("catalogs");
        assert!(
            inventory
                .models
                .iter()
                .any(|model| model.slug == var("BIBCODE_OPENCODE_V2_MODEL"))
        );
        assert!(
            inventory
                .agents
                .iter()
                .any(|agent| agent["name"] == "build")
        );
        assert!(!inventory.skills.is_empty());
    }

    /// Starts a real OpenCode 2 binary the way inventory does and reads its catalogs. Set
    /// `BIBCODE_OPENCODE_V2_BINARY` (a copy under the temporary directory, which the hermetic
    /// test guard allows), `BIBCODE_OPENCODE_V2_HOME` (a scratch home whose configuration points
    /// the default model at a scripted local endpoint), `BIBCODE_OPENCODE_V2_DIRECTORY` and
    /// `BIBCODE_OPENCODE_V2_MODEL`.
    #[tokio::test]
    #[ignore = "needs a live OpenCode 2 binary"]
    async fn live_opencode_2_local_inventory() {
        let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"));
        let home = std::path::PathBuf::from(var("BIBCODE_OPENCODE_V2_HOME"));
        let environment = [
            ("HOME", home.clone()),
            ("XDG_CONFIG_HOME", home.join(".config")),
            ("XDG_DATA_HOME", home.join(".local/share")),
            ("XDG_STATE_HOME", home.join(".local/state")),
            ("XDG_CACHE_HOME", home.join(".cache")),
        ]
        .map(|(name, value)| (OsString::from(name), value.into_os_string()));
        let inventory = probe_local_opencode(
            Path::new(&var("BIBCODE_OPENCODE_V2_BINARY")),
            Path::new(&var("BIBCODE_OPENCODE_V2_DIRECTORY")),
            &[],
            &environment,
        )
        .await
        .expect("the managed OpenCode 2 server answers its catalogs");
        assert!(
            inventory
                .models
                .iter()
                .any(|model| model.slug == var("BIBCODE_OPENCODE_V2_MODEL"))
        );
    }

    /// A CLI whose version BiBCode cannot drive is reported without starting its server.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_unknown_opencode_cli_version_is_reported_without_starting_a_server() {
        let root = tempfile::tempdir().expect("inventory root");
        let marker = root.path().join("served");
        let executable = root.path().join("opencode");
        crate::test_support::executable_fixture::write_executable(
            &executable,
            format!(
                "#!/bin/sh\nif [ \"$1\" = --version ]; then printf 'opencode v3.0.0\\n'; exit 0; fi\n: > '{}'\nexit 1\n",
                marker.to_string_lossy()
            ),
        );
        let settings = json!({
            "enableProviderUpdateChecks": false,
            "providerInstances": {
                "opencode": {
                    "driver": "opencode",
                    "enabled": true,
                    "config": { "binaryPath": executable }
                }
            }
        });

        let providers = probe_full(
            &settings,
            Some("opencode"),
            root.path(),
            &ProviderMaintenance::new(),
        )
        .await;

        let snapshot = &providers[0].snapshot;
        assert_eq!(snapshot["version"], "opencode v3.0.0");
        assert_eq!(snapshot["status"], "error");
        assert_eq!(
            snapshot["message"],
            "This OpenCode version isn't supported yet. In Settings → Providers, set Binary path to an OpenCode 1.x or 2.x executable."
        );
        assert!(!marker.exists(), "no server may be started");
    }

    /// The quick probe reads OpenCode's `--version` too, so a version BiBCode cannot drive never
    /// reads Ready, while a supported one stays ready.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_quick_probe_reports_an_unknown_opencode_version() {
        for (version, status) in [("opencode v3.0.0", "error"), ("opencode v2.0.29", "ready")] {
            let root = tempfile::tempdir().expect("inventory root");
            let executable = root.path().join("opencode");
            crate::test_support::executable_fixture::write_executable(
                &executable,
                format!(
                    "#!/bin/sh\nif [ \"$1\" = --version ]; then printf '{version}\\n'; exit 0; fi\nexit 1\n"
                ),
            );
            let settings = json!({
                "enableProviderUpdateChecks": false,
                "providerInstances": {
                    "opencode": {
                        "driver": "opencode",
                        "enabled": true,
                        "config": { "binaryPath": executable }
                    }
                }
            });

            let providers = probe(
                &settings,
                Some("opencode"),
                root.path(),
                &ProviderMaintenance::new(),
            )
            .await;

            let snapshot = &providers[0].snapshot;
            assert_eq!(snapshot["status"], status, "{version}");
            if status == "error" {
                assert_eq!(snapshot["version"], version);
                assert_eq!(
                    snapshot["message"],
                    "This OpenCode version isn't supported yet. In Settings → Providers, set Binary path to an OpenCode 1.x or 2.x executable."
                );
            }
        }
    }

    /// An OpenCode 2 server's catalogs, as captured from 2.0.29, behind its `/api/info`.
    fn opencode_2_catalog_server() -> axum::Router {
        opencode_2_catalog_server_with_cold_agents(0)
    }

    /// As `opencode_2_catalog_server`, but its first `cold` agent lists are empty, as a
    /// just-started OpenCode 2 server answers before it has loaded its agents.
    fn opencode_2_catalog_server_with_cold_agents(cold: usize) -> axum::Router {
        use axum::{extract::OriginalUri, http::HeaderMap, routing::any};
        let agent_reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let path = format!(
            "{}/tests/fixtures/opencode-v2/catalog.exchanges.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let exchanges: Value =
            serde_json::from_str(&std::fs::read_to_string(path).expect("catalog fixture"))
                .expect("catalog JSON");
        let app = crate::provider::opencode::server_fixtures::v2("secret");
        app.route(
            "/api/{route}",
            any(move |uri: OriginalUri, headers: HeaderMap| {
                let exchanges = exchanges.clone();
                let agent_reads = agent_reads.clone();
                async move {
                    let expected = format!(
                        "Basic {}",
                        base64::Engine::encode(
                            &base64::engine::general_purpose::STANDARD,
                            "opencode:secret"
                        )
                    );
                    assert_eq!(headers["authorization"], expected.as_str());
                    assert!(
                        uri.0
                            .query()
                            .is_some_and(|query| query.starts_with("location%5Bdirectory%5D=")),
                        "catalogs are read for the session directory: {}",
                        uri.0
                    );
                    let route = uri.0.path().to_owned();
                    if route == "/api/agent"
                        && agent_reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < cold
                    {
                        return Json(json!({ "data": [] }));
                    }
                    Json(
                        exchanges["exchanges"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .find(|exchange| {
                                exchange["method"] == "GET"
                                    && exchange["path"].as_str().unwrap().split('?').next()
                                        == Some(route.as_str())
                            })
                            .map(|exchange| exchange["response"].clone())
                            .unwrap_or_else(|| json!({ "data": [] })),
                    )
                }
            }),
        )
    }

    /// A just-started OpenCode 2 server lists no agents for its first ~100 ms (seen on 2.0.29),
    /// though it always has built-in ones; its catalogs are read again until they are listed.
    #[tokio::test]
    async fn a_just_started_opencode_2_server_is_read_again_until_its_agents_load() {
        use crate::provider::opencode::server_fixtures;
        let sandbox = tempfile::tempdir().unwrap();
        let missing =
            crate::test_support::hermetic_providers::missing_provider_executable(sandbox.path());
        let (endpoint, server) =
            server_fixtures::serve(opencode_2_catalog_server_with_cold_agents(2)).await;
        let settings = json!({
            "enableProviderUpdateChecks": false,
            "providerInstances": {
                "remote": {
                    "driver": "opencode",
                    "enabled": true,
                    "config": {
                        "binaryPath": missing,
                        "serverUrl": endpoint,
                        "serverPassword": "secret"
                    }
                }
            }
        });

        let providers = probe_full(
            &settings,
            Some("remote"),
            sandbox.path(),
            &ProviderMaintenance::new(),
        )
        .await;
        server.abort();

        let agents = providers[0].snapshot["agents"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            agents.iter().any(|agent| agent["name"] == "build"),
            "{agents:?}"
        );
    }

    #[tokio::test]
    async fn opencode_server_urls_report_their_catalogs_or_an_unsupported_version() {
        use crate::provider::opencode::server_fixtures;
        let sandbox = tempfile::tempdir().unwrap();
        let missing =
            crate::test_support::hermetic_providers::missing_provider_executable(sandbox.path());
        for (app, version) in [
            (opencode_2_catalog_server(), "2.0.29"),
            // Answers `/api/info` but none of the catalogs.
            (server_fixtures::v2("secret"), "2 without catalogs"),
            (server_fixtures::api_server("secret", "3.0.0"), "3.0.0"),
        ] {
            let (endpoint, server) = server_fixtures::serve(app).await;
            let settings = json!({
                "enableProviderUpdateChecks": false,
                "providerInstances": {
                    "remote": {
                        "driver": "opencode",
                        "enabled": true,
                        "config": {
                            "binaryPath": missing,
                            "serverUrl": endpoint,
                            "serverPassword": "secret"
                        }
                    }
                }
            });

            let providers = probe_full(
                &settings,
                Some("remote"),
                sandbox.path(),
                &ProviderMaintenance::new(),
            )
            .await;
            server.abort();

            let snapshot = &providers[0].snapshot;
            if version == "2.0.29" {
                assert_eq!(snapshot["status"], "ready");
                assert!(snapshot.get("message").is_none_or(Value::is_null));
                assert_eq!(snapshot["models"][0]["slug"], "mock/mock-model");
                let skills = snapshot["skills"].as_array().expect("skills");
                assert!(skills.iter().any(|skill| skill["name"] == "report"));
            } else if version == "2 without catalogs" {
                assert_eq!(snapshot["installed"], true);
                assert_eq!(snapshot["status"], "ready");
                assert_eq!(snapshot["message"], OPENCODE_2_CATALOG_UNREADABLE);
            } else {
                assert_eq!(snapshot["status"], "error");
                assert_eq!(
                    snapshot["message"],
                    "The OpenCode server at Server URL runs 3.0.0, which isn't supported yet. In Settings → Providers, point Server URL at an OpenCode 1.x or 2.x server."
                );
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exact_target_probe_applies_the_same_case_variant_path_used_for_resolution() {
        let root = tempfile::tempdir().expect("inventory PATH root");
        let first = root.path().join("first");
        let second = root.path().join("second");
        std::fs::create_dir_all(&first).expect("first PATH directory");
        std::fs::create_dir_all(&second).expect("second PATH directory");
        let executable = first.join("codex");
        crate::test_support::executable_fixture::write_executable(
            &executable,
            format!(
                "#!/bin/sh\nif [ \"$PATH\" = '{}' ]; then printf '1.2.3\\n'; else printf '9.9.9\\n'; fi\n",
                first.to_string_lossy()
            ),
        );
        let target = ProviderMaintenanceTarget {
            instance_id: "codex-work".to_owned(),
            driver: "codex".to_owned(),
            binary_path: "codex".to_owned(),
            environment: vec![
                ("pAtH".into(), first.as_os_str().to_owned()),
                ("PATH".into(), second.as_os_str().to_owned()),
            ],
        };

        assert_eq!(
            probe_maintenance_target_version(&target, root.path()).await,
            Some("1.2.3".to_owned())
        );
    }

    #[test]
    fn maintenance_target_rejects_mismatched_instance_driver_pairs() {
        let settings = json!({
            "providerInstances": {
                "configured-codex": {
                    "driver": "codex",
                    "config": { "binaryPath": "custom-codex" }
                }
            }
        });

        let target = maintenance_target(&settings, "codex", Some("configured-codex"))
            .expect("matching provider target");
        assert_eq!(target.binary_path, "custom-codex");
        assert!(maintenance_target(&settings, "claudeAgent", Some("configured-codex")).is_none());
    }

    #[tokio::test]
    async fn quick_and_full_disabled_probes_attach_unknown_advisories() {
        let sandbox = tempfile::tempdir().unwrap();
        let missing =
            crate::test_support::hermetic_providers::missing_provider_executable(sandbox.path());
        let settings = json!({
            "providerInstances": {
                "codex": { "driver": "codex", "enabled": false, "config": {"binaryPath": missing} }
            }
        });
        let maintenance = ProviderMaintenance::new();
        let quick = probe(&settings, Some("codex"), Path::new("."), &maintenance).await;
        let full = probe_full(&settings, Some("codex"), Path::new("."), &maintenance).await;

        for result in [quick, full] {
            assert_eq!(result[0].rich_metadata, RichMetadataOutcome::NotRequested);
            assert!(!result[0].snapshot["models"].as_array().unwrap().is_empty());
            assert_eq!(result[0].snapshot["versionAdvisory"]["status"], "unknown");
            assert!(result[0].snapshot["versionAdvisory"]["canUpdate"].is_boolean());
            assert!(result[0].snapshot["versionAdvisory"]["currentVersion"].is_null());
            assert!(result[0].snapshot["versionAdvisory"]["latestVersion"].is_null());
        }
    }

    #[test]
    fn provider_probe_timeouts_allow_slow_windows_cli_startup() {
        assert!(PROBE_TIMEOUT >= Duration::from_secs(10));
        assert!(LOCAL_OPENCODE_INVENTORY_TIMEOUT >= Duration::from_secs(30));
    }

    #[test]
    fn fresh_cursor_inventory_is_enabled_with_the_supported_binary() {
        let fresh_definitions = definitions(&json!({}));
        let cursor = fresh_definitions
            .iter()
            .find(|definition| definition.instance_id == "cursor")
            .expect("built-in Cursor definition");

        assert!(cursor.enabled);
        assert_eq!(cursor.binary_path, "cursor-agent");

        let explicit = definitions(&json!({
            "providerInstances": {
                "cursor": {
                    "driver": "cursor",
                    "enabled": true,
                    "config": {}
                }
            }
        }));
        let explicit_cursor = explicit
            .iter()
            .find(|definition| definition.instance_id == "cursor")
            .expect("explicit Cursor definition");
        assert_eq!(explicit_cursor.binary_path, "cursor-agent");
    }

    #[test]
    fn configured_instances_do_not_hide_enabled_legacy_providers() {
        let settings = json!({
            "providerInstances": {
                "cursor": {
                    "driver": "cursor",
                    "enabled": true,
                    "config": { "binaryPath": "agent" }
                }
            },
            "providers": {
                "codex": { "enabled": true, "binaryPath": "codex" },
                "claudeAgent": { "enabled": true, "binaryPath": "claude" },
                "cursor": { "enabled": false, "binaryPath": "agent" },
                "grok": { "enabled": true, "binaryPath": "grok" },
                "opencode": { "enabled": true, "binaryPath": "opencode" }
            }
        });

        let definitions = definitions(&settings);
        let drivers = definitions
            .iter()
            .map(|definition| definition.driver.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            drivers,
            ["cursor", "codex", "claudeAgent", "grok", "opencode"]
        );
        assert_eq!(
            drivers.iter().filter(|driver| **driver == "cursor").count(),
            1
        );
    }

    /// A launch reads an instance's custom models exactly as the catalog does, so the models the
    /// catalog offers are the models the launch accepts.
    #[test]
    fn instance_custom_models_are_the_ones_the_catalog_offers() {
        let settings = json!({
            "providers": { "codex": { "customModels": ["legacy-custom"] } },
            "providerInstances": {
                "codex-work": {
                    "driver": "codex",
                    "config": { "customModels": [" work-custom ", ""] }
                },
                "codex-plain": { "driver": "codex" }
            }
        });
        assert_eq!(
            instance_custom_models(&settings, "codex-work"),
            vec!["work-custom"]
        );
        // An instance without a list of its own takes its driver's legacy list.
        assert_eq!(
            instance_custom_models(&settings, "codex-plain"),
            vec!["legacy-custom"]
        );
        assert!(instance_custom_models(&settings, "codex-missing").is_empty());
        // Settings without instances offer the legacy list on the default instance.
        assert_eq!(
            instance_custom_models(
                &json!({ "providers": { "codex": { "customModels": ["legacy-custom"] } } }),
                "codex"
            ),
            vec!["legacy-custom"]
        );
    }

    #[test]
    fn grok_inventory_stays_disabled_when_saved_settings_enable_it() {
        let legacy = definitions(&json!({
            "providers": {
                "grok": { "enabled": true, "binaryPath": "grok" }
            }
        }));
        let legacy_grok = legacy
            .iter()
            .find(|definition| definition.instance_id == "grok")
            .expect("legacy Grok definition");
        assert!(!legacy_grok.enabled);

        let custom = definitions(&json!({
            "providerInstances": {
                "grok_work": {
                    "driver": "grok",
                    "enabled": true,
                    "config": { "binaryPath": "grok-work" }
                }
            }
        }));
        let custom_grok = custom
            .iter()
            .find(|definition| definition.instance_id == "grok_work")
            .expect("custom Grok definition");
        assert!(!custom_grok.enabled);
    }

    #[test]
    fn codex_inventory_keeps_effort_but_not_unverified_fast_before_live_discovery() {
        let definitions = definitions(&json!({
            "providerInstances": {
                "codex": { "driver": "codex", "enabled": true, "config": {} }
            },
            "providerSessionDefaults": {
                "codex": {
                    "model": "gpt-configured",
                    "options": [
                        { "id": "reasoningEffort", "value": "xhigh" },
                        { "id": "serviceTier", "value": "fast" }
                    ]
                }
            }
        }));
        let models = provider_models_without_version(&definitions[0]);

        assert_eq!(models[0]["slug"], "gpt-configured");
        assert_eq!(
            models[0]["capabilities"]["optionDescriptors"][0]["currentValue"],
            "xhigh"
        );
        assert_eq!(
            models[0]["capabilities"]["optionDescriptors"]
                .as_array()
                .expect("fallback option descriptors")
                .len(),
            1
        );
    }

    #[test]
    fn codex_inventory_marks_native_skills_as_dollar_invoked() {
        let inventory = codex_skill_inventory(&[codex::CodexProviderSkill {
            name: "refactor".to_owned(),
            path: "/fixture/refactor/SKILL.md".to_owned(),
            enabled: true,
            description: Some("Refactor fixture code".to_owned()),
            scope: Some("project".to_owned()),
            display_name: None,
            short_description: None,
        }]);

        assert_eq!(inventory.len(), 1);
        assert_eq!(inventory[0]["name"], "refactor");
        assert_eq!(inventory[0]["invocation"], "dollar");
    }

    #[test]
    fn provider_snapshots_stamp_instance_labels() {
        let definitions = definitions(&json!({
            "providerInstances": {
                "codex_personal": { "driver": "codex" },
                "codex": { "driver": "codex" },
                "codex_work": { "driver": "codex", "displayName": "  Work Codex  " }
            }
        }));
        let labels = ["codex_personal", "codex", "codex_work"].map(|instance_id| {
            snapshot_owned_message(
                definitions
                    .iter()
                    .find(|definition| definition.instance_id == instance_id)
                    .expect("configured provider definition"),
                true,
                None,
                "ready",
                json!({ "status": "authenticated" }),
                Vec::new(),
                ProviderCapabilities::default(),
                None,
                "2026-08-01T00:00:00.000Z".to_owned(),
            )["displayName"]
                .clone()
        });

        assert_eq!(
            labels,
            [json!("Codex Personal"), json!("Codex"), json!("Work Codex")]
        );
    }

    #[test]
    fn provider_inventory_advertises_measured_toolbar_capabilities() {
        let definitions = definitions(&json!({}));
        let snapshot_for = |driver| {
            snapshot_owned_message(
                definitions
                    .iter()
                    .find(|definition| definition.driver == driver)
                    .expect("built-in provider definition"),
                true,
                None,
                "ready",
                json!({ "status": "authenticated" }),
                Vec::new(),
                ProviderCapabilities::default(),
                None,
                "2026-08-01T00:00:00.000Z".to_owned(),
            )
        };
        let codex = snapshot_for("codex");
        let claude = snapshot_for("claudeAgent");
        let cursor = snapshot_for("cursor");
        let grok = snapshot_for("grok");
        let opencode = snapshot_for("opencode");

        assert_eq!(codex["supportsConversationRewind"], true);
        assert_eq!(opencode["supportsConversationRewind"], true);
        assert_eq!(claude["supportsConversationRewind"], false);
        assert_eq!(cursor["supportsConversationRewind"], false);
        assert_eq!(grok["supportsConversationRewind"], false);
        assert_eq!(codex["supportsTurnSteer"], true);
        assert_eq!(claude["supportsTurnSteer"], true);
        assert!(cursor.get("supportsTurnSteer").is_none());
        assert!(grok.get("supportsTurnSteer").is_none());
        assert!(opencode.get("supportsTurnSteer").is_none());
        assert_eq!(codex["supportsMcpStatus"], true);
        assert_eq!(claude["supportsMcpStatus"], true);
        assert!(cursor.get("supportsMcpStatus").is_none());
        assert!(grok.get("supportsMcpStatus").is_none());
        assert!(opencode.get("supportsMcpStatus").is_none());
        assert_eq!(codex["supportsContextWindowUsage"], true);
        assert_eq!(claude["supportsContextWindowUsage"], true);
        assert!(cursor.get("supportsContextWindowUsage").is_none());
        assert!(grok.get("supportsContextWindowUsage").is_none());
        assert!(opencode.get("supportsContextWindowUsage").is_none());
    }

    #[test]
    fn codex_inventory_keeps_legacy_effort_but_not_unverified_fast_before_live_discovery() {
        let definitions = definitions(&json!({
            "providerInstances": {
                "codex": { "driver": "codex", "enabled": true, "config": {} }
            },
            "providerSessionDefaults": {
                "codex": {
                    "model": "gpt-legacy",
                    "options": {
                        "effort": "xhigh",
                        "fastMode": true
                    }
                }
            }
        }));
        let models = provider_models_without_version(&definitions[0]);

        assert_eq!(models[0]["slug"], "gpt-legacy");
        assert_eq!(
            models[0]["capabilities"]["optionDescriptors"][0]["currentValue"],
            "xhigh"
        );
        assert_eq!(
            models[0]["capabilities"]["optionDescriptors"]
                .as_array()
                .expect("fallback option descriptors")
                .len(),
            1
        );
    }

    #[test]
    fn codex_cli_version_is_normalized_to_semver() {
        assert_eq!(
            normalize_provider_version("codex", Some("codex-cli 0.144.1".to_owned())),
            Some("0.144.1".to_owned())
        );
    }

    #[test]
    fn codex_required_auth_is_not_reported_as_ready() {
        let account = json!({ "account": null, "requiresOpenaiAuth": true });

        assert_eq!(
            codex_probe_health(&account),
            (
                "error",
                json!({ "status": "unauthenticated" }),
                Some("Codex is installed but requires authentication.")
            )
        );
    }

    #[test]
    fn codex_authenticated_account_remains_ready_when_openai_auth_is_required() {
        let account = json!({
            "account": {
                "type": "chatgpt",
                "email": "user@example.com"
            },
            "requiresOpenaiAuth": true
        });

        assert_eq!(
            codex_probe_health(&account),
            (
                "ready",
                json!({
                    "status": "authenticated",
                    "type": "chatgpt",
                    "email": "user@example.com"
                }),
                None
            )
        );
    }

    #[test]
    fn codex_without_required_openai_auth_remains_ready() {
        let account = json!({ "account": null, "requiresOpenaiAuth": false });

        assert_eq!(
            codex_probe_health(&account),
            ("ready", json!({ "status": "unauthenticated" }), None)
        );
    }

    #[test]
    fn grok_inventory_has_a_builtin_model_before_acp_discovery() {
        let definition = ProviderDefinition {
            instance_id: "grok".to_owned(),
            driver: "grok".to_owned(),
            display_name: None,
            enabled: true,
            binary_path: "grok".to_owned(),
            available: true,
            custom_models: Vec::new(),
            configured_model: None,
            configured_effort: None,
            configured_service_tier: None,
            endpoint: None,
            server_password: None,
            environment: Vec::new(),
        };

        assert_eq!(
            provider_models_without_version(&definition)[0]["slug"],
            "grok-build"
        );
    }

    #[test]
    fn provider_environment_excludes_redacted_and_empty_entries() {
        let instance = json!({
            "environment": [
                { "name": "API_KEY", "value": "secret", "valueRedacted": false },
                { "name": "pAtH", "value": "/first", "valueRedacted": false },
                { "name": "PATH", "value": "/second", "valueRedacted": false },
                { "name": "HIDDEN", "value": "redacted", "valueRedacted": true },
                { "name": "", "value": "ignored", "valueRedacted": false }
            ]
        });
        assert_eq!(
            provider_environment(&instance),
            vec![
                (OsString::from("API_KEY"), OsString::from("secret")),
                (OsString::from("PATH"), OsString::from("/first"))
            ]
        );
    }

    #[test]
    fn claude_initialization_exposes_commands_and_agents() {
        let metadata = parse_claude_initialization_response(
            &json!({
                "commands": [
                    { "name": "goal", "description": "Keep working", "argumentHint": "[condition]" },
                    { "name": "loop", "description": "Run repeatedly", "argumentHint": "[interval] [prompt]" }
                ],
                "agents": [
                    { "name": "code-reviewer", "description": "Reviews code", "model": "opus" }
                ]
            }),
            &[],
        );
        let capabilities = metadata.capabilities;

        assert_eq!(capabilities.slash_commands[0]["name"], "goal");
        assert_eq!(
            capabilities.slash_commands[0]["input"]["hint"],
            "[condition]"
        );
        assert_eq!(capabilities.slash_commands[1]["name"], "loop");
        assert_eq!(capabilities.agents[0]["name"], "code-reviewer");
        assert_eq!(capabilities.agents[0]["model"], "opus");

        let mut models = vec![json!({"capabilities": {}})];
        apply_agent_options(&mut models, &[json!({"name":"reviewer"})]);
        assert_eq!(
            models[0]["capabilities"]["optionDescriptors"][0]["currentValue"],
            "reviewer"
        );
    }

    #[test]
    fn claude_reload_skills_uses_provider_native_slash_invocation() {
        let skills = parse_claude_skills_response(&json!({
            "skills": [
                { "name": "loop", "description": "Run repeatedly", "argumentHint": "[interval] [prompt]" },
                { "name": "loop", "description": "Duplicate lower-priority skill" }
            ]
        }), &json!({"commands":[{"name":"loop"}]}))
        .expect("valid skills response");

        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0]["name"], "loop");
        assert_eq!(skills[0]["invocation"], "slash");
        assert_eq!(skills[0]["path"], "claude://skill/loop");
    }

    #[test]
    fn claude_skills_prioritize_primary_command_names_over_aliases() {
        let skills = parse_claude_skills_response(
            &json!({"skills":[{"name":"beta"}]}),
            &json!({"commands":[{"name":"alpha","aliases":["beta"]},{"name":"beta"}]}),
        )
        .expect("valid native catalogs");
        assert_eq!(skills[0]["name"], "beta");
    }

    #[test]
    fn claude_skills_probe_distinguishes_empty_inventory_from_parse_failure() {
        assert_eq!(
            parse_claude_skills_response(&json!({ "skills": [] }), &json!({"commands":[]})),
            Some(Vec::new())
        );
        assert_eq!(
            parse_claude_skills_response(&json!({ "skills": "invalid" }), &json!({"commands":[]})),
            None
        );
    }

    #[test]
    fn claude_inventory_probe_disables_hooks_without_disabling_capabilities() {
        assert!(
            CLAUDE_CAPABILITY_PROBE_ARGS
                .windows(2)
                .any(|pair| pair == ["--settings", r#"{"disableAllHooks":true}"#])
        );
        assert!(CLAUDE_CAPABILITY_PROBE_ARGS.contains(&"--setting-sources=user,project,local"));
        assert!(!CLAUDE_CAPABILITY_PROBE_ARGS.contains(&"--safe-mode"));
        assert!(!CLAUDE_CAPABILITY_PROBE_ARGS.contains(&"--bare"));
    }

    #[test]
    fn opencode_command_inventory_preserves_arguments() {
        let commands = opencode::model::command_inventory(&json!([
            {
                "name": "review",
                "description": "Review changes",
                "template": "Review $ARGUMENTS"
            }
        ]));

        assert_eq!(commands[0]["name"], "review");
        assert_eq!(commands[0]["input"]["hint"], "arguments");
    }

    #[tokio::test]
    async fn full_probe_does_not_serialize_independent_provider_inventories() {
        let sandbox = tempfile::tempdir().unwrap();
        let missing =
            crate::test_support::hermetic_providers::missing_provider_executable(sandbox.path());
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(6));
        let app = Router::new()
            .route(
                "/global/health",
                get(|| async { Json(json!({ "healthy": true })) }),
            )
            .route(
                "/{*path}",
                get(move || {
                    let barrier = barrier.clone();
                    async move {
                        barrier.wait().await;
                        Json(json!({}))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake OpenCode server");
        let endpoint = format!("http://{}", listener.local_addr().expect("local address"));
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve fake OpenCode API");
        });
        let settings = json!({
            "enableProviderUpdateChecks": false,
            "providers": {
                "codex": { "enabled": false, "binaryPath": missing },
                "claudeAgent": { "enabled": false, "binaryPath": missing },
                "cursor": { "enabled": false, "binaryPath": missing },
                "grok": { "enabled": false, "binaryPath": missing },
                "opencode": { "enabled": false, "binaryPath": missing }
            },
            "providerInstances": {
                "first": {
                    "driver": "opencode",
                    "enabled": true,
                    "config": { "serverUrl": endpoint }
                },
                "second": {
                    "driver": "opencode",
                    "enabled": true,
                    "config": { "serverUrl": endpoint }
                }
            }
        });

        let maintenance = ProviderMaintenance::new();
        let result = timeout(
            Duration::from_secs(2),
            probe_full(&settings, None, Path::new("."), &maintenance),
        )
        .await;
        server.abort();

        let result = result.expect("independent provider probes should not block each other");
        assert_eq!(result[0].snapshot["instanceId"], "first");
        assert_eq!(result[1].snapshot["instanceId"], "second");
    }

    #[tokio::test]
    async fn opencode_partial_inventory_is_not_authoritative() {
        let app = Router::new()
            .route(
                "/provider",
                get(|| async {
                    Json(json!({
                        "all": [{
                            "id": "openai",
                            "name": "OpenAI",
                            "models": {
                                "gpt-5": {
                                    "id": "gpt-5",
                                    "providerID": "openai",
                                    "name": "GPT-5"
                                }
                            }
                        }],
                        "connected": ["openai"],
                        "default": { "openai": "gpt-5" }
                    }))
                }),
            )
            .route(
                "/agent",
                get(|| async {
                    Json(json!([{
                        "name": "build",
                        "description": "Default agent",
                        "mode": "primary",
                        "native": true
                    }]))
                }),
            )
            .route(
                "/command",
                get(|| async {
                    sleep(Duration::from_millis(200)).await;
                    Json(json!([{
                        "name": "review",
                        "description": "Review changes",
                        "template": "Review $ARGUMENTS"
                    }]))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake OpenCode server");
        let endpoint = format!("http://{}", listener.local_addr().expect("local address"));
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve fake OpenCode API");
        });

        let inventory = probe_opencode_with_timeout(
            &endpoint,
            None,
            &["custom/model".to_owned()],
            Duration::from_millis(75),
            opencode::OpenCodeProtocol::V1,
            Path::new("."),
        )
        .await;
        server.abort();

        assert!(inventory.is_none());
        assert!(
            probe_opencode(
                "http://127.0.0.1:1",
                None,
                &[],
                opencode::OpenCodeProtocol::V1,
                Path::new("."),
            )
            .await
            .is_none()
        );
    }

    #[tokio::test]
    async fn cursor_discovers_project_commands_skills_and_agents() {
        let workspace = tempfile::tempdir().expect("temporary Cursor workspace");
        let command_directory = workspace.path().join(".cursor/commands");
        let skill_directory = workspace.path().join(".cursor/skills/review-code");
        let agent_directory = workspace.path().join(".cursor/agents");
        tokio::fs::create_dir_all(&command_directory)
            .await
            .expect("create Cursor command directory");
        tokio::fs::create_dir_all(&skill_directory)
            .await
            .expect("create Cursor skill directory");
        tokio::fs::create_dir_all(&agent_directory)
            .await
            .expect("create Cursor agent directory");
        tokio::fs::write(
            command_directory.join("review.md"),
            "Review the current changes.",
        )
        .await
        .expect("write Cursor command");
        tokio::fs::write(skill_directory.join("SKILL.md"), "# Review code")
            .await
            .expect("write Cursor skill");
        tokio::fs::write(
            agent_directory.join("reviewer.md"),
            "Review code carefully.",
        )
        .await
        .expect("write Cursor agent");

        let capabilities = cursor::discover_workspace_capabilities(workspace.path()).await;

        assert!(
            capabilities
                .slash_commands
                .iter()
                .any(|command| command["name"] == "review")
        );
        assert!(capabilities.skills.iter().any(|skill| {
            skill["name"] == "review-code"
                && skill["scope"] == "project"
                && skill["invocation"] == "slash"
        }));
        assert!(
            capabilities
                .agents
                .iter()
                .any(|agent| agent["name"] == "reviewer")
        );
    }

    #[tokio::test]
    async fn cursor_discovers_only_the_configured_environment_home() {
        let workspace = tempfile::tempdir().expect("temporary Cursor workspace");
        let configured_home = tempfile::tempdir().expect("temporary configured Cursor home");
        let command_directory = configured_home.path().join(".cursor/commands");
        let skill_directory = configured_home.path().join(".cursor/skills/user-review");
        let agent_directory = configured_home.path().join(".cursor/agents");
        tokio::fs::create_dir_all(&command_directory)
            .await
            .expect("create configured Cursor command directory");
        tokio::fs::create_dir_all(&skill_directory)
            .await
            .expect("create configured Cursor skill directory");
        tokio::fs::create_dir_all(&agent_directory)
            .await
            .expect("create configured Cursor agent directory");
        tokio::fs::write(
            command_directory.join("configured-review.md"),
            "Review from the configured user home.",
        )
        .await
        .expect("write configured Cursor command");
        tokio::fs::write(skill_directory.join("SKILL.md"), "# Configured review")
            .await
            .expect("write configured Cursor skill");
        tokio::fs::write(
            agent_directory.join("configured-reviewer.md"),
            "Review from the configured user home.",
        )
        .await
        .expect("write configured Cursor agent");
        let home_variable = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let environment = vec![(
            OsString::from(home_variable),
            configured_home.path().as_os_str().to_owned(),
        )];

        let capabilities = cursor::discover_workspace_capabilities_with_environment(
            workspace.path(),
            &environment,
        )
        .await;

        assert_eq!(
            capabilities
                .slash_commands
                .iter()
                .map(|command| command["name"].as_str().expect("Cursor command name"))
                .collect::<Vec<_>>(),
            vec!["configured-review"]
        );
        assert_eq!(
            capabilities
                .skills
                .iter()
                .map(|skill| (
                    skill["name"].as_str().expect("Cursor skill name"),
                    skill["scope"].as_str().expect("Cursor skill scope"),
                ))
                .collect::<Vec<_>>(),
            vec![("user-review", "user")]
        );
        assert_eq!(
            capabilities
                .agents
                .iter()
                .map(|agent| agent["name"].as_str().expect("Cursor agent name"))
                .collect::<Vec<_>>(),
            vec!["configured-reviewer"]
        );
    }

    #[test]
    fn built_in_capabilities_include_provider_specific_goal_and_loop_commands() {
        let codex = built_in_slash_commands("codex");
        let claude = built_in_slash_commands("claudeAgent");
        let cursor = built_in_slash_commands("cursor");
        let grok = built_in_slash_commands("grok");

        assert!(codex.iter().any(|command| command["name"] == "goal"));
        assert!(claude.iter().any(|command| command["name"] == "goal"));
        assert!(claude.iter().any(|command| command["name"] == "loop"));
        assert!(cursor.iter().any(|command| command["name"] == "models"));
        assert!(cursor.iter().any(|command| command["name"] == "rules"));
        assert!(cursor.iter().any(|command| command["name"] == "commands"));
        assert!(grok.iter().any(|command| command["name"] == "loop"));
        assert!(grok.iter().any(|command| command["name"] == "agents"));
        assert!(grok.iter().any(|command| command["name"] == "skills"));
    }
}
