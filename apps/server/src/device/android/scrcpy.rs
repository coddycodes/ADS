//! The pinned scrcpy-server: push it, run it, forward its sockets, and read its H.264 units.
//!
//! Wire format of scrcpy v4.1 (at tag `v4.1`: `app/src/demuxer.c` lines 14-17 and 73-119,
//! `server/src/main/java/com/genymobile/scrcpy/device/Streamer.java` lines 48-65 and 91-121,
//! `control/ControlMessage.java` line 27; native verification item 4). The video socket
//! carries one dummy byte (tunnel forward mode), a 4-byte codec id, a 12-byte session header,
//! then 12-byte packet headers.
//! A header whose bit 63 is set is a session packet (no payload; bytes 4..8 width, 8..12
//! height), sent again when the display size changes. Otherwise bit 62 marks a config
//! packet, bit 61 a key frame, the low 61 bits are the PTS in microseconds, and a 4-byte
//! length precedes the payload.

use std::{
    net::Ipv4Addr,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, OnceLock},
    time::Duration,
};

use process_wrap::tokio::CommandWrap;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::mpsc,
    task::AbortHandle,
    time::Instant,
};
use tokio_util::sync::CancellationToken;

use crate::{
    device::{
        DeviceError,
        android::orientation::DisplayHandle,
        stderr_tail::{STDERR_DRAIN_GRACE, StderrTail},
        video::{UnitKind, VideoUnit},
    },
    diagnostics::{ProcessAttributionRegistry, profiler::exits},
    process::{
        HelperSpawnError, RegisteredHelper, configure_supervised_background_command_wrap,
        spawn_registered_helper,
        supervised::{SupervisedOverflow, SupervisedRunRequest, run_supervised},
    },
};

pub(crate) const SCRCPY_VERSION: &str = "4.1";
pub(crate) const SCRCPY_SERVER_JAR: &[u8] = include_bytes!("scrcpy/scrcpy-server-v4.1");
pub(crate) const SCRCPY_SERVER_SHA256: &str =
    "deacb991ed2509715160ffdc7907e47b4160eb30d1566217e9047fd5b8850cae";

pub(crate) const HELPER_LABEL: &str = "android-scrcpy";
const REMOTE_JAR: &str = "/data/local/tmp/bibcode-scrcpy-server-v4.1.jar";
const CODEC_H264: u32 = 0x6832_3634;
/// scrcpy writes these in place of a codec id when it disables the stream or fails to
/// configure it.
const CODEC_DISABLED: u32 = 0;
const CODEC_ERROR: u32 = 1;
/// Larger than any display scrcpy is asked to encode (`max_size=1280`).
const MAX_DIMENSION: u32 = 16_384;
/// Far above one unit at `max_size=1280` and 4 Mbit/s; a longer length means a garbled or
/// hostile stream, which is refused before anything is allocated.
const MAX_PACKET_BYTES: u32 = 8 * 1024 * 1024;
const PACKET_FLAG_SESSION: u64 = 1 << 63;
const PACKET_FLAG_CONFIG: u64 = 1 << 62;
const PACKET_FLAG_KEY_FRAME: u64 = 1 << 61;
const PACKET_PTS_MASK: u64 = PACKET_FLAG_KEY_FRAME - 1;
/// `ControlMessage.TYPE_RESET_VIDEO`: the server restarts the encoder, which sends a fresh
/// config and key unit.
const TYPE_RESET_VIDEO: u8 = 17;
const UNIT_QUEUE: usize = 64;
/// How long scrcpy-server may take to listen behind the forward, retried this often.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_RETRY: Duration = Duration::from_millis(100);
/// How long the codec and session headers may take once both sockets are connected.
const HEADER_TIMEOUT: Duration = Duration::from_secs(10);
const ADB_TIMEOUT: Duration = Duration::from_secs(10);
const ADB_PUSH_TIMEOUT: Duration = Duration::from_secs(60);
const ADB_CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);
const ADB_MAX_OUTPUT_BYTES: usize = 4 * 1024;
/// How long a server whose stream broke may take to be seen exiting before a stop.
const SERVER_EXIT_GRACE: Duration = Duration::from_secs(1);
const SERVER_EXIT_POLL: Duration = Duration::from_millis(50);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CodecInfo {
    pub codec_id: u32,
    pub width: u32,
    pub height: u32,
}

