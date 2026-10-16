pub(crate) mod activity;
pub mod model;
pub mod runtime;
pub(crate) mod sse;
pub(crate) mod v2;
pub(crate) mod v2_activity;
#[cfg(test)]
mod v2_runtime_tests;

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::process::Platform;

/// The OpenCode server API a runtime speaks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OpenCodeProtocol {
    /// OpenCode 1.x: unprefixed routes such as `/session` and `/event`.
    V1,
    /// OpenCode 2.x: `/api/*` routes and the V2 event vocabulary.
    V2,
}

/// Which OpenCode server API answers at an endpoint.
///
/// OpenCode 2 replaced the V1 server API with `/api/*` routes and serves its web UI, as HTTP 200
/// HTML, for every unprefixed V1 path, so "any HTTP response" cannot tell the two apart.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum OpenCodeServerApi {
    /// `GET /global/health` answered `{"healthy": true}` as JSON: OpenCode 1.x.
    V1,
    /// `GET /api/info` answered with a 2.x version: OpenCode 2.
    V2 { version: String },
    /// `GET /api/info` answered with a version BiBCode cannot drive.
    Unsupported { version: String },
    /// Neither answered yet (still starting, unreachable, or wrong credentials).
    Unavailable,
}

/// Bounds each request of [`probe_server_api`] for callers without a tighter budget, so a stalled
/// connection delays a launch or an inventory refresh by seconds, not indefinitely.
pub(crate) const API_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Identifies the OpenCode server API at `endpoint` with the server's Basic credentials.
pub(crate) async fn probe_server_api(
    client: &reqwest::Client,
    endpoint: &str,
    password: Option<&str>,
) -> OpenCodeServerApi {
    let get_json = |path: &str| {
        let request = client.get(format!("{}{path}", endpoint.trim_end_matches('/')));
        let request = match password.filter(|password| !password.is_empty()) {
            Some(password) => request.basic_auth("opencode", Some(password)),
            None => request,
        };
        async move {
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
    };
    if get_json("/global/health")
        .await
        .is_some_and(|health| health.get("healthy").and_then(Value::as_bool) == Some(true))
    {
        return OpenCodeServerApi::V1;
    }
    match get_json("/api/info")
        .await
        .and_then(|info| info.get("version")?.as_str().map(str::to_owned))
    {
        Some(version) if major_version(&version) == Some(2) => OpenCodeServerApi::V2 { version },
        Some(version) => OpenCodeServerApi::Unsupported { version },
        None => OpenCodeServerApi::Unavailable,
    }
}

/// The provider error for an OpenCode server BiBCode cannot drive. `managed` is true when BiBCode
/// started the server from Binary path, and false for a configured Server URL.
pub(crate) fn unsupported_server_message(version: &str, managed: bool) -> String {
    if managed {
        format!(
            "OpenCode {version} isn't supported yet. In Settings → Providers, set Binary path to an OpenCode 1.x or 2.x executable."
        )
    } else {
        format!(
            "The OpenCode server at Server URL runs {version}, which isn't supported yet. In Settings → Providers, point Server URL at an OpenCode 1.x or 2.x server."
        )
    }
}

/// The first number of the first version-like word: OpenCode 1 prints `1.18.32`, OpenCode 2
/// prints `opencode v2.0.29`.
fn major_version(version_output: &str) -> Option<u64> {
    version_output.split_whitespace().find_map(|word| {
        word.trim_start_matches('v')
            .split('.')
            .next()?
            .parse::<u64>()
            .ok()
    })
}

/// How the inventory treats an OpenCode CLI, from its `--version` output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OpenCodeCliSupport {
    Supported(OpenCodeProtocol),
    /// A major version BiBCode cannot drive, with the provider status message.
    Unsupported(&'static str),
}

pub(crate) fn cli_support(version_output: &str) -> OpenCodeCliSupport {
    match major_version(version_output) {
        Some(major) if major >= 3 => OpenCodeCliSupport::Unsupported(
            "This OpenCode version isn't supported yet. In Settings → Providers, set Binary path to an OpenCode 1.x or 2.x executable.",
        ),
        Some(2) => OpenCodeCliSupport::Supported(OpenCodeProtocol::V2),
        _ => OpenCodeCliSupport::Supported(OpenCodeProtocol::V1),
    }
}

/// Resolves the native OpenCode executable behind the standard Windows npm
/// shim when BiBCode must own the launched process tree. OpenCode's native
/// launcher can detach from `opencode.cmd`, so supervising only the batch shim
/// does not provide a reliable shutdown boundary. OpenCode 1 installs as
/// `opencode-ai`; OpenCode 2 as `@opencode/cli`, whose shims are `opencode.cmd`
/// and `opencode2.cmd`.
pub(crate) fn resolve_owned_executable(platform: Platform, executable: &Path) -> PathBuf {
    let shim = executable
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase());
    let packages: &[&[&str]] = match shim.as_deref() {
        Some("opencode.cmd") => &[&["opencode-ai"], &["@opencode", "cli"]],
        Some("opencode2.cmd") => &[&["@opencode", "cli"]],
        _ => &[],
    };
    if platform != Platform::Windows || packages.is_empty() {
        return executable.to_path_buf();
    }
    let Some(shim_directory) = executable.parent() else {
        return executable.to_path_buf();
    };
    packages
        .iter()
        .map(|package| {
            package
                .iter()
                .fold(shim_directory.join("node_modules"), |path, part| {
                    path.join(part)
                })
                .join("bin")
                .join("opencode.exe")
        })
        .find(|native| native.is_file())
        .unwrap_or_else(|| executable.to_path_buf())
}

