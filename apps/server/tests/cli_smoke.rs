use std::{
    process::{Command, Stdio},
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use clap::Parser;
use coddy_studio_server::{
    Cli, ConfigError, ServerConfig, ServerRuntime,
    persistence::{BackupTrigger, StatePaths, create_verified_backup, prepare_store},
    resolve_data_root,
};
use rusqlite::Connection;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::Command as TokioCommand,
    time::timeout,
};

#[path = "support/hermetic_providers.rs"]
mod hermetic_providers;

/// Writes hermetic provider settings into `config`'s selected state directory
/// before any boot (in-process or the spawned `coddy-studio` binary) reads it, so
/// neither probes a real host provider CLI. A later boot with the same
/// `--base-dir` and dev URL only reads this file; nothing here re-writes it.
fn seed_hermetic_settings(config: &ServerConfig) {
    hermetic_providers::write_hermetic_settings(&config.state_dir(), json!({}));
}

#[test]
fn hermetic_settings_are_seeded_in_the_selected_state_directory() {
    for (dev_url, state, other_state) in [
        (None, "userdata", "dev"),
        (Some("http://localhost:5733"), "dev", "userdata"),
    ] {
        let root = TempDir::new().expect("temporary settings root");
        let mut config = ServerConfig::new(root.path());
        if let Some(dev_url) = dev_url {
            config = config.with_dev_url(dev_url.parse().expect("dev URL"));
        }
        seed_hermetic_settings(&config);

        let state_dir = root.path().join(state);
        let settings: Value = serde_json::from_slice(
            &std::fs::read(state_dir.join("settings.json")).expect("selected state settings"),
        )
        .expect("hermetic settings JSON");
        assert_eq!(settings["enableProviderUpdateChecks"], false);
        for driver in ["codex", "claudeAgent", "cursor", "grok", "opencode"] {
            let binary = settings["providers"][driver]["binaryPath"]
                .as_str()
                .expect("pinned provider binary");
            let binary = std::path::Path::new(binary);
            assert!(binary.is_absolute() && binary.starts_with(&state_dir));
            assert!(!binary.exists());
        }
        assert!(!root.path().join(other_state).join("settings.json").exists());
    }
}

async fn exchange_startup_admin(handle: &coddy_studio_server::ServerHandle) -> String {
    let startup = handle.startup_access().expect("startup pairing");
    let exchange = reqwest::Client::new()
        .post(format!("http://{}/oauth/token", handle.local_addr()))
        .form(&[
            (
                "grant_type",
                "urn:ietf:params:oauth:grant-type:token-exchange",
            ),
            ("subject_token", startup.credential.as_str()),
            (
                "subject_token_type",
                "urn:coddy-studio:params:oauth:token-type:environment-bootstrap",
            ),
            (
                "requested_token_type",
                "urn:ietf:params:oauth:token-type:access_token",
            ),
            ("client_label", "CLI offer smoke"),
            ("client_device_type", "desktop"),
        ])
        .send()
        .await
        .expect("startup token exchange");
    assert_eq!(exchange.status(), reqwest::StatusCode::OK);
    let token: Value = exchange.json().await.expect("token exchange JSON");
    token["access_token"]
        .as_str()
        .expect("access token")
        .to_owned()
}

#[test]
fn headless_binary_exposes_the_compatible_serve_flags() {
    let output = Command::new(env!("CARGO_BIN_EXE_coddy-studio"))
        .args(["serve", "--help"])
        .output()
        .expect("run coddy-studio serve --help");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 help output");
    for expected in [
        "--host",
        "--port",
        "--base-dir",
        "--bootstrap-fd",
        "--no-browser",
    ] {
        assert!(stdout.contains(expected), "missing {expected} in {stdout}");
    }
}