/// The stream's codec id and its first session header; only H.264 with a sane size passes.
pub(crate) fn parse_codec_header(
    codec_id: [u8; 4],
    session: [u8; 12],
) -> Result<CodecInfo, DeviceError> {
    let codec_id = u32::from_be_bytes(codec_id);
    match codec_id {
        CODEC_H264 => {}
        CODEC_DISABLED => {
            return Err(DeviceError::Protocol(
                "scrcpy-server disabled the video stream".to_owned(),
            ));
        }
        CODEC_ERROR => {
            return Err(DeviceError::Protocol(
                "scrcpy-server could not configure the video encoder".to_owned(),
            ));
        }
        other => {
            return Err(DeviceError::Protocol(format!(
                "scrcpy-server sent codec {other:#010x}, not H.264"
            )));
        }
    }
    let (width, height) = session_dimensions(&session)
        .ok_or_else(|| DeviceError::Protocol("scrcpy-server sent no session header".to_owned()))?;
    if !(1..=MAX_DIMENSION).contains(&width) || !(1..=MAX_DIMENSION).contains(&height) {
        return Err(DeviceError::Protocol(format!(
            "scrcpy-server reported a {width}x{height} display"
        )));
    }
    Ok(CodecInfo {
        codec_id,
        width,
        height,
    })
}

/// The display size of a session packet; `None` for a media packet header.
pub(crate) fn session_dimensions(header: &[u8; 12]) -> Option<(u32, u32)> {
    let flags = u64::from_be_bytes(header[..8].try_into().expect("eight bytes"));
    (flags & PACKET_FLAG_SESSION != 0).then(|| {
        let width = u32::from_be_bytes(header[4..8].try_into().expect("four bytes"));
        let height = u32::from_be_bytes(header[8..].try_into().expect("four bytes"));
        (width, height)
    })
}

/// A media packet header's kind, PTS, and payload length. Session packets are told apart
/// first with [`session_dimensions`].
pub(crate) fn parse_packet_header(bytes: [u8; 12]) -> (UnitKind, u64, u32) {
    let flags = u64::from_be_bytes(bytes[..8].try_into().expect("eight bytes"));
    let len = u32::from_be_bytes(bytes[8..].try_into().expect("four bytes"));
    let kind = if flags & PACKET_FLAG_CONFIG != 0 {
        UnitKind::Config
    } else if flags & PACKET_FLAG_KEY_FRAME != 0 {
        UnitKind::Key
    } else {
        UnitKind::Delta
    };
    (kind, flags & PACKET_PTS_MASK, len)
}

/// The `adb shell` command line that runs the pinned server; the version argument must match
/// the jar or the server refuses to start. `scid` is always eight lowercase hex digits.
pub(crate) fn server_args(scid: u32) -> Vec<String> {
    [
        format!("CLASSPATH={REMOTE_JAR}"),
        "app_process".into(),
        "/".into(),
        "com.genymobile.scrcpy.Server".into(),
        SCRCPY_VERSION.into(),
        format!("scid={scid:08x}"),
        "tunnel_forward=true".into(),
        "video=true".into(),
        "audio=false".into(),
        "control=true".into(),
        "max_size=1280".into(),
        "video_bit_rate=4000000".into(),
        "max_fps=30".into(),
        "send_device_meta=false".into(),
        // Nothing reads the control socket, so the server must not write clipboard changes.
        "clipboard_autosync=false".into(),
        // With cleanup on, scrcpy's CleanUp helper deletes the jar as it starts, so every
        // session would push it again. It restores only options BiBCode never sets.
        "cleanup=false".into(),
    ]
    .into()
}

/// Why `units` closed.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum StreamEnd {
    /// Nobody received units any more.
    ConsumerLeft,
    /// The server closed the stream or sent one that cannot be read; it may have died.
    Failed(DeviceError),
}

/// A running scrcpy-server streaming one device's display as H.264 units. Call
/// [`stop`](Self::stop) to end it. Dropping it instead also stops the reader, kills the
/// server, and removes the forward in the background, but reports nothing.
pub(crate) struct ScrcpySession {
    pub units: mpsc::Receiver<VideoUnit>,
    pub codec: CodecInfo,
    control: TcpStream,
    helper: RegisteredHelper,
    forward: Forward,
    reader: AbortOnDrop,
    ended: Arc<OnceLock<StreamEnd>>,
    stderr: StderrTail,
}

