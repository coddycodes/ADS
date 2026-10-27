//! Development-only live-check host for remote server updates. Not shipped.
//!
//! Usage: cargo run -p coddy-studio-server --example remote_update_fake_host -- \
//!   <base-dir> <port 4800-4899> <server-version> [label, default "Fake-host"]
//! Always binds 127.0.0.1. Pair separately with:
//!   coddy-studio pairing offer --base-dir <base-dir> --endpoint http://127.0.0.1:<port> \
//!     --reach this-computer --json
//! Prints {"event":"started","port":...,"serverVersion":...,"bootId":...} per start.
//! Commands on stdin, one JSON object per line:
//!   {"status":{"state":"downloading","latestVersion":"9.9.1","targetVersion":"9.9.1",
//!              "downloadPercent":42,"installStage":null,"error":null}}
//!   {"restart":{"serverVersion":"9.9.1","afterMs":2000}}
//!   {"stop":true}  // stop serving permanently, but keep reading stdin
//!   {"exit":true}  // shut down and exit; EOF/Ctrl-C do the same
//! Input is bounded to 16 KiB per command; restart delay is at most ten minutes.
//! Windows controls must use piped stdin or a regular input file.

use std::{
    env,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use coddy_studio_server::{
    ENVIRONMENT_DESCRIPTOR_PATH, HostUpdaterFuture, HostUpdaterStatus, RemoteUpdateDelegate,
    RemoteUpdateInstallKind, RemoteUpdateInstallMode, RemoteUpdateRequester, RemoteUpdateState,
    RemoteUpdateSupport, RemoteUpdateSupportReason, ServerConfig, ServerHandle, ServerRuntime,
    diagnostics::UnavailableDesktopUiProcessObserver, resolve_data_root,
};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;
#[path = "../tests/support/hermetic_providers.rs"]
mod hermetic_providers;

#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ScriptedStatus {
    state: Option<RemoteUpdateState>,
    latest_version: Option<String>,
    target_version: Option<String>,
    download_percent: Option<u8>,
    install_stage: Option<String>,
    error: Option<String>,
}

struct ScriptedDelegate(Arc<Mutex<ScriptedStatus>>);

impl ScriptedDelegate {
    fn current(&self) -> HostUpdaterStatus {
        let status = self.0.lock().expect("scripted status").clone();
        let state = status.state.unwrap_or_default();
        HostUpdaterStatus {
            latest_version: status.latest_version,
            state,
            error: status.error,
            download_percent: status.download_percent,
            target_version: status.target_version,
            install_stage: status.install_stage,
        }
    }
}

impl RemoteUpdateDelegate for ScriptedDelegate {
    fn status(&self) -> HostUpdaterFuture {
        let status = self.current();
        Box::pin(async move { status })
    }

    fn check(&self) -> HostUpdaterFuture {
        self.status()
    }