#[tokio::test]
async fn storage_inspect_prints_one_json_document_for_an_offline_store() {
    let root = TempDir::new().expect("temporary storage root");
    let config = ServerConfig::new(root.path()).with_bind("127.0.0.1", 0);
    seed_hermetic_settings(&config);
    let handle = ServerRuntime::start(config)
        .await
        .expect("seed inspectable store");
    handle.shutdown();
    handle.join().await.expect("stop inspectable store");

    let output = Command::new(env!("CARGO_BIN_EXE_coddy-studio"))
        .args(["storage", "inspect", "--base-dir"])
        .arg(root.path())
        .arg("--json")
        .output()
        .expect("run storage inspect");

    assert!(
        output.status.success(),
        "storage inspect failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).expect("inspection JSON");
    assert_eq!(value["classification"], "existing");
    assert!(value["storageInstanceId"].as_str().is_some());
    assert_eq!(value["backups"], json!([]));
    assert_eq!(
        value["effectiveRoot"],
        root.path()
            .canonicalize()
            .expect("canonical storage root")
            .to_string_lossy()
            .as_ref()
    );
}

#[tokio::test]
async fn storage_restore_prints_json_and_restores_the_selected_verified_generation() {
    let root = TempDir::new().expect("temporary storage root");
    let mut config = ServerConfig::new(root.path());
    let resolved = resolve_data_root(config.data_root_request.clone()).expect("resolve root");
    config.base_dir.clone_from(&resolved.effective);
    config.resolved_data_root = Some(resolved);
    let paths = StatePaths::from_config(&config);
    std::fs::create_dir_all(&paths.state_dir).expect("state directory");
    let prepared = prepare_store(&config)
        .await
        .expect("prepare CLI recovery store");
    prepared
        .database
        .call(|connection| {
            connection.execute(
                "INSERT INTO projection_projects (
                   project_id, title, workspace_root, default_model_selection_json,
                   scripts_json, created_at, updated_at, deleted_at
                 ) VALUES ('cli-project', 'Before restore', '/tmp/cli-project', NULL, '{}',
                           '2026-08-10T00:00:00Z', '2026-08-10T00:00:00Z', NULL)",
                [],
            )?;
            Ok(())
        })
        .await
        .expect("seed CLI project");
    let backup = create_verified_backup(
        &prepared.database,
        &prepared,
        BackupTrigger::PreUpdate,
        env!("CARGO_PKG_VERSION"),
    )
    .await
    .expect("create CLI restore generation");
    prepared
        .database
        .call(|connection| {
            connection.execute(
                "UPDATE projection_projects SET title = 'After backup' WHERE project_id = 'cli-project'",
                [],
            )?;
            Ok(())
        })
        .await
        .expect("mutate CLI project");
    drop(prepared);
    tokio::time::sleep(Duration::from_millis(25)).await;

    let output = Command::new(env!("CARGO_BIN_EXE_coddy-studio"))
        .args(["storage", "restore", "--base-dir"])
        .arg(root.path())
        .args([
            "--backup-id",
            &backup.manifest.backup_id.to_string(),
            "--json",
        ])
        .output()
        .expect("run storage restore");

    assert!(
        output.status.success(),
        "storage restore failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).expect("restore JSON");
    assert_eq!(value["action"], "restore");
    assert!(value["preservedDirectory"].as_str().is_some());
    let restored = Connection::open(&paths.database).expect("restored database");
    let title: String = restored
        .query_row(
            "SELECT title FROM projection_projects WHERE project_id = 'cli-project'",
            [],
            |row| row.get(0),
        )
        .expect("restored CLI project");
    assert_eq!(title, "Before restore");
}

#[tokio::test]
async fn storage_start_empty_exits_nonzero_without_mutating_a_running_store() {
    let root = TempDir::new().expect("temporary active storage root");
    let config = ServerConfig::new(root.path()).with_bind("127.0.0.1", 0);
    seed_hermetic_settings(&config);
    let handle = ServerRuntime::start(config)
        .await
        .expect("start active storage owner");
    let mut config = ServerConfig::new(root.path());
    let resolved = handle.data_root().clone();
    config.base_dir.clone_from(&resolved.effective);
    config.resolved_data_root = Some(resolved);
    let paths = StatePaths::from_config(&config);
    let database_before = std::fs::read(&paths.database).expect("active database bytes");
    let marker_before = std::fs::read(&paths.environment_id).expect("active marker bytes");

    let output = Command::new(env!("CARGO_BIN_EXE_coddy-studio"))
        .args(["storage", "start-empty", "--base-dir"])
        .arg(root.path())
        .arg("--json")
        .output()
        .expect("run unsafe start-empty");

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("project-data store is currently owned by a running server")
    );
    assert_eq!(
        std::fs::read(&paths.database).expect("active database remains"),
        database_before
    );
    assert_eq!(
        std::fs::read(&paths.environment_id).expect("active marker remains"),
        marker_before
    );
    handle.shutdown();
    handle.join().await.expect("stop active storage owner");
}

