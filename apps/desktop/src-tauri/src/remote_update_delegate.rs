//! Bridges the in-process server's remote-update seam (spec section 4.5) onto the
//! desktop host's real updater. `updater.install` triggers exactly the flow a local
//! user triggers — including the update-protection drain of the backend.

use std::sync::Arc;

use coddy_studio_server::remote_update::{
    HostUpdaterFuture, HostUpdaterStatus, RemoteUpdateDelegate, RemoteUpdateInstallKind,
    RemoteUpdateInstallMode, RemoteUpdateRequester, RemoteUpdateState, RemoteUpdateSupport,
    RemoteUpdateSupportReason,
};
use serde_json::Value;
use tauri::{AppHandle, Manager, Runtime};

use crate::backend::BackendSupervisor;
use crate::updates::{DesktopUpdateInstallInput, DesktopUpdateManager, RemoteUpdateRequest};

/// The same facts feed `ServerConfig.remote_update_support` and this delegate, so the
/// descriptor and the RPC behavior cannot drift.
#[must_use]
pub fn derive_remote_update_support(updater_enabled: bool) -> RemoteUpdateSupport {
    if cfg!(debug_assertions) {
        RemoteUpdateSupport {
            install_mode: RemoteUpdateInstallMode::Manual,
            reason: RemoteUpdateSupportReason::UnpackagedBuild,
            install_kind: RemoteUpdateInstallKind::Unknown,
        }
    } else if updater_enabled {
        RemoteUpdateSupport {
            install_mode: RemoteUpdateInstallMode::Interactive,
            reason: RemoteUpdateSupportReason::Available,
            install_kind: RemoteUpdateInstallKind::Unknown,
        }
    } else {
        RemoteUpdateSupport {
            install_mode: RemoteUpdateInstallMode::Manual,
            reason: RemoteUpdateSupportReason::UpdaterUnavailable,
            install_kind: RemoteUpdateInstallKind::Unknown,
        }
    }
}

#[must_use]
pub fn map_desktop_update_state(state: &Value) -> HostUpdaterStatus {
    let phase = state["phase"].as_str().unwrap_or("idle");
    let status = state["status"].as_str().unwrap_or("idle");
    let latest_version = state["availableVersion"]
        .as_str()
        .or_else(|| state["downloadedVersion"].as_str())
        .map(str::to_owned);
    let mapped = match (phase, status) {
        ("protecting" | "installing", _) => RemoteUpdateState::Installing,
        ("failed", _) | (_, "error") => RemoteUpdateState::Error,
        (_, "checking") => RemoteUpdateState::Checking,
        (_, "downloading") => RemoteUpdateState::Downloading,
        (_, "available" | "downloaded") => RemoteUpdateState::UpdateAvailable,
        (_, "up-to-date") => RemoteUpdateState::UpToDate,
        _ => RemoteUpdateState::Idle,
    };
    let error = if mapped == RemoteUpdateState::Error {
        state["message"].as_str().map(str::to_owned)
    } else {
        None
    };
    let download_percent = (mapped == RemoteUpdateState::Downloading)
        .then(|| state["downloadPercent"].as_f64())
        .flatten()
        .map(|percent| percent.clamp(0.0, 100.0).floor() as u8);
    let install_stage = match phase {
        "installing" => Some("installing".to_owned()),
        "protecting" => state["protection"].as_array().and_then(|entries| {
            entries
                .iter()
                .find(|entry| entry["status"] == "pending")
                .and_then(|entry| entry["stage"].as_str())
                .map(str::to_owned)
        }),
        _ => None,
    };
    HostUpdaterStatus {
        target_version: latest_version.clone(),
        latest_version,
        state: mapped,
        error,
        download_percent,
        install_stage,
    }
}

#[must_use]
pub(crate) fn remote_update_request(requester: &RemoteUpdateRequester) -> RemoteUpdateRequest {
    let detail = match (requester.os.as_deref(), requester.ip_address.as_deref()) {
        (Some(os), Some(address)) => Some(format!("{os} ({address})")),
        (Some(os), None) => Some(os.to_owned()),
        (None, Some(address)) => Some(address.to_owned()),
        (None, None) => None,
    };
    RemoteUpdateRequest {
        label: requester
            .label
            .clone()
            .unwrap_or_else(|| "Another device".to_owned()),
        detail,
    }
}

