//! Outbound half of one RPC connection.
//!
//! One writer task owns the WebSocket sink. It writes each queued RPC
//! message as one text frame or as records in the E2EE record format
//! (`0x00` final, `0x01` continuation), lets a small control lane overtake
//! queued data, and bounds every write by progress: each record must be
//! accepted within [`WRITE_PROGRESS_TIMEOUT`], and each message within 30 s
//! plus its size at 16 KiB/s. When a deadline passes the writer ends the
//! session at once, so the socket closes instead of going silent.

use std::{
    future::{Future, poll_fn},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    task::Poll,
    time::Duration,
};

use axum::extract::ws::Message;
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use tokio::{
    sync::{Notify, mpsc},
    time::{Instant, MissedTickBehavior, timeout, timeout_at},
};
use tokio_util::sync::CancellationToken;

use super::{
    e2ee::{E2eeChannel, MAX_E2EE_CHUNK_BYTES, plaintext_records},
    message::ServerMessage,
    session::RpcOutboundFrame,
};
use crate::diagnostics::profiler::connections;

/// WebSocket subprotocol a client offers to receive large plain `/ws`
/// messages as records.
pub(crate) const CHUNKED_RPC_SUBPROTOCOL: &str = "bibcode.rpc.chunked.v1";
/// A stand-alone control message sent between the records of another message.
pub(crate) const RECORD_FLAG_CONTROL: u8 = 0x02;
/// E2EE feature that lets control records interleave with a split message.
pub(crate) const E2EE_INTERLEAVE_FEATURE: &str = "interleave-v1";
/// E2EE feature: large server messages may arrive raw-deflate compressed, every
/// record of such a message carrying [`RECORD_FLAG_DEFLATE`].
pub(crate) const E2EE_DEFLATE_FEATURE: &str = "deflate-v1";
/// Set on every record of a compressed message, with the final or continuation value.
pub(crate) const RECORD_FLAG_DEFLATE: u8 = 0x10;
/// Smaller messages stay uncompressed: interactive replies keep content-independent
/// sizes, which limits the length side channel compression opens (see the design).
const DEFLATE_MIN_BYTES: usize = 16 * 1024;
/// Plain messages up to this size stay one text frame on a chunked socket.
pub(crate) const PLAIN_WHOLE_MESSAGE_MAX_BYTES: usize = 64 * 1024;
/// Largest message a record-reassembling client accepts (64 MiB).
pub(crate) const MAX_RECORDED_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
/// Every record must be accepted by the socket within this time.
pub(crate) const WRITE_PROGRESS_TIMEOUT: Duration = Duration::from_secs(20);
const WRITE_BASE_ALLOWANCE: Duration = Duration::from_secs(30);
const WRITE_FLOOR_BYTES_PER_SECOND: u64 = 16 * 1024;
/// One control message per in-flight request (64) plus headroom for Pongs.
pub(crate) const CONTROL_LANE_CAPACITY: usize = 80;
/// How long a normal shutdown may spend sending the close frame.
const GRACEFUL_CLOSE_TIMEOUT: Duration = Duration::from_secs(1);
/// Heartbeat Pings go out this often once the socket is authenticated.
pub(crate) const HEARTBEAT_PING_INTERVAL: Duration = Duration::from_secs(15);
/// A connection with no inbound frame and no data write that waited on the peer (see
/// [`BACKPRESSURE_PROGRESS_MIN_WAIT`]) for this long ends.
pub(crate) const HEARTBEAT_SILENCE_LIMIT: Duration = Duration::from_secs(45);
const HEARTBEAT_CHECK_INTERVAL: Duration = Duration::from_secs(5);
/// A data write counts as heartbeat progress only if it waited at least this long for
/// the peer to drain the socket. Shorter waits (lock handoffs, scheduling) prove nothing.
const BACKPRESSURE_PROGRESS_MIN_WAIT: Duration = Duration::from_millis(100);
/// A check that fires more than this much later than planned does not charge the gap.
const HEARTBEAT_LATE_TOLERANCE: Duration = Duration::from_secs(10);
/// Close code the client sends after its own liveness timeout.
pub(crate) const LIVENESS_CLOSE_CODE: u16 = 4408;

/// Activity shared by one connection's reader, writer and heartbeat. Times are
/// milliseconds since the connection started.
pub(crate) struct ConnectionLiveness {
    origin: Instant,
    last_activity_ms: AtomicU64,
    last_inbound_ms: AtomicU64,
    last_outbound_ms: AtomicU64,
    outbound_wait_max_ms: AtomicU64,
    /// The first end the reader, heartbeat or writer saw, packed by
    /// [`ConnectionEnd::code`]; [`NO_END`] until then.
    end: AtomicU32,
    ping_due: AtomicBool,
    wake_writer: Notify,
}

const NO_END: u32 = 0;

impl ConnectionLiveness {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            origin: Instant::now(),
            last_activity_ms: AtomicU64::new(0),
            last_inbound_ms: AtomicU64::new(0),
            last_outbound_ms: AtomicU64::new(0),
            outbound_wait_max_ms: AtomicU64::new(0),
            end: AtomicU32::new(NO_END),
            ping_due: AtomicBool::new(false),
            wake_writer: Notify::new(),
        })
    }

    /// Any inbound frame, including WebSocket Ping, Pong and Close. Readers
    /// count frames through [`observe_inbound`].
    fn record_inbound(&self) {
        self.touch();
        self.last_inbound_ms
            .fetch_max(self.elapsed_ms(), Ordering::Relaxed);
    }

    /// Only data writes that waited on backpressure count: completing proves the peer
    /// drained the socket. Ping, Pong and interrupt/control writes never count.
    fn record_progress(&self) {
        self.touch();
    }

    /// An RPC message, data or control, finished leaving. Heartbeat Pings are not messages.
    fn record_outbound(&self) {
        self.last_outbound_ms
            .fetch_max(self.elapsed_ms(), Ordering::Relaxed);
    }

    /// How long a data message waited in the queue before its write began.
    fn record_outbound_wait(&self, wait: Duration) {
        self.outbound_wait_max_ms
            .fetch_max(millis(wait), Ordering::Relaxed);
    }

    /// The socket ended or failed without a close frame, seen by a reader the session loop does
    /// not own (the E2EE pump).
    pub(crate) fn record_client_gone(&self) {
        self.set_end(ConnectionEnd::ClientGone);
    }

    /// Records why the connection ended; the first end wins and the session reports it.
    fn set_end(&self, end: ConnectionEnd) {
        let _ = self
            .end
            .compare_exchange(NO_END, end.code(), Ordering::AcqRel, Ordering::Acquire);
    }

    fn touch(&self) {
        self.last_activity_ms
            .fetch_max(self.elapsed_ms(), Ordering::Relaxed);
    }

    fn elapsed_ms(&self) -> u64 {
        millis(self.origin.elapsed())
    }

    /// How long the connection has been open.
    pub(crate) fn age(&self) -> Duration {
        self.origin.elapsed()
    }

    fn since(&self, at_ms: &AtomicU64, now: Instant) -> Duration {
        now.saturating_duration_since(
            self.origin + Duration::from_millis(at_ms.load(Ordering::Relaxed)),
        )
    }

    fn silent_for(&self, now: Instant) -> Duration {
        self.since(&self.last_activity_ms, now)
    }

    fn request_ping(&self) {
        self.ping_due.store(true, Ordering::Release);
        self.wake_writer.notify_one();
    }

    fn take_ping(&self) -> bool {
        self.ping_due.swap(false, Ordering::AcqRel)
    }

    /// Logs how the session ended, at info level when the end was abnormal, and records it
    /// as `rpc.disconnect`. The first end the reader, heartbeat or writer recorded wins over
    /// `loop_end`, what the session loop saw after it.
    pub(crate) fn report_end(
        &self,
        loop_end: ConnectionEnd,
        transport: &'static str,
        queued_messages: usize,
        in_flight: usize,
        client_id: Option<&str>,
    ) {
        let end = ConnectionEnd::from_code(self.end.load(Ordering::Acquire)).unwrap_or(loop_end);
        let now = Instant::now();
        let disconnect = connections::Disconnect {
            reason: end.reason(),
            close_code: end.close_code(),
            transport,
            age: now.saturating_duration_since(self.origin),
            since_inbound: self.since(&self.last_inbound_ms, now),
            since_outbound: self.since(&self.last_outbound_ms, now),
            outbound_wait_max: Duration::from_millis(
                self.outbound_wait_max_ms.load(Ordering::Relaxed),
            ),
            queued_messages,
            in_flight,
            client_id,
        };
        if end.is_abnormal() {
            tracing::info!(
                reason = disconnect.reason,
                close_code = disconnect.close_code,
                transport,
                age_ms = millis(disconnect.age),
                since_inbound_ms = millis(disconnect.since_inbound),
                since_outbound_ms = millis(disconnect.since_outbound),
                outbound_wait_max_ms = millis(disconnect.outbound_wait_max),
                queued_messages,
                in_flight,
                client_id,
                "RPC connection ended"
            );
        } else {
            tracing::debug!(reason = disconnect.reason, "RPC connection ended");
        }
        connections::record_disconnect(&disconnect);
    }
}