#[tokio::test]
async fn pairing_issue_prints_a_credential_the_running_server_exchanges() {
    assert_pairing_issue_exchanges(None).await;
}

#[tokio::test]
async fn pairing_issue_with_dev_url_prints_a_credential_the_dev_server_exchanges() {
    assert_pairing_issue_exchanges(Some("http://localhost:5733")).await;
}

async fn assert_pairing_issue_exchanges(dev_url: Option<&str>) {
    let root = TempDir::new().expect("temporary storage root");
    let mut config = ServerConfig::new(root.path()).with_bind("127.0.0.1", 0);
    if let Some(dev_url) = dev_url {
        config = config.with_dev_url(dev_url.parse().expect("dev URL"));
    }
    seed_hermetic_settings(&config);
    let handle = ServerRuntime::start(config)
        .await
        .expect("start pairing storage owner");
    let http_base_url = format!("http://{}", handle.local_addr());

    let mut command = Command::new(env!("CARGO_BIN_EXE_coddy-studio"));
    command
        .env_remove("VITE_DEV_SERVER_URL")
        .args(["pairing", "issue", "--base-dir"])
        .arg(root.path())
        .args(["--label", "SSH bootstrap", "--json"]);
    if let Some(dev_url) = dev_url {
        command.args(["--dev-url", dev_url]);
    }
    let output = command.output().expect("run pairing issue");
    assert!(
        output.status.success(),
        "pairing issue failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 pairing output");
    let line = stdout
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .expect("pairing JSON line");
    let value: Value = serde_json::from_str(line).expect("pairing JSON document");
    let credential = value["credential"].as_str().expect("credential string");
    assert!(!credential.trim().is_empty());
    assert_eq!(value["label"], "SSH bootstrap");
    assert!(value["expiresAt"].as_str().is_some());

    let exchange = reqwest::Client::new()
        .post(format!("{http_base_url}/oauth/token"))
        .form(&[
            (
                "grant_type",
                "urn:ietf:params:oauth:grant-type:token-exchange",
            ),
            ("subject_token", credential),
            (
                "subject_token_type",
                "urn:coddy-studio:params:oauth:token-type:environment-bootstrap",
            ),
            (
                "requested_token_type",
                "urn:ietf:params:oauth:token-type:access_token",
            ),
            ("client_label", "CLI pairing smoke"),
            ("client_device_type", "desktop"),
        ])
        .send()
        .await
        .expect("token exchange request");
    assert_eq!(exchange.status(), reqwest::StatusCode::OK);
    let token: Value = exchange.json().await.expect("token exchange JSON");
    assert!(
        token["access_token"]
            .as_str()
            .is_some_and(|token| !token.is_empty())
    );
    assert_eq!(token["token_type"], "Bearer");
    assert!(
        token["scope"]
            .as_str()
            .is_some_and(|scope| scope.contains("access:write"))
    );

    handle.shutdown();
    handle.join().await.expect("stop pairing storage owner");
}

#[test]
fn pairing_issue_fails_closed_without_a_data_store() {
    let root = TempDir::new().expect("temporary empty root");

    let output = Command::new(env!("CARGO_BIN_EXE_coddy-studio"))
        .args(["pairing", "issue", "--base-dir"])
        .arg(root.path())
        .arg("--json")
        .output()
        .expect("run pairing issue without a store");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no Coddy Studio data store"), "{stderr}");
    assert!(
        output.stdout.is_empty(),
        "no credential may be printed on failure"
    );
}

#[test]
#[cfg(windows)]
fn headless_binary_reports_invalid_bootstrap_descriptors_without_panicking() {
    let output = Command::new(env!("CARGO_BIN_EXE_coddy-studio"))
        .args(["serve", "--bootstrap-fd", "4"])
        .output()
        .expect("run coddy-studio with unsupported bootstrap fd");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("UTF-8 error output");
    assert!(stderr.contains("bootstrap file descriptor 4 is unsupported on this platform"));
    assert!(!stderr.to_ascii_lowercase().contains("panicked"));
}