pub struct DesktopRemoteUpdateDelegate<R: Runtime> {
    app: AppHandle<R>,
}

impl<R: Runtime> DesktopRemoteUpdateDelegate<R> {
    pub fn new(app: AppHandle<R>) -> Arc<Self> {
        Arc::new(Self { app })
    }
}

impl<R: Runtime> RemoteUpdateDelegate for DesktopRemoteUpdateDelegate<R> {
    fn status(&self) -> HostUpdaterFuture {
        let app = self.app.clone();
        Box::pin(async move {
            let state = app.state::<DesktopUpdateManager>().state(&app);
            map_desktop_update_state(&state)
        })
    }

    fn check(&self) -> HostUpdaterFuture {
        let app = self.app.clone();
        Box::pin(async move {
            let result = app
                .state::<DesktopUpdateManager>()
                .check_for_update(app.clone())
                .await;
            map_desktop_update_state(&result["state"])
        })
    }

    fn request_install(&self, requester: RemoteUpdateRequester) -> HostUpdaterFuture {
        let app = self.app.clone();
        Box::pin(async move {
            // Start the full host flow in the background and report the host's own
            // state. Claiming "installing" here would be a guess: the flow may still find
            // nothing to install or fail to download. Remote clients follow the flow
            // through `updater.status`, which they re-read while the host is busy.
            let updates = app.state::<DesktopUpdateManager>();
            if updates.begin_remote_request(&app, remote_update_request(&requester)) {
                tauri::async_runtime::spawn(run_remote_install(app.clone(), requester));
            }
            map_desktop_update_state(&updates.state(&app))
        })
    }
}

struct RemoteRequestGuard<R: Runtime>(AppHandle<R>);

impl<R: Runtime> Drop for RemoteRequestGuard<R> {
    fn drop(&mut self) {
        self.0
            .state::<DesktopUpdateManager>()
            .clear_remote_request(&self.0);
    }
}

