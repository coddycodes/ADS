use coddy_studio_server::server_settings;

use server_settings::{
    ProviderEnvironmentVariableInput, ProviderInstanceInput, ProviderOptionSelectionState,
    ProviderOptionSelectionValueState, ProviderSessionDefaultState, ProviderSettingsPatch,
    ProviderSettingsState, ProviderSettingsStore, ProvidersPatch, ServerSettingsPatch,
    ServerSettingsReadError,
};
use tempfile::TempDir;

#[tokio::test]
async fn worktree_workspace_roundtrips_and_empty_is_the_default() {
    let temp = tempfile::tempdir().expect("settings root");
    let store = ProviderSettingsStore::new(temp.path());

    assert_eq!(
        store
            .get()
            .await
            .expect("default settings")
            .worktree_base_directory,
        ""
    );
    let updated = store
        .update(ServerSettingsPatch {
            worktree_base_directory: Some("  /tmp/coddy-studio-worktrees  ".to_owned()),
            ..ServerSettingsPatch::default()
        })
        .await
        .expect("workspace update");
    assert_eq!(
        updated.worktree_base_directory,
        "/tmp/coddy-studio-worktrees"
    );

    let reopened = ProviderSettingsStore::new(temp.path());
    assert_eq!(
        reopened
            .get()
            .await
            .expect("reloaded settings")
            .worktree_base_directory,
        "/tmp/coddy-studio-worktrees"
    );
}

#[tokio::test]
async fn agent_activity_defaults_chat_on_and_terminal_off() {
    let state = tempfile::tempdir().expect("state");
    let settings = ProviderSettingsStore::new(state.path())
        .get()
        .await
        .expect("settings");
    assert!(settings.enable_chat_agent_activity);
    assert!(!settings.enable_terminal_agent_activity);
}

#[tokio::test]
async fn automatic_git_fetch_defaults_to_three_minutes() {
    let state = tempfile::tempdir().expect("state");
    let settings = ProviderSettingsStore::new(state.path())
        .get()
        .await
        .expect("settings");
    assert_eq!(settings.automatic_git_fetch_interval, 180_000);
}

#[tokio::test]
async fn legacy_agent_activity_migrates_only_to_chat() {
    let state = tempfile::tempdir().expect("state");
    tokio::fs::write(
        state.path().join("settings.json"),
        br#"{"enableAgentActivity":false}"#,
    )
    .await
    .expect("legacy settings");
    let settings = ProviderSettingsStore::new(state.path())
        .get()
        .await
        .expect("settings");
    assert!(!settings.enable_chat_agent_activity);
    assert!(!settings.enable_terminal_agent_activity);
}

#[tokio::test]
async fn agent_activity_migration_preserves_explicit_split_fields() {
    for (document, expected_chat, expected_terminal) in [
        (
            r#"{"enableAgentActivity":false,"enableChatAgentActivity":true}"#,
            true,
            false,
        ),
        (
            r#"{"enableAgentActivity":false,"enableTerminalAgentActivity":true}"#,
            false,
            true,
        ),
        (
            r#"{"enableAgentActivity":true,"enableChatAgentActivity":false,"enableTerminalAgentActivity":true}"#,
            false,
            true,
        ),
    ] {
        let state = tempfile::tempdir().expect("state");
        tokio::fs::write(state.path().join("settings.json"), document)
            .await
            .expect("settings document");

        let settings = ProviderSettingsStore::new(state.path())
            .get()
            .await
            .expect("settings");

        assert_eq!(settings.enable_chat_agent_activity, expected_chat);
        assert_eq!(settings.enable_terminal_agent_activity, expected_terminal);
    }
}

#[tokio::test]
async fn agent_activity_split_settings_roundtrip_independently() {
    let state = tempfile::tempdir().expect("state");
    let store = ProviderSettingsStore::new(state.path());

    let updated = store
        .update(ServerSettingsPatch {
            enable_chat_agent_activity: Some(false),
            enable_terminal_agent_activity: Some(true),
            ..ServerSettingsPatch::default()
        })
        .await
        .expect("update activity settings");
    assert!(!updated.enable_chat_agent_activity);
    assert!(updated.enable_terminal_agent_activity);

    let reloaded = ProviderSettingsStore::new(state.path())
        .get()
        .await
        .expect("reloaded settings");
    assert!(!reloaded.enable_chat_agent_activity);
    assert!(reloaded.enable_terminal_agent_activity);
}

