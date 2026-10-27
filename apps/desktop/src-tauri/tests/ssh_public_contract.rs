use std::{collections::BTreeSet, fs, time::Duration};

use coddy_studio_desktop_lib::ssh::{
    DiscoveredSshHost, REMOTE_PAIRING_SCRIPT, RemoteLaunchResult, SshAuthOptions,
    SshEnvironmentBootstrap, SshEnvironmentLaunchPlan, SshEnvironmentManager, SshEnvironmentTarget,
    SshPasswordPromptManager, SshPasswordPromptRequestError, SshPasswordPromptResolution,
    SshPasswordPromptResolveError, SshPasswordRequest, default_home_dir, discover_ssh_hosts,
    parse_known_hosts_hostnames, parse_remote_launch_result, parse_remote_pairing_credential,
};

#[tokio::test]
async fn public_environment_manager_surfaces_native_ssh_connection_failures() {
    use tauri::test::{mock_builder, mock_context, noop_assets};

    let app = mock_builder()
        .build(mock_context(noop_assets()))
        .expect("mock Tauri app");
    let manager = SshEnvironmentManager::new();
    let prompts = SshPasswordPromptManager::with_timeout(Duration::from_millis(10));
    let unreachable = SshEnvironmentTarget {
        alias: "127.0.0.1".to_owned(),
        hostname: "127.0.0.1".to_owned(),
        username: None,
        port: Some(1),
    };

    let ensure_error = manager
        .ensure_environment(app.handle(), &prompts, unreachable.clone(), None)
        .await
        .expect_err("closed local port should reject remote bootstrap");
    assert!(
        ensure_error.contains("SSH launch command failed"),
        "{ensure_error}"
    );

    let disconnect_error = manager
        .disconnect_environment(app.handle(), &prompts, unreachable)
        .await
        .expect_err("closed local port should reject remote cleanup");
    assert!(
        disconnect_error.contains("SSH stop command failed"),
        "{disconnect_error}"
    );

    let prompt_error = prompts
        .request_password(
            app.handle(),
            SshPasswordRequest {
                destination: "127.0.0.1".to_owned(),
                username: None,
                prompt: "Password".to_owned(),
            },
        )
        .await
        .expect_err("unresolved prompt should expire");
    assert!(matches!(
        prompt_error,
        SshPasswordPromptRequestError::TimedOut { .. }
    ));
}

fn target() -> SshEnvironmentTarget {
    SshEnvironmentTarget {
        alias: " devbox ".to_string(),
        hostname: " devbox.internal ".to_string(),
        username: Some(" alice ".to_string()),
        port: Some(2222),
    }
}

#[test]
fn public_launch_plans_normalize_targets_and_preserve_bootstrap_contracts() {
    let external = SshEnvironmentLaunchPlan::external(target(), 45_123)
        .expect("external launch plan should build");
    assert_eq!(external.target.alias, "devbox");
    assert_eq!(external.target.hostname, "devbox.internal");
    assert_eq!(external.target.username.as_deref(), Some("alice"));
    assert_eq!(external.remote_port, 3773);
    assert_eq!(external.remote_server_kind, "external");
    assert_eq!(external.http_base_url, "http://127.0.0.1:45123/");
    assert!(external.args.windows(2).any(|pair| pair == ["-p", "2222"]));
    assert_eq!(
        external.args.last().map(String::as_str),
        Some("alice@devbox")
    );

    let managed = SshEnvironmentLaunchPlan::forward_with_auth(
        SshEnvironmentTarget {
            alias: String::new(),
            hostname: "host.internal".to_string(),
            username: None,
            port: None,
        },
        45_124,
        RemoteLaunchResult {
            remote_port: 41_111,
            server_kind: "managed".to_string(),
        },
        &SshAuthOptions::with_secret("secret".to_string()),
    )
    .expect("managed launch plan should build");
    assert_eq!(managed.target.alias, "host.internal");
    assert_eq!(managed.args[1], "BatchMode=no");
    assert_eq!(managed.remote_server_kind, "managed");

    let batch = SshEnvironmentLaunchPlan::forward(
        managed.target.clone(),
        45_125,
        RemoteLaunchResult {
            remote_port: 41_112,
            server_kind: "unexpected".to_string(),
        },
    )
    .expect("batch launch plan should build");
    assert_eq!(batch.args[1], "BatchMode=yes");
    assert_eq!(batch.remote_server_kind, "managed");

    let bootstrap = SshEnvironmentBootstrap::new(
        managed.target.clone(),
        managed.remote_port,
        managed.http_base_url.clone(),
        managed.ws_base_url.clone(),
        Some("pairing-token".to_string()),
        "managed",
    );
    let external_bootstrap = SshEnvironmentBootstrap::external(
        external.target,
        external.remote_port,
        external.http_base_url,
        external.ws_base_url,
        None,
    );
    assert_eq!(bootstrap.remote_server_kind, "managed");
    assert_eq!(external_bootstrap.remote_server_kind, "external");
    assert_eq!(external_bootstrap.pairing_token, None);

    assert!(
        SshEnvironmentLaunchPlan::external(
            SshEnvironmentTarget {
                alias: " ".to_string(),
                hostname: " ".to_string(),
                username: None,
                port: None,
            },
            45_126,
        )
        .is_err()
    );
}