/// Check, download, and install exactly as a local user would. Each early exit leaves
/// the host updater's own terminal state for `updater.status`: the check records
/// `up-to-date` when the feed has nothing newer and `error` when it fails, and the
/// download records `error` with its message. A step that is not admitted because
/// another check or download is running leaves that operation's state, which settles
/// by itself.
async fn run_remote_install<R: Runtime>(app: AppHandle<R>, requester: RemoteUpdateRequester) {
    let _request = RemoteRequestGuard(app.clone());
    let updates = app.state::<DesktopUpdateManager>();
    let state = updates.state(&app);
    let needs_download = state["downloadedVersion"].as_str().is_none();
    if needs_download {
        if state["availableVersion"].as_str().is_none() {
            let checked = updates.check_for_update(app.clone()).await;
            if checked["state"]["availableVersion"].as_str().is_none() {
                tracing::info!(
                    status = checked["state"]["status"].as_str().unwrap_or("unknown"),
                    "remote update install found no update to install"
                );
                return;
            }
        }
        let downloaded = updates.download_update(app.clone()).await;
        if downloaded["state"]["downloadedVersion"].as_str().is_none() {
            tracing::warn!(
                requester_label = requester.label.as_deref().unwrap_or("Another device"),
                requester_session = requester.session_id_prefix.as_deref().unwrap_or(""),
                status = downloaded["state"]["status"].as_str().unwrap_or("unknown"),
                message = downloaded["state"]["message"].as_str().unwrap_or(""),
                "remote update install stopped before the update was downloaded"
            );
            return;
        }
    }
    let backend = app.state::<BackendSupervisor>();
    // A successful install restarts the host on Linux and macOS, so a result that comes
    // back without `completed` is a refusal or a failure the host updater has recorded.
    let installed = updates
        .install_update(&app, backend.inner(), DesktopUpdateInstallInput::default())
        .await;
    if installed["completed"].as_bool() != Some(true) {
        updates.note_remote_secondary_protection_failure(&app);
        tracing::warn!(
            requester_label = requester.label.as_deref().unwrap_or("Another device"),
            requester_session = requester.session_id_prefix.as_deref().unwrap_or(""),
            accepted = installed["accepted"].as_bool().unwrap_or(false),
            status = installed["state"]["status"].as_str().unwrap_or("unknown"),
            message = installed["state"]["message"].as_str().unwrap_or(""),
            "remote update install did not complete"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use serde_json::json;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
        time::Duration,
    };
    use tauri::test::{MockRuntime, mock_builder, mock_context, noop_assets};

    /// The public half of the updater test key used by `updates.rs` tests. These tests
    /// never reach signature verification: their downloads fail before it.
    const TEST_PUBLIC_KEY: &str = "untrusted comment: minisign public key E7620F1842B4E81F\nRWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";

    #[derive(Clone, Copy)]
    enum Feed {
        /// The feed has nothing newer (HTTP 204).
        NoUpdate,
        /// The feed offers 99.0.0, but downloading it fails with HTTP 500.
        UpdateWithFailingDownload,
    }

    fn spawn_feed(feed: Feed) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("update feed should bind");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("update feed address")
        );
        let manifest = json!({
            "version": "99.0.0",
            "notes": "Remote install fixture",
            "pub_date": "2026-09-24T00:00:00Z",
            "url": format!("{base_url}/artifact"),
            "signature": "never verified: the download fails first",
        })
        .to_string();
        let requests = match feed {
            Feed::NoUpdate => 1,
            Feed::UpdateWithFailingDownload => 2,
        };
        let server = thread::spawn(move || {
            for _ in 0..requests {
                let (mut stream, _) = listener.accept().expect("feed request should arrive");
                let mut request = [0_u8; 2048];
                let read = stream.read(&mut request).expect("feed request should read");
                let request = String::from_utf8_lossy(&request[..read]);
                let response = match feed {
                    Feed::NoUpdate => "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_owned(),
                    Feed::UpdateWithFailingDownload if request.starts_with("GET /artifact ") => {
                        "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_owned()
                    }
                    Feed::UpdateWithFailingDownload => format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{manifest}",
                        manifest.len()
                    ),
                };
                stream
                    .write_all(response.as_bytes())
                    .expect("feed response should write");
            }
        });
        (base_url, server)
    }

    /// Holds the check open so the test can observe the request while its flow runs.
    fn spawn_gated_no_update_feed() -> (String, std::sync::mpsc::Sender<()>, thread::JoinHandle<()>)
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("update feed should bind");
        let base_url = format!("http://{}", listener.local_addr().expect("feed address"));
        let (release, released) = std::sync::mpsc::channel::<()>();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("feed request should arrive");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).expect("feed request should read");
            released.recv().expect("the test releases the feed");
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .expect("feed response should write");
        });
        (base_url, release, server)
    }

    fn host_app(feed_url: String) -> tauri::App<MockRuntime> {
        let mut context = mock_context(noop_assets());
        context.config_mut().plugins.0.insert(
            "updater".to_owned(),
            json!({
                "pubkey": STANDARD.encode(TEST_PUBLIC_KEY),
                "endpoints": [feed_url],
                "dangerousInsecureTransportProtocol": true,
                "windows": null,
            }),
        );
        mock_builder()
            .manage(DesktopUpdateManager::new())
            .manage(BackendSupervisor::new())
            .plugin(tauri_plugin_updater::Builder::new().build())
            .build(context)
            .expect("mock Tauri app")
    }

    /// Polls `status` the way a remote client does until the background flow settles.
    async fn settled_status<R: Runtime>(
        delegate: &DesktopRemoteUpdateDelegate<R>,
    ) -> HostUpdaterStatus {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let status = delegate.status().await;
                if matches!(
                    status.state,
                    RemoteUpdateState::UpToDate | RemoteUpdateState::Error
                ) {
                    return status;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the remote install flow should settle")
    }

    #[tokio::test]
    async fn a_remote_request_is_recorded_once_and_cleared_when_the_flow_ends() {
        let (base_url, release, feed) = spawn_gated_no_update_feed();
        let app = host_app(format!("{base_url}/latest.json"));
        let delegate = DesktopRemoteUpdateDelegate::new(app.handle().clone());
        let manager = app.state::<DesktopUpdateManager>();
        let requester = RemoteUpdateRequester {
            label: Some("Tablet".to_owned()),
            ..RemoteUpdateRequester::default()
        };

        delegate.request_install(requester.clone()).await;
        assert_eq!(
            manager.state(app.handle())["requestedBy"]["label"],
            "Tablet",
            "the host window sees who asked while the flow runs"
        );
        assert!(!manager.begin_remote_request(app.handle(), remote_update_request(&requester)));
        delegate
            .request_install(RemoteUpdateRequester {
                label: Some("Laptop".to_owned()),
                ..RemoteUpdateRequester::default()
            })
            .await;
        assert_eq!(
            manager.state(app.handle())["requestedBy"]["label"],
            "Tablet"
        );

        release.send(()).expect("release the feed");
        let settled = settled_status(&delegate).await;
        assert_eq!(settled.state, RemoteUpdateState::UpToDate);
        tokio::time::timeout(Duration::from_secs(1), async {
            while manager.state(app.handle())["requestedBy"] != Value::Null {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the request clears when the flow ends");
        feed.join().expect("update feed should stop");
    }

    #[test]
    fn requester_becomes_a_host_notice_with_a_generic_fallback() {
        let full = remote_update_request(&RemoteUpdateRequester {
            label: Some("Coddy Studio Desktop".to_owned()),
            os: Some("MacIntel".to_owned()),
            ip_address: Some("192.168.1.34".to_owned()),
            session_id_prefix: Some("0123abcd".to_owned()),
        });
        assert_eq!(full.label, "Coddy Studio Desktop");
        assert_eq!(full.detail.as_deref(), Some("MacIntel (192.168.1.34)"));

        let os_only = remote_update_request(&RemoteUpdateRequester {
            label: Some("Coddy Studio Web".to_owned()),
            os: Some("Linux x86_64".to_owned()),
            ..RemoteUpdateRequester::default()
        });
        assert_eq!(os_only.detail.as_deref(), Some("Linux x86_64"));

        let address_only = remote_update_request(&RemoteUpdateRequester {
            ip_address: Some("192.168.1.34".to_owned()),
            ..RemoteUpdateRequester::default()
        });
        assert_eq!(address_only.detail.as_deref(), Some("192.168.1.34"));

        let unknown = remote_update_request(&RemoteUpdateRequester::default());
        assert_eq!(unknown.label, "Another device");
        assert_eq!(unknown.detail, None);
    }

    #[tokio::test]
    async fn install_request_without_an_update_reports_the_host_state_then_up_to_date() {
        let (base_url, feed) = spawn_feed(Feed::NoUpdate);
        let app = host_app(format!("{base_url}/latest.json"));
        let delegate = DesktopRemoteUpdateDelegate::new(app.handle().clone());

        let requested = delegate
            .request_install(RemoteUpdateRequester::default())
            .await;
        assert!(
            matches!(
                requested.state,
                RemoteUpdateState::Idle | RemoteUpdateState::Checking
            ),
            "the reply must be the host's own state, not a guess: {requested:?}"
        );

        let settled = settled_status(&delegate).await;
        assert_eq!(settled.state, RemoteUpdateState::UpToDate);
        assert_eq!(settled.error, None);
        feed.join().expect("update feed should stop");
    }

    #[tokio::test]
    async fn install_request_records_a_failed_download_as_an_error_with_its_message() {
        let (base_url, feed) = spawn_feed(Feed::UpdateWithFailingDownload);
        let app = host_app(format!("{base_url}/latest.json"));
        let delegate = DesktopRemoteUpdateDelegate::new(app.handle().clone());

        let requested = delegate
            .request_install(RemoteUpdateRequester::default())
            .await;
        assert_ne!(
            requested.state,
            RemoteUpdateState::Installing,
            "nothing is installing before the update is even downloaded: {requested:?}"
        );

        let settled = settled_status(&delegate).await;
        assert_eq!(settled.state, RemoteUpdateState::Error);
        assert!(
            settled
                .error
                .as_deref()
                .is_some_and(|message| message.contains("500")),
            "the download failure should carry its message: {settled:?}"
        );
        assert_eq!(settled.latest_version.as_deref(), Some("99.0.0"));
        feed.join().expect("update feed should stop");
    }

    #[test]
    fn support_derivation_is_honest_about_the_updater() {
        if cfg!(debug_assertions) {
            let support = derive_remote_update_support(true);
            assert_eq!(support.install_mode, RemoteUpdateInstallMode::Manual);
            assert_eq!(support.reason, RemoteUpdateSupportReason::UnpackagedBuild);
        } else {
            let enabled = derive_remote_update_support(true);
            assert_eq!(enabled.install_mode, RemoteUpdateInstallMode::Interactive);
            assert_eq!(enabled.reason, RemoteUpdateSupportReason::Available);

            let disabled = derive_remote_update_support(false);
            assert_eq!(disabled.install_mode, RemoteUpdateInstallMode::Manual);
            assert_eq!(
                disabled.reason,
                RemoteUpdateSupportReason::UpdaterUnavailable
            );
        }
    }

    #[test]
    fn maps_every_desktop_updater_state_onto_the_wire_contract() {
        let cases = [
            (
                json!({"status": "idle", "phase": "idle"}),
                RemoteUpdateState::Idle,
            ),
            (
                json!({"status": "disabled", "phase": "idle"}),
                RemoteUpdateState::Idle,
            ),
            (
                json!({"status": "checking", "phase": "checking"}),
                RemoteUpdateState::Checking,
            ),
            (
                json!({"status": "up-to-date", "phase": "idle"}),
                RemoteUpdateState::UpToDate,
            ),
            (
                json!({"status": "available", "phase": "available", "availableVersion": "0.5.0"}),
                RemoteUpdateState::UpdateAvailable,
            ),
            (
                json!({"status": "downloading", "phase": "available", "availableVersion": "0.5.0"}),
                RemoteUpdateState::Downloading,
            ),
            (
                json!({"status": "downloaded", "phase": "available", "downloadedVersion": "0.5.0"}),
                RemoteUpdateState::UpdateAvailable,
            ),
            (
                json!({"status": "downloaded", "phase": "protecting", "downloadedVersion": "0.5.0"}),
                RemoteUpdateState::Installing,
            ),
            (
                json!({"status": "downloaded", "phase": "installing", "downloadedVersion": "0.5.0"}),
                RemoteUpdateState::Installing,
            ),
            (
                json!({"status": "error", "phase": "failed", "message": "boom"}),
                RemoteUpdateState::Error,
            ),
        ];
        for (state, expected) in cases {
            let mapped = map_desktop_update_state(&state);
            assert_eq!(mapped.state, expected, "for desktop state {state}");
        }

        let available = map_desktop_update_state(
            &json!({"status": "available", "phase": "available", "availableVersion": "0.5.0"}),
        );
        assert_eq!(available.latest_version.as_deref(), Some("0.5.0"));
        assert_eq!(available.error, None);

        let downloading = map_desktop_update_state(&json!({
            "status": "downloading", "phase": "available",
            "availableVersion": "0.6.4", "downloadPercent": 42.7
        }));
        assert_eq!(downloading.download_percent, Some(42));
        assert_eq!(downloading.target_version.as_deref(), Some("0.6.4"));
        assert_eq!(downloading.install_stage, None);

        for (percent, expected) in [(-1.0, 0), (0.0, 0), (100.0, 100), (101.0, 100)] {
            let downloading = map_desktop_update_state(&json!({
                "status": "downloading", "phase": "available", "downloadPercent": percent
            }));
            assert_eq!(downloading.download_percent, Some(expected));
        }

        let downloaded = map_desktop_update_state(&json!({
            "status": "downloaded", "phase": "available",
            "availableVersion": "0.6.4", "downloadedVersion": "0.6.4", "downloadPercent": 100.0
        }));
        assert_eq!(
            downloaded.download_percent, None,
            "percent only while downloading"
        );
        assert_eq!(downloaded.target_version.as_deref(), Some("0.6.4"));
        let available_first = map_desktop_update_state(&json!({
            "availableVersion": "0.6.4", "downloadedVersion": "0.6.3"
        }));
        assert_eq!(available_first.target_version.as_deref(), Some("0.6.4"));

        let protecting = map_desktop_update_state(&json!({
            "status": "downloaded", "phase": "protecting", "downloadedVersion": "0.6.4",
            "protection": [
                {"environmentId": "primary", "status": "protected", "stage": "stopping-backend"},
                {"environmentId": "wsl:Ubuntu", "status": "pending", "stage": "creating-verified-backup"}
            ]
        }));
        assert_eq!(protecting.target_version.as_deref(), Some("0.6.4"));
        assert_eq!(
            protecting.install_stage.as_deref(),
            Some("creating-verified-backup")
        );

        let installing = map_desktop_update_state(&json!({
            "status": "downloaded", "phase": "installing", "downloadedVersion": "0.6.4",
            "protection": []
        }));
        assert_eq!(installing.install_stage.as_deref(), Some("installing"));

        let failed = map_desktop_update_state(
            &json!({"status": "error", "phase": "failed", "message": "boom"}),
        );
        assert_eq!(failed.error.as_deref(), Some("boom"));
    }
}