/// Why a connection ended, from the server's side.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConnectionEnd {
    /// The client sent a close frame; code 4408 is its liveness timeout.
    ClientClosed(Option<u16>),
    /// The socket ended or failed without a close frame.
    ClientGone,
    /// The client sent `Eof` and its last request finished.
    ClientEof,
    /// No inbound frame and no data write that waited on the peer for 45 s.
    HeartbeatTimeout,
    /// A write missed its deadline.
    WriterStalled,
    /// The socket refused a write.
    WriterClosed,
    /// A message could not be encoded or encrypted.
    WriterFailed,
    /// The server ended the session: shutdown, expiry, or revocation.
    Server,
}

impl ConnectionEnd {
    fn reason(self) -> &'static str {
        match self {
            Self::ClientClosed(Some(LIVENESS_CLOSE_CODE)) => "client-liveness-timeout",
            Self::ClientClosed(_) => "client-closed",
            Self::ClientGone => "client-gone",
            Self::ClientEof => "client-eof",
            Self::HeartbeatTimeout => "heartbeat-timeout",
            Self::WriterStalled => "writer-stalled",
            Self::WriterClosed => "writer-closed",
            Self::WriterFailed => "writer-failed",
            Self::Server => "server",
        }
    }

    fn close_code(self) -> Option<u16> {
        match self {
            Self::ClientClosed(code) => code,
            _ => None,
        }
    }

    /// Anything but an orderly close by either side: a normal (1000), going-away (1001) or
    /// codeless close, an `Eof`, or the server's own end.
    fn is_abnormal(self) -> bool {
        !matches!(
            self,
            Self::ClientClosed(None | Some(1000 | 1001)) | Self::ClientEof | Self::Server
        )
    }

    /// The end packed for [`ConnectionLiveness::set_end`]: a kind in the high half and a close
    /// code in the low half.
    fn code(self) -> u32 {
        let (kind, close_code) = match self {
            Self::ClientClosed(Some(code)) => (1, code),
            Self::ClientClosed(None) => (2, 0),
            Self::ClientGone => (3, 0),
            Self::ClientEof => (4, 0),
            Self::HeartbeatTimeout => (5, 0),
            Self::WriterStalled => (6, 0),
            Self::WriterClosed => (7, 0),
            Self::WriterFailed => (8, 0),
            Self::Server => (9, 0),
        };
        (kind << 16) | u32::from(close_code)
    }

    fn from_code(code: u32) -> Option<Self> {
        let close_code = u16::try_from(code & 0xffff).unwrap_or_default();
        Some(match code >> 16 {
            1 => Self::ClientClosed(Some(close_code)),
            2 => Self::ClientClosed(None),
            3 => Self::ClientGone,
            4 => Self::ClientEof,
            5 => Self::HeartbeatTimeout,
            6 => Self::WriterStalled,
            7 => Self::WriterClosed,
            8 => Self::WriterFailed,
            9 => Self::Server,
            _ => return None,
        })
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Counts every frame read from the socket as inbound activity, including
/// WebSocket Ping, Pong and Close; a read error is not activity. A Close frame
/// also records the client's close code as the connection's end. Plain and
/// E2EE readers both wrap their raw socket reader with it.
pub(crate) fn observe_inbound<S>(
    liveness: Arc<ConnectionLiveness>,
    reader: S,
) -> impl Stream<Item = Result<Message, axum::Error>> + Unpin
where
    S: Stream<Item = Result<Message, axum::Error>> + Unpin,
{
    reader.inspect(move |frame| {
        if let Ok(frame) = frame {
            liveness.record_inbound();
            if let Message::Close(close) = frame {
                liveness.set_end(ConnectionEnd::ClientClosed(
                    close.as_ref().map(|close| close.code),
                ));
            }
        }
    })
}

/// Asks the writer for a WebSocket Ping every 15 s and ends the session after
/// 45 s with no inbound frame and no data write that waited on the peer. Checks
/// run every 5 s, so a peer that stops reading is reaped within 50 s. A check that
/// fires more than 10 s late (the process was suspended or starved) restarts the
/// silence clock and pings at once instead of charging time the process did not run.
pub(crate) async fn run_heartbeat(liveness: Arc<ConnectionLiveness>, shutdown: CancellationToken) {
    let mut checks = tokio::time::interval(HEARTBEAT_CHECK_INTERVAL);
    checks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut previous_check = checks.tick().await;
    let mut next_ping = previous_check + HEARTBEAT_PING_INTERVAL;
    loop {
        tokio::select! {
            () = shutdown.cancelled() => return,
            _ = checks.tick() => {}
        }
        let now = Instant::now();
        if now.saturating_duration_since(previous_check)
            > HEARTBEAT_CHECK_INTERVAL + HEARTBEAT_LATE_TOLERANCE
        {
            liveness.touch();
            next_ping = now;
        }
        previous_check = now;
        let silent_for = liveness.silent_for(now);
        if silent_for >= HEARTBEAT_SILENCE_LIMIT {
            tracing::debug!(
                ?silent_for,
                limit = ?HEARTBEAT_SILENCE_LIMIT,
                "RPC peer silent past the heartbeat limit; ending the session"
            );
            liveness.set_end(ConnectionEnd::HeartbeatTimeout);
            shutdown.cancel();
            return;
        }
        if now >= next_ping {
            liveness.request_ping();
            // Small check delays must not shift every subsequent Ping.
            next_ping += HEARTBEAT_PING_INTERVAL;
        }
    }
}

/// How queued messages leave the socket.
#[derive(Clone)]
pub(crate) enum OutboundFraming {
    /// Plain `/ws` without the subprotocol: one text frame per message.
    Whole,
    /// Plain `/ws` with [`CHUNKED_RPC_SUBPROTOCOL`]: messages over 64 KiB leave
    /// as binary records, and control messages interleave as `0x02` records.
    PlainRecords,
    /// `/ws-e2ee`: every message leaves as encrypted records; `interleave` is
    /// true when the client and server negotiated control interleaving.
    Encrypted {
        channel: Arc<Mutex<E2eeChannel>>,
        interleave: bool,
        /// The client accepts compressed data messages ([`E2EE_DEFLATE_FEATURE`]).
        deflate: bool,
    },
}

impl OutboundFraming {
    /// `plain` or `e2ee`, as `rpc.disconnect` names the transport.
    pub(crate) fn transport(&self) -> &'static str {
        match self {
            Self::Whole | Self::PlainRecords => "plain",
            Self::Encrypted { .. } => "e2ee",
        }
    }

    /// The largest message the peer can reassemble; `None` for whole frames.
    pub(crate) fn max_message_bytes(&self) -> Option<usize> {
        match self {
            Self::Whole => None,
            Self::PlainRecords | Self::Encrypted { .. } => Some(MAX_RECORDED_MESSAGE_BYTES),
        }
    }

    fn splits(&self, bytes: usize) -> bool {
        match self {
            Self::Whole => false,
            Self::PlainRecords => bytes > PLAIN_WHOLE_MESSAGE_MAX_BYTES,
            Self::Encrypted { .. } => true,
        }
    }

    fn interleaves(&self) -> bool {
        match self {
            Self::Whole => false,
            Self::PlainRecords => true,
            Self::Encrypted { interleave, .. } => *interleave,
        }
    }

    fn deflates(&self) -> bool {
        matches!(self, Self::Encrypted { deflate: true, .. })
    }

    fn record(&self, flag: u8, chunk: &[u8]) -> Result<Message, TransportError> {
        match self {
            Self::Whole | Self::PlainRecords => {
                let mut record = Vec::with_capacity(1 + chunk.len());
                record.push(flag);
                record.extend_from_slice(chunk);
                Ok(Message::Binary(record.into()))
            }
            Self::Encrypted { channel, .. } => channel
                .lock()
                .expect("E2EE channel lock")
                .encrypt_record(flag, chunk)
                .map(|frame| Message::Binary(frame.into()))
                .map_err(|_| TransportError::Encrypt),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TransportError {
    /// A write did not finish before its deadline.
    Stalled,
    /// The socket rejected a write.
    Closed,
    /// A message could not be encoded or split.
    Encode,
    /// A record could not be encrypted.
    Encrypt,
}

/// The whole-message deadline: 30 s plus the message size at 16 KiB/s.
pub(crate) fn message_deadline(started: Instant, bytes: usize) -> Instant {
    let size_millis = u64::try_from(bytes)
        .unwrap_or(u64::MAX)
        .saturating_mul(1_000)
        / WRITE_FLOOR_BYTES_PER_SECOND;
    started + WRITE_BASE_ALLOWANCE + Duration::from_millis(size_millis)
}

enum WriterStep {
    Control(ServerMessage),
    Data(RpcOutboundFrame),
}

/// Owns the sink until the session ends. Returns after a normal shutdown (the
/// queue closed or the session was cancelled), which sends a close frame, or
/// after a failed write, which cancels `shutdown` so the whole session ends
/// and the socket closes without waiting on the stuck sink. A snapshot drains
/// only the controls and the heartbeat Ping pending at its start; ready data
/// is always selected before any further control service.
pub(crate) async fn run_writer<S>(
    mut sink: S,
    framing: OutboundFraming,
    liveness: Arc<ConnectionLiveness>,
    mut data: mpsc::Receiver<RpcOutboundFrame>,
    mut control: mpsc::Receiver<ServerMessage>,
    shutdown: CancellationToken,
) where
    S: Sink<Message> + Unpin,
{
    let result: Result<(), TransportError> = async {
        let mut service_snapshot = true;
        loop {
            if service_snapshot {
                tokio::select! {
                    () = shutdown.cancelled() => return Ok(()),
                    result = write_queued_control(
                        &mut sink,
                        &framing,
                        &liveness,
                        Some(&mut control),
                        Instant::now() + WRITE_PROGRESS_TIMEOUT,
                        false,
                    ) => result?,
                }
            }
            let step = tokio::select! {
                biased;
                () = shutdown.cancelled() => return Ok(()),
                frame = data.recv() => match frame {
                    Some(frame) => WriterStep::Data(frame),
                    None => return Ok(()),
                },
                () = liveness.wake_writer.notified() => {
                    // A requested heartbeat Ping joins the next finite snapshot.
                    service_snapshot = true;
                    continue;
                }
                Some(message) = control.recv() => WriterStep::Control(message),
            };
            // After an idle control write, select ready data immediately.
            // After data, another finite snapshot is allowed.
            service_snapshot = matches!(&step, WriterStep::Data(_));
            let write = async {
                match step {
                    WriterStep::Control(message) => {
                        write_control(
                            &mut sink,
                            &framing,
                            message,
                            Instant::now() + WRITE_PROGRESS_TIMEOUT,
                            false,
                        )
                        .await
                    }
                    WriterStep::Data(frame) => {
                        liveness.record_outbound_wait(frame.queued_at().elapsed());
                        let is_data = !frame.is_control();
                        let frame = frame.into_wire().map_err(|_| TransportError::Encode)?;
                        // Hold the byte permit until all records leave or this
                        // future is cancelled; control sends never release it.
                        let (message, _budget) = frame.into_parts();
                        write_message(
                            &mut sink,
                            &framing,
                            &liveness,
                            message,
                            is_data,
                            Some(&mut control),
                        )
                        .await
                    }
                }
            };
            tokio::select! {
                () = shutdown.cancelled() => return Ok(()),
                result = write => result?,
            }
            liveness.record_outbound();
        }
    }
    .await;
    match result {
        Ok(()) => {
            let _ = timeout(GRACEFUL_CLOSE_TIMEOUT, sink.close()).await;
        }
        Err(error) => {
            tracing::debug!(?error, "RPC writer gave up; ending the session");
            liveness.set_end(match error {
                TransportError::Stalled => ConnectionEnd::WriterStalled,
                TransportError::Closed => ConnectionEnd::WriterClosed,
                TransportError::Encode | TransportError::Encrypt => ConnectionEnd::WriterFailed,
            });
            shutdown.cancel();
        }
    }
}

/// Writes one message. A whole frame's progress is invisible through the
/// sink, so a whole frame gets the whole-message deadline; each record gets
/// the progress deadline, capped by the message deadline. Between records the
/// pending heartbeat Ping always leaves, and with an interleaving `control`
/// lane queued control messages go out as `0x02` records. Oversized controls
/// keep `is_data == false` through normal framing, so their accepted records
/// renew neither the data-progress deadline nor the heartbeat clock.
async fn write_message<S>(
    sink: &mut S,
    framing: &OutboundFraming,
    liveness: &ConnectionLiveness,
    message: Message,
    is_data: bool,
    mut control: Option<&mut mpsc::Receiver<ServerMessage>>,
) -> Result<(), TransportError>
where
    S: Sink<Message> + Unpin,
{
    let length = match &message {
        Message::Text(text) => text.len(),
        Message::Binary(bytes) => bytes.len(),
        Message::Ping(_) | Message::Pong(_) | Message::Close(_) => return Ok(()),
    };
    let deadline = message_deadline(Instant::now(), length);
    let mut data_deadline = Instant::now() + WRITE_PROGRESS_TIMEOUT;
    if !framing.splits(length) {
        let sent = send_before(
            sink,
            message,
            if is_data {
                deadline
            } else {
                deadline.min(data_deadline)
            },
        )
        .await?;
        if is_data && sent == Sent::AfterBackpressure {
            liveness.record_progress();
        }
        return Ok(());
    }
    let payload: &[u8] = match &message {
        Message::Text(text) => text.as_bytes(),
        Message::Binary(bytes) => bytes,
        Message::Ping(_) | Message::Pong(_) | Message::Close(_) => return Ok(()),
    };
    // Large data messages to a client that accepts it leave compressed, on a blocking
    // thread so a multi-megabyte snapshot does not stall the runtime. The job shares the
    // message's bytes and stops once this write is dropped, so a closed connection
    // leaves no compression running outside its byte budget.
    let compressed = if is_data && framing.deflates() && payload.len() >= DEFLATE_MIN_BYTES {
        let shared: axum::body::Bytes = match &message {
            Message::Text(text) => text.clone().into(),
            Message::Binary(bytes) => bytes.clone(),
            Message::Ping(_) | Message::Pong(_) | Message::Close(_) => return Ok(()),
        };
        let abandoned = CancellationToken::new();
        let _abandon_on_drop = abandoned.clone().drop_guard();
        tokio::task::spawn_blocking(move || deflate_message(&shared, &abandoned))
            .await
            .ok()
            .flatten()
    } else {
        None
    };
    let (payload, compression_flag) = match &compressed {
        Some(compressed) => (compressed.as_slice(), RECORD_FLAG_DEFLATE),
        None => (payload, 0),
    };
    let records = plaintext_records(payload).map_err(|_| TransportError::Encode)?;
    for (index, (flag, chunk)) in records.enumerate() {
        let flag = flag | compression_flag;
        if index > 0 {
            let lane = if framing.interleaves() {
                control.as_deref_mut()
            } else {
                None
            };
            write_queued_control(
                sink,
                framing,
                liveness,
                lane,
                deadline.min(data_deadline),
                true,
            )
            .await?;
        }
        let record = framing.record(flag, chunk)?;
        let sent = send_before(sink, record, deadline.min(data_deadline)).await?;
        if is_data {
            data_deadline = Instant::now() + WRITE_PROGRESS_TIMEOUT;
            if sent == Sent::AfterBackpressure {
                liveness.record_progress();
            }
        }
    }
    Ok(())
}

/// A 4-byte big-endian uncompressed length, then raw deflate data; `None` when the
/// result would not be smaller or the write was `abandoned`, checked between chunks.
fn deflate_message(payload: &[u8], abandoned: &CancellationToken) -> Option<Vec<u8>> {
    use std::io::Write as _;
    const CHUNK_BYTES: usize = 256 * 1024;
    let length = u32::try_from(payload.len()).ok()?;
    let mut encoder = flate2::write::DeflateEncoder::new(
        Vec::with_capacity(payload.len() / 4 + 4),
        flate2::Compression::new(3),
    );
    encoder.get_mut().extend_from_slice(&length.to_be_bytes());
    for chunk in payload.chunks(CHUNK_BYTES) {
        if abandoned.is_cancelled() {
            return None;
        }
        encoder.write_all(chunk).ok()?;
    }
    let compressed = encoder.finish().ok()?;
    (compressed.len() < payload.len()).then_some(compressed)
}

/// Drains one finite snapshot: the heartbeat Ping and the control messages
/// pending at entry, then gives data a turn. Refills never extend the turn.
async fn write_queued_control<S>(
    sink: &mut S,
    framing: &OutboundFraming,
    liveness: &ConnectionLiveness,
    mut control: Option<&mut mpsc::Receiver<ServerMessage>>,
    deadline: Instant,
    interleaved: bool,
) -> Result<(), TransportError>
where
    S: Sink<Message> + Unpin,
{
    let queued_at_start = control.as_ref().map_or(0, |lane| lane.len());
    let ping_at_start = liveness.take_ping();
    if ping_at_start {
        send_before(
            sink,
            Message::Ping(Default::default()),
            deadline.min(Instant::now() + WRITE_PROGRESS_TIMEOUT),
        )
        .await?;
    }
    if let Some(control) = control.as_mut() {
        for _ in 0..queued_at_start {
            let Ok(message) = control.try_recv() else {
                break;
            };
            write_control(sink, framing, message, deadline, interleaved).await?;
            liveness.record_outbound();
        }
    }
    Ok(())
}

/// Controls are admitted only if their encoded payload fits one record.
/// This defensive check protects the bounded lane if a caller bypasses admission.
async fn write_control<S>(
    sink: &mut S,
    framing: &OutboundFraming,
    message: ServerMessage,
    deadline: Instant,
    interleaved: bool,
) -> Result<(), TransportError>
where
    S: Sink<Message> + Unpin,
{
    let text = serde_json::to_string(&message).map_err(|_| TransportError::Encode)?;
    if text.len() > MAX_E2EE_CHUNK_BYTES {
        return Err(TransportError::Encode);
    }
    let frame = if interleaved {
        framing.record(RECORD_FLAG_CONTROL, text.as_bytes())?
    } else if matches!(framing, OutboundFraming::Encrypted { .. }) {
        framing.record(0x00, text.as_bytes())?
    } else {
        Message::Text(text.into())
    };
    send_before(
        sink,
        frame,
        deadline.min(Instant::now() + WRITE_PROGRESS_TIMEOUT),
    )
    .await
    .map(|_| ())
}

/// How a send completed. Only [`Sent::AfterBackpressure`] shows that the peer read:
/// the socket had no room for at least [`BACKPRESSURE_PROGRESS_MIN_WAIT`] and the write
/// finished once it drained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Sent {
    Immediately,
    AfterBackpressure,
}

async fn send_before<S>(
    sink: &mut S,
    message: Message,
    deadline: Instant,
) -> Result<Sent, TransportError>
where
    S: Sink<Message> + Unpin,
{
    if Instant::now() >= deadline {
        return Err(TransportError::Stalled);
    }
    // `send` feeds and flushes one frame, so a control record never waits in
    // the sink's write buffer behind data.
    let mut send = sink.send(message);
    // One poll tells an immediate accept from a write that has to wait; only a
    // measurable wait means the peer, not the runtime, held the write back.
    match poll_fn(|context| Poll::Ready(Pin::new(&mut send).poll(context))).await {
        Poll::Ready(Ok(())) => return Ok(Sent::Immediately),
        Poll::Ready(Err(_)) => return Err(TransportError::Closed),
        Poll::Pending => {}
    }
    let waiting_since = Instant::now();
    match timeout_at(deadline, send).await {
        Ok(Ok(())) if waiting_since.elapsed() >= BACKPRESSURE_PROGRESS_MIN_WAIT => {
            Ok(Sent::AfterBackpressure)
        }
        Ok(Ok(())) => Ok(Sent::Immediately),
        Ok(Err(_)) => Err(TransportError::Closed),
        Err(_) => Err(TransportError::Stalled),
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use futures_util::sink;
    use serde_json::{Value, json};

    use axum::extract::ws::{CloseFrame, Utf8Bytes};

    use super::*;

    #[test]
    fn deflate_round_trips_and_stops_once_its_write_is_abandoned() {
        use std::io::Read as _;
        let payload = br#"{"activities":[{"kind":"tool"}]}"#.repeat(20_000);
        let compressed =
            deflate_message(&payload, &CancellationToken::new()).expect("compresses smaller");
        assert_eq!(
            compressed[..4],
            u32::try_from(payload.len()).unwrap().to_be_bytes()
        );
        let mut inflated = Vec::new();
        flate2::read::DeflateDecoder::new(&compressed[4..])
            .read_to_end(&mut inflated)
            .expect("raw deflate");
        assert_eq!(inflated, payload);

        let abandoned = CancellationToken::new();
        abandoned.cancel();
        assert_eq!(deflate_message(&payload, &abandoned), None);
    }
    use crate::rpc::message::RequestId;

    type Recorded = Arc<Mutex<Vec<Message>>>;

    fn recording_sink(
        delay: Duration,
    ) -> (impl Sink<Message, Error = Infallible> + Unpin, Recorded) {
        let recorded: Recorded = Arc::default();
        let sink_recorded = Arc::clone(&recorded);
        let sink = Box::pin(sink::unfold((), move |(), message: Message| {
            let recorded = Arc::clone(&sink_recorded);
            async move {
                tokio::time::sleep(delay).await;
                recorded.lock().expect("recorded frames").push(message);
                Ok::<_, Infallible>(())
            }
        }));
        (sink, recorded)
    }

    fn response(id: &str, bytes: usize) -> RpcOutboundFrame {
        RpcOutboundFrame::plain(ServerMessage::success(
            RequestId::try_from(id).expect("request id"),
            Some(json!({ "data": "x".repeat(bytes) })),
        ))
    }

    fn flags(recorded: &[Message]) -> Vec<u8> {
        recorded
            .iter()
            .filter_map(|message| match message {
                Message::Binary(bytes) => bytes.first().copied(),
                _ => None,
            })
            .collect()
    }

    async fn write_all(
        framing: OutboundFraming,
        delay: Duration,
        frames: Vec<RpcOutboundFrame>,
    ) -> (Recorded, CancellationToken) {
        let (sink, recorded) = recording_sink(delay);
        let (data_sender, data) = mpsc::channel(8);
        let (_control_sender, control) = mpsc::channel(CONTROL_LANE_CAPACITY);
        let shutdown = CancellationToken::new();
        for frame in frames {
            data_sender.try_send(frame).expect("queue frame");
        }
        drop(data_sender);
        run_writer(
            sink,
            framing,
            ConnectionLiveness::new(),
            data,
            control,
            shutdown.clone(),
        )
        .await;
        (recorded, shutdown)
    }

    #[tokio::test]
    async fn message_deadline_is_thirty_seconds_plus_size_at_sixteen_kib_per_second() {
        let started = Instant::now();
        assert_eq!(
            message_deadline(started, 0) - started,
            Duration::from_secs(30)
        );
        assert_eq!(
            message_deadline(started, 16 * 1024) - started,
            Duration::from_secs(31)
        );
        assert_eq!(
            message_deadline(started, 8 * 1024 * 1024) - started,
            Duration::from_secs(542)
        );
    }

    #[tokio::test]
    async fn whole_framing_sends_one_text_frame_per_message() {
        let (recorded, shutdown) = write_all(
            OutboundFraming::Whole,
            Duration::ZERO,
            vec![response("1", 200 * 1024)],
        )
        .await;
        let recorded = recorded.lock().expect("recorded frames");
        assert_eq!(recorded.len(), 1);
        assert!(matches!(&recorded[0], Message::Text(text) if text.len() > 200 * 1024));
        assert!(!shutdown.is_cancelled());
    }

    #[tokio::test]
    async fn plain_records_split_large_messages_and_keep_small_ones_whole() {
        let (recorded, _) = write_all(
            OutboundFraming::PlainRecords,
            Duration::ZERO,
            vec![response("1", 1024), response("2", 200 * 1024)],
        )
        .await;
        let recorded = recorded.lock().expect("recorded frames");
        assert!(
            matches!(&recorded[0], Message::Text(_)),
            "a small message stays one text frame"
        );
        assert_eq!(flags(&recorded[1..]), vec![0x01, 0x01, 0x01, 0x00]);
        let body: Vec<u8> = recorded[1..]
            .iter()
            .flat_map(|message| match message {
                Message::Binary(bytes) => bytes[1..].to_vec(),
                _ => Vec::new(),
            })
            .collect();
        let decoded: Value = serde_json::from_slice(&body).expect("records reassemble into JSON");
        assert_eq!(decoded["requestId"], "2");
        assert_eq!(
            decoded["exit"]["value"]["data"].as_str().map(str::len),
            Some(200 * 1024)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn control_messages_leave_between_records_as_control_records() {
        let (sink, recorded) = recording_sink(Duration::from_secs(1));
        let (data_sender, data) = mpsc::channel(8);
        let (control_sender, control) = mpsc::channel(CONTROL_LANE_CAPACITY);
        data_sender
            .try_send(response("1", 300 * 1024))
            .expect("queue frame");
        let writer = tokio::spawn(run_writer(
            sink,
            OutboundFraming::PlainRecords,
            ConnectionLiveness::new(),
            data,
            control,
            CancellationToken::new(),
        ));
        tokio::time::sleep(Duration::from_millis(1500)).await;
        control_sender
            .try_send(ServerMessage::Pong)
            .expect("queue pong");
        drop(data_sender);
        writer.await.expect("writer joins");

        let recorded = recorded.lock().expect("recorded frames");
        let flags = flags(&recorded);
        let control_at = flags
            .iter()
            .position(|flag| *flag == RECORD_FLAG_CONTROL)
            .expect("a control record");
        assert!(
            control_at > 0 && control_at < flags.len() - 1,
            "the Pong interleaves with the data records: {flags:?}"
        );
        let Message::Binary(control) = &recorded[control_at] else {
            panic!("a control record is binary");
        };
        assert_eq!(&control[1..], br#"{"_tag":"Pong"}"#);
    }

    #[tokio::test(start_paused = true)]
    async fn whole_framing_sends_control_after_the_message_in_progress() {
        let (sink, recorded) = recording_sink(Duration::from_secs(1));
        let (data_sender, data) = mpsc::channel(8);
        let (control_sender, control) = mpsc::channel(CONTROL_LANE_CAPACITY);
        data_sender
            .try_send(response("1", 1024))
            .expect("queue first");
        data_sender
            .try_send(response("2", 1024))
            .expect("queue second");
        let writer = tokio::spawn(run_writer(
            sink,
            OutboundFraming::Whole,
            ConnectionLiveness::new(),
            data,
            control,
            CancellationToken::new(),
        ));
        tokio::time::sleep(Duration::from_millis(500)).await;
        control_sender
            .try_send(ServerMessage::Pong)
            .expect("queue pong");
        drop(data_sender);
        writer.await.expect("writer joins");

        let recorded = recorded.lock().expect("recorded frames");
        let tags: Vec<String> = recorded
            .iter()
            .map(|message| {
                let Message::Text(text) = message else {
                    panic!("whole framing writes text frames");
                };
                let value: Value = serde_json::from_str(text).expect("frame JSON");
                format!(
                    "{}{}",
                    value["_tag"].as_str().unwrap_or_default(),
                    value["requestId"].as_str().unwrap_or_default()
                )
            })
            .collect();
        assert_eq!(tags, vec!["Exit1", "Pong", "Exit2"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_record_stalled_for_twenty_seconds_ends_the_session() {
        let (sink, _recorded) = recording_sink(Duration::from_secs(3_600));
        let (data_sender, data) = mpsc::channel(8);
        let (_control_sender, control) = mpsc::channel(CONTROL_LANE_CAPACITY);
        let shutdown = CancellationToken::new();
        data_sender
            .try_send(response("1", 200 * 1024))
            .expect("queue frame");
        let started = Instant::now();
        let writer = tokio::spawn(run_writer(
            sink,
            OutboundFraming::PlainRecords,
            ConnectionLiveness::new(),
            data,
            control,
            shutdown.clone(),
        ));
        shutdown.cancelled().await;
        assert_eq!(Instant::now() - started, WRITE_PROGRESS_TIMEOUT);
        writer.await.expect("writer joins");
    }

    #[tokio::test(start_paused = true)]
    async fn a_trickling_sink_fails_the_whole_message_deadline() {
        // Each 64 KiB record takes 19 s, under the progress deadline, but a
        // 1 MiB message gets only 30 s + 64 s in total.
        let (sink, _recorded) = recording_sink(Duration::from_secs(19));
        let (data_sender, data) = mpsc::channel(8);
        let (_control_sender, control) = mpsc::channel(CONTROL_LANE_CAPACITY);
        let shutdown = CancellationToken::new();
        data_sender
            .try_send(response("1", 1024 * 1024))
            .expect("queue frame");
        let started = Instant::now();
        let writer = tokio::spawn(run_writer(
            sink,
            OutboundFraming::PlainRecords,
            ConnectionLiveness::new(),
            data,
            control,
            shutdown.clone(),
        ));
        shutdown.cancelled().await;
        let elapsed = Instant::now() - started;
        assert!(
            elapsed > Duration::from_secs(90) && elapsed < Duration::from_secs(95),
            "the message deadline fires after about 94 s, not {elapsed:?}"
        );
        writer.await.expect("writer joins");
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_sink_that_keeps_the_floor_finishes_a_large_message() {
        let (recorded, shutdown) = write_all(
            OutboundFraming::PlainRecords,
            Duration::from_secs(1),
            vec![response("1", 8 * 1024 * 1024)],
        )
        .await;
        assert!(!shutdown.is_cancelled());
        assert_eq!(recorded.lock().expect("recorded frames").len(), 129);
    }

    #[tokio::test(start_paused = true)]
    async fn refilled_controls_allow_data_and_do_not_extend_the_message_deadline() {
        let recorded: Recorded = Arc::default();
        let sink_recorded = Arc::clone(&recorded);
        let sink = Box::pin(sink::unfold((), move |(), message: Message| {
            let recorded = Arc::clone(&sink_recorded);
            async move {
                let is_data = matches!(&message, Message::Binary(bytes)
                    if bytes.first().is_some_and(|flag| *flag != RECORD_FLAG_CONTROL));
                tokio::time::sleep(if is_data {
                    Duration::from_secs(6)
                } else {
                    Duration::from_millis(100)
                })
                .await;
                recorded.lock().expect("frames").push(message);
                Ok::<_, Infallible>(())
            }
        }));
        let (data_sender, data) = mpsc::channel(1);
        let (control_sender, control) = mpsc::channel(1);
        data_sender
            .try_send(response("1", 1024 * 1024))
            .expect("queue data");
        let producer =
            tokio::spawn(
                async move { while control_sender.send(ServerMessage::Pong).await.is_ok() {} },
            );
        let shutdown = CancellationToken::new();
        let started = Instant::now();
        let writer = tokio::spawn(run_writer(
            sink,
            OutboundFraming::PlainRecords,
            ConnectionLiveness::new(),
            data,
            control,
            shutdown.clone(),
        ));
        timeout(Duration::from_secs(95), shutdown.cancelled())
            .await
            .expect("message deadline still fires under continuous control refill");
        writer.await.expect("writer joins");
        producer.await.expect("producer sees closed lane");
        let frames = recorded.lock().expect("frames");
        let data_count = flags(&frames)
            .iter()
            .filter(|flag| **flag != RECORD_FLAG_CONTROL)
            .count();
        assert!(
            data_count >= 2,
            "a refilling producer cannot starve the next data record"
        );
        assert!(
            frames
                .iter()
                .any(|frame| matches!(frame, Message::Binary(bytes)
            if bytes.first() == Some(&RECORD_FLAG_CONTROL)))
        );
        assert!(
            started.elapsed() >= Duration::from_secs(94)
                && started.elapsed() <= Duration::from_secs(95),
            "aggregate deadline, not renewed control deadlines: {:?}",
            started.elapsed()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn control_writes_cannot_renew_the_data_progress_deadline() {
        let sink = Box::pin(sink::unfold((), |(), message: Message| async move {
            let control = matches!(&message, Message::Binary(bytes)
                if bytes.first() == Some(&RECORD_FLAG_CONTROL));
            tokio::time::sleep(Duration::from_secs(if control { 12 } else { 1 })).await;
            Ok::<_, Infallible>(())
        }));
        let (data_sender, data) = mpsc::channel(1);
        let (control_sender, control) = mpsc::channel(2);
        data_sender
            .try_send(response("1", 1024 * 1024))
            .expect("data");
        let shutdown = CancellationToken::new();
        let started = Instant::now();
        let writer = tokio::spawn(run_writer(
            sink,
            OutboundFraming::PlainRecords,
            ConnectionLiveness::new(),
            data,
            control,
            shutdown.clone(),
        ));
        tokio::time::sleep(Duration::from_millis(500)).await;
        control_sender
            .try_send(ServerMessage::Pong)
            .expect("first control");
        control_sender
            .try_send(ServerMessage::Pong)
            .expect("second control");
        timeout(Duration::from_secs(32), shutdown.cancelled())
            .await
            .expect("controls cannot extend the data-progress deadline");
        assert_eq!(
            started.elapsed(),
            Duration::from_secs(21),
            "one data second followed by at most twenty seconds without data progress"
        );
        writer.await.expect("writer joins");
    }

    #[tokio::test(start_paused = true)]
    async fn a_stopped_reader_is_reaped_within_fifty_seconds() {
        let liveness = ConnectionLiveness::new();
        let shutdown = CancellationToken::new();
        let started = Instant::now();
        let heartbeat = tokio::spawn(run_heartbeat(Arc::clone(&liveness), shutdown.clone()));
        tokio::time::sleep(Duration::from_secs(44)).await;
        assert!(!shutdown.is_cancelled(), "not before 45 s of silence");
        shutdown.cancelled().await;
        let elapsed = Instant::now() - started;
        assert!(
            elapsed >= HEARTBEAT_SILENCE_LIMIT && elapsed <= Duration::from_secs(50),
            "reaped after {elapsed:?}"
        );
        heartbeat.await.expect("heartbeat joins");
    }

    #[tokio::test(start_paused = true)]
    async fn five_second_checks_reap_between_45_and_50_seconds_after_the_last_activity() {
        let liveness = ConnectionLiveness::new();
        let shutdown = CancellationToken::new();
        let heartbeat = tokio::spawn(run_heartbeat(Arc::clone(&liveness), shutdown.clone()));
        // Activity off a check boundary.
        tokio::time::sleep(Duration::from_secs(7)).await;
        liveness.record_inbound();
        let activity = Instant::now();
        tokio::time::sleep(HEARTBEAT_SILENCE_LIMIT - Duration::from_millis(1)).await;
        assert!(
            !shutdown.is_cancelled(),
            "not reaped before 45 s of silence"
        );
        timeout(Duration::from_millis(5_001), shutdown.cancelled())
            .await
            .expect("reaped within 50 s of the last activity");
        let silent = Instant::now() - activity;
        assert!(
            silent >= HEARTBEAT_SILENCE_LIMIT && silent <= Duration::from_secs(50),
            "reaped {silent:?} after the last activity"
        );
        heartbeat.await.expect("heartbeat joins");
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_reader_making_progress_is_not_reaped() {
        let liveness = ConnectionLiveness::new();
        let shutdown = CancellationToken::new();
        let heartbeat = tokio::spawn(run_heartbeat(Arc::clone(&liveness), shutdown.clone()));
        for _ in 0..20 {
            tokio::time::sleep(Duration::from_secs(10)).await;
            liveness.record_progress();
        }
        assert!(!shutdown.is_cancelled());
        shutdown.cancel();
        heartbeat.await.expect("heartbeat joins");
    }

    #[tokio::test(start_paused = true)]
    async fn the_heartbeat_asks_for_a_ping_every_fifteen_seconds() {
        let liveness = ConnectionLiveness::new();
        let shutdown = CancellationToken::new();
        let heartbeat = tokio::spawn(run_heartbeat(Arc::clone(&liveness), shutdown.clone()));
        tokio::time::sleep(Duration::from_secs(14)).await;
        assert!(!liveness.take_ping());
        // Observe just after the 15 s check: at the same paused instant the
        // test could otherwise run before the heartbeat task.
        tokio::time::sleep(Duration::from_millis(1_001)).await;
        assert!(liveness.take_ping());
        assert!(!liveness.take_ping());
        liveness.record_inbound();
        tokio::time::sleep(Duration::from_secs(15)).await;
        assert!(liveness.take_ping());
        shutdown.cancel();
        heartbeat.await.expect("heartbeat joins");
    }

    #[tokio::test(start_paused = true)]
    async fn heartbeat_ping_cadence_does_not_accumulate_check_jitter() {
        let liveness = ConnectionLiveness::new();
        let shutdown = CancellationToken::new();
        let started = Instant::now();
        let heartbeat = run_heartbeat(Arc::clone(&liveness), shutdown.clone());
        tokio::pin!(heartbeat);
        assert!(futures_util::poll!(&mut heartbeat).is_pending());

        for second in (5..=45).step_by(5) {
            // A small wake delay must not push the next Ping past its check.
            let jitter_ms = if second == 15 { 2 } else { 0 };
            let check_at = started + Duration::from_secs(second) + Duration::from_millis(jitter_ms);
            tokio::time::advance(check_at - Instant::now()).await;
            assert!(futures_util::poll!(&mut heartbeat).is_pending());
            let ping_due = second % 15 == 0;
            assert_eq!(
                liveness.take_ping(),
                ping_due,
                "Ping cadence at {:?}",
                started.elapsed()
            );
            if ping_due {
                liveness.record_inbound();
            }
        }

        shutdown.cancel();
        heartbeat.await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_late_check_does_not_charge_the_gap() {
        let liveness = ConnectionLiveness::new();
        let shutdown = CancellationToken::new();
        let heartbeat = tokio::spawn(run_heartbeat(Arc::clone(&liveness), shutdown.clone()));
        tokio::task::yield_now().await;
        // The runtime stalls for a minute (a suspended laptop), so the next check
        // fires 55 s late.
        tokio::time::advance(Duration::from_secs(60)).await;
        tokio::task::yield_now().await;
        assert!(
            !shutdown.is_cancelled(),
            "the stalled minute is not silence"
        );
        assert!(liveness.take_ping(), "one immediate Ping after the stall");
        for elapsed in [5, 10, 15] {
            tokio::time::advance(Duration::from_secs(5)).await;
            tokio::task::yield_now().await;
            assert_eq!(
                liveness.take_ping(),
                elapsed == 15,
                "restart the Ping cadence without a catch-up burst"
            );
        }
        tokio::time::sleep(Duration::from_secs(35)).await;
        assert!(
            shutdown.is_cancelled(),
            "silence after the stall still counts"
        );
        heartbeat.await.expect("heartbeat joins");
    }

    #[tokio::test(start_paused = true)]
    async fn the_writer_sends_a_requested_ping_between_records() {
        let liveness = ConnectionLiveness::new();
        let (sink, recorded) = recording_sink(Duration::from_secs(1));
        let (data_sender, data) = mpsc::channel(8);
        let (_control_sender, control) = mpsc::channel(CONTROL_LANE_CAPACITY);
        data_sender
            .try_send(response("1", 300 * 1024))
            .expect("queue frame");
        let writer = tokio::spawn(run_writer(
            sink,
            OutboundFraming::PlainRecords,
            Arc::clone(&liveness),
            data,
            control,
            CancellationToken::new(),
        ));
        tokio::time::sleep(Duration::from_millis(1500)).await;
        liveness.request_ping();
        drop(data_sender);
        writer.await.expect("writer joins");
        let recorded = recorded.lock().expect("recorded frames");
        let ping_at = recorded
            .iter()
            .position(|message| matches!(message, Message::Ping(_)))
            .expect("a heartbeat ping");
        assert!(
            ping_at > 0 && ping_at < recorded.len() - 1,
            "the ping leaves between records"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn only_a_data_write_that_waited_on_the_peer_is_heartbeat_progress() {
        let liveness = ConnectionLiveness::new();
        tokio::time::advance(Duration::from_secs(10)).await;
        // Accepted at once: the socket had room, which proves nothing about the peer.
        let mut immediate = sink::drain();
        write_message(
            &mut immediate,
            &OutboundFraming::Whole,
            &liveness,
            Message::Text(Utf8Bytes::from_static("{}")),
            true,
            None,
        )
        .await
        .expect("immediate write");
        assert_eq!(liveness.silent_for(Instant::now()), Duration::from_secs(10));
        // A short wait (a lock handoff or scheduling) proves nothing either.
        let (mut briefly, _recorded) = recording_sink(Duration::from_millis(1));
        write_message(
            &mut briefly,
            &OutboundFraming::Whole,
            &liveness,
            Message::Text(Utf8Bytes::from_static("{}")),
            true,
            None,
        )
        .await
        .expect("briefly delayed write");
        assert_eq!(
            liveness.silent_for(Instant::now()),
            Duration::from_millis(10_001)
        );
        // Waited on backpressure, then completed: the peer drained the socket.
        let (mut waiting, _recorded) = recording_sink(BACKPRESSURE_PROGRESS_MIN_WAIT);
        write_message(
            &mut waiting,
            &OutboundFraming::Whole,
            &liveness,
            Message::Text(Utf8Bytes::from_static("{}")),
            true,
            None,
        )
        .await
        .expect("waited write");
        assert_eq!(liveness.silent_for(Instant::now()), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn a_ping_write_is_not_progress() {
        let liveness = ConnectionLiveness::new();
        let (sink, recorded) = recording_sink(Duration::ZERO);
        let (_data_sender, data) = mpsc::channel::<RpcOutboundFrame>(1);
        let (_control_sender, control) = mpsc::channel(CONTROL_LANE_CAPACITY);
        let writer = tokio::spawn(run_writer(
            sink,
            OutboundFraming::Whole,
            Arc::clone(&liveness),
            data,
            control,
            CancellationToken::new(),
        ));
        tokio::time::sleep(Duration::from_secs(10)).await;
        liveness.request_ping();
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(matches!(
            recorded.lock().expect("recorded frames").first(),
            Some(Message::Ping(_))
        ));
        assert_eq!(
            liveness.silent_for(Instant::now()),
            Duration::from_millis(10_010)
        );
        writer.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn pong_and_interrupt_writes_are_not_heartbeat_progress() {
        let liveness = ConnectionLiveness::new();
        let (sink, recorded) = recording_sink(Duration::ZERO);
        let (_data_sender, data) = mpsc::channel(1);
        let (control_sender, control) = mpsc::channel(2);
        let shutdown = CancellationToken::new();
        let writer = tokio::spawn(run_writer(
            sink,
            OutboundFraming::Whole,
            Arc::clone(&liveness),
            data,
            control,
            shutdown.clone(),
        ));
        tokio::time::sleep(Duration::from_secs(10)).await;
        control_sender.try_send(ServerMessage::Pong).expect("Pong");
        control_sender
            .try_send(ServerMessage::interrupt(
                RequestId::try_from("1").expect("id"),
            ))
            .expect("interrupt");
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(recorded.lock().expect("frames").len(), 2);
        assert_eq!(liveness.silent_for(Instant::now()), Duration::from_secs(11));
        shutdown.cancel();
        writer.await.expect("writer joins");
    }

    #[tokio::test(start_paused = true)]
    async fn heartbeat_and_refilled_controls_yield_to_data_and_preserve_its_deadline() {
        let started = Instant::now();
        let recorded: Recorded = Arc::default();
        let sink_recorded = Arc::clone(&recorded);
        let sink = Box::pin(sink::unfold((), move |(), message: Message| {
            let recorded = Arc::clone(&sink_recorded);
            async move {
                let data = matches!(&message, Message::Binary(bytes)
                    if bytes.first().is_some_and(|flag| *flag != RECORD_FLAG_CONTROL));
                // 80 controls take 16 s: longer than the heartbeat interval.
                // Stall late in the transfer, when the aggregate deadline is
                // tighter than the deadline since the last data progress.
                if started.elapsed() >= Duration::from_secs(105) {
                    std::future::pending::<()>().await;
                }
                tokio::time::sleep(if data {
                    Duration::from_secs(1)
                } else {
                    Duration::from_millis(200)
                })
                .await;
                recorded.lock().expect("frames").push(message);
                Ok::<_, Infallible>(())
            }
        }));
        let (data_sender, data) = mpsc::channel(1);
        let (control_sender, control) = mpsc::channel(CONTROL_LANE_CAPACITY);
        data_sender
            .try_send(response("1", 1024 * 1024))
            .expect("queued data");
        for _ in 0..CONTROL_LANE_CAPACITY {
            control_sender
                .try_send(ServerMessage::Pong)
                .expect("initial control snapshot");
        }
        let producer =
            tokio::spawn(
                async move { while control_sender.send(ServerMessage::Pong).await.is_ok() {} },
            );
        let liveness = ConnectionLiveness::new();
        let shutdown = CancellationToken::new();
        let heartbeat = tokio::spawn(run_heartbeat(Arc::clone(&liveness), shutdown.clone()));
        let inbound_liveness = Arc::clone(&liveness);
        let inbound_shutdown = shutdown.clone();
        let inbound = tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = inbound_shutdown.cancelled() => return,
                    () = tokio::time::sleep(Duration::from_secs(5)) => inbound_liveness.record_inbound(),
                }
            }
        });
        let writer = tokio::spawn(run_writer(
            sink,
            OutboundFraming::PlainRecords,
            liveness,
            data,
            control,
            shutdown.clone(),
        ));
        timeout(Duration::from_secs(112), shutdown.cancelled())
            .await
            .expect("writer deadline fires despite inbound activity and continuous controls");
        // First snapshot takes 16 s; the 1 MiB message then gets about 94 s.
        assert!(
            started.elapsed() >= Duration::from_secs(110)
                && started.elapsed() < Duration::from_secs(111),
            "aggregate message deadline, not a new allowance per control: {:?}",
            started.elapsed()
        );
        timeout(Duration::from_secs(1), writer)
            .await
            .expect("bounded writer")
            .expect("writer joins");
        timeout(Duration::from_secs(1), heartbeat)
            .await
            .expect("bounded heartbeat")
            .expect("heartbeat joins");
        timeout(Duration::from_secs(1), inbound)
            .await
            .expect("bounded inbound")
            .expect("inbound joins");
        timeout(Duration::from_secs(1), producer)
            .await
            .expect("bounded producer")
            .expect("producer joins");
        let frames = recorded.lock().expect("frames");
        assert!(
            flags(&frames)
                .iter()
                .filter(|flag| **flag != RECORD_FLAG_CONTROL)
                .count()
                >= 2,
            "ready data must leave before another heartbeat/control snapshot"
        );
        assert!(
            frames.iter().any(|frame| matches!(frame, Message::Ping(_))),
            "the real heartbeat was running"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn oversized_control_records_renew_neither_progress_clock() {
        let liveness = ConnectionLiveness::new();
        let started = Instant::now();
        let (sink, recorded) = recording_sink(Duration::from_secs(6));
        let (data_sender, data) = mpsc::channel(1);
        let (_control_sender, control) = mpsc::channel(CONTROL_LANE_CAPACITY);
        data_sender
            .try_send(response("1", 300 * 1024).into_control().expect("control"))
            .expect("oversized control on data queue");
        let shutdown = CancellationToken::new();
        let writer = tokio::spawn(run_writer(
            sink,
            OutboundFraming::PlainRecords,
            Arc::clone(&liveness),
            data,
            control,
            shutdown.clone(),
        ));
        timeout(Duration::from_secs(21), shutdown.cancelled())
            .await
            .expect("fixed progress bound");
        assert_eq!(started.elapsed(), WRITE_PROGRESS_TIMEOUT);
        assert_eq!(liveness.silent_for(Instant::now()), WRITE_PROGRESS_TIMEOUT);
        assert_eq!(
            recorded.lock().expect("frames").len(),
            3,
            "accepted controls still do not count"
        );
        timeout(Duration::from_secs(1), writer)
            .await
            .expect("bounded writer")
            .expect("writer joins");
    }

    #[test]
    fn a_4408_close_is_the_clients_liveness_timeout() {
        let liveness = ConnectionEnd::ClientClosed(Some(LIVENESS_CLOSE_CODE));
        assert_eq!(liveness.reason(), "client-liveness-timeout");
        assert_eq!(liveness.close_code(), Some(4408));
        assert!(liveness.is_abnormal());
        for orderly in [
            ConnectionEnd::ClientClosed(Some(1000)),
            ConnectionEnd::ClientClosed(Some(1001)),
            ConnectionEnd::ClientClosed(None),
            ConnectionEnd::ClientEof,
            ConnectionEnd::Server,
        ] {
            assert!(!orderly.is_abnormal(), "{orderly:?} is orderly");
        }
        assert_eq!(
            ConnectionEnd::ClientClosed(Some(1000)).reason(),
            "client-closed"
        );
    }

    #[tokio::test]
    async fn a_close_frame_records_the_clients_close_code() {
        let liveness = ConnectionLiveness::new();
        let frames = futures_util::stream::iter([Ok(Message::Close(Some(CloseFrame {
            code: LIVENESS_CLOSE_CODE,
            reason: Utf8Bytes::from_static("liveness timeout"),
        })))]);
        let mut reader = observe_inbound(Arc::clone(&liveness), frames);
        reader.next().await;
        assert_eq!(
            ConnectionEnd::from_code(liveness.end.load(Ordering::Acquire)),
            Some(ConnectionEnd::ClientClosed(Some(LIVENESS_CLOSE_CODE)))
        );
        for end in [
            ConnectionEnd::ClientClosed(Some(1000)),
            ConnectionEnd::ClientClosed(None),
            ConnectionEnd::ClientGone,
            ConnectionEnd::ClientEof,
            ConnectionEnd::HeartbeatTimeout,
            ConnectionEnd::WriterStalled,
            ConnectionEnd::WriterClosed,
            ConnectionEnd::WriterFailed,
            ConnectionEnd::Server,
        ] {
            assert_eq!(ConnectionEnd::from_code(end.code()), Some(end));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_heartbeat_and_the_writer_name_the_end_they_cause() {
        let liveness = ConnectionLiveness::new();
        let shutdown = CancellationToken::new();
        let heartbeat = tokio::spawn(run_heartbeat(Arc::clone(&liveness), shutdown.clone()));
        shutdown.cancelled().await;
        heartbeat.await.expect("heartbeat joins");
        assert_eq!(
            ConnectionEnd::from_code(liveness.end.load(Ordering::Acquire)),
            Some(ConnectionEnd::HeartbeatTimeout)
        );
        // The first end wins: a writer failing during teardown does not rename it.
        liveness.set_end(ConnectionEnd::WriterStalled);
        assert_eq!(
            ConnectionEnd::from_code(liveness.end.load(Ordering::Acquire)),
            Some(ConnectionEnd::HeartbeatTimeout)
        );

        let liveness = ConnectionLiveness::new();
        let (sink, _recorded) = recording_sink(Duration::from_secs(3_600));
        let (data_sender, data) = mpsc::channel(8);
        let (_control_sender, control) = mpsc::channel(CONTROL_LANE_CAPACITY);
        data_sender
            .try_send(response("1", 1024))
            .expect("queue frame");
        run_writer(
            sink,
            OutboundFraming::Whole,
            Arc::clone(&liveness),
            data,
            control,
            CancellationToken::new(),
        )
        .await;
        assert_eq!(
            ConnectionEnd::from_code(liveness.end.load(Ordering::Acquire)),
            Some(ConnectionEnd::WriterStalled)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_writer_records_the_last_message_and_the_longest_queue_wait() {
        let liveness = ConnectionLiveness::new();
        let (sink, _recorded) = recording_sink(Duration::from_secs(1));
        let (data_sender, data) = mpsc::channel(8);
        let (control_sender, control) = mpsc::channel(CONTROL_LANE_CAPACITY);
        // The second response waits behind the first one's 1 s write.
        data_sender.try_send(response("1", 1024)).expect("first");
        data_sender.try_send(response("2", 1024)).expect("second");
        let writer = tokio::spawn(run_writer(
            sink,
            OutboundFraming::Whole,
            Arc::clone(&liveness),
            data,
            control,
            CancellationToken::new(),
        ));
        tokio::time::sleep(Duration::from_millis(2_500)).await;
        assert_eq!(liveness.outbound_wait_max_ms.load(Ordering::Relaxed), 1_000);
        assert_eq!(liveness.last_outbound_ms.load(Ordering::Relaxed), 2_000);
        // A heartbeat Ping is not an RPC message.
        liveness.request_ping();
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(liveness.last_outbound_ms.load(Ordering::Relaxed), 2_000);
        // A Pong is.
        control_sender
            .try_send(ServerMessage::Pong)
            .expect("queue pong");
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(liveness.last_outbound_ms.load(Ordering::Relaxed), 5_500);
        drop(data_sender);
        writer.await.expect("writer joins");
    }
}