#[test]
fn serve_flags_have_the_same_value_before_or_after_the_subcommand() {
    let temp = TempDir::new().expect("temporary base directory");
    let base_dir = temp.path().to_string_lossy();
    let before = Cli::try_parse_from([
        "coddy-studio",
        "--host",
        "0.0.0.0",
        "--port",
        "0",
        "--base-dir",
        base_dir.as_ref(),
        "serve",
    ])
    .expect("flags before serve")
    .into_server_config()
    .expect("configuration before serve");
    let after = Cli::try_parse_from([
        "coddy-studio",
        "serve",
        "--host",
        "0.0.0.0",
        "--port",
        "0",
        "--base-dir",
        base_dir.as_ref(),
    ])
    .expect("flags after serve")
    .into_server_config()
    .expect("configuration after serve");

    assert_eq!(before.host, after.host);
    assert_eq!(before.port, after.port);
    assert_eq!(before.base_dir, after.base_dir);
    assert!(before.no_browser);
    assert!(after.no_browser);
}

#[test]
fn start_opens_a_browser_unless_disabled_while_serve_is_always_headless() {
    let start = Cli::try_parse_from(["coddy-studio", "start"])
        .expect("start arguments")
        .into_server_config()
        .expect("start configuration");
    let disabled = Cli::try_parse_from(["coddy-studio", "start", "--no-browser"])
        .expect("disabled browser arguments")
        .into_server_config()
        .expect("disabled browser configuration");
    let serve = Cli::try_parse_from(["coddy-studio", "serve"])
        .expect("serve arguments")
        .into_server_config()
        .expect("serve configuration");

    assert!(!start.no_browser);
    assert!(disabled.no_browser);
    assert!(serve.no_browser);
}

#[test]
fn explicit_static_directory_must_contain_the_web_entry_point() {
    let root = TempDir::new().expect("temporary static root");
    let missing = root.path().join("missing");

    let error = Cli::try_parse_from([
        "coddy-studio",
        "serve",
        "--static-dir",
        missing.to_str().expect("UTF-8 fixture path"),
    ])
    .expect("parse static directory")
    .into_server_config()
    .expect_err("reject missing static entry point");

    assert!(matches!(error, ConfigError::StaticAssets(_)));
}