impl ScrcpySession {
    /// Pushes the jar when the device lacks it, forwards a loopback port to the server's
    /// socket, runs the server, and reads its stream headers. On failure everything started
    /// so far is stopped. `display` hears the guest's display size from the codec header and
    /// each session packet, and is cleared when the session ends.
    pub(crate) async fn start(
        adb: &Path,
        serial: &str,
        registry: &ProcessAttributionRegistry,
        display: DisplayHandle,
    ) -> Result<Self, DeviceError> {
        ensure_jar(adb, serial).await?;
        let scid = getrandom::u32()
            .map_err(|error| DeviceError::Io(format!("no random scrcpy id: {error}")))?
            & 0x7fff_ffff;
        // Removed in the background if this future is dropped before it finishes.
        let forward = Forward {
            port: forward(adb, serial, scid).await?,
            adb: adb.to_owned(),
            serial: serial.to_owned(),
            removed: false,
        };
        match launch(adb, serial, registry, scid, forward.port).await {
            Ok(Launched {
                helper,
                codec,
                video,
                control,
                stderr,
            }) => {
                display.set(codec.width, codec.height);
                let (sender, units) = mpsc::channel(UNIT_QUEUE);
                let ended = Arc::new(OnceLock::new());
                let reader = tokio::spawn({
                    let ended = ended.clone();
                    async move {
                        let mut video = video;
                        // Owned by the reader, so the size clears when it ends or is aborted.
                        let on_session = |width, height| display.set(width, height);
                        let end = match read_units(&mut video, &sender, on_session).await {
                            Ok(()) => {
                                tracing::debug!("scrcpy video consumer went away");
                                StreamEnd::ConsumerLeft
                            }
                            Err(error) => {
                                tracing::warn!(%error, "scrcpy video stream ended");
                                StreamEnd::Failed(error)
                            }
                        };
                        // Set before `sender` drops, so a closed `units` always has an end.
                        let _ = ended.set(end);
                        drop(sender);
                    }
                });
                Ok(Self {
                    units,
                    codec,
                    control,
                    helper,
                    forward,
                    reader: AbortOnDrop(reader.abort_handle()),
                    ended,
                    stderr,
                })
            }
            Err(error) => {
                tracing::warn!(%error, "scrcpy-server failed to start");
                forward.remove().await;
                Err(error)
            }
        }
    }

    /// How the unit stream ended; `None` while it runs. Set before `units` closes.
    pub(crate) fn ended(&self) -> Option<StreamEnd> {
        self.ended.get().cloned()
    }

    /// Asks the server to restart its encoder, which then sends a config and a key unit.
    pub(crate) async fn reset_video(&mut self) -> Result<(), DeviceError> {
        self.control
            .write_all(&[TYPE_RESET_VIDEO])
            .await
            .map_err(|error| DeviceError::Io(format!("scrcpy control socket: {error}")))
    }

    /// Closes the sockets, ends the server, and removes the forward unless `device_ended`:
    /// adb drops a device's forwards when it disconnects, and asking a departing device only
    /// waits for adb to fail. A server that died on its own is reported as an exit nobody
    /// asked for.
    pub(crate) async fn stop(self, device_ended: bool) {
        let Self {
            control,
            helper,
            forward,
            reader,
            ended,
            stderr,
            ..
        } = self;
        if let Some(StreamEnd::Failed(error)) = ended.get() {
            // A dying server breaks its stream first; give its exit a moment to show.
            let deadline = Instant::now() + SERVER_EXIT_GRACE;
            while helper.exited().is_none() && Instant::now() < deadline {
                tokio::time::sleep(SERVER_EXIT_POLL).await;
            }
            if helper.exited().is_none() {
                tracing::warn!(
                    %error,
                    stderr = stderr.text().unwrap_or_default(),
                    "scrcpy video stream failed while scrcpy-server still ran"
                );
            }
        }
        // Closing the sockets alone makes the server exit, so the stop is noted first.
        note_stop(&helper);
        drop(reader);
        drop(control);
        helper.terminate().await;
        if device_ended {
            forward.leave();
        } else {
            forward.remove().await;
        }
    }
}

/// Reads packets until the stream ends or breaks, sending each media unit to `units` and
/// each session packet's display size to `on_session`. Returns `Ok` once nobody receives
/// units any more.
async fn read_units(
    video: &mut (impl AsyncRead + Unpin),
    units: &mpsc::Sender<VideoUnit>,
    on_session: impl Fn(u32, u32),
) -> Result<(), DeviceError> {
    let mut header = [0_u8; 12];
    loop {
        video.read_exact(&mut header).await.map_err(read_error)?;
        if let Some((width, height)) = session_dimensions(&header) {
            // The config unit that follows carries the new size for decoders.
            tracing::debug!(width, height, "scrcpy video session changed");
            on_session(width, height);
            continue;
        }
        let (kind, pts_us, len) = parse_packet_header(header);
        if len == 0 || len > MAX_PACKET_BYTES {
            return Err(DeviceError::Protocol(format!(
                "scrcpy packet of {len} bytes is outside 1..={MAX_PACKET_BYTES}"
            )));
        }
        let mut data = vec![0_u8; len as usize];
        video.read_exact(&mut data).await.map_err(read_error)?;
        let unit = VideoUnit {
            kind,
            pts_us,
            data: data.into(),
        };
        if units.send(unit).await.is_err() {
            return Ok(());
        }
    }
}

fn read_error(error: std::io::Error) -> DeviceError {
    DeviceError::Io(format!("scrcpy video socket: {error}"))
}

struct Launched {
    helper: RegisteredHelper,
    codec: CodecInfo,
    video: TcpStream,
    control: TcpStream,
    stderr: StderrTail,
}