#[test]
fn public_remote_parsers_cover_success_defaults_and_validation_errors() {
    assert_eq!(
        parse_remote_pairing_credential("shell banner\n{\"credential\":\" pairing-token \"}\n",),
        Ok("pairing-token".to_string())
    );
    for invalid in [
        "",
        "not-json",
        "{\"credential\":42}",
        "{\"credential\":\"\"}",
    ] {
        assert!(parse_remote_pairing_credential(invalid).is_err());
    }

    assert_eq!(
        parse_remote_launch_result("banner\n{\"remotePort\":4111}\n")
            .expect("launch document should parse"),
        RemoteLaunchResult {
            remote_port: 4111,
            server_kind: "managed".to_string(),
        }
    );
    assert_eq!(
        parse_remote_launch_result("{\"remotePort\":3773,\"serverKind\":\"external\"}")
            .expect("external launch document should parse")
            .server_kind,
        "external"
    );
    for invalid in [
        "",
        "not-json",
        "{\"remotePort\":0}",
        "{\"remotePort\":65536}",
        "{\"remotePort\":3773,\"serverKind\":\"bogus\"}",
    ] {
        assert!(parse_remote_launch_result(invalid).is_err());
    }

    assert_eq!(
        parse_known_hosts_hostnames(
            "github.com,git.example ssh-ed25519 AAAA\n\
             |1|hashed|entry ssh-ed25519 BBBB\n\
             @cert-authority *.example.com ssh-ed25519 CCCC\n\
             [bastion.example.com]:2222 ssh-ed25519 DDDD\n\
             ::1 ssh-ed25519 EEEE\n",
        ),
        BTreeSet::from([
            "::1".to_string(),
            "bastion.example.com".to_string(),
            "git.example".to_string(),
            "github.com".to_string(),
        ])
    );
}

#[test]
fn public_remote_pairing_command_targets_the_native_cli_and_parses_its_output() {
    assert!(REMOTE_PAIRING_SCRIPT.starts_with("# coddy-studio-ssh:pairing\nset -eu\n"));
    assert!(REMOTE_PAIRING_SCRIPT.contains("command -v coddy-studio"));
    // The command runs under the remote watchdog, bounded by the script's
    // first argument.
    assert_eq!(
        REMOTE_PAIRING_SCRIPT.lines().last(),
        Some(
            "run_bounded \"$1\" coddy-studio pairing issue --base-dir \"$HOME/.coddy/studio\" --json"
        )
    );

    assert_eq!(
        parse_remote_pairing_credential(
            "Warning: Permanently added 'devbox' to the list of known hosts.\n\
             {\"id\":\"3f3c0f6e-8a0e-4f61-9d55-1af26cf54e21\",\
             \"credential\":\"WXYZ23456789\",\"label\":\"SSH bootstrap\",\
             \"expiresAt\":\"2026-08-27T12:05:00Z\"}\n",
        ),
        Ok("WXYZ23456789".to_string())
    );
}