#[tokio::test]
async fn desktop_bootstrap_rejects_an_empty_shutdown_token() {
    let temp = TempDir::new().expect("temporary base directory");
    let mut child = TokioCommand::new(env!("CARGO_BIN_EXE_coddy-studio"))
        .args([
            "serve",
            "--mode",
            "desktop",
            "--host",
            "127.0.0.1",
            "--port",
            "0",
            "--base-dir",
        ])
        .arg(temp.path())
        .args(["--bootstrap-fd", "0"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn coddy-studio server");
    let bootstrap = json!({
        "mode": "desktop",
        "noBrowser": true,
        "port": 0,
        "host": "127.0.0.1",
        "desktopBootstrapToken": "",
        "tailscaleServeEnabled": false,
        "tailscaleServePort": 443
    });
    let mut stdin = child.stdin.take().expect("child stdin");
    stdin
        .write_all(format!("{bootstrap}\n").as_bytes())
        .await
        .expect("write empty-token bootstrap");
    drop(stdin);

    let mut stderr = child.stderr.take().expect("child stderr");
    let status = match timeout(Duration::from_secs(10), child.wait()).await {
        Ok(status) => status.expect("server exit status"),
        Err(_) => {
            child.kill().await.expect("kill server after timeout");
            panic!("server accepted an empty desktop bootstrap token");
        }
    };
    let mut error = String::new();
    stderr
        .read_to_string(&mut error)
        .await
        .expect("read server error");
    assert!(!status.success());
    assert!(
        error.contains("desktop bootstrap token must not be empty"),
        "{error}"
    );
}

#[test]
#[cfg(unix)]
fn headless_configuration_reads_an_inherited_nonzero_bootstrap_fd() {
    use std::{
        io::Write,
        os::{fd::IntoRawFd, unix::net::UnixStream},
    };

    let (mut writer, reader) = UnixStream::pair().expect("bootstrap socket pair");
    let bootstrap = json!({
        "mode": "desktop",
        "noBrowser": true,
        "port": 4567,
        "host": "127.0.0.1",
        "desktopBootstrapToken": "inherited-fd-secret",
        "tailscaleServeEnabled": false,
        "tailscaleServePort": 443
    });
    writeln!(writer, "{bootstrap}").expect("write inherited bootstrap");
    let fd = reader.into_raw_fd().to_string();

    let config = Cli::try_parse_from(["coddy-studio", "serve", "--bootstrap-fd", fd.as_str()])
        .expect("inherited bootstrap arguments")
        .into_server_config()
        .expect("inherited bootstrap configuration");
    assert_eq!(config.port, 4567);
    assert_eq!(
        config.desktop_bootstrap_token.as_deref(),
        Some("inherited-fd-secret")
    );
}

#[tokio::test]
async fn headless_binary_reads_desktop_bootstrap_and_shuts_down_over_http() {
    let temp = TempDir::new().expect("temporary base directory");
    seed_hermetic_settings(&ServerConfig::new(temp.path()));
    let mut child = TokioCommand::new(env!("CARGO_BIN_EXE_coddy-studio"))
        .args([
            "serve",
            "--mode",
            "desktop",
            "--host",
            "127.0.0.1",
            "--port",
            "0",
            "--base-dir",
        ])
        .arg(temp.path())
        .args(["--no-browser", "--bootstrap-fd", "0"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn coddy-studio server");

    let bootstrap = json!({
        "mode": "desktop",
        "noBrowser": true,
        "port": 0,
        "coddyStudioHome": temp.path(),
        "host": "127.0.0.1",
        "desktopBootstrapToken": "process-smoke-secret",
        "tailscaleServeEnabled": false,
        "tailscaleServePort": 443
    });
    let mut stdin = child.stdin.take().expect("child stdin");
    stdin
        .write_all(format!("{bootstrap}\n").as_bytes())
        .await
        .expect("write desktop bootstrap");
    drop(stdin);

    let stdout = child.stdout.take().expect("child stdout");
    let mut lines = BufReader::new(stdout).lines();
    let ready_line = match timeout(Duration::from_secs(30), lines.next_line()).await {
        Ok(result) => result
            .expect("read readiness line")
            .expect("server readiness line"),
        Err(error) => {
            child.kill().await.expect("terminate unready server");
            panic!("server readiness timeout: {error}");
        }
    };
    let ready: Value = serde_json::from_str(&ready_line).expect("readiness JSON");
    let http_base_url = ready["httpBaseUrl"].as_str().expect("HTTP base URL");

    let shutdown = reqwest::Client::new()
        .post(format!(
            "{http_base_url}/.well-known/coddy-studio/desktop/shutdown"
        ))
        .header(
            "x-coddy-studio-desktop-bootstrap-token",
            "process-smoke-secret",
        )
        .send()
        .await
        .expect("desktop shutdown request");
    assert_eq!(shutdown.status(), reqwest::StatusCode::ACCEPTED);

    let status = timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("server exit timeout")
        .expect("server exit status");
    assert!(status.success(), "server exited with {status}");
}

#[tokio::test]
async fn pairing_offer_prints_a_code_the_running_server_redeems() {
    assert_pairing_offer_redeems(None).await;
}

#[tokio::test]
async fn pairing_offer_with_dev_url_prints_a_code_the_dev_server_redeems() {
    assert_pairing_offer_redeems(Some("http://localhost:5733")).await;
}

async fn assert_pairing_offer_redeems(dev_url: Option<&str>) {
    let root = TempDir::new().expect("temporary storage root");
    let mut config = ServerConfig::new(root.path()).with_bind("127.0.0.1", 0);
    if let Some(dev_url) = dev_url {
        config = config.with_dev_url(dev_url.parse().expect("dev URL"));
    }
    seed_hermetic_settings(&config);
    let handle = ServerRuntime::start(config)
        .await
        .expect("start pairing storage owner");

    let mut command = Command::new(env!("CARGO_BIN_EXE_coddy-studio"));
    command
        .env_remove("VITE_DEV_SERVER_URL")
        .args(["pairing", "offer", "--base-dir"])
        .arg(root.path())
        .args([
            "--endpoint",
            "http://192.168.1.20:3773",
            "--name",
            "ai-server",
            "--label",
            "laptop",
            "--json",
        ]);
    if let Some(dev_url) = dev_url {
        command.args(["--dev-url", dev_url]);
    }
    let output = command.output().expect("run pairing offer");
    assert!(
        output.status.success(),
        "pairing offer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 offer output");
    assert_eq!(
        stdout.trim().lines().count(),
        1,
        "exactly one JSON line: {stdout}"
    );
    let value: Value = serde_json::from_str(stdout.trim()).expect("offer JSON document");
    let code = value["code"].as_str().expect("code string");
    assert_eq!(value["link"], format!("coddy-studio://pair?code={code}"));
    assert_eq!(value["reach"], "another-device");
    assert_eq!(value["endpoint"], "http://192.168.1.20:3773");
    assert_eq!(value["name"], "ai-server");
    assert!(value["expiresAt"].as_str().is_some());
    let payload = coddy_studio_server::auth_pairing_code::decode_pairing_code(code)
        .expect("pairing code decodes");
    assert_eq!(payload.name, "ai-server");

    // The running server lists pairings from a cached view that its authority
    // watcher reconciles from the database on a short interval, so an offer
    // inserted by another process becomes visible shortly after, not instantly.
    let admin = exchange_startup_admin(&handle).await;
    let client = reqwest::Client::new();
    let mut listed = None;
    for _ in 0..40 {
        let links = client
            .get(format!(
                "http://{}/api/auth/pairing-links",
                handle.local_addr()
            ))
            .bearer_auth(&admin)
            .send()
            .await
            .expect("pairing list request");
        assert_eq!(links.status(), reqwest::StatusCode::OK);
        let links: Value = links.json().await.expect("pairing list JSON");
        listed = links
            .as_array()
            .expect("pairing list")
            .iter()
            .find(|link| link["id"] == value["id"])
            .cloned();
        if listed.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let listed = listed.expect("offline offer becomes visible to the running server");
    assert_eq!(listed["label"], "laptop");

    handle.shutdown();
    handle.join().await.expect("stop pairing storage owner");
}

#[test]
fn pairing_offer_fails_closed_without_a_data_store() {
    let root = TempDir::new().expect("temporary empty root");
    let output = Command::new(env!("CARGO_BIN_EXE_coddy-studio"))
        .args(["pairing", "offer", "--base-dir"])
        .arg(root.path())
        .args(["--endpoint", "http://192.168.1.20:3773", "--json"])
        .output()
        .expect("run pairing offer without a store");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("start the server on this data root first"),
        "{stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "no code may be printed on failure"
    );
}

async fn prepare_pairing_store(root: &TempDir, dev_url: Option<&str>) -> (StatePaths, String) {
    let mut config = ServerConfig::new(root.path());
    if let Some(dev_url) = dev_url {
        config = config.with_dev_url(dev_url.parse().expect("dev URL"));
    }
    let resolved =
        resolve_data_root(config.data_root_request.clone()).expect("resolve pairing root");
    config.base_dir.clone_from(&resolved.effective);
    config.resolved_data_root = Some(resolved);
    std::fs::create_dir_all(config.state_dir()).expect("pairing state directory");
    let prepared = prepare_store(&config).await.expect("prepare pairing store");
    let storage_instance_id = prepared.storage_instance_id.to_string();
    // Startup persists a 64-byte private-then-public Noise identity before offers can be minted.
    let keypair = snow::Builder::new(
        "Noise_NK_25519_ChaChaPoly_SHA256"
            .parse()
            .expect("Noise params"),
    )
    .generate_keypair()
    .expect("fixture host identity");
    std::fs::create_dir_all(&prepared.paths.secrets_dir).expect("fixture secrets directory");
    std::fs::write(
        prepared.paths.secrets_dir.join("host-identity-x25519.bin"),
        [keypair.private, keypair.public].concat(),
    )
    .expect("persist fixture host identity");
    drop(prepared.database);
    (prepared.paths, storage_instance_id)
}

fn pairing_command(root: &TempDir, subcommand: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_coddy-studio"));
    command
        .env_remove("VITE_DEV_SERVER_URL")
        .args(["pairing", subcommand, "--base-dir"])
        .arg(root.path())
        .arg("--json");
    if subcommand == "offer" {
        command.args(["--endpoint", "http://192.168.1.20:3773"]);
    }
    command
}

#[tokio::test]
async fn pairing_commands_without_dev_url_report_the_existing_dev_store() {
    let root = TempDir::new().expect("temporary pairing root");
    let (paths, _) = prepare_pairing_store(&root, Some("http://localhost:5733")).await;
    let selected = paths.base_dir.join("userdata").join("state.sqlite");
    for subcommand in ["offer", "issue"] {
        let output = pairing_command(&root, subcommand)
            .output()
            .expect("run pairing CLI");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{subcommand}: {stderr}");
        assert!(output.stdout.is_empty(), "no pairing output on failure");
        assert!(
            stderr.contains(selected.to_string_lossy().as_ref()),
            "{stderr}"
        );
        assert!(
            stderr.contains(paths.database.to_string_lossy().as_ref()),
            "{stderr}"
        );
        assert!(stderr.contains("pass the same --dev-url"), "{stderr}");
        assert!(
            !selected.exists(),
            "must not create a store in the wrong state"
        );
    }
}

#[tokio::test]
async fn pairing_commands_with_dev_url_report_the_existing_userdata_store() {
    let root = TempDir::new().expect("temporary pairing root");
    let (paths, _) = prepare_pairing_store(&root, None).await;
    let selected = paths.base_dir.join("dev").join("state.sqlite");
    for subcommand in ["offer", "issue"] {
        let output = pairing_command(&root, subcommand)
            .args(["--dev-url", "http://localhost:5733"])
            .output()
            .expect("run pairing CLI");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{subcommand}: {stderr}");
        assert!(output.stdout.is_empty(), "no pairing output on failure");
        assert!(
            stderr.contains(selected.to_string_lossy().as_ref()),
            "{stderr}"
        );
        assert!(
            stderr.contains(paths.database.to_string_lossy().as_ref()),
            "{stderr}"
        );
        assert!(stderr.contains("drop --dev-url"), "{stderr}");
        assert!(
            !selected.exists(),
            "must not create a store in the wrong state"
        );
    }
}

#[test]
fn pairing_commands_with_dev_url_fail_closed_without_either_store() {
    let root = TempDir::new().expect("temporary empty pairing root");
    for subcommand in ["offer", "issue"] {
        let output = pairing_command(&root, subcommand)
            .args(["--dev-url", "http://localhost:5733"])
            .output()
            .expect("run pairing CLI without a store");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{subcommand}: {stderr}");
        assert!(output.stdout.is_empty(), "no pairing output on failure");
        assert!(
            stderr.contains("start the server on this data root first"),
            "{stderr}"
        );
        assert!(!root.path().join("dev/state.sqlite").exists());
        assert!(!root.path().join("userdata/state.sqlite").exists());
    }
}

async fn assert_pairing_commands_select_existing_state(
    args: &[&str],
    dev_env: Option<&str>,
    selected_state: &str,
) {
    let root = TempDir::new().expect("temporary pairing root");
    let (userdata, userdata_id) = prepare_pairing_store(&root, None).await;
    let (dev, dev_id) = prepare_pairing_store(&root, Some("http://localhost:5733")).await;
    for subcommand in ["offer", "issue"] {
        let mut command = pairing_command(&root, subcommand);
        command.args(args);
        if let Some(dev_env) = dev_env {
            command.env("VITE_DEV_SERVER_URL", dev_env);
        }
        let output = command.output().expect("run pairing CLI");
        assert!(
            output.status.success(),
            "{subcommand}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: Value = serde_json::from_slice(&output.stdout).expect("pairing JSON");
        let id = value["id"].as_str().expect("pairing ID");
        for (state, paths, storage_id) in [
            ("userdata", &userdata, &userdata_id),
            ("dev", &dev, &dev_id),
        ] {
            let connection = Connection::open_with_flags(
                &paths.database,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .expect("read pairing store");
            let count: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM auth_pairing_links WHERE id = ?1",
                    [id],
                    |row| row.get(0),
                )
                .expect("pairing row count");
            assert_eq!(
                count,
                i64::from(state == selected_state),
                "{subcommand}: {state}"
            );
            if subcommand == "offer" && state == selected_state {
                let payload = coddy_studio_server::auth_pairing_code::decode_pairing_code(
                    value["code"].as_str().expect("pairing code"),
                )
                .expect("decode pairing code");
                assert_eq!(&payload.storage_instance_id, storage_id);
                let identity = std::fs::read(paths.secrets_dir.join("host-identity-x25519.bin"))
                    .expect("selected host identity");
                assert_eq!(
                    payload.host_key,
                    URL_SAFE_NO_PAD.encode(&identity[32..]),
                    "host identity must use the selected state"
                );
            }
        }
    }
}

#[tokio::test]
async fn pairing_commands_with_dev_url_select_dev_when_both_stores_exist() {
    assert_pairing_commands_select_existing_state(
        &["--dev-url", "http://localhost:5733"],
        None,
        "dev",
    )
    .await;
}

#[tokio::test]
async fn pairing_commands_with_dev_environment_select_dev_when_both_stores_exist() {
    assert_pairing_commands_select_existing_state(&[], Some("http://localhost:5733"), "dev").await;
}

#[tokio::test]
async fn pairing_commands_without_dev_url_select_userdata_when_both_stores_exist() {
    assert_pairing_commands_select_existing_state(&[], None, "userdata").await;
}

#[tokio::test]
async fn pairing_offer_rejects_a_loopback_endpoint_for_another_device() {
    let root = TempDir::new().expect("temporary storage root");
    let config = ServerConfig::new(root.path()).with_bind("127.0.0.1", 0);
    seed_hermetic_settings(&config);
    let handle = ServerRuntime::start(config)
        .await
        .expect("start pairing storage owner");
    let output = Command::new(env!("CARGO_BIN_EXE_coddy-studio"))
        .args(["pairing", "offer", "--base-dir"])
        .arg(root.path())
        .args(["--endpoint", "http://127.0.0.1:3773"])
        .output()
        .expect("run pairing offer with a loopback endpoint");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("reach does not match the offered endpoint"),
        "{stderr}"
    );
    assert!(output.stdout.is_empty());
    handle.shutdown();
    handle.join().await.expect("stop pairing storage owner");
}

#[tokio::test]
async fn serve_on_loopback_prints_no_startup_pairing_code() {
    let temp = TempDir::new().expect("temporary base directory");
    seed_hermetic_settings(&ServerConfig::new(temp.path()));
    let mut child = TokioCommand::new(env!("CARGO_BIN_EXE_coddy-studio"))
        .args(["serve", "--host", "127.0.0.1", "--port", "0", "--base-dir"])
        .arg(temp.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn coddy-studio server");

    let stdout = child.stdout.take().expect("child stdout");
    let mut lines = BufReader::new(stdout).lines();
    let ready_line = match timeout(Duration::from_secs(30), lines.next_line()).await {
        Ok(result) => result
            .expect("read readiness line")
            .expect("server readiness line"),
        Err(error) => {
            child.kill().await.expect("terminate unready server");
            panic!("server readiness timeout: {error}");
        }
    };
    let ready: Value = serde_json::from_str(&ready_line).expect("readiness JSON");
    assert!(ready.get("pairingCode").is_none());
    assert!(ready["pairingUrl"].as_str().is_some());

    child.kill().await.expect("terminate server");
}

#[test]
fn service_help_lists_install_uninstall_and_status() {
    let output = Command::new(env!("CARGO_BIN_EXE_coddy-studio"))
        .args(["service", "--help"])
        .output()
        .expect("run service help");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    for expected in ["install", "uninstall", "status"] {
        assert!(stdout.contains(expected), "missing {expected}: {stdout}");
    }
    let install = Command::new(env!("CARGO_BIN_EXE_coddy-studio"))
        .args(["service", "install", "--help"])
        .output()
        .expect("run service install help");
    let stdout = String::from_utf8_lossy(&install.stdout);
    for expected in ["--host", "--port", "--base-dir", "--static-dir", "--json"] {
        assert!(stdout.contains(expected), "missing {expected}: {stdout}");
    }
}