    fn request_install(&self, requester: RemoteUpdateRequester) -> HostUpdaterFuture {
        eprintln!(
            "install requested label={} session={}",
            requester
                .label
                .as_deref()
                .unwrap_or("unknown")
                .replace(['\r', '\n'], " "),
            requester
                .session_id_prefix
                .as_deref()
                .unwrap_or("none")
                .replace(['\r', '\n'], " "),
        );
        self.status()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
enum Command {
    Status(ScriptedStatus),
    Restart(Restart),
    Stop(bool),
    Exit(bool),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Restart {
    server_version: String,
    #[serde(default = "default_restart_delay")]
    after_ms: u64,
}

impl ScriptedStatus {
    fn validate(&self) -> io::Result<()> {
        if self.download_percent.is_some_and(|percent| percent > 100) {
            return Err(io::Error::other("downloadPercent must be 0-100"));
        }
        Ok(())
    }
}
impl Command {
    fn validate(&self) -> io::Result<()> {
        match self {
            Self::Status(status) => status.validate(),
            Self::Restart(restart)
                if restart.server_version.is_empty()
                    || restart.server_version.trim() != restart.server_version
                    || restart.after_ms > 600_000 =>
            {
                Err(io::Error::other("invalid restart version or delay"))
            }
            Self::Stop(false) | Self::Exit(false) => {
                Err(io::Error::other("stop and exit require true"))
            }
            _ => Ok(()),
        }
    }
}
fn validate_fake_host_config(config: &ServerConfig) -> io::Result<()> {
    if !config
        .host
        .parse::<std::net::IpAddr>()
        .is_ok_and(|host| host.is_loopback())
        || !(4800..=4899).contains(&config.port)
    {
        return Err(io::Error::other(
            "fake host requires loopback and a port in 4800-4899; never 3773",
        ));
    }
    Ok(())
}

fn default_restart_delay() -> u64 {
    2_000
}

const MAX_COMMAND_BYTES: usize = 16 * 1024;
const INPUT_POLL: Duration = Duration::from_millis(50);

/// The reader owns only its duplicated stdin descriptor. Polling never waits
/// for a complete line, so a partial command cannot strand shutdown.
struct CommandInput {
    #[cfg(unix)]
    file: std::fs::File,
    #[cfg(windows)]
    handle: usize,
}
impl CommandInput {
    fn stdin() -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::fd::FromRawFd;
            // SAFETY: dup creates an owned descriptor; the File closes it.
            let descriptor = unsafe { libc::dup(libc::STDIN_FILENO) };
            if descriptor < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                file: unsafe { std::fs::File::from_raw_fd(descriptor) },
            })
        }
        #[cfg(windows)]
        {
            // SAFETY: stdin is borrowed for the process lifetime.
            let handle = unsafe { windows_input::get_std_handle(-10_i32 as u32) };
            if handle.is_null() || handle as isize == -1 {
                return Err(io::Error::last_os_error());
            }
            let kind = unsafe { windows_input::get_file_type(handle) };
            if !matches!(kind, 1 | 3) {
                return Err(io::Error::other("pipe JSON controls to stdin on Windows"));
            }
            Ok(Self {
                handle: handle as usize,
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(io::Error::other(
                "stdin controls are unsupported on this platform",
            ))
        }
    }
    fn read_ready(&mut self, bytes: &mut [u8]) -> io::Result<Option<usize>> {
        #[cfg(unix)]
        {
            use std::{io::Read, os::fd::AsRawFd};
            let mut descriptor = libc::pollfd {
                fd: self.file.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: poll borrows one valid descriptor. This reader is its only consumer.
            let ready = unsafe { libc::poll(&mut descriptor, 1, 50) };
            if ready < 0 {
                let error = io::Error::last_os_error();
                return if error.kind() == io::ErrorKind::Interrupted {
                    Ok(None)
                } else {
                    Err(error)
                };
            }
            if ready == 0 {
                return Ok(None);
            }
            if descriptor.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
                return Err(io::Error::other("stdin descriptor failed"));
            }
            self.file.read(bytes).map(Some)
        }
        #[cfg(windows)]
        {
            let handle = self.handle as *mut std::ffi::c_void;
            let kind = unsafe { windows_input::get_file_type(handle) };
            let mut limit = bytes.len() as u32;
            if kind == 3 {
                let mut available = 0;
                // SAFETY: the borrowed pipe handle and output pointer are valid.
                let ready = unsafe {
                    windows_input::peek_named_pipe(
                        handle,
                        std::ptr::null_mut(),
                        0,
                        std::ptr::null_mut(),
                        &mut available,
                        std::ptr::null_mut(),
                    )
                };
                if ready == 0 {
                    let error = io::Error::last_os_error();
                    return if error.raw_os_error() == Some(109) {
                        Ok(Some(0))
                    } else {
                        Err(error)
                    };
                }
                limit = limit.min(available);
                if available == 0 {
                    std::thread::sleep(INPUT_POLL);
                    return Ok(None);
                }
            }
            let mut received = 0;
            // SAFETY: the caller's initialized byte slice is writable for its full length.
            let read = unsafe {
                windows_input::read_file(
                    handle,
                    bytes.as_mut_ptr().cast(),
                    limit,
                    &mut received,
                    std::ptr::null_mut(),
                )
            };
            if read == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Some(received as usize))
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = bytes;
            Err(io::Error::other("stdin controls are unsupported"))
        }
    }
}
#[cfg(windows)]
mod windows_input {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        #[link_name = "GetStdHandle"]
        pub fn get_std_handle(kind: u32) -> *mut c_void;
        #[link_name = "GetFileType"]
        pub fn get_file_type(handle: *mut c_void) -> u32;
        #[link_name = "PeekNamedPipe"]
        pub fn peek_named_pipe(
            handle: *mut c_void,
            buffer: *mut c_void,
            size: u32,
            read: *mut u32,
            available: *mut u32,
            left: *mut u32,
        ) -> i32;
        #[link_name = "ReadFile"]
        pub fn read_file(
            handle: *mut c_void,
            buffer: *mut c_void,
            size: u32,
            read: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
    }
}
struct CommandReader {
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl CommandReader {
    fn start(mut input: CommandInput) -> (Self, mpsc::Receiver<Command>) {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (sender, receiver) = mpsc::channel(32);
        let stop = Arc::new(AtomicBool::new(false));
        let reading_stop = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut chunk = [0_u8; 4096];
            let mut line = Vec::new();
            let mut oversized = false;
            while !reading_stop.load(Ordering::Acquire) {
                let read = match input.read_ready(&mut chunk) {
                    Ok(None) => continue,
                    Ok(Some(read)) => read,
                    Err(_) => {
                        eprintln!("could not read stdin controls");
                        break;
                    }
                };
                if read == 0 {
                    if !line.is_empty() && !oversized {
                        Self::send_line(&sender, &reading_stop, &line);
                    }
                    break;
                }
                for byte in &chunk[..read] {
                    if *byte == b'\n' {
                        if !oversized && !Self::send_line(&sender, &reading_stop, &line) {
                            return;
                        }
                        line.clear();
                        oversized = false;
                    } else if !oversized {
                        if line.len() == MAX_COMMAND_BYTES {
                            eprintln!("ignoring oversized stdin command");
                            line.clear();
                            oversized = true;
                        } else {
                            line.push(*byte);
                        }
                    }
                }
            }
        });
        (
            Self {
                stop,
                thread: Some(thread),
            },
            receiver,
        )
    }
    fn send_line(
        sender: &mpsc::Sender<Command>,
        stop: &std::sync::atomic::AtomicBool,
        line: &[u8],
    ) -> bool {
        use std::sync::atomic::Ordering;
        if line.iter().all(u8::is_ascii_whitespace) {
            return true;
        }
        let mut command = match serde_json::from_slice::<Command>(line) {
            Ok(command) if command.validate().is_ok() => command,
            _ => {
                eprintln!("ignoring malformed stdin command");
                return true;
            }
        };
        while !stop.load(Ordering::Acquire) {
            match sender.try_send(command) {
                Ok(()) => return true,
                Err(mpsc::error::TrySendError::Closed(_)) => return false,
                Err(mpsc::error::TrySendError::Full(pending)) => {
                    command = pending;
                    std::thread::sleep(INPUT_POLL);
                }
            }
        }
        false
    }
    async fn shutdown(&mut self) -> io::Result<()> {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(thread) = self.thread.take() {
            tokio::task::spawn_blocking(move || thread.join())
                .await
                .map_err(io::Error::other)?
                .map_err(|_| io::Error::other("command reader failed"))?;
        }
        Ok(())
    }
}
impl Drop for CommandReader {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn disable_provider_processes(base_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let settings_dir = base_dir.join("userdata");
    fs::create_dir_all(&settings_dir)?;
    let file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(settings_dir.join("settings.json"))
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let settings: serde_json::Value =
                serde_json::from_slice(&fs::read(settings_dir.join("settings.json"))?)?;
            let missing = hermetic_providers::missing_provider_executable(&settings_dir);
            let pin = missing.to_str().ok_or("fixture path must be UTF-8")?;
            let safe_provider = |provider: &serde_json::Value| {
                provider["enabled"] == false && provider["binaryPath"] == pin
            };
            let legacy_safe = hermetic_providers::BUILTIN_PROVIDER_DRIVERS
                .iter()
                .all(|driver| safe_provider(&settings["providers"][*driver]));
            let instances_safe = settings.get("providerInstances").is_none_or(|instances| {
                instances.as_object().is_some_and(|instances| {
                    instances.values().all(|instance| {
                        instance["enabled"] == false && instance["config"]["binaryPath"] == pin
                    })
                })
            });
            if settings["enableProviderUpdateChecks"] != false || !legacy_safe || !instances_safe {
                return Err("Use a new private base directory: fixture settings must disable providers/update checks and pin absent executables".into());
            }
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    let mut settings = hermetic_providers::hermetic_provider_settings(&settings_dir);
    for provider in settings["providers"]
        .as_object_mut()
        .expect("fixture providers")
        .values_mut()
    {
        provider["enabled"] = json!(false);
    }
    serde_json::to_writer(file, &settings)?;
    Ok(())
}

async fn report_started(handle: &ServerHandle) -> Result<(), Box<dyn std::error::Error>> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Descriptor {
        server_version: String,
        boot_id: String,
    }