#[tokio::test]
async fn provider_session_defaults_replace_the_whole_map_and_roundtrip() {
    let temp = TempDir::new().expect("temp");
    let store = ProviderSettingsStore::new(temp.path());

    store
        .update(ServerSettingsPatch {
            automatic_git_fetch_interval_ms: Some(12_345),
            provider_instances: Some(std::collections::BTreeMap::from([(
                "work".to_owned(),
                ProviderInstanceInput {
                    driver: "codex".to_owned(),
                    enabled: true,
                    display_name: Some("Work".to_owned()),
                    environment: Vec::new(),
                    config: serde_json::json!({"binaryPath":"/opt/bin/codex"}),
                },
            )])),
            provider_session_defaults: Some(std::collections::BTreeMap::from([(
                "legacy".to_owned(),
                ProviderSessionDefaultState {
                    model: "legacy-model".to_owned(),
                    options: None,
                },
            )])),
            ..ServerSettingsPatch::default()
        })
        .await
        .expect("initial update");

    let expected_defaults = std::collections::BTreeMap::from([
        (
            "claudeAgent".to_owned(),
            ProviderSessionDefaultState {
                model: "claude-sonnet-4-6".to_owned(),
                options: Some(vec![ProviderOptionSelectionState {
                    id: "effort".to_owned(),
                    value: ProviderOptionSelectionValueState::String("high".to_owned()),
                }]),
            },
        ),
        (
            "codex".to_owned(),
            ProviderSessionDefaultState {
                model: "gpt-5.4".to_owned(),
                options: Some(vec![
                    ProviderOptionSelectionState {
                        id: "reasoningEffort".to_owned(),
                        value: ProviderOptionSelectionValueState::String("medium".to_owned()),
                    },
                    ProviderOptionSelectionState {
                        id: "fastMode".to_owned(),
                        value: ProviderOptionSelectionValueState::Boolean(true),
                    },
                ]),
            },
        ),
    ]);

    let updated = store
        .update(ServerSettingsPatch {
            provider_session_defaults: Some(expected_defaults.clone()),
            ..ServerSettingsPatch::default()
        })
        .await
        .expect("replace provider session defaults");

    assert_eq!(updated.provider_session_defaults, expected_defaults);
    assert!(!updated.provider_session_defaults.contains_key("legacy"));
    assert_eq!(updated.automatic_git_fetch_interval, 12_345);
    assert_eq!(
        updated
            .provider_instances
            .get("work")
            .expect("provider instance")
            .display_name
            .as_deref(),
        Some("Work")
    );

    let persisted: serde_json::Value = serde_json::from_slice(
        &tokio::fs::read(temp.path().join("settings.json"))
            .await
            .expect("settings file"),
    )
    .expect("valid settings JSON");
    assert_eq!(
        persisted["providerSessionDefaults"]["claudeAgent"]["model"],
        "claude-sonnet-4-6"
    );
    assert_eq!(
        persisted["providerSessionDefaults"]["codex"]["options"][0],
        serde_json::json!({"id":"reasoningEffort","value":"medium"})
    );
    assert_eq!(
        persisted["providerSessionDefaults"]["codex"]["options"][1],
        serde_json::json!({"id":"fastMode","value":true})
    );
    assert!(persisted.get("provider_session_defaults").is_none());

    drop(store);
    let reopened_store = ProviderSettingsStore::new(temp.path());
    let reloaded = reopened_store.get().await.expect("reload settings");
    assert_eq!(reloaded.provider_session_defaults, expected_defaults);
    assert_eq!(reloaded.automatic_git_fetch_interval, 12_345);
    assert!(reloaded.provider_instances.contains_key("work"));
}

#[tokio::test]
async fn trims_project_settings_and_defaults_blank_binary_paths() {
    let temp = TempDir::new().expect("temp");
    let store = ProviderSettingsStore::new(temp.path());

    let next = store
        .update(ServerSettingsPatch {
            add_project_base_directory: Some("  ~/Development  ".to_owned()),
            providers: Some(ProvidersPatch {
                codex: Some(ProviderSettingsPatch {
                    binary_path: Some("   ".to_owned()),
                    ..ProviderSettingsPatch::default()
                }),
                claude_agent: Some(ProviderSettingsPatch {
                    binary_path: Some(String::new()),
                    ..ProviderSettingsPatch::default()
                }),
                ..ProvidersPatch::default()
            }),
            ..ServerSettingsPatch::default()
        })
        .await
        .expect("update");

    assert_eq!(next.add_project_base_directory, "~/Development");
    assert_eq!(next.providers.codex.binary_path, "codex");
    assert_eq!(next.providers.claude_agent.binary_path, "claude");
}