#[test]
fn public_discovery_and_prompt_resolution_cover_filesystem_and_error_contracts() {
    let _environment_manager = SshEnvironmentManager::new();
    let _default_environment_manager = SshEnvironmentManager::default();
    let _ = default_home_dir();

    let home = tempfile::tempdir().expect("temporary home should create");
    let ssh_dir = home.path().join(".ssh");
    let include_dir = ssh_dir.join("config dir");
    fs::create_dir_all(&include_dir).expect("SSH include directory should create");
    fs::write(
        ssh_dir.join("config"),
        "Host devbox wildcard*\n  HostName ignored.example\n\
         Include \"config dir/team.conf\"\n",
    )
    .expect("SSH config should write");
    fs::write(include_dir.join("team.conf"), "Host staging\n")
        .expect("included SSH config should write");
    fs::write(
        ssh_dir.join("known_hosts"),
        "known.example ssh-ed25519 AAAA\n[devbox]:2222 ssh-ed25519 BBBB\n",
    )
    .expect("known hosts should write");

    let hosts =
        discover_ssh_hosts(Some(home.path().to_path_buf())).expect("SSH hosts should discover");
    assert_eq!(
        hosts,
        vec![
            DiscoveredSshHost {
                alias: "devbox".to_string(),
                hostname: "devbox".to_string(),
                username: None,
                port: None,
                source: "ssh-config",
            },
            DiscoveredSshHost {
                alias: "known.example".to_string(),
                hostname: "known.example".to_string(),
                username: None,
                port: None,
                source: "known-hosts",
            },
            DiscoveredSshHost {
                alias: "staging".to_string(),
                hostname: "staging".to_string(),
                username: None,
                port: None,
                source: "ssh-config",
            },
        ]
    );
    assert!(hosts[0].to_value().is_object());
    assert!(
        discover_ssh_hosts(None)
            .expect("missing home should be empty")
            .is_empty()
    );

    let manager = SshPasswordPromptManager::with_timeout(Duration::from_millis(1));
    assert_eq!(
        manager.resolve(SshPasswordPromptResolution {
            request_id: " ".to_string(),
            password: None,
        }),
        Err(SshPasswordPromptResolveError::InvalidRequestId)
    );
    assert_eq!(
        manager.resolve(SshPasswordPromptResolution {
            request_id: "expired".to_string(),
            password: Some("secret".to_string()),
        }),
        Err(SshPasswordPromptResolveError::Expired {
            request_id: "expired".to_string(),
        })
    );
    let default_manager = SshPasswordPromptManager::new();
    assert_eq!(
        default_manager.resolve(SshPasswordPromptResolution {
            request_id: "missing".to_string(),
            password: None,
        }),
        Err(SshPasswordPromptResolveError::Expired {
            request_id: "missing".to_string(),
        })
    );
    let _default_prompt_manager = SshPasswordPromptManager::default();
    assert_eq!(
        SshPasswordPromptResolveError::InvalidRequestId.to_string(),
        "Invalid SSH password prompt id."
    );
    for error in [
        SshPasswordPromptRequestError::Presentation {
            request_id: "request".to_string(),
            destination: "host".to_string(),
            operation: "emit",
            message: "closed".to_string(),
        },
        SshPasswordPromptRequestError::TimedOut {
            request_id: "request".to_string(),
            destination: "host".to_string(),
        },
        SshPasswordPromptRequestError::Cancelled {
            request_id: "request".to_string(),
            destination: "host".to_string(),
        },
        SshPasswordPromptRequestError::ServiceStopped {
            request_id: "request".to_string(),
            destination: "host".to_string(),
        },
    ] {
        assert!(!error.to_string().is_empty());
    }
}
