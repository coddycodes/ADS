#[path = "support/executable_fixture.rs"]
mod executable_fixture;

use bibcode_server::{
    cloud, preview::gateway::relay::RelayPreviewHosts,
    production::managed_endpoint::ManagedEndpointRuntime, text_generation,
};

use cloud::{RelayClientInstallEvent, RelayClientService, RelayClientStatus};
use serde_json::json;
use text_generation::{normalize_cli_error, sanitize_commit_subject, sanitize_thread_title};

#[tokio::test]
async fn relay_client_service_reports_status_and_streams_install_progress() {
    let service = RelayClientService::new(
        || async {
            RelayClientStatus::Missing {
                version: "2026.7.0".to_owned(),
            }
        },
        |report| async move {
            report(RelayClientInstallEvent::Progress {
                stage: "checking".to_owned(),
            })
            .await?;
            report(RelayClientInstallEvent::Progress {
                stage: "downloading".to_owned(),
            })
            .await?;
            Ok(RelayClientStatus::Available {
                executable_path: "/tmp/bibcode/tools/cloudflared".to_owned(),
                source: "managed".to_owned(),
                version: "2026.7.0".to_owned(),
            })
        },
    );

    let status = service.resolve().await;
    assert_eq!(
        status,
        RelayClientStatus::Missing {
            version: "2026.7.0".to_owned()
        }
    );

    let events = service.install().await.expect("install");
    assert_eq!(
        events,
        vec![
            RelayClientInstallEvent::Progress {
                stage: "checking".to_owned()
            },
            RelayClientInstallEvent::Progress {
                stage: "downloading".to_owned()
            },
            RelayClientInstallEvent::Complete {
                status: RelayClientStatus::Available {
                    executable_path: "/tmp/bibcode/tools/cloudflared".to_owned(),
                    source: "managed".to_owned(),
                    version: "2026.7.0".to_owned(),
                }
            }
        ]
    );
}

#[tokio::test]
async fn managed_endpoint_runtime_handles_disabled_unsupported_and_missing_connectors() {
    let runtime = ManagedEndpointRuntime::default();
    let _endpoint = runtime.endpoint();

    assert_eq!(
        runtime.apply(serde_json::Value::Null).await.unwrap(),
        json!({"status":"disabled"})
    );
    assert_eq!(
        runtime
            .apply(json!({
                "providerKind":"future_provider",
                "connectorToken":"ignored",
            }))
            .await
            .unwrap(),
        json!({"status":"unsupported","providerKind":"future_provider"})
    );
    assert_eq!(
        runtime
            .apply(json!({
                "providerKind":"cloudflare_tunnel",
                "connectorToken":"fixture-token",
                "tunnelId":"tunnel-1",
                "tunnelName":"Fixture tunnel",
                "hostname":"env.zone.test",
                "previewHostnames":["env-p1.zone.test"],
            }))
            .await
            .unwrap(),
        json!({
            "status":"failed",
            "providerKind":"cloudflare_tunnel",
            "reason":"The relay client is not installed.",
            "tunnelId":"tunnel-1",
            "tunnelName":"Fixture tunnel",
        })
    );
    // A tunnel that is not running publishes no hostnames.
    assert_eq!(runtime.relay_hosts().current(), None);

    runtime.shutdown().await;
}

#[cfg(any(unix, windows))]
fn long_running_connector_fixture(directory: &std::path::Path) -> std::path::PathBuf {
    #[cfg(unix)]
    {
        let executable = directory.join("bibcode-connect");
        executable_fixture::write_executable(
            &executable,
            "#!/bin/sh\ntrap 'exit 0' TERM INT\nwhile true; do sleep 1; done\n",
        );
        executable
    }
    #[cfg(windows)]
    {
        let executable = directory.join("bibcode-connect.cmd");
        executable_fixture::write_executable(
            &executable,
            "@echo off\r\n\
             :loop\r\n\
             %SystemRoot%\\System32\\ping.exe -n 2 127.0.0.1 >nul\r\n\
             goto loop\r\n",
        );
        executable
    }
}

#[tokio::test]
async fn managed_endpoint_runtime_reuses_replaces_and_stops_connectors() {
    let directory = tempfile::tempdir().expect("connector directory");
    let executable = long_running_connector_fixture(directory.path());

    let runtime = ManagedEndpointRuntime::with_executable_override(executable);
    let first_config = json!({
        "providerKind":"cloudflare_tunnel",
        "connectorToken":"fixture-token-1",
        "tunnelId":"tunnel-1",
        "tunnelName":"Fixture tunnel",
    });
    let first = runtime.apply(first_config.clone()).await.unwrap();
    assert_eq!(first["status"], "running");
    assert!(first["pid"].as_u64().is_some());

    let reused = runtime.apply(first_config).await.unwrap();
    assert_eq!(reused["pid"], first["pid"]);

    let replacement = runtime
        .apply(json!({
            "providerKind":"cloudflare_tunnel",
            "connectorToken":"fixture-token-2",
            "tunnelId":"tunnel-2",
            "tunnelName":"Replacement tunnel",
        }))
        .await
        .unwrap();
    assert_eq!(replacement["status"], "running");
    assert_eq!(replacement["tunnelId"], "tunnel-2");
    assert_ne!(replacement["pid"], first["pid"]);

    let hosts = runtime.relay_hosts();
    assert_eq!(
        hosts.current(),
        None,
        "a config without hostnames publishes none"
    );
    let republished = runtime
        .apply(json!({
            "providerKind":"cloudflare_tunnel",
            "connectorToken":"fixture-token-2",
            "tunnelId":"tunnel-2",
            "tunnelName":"Replacement tunnel",
            "hostname":"env.zone.test",
            "previewHostnames":[
                "env-p1.zone.test","env-p2.zone.test","env-p3.zone.test",
                "env-p4.zone.test","env-p5.zone.test",
            ],
        }))
        .await
        .unwrap();
    // Same connector key: the running connector is kept, only the hostnames change.
    assert_eq!(republished["pid"], replacement["pid"]);
    assert_eq!(
        hosts.current(),
        Some(RelayPreviewHosts {
            hostname: "env.zone.test".into(),
            previews: (1..=4)
                .map(|slot| format!("env-p{slot}.zone.test"))
                .collect(),
        })
    );
    assert!(
        runtime
            .apply(json!({
                "providerKind":"cloudflare_tunnel",
                "connectorToken":"fixture-token-2",
                "hostname":"Bad_Host.zone.test",
            }))
            .await
            .is_err(),
        "a malformed hostname rejects the config"
    );
    runtime.apply(serde_json::Value::Null).await.unwrap();
    assert_eq!(hosts.current(), None, "disabling withdraws the hostnames");

    runtime.shutdown().await;
    runtime.shutdown().await;
}

#[test]
fn text_generation_helpers_match_current_failure_and_sanitization_behavior() {
    assert_eq!(
        sanitize_commit_subject("  Add a much better commit subject.  "),
        "Add a much better commit subject"
    );
    assert_eq!(
        sanitize_thread_title("   \"A concise first user prompt\"   "),
        "A concise first user prompt"
    );

    let error = normalize_cli_error(
        "codex",
        "generateCommitMessage",
        &std::io::Error::new(std::io::ErrorKind::NotFound, "spawn codex ENOENT"),
        "fallback",
    );
    assert_eq!(error.operation, "generateCommitMessage");
    assert_eq!(
        error.detail,
        "Codex CLI (`codex`) is required but not available on PATH."
    );
}