#[tokio::test]
async fn writes_only_non_default_server_settings_to_disk() {
    let temp = TempDir::new().expect("temp");
    let store = ProviderSettingsStore::new(temp.path());

    let next = store
        .update(ServerSettingsPatch {
            add_project_base_directory: Some("~/Development".to_owned()),
            providers: Some(ProvidersPatch {
                codex: Some(ProviderSettingsPatch {
                    binary_path: Some("/opt/homebrew/bin/codex".to_owned()),
                    ..ProviderSettingsPatch::default()
                }),
                ..ProvidersPatch::default()
            }),
            automatic_git_fetch_interval_ms: Some(10_000),
            ..ServerSettingsPatch::default()
        })
        .await
        .expect("update");

    assert_eq!(next.providers.codex.binary_path, "/opt/homebrew/bin/codex");
    let raw = tokio::fs::read_to_string(temp.path().join("settings.json"))
        .await
        .expect("settings file");
    assert!(raw.contains("\"addProjectBaseDirectory\": \"~/Development\""));
    assert!(raw.contains("\"automaticGitFetchInterval\": 10000"));
    assert!(!raw.contains("\"claudeAgent\""));
}

#[tokio::test]
async fn stores_sensitive_environment_values_outside_settings_json_and_roundtrips() {
    let temp = TempDir::new().expect("temp");
    let store = ProviderSettingsStore::new(temp.path());

    let next = store
        .update(ServerSettingsPatch {
            provider_instances: Some(std::collections::BTreeMap::from([(
                "codex_personal".to_owned(),
                ProviderInstanceInput {
                    driver: "codex".to_owned(),
                    enabled: true,
                    display_name: None,
                    environment: vec![
                        ProviderEnvironmentVariableInput {
                            name: "OPENROUTER_API_KEY".to_owned(),
                            value: "sk-or-secret".to_owned(),
                            sensitive: true,
                            value_redacted: false,
                        },
                        ProviderEnvironmentVariableInput {
                            name: "ANTHROPIC_BASE_URL".to_owned(),
                            value: "https://openrouter.ai/api".to_owned(),
                            sensitive: false,
                            value_redacted: false,
                        },
                    ],
                    config: serde_json::Value::Null,
                },
            )])),
            ..ServerSettingsPatch::default()
        })
        .await
        .expect("update");

    let environment = &next
        .provider_instances
        .get("codex_personal")
        .expect("instance")
        .environment;
    assert_eq!(environment[0].value, "sk-or-secret");
    assert!(
        !environment[0].value_redacted,
        "runtime values have been materialized"
    );
    let effective = store.get().await.expect("effective provider settings");
    assert_eq!(
        effective.provider_instances["codex_personal"].environment[0].value,
        "sk-or-secret"
    );
    assert!(!effective.provider_instances["codex_personal"].environment[0].value_redacted);

    let raw = tokio::fs::read_to_string(temp.path().join("settings.json"))
        .await
        .expect("settings file");
    assert!(!raw.contains("sk-or-secret"));
    assert!(raw.contains("\"valueRedacted\": true"));

    let round_tripped = store
        .update(ServerSettingsPatch {
            provider_instances: Some(std::collections::BTreeMap::from([(
                "codex_personal".to_owned(),
                ProviderInstanceInput {
                    driver: "codex".to_owned(),
                    enabled: true,
                    display_name: Some("Codex Personal".to_owned()),
                    environment: vec![
                        ProviderEnvironmentVariableInput {
                            name: "OPENROUTER_API_KEY".to_owned(),
                            value: String::new(),
                            sensitive: true,
                            value_redacted: true,
                        },
                        ProviderEnvironmentVariableInput {
                            name: "ANTHROPIC_BASE_URL".to_owned(),
                            value: "https://openrouter.ai/api".to_owned(),
                            sensitive: false,
                            value_redacted: false,
                        },
                    ],
                    config: serde_json::Value::Null,
                },
            )])),
            ..ServerSettingsPatch::default()
        })
        .await
        .expect("round trip");

    assert_eq!(
        round_tripped
            .provider_instances
            .get("codex_personal")
            .expect("instance")
            .environment[0]
            .value,
        "sk-or-secret"
    );
}

#[tokio::test]
async fn preserves_provider_instance_enabled_state_and_driver_config() {
    let temp = TempDir::new().expect("temp");
    let store = ProviderSettingsStore::new(temp.path());
    tokio::fs::write(
        temp.path().join("settings.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "providerInstances": {
                "cursor": {
                    "driver": "cursor",
                    "enabled": true,
                    "config": {
                        "binaryPath": "cursor-agent",
                        "apiEndpoint": "http://127.0.0.1:3210"
                    }
                }
            }
        }))
        .expect("encode settings"),
    )
    .await
    .expect("write settings");

    let loaded = store.get().await.expect("load settings");
    let cursor = loaded
        .provider_instances
        .get("cursor")
        .expect("cursor instance");
    assert!(cursor.enabled);
    assert_eq!(cursor.config["binaryPath"], "cursor-agent");
    assert_eq!(cursor.config["apiEndpoint"], "http://127.0.0.1:3210");
}

