#[path = "support/executable_fixture.rs"]
mod executable_fixture;

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use coddy_studio_server::{
    ACTIVE_RPC_METHODS, MethodMode, RpcRegistry, ServerConfig, ServerRuntime,
    persistence::StorageInstanceId,
    production::{
        control::NativeServerControl, runtime::finalize_rpc_registry,
        server_terminal::ProductionServerControl,
    },
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[path = "support/hermetic_providers.rs"]
mod hermetic_providers;

const TEST_STORAGE_INSTANCE_ID: Uuid = Uuid::from_u128(0x00000000000040008000000000000003);

fn test_config(path: &Path) -> ServerConfig {
    let mut config = ServerConfig::new(path);
    config.storage_instance_id = Some(StorageInstanceId::from_uuid(TEST_STORAGE_INSTANCE_ID));
    config
}

fn auth_descriptor() -> Value {
    json!({
        "policy": "loopback-browser",
        "bootstrapMethods": ["one-time-token"],
        "sessionMethods": ["browser-session-cookie", "bearer-access-token"],
        "sessionCookieName": "coddy_studio_session",
    })
}

fn complete_registry_without(excluded: &[&str]) -> RpcRegistry {
    let mut registry = RpcRegistry::empty();
    for method in ACTIVE_RPC_METHODS {
        if excluded.contains(&method.name) {
            continue;
        }
        match method.mode {
            MethodMode::Unary => registry
                .register_unary(method.name, |_request, _cancellation| async {
                    Ok(json!({}))
                }),
            MethodMode::Stream => {
                registry.register_stream(method.name, |_request, _cancellation| {
                    let (_sender, receiver) = mpsc::channel(1);
                    receiver
                })
            }
        }
    }
    registry
}

fn complete_registry() -> RpcRegistry {
    complete_registry_without(&[])
}

#[test]
fn rpc_inventory_includes_activity_control_mutations_as_unary_methods() {
    let activity_control = ACTIVE_RPC_METHODS
        .iter()
        .filter(|method| {
            matches!(
                method.name,
                "activity.cancelSubtree" | "activity.retrySubtreeCancellation"
            )
        })
        .map(|method| (method.name, method.mode))
        .collect::<Vec<_>>();

    assert_eq!(
        activity_control,
        [
            ("activity.cancelSubtree", MethodMode::Unary),
            ("activity.retrySubtreeCancellation", MethodMode::Unary),
        ]
    );
}

#[tokio::test]
async fn production_runtime_registers_every_git_manager_method() {
    let git_manager_methods = ACTIVE_RPC_METHODS
        .iter()
        .filter(|method| {
            method.name.starts_with("gitManager.") || method.name == "subscribeGitManagerSignal"
        })
        .map(|method| (method.name, method.mode))
        .collect::<Vec<_>>();

    assert_eq!(
        git_manager_methods,
        [
            ("gitManager.commit", MethodMode::Unary),
            ("gitManager.discard", MethodMode::Unary),
            ("gitManager.discardPartial", MethodMode::Unary),
            ("gitManager.getCommits", MethodMode::Unary),
            ("gitManager.getDiff", MethodMode::Unary),
            ("gitManager.getRefs", MethodMode::Unary),
            ("gitManager.getRemoteTags", MethodMode::Unary),
            ("gitManager.getStashes", MethodMode::Unary),
            ("gitManager.listPullRequests", MethodMode::Unary),
            ("gitManager.previewMerge", MethodMode::Unary),
            ("gitManager.runOperation", MethodMode::Stream),
            ("gitManager.stagePartial", MethodMode::Unary),
            ("gitManager.undoCommit", MethodMode::Unary),
            ("gitManager.unstagePartial", MethodMode::Unary),
            ("subscribeGitManagerSignal", MethodMode::Stream),
        ]
    );

    let directory = tempfile::tempdir().expect("temporary state directory");
    let config = test_config(directory.path())
        .with_bind("127.0.0.1", 0)
        .with_unsafe_no_auth();
    hermetic_providers::write_hermetic_settings(&config.state_dir(), json!({}));
    let runtime = ServerRuntime::start(config)
        .await
        .expect("the real production runtime registers every Git Manager method");
    runtime.shutdown();
    runtime.join().await.expect("production runtime joins");
}

async fn fixture() -> (TempDir, NativeServerControl) {
    let directory = tempfile::tempdir().expect("temporary state directory");
    let mut config = test_config(directory.path());
    config.environment_id = "test-environment".into();
    config.environment_label = "Test Environment".into();
    hermetic_providers::write_hermetic_settings(&config.state_dir(), json!({}));
    let control = NativeServerControl::new(config, auth_descriptor()).await;
    finalize_rpc_registry(&complete_registry(), &control).expect("complete production registry");
    (directory, control)
}

async fn fixture_with_state_file(
    relative_path: &str,
    contents: &[u8],
) -> (TempDir, NativeServerControl) {
    let directory = tempfile::tempdir().expect("temporary state directory");
    let mut config = test_config(directory.path());
    let path = config.state_dir().join(relative_path);
    tokio::fs::create_dir_all(path.parent().expect("state file parent"))
        .await
        .expect("create state directory");
    tokio::fs::write(path, contents)
        .await
        .expect("write state fixture");
    // Preserve settings overrides and any separate keybinding fixture while
    // supplying the hermetic base for unspecified provider configuration.
    hermetic_providers::ensure_hermetic_settings(&config.state_dir());
    config.environment_id = "test-environment".into();
    config.environment_label = "Test Environment".into();
    let control = NativeServerControl::new(config, auth_descriptor()).await;
    finalize_rpc_registry(&complete_registry(), &control).expect("complete production registry");
    (directory, control)
}

async fn write_provider_fixture(directory: &TempDir) -> PathBuf {
    #[cfg(windows)]
    let (name, contents) = (
        "provider.cmd",
        "@echo off\r\nif \"%1\"==\"about\" (echo {\"cliVersion\":\"9.8.7\",\"userEmail\":\"dev@example.com\",\"subscriptionTier\":\"pro\"}& exit /b 0)\r\necho provider 1.0.0\r\n",
    );
    #[cfg(not(windows))]
    let (name, contents) = (
        "provider",
        "#!/bin/sh\nif [ \"$1\" = \"about\" ]; then\n  echo '{\"cliVersion\":\"9.8.7\",\"userEmail\":\"dev@example.com\",\"subscriptionTier\":\"pro\"}'\nelse\n  echo 'provider 1.0.0'\nfi\n",
    );
    let path = directory.path().join(name);
    executable_fixture::write_executable(&path, contents);
    path
}

async fn write_updating_cursor_fixture(directory: &TempDir) -> PathBuf {
    let release_directory = directory
        .path()
        .join(".local/share/cursor-agent/versions/2026.06.19-653a7fb");
    tokio::fs::create_dir_all(&release_directory)
        .await
        .expect("create Cursor release directory");
    let version_state = release_directory.join("version-state");
    tokio::fs::write(&version_state, "2026.06.19-653a7fb")
        .await
        .expect("write Cursor version state");
    tokio::fs::write(release_directory.join("next-version"), "2026.08.04-aaa8809")
        .await
        .expect("write next Cursor version");
    let executable = release_directory.join(if cfg!(windows) {
        "cursor-agent.exe"
    } else {
        "cursor-agent"
    });
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/cursor_update_fixture.rs");
    let output =
        tokio::process::Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
            .arg("--edition=2024")
            .arg(source)
            .arg("-o")
            .arg(&executable)
            .output()
            .await
            .expect("compile Cursor update fixture");
    assert!(
        output.status.success(),
        "compile Cursor update fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    executable
}

async fn write_claude_fixture(directory: &TempDir, version: &str) -> PathBuf {
    #[cfg(windows)]
    let (name, contents) = (
        "claude.cmd",
        format!(
            "@echo off\r\nif \"%1\"==\"--version\" (echo {version} (Claude Code)& exit /b 0)\r\nif \"%1\"==\"auth\" (echo {{\"loggedIn\":true,\"authMethod\":\"claude.ai\",\"email\":\"dev@example.com\"}}& exit /b 0)\r\nexit /b 1\r\n"
        ),
    );
    #[cfg(not(windows))]
    let (name, contents) = (
        "claude",
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  echo '{version} (Claude Code)'\nelif [ \"$1\" = \"auth\" ]; then\n  echo '{{\"loggedIn\":true,\"authMethod\":\"claude.ai\",\"email\":\"dev@example.com\"}}'\nelse\n  exit 1\nfi\n"
        ),
    );
    let path = directory.path().join(name);
    executable_fixture::write_executable(&path, contents);
    path
}

async fn write_discovering_claude_fixture(directory: &TempDir) -> PathBuf {
    let script = r#"import fs from "node:fs";
import readline from "node:readline";
const version = process.env.CODDY_STUDIO_CLAUDE_FIXTURE_VERSION;
const reloadMarker = new URL("./claude-fixture-reloaded", import.meta.url);
const reloaded = fs.existsSync(reloadMarker);
if (process.argv[2] === "--version") {
  process.stdout.write(`${version} (Claude Code)\n`);
  process.exit(0);
}
if (process.argv[2] === "auth") {
  process.stdout.write(JSON.stringify({
    loggedIn: true,
    authMethod: "claude.ai",
    email: "dev@example.test"
  }) + "\n");
  process.exit(0);
}
const send = (message) => process.stdout.write(JSON.stringify(message) + "\n");
readline.createInterface({ input: process.stdin, crlfDelay: Infinity }).on("line", (line) => {
  const message = JSON.parse(line);
  if (message.type !== "control_request") return;
  if (message.request?.subtype === "initialize") {
    send({
      type: "control_response",
      response: {
        subtype: "success",
        request_id: message.request_id,
        response: {
          commands: [{ name: "review", description: "Review code" }],
          agents: [],
          models: reloaded ? [
            {
              value: "sonnet",
              resolvedModel: "claude-sonnet-5",
              displayName: "Sonnet",
              description: "Sonnet 5 · Efficient for routine tasks",
              supportsEffort: true,
              supportedEffortLevels: ["low", "medium", "high", "xhigh", "max"]
            }
          ] : [
            {
              value: "opus",
              resolvedModel: "claude-opus-5",
              displayName: "Opus",
              description: "Opus 5 · Best for everyday, complex tasks",
              supportsEffort: true,
              supportedEffortLevels: ["low", "medium", "high", "xhigh", "max"],
              supportsFastMode: true
            },
            {
              value: "sonnet",
              resolvedModel: "claude-sonnet-5",
              displayName: "Sonnet",
              description: "Sonnet 5 · Efficient for routine tasks",
              supportsEffort: true,
              supportedEffortLevels: ["low", "medium", "high", "xhigh", "max"]
            }
          ]
        }
      }
    });
  } else if (message.request?.subtype === "reload_skills") {
    if (!reloaded) fs.writeFileSync(reloadMarker, "");
    send({
      type: "control_response",
      response: {
        subtype: "success",
        request_id: message.request_id,
        response: { skills: reloaded ? "invalid" : [{ name: "review" }] }
      }
    });
  }
});"#;
    tokio::fs::write(directory.path().join("claude-fixture.mjs"), script)
        .await
        .expect("write discovering Claude fixture");

    #[cfg(windows)]
    let (name, launcher) = (
        "claude.cmd",
        "@echo off\r\nnode \"%~dp0claude-fixture.mjs\" %*\r\n",
    );
    #[cfg(not(windows))]
    let (name, launcher) = (
        "claude",
        "#!/bin/sh\nexec node \"$(dirname \"$0\")/claude-fixture.mjs\" \"$@\"\n",
    );
    let path = directory.path().join(name);
    executable_fixture::write_executable(&path, launcher);
    path
}

fn isolated_claude_settings(sandbox: &Path, instance: Value) -> Value {
    let mut settings = hermetic_providers::hermetic_provider_settings(sandbox);
    for driver in ["codex", "cursor", "grok", "opencode"] {
        settings["providers"][driver]["enabled"] = Value::Bool(false);
    }
    settings["providerInstances"] = json!({"claudeAgent": instance});
    settings
}

async fn call(control: &NativeServerControl, method: &'static str, payload: Value) -> Value {
    control
        .call(method, payload, CancellationToken::new())
        .await
        .unwrap_or_else(|error| panic!("{method} failed: {error}"))
}

async fn next_event(
    stream: &mut coddy_studio_server::production::server_terminal::JsonStream,
) -> Value {
    timeout(Duration::from_secs(2), stream.recv())
        .await
        .expect("stream event timeout")
        .expect("stream remains open")
        .expect("stream event succeeds")
        .into_iter()
        .next()
        .expect("non-empty event batch")
}

async fn next_event_of_type(
    stream: &mut coddy_studio_server::production::server_terminal::JsonStream,
    expected_type: &str,
) -> Value {
    timeout(Duration::from_secs(2), async {
        loop {
            let events = stream
                .recv()
                .await
                .expect("stream remains open")
                .expect("stream event succeeds");
            if let Some(event) = events
                .into_iter()
                .find(|event| event["type"] == expected_type)
            {
                return event;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("stream event {expected_type} timeout"))
}

#[tokio::test]
async fn config_and_settings_match_the_typescript_contract_without_faking_provider_authentication()
{
    // Probe a real fixture's version and authentication while keeping every
    // other driver missing. General settings still use production defaults.
    let sandbox = tempfile::tempdir().expect("provider sandbox");
    let pinned = hermetic_providers::hermetic_provider_settings(sandbox.path());
    let mut instances = pinned["providers"]
        .as_object()
        .expect("pinned providers")
        .iter()
        .map(|(driver, config)| (driver.clone(), json!({"driver": driver, "config": config})))
        .collect::<serde_json::Map<String, Value>>();
    let executable = write_discovering_claude_fixture(&sandbox).await;
    instances.insert(
        "claudeAgent".to_owned(),
        json!({
            "driver": "claudeAgent",
            "enabled": true,
            "environment": [{
                "name": "CODDY_STUDIO_CLAUDE_FIXTURE_VERSION",
                "value": "2.1.220",
                "sensitive": false,
            }],
            "config": { "binaryPath": executable },
        }),
    );
    let (directory, control) = fixture_with_state_file(
        "settings.json",
        &serde_json::to_vec(&json!({
            "enableProviderUpdateChecks": false,
            "providerInstances": instances,
        }))
        .expect("isolated defaults"),
    )
    .await;
    let settings = call(&control, "server.getSettings", json!({})).await;

    assert_eq!(settings["enableAssistantStreaming"], false);
    assert_eq!(settings["enableProviderUpdateChecks"], false);
    assert_eq!(settings["automaticGitFetchInterval"], 180_000);
    assert_eq!(
        settings["textGenerationModelSelection"]["model"],
        "gpt-5.4-mini"
    );
    assert_eq!(
        settings["providers"]["codex"]["binaryPath"],
        hermetic_providers::missing_provider_executable(&test_config(directory.path()).state_dir())
            .to_str()
            .unwrap()
    );
    assert_eq!(settings["providers"]["cursor"]["enabled"], true);

    call(&control, "server.refreshProviders", json!({})).await;
    let config = call(&control, "server.getConfig", json!({})).await;
    assert_eq!(config["auth"], auth_descriptor());
    assert_eq!(
        config["environment"]["capabilities"]["activityProtocolVersion"],
        2
    );
    assert!(
        config["cwd"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    assert!(
        config["keybindingsConfigPath"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    assert!(config["keybindings"].is_array());
    assert!(config["issues"].is_array());
    assert!(config["availableEditors"].is_array());
    assert_eq!(config["settings"], settings);
    let providers = config["providers"].as_array().expect("provider snapshots");
    assert_eq!(
        providers
            .iter()
            .filter(|provider| provider["status"] == "ready")
            .count(),
        1,
        "the ready-provider contract must be exercised"
    );
    assert!(
        providers
            .iter()
            .any(|provider| provider["installed"] == false),
        "the missing-provider contract must also be exercised"
    );
    for provider in config["providers"].as_array().expect("provider snapshots") {
        if provider["status"] == "ready" {
            assert_eq!(provider["driver"], "claudeAgent");
            assert_eq!(provider["version"], "2.1.220 (Claude Code)");
            assert_eq!(provider["auth"]["status"], "authenticated");
            assert_eq!(provider["installed"], true);
            assert!(matches!(
                provider["auth"]["status"].as_str(),
                Some("authenticated" | "unauthenticated" | "unknown")
            ));
        }
        if provider["installed"] == false {
            assert!(matches!(
                provider["status"].as_str(),
                Some("error" | "disabled")
            ));
        }
    }
}

#[tokio::test]
async fn missing_keybindings_file_uses_the_shipped_defaults() {
    let (_directory, control) = fixture().await;

    let config = call(&control, "server.getConfig", json!({})).await;
    let keybindings = config["keybindings"]
        .as_array()
        .expect("resolved keybindings");

    assert!(keybindings.iter().any(|binding| {
        binding["command"] == "sidebar.toggle"
            && binding["shortcut"]["key"] == "b"
            && binding["shortcut"]["modKey"] == true
    }));
    assert!(keybindings.iter().any(|binding| {
        binding["command"] == "terminal.newCenter"
            && binding["shortcut"]["key"] == "j"
            && binding["shortcut"]["modKey"] == true
    }));
}

#[tokio::test]
async fn activity_protocol_cannot_be_advertised_before_registry_validation() {
    let directory = tempfile::tempdir().expect("temporary state directory");
    let config = test_config(directory.path());
    hermetic_providers::write_hermetic_settings(&config.state_dir(), json!({}));
    let control = NativeServerControl::new(config, auth_descriptor()).await;

    let before_registration = call(&control, "server.getConfig", json!({})).await;
    assert_eq!(
        before_registration["environment"]["capabilities"]["activityProtocolVersion"],
        Value::Null
    );

    let missing_unary = complete_registry_without(&["activity.getSnapshot"]);
    let missing_unary_error =
        finalize_rpc_registry(&missing_unary, &control).expect_err("activity unary is required");
    assert!(missing_unary_error.contains("missing activity.getSnapshot"));
    let after_missing_unary = call(&control, "server.getConfig", json!({})).await;
    assert_eq!(
        after_missing_unary["environment"]["capabilities"]["activityProtocolVersion"],
        Value::Null
    );

    let missing_stream = complete_registry_without(&["subscribeActivity"]);
    let missing_stream_error =
        finalize_rpc_registry(&missing_stream, &control).expect_err("activity stream is required");
    assert!(missing_stream_error.contains("missing subscribeActivity"));
    let after_missing_stream = call(&control, "server.getConfig", json!({})).await;
    assert_eq!(
        after_missing_stream["environment"]["capabilities"]["activityProtocolVersion"],
        Value::Null
    );

    let cancellation = CancellationToken::new();
    let mut lifecycle = control.subscribe("subscribeServerLifecycle", cancellation.clone());
    finalize_rpc_registry(&complete_registry(), &control).expect("complete production registry");
    let welcome = next_event(&mut lifecycle).await;
    let ready = next_event(&mut lifecycle).await;
    assert_eq!(
        welcome["payload"]["environment"]["capabilities"]["activityProtocolVersion"],
        ready["payload"]["environment"]["capabilities"]["activityProtocolVersion"]
    );

    let after_registration = call(&control, "server.getConfig", json!({})).await;
    assert_eq!(
        after_registration["environment"]["capabilities"]["activityProtocolVersion"],
        2
    );

    let post_registration_cancellation = CancellationToken::new();
    let mut post_registration_lifecycle = control.subscribe(
        "subscribeServerLifecycle",
        post_registration_cancellation.clone(),
    );
    let post_registration_welcome = next_event(&mut post_registration_lifecycle).await;
    let post_registration_ready = next_event(&mut post_registration_lifecycle).await;
    assert_eq!(
        post_registration_welcome["payload"]["environment"]["capabilities"]["activityProtocolVersion"],
        2
    );
    assert_eq!(
        post_registration_ready["payload"]["environment"]["capabilities"]["activityProtocolVersion"],
        2
    );
    cancellation.cancel();
    post_registration_cancellation.cancel();
}

#[tokio::test]
async fn settings_update_persists_atomically_redacts_secrets_and_emits_stream_event() {
    let (directory, control) = fixture().await;
    let cancellation = CancellationToken::new();
    let mut stream = control.subscribe("subscribeServerConfig", cancellation.clone());
    assert_eq!(next_event(&mut stream).await["type"], "snapshot");

    let updated = call(
        &control,
        "server.updateSettings",
        json!({ "patch": {
            "enableAssistantStreaming": true,
            "providerInstances": {
                "work": {
                    "driver": "codex",
                    "displayName": "Work",
                    "environment": [{
                        "name": "TOKEN",
                        "value": "top-secret",
                        "sensitive": true
                    }]
                }
            }
        }}),
    )
    .await;
    assert_eq!(updated["enableAssistantStreaming"], true);
    assert_eq!(
        updated["providerInstances"]["work"]["environment"][0]["value"],
        ""
    );
    assert_eq!(
        updated["providerInstances"]["work"]["environment"][0]["valueRedacted"],
        true
    );

    let event = next_event_of_type(&mut stream, "settingsUpdated").await;
    assert_eq!(event["payload"]["settings"], updated);

    let persisted: Value = serde_json::from_slice(
        &tokio::fs::read(
            test_config(directory.path())
                .state_dir()
                .join("settings.json"),
        )
        .await
        .expect("persisted settings"),
    )
    .expect("valid settings JSON");
    assert!(!persisted.to_string().contains("top-secret"));
    assert_eq!(
        tokio::fs::read_to_string(
            test_config(directory.path())
                .state_dir()
                .join("secrets/provider-env-d29yaw-VE9LRU4"),
        )
        .await
        .expect("separate secret"),
        "top-secret"
    );
    cancellation.cancel();
}

#[tokio::test]
async fn enabling_the_profiler_persists_streams_and_flips_the_live_signal() {
    let (directory, control) = fixture().await;
    let mut profiler_enabled = control.profiler_enabled_signal();
    assert!(
        !*profiler_enabled.borrow_and_update(),
        "the profiler defaults off"
    );
    let cancellation = CancellationToken::new();
    let mut stream = control.subscribe("subscribeServerConfig", cancellation.clone());
    assert_eq!(next_event(&mut stream).await["type"], "snapshot");

    let updated = call(
        &control,
        "server.updateSettings",
        json!({ "patch": { "enableProfiler": true } }),
    )
    .await;
    assert_eq!(updated["enableProfiler"], true);

    let event = next_event_of_type(&mut stream, "settingsUpdated").await;
    assert_eq!(event["payload"]["settings"]["enableProfiler"], true);
    timeout(Duration::from_secs(2), profiler_enabled.changed())
        .await
        .expect("profiler signal timeout")
        .expect("profiler signal stays open");
    assert!(*profiler_enabled.borrow_and_update());

    let persisted: Value = serde_json::from_slice(
        &tokio::fs::read(
            test_config(directory.path())
                .state_dir()
                .join("settings.json"),
        )
        .await
        .expect("persisted settings"),
    )
    .expect("valid settings JSON");
    assert_eq!(persisted["enableProfiler"], true);
    let reloaded = NativeServerControl::new(test_config(directory.path()), auth_descriptor()).await;
    assert!(
        *reloaded.profiler_enabled_signal().borrow(),
        "the toggle survives a restart"
    );
    cancellation.cancel();
}

#[tokio::test]
async fn keybinding_upsert_replace_and_remove_are_resolved_persisted_and_streamed() {
    let (directory, control) = fixture().await;
    let cancellation = CancellationToken::new();
    let mut stream = control.subscribe("subscribeServerConfig", cancellation.clone());
    let _snapshot = next_event(&mut stream).await;

    let added = call(
        &control,
        "server.upsertKeybinding",
        json!({ "key": "ctrl+shift+k", "command": "terminal.newCenter" }),
    )
    .await;
    assert_eq!(added["issues"], json!([]));
    let binding = added["keybindings"].as_array().unwrap().last().unwrap();
    assert_eq!(binding["command"], "terminal.newCenter");
    assert_eq!(binding["shortcut"]["key"], "k");
    assert_eq!(binding["shortcut"]["ctrlKey"], true);
    assert_eq!(binding["shortcut"]["shiftKey"], true);
    assert_eq!(binding["shortcut"]["modKey"], true);
    let _add_event = next_event_of_type(&mut stream, "keybindingsUpdated").await;

    let replaced = call(
        &control,
        "server.upsertKeybinding",
        json!({
            "key": "alt+j",
            "command": "terminal.newCenter",
            "replace": { "key": "ctrl+shift+k", "command": "terminal.newCenter" }
        }),
    )
    .await;
    assert!(
        replaced["keybindings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| { row["command"] == "terminal.newCenter" && row["shortcut"]["key"] == "j" })
    );
    let _replace_event = next_event_of_type(&mut stream, "keybindingsUpdated").await;

    let removed = call(
        &control,
        "server.removeKeybinding",
        json!({ "key": "alt+j", "command": "terminal.newCenter" }),
    )
    .await;
    assert!(
        !removed["keybindings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| {
                row["command"] == "terminal.newCenter"
                    && row["shortcut"]["key"] == "j"
                    && row["shortcut"]["altKey"] == true
            })
    );
    let persisted: Value = serde_json::from_slice(
        &tokio::fs::read(
            test_config(directory.path())
                .state_dir()
                .join("keybindings.json"),
        )
        .await
        .expect("persisted keybindings"),
    )
    .expect("valid keybindings JSON");
    assert!(
        persisted
            .as_array()
            .unwrap()
            .iter()
            .any(|rule| { rule["key"] == "mod+j" && rule["command"] == "terminal.newCenter" })
    );
    assert!(
        !persisted
            .as_array()
            .unwrap()
            .iter()
            .any(|rule| { rule["key"] == "alt+j" && rule["command"] == "terminal.newCenter" })
    );
    cancellation.cancel();
}

#[tokio::test]
async fn malformed_keybinding_config_is_reported_instead_of_silently_replaced() {
    let (_directory, control) = fixture_with_state_file("keybindings.json", b"{not-json").await;

    let config = call(&control, "server.getConfig", json!({})).await;
    assert_eq!(config["keybindings"], json!([]));
    assert_eq!(config["issues"][0]["kind"], "keybindings.malformed-config");
    assert!(
        config["issues"][0]["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty())
    );
}

#[tokio::test]
async fn invalid_keybinding_entries_are_reported_by_original_index_while_valid_entries_survive() {
    let rules = json!([
        { "key": "ctrl+k", "command": "terminal.toggle" },
        { "key": "ctrl+shift", "command": "terminal.toggle" },
        "not-an-object"
    ]);
    let (_directory, control) = fixture_with_state_file(
        "keybindings.json",
        &serde_json::to_vec(&rules).expect("serialize keybindings fixture"),
    )
    .await;

    let config = call(&control, "server.getConfig", json!({})).await;
    assert_eq!(config["keybindings"].as_array().unwrap().len(), 1);
    assert_eq!(config["keybindings"][0]["command"], "terminal.newCenter");
    assert_eq!(config["issues"].as_array().unwrap().len(), 2);
    assert_eq!(config["issues"][0]["kind"], "keybindings.invalid-entry");
    assert_eq!(config["issues"][0]["index"], 1);
    assert_eq!(config["issues"][1]["index"], 2);
}

#[tokio::test]
async fn persisted_legacy_terminal_toggle_is_returned_as_new_center_without_losing_context() {
    let rules = json!([
        {
            "key": "alt+j",
            "command": "terminal.toggle",
            "when": "!terminalFocus"
        }
    ]);
    let (_directory, control) = fixture_with_state_file(
        "keybindings.json",
        &serde_json::to_vec(&rules).expect("serialize keybindings fixture"),
    )
    .await;

    let config = call(&control, "server.getConfig", json!({})).await;
    assert_eq!(config["issues"], json!([]));
    assert_eq!(config["keybindings"].as_array().unwrap().len(), 1);
    assert_eq!(config["keybindings"][0]["command"], "terminal.newCenter");
    assert_eq!(config["keybindings"][0]["shortcut"]["key"], "j");
    assert_eq!(config["keybindings"][0]["shortcut"]["altKey"], true);
    assert_eq!(
        config["keybindings"][0]["whenAst"],
        json!({
            "type": "not",
            "node": { "type": "identifier", "name": "terminalFocus" }
        })
    );
}

#[tokio::test]
async fn provider_inventory_uses_provider_specific_status_and_configured_models() {
    let directory = tempfile::tempdir().expect("temporary state directory");
    let executable = write_provider_fixture(&directory).await;
    let settings = json!({
        "providers": {
            "cursor": {
                "enabled": true,
                "binaryPath": executable,
                "customModels": []
            }
        },
        "providerInstances": {
            "cursor-work": {
                "driver": "cursor",
                "enabled": true,
                "config": { "customModels": ["cursor/custom-test"] }
            }
        }
    });
    // Layers this fixture's `cursor` override and `providerInstances` onto the
    // hermetic base, so the other built-in drivers stay pinned to a missing
    // executable instead of the default bare names.
    hermetic_providers::write_hermetic_settings(
        &test_config(directory.path()).state_dir(),
        settings,
    );
    let control = NativeServerControl::new(test_config(directory.path()), auth_descriptor()).await;

    call(&control, "server.refreshProviders", json!({})).await;
    let config = call(&control, "server.getConfig", json!({})).await;
    let provider = &config["providers"][0];
    assert_eq!(provider["instanceId"], "cursor-work");
    assert_eq!(provider["status"], "ready");
    assert_eq!(provider["version"], "9.8.7");
    assert_eq!(provider["auth"]["status"], "authenticated");
    assert_eq!(provider["auth"]["email"], "dev@example.com");
    assert!(
        provider["models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|model| { model["slug"] == "cursor/custom-test" && model["isCustom"] == true })
    );
}

#[tokio::test]
async fn claude_inventory_uses_authoritative_discovered_model_catalog() {
    let directory = tempfile::tempdir().expect("temporary state directory");
    let executable = write_discovering_claude_fixture(&directory).await;
    let settings = isolated_claude_settings(
        directory.path(),
        json!({
            "driver": "claudeAgent",
            "enabled": true,
            "environment": [{
                "name": "CODDY_STUDIO_CLAUDE_FIXTURE_VERSION",
                "value": "2.1.220",
                "sensitive": false
            }],
            "config": {
                "binaryPath": executable,
                "customModels": ["claude-custom-test"]
            }
        }),
    );
    let settings_path = test_config(directory.path())
        .state_dir()
        .join("settings.json");
    tokio::fs::create_dir_all(settings_path.parent().unwrap())
        .await
        .expect("create settings directory");
    tokio::fs::write(settings_path, serde_json::to_vec(&settings).unwrap())
        .await
        .expect("write settings fixture");
    let control = NativeServerControl::new(test_config(directory.path()), auth_descriptor()).await;

    let initial_config = call(&control, "server.getConfig", json!({})).await;
    let enabled_instance_ids = initial_config["providers"]
        .as_array()
        .expect("initial provider snapshots")
        .iter()
        .filter(|provider| provider["enabled"] == true)
        .filter_map(|provider| provider["instanceId"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        enabled_instance_ids,
        ["claudeAgent"],
        "Claude inventory fixture must not launch ambient providers"
    );

    call(&control, "server.refreshProviders", json!({})).await;
    let config = call(&control, "server.getConfig", json!({})).await;
    let provider = config["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["instanceId"] == "claudeAgent")
        .expect("Claude provider snapshot");
    let models = provider["models"].as_array().unwrap();
    let slugs = models
        .iter()
        .filter_map(|model| model["slug"].as_str())
        .collect::<Vec<_>>();

    assert_eq!(provider["status"], "ready");
    assert_eq!(provider["auth"]["status"], "authenticated");
    assert_eq!(
        slugs,
        ["opus", "sonnet", "claude-custom-test"],
        "Claude initialization models must replace fixed built-ins"
    );
    assert!(!slugs.contains(&"claude-opus-4-8"));
    assert_eq!(models[0]["name"], "Opus 5");
    assert_eq!(
        models[0]["capabilities"]["optionDescriptors"][1]["id"],
        "fastMode"
    );
}

#[tokio::test]
async fn claude_inventory_keeps_discovered_models_when_skill_reload_is_invalid() {
    let directory = tempfile::tempdir().expect("temporary state directory");
    let executable = write_discovering_claude_fixture(&directory).await;
    let settings = isolated_claude_settings(
        directory.path(),
        json!({
            "driver": "claudeAgent",
            "enabled": true,
            "environment": [{
                "name": "CODDY_STUDIO_CLAUDE_FIXTURE_VERSION",
                "value": "2.1.220",
                "sensitive": false
            }],
            "config": { "binaryPath": executable }
        }),
    );
    let settings_path = test_config(directory.path())
        .state_dir()
        .join("settings.json");
    tokio::fs::create_dir_all(settings_path.parent().unwrap())
        .await
        .expect("create settings directory");
    tokio::fs::write(settings_path, serde_json::to_vec(&settings).unwrap())
        .await
        .expect("write settings fixture");
    let control = NativeServerControl::new(test_config(directory.path()), auth_descriptor()).await;

    call(&control, "server.refreshProviders", json!({})).await;
    let first_config = call(&control, "server.getConfig", json!({})).await;
    let first_provider = first_config["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["instanceId"] == "claudeAgent")
        .expect("Claude provider snapshot");
    assert_eq!(first_provider["models"][0]["slug"], "opus");
    assert_eq!(first_provider["skills"][0]["name"], "review");

    call(&control, "server.refreshProviders", json!({})).await;
    let second_config = call(&control, "server.getConfig", json!({})).await;
    let second_provider = second_config["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["instanceId"] == "claudeAgent")
        .expect("refreshed Claude provider snapshot");
    let slugs = second_provider["models"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|model| model["slug"].as_str())
        .collect::<Vec<_>>();

    assert_eq!(slugs, ["sonnet"]);
    assert_eq!(second_provider["skills"], first_provider["skills"]);
}

#[tokio::test]
async fn claude_inventory_hides_models_unsupported_by_the_installed_cli_version() {
    let directory = tempfile::tempdir().expect("temporary state directory");
    let executable = write_claude_fixture(&directory, "2.1.100").await;
    let settings = isolated_claude_settings(
        directory.path(),
        json!({
            "driver": "claudeAgent",
            "enabled": true,
            "config": { "binaryPath": executable }
        }),
    );
    let settings_path = test_config(directory.path())
        .state_dir()
        .join("settings.json");
    tokio::fs::create_dir_all(settings_path.parent().unwrap())
        .await
        .expect("create settings directory");
    tokio::fs::write(settings_path, serde_json::to_vec(&settings).unwrap())
        .await
        .expect("write settings fixture");
    let control = NativeServerControl::new(test_config(directory.path()), auth_descriptor()).await;

    call(&control, "server.refreshProviders", json!({})).await;
    let config = call(&control, "server.getConfig", json!({})).await;
    let provider = config["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["instanceId"] == "claudeAgent")
        .expect("Claude provider snapshot");
    let slugs = provider["models"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|model| model["slug"].as_str())
        .collect::<Vec<_>>();

    assert!(!slugs.contains(&"claude-fable-5"));
    assert!(!slugs.contains(&"claude-opus-4-8"));
    assert!(!slugs.contains(&"claude-opus-4-7"));
    assert!(slugs.contains(&"claude-opus-4-6"));
    assert!(slugs.contains(&"claude-sonnet-5"));
}

#[tokio::test]
async fn trace_and_auxiliary_streams_use_exact_contract_shapes() {
    let (_directory, control) = fixture().await;
    let diagnostics = call(&control, "server.getTraceDiagnostics", json!({})).await;
    assert!(
        diagnostics["traceFilePath"]
            .as_str()
            .is_some_and(|path| !path.is_empty())
    );
    assert!(diagnostics["scannedFilePaths"].is_array());
    assert!(diagnostics["readAt"].is_string());
    for field in [
        "recordCount",
        "parseErrorCount",
        "failureCount",
        "interruptionCount",
        "slowSpanThresholdMs",
        "slowSpanCount",
    ] {
        assert!(
            diagnostics[field].is_number(),
            "missing numeric field {field}"
        );
    }
    for field in ["firstSpanAt", "lastSpanAt", "partialFailure"] {
        assert_eq!(
            diagnostics[field],
            json!({ "_id": "Option", "_tag": "None" })
        );
    }
    assert_eq!(diagnostics["error"]["_tag"], "Some");
    assert_eq!(
        diagnostics["error"]["value"]["kind"],
        "trace-file-not-found"
    );

    let lifecycle_cancel = CancellationToken::new();
    let mut lifecycle = control.subscribe("subscribeServerLifecycle", lifecycle_cancel.clone());
    assert_eq!(next_event(&mut lifecycle).await["type"], "welcome");
    assert_eq!(next_event(&mut lifecycle).await["type"], "ready");
    lifecycle_cancel.cancel();
}

#[tokio::test]
async fn provider_update_reports_the_contract_error_when_native_update_is_unavailable() {
    let (_directory, control) = fixture().await;
    let error = control
        .call(
            "server.updateProvider",
            json!({ "provider": "grok" }),
            CancellationToken::new(),
        )
        .await
        .expect_err("manual-only provider update must fail");
    assert_eq!(error["_tag"], "ServerProviderUpdateError");
    assert_eq!(error["provider"], "grok");
    assert!(
        error["reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty())
    );
}

#[tokio::test]
async fn refresh_providers_returns_version_advisories_without_registry_access() {
    let directory = tempfile::tempdir().expect("temporary state directory");
    let missing_codex = directory.path().join("missing-codex");
    let settings = json!({
        "enableProviderUpdateChecks": false,
        "providers": {
            "claudeAgent": { "enabled": false },
            "cursor": { "enabled": false },
            "grok": { "enabled": false },
            "opencode": { "enabled": false }
        },
        "providerInstances": {
            "codex": {
                "driver": "codex",
                "enabled": true,
                "config": { "binaryPath": missing_codex }
            }
        }
    });
    let settings_path = test_config(directory.path())
        .state_dir()
        .join("settings.json");
    tokio::fs::create_dir_all(settings_path.parent().expect("settings parent"))
        .await
        .expect("create settings directory");
    tokio::fs::write(
        settings_path,
        serde_json::to_vec(&settings).expect("settings JSON"),
    )
    .await
    .expect("write settings fixture");
    hermetic_providers::ensure_hermetic_settings(&test_config(directory.path()).state_dir());
    let control = NativeServerControl::new(test_config(directory.path()), auth_descriptor()).await;

    let refreshed = call(
        &control,
        "server.refreshProviders",
        json!({ "instanceId": "codex" }),
    )
    .await;
    let codex = refreshed["providers"]
        .as_array()
        .expect("providers")
        .iter()
        .find(|provider| provider["driver"] == "codex")
        .expect("codex provider");
    assert!(codex["versionAdvisory"].is_object());
    assert!(codex["versionAdvisory"]["canUpdate"].is_boolean());
    assert!(codex["versionAdvisory"]["status"].is_string());
}

#[tokio::test]
async fn provider_update_succeeds_when_cursor_installed_version_advances() {
    let directory = tempfile::tempdir().expect("temporary state directory");
    let executable = write_updating_cursor_fixture(&directory).await;
    let settings = json!({
        "enableProviderUpdateChecks": false,
        "providerInstances": {
            "cursor-work": {
                "driver": "cursor",
                "enabled": true,
                "config": { "binaryPath": executable }
            }
        }
    });
    hermetic_providers::write_hermetic_settings(
        &test_config(directory.path()).state_dir(),
        settings,
    );
    let control = NativeServerControl::new(test_config(directory.path()), auth_descriptor()).await;
    let initial = call(
        &control,
        "server.refreshProviders",
        json!({ "instanceId": "cursor-work" }),
    )
    .await;
    let provider = initial["providers"]
        .as_array()
        .expect("initial providers")
        .iter()
        .find(|provider| provider["instanceId"] == "cursor-work")
        .expect("initial Cursor provider");
    assert_eq!(provider["version"], "2026.06.19-653a7fb");

    let result = call(
        &control,
        "server.updateProvider",
        json!({ "provider": "cursor", "instanceId": "cursor-work" }),
    )
    .await;
    let provider = result["providers"]
        .as_array()
        .expect("providers")
        .iter()
        .find(|provider| provider["instanceId"] == "cursor-work")
        .expect("updated cursor");
    assert_eq!(provider["version"], "2026.08.04-aaa8809");
    assert_eq!(provider["updateState"]["status"], "succeeded");
    assert_eq!(provider["updateState"]["message"], "Provider updated.");
}

#[tokio::test]
async fn provider_update_rejects_an_instance_driver_mismatch() {
    let (_directory, control) = fixture().await;
    let error = control
        .call(
            "server.updateProvider",
            json!({ "provider": "cursor", "instanceId": "codex" }),
            CancellationToken::new(),
        )
        .await
        .expect_err("mismatched instance and driver must fail");
    assert_eq!(error["_tag"], "ServerProviderUpdateError");
    assert_eq!(error["provider"], "cursor");
}

#[tokio::test]
async fn provider_update_rejects_malformed_instance_ids_without_publishing_update_state() {
    let directory = tempfile::tempdir().expect("temporary state directory");
    let executable = write_provider_fixture(&directory).await;
    let settings = json!({
        "enableProviderUpdateChecks": false,
        "providerInstances": {
            "cursor-work": {
                "driver": "cursor",
                "enabled": true,
                "config": { "binaryPath": executable }
            }
        }
    });
    hermetic_providers::write_hermetic_settings(
        &test_config(directory.path()).state_dir(),
        settings,
    );
    let control = NativeServerControl::new(test_config(directory.path()), auth_descriptor()).await;

    for instance_id in [Value::Null, json!(7), json!({}), json!("not a slug")] {
        let error = control
            .call(
                "server.updateProvider",
                json!({ "provider": "cursor", "instanceId": instance_id }),
                CancellationToken::new(),
            )
            .await
            .expect_err("malformed instance ID must be rejected");
        assert_eq!(error["_tag"], "ServerProviderUpdateError");
        assert_eq!(error["provider"], "cursor");
        assert!(
            error["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("instanceId"))
        );
    }

    let config = call(&control, "server.getConfig", json!({})).await;
    assert_eq!(config["providers"][0]["updateState"], Value::Null);
}