/// Starts the server and opens its sockets in scrcpy's order: video (which answers with a
/// dummy byte once the server listens), then control; then reads the stream headers. A
/// failure carries the end of the server's stderr.
async fn launch(
    adb: &Path,
    serial: &str,
    registry: &ProcessAttributionRegistry,
    scid: u32,
    port: u16,
) -> Result<Launched, DeviceError> {
    let mut command = CommandWrap::with_new(adb, |command| {
        command
            .args(["-s", serial, "shell"])
            .args(server_args(scid))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
    });
    configure_supervised_background_command_wrap(&mut command);
    let exe = adb.to_string_lossy().into_owned();
    let stderr = StderrTail::default();
    let mut helper = match spawn_registered_helper(command, exe, HELPER_LABEL, registry).await {
        Ok(helper) => helper,
        Err(HelperSpawnError::ExitedEarly {
            status,
            stderr: pipe,
            ..
        }) => {
            if let Some(pipe) = pipe {
                let _ = tokio::time::timeout(STDERR_DRAIN_GRACE, stderr.drain(pipe)).await;
            }
            let status = status.map_or_else(|| "exited".to_owned(), |status| status.to_string());
            let error = DeviceError::Io(format!("scrcpy-server exited while starting: {status}"));
            return Err(with_stderr(error, &stderr));
        }
        Err(error) => {
            return Err(DeviceError::Io(format!(
                "could not start scrcpy-server: {error}"
            )));
        }
    };
    let drain = helper.child.stderr().take().map(|pipe| stderr.drain(pipe));
    // The sockets outlive the attempt so a failure notes the stop before closing them.
    let (mut video_socket, mut control_socket) = (None, None);
    let connected = async {
        let video = video_socket.insert(connect_video(&helper, port).await?);
        let control = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .map_err(|error| DeviceError::Io(format!("scrcpy control socket: {error}")))?;
        let _ = control_socket.insert(control).set_nodelay(true);
        let mut codec_id = [0_u8; 4];
        let mut session = [0_u8; 12];
        let headers = async {
            video.read_exact(&mut codec_id).await?;
            video.read_exact(&mut session).await
        };
        match tokio::time::timeout(HEADER_TIMEOUT, headers).await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => return Err(read_error(error)),
            Err(_) => {
                return Err(DeviceError::Io(
                    "scrcpy-server sent no stream header".to_owned(),
                ));
            }
        }
        parse_codec_header(codec_id, session)
    }
    .await;
    match (connected, video_socket, control_socket) {
        (Ok(codec), Some(video), Some(control)) => Ok(Launched {
            helper,
            codec,
            video,
            control,
            stderr,
        }),
        (connected, video, control) => {
            note_stop(&helper);
            drop((video, control));
            helper.terminate().await;
            if let Some(drain) = drain {
                let _ = tokio::time::timeout(STDERR_DRAIN_GRACE, drain).await;
            }
            let error = connected.err().unwrap_or_else(|| {
                DeviceError::Protocol("scrcpy sockets missing after connecting".to_owned())
            });
            Err(with_stderr(error, &stderr))
        }
    }
}

/// `error` followed by what the server last wrote to stderr, if anything.
fn with_stderr(error: DeviceError, stderr: &StderrTail) -> DeviceError {
    let Some(tail) = stderr.text() else {
        return error;
    };
    match error {
        DeviceError::Io(message) => DeviceError::Io(format!("{message}; scrcpy-server: {tail}")),
        DeviceError::Protocol(message) => {
            DeviceError::Protocol(format!("{message}; scrcpy-server: {tail}"))
        }
        other @ DeviceError::Grpc(_) => other,
    }
}

/// Aborts the reader task when the session goes away.
struct AbortOnDrop(AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The loopback port forwarded to the server: removed by [`remove`](Self::remove), or in the
/// background when dropped (a cancelled start, or a session dropped without `stop`).
struct Forward {
    adb: PathBuf,
    serial: String,
    port: u16,
    removed: bool,
}

impl Forward {
    async fn remove(mut self) {
        self.removed = true;
        remove_forward(&self.adb, &self.serial, self.port).await;
    }

    /// Leaves the forward to adb, which drops it with the device.
    fn leave(mut self) {
        self.removed = true;
        tracing::debug!(
            port = self.port,
            "left the scrcpy adb forward to its ending device"
        );
    }
}

impl Drop for Forward {
    fn drop(&mut self) {
        if self.removed {
            return;
        }
        let (adb, serial, port) = (
            std::mem::take(&mut self.adb),
            std::mem::take(&mut self.serial),
            self.port,
        );
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn(async move { remove_forward(&adb, &serial, port).await });
            }
            Err(_) => tracing::warn!(port, "could not remove the scrcpy adb forward: no runtime"),
        }
    }
}