#[tokio::test]
async fn missing_redacted_secret_returns_the_expected_read_error() {
    let temp = TempDir::new().expect("temp");
    let settings = ProviderSettingsState {
        provider_instances: std::collections::BTreeMap::from([(
            "codex_personal".to_owned(),
            server_settings::ProviderInstanceState {
                driver: "codex".to_owned(),
                enabled: true,
                display_name: None,
                environment: vec![server_settings::ProviderEnvironmentVariableState {
                    name: "OPENROUTER_API_KEY".to_owned(),
                    value: String::new(),
                    sensitive: true,
                    value_redacted: true,
                }],
                config: serde_json::Value::Null,
            },
        )]),
        ..ProviderSettingsState::default()
    };
    tokio::fs::write(
        temp.path().join("settings.json"),
        serde_json::to_vec_pretty(&settings).expect("encode"),
    )
    .await
    .expect("write settings");

    let store = ProviderSettingsStore::new(temp.path());
    let error = store.get().await.expect_err("missing secret");
    assert!(matches!(
        error,
        ServerSettingsReadError::MissingSecret {
            ref instance_id,
            ref environment_variable,
            ..
        } if instance_id == "codex_personal" && environment_variable == "OPENROUTER_API_KEY"
    ));
}

fn sensitive_instance_patch(value: &str, value_redacted: bool) -> ServerSettingsPatch {
    ServerSettingsPatch {
        provider_instances: Some(std::collections::BTreeMap::from([(
            "instance".to_owned(),
            ProviderInstanceInput {
                driver: "codex".to_owned(),
                enabled: true,
                display_name: None,
                environment: vec![ProviderEnvironmentVariableInput {
                    name: "TOKEN".to_owned(),
                    value: value.to_owned(),
                    sensitive: true,
                    value_redacted,
                }],
                config: serde_json::Value::Null,
            },
        )])),
        ..ServerSettingsPatch::default()
    }
}

#[tokio::test]
async fn public_store_maps_settings_and_secret_filesystem_failures() {
    let temp = TempDir::new().expect("temp");

    let unreadable_root = temp.path().join("unreadable-root");
    tokio::fs::create_dir_all(unreadable_root.join("settings.json"))
        .await
        .expect("settings directory");
    assert!(matches!(
        ProviderSettingsStore::new(&unreadable_root).get().await,
        Err(ServerSettingsReadError::Read { .. })
    ));

    let blocked_root = temp.path().join("blocked-root");
    tokio::fs::write(&blocked_root, "not a directory")
        .await
        .expect("blocker");
    assert!(matches!(
        ProviderSettingsStore::new(&blocked_root)
            .update(ServerSettingsPatch::default())
            .await,
        Err(ServerSettingsReadError::Read { .. }) | Err(ServerSettingsReadError::Persist { .. })
    ));

    let secret_root = temp.path().join("secret-root");
    let secret_store = ProviderSettingsStore::new(&secret_root);
    secret_store
        .update(sensitive_instance_patch("secret", false))
        .await
        .expect("initial secret");
    let mut secret_entries = tokio::fs::read_dir(secret_root.join("secrets"))
        .await
        .expect("secret directory");
    let secret_path = secret_entries
        .next_entry()
        .await
        .expect("read secret entry")
        .expect("secret entry")
        .path();
    tokio::fs::remove_file(&secret_path)
        .await
        .expect("remove secret");
    tokio::fs::create_dir(&secret_path)
        .await
        .expect("replace secret with directory");

    assert!(matches!(
        secret_store.get().await,
        Err(ServerSettingsReadError::Read { .. })
    ));
    assert!(matches!(
        secret_store
            .update(sensitive_instance_patch("replacement", false))
            .await,
        Err(ServerSettingsReadError::Persist { .. })
    ));

    let blocked_settings_root = temp.path().join("blocked-settings");
    tokio::fs::create_dir_all(blocked_settings_root.join("settings.json"))
        .await
        .expect("blocked settings path");
    assert!(matches!(
        ProviderSettingsStore::new(&blocked_settings_root)
            .update(ServerSettingsPatch::default())
            .await,
        Err(ServerSettingsReadError::Read { .. })
    ));
}