    let descriptor: Descriptor = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()?
        .get(format!(
            "http://{}{ENVIRONMENT_DESCRIPTOR_PATH}",
            handle.local_addr()
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let mut stdout = io::stdout().lock();
    writeln!(
        stdout,
        "{}",
        json!({
            "event": "started",
            "port": handle.local_addr().port(),
            "serverVersion": descriptor.server_version,
            "bootId": descriptor.boot_id,
        })
    )?;
    stdout.flush()?;
    Ok(())
}

async fn start_server(
    base_dir: &Path,
    port: u16,
    version: &str,
    label: &str,
    status: Arc<Mutex<ScriptedStatus>>,
) -> Result<ServerHandle, Box<dyn std::error::Error>> {
    let mut config = ServerConfig::new(base_dir)
        .with_bind("127.0.0.1", port)
        .with_remote_update_support(RemoteUpdateSupport {
            install_mode: RemoteUpdateInstallMode::Interactive,
            reason: RemoteUpdateSupportReason::Available,
            install_kind: RemoteUpdateInstallKind::Unknown,
        });
    validate_fake_host_config(&config)?;
    config.server_version = version.to_owned();
    config.environment_label = label.to_owned();
    config.no_browser = true;
    config.startup_pairing_offer = false;
    let handle = ServerRuntime::start_with_desktop_integration(
        config,
        Arc::new(UnavailableDesktopUiProcessObserver),
        Arc::new(ScriptedDelegate(status)),
    )
    .await?;
    if let Err(error) = report_started(&handle).await {
        handle.shutdown();
        handle.join().await?;
        return Err(error);
    }
    Ok(handle)
}

async fn shutdown(
    handle: &mut Option<ServerHandle>,
) -> Result<(), coddy_studio_server::ServerError> {
    if let Some(handle) = handle.take() {
        handle.shutdown();
        handle.join().await?;
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let base_dir = args.next().ok_or("missing base-dir argument")?;
    let port = args
        .next()
        .ok_or("missing port argument (expected 4800-4899)")?
        .parse::<u16>()
        .ok()
        .filter(|port| (4800..=4899).contains(port))
        .ok_or("port must be in 4800-4899; port 3773 is forbidden")?;
    let mut version = args.next().ok_or("missing server-version argument")?;
    let label = args.next().unwrap_or_else(|| "Fake-host".to_owned());
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    let data_root = resolve_data_root(ServerConfig::new(base_dir).data_root_request)?;
    let base_dir = data_root.effective.as_path();
    disable_provider_processes(base_dir)?;

    let status = Arc::new(Mutex::new(ScriptedStatus {
        state: Some(RemoteUpdateState::UpdateAvailable),
        latest_version: Some("9.9.1".to_owned()),
        target_version: Some("9.9.1".to_owned()),
        ..ScriptedStatus::default()
    }));
    let (mut reader, mut commands) = CommandReader::start(CommandInput::stdin()?);
    let mut handle = None;
    let result: Result<(), Box<dyn std::error::Error>> = async {
    handle = Some(start_server(base_dir, port, &version, &label, status.clone()).await?);
    let interrupt = tokio::signal::ctrl_c();
    tokio::pin!(interrupt);
    let mut restart_at = None;
    loop {
        let command = if let Some(deadline) = restart_at {
            tokio::select! {
                signal = &mut interrupt => { signal?; break; },
                command = commands.recv() => command,
                () = tokio::time::sleep_until(deadline) => {
                    *status.lock().expect("scripted status") = ScriptedStatus::default();
                    handle = Some(start_server(base_dir, port, &version, &label, status.clone()).await?);
                    restart_at = None;
                    continue;
                }
            }
        } else {
            tokio::select! { signal = &mut interrupt => { signal?; break; }, command = commands.recv() => command }
        };
        match command {
            None | Some(Command::Exit(true)) => break,
            Some(Command::Stop(false) | Command::Exit(false)) => {
                eprintln!("ignoring malformed stdin command: stop and exit require true");
            }
            Some(_) if handle.is_none() && restart_at.is_none() => {
                eprintln!("host is stopped; ignoring command");
            }
            Some(Command::Status(update)) => {
                *status.lock().expect("scripted status") = update;
            }
            Some(Command::Restart(restart)) => {
                shutdown(&mut handle).await?;
                version = restart.server_version;
                restart_at =
                    Some(tokio::time::Instant::now() + Duration::from_millis(restart.after_ms));
            }
            Some(Command::Stop(true)) => {
                shutdown(&mut handle).await?;
                restart_at = None;
            }
        }
    }
    Ok(())
    }.await;
    drop(commands);
    reader.shutdown().await?;
    shutdown(&mut handle).await?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixture_settings_disable_checks_and_pin_every_provider_without_overwriting_existing_work() {
        let temp = tempfile::tempdir().unwrap();
        disable_provider_processes(temp.path()).unwrap();
        let path = temp.path().join("userdata/settings.json");
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["enableProviderUpdateChecks"], false);
        for driver in ["codex", "claudeAgent", "cursor", "grok", "opencode"] {
            assert_eq!(value["providers"][driver]["enabled"], false);
            let pin = Path::new(value["providers"][driver]["binaryPath"].as_str().unwrap());
            assert!(pin.is_absolute());
            assert!(!pin.exists());
        }
        let unsafe_settings = b"{\"enableProviderUpdateChecks\":true}";
        fs::write(&path, unsafe_settings).unwrap();
        assert!(disable_provider_processes(temp.path()).is_err());
        assert_eq!(fs::read(path).unwrap(), unsafe_settings);
    }

    #[test]
    fn fixture_settings_accept_disabled_instances_with_their_configured_pin() {
        let temp = tempfile::tempdir().unwrap();
        disable_provider_processes(temp.path()).unwrap();
        let path = temp.path().join("userdata/settings.json");
        let mut settings: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let pin = settings["providers"]["codex"]["binaryPath"].clone();
        settings["providerInstances"] = json!({
            "controlled-codex": { "driver": "codex", "enabled": false, "config": { "binaryPath": pin } }
        });
        let bytes = serde_json::to_vec(&settings).unwrap();
        fs::write(&path, &bytes).unwrap();
        disable_provider_processes(temp.path()).unwrap();
        assert_eq!(fs::read(path).unwrap(), bytes);
    }

    #[test]
    fn fixture_settings_reject_a_safe_shadow_field_over_an_ambient_instance_binary() {
        let temp = tempfile::tempdir().unwrap();
        disable_provider_processes(temp.path()).unwrap();
        let path = temp.path().join("userdata/settings.json");
        let mut settings: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let pin = settings["providers"]["codex"]["binaryPath"].clone();
        settings["providerInstances"] = json!({
            "controlled-codex": {
                "driver": "codex", "enabled": false, "binaryPath": pin,
                "config": { "binaryPath": "codex" }
            }
        });
        let bytes = serde_json::to_vec(&settings).unwrap();
        fs::write(&path, &bytes).unwrap();
        assert!(disable_provider_processes(temp.path()).is_err());
        assert_eq!(fs::read(path).unwrap(), bytes);
    }

    #[test]
    fn control_protocol_refuses_unknown_state_and_invalid_percent() {
        assert!(serde_json::from_str::<Command>(r#"{"status":{"state":"bogus"}}"#).is_err());
        let command: Command =
            serde_json::from_str(r#"{"status":{"state":"downloading","downloadPercent":101}}"#)
                .unwrap();
        let Command::Status(status) = command else {
            panic!("status");
        };
        assert!(status.validate().is_err());
    }
    #[test]
    fn fake_host_config_only_allows_loopback_in_its_reserved_range() {
        for host in ["0.0.0.0", "192.168.1.10", "example.com"] {
            assert!(
                validate_fake_host_config(&ServerConfig::new("unused").with_bind(host, 4888))
                    .is_err()
            );
        }
        for port in [0, 3773, 4799, 4900, 14802] {
            assert!(
                validate_fake_host_config(
                    &ServerConfig::new("unused").with_bind("127.0.0.1", port)
                )
                .is_err()
            );
        }
        for port in [4800, 4888, 4899] {
            assert!(
                validate_fake_host_config(
                    &ServerConfig::new("unused").with_bind("127.0.0.1", port)
                )
                .is_ok()
            );
        }
    }
    #[cfg(unix)]
    fn test_reader() -> (
        CommandReader,
        mpsc::Receiver<Command>,
        std::os::unix::net::UnixStream,
    ) {
        use std::os::fd::{FromRawFd, IntoRawFd};
        let (input, writer) = std::os::unix::net::UnixStream::pair().unwrap();
        // SAFETY: ownership transfers from the UnixStream into its File wrapper.
        let input = CommandInput {
            file: unsafe { std::fs::File::from_raw_fd(input.into_raw_fd()) },
        };
        let (reader, commands) = CommandReader::start(input);
        (reader, commands, writer)
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn partial_line_and_full_command_queue_cannot_strand_reader_shutdown() {
        let (mut reader, mut commands, mut writer) = test_reader();
        writer.write_all(b"{\"status\":").unwrap();
        tokio::time::sleep(Duration::from_millis(70)).await;
        tokio::time::timeout(Duration::from_secs(1), reader.shutdown())
            .await
            .unwrap()
            .unwrap();
        assert!(commands.recv().await.is_none());
        let (mut reader, _commands, mut writer) = test_reader();
        writer
            .write_all(b"{\"exit\":true}\n".repeat(80).as_slice())
            .unwrap();
        tokio::time::sleep(Duration::from_millis(70)).await;
        tokio::time::timeout(Duration::from_secs(1), reader.shutdown())
            .await
            .unwrap()
            .unwrap();
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn eof_delivers_final_command_and_oversized_input_recovers_on_next_line() {
        let (mut reader, mut commands, mut writer) = test_reader();
        writer
            .write_all(&vec![b'x'; MAX_COMMAND_BYTES + 1])
            .unwrap();
        writer.write_all(b"\n{\"exit\":true}").unwrap();
        drop(writer);
        let command = tokio::time::timeout(Duration::from_secs(2), commands.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(command, Command::Exit(true)));
        assert!(commands.recv().await.is_none());
        reader.shutdown().await.unwrap();
    }
}
