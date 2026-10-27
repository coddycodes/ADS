use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};

use coddy_studio_server::cloud::{RelayClientInstallEvent, RelayClientStatus};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

use coddy_studio_server::production::relay::{
    CLOUDFLARED_VERSION, RelayClientOptions, RelayReleaseArchive, RelayReleaseAsset,
    relay_client_service, relay_client_service_with_options,
};

#[tokio::test]
async fn native_constructor_returns_a_resolvable_service() {
    let temp = TempDir::new().expect("temp dir");
    let status = relay_client_service(temp.path()).resolve().await;
    assert_eq!(status_version(&status), CLOUDFLARED_VERSION);
}

#[tokio::test]
async fn resolve_prefers_override_then_managed_then_path() {
    let temp = TempDir::new().expect("temp dir");
    let override_path = copy_native_fixture(temp.path().join(executable_name())).await;
    let path_dir = temp.path().join("path-bin");
    tokio::fs::create_dir_all(&path_dir)
        .await
        .expect("path dir");
    copy_native_fixture(path_dir.join(executable_name())).await;

    let options = test_options(temp.path(), None).with_executable_override(&override_path);
    let service = relay_client_service_with_options(options);
    assert_eq!(
        service.resolve().await,
        RelayClientStatus::Available {
            executable_path: override_path.to_string_lossy().into_owned(),
            source: "override".to_owned(),
            version: CLOUDFLARED_VERSION.to_owned(),
        }
    );

    tokio::fs::remove_file(&override_path)
        .await
        .expect("remove override");
    assert_eq!(
        service.resolve().await,
        RelayClientStatus::Missing {
            version: CLOUDFLARED_VERSION.to_owned(),
        },
        "an explicitly configured but missing executable must not fall through"
    );

    let options = test_options(temp.path(), None).with_search_path(OsString::from(&path_dir));
    let service = relay_client_service_with_options(options.clone());
    assert!(matches!(
        service.resolve().await,
        RelayClientStatus::Available { source, .. } if source == "path"
    ));

    let managed = options.managed_executable_path();
    tokio::fs::create_dir_all(managed.parent().expect("managed parent"))
        .await
        .expect("managed dir");
    copy_native_fixture(&managed).await;
    assert!(matches!(
        service.resolve().await,
        RelayClientStatus::Available { source, executable_path, .. }
            if source == "managed" && executable_path == managed.to_string_lossy()
    ));
}

#[tokio::test]
async fn resolve_reports_unsupported_platform_without_a_release_asset() {
    let temp = TempDir::new().expect("temp dir");
    let service =
        relay_client_service_with_options(RelayClientOptions::new(temp.path(), "plan9", "mips128"));

    assert_eq!(
        service.resolve().await,
        RelayClientStatus::Unsupported {
            platform: "plan9".to_owned(),
            arch: "mips128".to_owned(),
            version: CLOUDFLARED_VERSION.to_owned(),
        }
    );
}

#[tokio::test]
async fn install_downloads_verifies_validates_and_atomically_activates_binary() {
    let temp = TempDir::new().expect("temp dir");
    let fixture = tokio::fs::read(native_fixture())
        .await
        .expect("fixture bytes");
    let checksum = hex_sha256(&fixture);
    let (url, server) = serve_once(fixture).await;
    let asset = RelayReleaseAsset::new(url, checksum, RelayReleaseArchive::Binary);
    let options = test_options(temp.path(), Some(asset));
    let managed_path = options.managed_executable_path();
    let service = relay_client_service_with_options(options);

    let events = service.install().await.expect("relay install");
    server.await.expect("server task");

    let stages = events
        .iter()
        .filter_map(|event| match event {
            RelayClientInstallEvent::Progress { stage } => Some(stage.as_str()),
            RelayClientInstallEvent::Complete { .. } => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        stages,
        [
            "checking",
            "waiting_for_lock",
            "downloading",
            "verifying",
            "installing",
            "validating",
            "activating",
        ]
    );
    assert!(matches!(
        events.last(),
        Some(RelayClientInstallEvent::Complete {
            status: RelayClientStatus::Available { source, executable_path, .. }
        }) if source == "managed" && executable_path == managed_path.to_string_lossy().as_ref()
    ));
    assert!(managed_path.is_file());
    assert!(
        directory_entries(managed_path.parent().expect("managed parent"))
            .await
            .iter()
            .all(|path| !path.to_string_lossy().contains(".install-")
                && !path.to_string_lossy().ends_with(".lock"))
    );
}

#[tokio::test]
async fn install_rejects_checksum_mismatch_without_activating_download() {
    let temp = TempDir::new().expect("temp dir");
    let (url, server) = serve_once(b"not a trusted executable".to_vec()).await;
    let asset = RelayReleaseAsset::new(url, "00".repeat(32), RelayReleaseArchive::Binary);
    let options = test_options(temp.path(), Some(asset));
    let managed_path = options.managed_executable_path();
    let service = relay_client_service_with_options(options);

    let error = service.install().await.expect_err("checksum must fail");
    server.await.expect("server task");

    assert!(
        error.contains("invalid_checksum"),
        "unexpected error: {error}"
    );
    assert!(!managed_path.exists());
}

fn test_options(base_dir: &Path, asset: Option<RelayReleaseAsset>) -> RelayClientOptions {
    RelayClientOptions::new(base_dir, platform(), arch())
        .with_release_asset(asset)
        .with_search_path(OsString::new())
        .with_download_timeout(Duration::from_secs(5))
}

fn platform() -> &'static str {
    if cfg!(windows) {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    }
}

fn arch() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        std::env::consts::ARCH
    }
}

fn executable_name() -> &'static str {
    if cfg!(windows) {
        "cloudflared.exe"
    } else {
        "cloudflared"
    }
}

fn native_fixture() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot")).join("System32/curl.exe")
    } else if cfg!(target_os = "macos") {
        PathBuf::from("/usr/bin/true")
    } else {
        PathBuf::from("/bin/true")
    }
}

async fn copy_native_fixture(destination: impl AsRef<Path>) -> PathBuf {
    let destination = destination.as_ref();
    tokio::fs::copy(native_fixture(), destination)
        .await
        .expect("copy fixture");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o755))
            .await
            .expect("fixture permissions");
    }
    destination.to_owned()
}

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

async fn serve_once(body: Vec<u8>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind server");
    let address = listener.local_addr().expect("server address");
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept request");
        let mut request = [0_u8; 2048];
        let _ = socket.read(&mut request).await.expect("read request");
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        socket.write_all(headers.as_bytes()).await.expect("headers");
        socket.write_all(&body).await.expect("body");
    });
    (format!("http://{address}/cloudflared"), task)
}

async fn directory_entries(directory: &Path) -> Vec<PathBuf> {
    let mut entries = tokio::fs::read_dir(directory)
        .await
        .expect("read directory");
    let mut paths = Vec::new();
    while let Some(entry) = entries.next_entry().await.expect("next entry") {
        paths.push(entry.path());
    }
    paths
}

fn status_version(status: &RelayClientStatus) -> &str {
    match status {
        RelayClientStatus::Available { version, .. }
        | RelayClientStatus::Missing { version }
        | RelayClientStatus::Unsupported { version, .. } => version,
    }
}
