#[cfg(target_os = "linux")]
#[path = "../../tests/support/appimage_environment.rs"]
pub(crate) mod appimage_environment;
#[cfg(target_os = "linux")]
mod capability_probe;
mod event;
#[path = "../../tests/support/executable_fixture.rs"]
pub(crate) mod executable_fixture;
#[cfg(target_os = "linux")]
#[path = "../../tests/support/fork_probe.rs"]
pub(crate) mod fork_probe;
#[path = "../../tests/support/hermetic_providers.rs"]
pub(crate) mod hermetic_providers;
#[path = "../../tests/support/isolated_git_config.rs"]
pub(crate) mod isolated_git_config;
#[path = "../../tests/support/reexec.rs"]
pub(crate) mod reexec;
mod sandbox;
#[path = "../../tests/support/websocket_frames.rs"]
pub(crate) mod websocket_frames;

#[cfg(target_os = "linux")]
pub(crate) use capability_probe::check_capability_probe_appimage_environment;
pub(crate) use event::{FixtureEvent, within_fixture_deadline};
pub(crate) use sandbox::{FixtureLease, TestSandbox};

#[cfg(feature = "hermetic-test-guard")]
pub(crate) fn reexec_without_provider_path(test_name: &str) -> bool {
    const CASE: &str = "fixture-without-provider-path";
    if TestSandbox::is_isolated_case(CASE, test_name) {
        return false;
    }
    let sandbox = TestSandbox::new("provider-free-path");
    let output =
        sandbox.run_isolated_case(CASE, test_name, &[("PATH", sandbox.root().as_os_str())]);
    assert!(
        output.status.success(),
        "isolated {test_name} failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

pub(crate) use reexec::in_own_process;

#[cfg(target_os = "linux")]
pub(crate) const ISOLATING_AND_NO_OP_CASES: &[&str] = &["mixed", "unset-appimage"];

#[cfg(target_os = "linux")]
pub(crate) fn run_on_current_thread<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime")
        .block_on(future)
}

#[cfg(test)]
mod tests {
    use super::{FixtureEvent, FixtureLease, TestSandbox, hermetic_providers};

    /// Model of `production::control`'s
    /// `scheduler_control_providers_never_fall_back_to_host_executables`, but
    /// at the shared helper level rather than one construction site: proves
    /// `hermetic_providers::BUILTIN_PROVIDER_DRIVERS` names exactly the
    /// drivers the server's own default settings define under `providers`
    /// without the helper overlay (disabled instances keep that read hermetic),
    /// and that every one of them, pinned by
    /// `hermetic_providers::write_hermetic_settings`, resolves to nothing
    /// installed rather than a real host executable. It runs once here
    /// rather than once per integration binary that includes
    /// `tests/support/hermetic_providers.rs` (see that file's module doc).
    ///
    /// A driver added to or removed from the server's defaults without a
    /// matching edit to `BUILTIN_PROVIDER_DRIVERS` fails the first assertion;
    /// a driver whose enabled default differs in either settings reader, or
    /// whose pinned path still resolves, fails the remaining assertions.
    #[tokio::test]
    async fn hermetic_helper_pins_every_provider_driver_the_server_defines_as_built_in() {
        #[cfg(feature = "hermetic-test-guard")]
        if super::reexec_without_provider_path(
            "test_support::tests::hermetic_helper_pins_every_provider_driver_the_server_defines_as_built_in",
        ) {
            return;
        }
        use crate::{ServerConfig, production::control::NativeServerControl};

        let temp = tempfile::tempdir().expect("state directory");
        let mut config = ServerConfig::new(temp.path());
        config.storage_instance_id = Some(crate::persistence::StorageInstanceId::from_uuid(
            uuid::Uuid::from_u128(0x00000000000040008000000000000006),
        ));
        let state_dir = config.state_dir();
        // Do not seed `providers` with the helper: doing so masks a removed
        // default driver. Disabled instances suppress probing without adding
        // any legacy provider keys to the settings being compared.
        let disabled_instances = hermetic_providers::BUILTIN_PROVIDER_DRIVERS
            .iter()
            .map(|driver| {
                (
                    (*driver).to_owned(),
                    serde_json::json!({
                        "driver": driver,
                        "enabled": false,
                    }),
                )
            })
            .collect::<serde_json::Map<String, serde_json::Value>>();
        std::fs::create_dir_all(&state_dir).expect("default settings directory");
        std::fs::write(
            state_dir.join("settings.json"),
            serde_json::to_vec(&serde_json::json!({
                "enableProviderUpdateChecks": false,
                "providerInstances": disabled_instances,
            }))
            .unwrap(),
        )
        .expect("settings without a legacy-provider overlay");
        let defaults_control =
            NativeServerControl::new(config.clone(), serde_json::json!({})).await;
        let defaults = defaults_control.config_snapshot().await;
        let defined_drivers = defaults["settings"]["providers"]
            .as_object()
            .expect("default settings always define a providers object")
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let pinned_drivers = hermetic_providers::BUILTIN_PROVIDER_DRIVERS
            .iter()
            .map(|driver| (*driver).to_owned())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            defined_drivers, pinned_drivers,
            "hermetic_providers::BUILTIN_PROVIDER_DRIVERS has drifted from the server's own \
             default `providers` keys; update it so a new or removed built-in driver stays pinned"
        );

        hermetic_providers::write_hermetic_settings(&state_dir, serde_json::json!({}));
        let runtime_settings = crate::server_settings::ProviderSettingsStore::new(&state_dir)
            .get()
            .await
            .expect("runtime reads hermetic settings");
        let missing = hermetic_providers::missing_provider_executable(&state_dir);
        for (driver, settings) in [
            ("codex", &runtime_settings.providers.codex),
            ("claudeAgent", &runtime_settings.providers.claude_agent),
            ("cursor", &runtime_settings.providers.cursor),
            ("grok", &runtime_settings.providers.grok),
            ("opencode", &runtime_settings.providers.opencode),
        ] {
            assert_eq!(
                settings.enabled,
                defaults["settings"]["providers"][driver]["enabled"]
                    .as_bool()
                    .expect("control enabled default"),
                "{driver}: runtime reader must preserve the control's persisted-document default"
            );
            assert_eq!(settings.binary_path, missing.to_str().unwrap(), "{driver}");
        }
        let helper_settings = hermetic_providers::hermetic_provider_settings(&state_dir);
        assert_eq!(
            helper_settings["providers"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>(),
            pinned_drivers,
            "the helper's default table must match BUILTIN_PROVIDER_DRIVERS"
        );
        for driver in &pinned_drivers {
            assert_eq!(
                helper_settings["providers"][driver]["enabled"],
                defaults["settings"]["providers"][driver]["enabled"],
                "{driver}: helper must explicitly preserve the control's persisted-document default"
            );
        }
        let control = NativeServerControl::new(config, serde_json::json!({})).await;
        let snapshot = control.config_snapshot().await;
        assert_eq!(snapshot["settings"]["enableProviderUpdateChecks"], false);

        let probed = snapshot["providers"]
            .as_array()
            .expect("provider inventory snapshot array");
        assert_eq!(probed.len(), defined_drivers.len());
        let inventory_drivers = probed
            .iter()
            .map(|provider| {
                provider["driver"]
                    .as_str()
                    .expect("provider driver")
                    .to_owned()
            })
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            inventory_drivers, pinned_drivers,
            "every inventory driver must be pinned"
        );
        for driver in &inventory_drivers {
            assert_eq!(
                snapshot["settings"]["providers"][driver]["binaryPath"],
                missing.to_str().unwrap()
            );
        }
        let still_resolves = probed
            .iter()
            .filter(|provider| provider["installed"] != false)
            .map(|provider| provider["driver"].clone())
            .collect::<Vec<_>>();
        assert!(
            still_resolves.is_empty(),
            "every built-in driver's pinned binaryPath must resolve to nothing installed, or it \
             can still reach a real host executable: {still_resolves:?}"
        );
    }

    #[test]
    fn ensure_hermetic_settings_merges_existing_overrides_and_is_idempotent() {
        let temp = tempfile::tempdir().expect("state directory");
        let state_dir = crate::ServerConfig::new(temp.path()).state_dir();
        let fixture = temp.path().join("fixture-codex");
        let overlay = serde_json::json!({
            "enableAssistantStreaming": true,
            "providers": { "codex": { "binaryPath": fixture, "enabled": false } },
            "providerInstances": { "cursor-work": { "driver": "cursor" } },
        });
        std::fs::create_dir_all(&state_dir).expect("state directory");
        let settings_path = state_dir.join("settings.json");
        std::fs::write(&settings_path, serde_json::to_vec(&overlay).unwrap())
            .expect("existing settings");

        assert_eq!(
            hermetic_providers::ensure_hermetic_settings(&state_dir),
            settings_path
        );
        let first = std::fs::read(&settings_path).expect("merged settings");
        let written: serde_json::Value = serde_json::from_slice(&first).unwrap();
        assert_eq!(written["enableProviderUpdateChecks"], false);
        assert_eq!(written["enableAssistantStreaming"], true);
        assert_eq!(written["providers"]["codex"], overlay["providers"]["codex"]);
        assert_eq!(written["providerInstances"], overlay["providerInstances"]);
        for driver in ["claudeAgent", "cursor", "grok", "opencode"] {
            assert_eq!(
                written["providers"][driver]["binaryPath"],
                state_dir
                    .join("missing-provider-executable")
                    .to_str()
                    .unwrap()
            );
        }
        hermetic_providers::ensure_hermetic_settings(&state_dir);
        assert_eq!(std::fs::read(&settings_path).unwrap(), first);
    }

    #[test]
    fn hermetic_settings_overlay_replaces_recursively_but_leaves_other_drivers_pinned() {
        let temp = tempfile::tempdir().expect("state directory");
        let state_dir = crate::ServerConfig::new(temp.path()).state_dir();
        let missing = hermetic_providers::missing_provider_executable(&state_dir);
        let overlay = serde_json::json!({
            "providers": { "codex": { "binaryPath": "custom-codex" } },
            "providerInstances": { "cursor-work": { "driver": "cursor" } },
        });
        let settings_path = hermetic_providers::write_hermetic_settings(&state_dir, overlay);
        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&settings_path).expect("read settings"))
                .expect("parse settings");

        assert_eq!(written["providers"]["codex"]["binaryPath"], "custom-codex");
        assert_eq!(
            written["providers"]["claudeAgent"]["binaryPath"],
            missing.to_str().expect("missing path is UTF-8"),
            "the overlay must not disturb a driver it does not mention"
        );
        assert_eq!(written["enableProviderUpdateChecks"], false);
        assert_eq!(
            written["providerInstances"]["cursor-work"]["driver"],
            "cursor"
        );
    }

    #[tokio::test]
    async fn sandboxes_and_events_are_parallel_and_resource_distinct() {
        let first = TestSandbox::new("first");
        let second = TestSandbox::new("second");
        assert_ne!(first.root(), second.root());
        assert_ne!(first.path("child.pid"), second.path("child.pid"));

        let event = FixtureEvent::default();
        let checkpoint = event.checkpoint();
        event.publish();
        event.wait_after(checkpoint).await;
    }

    #[tokio::test]
    async fn fixture_lease_counts_concurrent_resources_and_releases_on_drop() {
        let sandbox = TestSandbox::new("leases");
        let first = sandbox.acquire_fixture();
        let second = sandbox.acquire_fixture();
        assert_eq!(sandbox.active_fixtures(), 2);
        assert_eq!(sandbox.maximum_active_fixtures(), 2);
        drop(first);
        drop(second);
        assert_eq!(sandbox.active_fixtures(), 0);
    }

    #[test]
    fn fixture_lease_releases_during_panic_unwind() {
        let sandbox = TestSandbox::new("panic-release");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _lease = sandbox.acquire_fixture();
            panic!("fixture panic");
        }));
        assert!(result.is_err());
        assert_eq!(sandbox.active_fixtures(), 0);
    }

    #[test]
    fn sandbox_environment_applies_overrides_without_mutating_its_snapshot() {
        let sandbox = TestSandbox::new("environment");
        let mut first = sandbox.environment([("BIBCODE_FIXTURE_LABEL", "first")]);
        first.insert("BIBCODE_FIXTURE_LABEL".to_owned(), "changed".to_owned());
        let second = sandbox.environment([("BIBCODE_FIXTURE_LABEL", "second")]);

        assert_eq!(
            second.get("BIBCODE_FIXTURE_LABEL"),
            Some(&"second".to_owned())
        );
    }

    #[tokio::test]
    async fn sandbox_git_operations_ignore_hostile_discovery_and_config_environment() {
        use std::{collections::BTreeMap, ffi::OsStr, fs, path::Path, process::Command};

        fn snapshot(root: &Path) -> BTreeMap<std::path::PathBuf, Vec<u8>> {
            fn visit(
                root: &Path,
                directory: &Path,
                files: &mut BTreeMap<std::path::PathBuf, Vec<u8>>,
            ) {
                for entry in fs::read_dir(directory).expect("read disposable repository") {
                    let path = entry.expect("repository entry").path();
                    if path.is_dir() {
                        visit(root, &path, files);
                    } else {
                        files.insert(
                            path.strip_prefix(root).unwrap().to_path_buf(),
                            fs::read(path).expect("snapshot repository file"),
                        );
                    }
                }
            }
            let mut files = BTreeMap::new();
            visit(root, root, &mut files);
            files
        }

        const CASE: &str = "hostile-git-environment";
        const TEST: &str = "test_support::tests::sandbox_git_operations_ignore_hostile_discovery_and_config_environment";
        let sandbox = TestSandbox::new("git-environment");
        let git = sandbox.executable_on_path("git");
        if TestSandbox::is_isolated_case(CASE, TEST) {
            let empty_config = sandbox.path("empty.gitconfig");
            fs::write(&empty_config, "").expect("fixture Git config");
            let safe_git_environment = sandbox.environment([
                ("GIT_CONFIG_GLOBAL", empty_config.display().to_string()),
                ("GIT_CONFIG_SYSTEM", empty_config.display().to_string()),
                ("GIT_CONFIG_NOSYSTEM", "1".to_owned()),
            ]);
            let run_git = |args: &[&str]| {
                let mut input = sandbox.process_input(&git, args.iter().copied());
                input.env = Some(safe_git_environment.clone());
                crate::process::ProcessRunner.run(input)
            };
            let initialized = run_git(&["init", "-b", "main"])
                .await
                .expect("initialize scratch Git");
            assert_eq!(initialized.code, Some(0), "{initialized:?}");
            let directory = run_git(&["rev-parse", "--absolute-git-dir"])
                .await
                .expect("resolve scratch Git directory");
            assert_eq!(directory.code, Some(0), "{directory:?}");
            assert_eq!(
                fs::canonicalize(directory.stdout.trim()).expect("observed Git directory"),
                fs::canonicalize(sandbox.path(".git")).expect("scratch Git directory"),
                "ambient discovery must never select the protected bare repository"
            );
            assert!(
                sandbox
                    .environment(std::iter::empty::<(String, String)>())
                    .keys()
                    .all(|name| {
                        !name
                            .as_bytes()
                            .get(..4)
                            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"GIT_"))
                    }),
                "the base snapshot must exclude every inherited Git variable"
            );
            let ambient = run_git(&["config", "--get", "fixture.ambient"])
                .await
                .expect("read absent ambient fixture config");
            assert_eq!(ambient.code, Some(1), "{ambient:?}");

            let config = super::isolated_git_config::IsolatedGitConfig::new();
            let mut command = Command::new(&git);
            config.apply_to_command(&mut command);
            let directory = command
                .current_dir(sandbox.root())
                .args(["rev-parse", "--absolute-git-dir"])
                .output()
                .expect("Git with command-owned configuration");
            assert!(directory.status.success(), "{directory:?}");
            assert_eq!(
                fs::canonicalize(String::from_utf8(directory.stdout).unwrap().trim()).unwrap(),
                fs::canonicalize(sandbox.path(".git")).unwrap(),
                "the shared command helper must also remove ambient discovery"
            );
            for (key, expected_code, expected_value) in
                [("fixture.ambient", 1, ""), ("commit.gpgSign", 0, "false")]
            {
                let mut command = Command::new(&git);
                config.apply_to_command(&mut command);
                let result = command
                    .current_dir(sandbox.root())
                    .args(["config", "--get", key])
                    .output()
                    .expect("read command-owned Git configuration");
                assert_eq!(result.status.code(), Some(expected_code), "{result:?}");
                assert_eq!(
                    String::from_utf8(result.stdout).unwrap().trim(),
                    expected_value
                );
            }

            fs::write(
                sandbox.path("fixture.txt"),
                "owned by the scratch repository\n",
            )
            .expect("scratch file");
            let mut environment = safe_git_environment;
            environment.extend([
                ("GIT_AUTHOR_NAME".to_owned(), "Fixture".to_owned()),
                (
                    "GIT_AUTHOR_EMAIL".to_owned(),
                    "fixture@example.invalid".to_owned(),
                ),
                ("GIT_COMMITTER_NAME".to_owned(), "Fixture".to_owned()),
                (
                    "GIT_COMMITTER_EMAIL".to_owned(),
                    "fixture@example.invalid".to_owned(),
                ),
                ("GIT_CONFIG_COUNT".to_owned(), "1".to_owned()),
                ("GIT_CONFIG_KEY_0".to_owned(), "fixture.explicit".to_owned()),
                ("GIT_CONFIG_VALUE_0".to_owned(), "requested".to_owned()),
            ]);
            for args in [
                vec!["add", "fixture.txt"],
                vec![
                    "-c",
                    "commit.gpgSign=false",
                    "commit",
                    "-m",
                    "scratch commit",
                ],
                vec!["config", "--get", "fixture.explicit"],
            ] {
                let mut input = sandbox.process_input(&git, args);
                input.env = Some(environment.clone());
                let result = crate::process::ProcessRunner
                    .run(input)
                    .await
                    .expect("scratch Git operation");
                assert_eq!(result.code, Some(0), "{result:?}");
                if result.stdout.trim() == "requested" {
                    return;
                }
            }
            panic!("explicit Git fixture configuration was not preserved");
        }

        let bare = sandbox.path("protected.git");
        let home = sandbox.path("home");
        let work_tree = sandbox.path("protected-work-tree");
        fs::create_dir(&home).expect("isolated home");
        fs::create_dir(&work_tree).expect("protected work tree");
        let config = sandbox.path("hostile.gitconfig");
        fs::write(&config, "[fixture]\n\tambient = hostile\n").expect("hostile config");
        let index = sandbox.path("protected.index");
        fs::write(&index, "must not be overwritten\n").expect("protected index");
        let initialized = Command::new(git)
            .env_clear()
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(["init", "--bare", "-b", "protected"])
            .arg(&bare)
            .output()
            .expect("initialize disposable bare repository");
        assert!(initialized.status.success(), "{initialized:?}");
        let before = snapshot(sandbox.root());
        let objects = bare.join("objects");
        let environment = [
            ("HOME", home.as_os_str()),
            ("USERPROFILE", home.as_os_str()),
            ("GIT_DIR", bare.as_os_str()),
            ("GIT_COMMON_DIR", bare.as_os_str()),
            ("GIT_WORK_TREE", work_tree.as_os_str()),
            ("GIT_INDEX_FILE", index.as_os_str()),
            ("GIT_OBJECT_DIRECTORY", objects.as_os_str()),
            ("GIT_ALTERNATE_OBJECT_DIRECTORIES", objects.as_os_str()),
            ("GIT_CEILING_DIRECTORIES", sandbox.root().as_os_str()),
            ("GIT_CONFIG_GLOBAL", config.as_os_str()),
            ("GIT_CONFIG_SYSTEM", config.as_os_str()),
            ("GIT_CONFIG", config.as_os_str()),
            ("GIT_CONFIG_NOSYSTEM", OsStr::new("0")),
            ("GIT_CONFIG_COUNT", OsStr::new("1")),
            ("GIT_CONFIG_KEY_0", OsStr::new("fixture.ambient")),
            ("GIT_CONFIG_VALUE_0", OsStr::new("hostile")),
            ("gIt_DISCOVERY_SENTINEL", OsStr::new("mixed-case prefix")),
        ];
        let result = sandbox.run_isolated_case(CASE, TEST, &environment);
        assert!(
            snapshot(sandbox.root()) == before,
            "protected repository, config and index changed"
        );
        assert!(
            result.status.success(),
            "hostile Git environment child failed:\n{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }

    #[test]
    fn sandbox_resolves_executables_from_its_captured_path() {
        let sandbox = TestSandbox::new("captured-executable");
        let executable = sandbox.executable_on_path("git");

        assert!(executable.is_absolute());
        assert!(executable.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn sandbox_writes_an_explicit_owner_executable_unix_script() {
        use std::os::unix::fs::PermissionsExt;

        let sandbox = TestSandbox::new("script");
        let script = sandbox.executable_script("fixture", "printf fixture", "@echo off");

        assert_eq!(script, sandbox.path("fixture.sh"));
        assert_eq!(
            std::fs::read_to_string(&script).expect("read fixture script"),
            "#!/bin/sh\nprintf fixture\n"
        );
        assert_eq!(
            std::fs::metadata(&script)
                .expect("fixture script metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[cfg(windows)]
    #[test]
    fn sandbox_writes_an_explicit_windows_command_script() {
        let sandbox = TestSandbox::new("script");
        let script = sandbox.executable_script("fixture", "printf fixture", "@echo off");

        assert_eq!(script, sandbox.path("fixture.cmd"));
        assert_eq!(
            std::fs::read_to_string(&script).expect("read fixture script"),
            "@echo off\r\n"
        );
    }

    #[test]
    fn fixture_lease_has_a_crate_private_name() {
        let sandbox = TestSandbox::new("lease-name");
        let _lease: FixtureLease = sandbox.acquire_fixture();
    }

    /// Runs once here rather than in every integration binary that includes
    /// `tests/support/websocket_frames.rs`.
    #[tokio::test]
    async fn next_frame_past_heartbeat_skips_only_ping_and_pong() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::{
            WebSocketStream,
            tungstenite::{
                Error, Message,
                error::ProtocolError,
                protocol::{CloseFrame, Role, frame::coding::CloseCode},
            },
        };

        use super::websocket_frames::next_frame_past_heartbeat;

        // Ping and Pong are skipped, the data frames behind them arrive
        // unchanged, and skipping the Ping sent tungstenite's queued Pong.
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let mut client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let mut server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        for frame in [
            Message::Ping("heartbeat".into()),
            Message::Pong("unsolicited".into()),
            Message::Text("data".into()),
            Message::Binary(vec![1, 2, 3].into()),
        ] {
            server.send(frame).await.expect("peer sends a frame");
        }
        assert_eq!(
            next_frame_past_heartbeat(&mut client)
                .await
                .expect("a frame")
                .expect("a valid frame"),
            Message::Text("data".into())
        );
        assert_eq!(
            next_frame_past_heartbeat(&mut client)
                .await
                .expect("a frame")
                .expect("a valid frame"),
            Message::Binary(vec![1, 2, 3].into())
        );
        assert_eq!(
            server
                .next()
                .await
                .expect("the client's reply")
                .expect("a valid reply"),
            Message::Pong("heartbeat".into())
        );

        // A Close frame passes through, then the end of the stream once the
        // closing handshake completes.
        let close = CloseFrame {
            code: CloseCode::Away,
            reason: "done".into(),
        };
        server
            .send(Message::Close(Some(close.clone())))
            .await
            .expect("peer closes");
        assert_eq!(
            next_frame_past_heartbeat(&mut client)
                .await
                .expect("a frame")
                .expect("a valid frame"),
            Message::Close(Some(close))
        );
        let (end, _) = tokio::join!(next_frame_past_heartbeat(&mut client), async move {
            // The peer reads the client's close reply, then drops the socket.
            let reply = server.next().await;
            drop(server);
            reply
        });
        assert!(
            end.is_none(),
            "the end of the stream passes through: {end:?}"
        );

        // An error passes through: the peer vanishes without a closing handshake.
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let mut client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        drop(server_io);
        let reset = next_frame_past_heartbeat(&mut client).await;
        assert!(
            matches!(
                reset,
                Some(Err(Error::Protocol(
                    ProtocolError::ResetWithoutClosingHandshake
                )))
            ),
            "{reset:?}"
        );
    }
}