/// Connects to the forwarded port until the server answers with its dummy byte. Before the
/// server listens, adb accepts the connection and closes it at once.
async fn connect_video(helper: &RegisteredHelper, port: u16) -> Result<TcpStream, DeviceError> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    loop {
        if let Some(status) = helper.exited() {
            return Err(DeviceError::Io(format!(
                "scrcpy-server exited while starting: {status}"
            )));
        }
        let attempt = async {
            let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await?;
            let mut dummy = [0_u8; 1];
            stream.read_exact(&mut dummy).await?;
            Ok::<_, std::io::Error>(stream)
        };
        let error = match tokio::time::timeout_at(deadline, attempt).await {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(error)) => error.to_string(),
            Err(_) => "timed out".to_owned(),
        };
        if Instant::now() + CONNECT_RETRY >= deadline {
            return Err(DeviceError::Io(format!(
                "scrcpy-server did not accept a connection: {error}"
            )));
        }
        tokio::time::sleep(CONNECT_RETRY).await;
    }
}

/// Before the server is asked to stop: an exit nobody asked for is reported, otherwise the
/// coming stop is noted so its exit is not reported.
fn note_stop(helper: &RegisteredHelper) {
    match helper.exited() {
        Some(status) => helper.report_unrequested_exit(HELPER_LABEL, &status),
        None => exits::note_stop_requested(helper.pid),
    }
}

/// Pushes the embedded jar unless the device already holds a copy with the pinned hash.
async fn ensure_jar(adb: &Path, serial: &str) -> Result<(), DeviceError> {
    let remote = run_adb(
        adb,
        serial,
        &["shell", "sha256sum", REMOTE_JAR],
        ADB_TIMEOUT,
    )
    .await
    .ok();
    let current = remote
        .as_deref()
        .and_then(|output| output.split_whitespace().next());
    if current == Some(SCRCPY_SERVER_SHA256) {
        return Ok(());
    }
    let local = write_local_jar().await?;
    let pushed = run_adb(
        adb,
        serial,
        &["push", &local.to_string_lossy(), REMOTE_JAR],
        ADB_PUSH_TIMEOUT,
    )
    .await;
    if let Err(error) = tokio::fs::remove_file(&local).await {
        tracing::warn!(%error, path = %local.display(), "could not remove the scrcpy-server copy");
    }
    pushed.map(|_| ())
}

/// The embedded jar in a new file under the system temporary directory.
async fn write_local_jar() -> Result<PathBuf, DeviceError> {
    let io = |error: std::io::Error| DeviceError::Io(format!("scrcpy-server copy: {error}"));
    let nonce = getrandom::u64()
        .map_err(|error| DeviceError::Io(format!("no random file name: {error}")))?;
    let path = std::env::temp_dir().join(format!(
        "bibcode-scrcpy-server-v{SCRCPY_VERSION}-{nonce:016x}.jar"
    ));
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .await
        .map_err(io)?;
    let written = async {
        file.write_all(SCRCPY_SERVER_JAR).await?;
        file.flush().await
    }
    .await;
    if let Err(error) = written {
        let _ = tokio::fs::remove_file(&path).await;
        return Err(io(error));
    }
    Ok(path)
}

/// Forwards an OS-assigned loopback port to the server's abstract socket. `tcp:0` binds
/// 127.0.0.1 only: adb listens on every interface only with `-a`, which is never passed.
async fn forward(adb: &Path, serial: &str, scid: u32) -> Result<u16, DeviceError> {
    let remote = format!("localabstract:scrcpy_{scid:08x}");
    let output = run_adb(adb, serial, &["forward", "tcp:0", &remote], ADB_TIMEOUT).await?;
    output
        .trim()
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| DeviceError::Io(format!("adb forward printed no port: {}", output.trim())))
}

async fn remove_forward(adb: &Path, serial: &str, port: u16) {
    let local = format!("tcp:{port}");
    if let Err(error) = run_adb(adb, serial, &["forward", "--remove", &local], ADB_TIMEOUT).await {
        tracing::warn!(%error, port, "could not remove the scrcpy adb forward");
    }
}