#[doc(hidden)]
pub use activity::{
    OpenCodeActivityFixtureAdapter, OpenCodeActivityOutput, OpenCodeActivityStateCounts,
};
#[cfg_attr(test, allow(unused_imports))]
pub use model::{
    OpenCodeInventorySnapshot, OpenCodeProviderModel, build_inventory_snapshot,
    merge_assistant_text, parse_model_slug,
};
#[cfg_attr(test, allow(unused_imports))]
pub use runtime::{
    OpenCodeRuntimeEvent, OpenCodeRuntimeEventStableView, OpenCodeSessionRuntime,
    OpenCodeSessionSnapshot,
};

/// HTTP fixtures shaped like real OpenCode servers, captured from `opencode serve` 1.18.32 and
/// 2.0.29 with Basic credentials `opencode:<password>`.
#[cfg(test)]
pub(crate) mod server_fixtures {
    use axum::{
        Json, Router,
        http::{HeaderMap, Method, StatusCode},
        response::{Html, IntoResponse},
        routing::get,
    };
    use base64::Engine as _;
    use serde_json::json;

    /// OpenCode 2 serves its web UI for every unprefixed GET, with or without credentials.
    const WEB_UI: &str = "<!doctype html>\n<html lang=\"en\"><head><title>OpenCode</title></head><body></body></html>\n";

    fn authorized(headers: &HeaderMap, password: &str) -> bool {
        let expected = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("opencode:{password}"))
        );
        headers
            .get("authorization")
            .is_some_and(|value| value.as_bytes() == expected.as_bytes())
    }

    /// The OpenCode 1 health route; 1.x answers 401 to every unauthenticated request.
    pub(crate) fn v1(password: &'static str) -> Router {
        Router::new().route(
            "/global/health",
            get(move |headers: HeaderMap| async move {
                if !authorized(&headers, password) {
                    return StatusCode::UNAUTHORIZED.into_response();
                }
                Json(json!({"healthy":true,"version":"1.18.32"})).into_response()
            }),
        )
    }

    /// An OpenCode 2.0.29 server: `/api/info` behind credentials, the web UI for unprefixed GETs
    /// (including the V1 `/global/health`, `/session`, `/event` and `/session/{id}/children`), and
    /// 405 for unprefixed POSTs such as `/session`, `/mcp` and `/session/{id}/prompt_async`.
    pub(crate) fn v2(password: &'static str) -> Router {
        api_server(password, "2.0.29")
    }

    /// An `/api/*` server reporting `version`, shaped like OpenCode 2.0.29.
    pub(crate) fn api_server(password: &'static str, version: &'static str) -> Router {
        Router::new()
            .route(
                "/api/info",
                get(move |headers: HeaderMap| async move {
                    if !authorized(&headers, password) {
                        return StatusCode::UNAUTHORIZED.into_response();
                    }
                    Json(json!({
                        "version":version,
                        "pid":4242,
                        "urls":["http://127.0.0.1:4096"],
                        "paths":{"tmp":"/tmp/opencode"},
                        "capabilities":{"persistentPty":true}
                    }))
                    .into_response()
                }),
            )
            .fallback(|method: Method| async move {
                if method == Method::GET {
                    Html(WEB_UI).into_response()
                } else {
                    StatusCode::METHOD_NOT_ALLOWED.into_response()
                }
            })
    }

    /// Serves `app` on a loopback port and returns its `http://` endpoint.
    pub(crate) async fn serve(app: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("OpenCode fixture should bind");
        let endpoint = format!(
            "http://{}",
            listener.local_addr().expect("OpenCode fixture address")
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("OpenCode fixture should serve");
        });
        (endpoint, server)
    }
}

#[cfg(test)]
mod server_api_tests {
    use super::{
        OpenCodeCliSupport, OpenCodeProtocol, OpenCodeServerApi, cli_support, probe_server_api,
        server_fixtures, unsupported_server_message,
    };

    #[tokio::test]
    async fn probe_identifies_the_v1_api_by_its_health_route() {
        let (endpoint, server) = server_fixtures::serve(server_fixtures::v1("secret")).await;
        let client = reqwest::Client::new();

        assert_eq!(
            probe_server_api(&client, &endpoint, Some("secret")).await,
            OpenCodeServerApi::V1
        );
        assert_eq!(
            probe_server_api(&client, &endpoint, Some("wrong")).await,
            OpenCodeServerApi::Unavailable,
            "a 401 is not readiness"
        );
        server.abort();
    }

