#[allow(dead_code)]
// Some closed source variants remain available for later provider classifications.
mod latest;
mod source;

pub(crate) use source::{ProviderMaintenanceCapabilities, ProviderUpdateCommand};

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        path::Path,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use axum::{Json, Router, extract::State, http::StatusCode, routing::get};
    use serde_json::{Value, json};
    use tokio::net::TcpListener;
    use tokio_util::sync::CancellationToken;
    use url::Url;

    use super::latest::{ClaudeReleaseChannel, LatestVersionSource};
    use super::*;
    use crate::test_support::TestSandbox;

    #[test]
    fn resolves_cross_platform_installation_sources() {
        let cases = [
            (
                "codex",
                "codex",
                Some("C:/Users/me/AppData/Roaming/npm/codex.cmd"),
                None,
                Some("npm install -g @openai/codex@latest"),
            ),
            (
                "codex",
                "codex",
                Some("C:/Users/me/.bun/bin/codex.exe"),
                None,
                Some("bun i -g @openai/codex@latest"),
            ),
            (
                "claudeAgent",
                "claude",
                Some("/Users/me/.local/bin/claude"),
                None,
                Some("/Users/me/.local/bin/claude update"),
            ),
            (
                "claudeAgent",
                "claude",
                Some("/opt/homebrew/bin/claude"),
                Some("/opt/homebrew/Caskroom/claude-code/2.1.0/claude"),
                Some("brew upgrade --cask claude-code"),
            ),
            (
                "opencode",
                "opencode",
                Some("/home/me/.opencode/bin/opencode"),
                None,
                Some("/home/me/.opencode/bin/opencode upgrade"),
            ),
            (
                "opencode",
                "opencode",
                Some("C:/Users/me/AppData/Roaming/npm/opencode.cmd"),
                None,
                Some("npm install -g opencode-ai@latest"),
            ),
            (
                "opencode",
                "opencode",
                Some("/usr/local/bin/opencode"),
                Some("/usr/local/lib/node_modules/@opencode/cli/bin/opencode.exe"),
                Some("npm install -g @opencode/cli@latest"),
            ),
            (
                "opencode",
                "opencode",
                Some("C:/Users/me/AppData/Roaming/npm/opencode.cmd"),
                Some("C:/Users/me/AppData/Roaming/npm/node_modules/@opencode/cli/bin/opencode.exe"),
                Some("npm install -g @opencode/cli@latest"),
            ),
            (
                "opencode",
                "opencode2",
                Some("C:/Users/me/AppData/Roaming/npm/opencode2.cmd"),
                Some("C:/Users/me/AppData/Roaming/npm/node_modules/@opencode/cli/bin/opencode.exe"),
                Some("npm install -g @opencode/cli@latest"),
            ),
            (
                "opencode",
                "opencode",
                Some("/home/linuxbrew/.linuxbrew/bin/opencode"),
                Some("/home/linuxbrew/.linuxbrew/Cellar/opencode/1.0.0/bin/opencode"),
                Some("brew upgrade anomalyco/tap/opencode"),
            ),
            (
                "codex",
                "codex",
                Some("/home/me/.local/share/pnpm/codex"),
                None,
                Some("pnpm add -g @openai/codex@latest"),
            ),
            (
                "codex",
                "codex",
                Some("C:/Users/me/AppData/Roaming/pnpm/codex.cmd"),
                None,
                Some("pnpm add -g @openai/codex@latest"),
            ),
            (
                "codex",
                "codex",
                Some("/home/me/.vite-plus/bin/codex"),
                None,
                Some("vp i -g @openai/codex"),
            ),
            (
                "codex",
                "codex",
                Some("/usr/local/bin/codex"),
                Some("/usr/local/lib/node_modules/@openai/codex/bin/codex.js"),
                Some("npm install -g @openai/codex@latest"),
            ),
            (
                "codex",
                "codex",
                Some("/srv/project/node_modules/.bin/codex"),
                None,
                None,
            ),
            (
                "codex",
                "codex",
                Some("C:/npm/node_modules/@openai/codex/bin/codex.js"),
                None,
                Some("npm install -g @openai/codex@latest"),
            ),
            (
                "codex",
                "codex",
                Some("/usr/local/bin/codex"),
                Some("/Users/me/Library/pnpm/global/5/node_modules/@openai/codex/bin/codex.js"),
                Some("pnpm add -g @openai/codex@latest"),
            ),
            (
                "codex",
                "codex",
                Some("/usr/bin/codex"),
                Some("/home/me/.local/share/pnpm/global/5/node_modules/@openai/codex/bin/codex.js"),
                Some("pnpm add -g @openai/codex@latest"),
            ),
            (
                "codex",
                "codex",
                Some("/home/me/.local/bin/codex"),
                Some("/home/me/.codex/packages/standalone/current/bin/codex"),
                Some("/home/me/.local/bin/codex update"),
            ),
            (
                "codex",
                "codex",
                Some("C:/Users/me/AppData/Local/Programs/OpenAI/Codex/bin/codex.exe"),
                None,
                Some("C:/Users/me/AppData/Local/Programs/OpenAI/Codex/bin/codex.exe update"),
            ),
            (
                "codex",
                "codex",
                Some("/opt/homebrew/bin/codex"),
                Some("/opt/homebrew/Caskroom/codex/0.148.0/codex"),
                Some("brew upgrade --cask codex"),
            ),
            ("codex", "codex", Some("/srv/tools/codex"), None, None),
        ];
        for (driver, binary, resolved, canonical, expected) in cases {
            let capabilities = capabilities_for_paths(
                driver,
                binary,
                resolved.map(Path::new),
                canonical.map(Path::new),
            );
            assert_eq!(
                capabilities
                    .update
                    .as_ref()
                    .map(|value| value.display.as_str()),
                expected
            );
        }
    }

    #[test]
    fn canonical_homebrew_evidence_outranks_generic_package_manager_markers() {
        let capabilities = capabilities_for_paths(
            "codex",
            "codex",
            Some(Path::new("/opt/homebrew/bin/codex")),
            Some(Path::new(
                "/opt/homebrew/Caskroom/codex/0.148.0/node_modules/.bin/codex",
            )),
        );

        assert_eq!(
            capabilities
                .update
                .as_ref()
                .map(|value| value.display.as_str()),
            Some("brew upgrade --cask codex")
        );
    }

    #[test]
    fn rejects_inexact_or_conflicting_installation_ownership_evidence() {
        let cases = [
            (
                "project-local npm shim",
                "/srv/project/node_modules/.bin/codex",
                None,
            ),
            (
                "unrelated Homebrew formula",
                "/opt/homebrew/bin/codex",
                Some("/opt/homebrew/Cellar/unrelated/1.0.0/bin/codex"),
            ),
            (
                "wrong Homebrew basename",
                "/opt/homebrew/bin/not-codex",
                Some("/opt/homebrew/Caskroom/codex/0.148.0/not-codex"),
            ),
            (
                "wrong canonical npm package",
                "/usr/local/bin/codex",
                Some("/usr/local/lib/node_modules/not-codex/bin/codex.js"),
            ),
            (
                "nested Vite+ lookalike",
                "/home/me/.vite-plus/bin/nested/codex",
                None,
            ),
            (
                "nested pnpm lookalike",
                "/home/me/.local/share/pnpm/nested/codex",
                None,
            ),
            (
                "conflicting npm and Homebrew evidence",
                "C:/Users/me/AppData/Roaming/npm/codex.cmd",
                Some("/opt/homebrew/Caskroom/codex/0.148.0/codex"),
            ),
        ];

        for (label, resolved, canonical) in cases {
            let capabilities = capabilities_for_paths(
                "codex",
                "codex",
                Some(Path::new(resolved)),
                canonical.map(Path::new),
            );
            assert_eq!(
                capabilities,
                ProviderMaintenanceCapabilities::unknown(),
                "{label}"
            );
        }
    }

    #[test]
    fn native_installers_require_exact_paths() {
        let cases = [
            ("claudeAgent", "/srv/.local/bin/claude-wrapper", None),
            ("opencode", "/srv/.opencode/bin/opencode-backup", None),
            (
                "claudeAgent",
                "C:/Users/me/AppData/Local/Microsoft/WinGet/Links/nested/claude.exe",
                None,
            ),
            (
                "claudeAgent",
                "C:/Users/me/AppData/Local/Microsoft/WinGet/Packages/Anthropic.ClaudeCode_Microsoft.Winget.Source_8wekyb3d8bbwe/nested/claude.exe",
                None,
            ),
            (
                "claudeAgent",
                "C:/Users/me/.local/bin/claude.exe",
                Some("C:/Users/me/.local/bin/claude.exe update"),
            ),
            (
                "opencode",
                "C:/Users/me/.opencode/bin/opencode.exe",
                Some("C:/Users/me/.opencode/bin/opencode.exe upgrade"),
            ),
        ];
        for (driver, binary, expected) in cases {
            let capabilities =
                capabilities_for_paths(driver, binary, Some(Path::new(binary)), None);
            assert_eq!(
                capabilities
                    .update
                    .as_ref()
                    .map(|value| value.display.as_str()),
                expected
            );
        }
    }

    #[test]
    fn rejects_arbitrary_lookalike_manager_and_native_roots() {
        let cases = [
            (
                "npm lib lookalike",
                "codex",
                "/usr/local/bin/codex",
                Some("/srv/lookalike/lib/node_modules/@openai/codex/bin/codex.js"),
            ),
            (
                "unordered pnpm global lookalike",
                "codex",
                "/usr/bin/codex",
                Some("/srv/global/cache/pnpm/node_modules/@openai/codex/bin/codex.js"),
            ),
            (
                "pnpm shim lookalike",
                "codex",
                "/srv/lookalike/.local/share/pnpm/codex",
                None,
            ),
            (
                "Vite+ shim lookalike",
                "codex",
                "/srv/lookalike/.vite-plus/bin/codex",
                None,
            ),
            (
                "Bun shim lookalike",
                "codex",
                "/srv/lookalike/.bun/bin/codex",
                None,
            ),
            (
                "Bun global package lookalike",
                "codex",
                "/usr/bin/codex",
                Some("/srv/lookalike/.bun/install/global/node_modules/@openai/codex/bin/codex.js"),
            ),
            (
                "Homebrew cask lookalike",
                "codex",
                "/usr/bin/codex",
                Some("/srv/lookalike/Caskroom/codex/1.0.0/codex"),
            ),
            (
                "Claude native lookalike",
                "claudeAgent",
                "/srv/lookalike/.local/bin/claude",
                None,
            ),
            (
                "OpenCode native lookalike",
                "opencode",
                "/srv/lookalike/.opencode/bin/opencode",
                None,
            ),
            (
                "Codex standalone lookalike",
                "codex",
                "/srv/lookalike/.codex/packages/standalone/current/bin/codex",
                None,
            ),
            (
                "WinGet Links lookalike",
                "claudeAgent",
                "C:/scratch/Microsoft/WinGet/Links/claude.exe",
                None,
            ),
            (
                "WinGet Packages lookalike",
                "claudeAgent",
                "C:/scratch/Microsoft/WinGet/Packages/Anthropic.ClaudeCode_Microsoft.Winget.Source_8wekyb3d8bbwe/claude.exe",
                None,
            ),
        ];

        for (label, driver, resolved, canonical) in cases {
            let binary = match driver {
                "claudeAgent" => "claude",
                value => value,
            };
            assert_eq!(
                capabilities_for_paths(
                    driver,
                    binary,
                    Some(Path::new(resolved)),
                    canonical.map(Path::new),
                ),
                ProviderMaintenanceCapabilities::unknown(),
                "{label}"
            );
        }
    }

    #[test]
    fn unknown_explicit_path_is_manual_only() {
        let capabilities = capabilities_for_paths(
            "codex",
            "/srv/custom/codex",
            Some(Path::new("/srv/custom/codex")),
            None,
        );
        assert!(capabilities.update.is_none());
    }

    #[test]
    fn compares_release_and_prerelease_versions() {
        assert_eq!(
            advisory_status(Some("2.1.110"), Some("2.1.111")),
            "behind_latest"
        );
        assert_eq!(advisory_status(Some("2.1.111"), Some("2.1.111")), "current");
        assert_eq!(
            advisory_status(Some("2.1.111-beta.1"), Some("2.1.111")),
            "behind_latest"
        );
        assert_eq!(
            advisory_status(Some("Claude Code 2.1.111"), Some("2.1.111")),
            "current"
        );
        assert_eq!(advisory_status(None, Some("2.1.111")), "unknown");
    }

    fn target(driver: &str, binary_path: &str) -> ProviderMaintenanceTarget {
        ProviderMaintenanceTarget {
            instance_id: driver.to_owned(),
            driver: driver.to_owned(),
            binary_path: binary_path.to_owned(),
            environment: Vec::new(),
        }
    }

    fn sandbox_target(
        sandbox: &TestSandbox,
        driver: &str,
        binary_path: &str,
    ) -> ProviderMaintenanceTarget {
        let mut target = target(driver, binary_path);
        target.environment = sandbox
            .environment(std::iter::empty::<(String, String)>())
            .into_iter()
            .map(|(name, value)| (OsString::from(name), OsString::from(value)))
            .collect();
        target
    }

    fn global_npm_target(directory: &Path, driver: &str) -> ProviderMaintenanceTarget {
        let binary_name = match driver {
            "claudeAgent" => "claude",
            value => value,
        };
        let package_bin = directory.join(".npm-global/bin");
        std::fs::create_dir_all(&package_bin).expect("global npm bin");
        let binary = package_bin.join(if cfg!(windows) {
            format!("{binary_name}.cmd")
        } else {
            binary_name.to_owned()
        });
        std::fs::write(&binary, b"fixture").expect("global npm executable");
        target(driver, &binary.to_string_lossy())
    }

    fn capabilities_for_paths(
        driver: &str,
        binary_path: &str,
        resolved: Option<&Path>,
        canonical: Option<&Path>,
    ) -> ProviderMaintenanceCapabilities {
        super::source::capabilities_for_paths(&target(driver, binary_path), resolved, canonical)
    }

    fn installed_snapshot(driver: &str, version: &str) -> Value {
        json!({ "instanceId": driver, "driver": driver, "enabled": true, "installed": true, "version": version, "checkedAt": "2026-08-01T12:00:00Z" })
    }

    #[derive(Clone)]
    struct LatestVersionEndpointFixture {
        npm_body: Vec<u8>,
        npm_requests: Arc<AtomicUsize>,
        claude_stable_requests: Arc<AtomicUsize>,
        claude_latest_requests: Arc<AtomicUsize>,
        cursor_installer_requests: Arc<AtomicUsize>,
    }

    async fn latest_version_endpoint_fixture(
        npm_body: Vec<u8>,
    ) -> (LatestVersionEndpoints, LatestVersionEndpointFixture) {
        let fixture = LatestVersionEndpointFixture {
            npm_body,
            npm_requests: Arc::new(AtomicUsize::new(0)),
            claude_stable_requests: Arc::new(AtomicUsize::new(0)),
            claude_latest_requests: Arc::new(AtomicUsize::new(0)),
            cursor_installer_requests: Arc::new(AtomicUsize::new(0)),
        };
        let app = Router::new()
            .route(
                "/claude/stable",
                get(|State(fixture): State<LatestVersionEndpointFixture>| async move {
                    fixture.claude_stable_requests.fetch_add(1, Ordering::SeqCst);
                    "2.1.220\n"
                }),
            )
            .route(
                "/claude/latest",
                get(|State(fixture): State<LatestVersionEndpointFixture>| async move {
                    fixture.claude_latest_requests.fetch_add(1, Ordering::SeqCst);
                    "2.1.224\n"
                }),
            )
            .route(
                "/cursor/install",
                get(|State(fixture): State<LatestVersionEndpointFixture>| async move {
                    fixture
                        .cursor_installer_requests
                        .fetch_add(1, Ordering::SeqCst);
                    "DOWNLOAD_URL=\"https://downloads.cursor.com/lab/2026.08.04-aaa8809/${OS}/${ARCH}/agent-cli-package.tar.gz\"\nFINAL_DIR=\"$HOME/.local/share/cursor-agent/versions/2026.08.04-aaa8809\""
                }),
            )
            .route(
                "/{*path}",
                get(|State(fixture): State<LatestVersionEndpointFixture>| async move {
                    fixture.npm_requests.fetch_add(1, Ordering::SeqCst);
                    fixture.npm_body
                }),
            )
            .with_state(fixture.clone());
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("latest-version listener");
        let address = listener.local_addr().expect("latest-version address");
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("latest-version server")
        });
        let base_url = Url::parse(&format!("http://{address}/")).expect("latest-version URL");
        (
            LatestVersionEndpoints {
                npm_registry_base_url: base_url.clone(),
                claude_release_base_url: base_url.join("claude/").expect("Claude URL"),
                cursor_installer_url: base_url.join("cursor/install").expect("Cursor URL"),
            },
            fixture,
        )
    }

    #[tokio::test]
    async fn caches_claude_stable_and_latest_as_distinct_sources() {
        let (endpoints, fixture) =
            latest_version_endpoint_fixture(br#"{"version":"0.148.0"}"#.to_vec()).await;
        let maintenance = ProviderMaintenance::with_version_endpoints(endpoints);
        let stable = LatestVersionSource::Claude(ClaudeReleaseChannel::Stable);
        let latest = LatestVersionSource::Claude(ClaudeReleaseChannel::Latest);

        assert_eq!(
            maintenance.latest_version(stable, None).await,
            LatestVersionCheck::Success {
                version: "2.1.220".to_owned(),
                checked_at: None,
            }
        );
        assert_eq!(
            maintenance.latest_version(latest, None).await,
            LatestVersionCheck::Success {
                version: "2.1.224".to_owned(),
                checked_at: None,
            }
        );
        assert_eq!(
            maintenance.latest_version(stable, None).await,
            LatestVersionCheck::Success {
                version: "2.1.220".to_owned(),
                checked_at: None,
            }
        );
        assert_eq!(
            maintenance.latest_version(latest, None).await,
            LatestVersionCheck::Success {
                version: "2.1.224".to_owned(),
                checked_at: None,
            }
        );

        assert_eq!(fixture.claude_stable_requests.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.claude_latest_requests.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn fetches_and_parses_cursor_installer_metadata() {
        let (endpoints, fixture) =
            latest_version_endpoint_fixture(br#"{"version":"0.148.0"}"#.to_vec()).await;
        let maintenance = ProviderMaintenance::with_version_endpoints(endpoints);

        assert_eq!(
            maintenance
                .latest_version(LatestVersionSource::CursorInstaller, None)
                .await,
            LatestVersionCheck::Success {
                version: "2026.08.04-aaa8809".to_owned(),
                checked_at: None,
            }
        );
        assert_eq!(fixture.cursor_installer_requests.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn rejects_latest_version_responses_larger_than_256_kib() {
        let (endpoints, fixture) =
            latest_version_endpoint_fixture(vec![b'x'; 256 * 1024 + 1]).await;
        let maintenance = ProviderMaintenance::with_version_endpoints(endpoints);
        let source = LatestVersionSource::Npm("@openai/codex");

        assert_eq!(
            maintenance.latest_version(source, None).await,
            LatestVersionCheck::Failed { checked_at: None }
        );
        assert_eq!(fixture.npm_requests.load(Ordering::SeqCst), 1);
        assert!(
            !maintenance
                .inner
                .latest_versions
                .lock()
                .await
                .contains_key(&source)
        );
    }

    async fn npm_registry_fixture(version: &str) -> (Url, Arc<AtomicUsize>) {
        let requests = Arc::new(AtomicUsize::new(0));
        let state = (version.to_owned(), requests.clone());
        let app = Router::new()
            .route(
                "/{*path}",
                get(
                    |State((version, requests)): State<(String, Arc<AtomicUsize>)>| async move {
                        requests.fetch_add(1, Ordering::SeqCst);
                        Json(json!({ "version": version }))
                    },
                ),
            )
            .with_state(state);
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("registry listener");
        let address = listener.local_addr().expect("registry address");
        tokio::spawn(async move { axum::serve(listener, app).await.expect("registry server") });
        (
            Url::parse(&format!("http://{address}/")).expect("registry URL"),
            requests,
        )
    }

    async fn mutable_npm_registry_fixture(
        version: &str,
    ) -> (Url, Arc<tokio::sync::RwLock<String>>, Arc<AtomicUsize>) {
        let version = Arc::new(tokio::sync::RwLock::new(version.to_owned()));
        let requests = Arc::new(AtomicUsize::new(0));
        let state = (version.clone(), requests.clone());
        let app = Router::new()
            .route(
                "/opencode-ai/latest",
                get(
                    |State((version, requests)): State<(
                        Arc<tokio::sync::RwLock<String>>,
                        Arc<AtomicUsize>,
                    )>| async move {
                        requests.fetch_add(1, Ordering::SeqCst);
                        Json(json!({ "version": version.read().await.clone() }))
                    },
                ),
            )
            .with_state(state);
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("registry listener");
        let address = listener.local_addr().expect("registry address");
        tokio::spawn(async move { axum::serve(listener, app).await.expect("registry server") });
        (
            Url::parse(&format!("http://{address}/")).expect("registry URL"),
            version,
            requests,
        )
    }

    async fn recovering_npm_registry_fixture(
        version: &str,
    ) -> (Url, Arc<AtomicBool>, Arc<AtomicUsize>) {
        let failing = Arc::new(AtomicBool::new(true));
        let requests = Arc::new(AtomicUsize::new(0));
        let state = (version.to_owned(), failing.clone(), requests.clone());
        let app = Router::new()
            .route(
                "/opencode-ai/latest",
                get(
                    |State((version, failing, requests)): State<(
                        String,
                        Arc<AtomicBool>,
                        Arc<AtomicUsize>,
                    )>| async move {
                        requests.fetch_add(1, Ordering::SeqCst);
                        if failing.load(Ordering::SeqCst) {
                            (StatusCode::SERVICE_UNAVAILABLE, Json(json!({})))
                        } else {
                            (StatusCode::OK, Json(json!({ "version": version })))
                        }
                    },
                ),
            )
            .with_state(state);
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("registry listener");
        let address = listener.local_addr().expect("registry address");
        tokio::spawn(async move { axum::serve(listener, app).await.expect("registry server") });
        (
            Url::parse(&format!("http://{address}/")).expect("registry URL"),
            failing,
            requests,
        )
    }

    async fn delayed_npm_registry_fixture(version: &str) -> (Url, Arc<AtomicUsize>) {
        let concurrent_requests = Arc::new(AtomicUsize::new(0));
        let active_requests = Arc::new(AtomicUsize::new(0));
        let state = (
            version.to_owned(),
            concurrent_requests.clone(),
            active_requests,
        );
        let app = Router::new()
            .route(
                "/{*path}",
                get(
                    |State((version, concurrent_requests, active_requests)): State<(
                        String,
                        Arc<AtomicUsize>,
                        Arc<AtomicUsize>,
                    )>| async move {
                        let active = active_requests.fetch_add(1, Ordering::SeqCst) + 1;
                        concurrent_requests.fetch_max(active, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        active_requests.fetch_sub(1, Ordering::SeqCst);
                        Json(json!({ "version": version }))
                    },
                ),
            )
            .with_state(state);
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("registry listener");
        let address = listener.local_addr().expect("registry address");
        tokio::spawn(async move { axum::serve(listener, app).await.expect("registry server") });
        (
            Url::parse(&format!("http://{address}/")).expect("registry URL"),
            concurrent_requests,
        )
    }

    #[tokio::test]
    async fn enriches_snapshot_and_caches_npm_latest_for_one_hour() {
        let (registry_url, requests) = npm_registry_fixture("9.9.9").await;
        let maintenance = ProviderMaintenance::with_registry_base_url(registry_url);
        let directory = tempfile::tempdir().expect("global npm directory");
        let target = global_npm_target(directory.path(), "codex");
        let mut first = installed_snapshot("codex", "1.0.0");
        let mut second = installed_snapshot("codex", "1.0.0");
        second["checkedAt"] = json!("2026-08-01T12:05:00Z");
        maintenance.enrich_snapshot(&target, &mut first, true).await;
        maintenance
            .enrich_snapshot(&target, &mut second, true)
            .await;
        assert_eq!(first["versionAdvisory"]["status"], "behind_latest");
        assert_eq!(first["versionAdvisory"]["latestVersion"], "9.9.9");
        assert_eq!(
            second["versionAdvisory"]["checkedAt"],
            "2026-08-01T12:00:00Z"
        );
        assert_eq!(requests.load(Ordering::SeqCst), 1);
    }

    /// OpenCode 1 is published as `opencode-ai` and OpenCode 2 as `@opencode/cli`, so each major
    /// compares against its own line even where the installation path cannot tell them apart.
    #[tokio::test]
    async fn opencode_versions_compare_against_the_package_of_their_major() {
        let app = Router::new().route(
            "/{*path}",
            get(|uri: axum::extract::OriginalUri| async move {
                let version = if uri.0.path().contains("opencode%2Fcli") {
                    "2.0.30"
                } else {
                    "1.19.2"
                };
                Json(json!({ "version": version }))
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("registry listener");
        let address = listener.local_addr().expect("registry address");
        tokio::spawn(async move { axum::serve(listener, app).await.expect("registry server") });
        let maintenance = ProviderMaintenance::with_registry_base_url(
            Url::parse(&format!("http://{address}/")).expect("registry URL"),
        );
        // A global npm shim whose package the path does not name.
        let directory = tempfile::tempdir().expect("global npm directory");
        let target = global_npm_target(directory.path(), "opencode");
        for (installed, latest) in [("2.0.29", "2.0.30"), ("1.18.32", "1.19.2")] {
            let mut snapshot = installed_snapshot("opencode", installed);
            maintenance
                .enrich_snapshot(&target, &mut snapshot, true)
                .await;
            assert_eq!(
                snapshot["versionAdvisory"]["latestVersion"], latest,
                "{installed}"
            );
            assert_eq!(snapshot["versionAdvisory"]["status"], "behind_latest");
            // The update action installs the same package the advisory compares against.
            let package = if installed.starts_with("2.") {
                "@opencode/cli"
            } else {
                "opencode-ai"
            };
            assert_eq!(
                snapshot["versionAdvisory"]["updateCommand"],
                format!("npm install -g {package}@latest"),
                "{installed}"
            );
            let update = maintenance
                .capabilities(&target)
                .await
                .for_installed_version(&target.driver, Some(installed))
                .update
                .expect("update action");
            assert_eq!(update.args, ["install", "-g", &format!("{package}@latest")]);
        }
    }

    #[tokio::test]
    async fn enriches_cursor_snapshot_from_installer_metadata() {
        let (endpoints, _) =
            latest_version_endpoint_fixture(br#"{\"version\":\"0.148.0\"}"#.to_vec()).await;
        let maintenance = ProviderMaintenance::with_version_endpoints(endpoints);
        let directory = tempfile::tempdir().expect("Cursor release directory");
        let binary = directory.path().join(
            "home/.local/share/cursor-agent/versions/2026.06.19-20-24-33-653a7fb/cursor-agent",
        );
        std::fs::create_dir_all(binary.parent().expect("Cursor release parent"))
            .expect("Cursor release parent");
        std::fs::write(&binary, b"fixture").expect("Cursor release binary");
        let binary_path = binary.to_string_lossy().into_owned();
        let mut snapshot = installed_snapshot("cursor", "2026.06.19-20-24-33-653a7fb");

        maintenance
            .enrich_snapshot(&target("cursor", &binary_path), &mut snapshot, true)
            .await;

        assert_eq!(snapshot["versionAdvisory"]["status"], "behind_latest");
        assert_eq!(
            snapshot["versionAdvisory"]["latestVersion"],
            "2026.08.04-aaa8809"
        );
        assert_eq!(
            snapshot["versionAdvisory"]["updateCommand"],
            format!("{binary_path} update")
        );
        assert_eq!(snapshot["versionAdvisory"]["canUpdate"], true);
    }

    #[tokio::test]
    async fn manual_refresh_observes_a_new_opencode_release() {
        let (registry_url, registry_version, requests) =
            mutable_npm_registry_fixture("1.18.11").await;
        let maintenance = ProviderMaintenance::with_registry_base_url(registry_url);
        let directory = tempfile::tempdir().expect("global npm directory");
        let target = global_npm_target(directory.path(), "opencode");
        let mut startup = installed_snapshot("opencode", "1.18.11");
        maintenance
            .enrich_snapshot(&target, &mut startup, true)
            .await;

        *registry_version.write().await = "1.18.15".to_owned();
        maintenance.begin_latest_version_refresh();
        let mut manual = installed_snapshot("opencode", "1.18.11");
        manual["checkedAt"] = json!("2026-08-01T12:05:00Z");
        maintenance
            .enrich_snapshot(&target, &mut manual, true)
            .await;

        assert_eq!(manual["versionAdvisory"]["status"], "behind_latest");
        assert_eq!(manual["versionAdvisory"]["latestVersion"], "1.18.15");
        assert_eq!(
            manual["versionAdvisory"]["checkedAt"],
            "2026-08-01T12:05:00Z"
        );
        assert_eq!(requests.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn failed_registry_lookup_is_retried_without_waiting_for_cache_expiry() {
        let (registry_url, failing, requests) = recovering_npm_registry_fixture("1.18.15").await;
        let maintenance = ProviderMaintenance::with_registry_base_url(registry_url);
        let directory = tempfile::tempdir().expect("global npm directory");
        let target = global_npm_target(directory.path(), "opencode");
        let mut failed = installed_snapshot("opencode", "1.18.11");
        maintenance
            .enrich_snapshot(&target, &mut failed, true)
            .await;
        assert_eq!(failed["versionAdvisory"]["status"], "unknown");
        assert!(failed["versionAdvisory"]["message"].is_string());

        failing.store(false, Ordering::SeqCst);
        let mut recovered = installed_snapshot("opencode", "1.18.11");
        recovered["checkedAt"] = json!("2026-08-01T12:01:00Z");
        maintenance
            .enrich_snapshot(&target, &mut recovered, true)
            .await;

        assert_eq!(recovered["versionAdvisory"]["status"], "behind_latest");
        assert_eq!(recovered["versionAdvisory"]["latestVersion"], "1.18.15");
        assert_eq!(requests.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn disabled_checks_do_not_contact_the_registry() {
        let (registry_url, requests) = npm_registry_fixture("9.9.9").await;
        let maintenance = ProviderMaintenance::with_registry_base_url(registry_url);
        let directory = tempfile::tempdir().expect("global npm directory");
        let mut snapshot = installed_snapshot("codex", "1.0.0");
        maintenance
            .enrich_snapshot(
                &global_npm_target(directory.path(), "codex"),
                &mut snapshot,
                false,
            )
            .await;
        assert_eq!(snapshot["versionAdvisory"]["status"], "unknown");
        assert_eq!(requests.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn unknown_custom_installations_do_not_check_npm() {
        let wrapper = tempfile::NamedTempFile::new().expect("custom Codex wrapper");
        let (registry_url, requests) = npm_registry_fixture("9.9.9").await;
        let maintenance = ProviderMaintenance::with_registry_base_url(registry_url);
        let mut snapshot = installed_snapshot("codex", "1.0.0");
        let binary_path = wrapper.path().to_string_lossy().into_owned();

        maintenance
            .enrich_snapshot(&target("codex", &binary_path), &mut snapshot, true)
            .await;

        assert_eq!(snapshot["versionAdvisory"]["status"], "unknown");
        assert!(snapshot["versionAdvisory"]["updateCommand"].is_null());
        assert_eq!(snapshot["versionAdvisory"]["canUpdate"], false);
        assert_eq!(requests.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn registry_failure_keeps_provider_data_and_returns_unknown_advisory() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("registry listener");
        let address = listener.local_addr().expect("registry address");
        let app = Router::new().route(
            "/{*path}",
            get(|| async { StatusCode::SERVICE_UNAVAILABLE }),
        );
        tokio::spawn(async move { axum::serve(listener, app).await.expect("registry server") });
        let maintenance = ProviderMaintenance::with_registry_base_url(
            Url::parse(&format!("http://{address}/")).expect("registry URL"),
        );
        let directory = tempfile::tempdir().expect("global npm directory");
        let mut snapshot = installed_snapshot("codex", "1.0.0");
        snapshot["models"] = json!([{ "slug": "existing" }]);
        maintenance
            .enrich_snapshot(
                &global_npm_target(directory.path(), "codex"),
                &mut snapshot,
                true,
            )
            .await;
        assert_eq!(snapshot["versionAdvisory"]["status"], "unknown");
        assert_eq!(snapshot["models"][0]["slug"], "existing");
    }

    #[tokio::test]
    async fn expired_cache_entry_is_refetched() {
        let (registry_url, requests) = npm_registry_fixture("9.9.9").await;
        let maintenance = ProviderMaintenance::with_registry_base_url(registry_url);
        let directory = tempfile::tempdir().expect("global npm directory");
        maintenance.inner.latest_versions.lock().await.insert(
            LatestVersionSource::Npm("@openai/codex"),
            VersionCacheEntry {
                expires_at: tokio::time::Instant::now() - Duration::from_secs(1),
                version: "1.0.0".to_owned(),
                checked_at: Some("2026-08-01T11:00:00Z".to_owned()),
                generation: 0,
            },
        );
        let mut snapshot = installed_snapshot("codex", "1.0.0");
        maintenance
            .enrich_snapshot(
                &global_npm_target(directory.path(), "codex"),
                &mut snapshot,
                true,
            )
            .await;
        assert_eq!(snapshot["versionAdvisory"]["latestVersion"], "9.9.9");
        assert_eq!(requests.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn fetches_different_package_versions_concurrently() {
        let (registry_url, concurrent_requests) = delayed_npm_registry_fixture("9.9.9").await;
        let maintenance = ProviderMaintenance::with_registry_base_url(registry_url);
        let directory = tempfile::tempdir().expect("package manager directory");
        let codex_target = global_npm_target(directory.path(), "codex");
        let opencode_target = global_npm_target(directory.path(), "opencode");
        let mut codex = installed_snapshot("codex", "1.0.0");
        let mut opencode = installed_snapshot("opencode", "1.0.0");

        tokio::join!(
            maintenance.enrich_snapshot(&codex_target, &mut codex, true),
            maintenance.enrich_snapshot(&opencode_target, &mut opencode, true),
        );

        assert!(concurrent_requests.load(Ordering::SeqCst) >= 2);
    }

    fn update_command_fixture(
        sandbox: &TestSandbox,
        name: &'static str,
        script: String,
    ) -> ProviderUpdateCommand {
        let executable = sandbox.path(if cfg!(windows) {
            format!("{name}.cmd")
        } else {
            name.to_owned()
        });
        crate::test_support::executable_fixture::write_executable(&executable, script);
        ProviderUpdateCommand {
            display: name.to_owned(),
            executable: executable.display().to_string(),
            args: vec![],
            lock_key: name,
        }
    }

    fn output_command(sandbox: &TestSandbox, bytes: usize) -> ProviderUpdateCommand {
        let script = if cfg!(windows) {
            format!(
                "@echo off\r\n\"{}\" -NoProfile -NonInteractive -Command \"[Console]::Out.Write('x' * {bytes})\"\r\nexit /b %ERRORLEVEL%\r\n",
                sandbox.executable_on_path("powershell.exe").display()
            )
        } else {
            format!("#!/bin/sh\nhead -c {bytes} /dev/zero | tr '\\0' x\n")
        };
        update_command_fixture(sandbox, "test-output", script)
    }

    fn exit_command(sandbox: &TestSandbox, code: i32) -> ProviderUpdateCommand {
        let script = if cfg!(windows) {
            format!("@exit /b {code}\r\n")
        } else {
            format!("#!/bin/sh\nexit {code}\n")
        };
        update_command_fixture(sandbox, "test-exit", script)
    }

    fn sleep_command(sandbox: &TestSandbox) -> ProviderUpdateCommand {
        let script = if cfg!(windows) {
            format!(
                "@echo off\r\n\"{}\" -NoProfile -NonInteractive -Command \"Start-Sleep -Seconds 2\"\r\nexit /b %ERRORLEVEL%\r\n",
                sandbox.executable_on_path("powershell.exe").display()
            )
        } else {
            format!(
                "#!/bin/sh\nexec '{}' 2\n",
                sandbox.executable_on_path("sleep").display()
            )
        };
        update_command_fixture(sandbox, "test-sleep", script)
    }

    #[tokio::test]
    async fn update_command_captures_bounded_output() {
        let sandbox = TestSandbox::new("provider-update-output");
        let command = output_command(&sandbox, 12_000);
        let result = ProviderMaintenance::new()
            .run_update_command(
                &sandbox_target(&sandbox, "cursor", "cursor-agent"),
                &command,
                &CancellationToken::new(),
            )
            .await
            .expect("command result");
        assert_eq!(result.exit_code, 0);
        assert!(
            result
                .output
                .as_deref()
                .is_some_and(|value| value.chars().count() <= 10_000)
        );
    }

    #[tokio::test]
    async fn update_command_preserves_non_zero_exit_code() {
        let sandbox = TestSandbox::new("provider-update-exit");
        let result = ProviderMaintenance::new()
            .run_update_command(
                &sandbox_target(&sandbox, "cursor", "cursor-agent"),
                &exit_command(&sandbox, 7),
                &CancellationToken::new(),
            )
            .await
            .expect("non-zero command result");
        assert_eq!(result.exit_code, 7);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn update_command_applies_the_same_case_variant_path_used_for_resolution() {
        let root = tempfile::tempdir().expect("update PATH root");
        let first = root.path().join("first");
        let second = root.path().join("second");
        std::fs::create_dir_all(&first).expect("first PATH directory");
        std::fs::create_dir_all(&second).expect("second PATH directory");
        for (directory, label) in [(&first, "first"), (&second, "second")] {
            let executable = directory.join("manager");
            crate::test_support::executable_fixture::write_executable(
                &executable,
                format!("#!/bin/sh\nprintf '%s:%s' '{label}' \"$PATH\" > \"$PATH_MARKER\"\n"),
            );
        }
        let marker = root.path().join("effective-path");
        let target = ProviderMaintenanceTarget {
            instance_id: "cursor-work".to_owned(),
            driver: "cursor".to_owned(),
            binary_path: "cursor-agent".to_owned(),
            environment: vec![
                ("pAtH".into(), first.as_os_str().to_owned()),
                ("PATH".into(), second.as_os_str().to_owned()),
                ("PATH_MARKER".into(), marker.as_os_str().to_owned()),
            ],
        };
        let update = ProviderUpdateCommand {
            display: "manager update".to_owned(),
            executable: "manager".to_owned(),
            args: vec!["update".to_owned()],
            lock_key: "test-manager",
        };

        let result = ProviderMaintenance::new()
            .run_update_command(&target, &update, &CancellationToken::new())
            .await
            .expect("update command");

        assert_eq!(result.exit_code, 0);
        assert_eq!(
            std::fs::read_to_string(marker).expect("PATH marker"),
            format!("first:{}", first.to_string_lossy())
        );
    }

    #[tokio::test]
    async fn update_command_timeout_stops_the_child() {
        let sandbox = TestSandbox::new("provider-update-timeout");
        let error = ProviderMaintenance::new()
            .run_update_command_with_timeout(
                &sandbox_target(&sandbox, "cursor", "cursor-agent"),
                &sleep_command(&sandbox),
                &CancellationToken::new(),
                Duration::from_millis(25),
            )
            .await
            .expect_err("sleep command must time out");
        assert!(error.contains("timed out"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_claude_package_actions_run_through_cmd_shims() {
        let directory = tempfile::tempdir().expect("update shim directory");
        let winget_capture = directory.path().join("winget-args.txt");
        let npm_capture = directory.path().join("npm-args.txt");
        crate::test_support::executable_fixture::write_executable(
            &directory.path().join("winget.cmd"),
            "@echo off\r\n> \"%WINGET_CAPTURE%\" echo %*\r\nexit /b 0\r\n",
        );
        crate::test_support::executable_fixture::write_executable(
            &directory.path().join("npm.cmd"),
            "@echo off\r\n> \"%NPM_CAPTURE%\" echo %*\r\nexit /b 0\r\n",
        );
        let target = ProviderMaintenanceTarget {
            instance_id: "claude-windows".to_owned(),
            driver: "claudeAgent".to_owned(),
            binary_path: "claude".to_owned(),
            environment: vec![
                ("PATH".into(), directory.path().as_os_str().to_owned()),
                (
                    "WINGET_CAPTURE".into(),
                    winget_capture.as_os_str().to_owned(),
                ),
                ("NPM_CAPTURE".into(), npm_capture.as_os_str().to_owned()),
            ],
        };
        let cases = [
            (
                super::source::capabilities_for_paths(
                    &target,
                    Some(Path::new(
                        "C:/Users/me/AppData/Local/Microsoft/WinGet/Links/claude.exe",
                    )),
                    None,
                ),
                &winget_capture,
                "upgrade Anthropic.ClaudeCode",
            ),
            (
                super::source::capabilities_for_paths(
                    &target,
                    Some(Path::new("C:/Users/me/AppData/Roaming/npm/claude.cmd")),
                    None,
                ),
                &npm_capture,
                "install -g @anthropic-ai/claude-code@latest",
            ),
        ];
        for (capabilities, capture, expected) in cases {
            let result = ProviderMaintenance::new()
                .run_update_command(
                    &target,
                    capabilities.update.as_ref().expect("executable action"),
                    &CancellationToken::new(),
                )
                .await
                .expect("update command result");
            assert_eq!(result.exit_code, 0);
            assert_eq!(
                std::fs::read_to_string(capture)
                    .expect("captured arguments")
                    .trim(),
                expected
            );
        }
    }

    #[tokio::test]
    async fn rejects_a_second_update_for_the_same_instance() {
        let maintenance = ProviderMaintenance::new();
        let target = target("codex", "codex");
        let first = maintenance
            .reserve_target(
                &ProviderMaintenanceTarget {
                    instance_id: "codex-work".to_owned(),
                    ..target.clone()
                },
                1,
            )
            .expect("first reservation");
        let target = ProviderMaintenanceTarget {
            instance_id: "codex-work".to_owned(),
            ..target
        };
        assert_eq!(
            maintenance.reserve_target(&target, 1).unwrap_err(),
            "An update is already running for this provider."
        );
        drop(first);
        assert!(maintenance.reserve_target(&target, 1).is_ok());
    }

    #[test]
    fn stale_update_lifecycle_cannot_overwrite_or_release_a_new_update() {
        let maintenance = ProviderMaintenance::new();
        let target = ProviderMaintenanceTarget {
            instance_id: "cursor-work".to_owned(),
            ..target("cursor", "cursor-agent")
        };
        let old = maintenance
            .reserve_target(&target, 1)
            .expect("old reservation");
        assert!(maintenance.set_update_state_if_current(
            "cursor-work",
            "cursor",
            old.token(),
            json!({ "status": "running", "message": "old" }),
        ));
        maintenance.invalidate_update_lifecycles(2, |current| current == &target);
        assert!(maintenance.set_update_state_if_current(
            "cursor-work",
            "cursor",
            old.token(),
            json!({ "status": "running", "message": "still-current" }),
        ));

        maintenance.invalidate_update_lifecycles(3, |_| false);
        let new = maintenance
            .reserve_target(&target, 3)
            .expect("new reservation");
        assert!(maintenance.set_update_state_if_current(
            "cursor-work",
            "cursor",
            new.token(),
            json!({ "status": "running", "message": "new" }),
        ));
        assert!(!maintenance.set_update_state_if_current(
            "cursor-work",
            "cursor",
            old.token(),
            json!({ "status": "failed", "message": "stale" }),
        ));

        drop(old);
        assert_eq!(
            maintenance.reserve_target(&target, 3).unwrap_err(),
            "An update is already running for this provider."
        );
        let mut snapshot = json!({ "instanceId": "cursor-work", "driver": "cursor" });
        maintenance.overlay_update_state(&mut snapshot);
        assert_eq!(snapshot["updateState"]["message"], "new");

        drop(new);
        assert!(maintenance.reserve_target(&target, 3).is_ok());
    }

    #[tokio::test]
    async fn shared_package_manager_updates_queue() {
        let maintenance = ProviderMaintenance::new();
        let lock = maintenance.command_lock("npm-global");
        let first = lock.clone().lock_owned().await;
        assert!(lock.clone().try_lock_owned().is_err());
        drop(first);
        assert!(lock.try_lock_owned().is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn maintenance_instance_path_outranks_ambient_path_in_an_isolated_process() {
        const CASE: &str = "provider-maintenance-path-precedence";
        const TEST_NAME: &str = "production::provider_maintenance::tests::maintenance_instance_path_outranks_ambient_path_in_an_isolated_process";
        const SENTINEL: &str =
            "BIBCODE_TEST_ISOLATED_CASE_DONE=provider-maintenance-path-precedence";

        if TestSandbox::is_isolated_case(CASE, TEST_NAME) {
            let instance = std::path::PathBuf::from(
                std::env::var_os("BIBCODE_TEST_INSTANCE_PATH").expect("isolated instance PATH"),
            );
            let ambient_executable = std::path::PathBuf::from(
                std::env::var_os("BIBCODE_TEST_AMBIENT_EXECUTABLE")
                    .expect("isolated ambient executable"),
            );
            assert_eq!(
                resolve_provider_executable_with_environment(
                    "opencode",
                    std::iter::empty::<(&std::ffi::OsStr, &std::ffi::OsStr)>(),
                ),
                Some(ambient_executable),
                "the child must have a competing ambient provider executable"
            );
            let target = ProviderMaintenanceTarget {
                instance_id: "opencode-work".to_owned(),
                driver: "opencode".to_owned(),
                binary_path: "opencode".to_owned(),
                environment: vec![("PaTh".into(), instance.as_os_str().to_owned())],
            };

            let capabilities = ProviderMaintenance::new().capabilities(&target).await;

            assert_eq!(
                capabilities
                    .update
                    .as_ref()
                    .map(|update| update.display.as_str()),
                Some("npm install -g opencode-ai@latest")
            );
            println!("{SENTINEL}");
            return;
        }

        let sandbox = TestSandbox::new("provider-maintenance-path");
        let ambient = sandbox.path("ambient/.opencode/bin");
        let instance = sandbox.path("instance/.npm-global/bin");
        std::fs::create_dir_all(&ambient).expect("ambient executable directory");
        std::fs::create_dir_all(&instance).expect("instance executable directory");
        let ambient_executable = ambient.join("opencode");
        std::fs::write(&ambient_executable, b"ambient").expect("ambient executable");
        std::fs::write(instance.join("opencode"), b"instance").expect("instance executable");
        let output = sandbox.run_isolated_case(
            CASE,
            TEST_NAME,
            &[
                ("PATH", ambient.as_os_str()),
                ("BIBCODE_TEST_INSTANCE_PATH", instance.as_os_str()),
                (
                    "BIBCODE_TEST_AMBIENT_EXECUTABLE",
                    ambient_executable.as_os_str(),
                ),
            ],
        );
        assert!(
            output.status.success(),
            "isolated maintenance PATH precedence case failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains(SENTINEL));
    }
}
use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use futures_util::StreamExt;
use serde_json::{Value, json};
use url::Url;

use latest::{LatestVersionFailure, LatestVersionSource};

use crate::git::{OutputPolicy, ProcessRequest, ProcessRunner};

use super::provider_runtime::{
    normalize_provider_environment, prepare_provider_launch,
    resolve_provider_executable_with_environment,
};

const CACHE_TTL: Duration = Duration::from_secs(60 * 60);
const LATEST_RESPONSE_LIMIT: usize = 256 * 1024;
const REGISTRY_TIMEOUT: Duration = Duration::from_secs(4);
const UPDATE_CHECK_FAILED_MESSAGE: &str =
    "Could not check for provider updates. Refresh provider status to try again.";
const UPDATE_MESSAGE: &str = "Install the update now or review provider settings.";
const UPDATE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const UPDATE_OUTPUT_LIMIT: usize = 10_000;

pub(crate) fn provider_version_advanced(
    driver: &str,
    before: Option<&str>,
    after: Option<&str>,
) -> bool {
    provider_version_scheme(driver)
        .is_some_and(|scheme| latest::version_advanced(scheme, before, after))
}

pub(crate) fn provider_version_regressed(
    driver: &str,
    before: Option<&str>,
    after: Option<&str>,
) -> bool {
    provider_version_scheme(driver)
        .is_some_and(|scheme| latest::version_advanced(scheme, after, before))
}

fn provider_version_scheme(driver: &str) -> Option<latest::VersionScheme> {
    match driver {
        "cursor" => Some(latest::VersionScheme::CursorRelease),
        "claudeAgent" | "codex" | "opencode" => Some(latest::VersionScheme::Semver),
        _ => None,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProviderMaintenanceTarget {
    #[allow(dead_code)] // Used by Task 2 for per-provider update state.
    pub(crate) instance_id: String,
    pub(crate) driver: String,
    pub(crate) binary_path: String,
    pub(crate) environment: Vec<(OsString, OsString)>,
}

#[derive(Clone, Debug)]
pub(crate) struct ProviderMaintenance {
    inner: Arc<ProviderMaintenanceInner>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProviderUpdateLifecycleToken(u64);

#[derive(Debug)]
pub(crate) struct ProviderUpdateReservation {
    instance_id: String,
    token: ProviderUpdateLifecycleToken,
    updates: Arc<Mutex<ProviderUpdateCoordinator>>,
}

impl Drop for ProviderUpdateReservation {
    fn drop(&mut self) {
        let mut updates = self
            .updates
            .lock()
            .expect("provider update coordinator lock");
        if let Some(lifecycle) = updates.lifecycles.get_mut(&self.instance_id)
            && lifecycle.token == self.token
        {
            lifecycle.active = false;
        }
    }
}

impl ProviderUpdateReservation {
    pub(crate) fn token(&self) -> ProviderUpdateLifecycleToken {
        self.token
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProviderUpdateCommandResult {
    pub(crate) exit_code: i32,
    pub(crate) output: Option<String>,
}

#[derive(Debug)]
struct ProviderMaintenanceInner {
    client: reqwest::Client,
    version_endpoints: LatestVersionEndpoints,
    latest_version_generation: AtomicU64,
    latest_versions: tokio::sync::Mutex<HashMap<LatestVersionSource, VersionCacheEntry>>,
    latest_version_locks: Mutex<HashMap<LatestVersionSource, Arc<tokio::sync::Mutex<()>>>>,
    updates: Arc<Mutex<ProviderUpdateCoordinator>>,
    command_locks: Mutex<HashMap<&'static str, Arc<tokio::sync::Mutex<()>>>>,
}

#[derive(Debug, Default)]
struct ProviderUpdateCoordinator {
    next_token: u64,
    lifecycles: HashMap<String, ProviderUpdateLifecycle>,
    states: HashMap<String, RetainedProviderUpdateState>,
}

#[derive(Debug)]
struct ProviderUpdateLifecycle {
    target: ProviderMaintenanceTarget,
    settings_generation: u64,
    token: ProviderUpdateLifecycleToken,
    active: bool,
}

#[derive(Debug)]
struct RetainedProviderUpdateState {
    driver: String,
    token: ProviderUpdateLifecycleToken,
    state: Value,
}

#[derive(Clone, Debug)]
struct VersionCacheEntry {
    expires_at: tokio::time::Instant,
    version: String,
    checked_at: Option<String>,
    generation: u64,
}

#[derive(Clone, Debug)]
struct LatestVersionEndpoints {
    npm_registry_base_url: Url,
    claude_release_base_url: Url,
    cursor_installer_url: Url,
}

impl LatestVersionEndpoints {
    fn production() -> Self {
        Self {
            npm_registry_base_url: Url::parse("https://registry.npmjs.org/")
                .expect("npm registry URL"),
            claude_release_base_url: Url::parse(
                "https://downloads.claude.ai/claude-code-releases/",
            )
            .expect("Claude release URL"),
            cursor_installer_url: Url::parse("https://cursor.com/install")
                .expect("Cursor installer URL"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum LatestVersionCheck {
    Success {
        version: String,
        checked_at: Option<String>,
    },
    Failed {
        checked_at: Option<String>,
    },
}

impl ProviderMaintenance {
    pub(crate) fn new() -> Self {
        Self::with_version_endpoints(LatestVersionEndpoints::production())
    }

    #[cfg(test)]
    pub(crate) fn with_registry_base_url(registry_base_url: Url) -> Self {
        let mut endpoints = LatestVersionEndpoints::production();
        endpoints.npm_registry_base_url = registry_base_url;
        Self::with_version_endpoints(endpoints)
    }

    #[cfg(test)]
    pub(crate) fn with_cursor_installer_url(cursor_installer_url: Url) -> Self {
        let mut endpoints = LatestVersionEndpoints::production();
        endpoints.cursor_installer_url = cursor_installer_url;
        Self::with_version_endpoints(endpoints)
    }

    #[cfg(test)]
    fn with_version_endpoints(version_endpoints: LatestVersionEndpoints) -> Self {
        Self::with_version_endpoints_inner(version_endpoints)
    }

    #[cfg(not(test))]
    fn with_version_endpoints(version_endpoints: LatestVersionEndpoints) -> Self {
        Self::with_version_endpoints_inner(version_endpoints)
    }

    fn with_version_endpoints_inner(version_endpoints: LatestVersionEndpoints) -> Self {
        Self {
            inner: Arc::new(ProviderMaintenanceInner {
                client: reqwest::Client::new(),
                version_endpoints,
                latest_version_generation: AtomicU64::new(0),
                latest_versions: tokio::sync::Mutex::new(HashMap::new()),
                latest_version_locks: Mutex::new(HashMap::new()),
                updates: Arc::new(Mutex::new(ProviderUpdateCoordinator::default())),
                command_locks: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub(crate) fn begin_latest_version_refresh(&self) {
        self.inner
            .latest_version_generation
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |generation| {
                generation.checked_add(1)
            })
            .expect("provider latest-version generations exhausted");
    }

    fn latest_version_lock(&self, source: LatestVersionSource) -> Arc<tokio::sync::Mutex<()>> {
        self.inner
            .latest_version_locks
            .lock()
            .expect("provider latest-version locks")
            .entry(source)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    pub(crate) fn reserve_target(
        &self,
        target: &ProviderMaintenanceTarget,
        settings_generation: u64,
    ) -> Result<ProviderUpdateReservation, &'static str> {
        let mut updates = self
            .inner
            .updates
            .lock()
            .expect("provider update coordinator lock");
        if updates
            .lifecycles
            .get(&target.instance_id)
            .is_some_and(|lifecycle| lifecycle.active)
        {
            return Err("An update is already running for this provider.");
        }
        updates.next_token = updates
            .next_token
            .checked_add(1)
            .expect("provider update lifecycle tokens exhausted");
        let token = ProviderUpdateLifecycleToken(updates.next_token);
        updates.lifecycles.insert(
            target.instance_id.clone(),
            ProviderUpdateLifecycle {
                target: target.clone(),
                settings_generation,
                token,
                active: true,
            },
        );
        updates.states.remove(&target.instance_id);
        Ok(ProviderUpdateReservation {
            instance_id: target.instance_id.clone(),
            token,
            updates: self.inner.updates.clone(),
        })
    }

    pub(crate) fn command_lock(&self, lock_key: &'static str) -> Arc<tokio::sync::Mutex<()>> {
        self.inner
            .command_locks
            .lock()
            .expect("provider update command locks")
            .entry(lock_key)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    pub(crate) fn set_update_state_if_current(
        &self,
        instance_id: &str,
        driver: &str,
        token: ProviderUpdateLifecycleToken,
        state: Value,
    ) -> bool {
        let mut updates = self
            .inner
            .updates
            .lock()
            .expect("provider update coordinator lock");
        if !updates
            .lifecycles
            .get(instance_id)
            .is_some_and(|lifecycle| lifecycle.target.driver == driver && lifecycle.token == token)
        {
            return false;
        }
        updates.states.insert(
            instance_id.to_owned(),
            RetainedProviderUpdateState {
                driver: driver.to_owned(),
                token,
                state,
            },
        );
        true
    }

    pub(crate) fn invalidate_update_lifecycle_if_current(
        &self,
        instance_id: &str,
        driver: &str,
        token: ProviderUpdateLifecycleToken,
    ) {
        let mut updates = self
            .inner
            .updates
            .lock()
            .expect("provider update coordinator lock");
        if !updates
            .lifecycles
            .get(instance_id)
            .is_some_and(|lifecycle| lifecycle.target.driver == driver && lifecycle.token == token)
        {
            return;
        }
        updates.lifecycles.remove(instance_id);
        if updates
            .states
            .get(instance_id)
            .is_some_and(|state| state.driver == driver && state.token == token)
        {
            updates.states.remove(instance_id);
        }
    }

    pub(crate) fn invalidate_update_lifecycles(
        &self,
        settings_generation: u64,
        mut is_current: impl FnMut(&ProviderMaintenanceTarget) -> bool,
    ) {
        let mut updates = self
            .inner
            .updates
            .lock()
            .expect("provider update coordinator lock");
        let invalid = updates
            .lifecycles
            .iter_mut()
            .filter_map(|(instance_id, lifecycle)| {
                if is_current(&lifecycle.target) {
                    lifecycle.settings_generation = settings_generation;
                    None
                } else {
                    Some(instance_id.clone())
                }
            })
            .collect::<Vec<_>>();
        for instance_id in invalid {
            let Some(lifecycle) = updates.lifecycles.remove(&instance_id) else {
                continue;
            };
            if updates.states.get(&instance_id).is_some_and(|state| {
                state.driver == lifecycle.target.driver && state.token == lifecycle.token
            }) {
                updates.states.remove(&instance_id);
            }
        }
    }

    pub(crate) fn overlay_update_state(&self, snapshot: &mut Value) {
        let Some((instance_id, driver)) = snapshot
            .get("instanceId")
            .and_then(Value::as_str)
            .zip(snapshot.get("driver").and_then(Value::as_str))
        else {
            return;
        };
        let updates = self
            .inner
            .updates
            .lock()
            .expect("provider update coordinator lock");
        let state = updates.states.get(instance_id).and_then(|retained| {
            updates
                .lifecycles
                .get(instance_id)
                .filter(|lifecycle| {
                    lifecycle.target.driver == driver
                        && retained.driver == driver
                        && lifecycle.token == retained.token
                })
                .map(|_| retained.state.clone())
        });
        drop(updates);
        if let Some(state) = state {
            snapshot["updateState"] = state;
        } else if let Some(snapshot) = snapshot.as_object_mut() {
            snapshot.remove("updateState");
        }
    }

    pub(crate) fn prune_update_states<'a, I>(&self, identities: I)
    where
        I: IntoIterator<Item = (&'a str, &'a str)>,
    {
        let identities = identities.into_iter().collect::<HashSet<_>>();
        let mut updates = self
            .inner
            .updates
            .lock()
            .expect("provider update coordinator lock");
        let invalid = updates
            .states
            .iter()
            .filter(|(instance_id, retained)| {
                !identities.contains(&(instance_id.as_str(), retained.driver.as_str()))
                    || !updates
                        .lifecycles
                        .get(instance_id.as_str())
                        .is_some_and(|lifecycle| {
                            lifecycle.target.driver == retained.driver
                                && lifecycle.token == retained.token
                        })
            })
            .map(|(instance_id, _)| instance_id.clone())
            .collect::<Vec<_>>();
        for instance_id in invalid {
            updates.states.remove(&instance_id);
        }
    }

    pub(crate) fn update_lifecycle_is_current(
        &self,
        target: &ProviderMaintenanceTarget,
        settings_generation: u64,
        token: ProviderUpdateLifecycleToken,
    ) -> bool {
        self.inner
            .updates
            .lock()
            .expect("provider update coordinator lock")
            .lifecycles
            .get(&target.instance_id)
            .is_some_and(|lifecycle| {
                lifecycle.target == *target
                    && lifecycle.settings_generation == settings_generation
                    && lifecycle.token == token
                    && lifecycle.active
            })
    }

    pub(crate) async fn capabilities(
        &self,
        target: &ProviderMaintenanceTarget,
    ) -> ProviderMaintenanceCapabilities {
        let environment = normalize_provider_environment(
            target
                .environment
                .iter()
                .map(|(name, value)| (name.as_os_str(), value.as_os_str())),
        );
        let resolved = resolve_provider_executable_with_environment(
            &target.binary_path,
            environment
                .iter()
                .map(|(name, value)| (name.as_os_str(), value.as_os_str())),
        );
        let canonical = match resolved.as_deref() {
            Some(path) => tokio::fs::canonicalize(path).await.ok(),
            None => None,
        };
        // A Windows npm shim is a batch file, not a link: the package it runs tells OpenCode 1
        // (`opencode-ai`) from OpenCode 2 (`@opencode/cli`).
        let canonical = match resolved.as_deref() {
            Some(path) if target.driver == "opencode" => {
                let owned = crate::provider::opencode::resolve_owned_executable(
                    crate::process::Platform::current(),
                    path,
                );
                if owned.as_path() == path {
                    canonical
                } else {
                    Some(owned)
                }
            }
            _ => canonical,
        };
        if target.driver == "claudeAgent" {
            let hints = source::discover_claude_hints(target, resolved.as_deref()).await;
            source::capabilities_for_paths_with_claude_hints(
                &target.driver,
                &target.binary_path,
                resolved.as_deref(),
                canonical.as_deref(),
                &hints,
            )
        } else {
            source::capabilities_for_paths(target, resolved.as_deref(), canonical.as_deref())
        }
    }

    pub(crate) async fn run_update_command(
        &self,
        target: &ProviderMaintenanceTarget,
        update: &ProviderUpdateCommand,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<ProviderUpdateCommandResult, String> {
        self.run_update_command_with_timeout(target, update, cancellation, UPDATE_TIMEOUT)
            .await
    }

    async fn run_update_command_with_timeout(
        &self,
        target: &ProviderMaintenanceTarget,
        update: &ProviderUpdateCommand,
        cancellation: &tokio_util::sync::CancellationToken,
        timeout: Duration,
    ) -> Result<ProviderUpdateCommandResult, String> {
        let environment = normalize_provider_environment(
            target
                .environment
                .iter()
                .map(|(name, value)| (name.as_os_str(), value.as_os_str())),
        );
        let executable = resolve_provider_executable_with_environment(
            &update.executable,
            environment
                .iter()
                .map(|(name, value)| (name.as_os_str(), value.as_os_str())),
        )
        .ok_or_else(|| format!("Could not resolve update command '{}'.", update.executable))?;
        let launch = prepare_provider_launch(&executable, &update.args)?;
        let cwd = std::env::current_dir()
            .map_err(|error| format!("Could not determine update working directory: {error}"))?;
        let process_output = ProcessRunner
            .run(
                ProcessRequest {
                    operation: "provider.maintenance.update".to_owned(),
                    command: launch.program,
                    args: launch.args,
                    cwd,
                    env: environment,
                    stdin: None,
                    timeout,
                    max_output_bytes: UPDATE_OUTPUT_LIMIT,
                    output_policy: OutputPolicy::Truncate,
                    append_truncation_marker: true,
                    allow_non_zero_exit: true,
                },
                cancellation,
            )
            .await
            .map_err(|error| error.to_string())?;
        let output = [process_output.stderr, process_output.stdout]
            .into_iter()
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ProviderUpdateCommandResult {
            exit_code: process_output.exit_code,
            output: (!output.is_empty())
                .then(|| output.chars().take(UPDATE_OUTPUT_LIMIT).collect()),
        })
    }

    pub(crate) async fn enrich_snapshot(
        &self,
        target: &ProviderMaintenanceTarget,
        snapshot: &mut Value,
        checks_enabled: bool,
    ) {
        let current = snapshot
            .get("version")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty());
        let capabilities = self
            .capabilities(target)
            .await
            .for_installed_version(&target.driver, current);
        let should_check = checks_enabled
            && snapshot.get("enabled").and_then(Value::as_bool) == Some(true)
            && snapshot.get("installed").and_then(Value::as_bool) == Some(true)
            && current.is_some();
        let probe_checked_at = snapshot
            .get("checkedAt")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let latest_check = match (should_check, capabilities.latest) {
            (true, Some(source)) => Some(self.latest_version(source, probe_checked_at).await),
            _ => None,
        };
        let (latest, checked_at, check_failed) = match latest_check {
            Some(LatestVersionCheck::Success {
                version,
                checked_at,
            }) => (Some(version), checked_at, false),
            Some(LatestVersionCheck::Failed { checked_at }) => (None, checked_at, true),
            None => (None, None, false),
        };
        let status = if should_check {
            latest::advisory_status(capabilities.version_scheme, current, latest.as_deref())
        } else {
            "unknown"
        };
        let message = match status {
            "behind_latest" => Some(UPDATE_MESSAGE),
            _ if check_failed => Some(UPDATE_CHECK_FAILED_MESSAGE),
            _ => None,
        };
        snapshot["versionAdvisory"] = json!({
            "status": status,
            "currentVersion": current,
            "latestVersion": latest,
            "updateCommand": capabilities.display_command,
            "canUpdate": capabilities.update.is_some(),
            "checkedAt": checked_at,
            "message": message,
        });
    }

    async fn latest_version(
        &self,
        source: LatestVersionSource,
        checked_at: Option<String>,
    ) -> LatestVersionCheck {
        let lookup_lock = self.latest_version_lock(source);
        let _lookup_guard = lookup_lock.lock().await;
        let required_generation = self.inner.latest_version_generation.load(Ordering::Acquire);
        let cached = self
            .inner
            .latest_versions
            .lock()
            .await
            .get(&source)
            .filter(|entry| {
                entry.expires_at > tokio::time::Instant::now()
                    && entry.generation >= required_generation
            })
            .cloned();
        if let Some(entry) = cached {
            return LatestVersionCheck::Success {
                version: entry.version,
                checked_at: entry.checked_at,
            };
        }

        let result = self.fetch_latest_version(source).await;
        let version = match result {
            Ok(version) => version,
            Err(failure) => {
                tracing::warn!(
                    source = latest_source_label(source),
                    failure = failure.as_str(),
                    "provider latest-version check failed"
                );
                return LatestVersionCheck::Failed { checked_at };
            }
        };
        self.inner.latest_versions.lock().await.insert(
            source,
            VersionCacheEntry {
                expires_at: tokio::time::Instant::now() + CACHE_TTL,
                version: version.clone(),
                checked_at: checked_at.clone(),
                generation: required_generation,
            },
        );
        LatestVersionCheck::Success {
            version,
            checked_at,
        }
    }

    async fn fetch_latest_version(
        &self,
        source: LatestVersionSource,
    ) -> Result<String, LatestVersionFailure> {
        let url = match source {
            LatestVersionSource::Npm(package_name) => {
                let encoded: String =
                    url::form_urlencoded::byte_serialize(package_name.as_bytes()).collect();
                self.inner
                    .version_endpoints
                    .npm_registry_base_url
                    .join(&format!("{encoded}/latest"))
                    .map_err(|_| LatestVersionFailure::InvalidUrl)?
            }
            LatestVersionSource::Claude(channel) => self
                .inner
                .version_endpoints
                .claude_release_base_url
                .join(match channel {
                    latest::ClaudeReleaseChannel::Stable => "stable",
                    latest::ClaudeReleaseChannel::Latest => "latest",
                })
                .map_err(|_| LatestVersionFailure::InvalidUrl)?,
            LatestVersionSource::CursorInstaller => {
                self.inner.version_endpoints.cursor_installer_url.clone()
            }
        };
        let response = self
            .inner
            .client
            .get(url)
            .timeout(REGISTRY_TIMEOUT)
            .send()
            .await
            .map_err(|_| LatestVersionFailure::Request)?
            .error_for_status()
            .map_err(|_| LatestVersionFailure::HttpStatus)?;
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| LatestVersionFailure::Request)?;
            if body.len().saturating_add(chunk.len()) > LATEST_RESPONSE_LIMIT {
                return Err(LatestVersionFailure::ResponseTooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        latest::parse_latest_response(source, &body)
    }
}

fn latest_source_label(source: LatestVersionSource) -> &'static str {
    match source {
        LatestVersionSource::Npm(_) => "npm",
        LatestVersionSource::Claude(latest::ClaudeReleaseChannel::Stable) => "claude_stable",
        LatestVersionSource::Claude(latest::ClaudeReleaseChannel::Latest) => "claude_latest",
        LatestVersionSource::CursorInstaller => "cursor_installer",
    }
}

#[cfg(test)]
fn advisory_status(current: Option<&str>, latest: Option<&str>) -> &'static str {
    latest::advisory_status(latest::VersionScheme::Semver, current, latest)
}