/// Runs `adb -s <serial> <args>` and returns its standard output.
async fn run_adb(
    adb: &Path,
    serial: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<String, DeviceError> {
    let mut command = tokio::process::Command::new(adb);
    command
        .args(["-s", serial])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let request = SupervisedRunRequest {
        command,
        stdin: None,
        timeout,
        cleanup_timeout: ADB_CLEANUP_TIMEOUT,
        max_output_bytes: ADB_MAX_OUTPUT_BYTES,
        overflow: SupervisedOverflow::Truncate,
    };
    let verb = args.first().copied().unwrap_or_default();
    let output = run_supervised(request, &CancellationToken::new())
        .await
        .map_err(|error| DeviceError::Io(format!("adb {verb}: {error:?}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr.bytes);
        return Err(DeviceError::Io(format!(
            "adb {verb} failed ({}): {}",
            output.status,
            stderr.trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout.bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::android::testing::{config_unit, delta_unit, key_unit, session_packet};

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn embedded_jar_matches_pinned_checksum() {
        use sha2::{Digest, Sha256};
        assert_eq!(
            hex(&Sha256::digest(SCRCPY_SERVER_JAR)),
            SCRCPY_SERVER_SHA256
        );
    }

    // scrcpy v4.1 (`app/src/demuxer.c`): bit 63 marks a session packet, bit 62 a config
    // packet, bit 61 a key frame, and the low 61 bits are the PTS.
    #[test]
    fn parses_packet_flags_and_pts() {
        let mut header = [0_u8; 12];
        header[..8].copy_from_slice(&((1_u64 << 61) | 123_456).to_be_bytes());
        header[8..].copy_from_slice(&4096_u32.to_be_bytes());
        assert_eq!(parse_packet_header(header), (UnitKind::Key, 123_456, 4096));
        header[..8].copy_from_slice(&(1_u64 << 62).to_be_bytes());
        assert_eq!(parse_packet_header(header).0, UnitKind::Config);
        header[..8].copy_from_slice(&77_u64.to_be_bytes());
        assert_eq!(parse_packet_header(header).0, UnitKind::Delta);
        assert_eq!(session_dimensions(&header), None);
    }

    #[test]
    fn parses_session_packets() {
        let header: [u8; 12] = session_packet(1280, 720).try_into().unwrap();
        assert_eq!(session_dimensions(&header), Some((1280, 720)));
    }

    #[test]
    fn codec_header_requires_h264_and_a_session_size() {
        let h264 = 0x6832_3634_u32.to_be_bytes();
        let session: [u8; 12] = session_packet(720, 1280).try_into().unwrap();
        assert_eq!(
            parse_codec_header(h264, session).unwrap(),
            CodecInfo {
                codec_id: 0x6832_3634,
                width: 720,
                height: 1280
            }
        );
        for codec in [0_u32, 1, 0x6832_3635] {
            assert!(
                parse_codec_header(codec.to_be_bytes(), session).is_err(),
                "{codec:#x}"
            );
        }
        let not_session: [u8; 12] = key_unit(1)[..12].try_into().unwrap();
        assert!(parse_codec_header(h264, not_session).is_err());
        for (width, height) in [(0, 1280), (720, 0), (720, 100_000)] {
            let session: [u8; 12] = session_packet(width, height).try_into().unwrap();
            assert!(
                parse_codec_header(h264, session).is_err(),
                "{width}x{height}"
            );
        }
    }

    #[test]
    fn server_args_pin_version_and_disable_audio() {
        let args = server_args(0x1234_abcd);
        assert_eq!(
            args[..3],
            [
                "CLASSPATH=/data/local/tmp/bibcode-scrcpy-server-v4.1.jar".to_owned(),
                "app_process".into(),
                "/".into()
            ]
        );
        for expected in [
            "com.genymobile.scrcpy.Server",
            "4.1",
            "scid=1234abcd",
            "tunnel_forward=true",
            "video=true",
            "audio=false",
            "control=true",
            "max_size=1280",
            "video_bit_rate=4000000",
            "max_fps=30",
            "send_device_meta=false",
            "clipboard_autosync=false",
            // Keeps the pushed jar on the device, so the next session skips the push.
            "cleanup=false",
        ] {
            assert!(args.iter().any(|arg| arg == expected), "{expected}");
        }
    }

    #[test]
    fn scid_is_eight_lowercase_hex_digits() {
        let args = server_args(0x1f);
        assert!(args.iter().any(|arg| arg == "scid=0000001f"));
    }

    #[tokio::test]
    async fn reader_skips_session_packets_and_streams_units() {
        let stream = [
            config_unit(),
            key_unit(1),
            session_packet(1280, 720),
            config_unit(),
            delta_unit(2),
        ]
        .concat();
        let (sender, mut units) = mpsc::channel(8);
        let sessions = std::cell::RefCell::new(Vec::new());
        let error = read_units(&mut stream.as_slice(), &sender, |width, height| {
            sessions.borrow_mut().push((width, height));
        })
        .await
        .unwrap_err();
        assert!(
            matches!(error, DeviceError::Io(_)),
            "the stream ended: {error}"
        );
        // A session packet reports the guest's new display size (a rotation).
        assert_eq!(sessions.into_inner(), [(1280, 720)]);
        let mut kinds = Vec::new();
        while let Ok(unit) = units.try_recv() {
            kinds.push((unit.kind, unit.pts_us, unit.data.len()));
        }
        assert_eq!(
            kinds,
            [
                (UnitKind::Config, 0, 6),
                (UnitKind::Key, 1, 6),
                (UnitKind::Config, 0, 6),
                (UnitKind::Delta, 2, 6)
            ]
        );
    }

    #[tokio::test]
    async fn reader_rejects_empty_and_oversized_packets_without_allocating() {
        for len in [0, MAX_PACKET_BYTES + 1, u32::MAX] {
            let mut header = 5_u64.to_be_bytes().to_vec();
            header.extend(len.to_be_bytes());
            let (sender, mut units) = mpsc::channel(8);
            let error = read_units(&mut header.as_slice(), &sender, |_, _| {})
                .await
                .unwrap_err();
            assert!(matches!(error, DeviceError::Protocol(_)), "{len}: {error}");
            assert!(units.try_recv().is_err());
        }
    }

    #[tokio::test]
    async fn reader_ends_quietly_when_the_consumer_leaves() {
        let stream = [config_unit(), key_unit(1)].concat();
        let (sender, units) = mpsc::channel(8);
        drop(units);
        read_units(&mut stream.as_slice(), &sender, |_, _| {})
            .await
            .unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn session_streams_units_and_sends_reset() {
        use crate::{
            device::android::testing::{FAKE_SCRCPY_SIZE, FakeScrcpySource, FakeSdk},
            diagnostics::ProcessAttributionRegistry,
        };
        let source = FakeScrcpySource::spawn(vec![config_unit(), key_unit(1), delta_unit(2)]).await;
        let sdk = FakeSdk::new().adb_forwards_to(source.port).write();
        let tracker = crate::device::android::orientation::OrientationTracker::new((1080, 2400));
        let mut session = ScrcpySession::start(
            &sdk.adb(),
            "emulator-5554",
            &ProcessAttributionRegistry::new(),
            tracker.start_display(),
        )
        .await
        .unwrap();
        // The codec header's tall display overrules a landscape skin while the session runs.
        assert_eq!(
            tracker.report_skin(crate::device::catalog::Orientation::Landscape),
            crate::device::catalog::Orientation::Portrait
        );
        assert_eq!(session.codec.codec_id, 0x6832_3634);
        assert_eq!(
            (session.codec.width, session.codec.height),
            FAKE_SCRCPY_SIZE
        );
        let kinds: Vec<_> = [
            session.units.recv().await,
            session.units.recv().await,
            session.units.recv().await,
        ]
        .into_iter()
        .map(|unit| unit.unwrap().kind)
        .collect();
        assert_eq!(kinds, [UnitKind::Config, UnitKind::Key, UnitKind::Delta]);
        assert_eq!(session.ended(), None);
        session.reset_video().await.unwrap();
        assert_eq!(source.control_bytes().await, vec![17]);
        let pid = session.helper.pid;
        session.stop(false).await;
        assert!(!crate::device::android::testing::process_alive(pid));
        // Stopped: the display is gone, so the skin decides again.
        assert_eq!(
            tracker.report_skin(crate::device::catalog::Orientation::Landscape),
            crate::device::catalog::Orientation::Landscape
        );

        let log = sdk.adb_log();
        assert!(log.contains("forward --remove tcp:"), "{log}");
        assert!(
            log.contains(&format!("forward --remove tcp:{}", source.port)),
            "{log}"
        );
        // The remote hash never matches the fake's, so the jar was pushed.
        assert!(
            log.contains("push ")
                && log.contains(" /data/local/tmp/bibcode-scrcpy-server-v4.1.jar"),
            "{log}"
        );
        let forward = log
            .lines()
            .find(|line| line.contains("forward tcp:0"))
            .expect("forward call");
        let scid = forward
            .rsplit("localabstract:scrcpy_")
            .next()
            .expect("socket name");
        assert_eq!(scid.len(), 8, "{forward}");
        assert!(
            log.contains(&format!("scid={scid} ")),
            "the socket name and the server's scid agree: {log}"
        );
        assert!(
            !log.contains(" -a "),
            "adb never listens beyond loopback: {log}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stopping_for_an_ended_device_leaves_the_forward_to_adb() {
        use crate::device::android::testing::{FakeScrcpySource, FakeSdk};
        let source = FakeScrcpySource::spawn(vec![config_unit()]).await;
        let sdk = FakeSdk::new().adb_forwards_to(source.port).write();
        let session = session_on(&sdk).await.unwrap();
        let pid = session.helper.pid;
        session.stop(true).await;
        assert!(!crate::device::android::testing::process_alive(pid));
        // Give a background removal (a dropped `Forward`) time to show up in the log.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let log = sdk.adb_log();
        assert!(log.contains("forward tcp:0"), "{log}");
        assert!(!log.contains("forward --remove"), "{log}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn start_fails_and_removes_the_forward_when_nothing_listens() {
        use crate::{device::android::testing::FakeSdk, diagnostics::ProcessAttributionRegistry};
        // A port nobody listens on: bind, read it, release it.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let sdk = FakeSdk::new().adb_forwards_to(port).write();
        let error = ScrcpySession::start(
            &sdk.adb(),
            "emulator-5554",
            &ProcessAttributionRegistry::new(),
            crate::device::android::orientation::OrientationTracker::new((1080, 2400))
                .start_display(),
        )
        .await
        .err()
        .expect("nothing listens");
        assert!(matches!(error, DeviceError::Io(_)), "{error}");
        assert!(
            sdk.adb_log()
                .contains(&format!("forward --remove tcp:{port}")),
            "{}",
            sdk.adb_log()
        );
    }

    #[cfg(unix)]
    async fn session_on(
        sdk: &crate::device::android::testing::FakeSdk,
    ) -> Result<ScrcpySession, DeviceError> {
        ScrcpySession::start(
            &sdk.adb(),
            "emulator-5554",
            &crate::diagnostics::ProcessAttributionRegistry::new(),
            crate::device::android::orientation::OrientationTracker::new((1080, 2400))
                .start_display(),
        )
        .await
    }

    /// A loopback port nobody listens on.
    #[cfg(unix)]
    fn closed_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn matching_remote_jar_is_not_pushed() {
        use crate::device::android::testing::{FakeScrcpySource, FakeSdk};
        let source = FakeScrcpySource::spawn(vec![config_unit()]).await;
        let sdk = FakeSdk::new()
            .adb_forwards_to(source.port)
            .remote_jar_sha256(SCRCPY_SERVER_SHA256)
            .write();
        let session = session_on(&sdk).await.unwrap();
        session.stop(false).await;
        let log = sdk.adb_log();
        assert!(log.contains("shell sha256sum"), "{log}");
        assert!(!log.contains(" push "), "{log}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn server_dying_mid_stream_is_reported_as_unrequested() {
        use crate::{
            device::android::testing::{FakeScrcpySource, FakeSdk, ScrcpyStub},
            diagnostics::profiler::exits::CapturedExits,
        };
        let captured = CapturedExits::start();
        let source = FakeScrcpySource::spawn(vec![config_unit()]).await;
        let sdk = FakeSdk::new()
            .adb_forwards_to(source.port)
            .scrcpy_stub(ScrcpyStub::ExitsWhenTold)
            .write();
        let mut session = session_on(&sdk).await.unwrap();
        assert_eq!(session.units.recv().await.unwrap().kind, UnitKind::Config);
        sdk.end_scrcpy_server();
        source.close_video();
        assert!(session.units.recv().await.is_none());
        assert!(
            matches!(session.ended(), Some(StreamEnd::Failed(DeviceError::Io(_)))),
            "{:?}",
            session.ended()
        );
        session.stop(false).await;
        let reported = captured.take();
        assert_eq!(reported.len(), 1, "{reported:?}");
        assert_eq!(reported[0][1], HELPER_LABEL, "{reported:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stream_failing_while_the_server_runs_is_a_requested_stop() {
        use crate::{
            device::android::testing::{FakeScrcpySource, FakeSdk},
            diagnostics::profiler::exits::CapturedExits,
        };
        let captured = CapturedExits::start();
        let source = FakeScrcpySource::spawn(vec![config_unit()]).await;
        let sdk = FakeSdk::new().adb_forwards_to(source.port).write();
        let mut session = session_on(&sdk).await.unwrap();
        assert_eq!(session.units.recv().await.unwrap().kind, UnitKind::Config);
        source.close_video();
        assert!(session.units.recv().await.is_none());
        assert!(matches!(session.ended(), Some(StreamEnd::Failed(_))));
        let pid = session.helper.pid;
        session.stop(false).await;
        assert!(!crate::device::android::testing::process_alive(pid));
        assert!(captured.take().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn start_failure_carries_the_server_stderr() {
        use crate::device::android::testing::{FakeSdk, ScrcpyStub};
        let port = closed_port();
        let sdk = FakeSdk::new()
            .adb_forwards_to(port)
            .scrcpy_stub(ScrcpyStub::FailsWith("Unknown option: bogus"))
            .write();
        let error = session_on(&sdk).await.err().expect("the server fails");
        assert!(
            error.to_string().contains("Unknown option: bogus"),
            "{error}"
        );
        assert!(
            sdk.adb_log()
                .contains(&format!("forward --remove tcp:{port}")),
            "{}",
            sdk.adb_log()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_start_removes_the_forward() {
        use crate::device::android::testing::{FakeSdk, wait_until};
        let port = closed_port();
        let sdk = FakeSdk::new().adb_forwards_to(port).write();
        // Cancelled while it retries the connection.
        let start = tokio::time::timeout(Duration::from_millis(500), session_on(&sdk)).await;
        assert!(start.is_err(), "start was still connecting");
        let removed = format!("forward --remove tcp:{port}");
        wait_until(|| sdk.adb_log().contains(&removed)).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dropped_session_removes_the_forward() {
        use crate::device::android::testing::{FakeScrcpySource, FakeSdk, wait_until};
        let source = FakeScrcpySource::spawn(vec![config_unit()]).await;
        let sdk = FakeSdk::new().adb_forwards_to(source.port).write();
        let session = session_on(&sdk).await.unwrap();
        let pid = session.helper.pid;
        drop(session);
        let removed = format!("forward --remove tcp:{}", source.port);
        wait_until(|| sdk.adb_log().contains(&removed)).await;
        wait_until(|| !crate::device::android::testing::process_alive(pid)).await;
    }
}