    #[tokio::test]
    async fn probe_identifies_opencode_2_by_its_info_route_not_its_html() {
        let (endpoint, server) = server_fixtures::serve(server_fixtures::v2("secret")).await;
        let client = reqwest::Client::new();

        assert!(
            client
                .get(format!("{endpoint}/global/health"))
                .send()
                .await
                .expect("fixture answers")
                .status()
                .is_success(),
            "the fixture must reproduce OpenCode 2's HTTP 200 for V1 paths"
        );
        assert_eq!(
            probe_server_api(&client, &endpoint, Some("secret")).await,
            OpenCodeServerApi::V2 {
                version: "2.0.29".to_owned()
            }
        );
        assert_eq!(
            probe_server_api(&client, &endpoint, Some("wrong")).await,
            OpenCodeServerApi::Unavailable
        );
        assert_eq!(
            probe_server_api(&client, "http://127.0.0.1:1", Some("secret")).await,
            OpenCodeServerApi::Unavailable
        );
        server.abort();
    }

    #[tokio::test]
    async fn probe_refuses_an_api_server_of_another_major_version() {
        let (endpoint, server) =
            server_fixtures::serve(server_fixtures::api_server("secret", "3.0.0")).await;
        assert_eq!(
            probe_server_api(&reqwest::Client::new(), &endpoint, Some("secret")).await,
            OpenCodeServerApi::Unsupported {
                version: "3.0.0".to_owned()
            }
        );
        server.abort();
    }

    #[test]
    fn cli_versions_select_the_protocol_and_refuse_unknown_majors() {
        for v1 in ["1.18.32", "opencode 1.18.4", "", "not a version"] {
            assert_eq!(
                cli_support(v1),
                OpenCodeCliSupport::Supported(OpenCodeProtocol::V1),
                "{v1:?}"
            );
        }
        for v2 in ["opencode v2.0.29", "2.1.0"] {
            assert_eq!(
                cli_support(v2),
                OpenCodeCliSupport::Supported(OpenCodeProtocol::V2),
                "{v2:?}"
            );
        }
        assert!(matches!(
            cli_support("v3.0.0"),
            OpenCodeCliSupport::Unsupported(message) if message.contains("OpenCode 1.x or 2.x")
        ));
    }

    #[test]
    fn unsupported_server_messages_name_the_setting_to_change() {
        let managed = unsupported_server_message("3.0.0", true);
        assert!(managed.starts_with("OpenCode 3.0.0 isn't supported yet."));
        assert!(managed.contains("Binary path"));
        let external = unsupported_server_message("3.0.0", false);
        assert!(external.contains("runs 3.0.0"));
        assert!(external.contains("Server URL"));
    }
}

#[cfg(test)]
mod launch_tests {
    use super::*;

    #[test]
    fn windows_npm_shim_resolves_to_the_owned_native_executable() {
        let root = tempfile::tempdir().expect("temporary OpenCode npm layout");
        let shim = root.path().join("opencode.cmd");
        let native = root
            .path()
            .join("node_modules/opencode-ai/bin/opencode.exe");
        std::fs::create_dir_all(native.parent().expect("native parent"))
            .expect("native OpenCode directory");
        std::fs::write(&shim, "@opencode fixture").expect("OpenCode shim fixture");
        std::fs::write(&native, b"fixture").expect("native OpenCode fixture");

        assert_eq!(resolve_owned_executable(Platform::Windows, &shim), native);
        assert_eq!(resolve_owned_executable(Platform::Unix, &shim), shim);
    }

    /// OpenCode 2 installs as `@opencode/cli`, with `opencode.cmd` and `opencode2.cmd` shims.
    #[test]
    fn windows_npm_shims_of_opencode_2_resolve_to_its_native_executable() {
        let root = tempfile::tempdir().expect("temporary OpenCode 2 npm layout");
        let native = root
            .path()
            .join("node_modules/@opencode/cli/bin/opencode.exe");
        std::fs::create_dir_all(native.parent().expect("native parent"))
            .expect("native OpenCode directory");
        std::fs::write(&native, b"fixture").expect("native OpenCode fixture");
        for name in ["opencode.cmd", "opencode2.cmd"] {
            let shim = root.path().join(name);
            std::fs::write(&shim, "@opencode fixture").expect("OpenCode shim fixture");
            assert_eq!(
                resolve_owned_executable(Platform::Windows, &shim),
                native,
                "{name}"
            );
        }
    }

    #[test]
    fn custom_or_incomplete_windows_shims_keep_the_configured_executable() {
        let root = tempfile::tempdir().expect("temporary OpenCode shim layout");
        let custom = root.path().join("custom-opencode.cmd");
        let incomplete = root.path().join("opencode.cmd");
        std::fs::write(&custom, "@custom fixture").expect("custom shim fixture");
        std::fs::write(&incomplete, "@incomplete fixture").expect("incomplete shim fixture");

        assert_eq!(resolve_owned_executable(Platform::Windows, &custom), custom);
        assert_eq!(
            resolve_owned_executable(Platform::Windows, &incomplete),
            incomplete
        );
    }
}
