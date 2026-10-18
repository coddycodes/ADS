//! H.264 media fan-out: one video source per device while any media socket watches it, and
//! the single-use tickets that admit those sockets (spec §4).

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError, Weak,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use bytes::Bytes;
use futures_util::future::BoxFuture;
use sha2::{Digest, Sha256};
use tokio::{
    sync::{Notify, mpsc},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

use super::{
    DeviceError, DeviceManager, DeviceOpError, Inner,
    backend::ErasedVideoSource,
    video::{UnitKind, VideoUnit},
};

pub(crate) const SOCKET_QUEUE_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_SOCKETS_PER_DEVICE: usize = 4;
pub(crate) const RESET_MIN_INTERVAL: Duration = Duration::from_secs(1);
pub(crate) const TICKET_TTL: Duration = Duration::from_secs(60);
/// Frames one socket may have queued, whatever their size.
const SOCKET_QUEUE_FRAMES: usize = 256;
/// Tickets kept at once; the oldest is dropped to make room.
const MAX_OUTSTANDING_TICKETS: usize = 256;
/// Tickets one session may hold at once, so a single client cannot flush everyone else's
/// out of the book; its own oldest is dropped first.
const MAX_TICKETS_PER_PRINCIPAL: usize = 8;
/// How long a `RESET_VIDEO` write may take before the hub gives up on it. The write is one
/// byte, so this only bites on a stalled control socket, and keeps it from holding up the
/// unit stream for long.
const RESET_WRITE_TIMEOUT: Duration = Duration::from_millis(200);
const FRAME_HEADER_BYTES: usize = 9;
/// A socket hears from its hub at least this often while the source is open: a quiet
/// source (an unchanged iOS screen sends nothing) gets a [`KEEPALIVE_FRAME`] instead.
pub(crate) const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(1);
/// Kind 3 with pts 0 and no payload: the stream is alive, and there is nothing to decode.
pub(crate) const KEEPALIVE_FRAME: [u8; FRAME_HEADER_BYTES] = [3, 0, 0, 0, 0, 0, 0, 0, 0];
/// How long a socket that reads what it is sent may wait for a key unit before the hub asks
/// again. A legitimate wait lasts at most one deferred window plus the encode.
const WAIT_WATCHDOG: Duration = RESET_MIN_INTERVAL.saturating_mul(2);

/// The connection a ticket was minted on, and the only one that may redeem it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MediaChannel {
    #[default]
    Plain,
    E2ee,
}

/// Why a ticket was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TicketError {
    Expired,
    Reused,
    WrongDevice,
    WrongChannel,
    Unknown,
}

impl TicketError {
    /// The media socket's close reason.
    pub(crate) fn close_reason(self) -> &'static str {
        match self {
            Self::Expired | Self::Unknown => "expired",
            Self::Reused => "reused",
            Self::WrongDevice => "wrongDevice",
            Self::WrongChannel => "wrongChannel",
        }
    }
}

/// Why a media socket could not join a device's stream.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MediaAttachError {
    Limit,
    NotReady,
    Disabled,
}

impl MediaAttachError {
    /// The media socket's close reason.
    pub(crate) fn close_reason(self) -> &'static str {
        match self {
            Self::Limit => "limit",
            Self::NotReady => "stopped",
            Self::Disabled => "disabled",
        }
    }
}

/// `[kind u8][pts u64 BE][Annex-B data]`, kind 0 config, 1 key, 2 delta; the hub's own
/// [`KEEPALIVE_FRAME`] is kind 3.
pub(crate) fn encode_frame(unit: &VideoUnit) -> Vec<u8> {
    let kind = match unit.kind {
        UnitKind::Config => 0,
        UnitKind::Key => 1,
        UnitKind::Delta => 2,
    };
    let mut frame = Vec::with_capacity(FRAME_HEADER_BYTES + unit.data.len());
    frame.push(kind);
    frame.extend(unit.pts_us.to_be_bytes());
    frame.extend_from_slice(&unit.data);
    frame
}

// ---------------------------------------------------------------------------------------------
// Tickets

struct TicketRecord {
    device_id: String,
    principal: String,
    channel: MediaChannel,
    expires: Instant,
    used: bool,
    issued: u64,
}

/// Outstanding media tickets, keyed by the SHA-256 of the ticket so a lookup never compares
/// the secret itself.
#[derive(Default)]
pub(crate) struct TicketBook {
    records: Mutex<TicketTable>,
}

#[derive(Default)]
struct TicketTable {
    by_hash: HashMap<[u8; 32], TicketRecord>,
    next_issue: u64,
}

impl TicketTable {
    /// Drops the oldest ticket that `matches`.
    fn remove_oldest(&mut self, matches: impl Fn(&TicketRecord) -> bool) {
        let oldest = self
            .by_hash
            .iter()
            .filter(|(_, record)| matches(record))
            .min_by_key(|(_, record)| record.issued)
            .map(|(hash, _)| *hash);
        if let Some(oldest) = oldest {
            self.by_hash.remove(&oldest);
        }
    }
}

fn ticket_hash(ticket: &str) -> [u8; 32] {
    Sha256::digest(ticket.as_bytes()).into()
}

impl TicketBook {
    fn table(&self) -> MutexGuard<'_, TicketTable> {
        self.records.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A fresh 256-bit ticket for `device_id`, valid on `channel` for [`TICKET_TTL`].
    pub(crate) fn issue(
        &self,
        device_id: &str,
        principal: &str,
        channel: MediaChannel,
        now: Instant,
    ) -> Result<(String, Instant), DeviceError> {
        let mut secret = [0_u8; 32];
        getrandom::fill(&mut secret)
            .map_err(|error| DeviceError::Io(format!("no random media ticket: {error}")))?;
        let ticket =
            base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, secret);
        let expires = now + TICKET_TTL;
        let mut table = self.table();
        table.by_hash.retain(|_, record| record.expires > now);
        while table
            .by_hash
            .values()
            .filter(|record| record.principal == principal)
            .count()
            >= MAX_TICKETS_PER_PRINCIPAL
        {
            table.remove_oldest(|record| record.principal == principal);
        }
        while table.by_hash.len() >= MAX_OUTSTANDING_TICKETS {
            table.remove_oldest(|_| true);
        }
        let issued = table.next_issue;
        table.next_issue += 1;
        table.by_hash.insert(
            ticket_hash(&ticket),
            TicketRecord {
                device_id: device_id.to_owned(),
                principal: principal.to_owned(),
                channel,
                expires,
                used: false,
                issued,
            },
        );
        Ok((ticket, expires))
    }

    /// The device and principal of `ticket`. Any attempt uses the ticket up, so a refused one
    /// cannot be tried again on another channel.
    pub(crate) fn redeem(
        &self,
        ticket: &str,
        channel: MediaChannel,
        now: Instant,
    ) -> Result<(String, String), TicketError> {
        let mut table = self.table();
        let record = table
            .by_hash
            .get_mut(&ticket_hash(ticket))
            .ok_or(TicketError::Unknown)?;
        if record.used {
            return Err(TicketError::Reused);
        }
        record.used = true;
        if now >= record.expires {
            return Err(TicketError::Expired);
        }
        if record.channel != channel {
            return Err(TicketError::WrongChannel);
        }
        Ok((record.device_id.clone(), record.principal.clone()))
    }
}

// ---------------------------------------------------------------------------------------------
// Fan-out

/// One frame in a socket's queue, tagged with the queue generation it was sent in.
struct QueuedFrame {
    generation: u64,
    delta: bool,
    /// A key unit or a delta, counted in [`SubscriberSlot::media_bytes`].
    media: bool,
    bytes: Bytes,
}

/// The hub's end of one socket's queue.
struct SubscriberSlot {
    id: u64,
    sender: mpsc::Sender<QueuedFrame>,
    queued_bytes: Arc<AtomicUsize>,
    /// The part of [`Self::queued_bytes`] that is key units and deltas, not configs or
    /// keepalives.
    media_bytes: Arc<AtomicUsize>,
    /// Bumped when a unit is dropped: deltas queued before it are discarded unsent.
    generation: Arc<AtomicU64>,
    waiting_for_key: bool,
    /// Set when the socket, already waiting for a key unit, overflows on that key unit or the
    /// config before it (sent by the source or resent for [`Self::needs_config`]), and cleared
    /// when a key unit reaches it. Its reader asks for a key unit once when it drains the
    /// queue, since a still screen sends no other.
    missed_key: Arc<AtomicBool>,
    /// Set when the socket overflows on a config unit, and cleared when a config reaches it.
    /// The client decodes each key unit with the last config it received, so the hub's
    /// newest config goes first to such a socket before its next key unit. Only an overflow
    /// sets it, and every overflow starts the key wait, so a socket that needs a config is
    /// always waiting for a key unit.
    needs_config: bool,
    /// When the socket last started waiting for a key unit; `None` while it is not waiting.
    /// The hub's keepalive tick asks again for a socket that has waited too long with its
    /// queue empty (see [`MediaHub::keepalive`]).
    waiting_since: Option<Instant>,
    closed: CancellationToken,
}

enum Offer {
    Sent,
    Overflow,
    Closed,
}

/// What the hub's task does next about `RESET_VIDEO`.
enum ResetStep {
    /// Write one now.
    Write,
    /// Wait until then, when the window of a deferred request ends.
    At(Instant),
    /// Nothing is asked for.
    Idle,
}

impl SubscriberSlot {
    /// Queues `frame`, a unit of `kind` or, for `None`, a keepalive, or reports `Overflow`
    /// when the queue is full in frames or bytes. The caller decides what an overflow costs
    /// the socket (see [`MediaHub::dispatch`]).
    ///
    /// An empty queue always takes the frame, so a unit larger than the budget still
    /// reaches an idle socket. For a key unit the queue counts as empty while it holds only
    /// configs and keepalives: the config always goes out just before its key unit, and
    /// the two must arrive together.
    fn offer(&self, frame: &Bytes, kind: Option<UnitKind>) -> Offer {
        let queued = self.queued_bytes.load(Ordering::Acquire);
        let backlog = if kind == Some(UnitKind::Key) {
            // Only dispatch adds media bytes, under the hub lock, so this is never less
            // than what the queue holds.
            self.media_bytes.load(Ordering::Acquire)
        } else {
            queued
        };
        if backlog > 0 && queued + frame.len() > SOCKET_QUEUE_BYTES {
            return Offer::Overflow;
        }
        let media = matches!(kind, Some(UnitKind::Key | UnitKind::Delta));
        self.queued_bytes.fetch_add(frame.len(), Ordering::AcqRel);
        if media {
            self.media_bytes.fetch_add(frame.len(), Ordering::AcqRel);
        }
        let item = QueuedFrame {
            generation: self.generation.load(Ordering::Acquire),
            delta: kind == Some(UnitKind::Delta),
            media,
            bytes: frame.clone(),
        };
        match self.sender.try_send(item) {
            Ok(()) => Offer::Sent,
            Err(error) => {
                self.queued_bytes.fetch_sub(frame.len(), Ordering::AcqRel);
                if media {
                    self.media_bytes.fetch_sub(frame.len(), Ordering::AcqRel);
                }
                match error {
                    mpsc::error::TrySendError::Full(_) => Offer::Overflow,
                    mpsc::error::TrySendError::Closed(_) => Offer::Closed,
                }
            }
        }
    }

    /// Drops what is queued and waits for the next key unit.
    fn skip_to_key(&mut self) {
        self.waiting_for_key = true;
        self.waiting_since = Some(Instant::now());
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    /// Queues a key unit, after `config` when the socket lost a config since the last one
    /// that reached it. A refused config stops there: the key unit would reach the decoder
    /// with stale parameters.
    fn offer_key(&mut self, frame: &Bytes, config: Option<&Bytes>) -> Offer {
        if self.needs_config
            && let Some(config) = config
        {
            match self.offer(config, Some(UnitKind::Config)) {
                Offer::Sent => self.needs_config = false,
                refused => return refused,
            }
        }
        self.offer(frame, Some(UnitKind::Key))
    }

    /// What a full queue costs the socket. It drops what is queued and waits for the next
    /// key unit, and `true` means it newly fell behind and should ask for one now. Only such
    /// a socket asks here: one that stays stalled must not make the encoder restart every
    /// second for everyone else. One already waiting drops only a config or a key unit here
    /// (deltas skip it), and may have dropped the answer to its own reset, so its reader
    /// asks again once it drains.
    fn overflow(&mut self) -> bool {
        let newly_behind = !self.waiting_for_key;
        if !newly_behind {
            self.missed_key.store(true, Ordering::Release);
        }
        self.skip_to_key();
        newly_behind
    }
}

#[derive(Default)]
struct HubState {
    subscribers: Vec<SubscriberSlot>,
    cached_config: Option<Bytes>,
    last_reset: Option<Instant>,
    /// A reset the hub's task has yet to write.
    reset_unsent: bool,
    /// A reset asked for inside the rate-limit window, for the task to send when it ends.
    reset_deferred: bool,
    /// `RESET_VIDEO` writes started, sent or not yet; a deferred request counts once it
    /// falls due.
    resets: u64,
    /// Units fanned out so far.
    dispatched: u64,
    next_id: u64,
    /// No subscriber may join any more.
    closed: bool,
    /// The video source failed rather than being stopped.
    failed: bool,
}

/// One device's H.264 fan-out, shared by its media sockets and the task that feeds it.
pub(super) struct MediaHub {
    state: Mutex<HubState>,
    reset: Notify,
    cancel: CancellationToken,
    /// Set when the hub stops because its device ended, before `cancel`.
    device_ended: AtomicBool,
}

impl MediaHub {
    pub(super) fn new(cancel: CancellationToken) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::default(),
            reset: Notify::new(),
            cancel,
            device_ended: AtomicBool::new(false),
        })
    }

    fn lock(&self) -> MutexGuard<'_, HubState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether new subscribers may still join.
    pub(super) fn is_open(&self) -> bool {
        !self.lock().closed && !self.cancel.is_cancelled()
    }

    /// Closes every socket and ends the hub's task, which stops the session.
    pub(super) fn stop(&self) {
        self.close(false);
        self.cancel.cancel();
    }

    /// [`stop`](Self::stop) for a device that stopped running, whose session then skips the
    /// cleanup that needs the device.
    pub(super) fn stop_device_ended(&self) {
        self.device_ended.store(true, Ordering::Release);
        self.stop();
    }

    fn close(&self, failed: bool) {
        let subscribers = {
            let mut state = self.lock();
            if state.closed {
                return;
            }
            state.closed = true;
            state.failed = failed;
            std::mem::take(&mut state.subscribers)
        };
        for slot in subscribers {
            slot.closed.cancel();
        }
    }

    fn failed(&self) -> bool {
        self.lock().failed
    }

    /// Adds a socket with a queue of `capacity` frames. It starts waiting for a key unit,
    /// gets the cached config at once, and asks for a fresh key unit.
    fn subscribe(
        self: &Arc<Self>,
        capacity: usize,
        inner: Weak<Inner>,
        device_id: &str,
    ) -> Result<MediaSubscriber, MediaAttachError> {
        let (sender, frames) = mpsc::channel(capacity);
        let closed = CancellationToken::new();
        let queued_bytes = Arc::new(AtomicUsize::new(0));
        let media_bytes = Arc::new(AtomicUsize::new(0));
        let generation = Arc::new(AtomicU64::new(0));
        let missed_key = Arc::new(AtomicBool::new(false));
        let id = {
            let mut state = self.lock();
            if state.closed {
                return Err(MediaAttachError::NotReady);
            }
            if state.subscribers.len() >= MAX_SOCKETS_PER_DEVICE {
                return Err(MediaAttachError::Limit);
            }
            let id = state.next_id;
            state.next_id += 1;
            let slot = SubscriberSlot {
                id,
                sender,
                queued_bytes: queued_bytes.clone(),
                media_bytes: media_bytes.clone(),
                generation: generation.clone(),
                waiting_for_key: true,
                missed_key: missed_key.clone(),
                needs_config: false,
                waiting_since: Some(Instant::now()),
                closed: closed.clone(),
            };
            if let Some(config) = &state.cached_config {
                // A fresh queue takes any frame (an empty one skips the byte budget), so only
                // a closed receiver could refuse it, and it is ours: the config always arrives.
                let _ = slot.offer(config, Some(UnitKind::Config));
            }
            state.subscribers.push(slot);
            self.maybe_reset(&mut state);
            id
        };
        Ok(MediaSubscriber {
            frames,
            closed,
            queued_bytes,
            media_bytes,
            generation,
            missed_key,
            guard: MediaGuard {
                hub: Some(self.clone()),
                id,
                inner,
                device_id: device_id.to_owned(),
            },
        })
    }

    /// Removes a socket; `true` when it was the last one, which closes the hub.
    fn unsubscribe(&self, id: u64) -> bool {
        let mut state = self.lock();
        state.subscribers.retain(|slot| slot.id != id);
        let last = state.subscribers.is_empty() && !state.closed;
        if last {
            state.closed = true;
        }
        last
    }

    fn request_keyframe(&self, id: u64) {
        let mut state = self.lock();
        if let Some(slot) = state.subscribers.iter_mut().find(|slot| slot.id == id) {
            slot.skip_to_key();
            self.maybe_reset(&mut state);
        }
    }

    /// A drained socket that missed its key unit while its queue was full asks for one,
    /// unless a key unit reached it after its reader looked. A key unit clears the wait, so a
    /// socket that still waits has not had one. Its wait is left as it is: starting it again
    /// would drop the deltas that follow a key unit the client already has.
    fn request_missed_key(&self, id: u64) {
        let mut state = self.lock();
        if state
            .subscribers
            .iter()
            .any(|slot| slot.id == id && slot.waiting_for_key)
        {
            self.maybe_reset(&mut state);
        }
    }

    /// Asks the encoder for a fresh config and key unit, at most once per
    /// [`RESET_MIN_INTERVAL`] per device. The window runs from the write: the stamp taken
    /// here gates later requests until the task writes the reset and stamps it again (a
    /// reset made while the source starts is written much later). A request inside the
    /// window is deferred, never dropped: when the window ends, the hub's task sends one
    /// reset for every request made in it, if a socket still waits for a key unit then. A
    /// still screen encodes nothing unasked (iOS, and the Android emulator's encoder), so a
    /// viewer that joins, or a decoder that recovers, inside the window would otherwise
    /// wait for a key unit forever.
    fn maybe_reset(&self, state: &mut HubState) {
        let now = Instant::now();
        if state
            .last_reset
            .is_some_and(|last| now.duration_since(last) < RESET_MIN_INTERVAL)
        {
            if !state.reset_deferred {
                state.reset_deferred = true;
                // Wakes the task to wait for the window's end.
                self.reset.notify_one();
            }
            return;
        }
        Self::start_reset(state, now);
        state.reset_unsent = true;
        self.reset.notify_one();
    }

    fn start_reset(state: &mut HubState, now: Instant) {
        state.last_reset = Some(now);
        state.reset_deferred = false;
        state.resets += 1;
    }

    /// The task's next reset step at `now`. A deferred request that falls due starts a
    /// reset only while some socket still waits for a key unit; either way it is spent.
    fn next_reset(&self, now: Instant) -> ResetStep {
        let mut state = self.lock();
        if state.reset_unsent {
            state.reset_unsent = false;
            // The window runs from the write: the task may write a reset made while the
            // source started long after it was asked for, and a deferral must not follow
            // it at once.
            state.last_reset = Some(now);
            return ResetStep::Write;
        }
        if !state.reset_deferred {
            return ResetStep::Idle;
        }
        if let Some(last) = state.last_reset
            && now < last + RESET_MIN_INTERVAL
        {
            return ResetStep::At(last + RESET_MIN_INTERVAL);
        }
        state.reset_deferred = false;
        if !state.subscribers.iter().any(|slot| slot.waiting_for_key) {
            return ResetStep::Idle;
        }
        Self::start_reset(&mut state, now);
        ResetStep::Write
    }

    /// Tells every socket with nothing queued that the stream is still open. A socket with
    /// frames queued already has its proof of life, so a keepalive never waits behind them
    /// and never grows a stalled socket's queue.
    ///
    /// It also asks for a key unit, under the reset rate limit, when a socket with nothing
    /// queued has waited for one longer than [`WAIT_WATCHDOG`] since its wait started or
    /// the last reset was written, whichever is later. Such a socket reads what it is sent,
    /// yet its key unit was lost on the way, and a still screen sends no other: without
    /// this it would get only keepalives, which its client takes for a live stream. A
    /// socket that stays blocked always has frames queued, so it never asks here.
    fn keepalive(&self) {
        static FRAME: Bytes = Bytes::from_static(&KEEPALIVE_FRAME);
        let now = Instant::now();
        let mut state = self.lock();
        let last_reset = state.last_reset;
        let mut stuck = false;
        state.subscribers.retain(|slot| {
            if slot.queued_bytes.load(Ordering::Acquire) > 0 {
                return true;
            }
            stuck |= slot.waiting_for_key
                && slot
                    .waiting_since
                    .max(last_reset)
                    .is_some_and(|since| now.duration_since(since) > WAIT_WATCHDOG);
            !matches!(slot.offer(&FRAME, None), Offer::Closed)
        });
        if stuck {
            tracing::debug!("a device media socket still waits for a key unit; asking again");
            self.maybe_reset(&mut state);
        }
    }

    /// Sends one unit to every socket: deltas skip sockets waiting for a key unit, and a
    /// socket whose queue is full drops what it has queued and waits for the next key.
    /// A socket that newly falls behind asks for a key unit at once; one already waiting
    /// never asks here. When that one drops a key unit or the config before it, its reader
    /// asks once on draining the queue (see [`MediaSubscriber::next_frame`]), so a socket
    /// that stays blocked never asks again.
    ///
    /// A socket that dropped a config (a rotation's, say) gets the newest config again
    /// immediately before its next key unit, since the client decodes a key unit with the
    /// last config it received. A config that does not fit holds the key unit back with it,
    /// and the socket still needs the config; one that fits is queued and not sent again.
    /// The wait ends only when the key unit is queued, and a socket that misses either
    /// counts as missing its key unit. A socket that lost no config gets no extra one.
    fn dispatch(&self, unit: &VideoUnit) {
        let frame = Bytes::from(encode_frame(unit));
        let delta = unit.kind == UnitKind::Delta;
        let mut state = self.lock();
        state.dispatched += 1;
        if unit.kind == UnitKind::Config {
            state.cached_config = Some(frame.clone());
        }
        // The newest config, for a socket that lost one before this key unit.
        let config = match unit.kind {
            UnitKind::Key => state.cached_config.clone(),
            UnitKind::Config | UnitKind::Delta => None,
        };
        let mut overflowed = false;
        state.subscribers.retain_mut(|slot| {
            if delta && slot.waiting_for_key {
                return true;
            }
            let offered = match unit.kind {
                UnitKind::Key => slot.offer_key(&frame, config.as_ref()),
                UnitKind::Config | UnitKind::Delta => slot.offer(&frame, Some(unit.kind.clone())),
            };
            match offered {
                Offer::Sent => {
                    match unit.kind {
                        UnitKind::Config => slot.needs_config = false,
                        UnitKind::Key => {
                            slot.waiting_for_key = false;
                            slot.waiting_since = None;
                            // A key unit makes good any it missed, so a drain asks for none.
                            slot.missed_key.store(false, Ordering::Release);
                        }
                        UnitKind::Delta => {}
                    }
                    true
                }
                Offer::Overflow => {
                    if unit.kind == UnitKind::Config {
                        slot.needs_config = true;
                    }
                    overflowed |= slot.overflow();
                    true
                }
                Offer::Closed => false,
            }
        });
        if overflowed {
            self.maybe_reset(&mut state);
        }
    }
}

/// What feeds a hub: a backend's video source, or a channel in tests.
pub(super) trait VideoSource: Send + 'static {
    /// The next unit; `None` once the stream ended.
    fn next_unit(&mut self) -> impl Future<Output = Option<VideoUnit>> + Send;
    fn reset_video(&mut self) -> impl Future<Output = Result<(), DeviceError>> + Send;
    /// Whether the stream ended because it failed.
    fn failed(&self) -> bool;
    /// Ends the source; `device_ended` when its device stopped running.
    fn stop(self, device_ended: bool) -> impl Future<Output = ()> + Send;
}

impl<S: VideoSource> ErasedVideoSource for S {
    fn next_unit(&mut self) -> BoxFuture<'_, Option<VideoUnit>> {
        Box::pin(VideoSource::next_unit(self))
    }

    fn reset_video(&mut self) -> BoxFuture<'_, Result<(), DeviceError>> {
        Box::pin(VideoSource::reset_video(self))
    }

    fn failed(&self) -> bool {
        VideoSource::failed(self)
    }

    fn stop(self: Box<Self>, device_ended: bool) -> BoxFuture<'static, ()> {
        Box::pin(VideoSource::stop(*self, device_ended))
    }
}

/// A backend's source, whatever its concrete type.
impl VideoSource for Box<dyn ErasedVideoSource> {
    fn next_unit(&mut self) -> impl Future<Output = Option<VideoUnit>> + Send {
        ErasedVideoSource::next_unit(&mut **self)
    }

    fn reset_video(&mut self) -> impl Future<Output = Result<(), DeviceError>> + Send {
        ErasedVideoSource::reset_video(&mut **self)
    }

    fn failed(&self) -> bool {
        ErasedVideoSource::failed(&**self)
    }

    fn stop(self, device_ended: bool) -> impl Future<Output = ()> + Send {
        ErasedVideoSource::stop(self, device_ended)
    }
}

/// Starts the source, then drains it promptly into the hub until the hub is stopped or the
/// stream ends; the source is stopped on every path.
pub(super) async fn run<S: VideoSource>(
    hub: Arc<MediaHub>,
    start: impl Future<Output = Result<S, DeviceError>> + Send,
) {
    let mut source = tokio::select! {
        () = hub.cancel.cancelled() => {
            // Dropping `start` stops whatever it started.
            hub.close(false);
            return;
        }
        started = start => match started {
            Ok(source) => source,
            Err(error) => {
                tracing::warn!(%error, "device media stream could not start");
                hub.close(true);
                return;
            }
        },
    };
    // A quiet source (an unchanged iOS screen sends nothing) still proves the stream alive
    // to its sockets, which treat a long silence as a stall.
    let mut keepalive_at = Instant::now() + KEEPALIVE_INTERVAL;
    // When a deferred reset falls due; the one deadline the reset rate limit needs.
    let mut reset_at: Option<Instant> = None;
    let failed = loop {
        tokio::select! {
            biased;
            () = hub.cancel.cancelled() => break false,
            () = hub.reset.notified() => reset_at = serve_resets(&hub, &mut source).await,
            () = tokio::time::sleep_until(reset_at.unwrap_or_else(Instant::now)),
                if reset_at.is_some() =>
            {
                reset_at = serve_resets(&hub, &mut source).await;
            }
            unit = source.next_unit() => match unit {
                Some(unit) => {
                    hub.dispatch(&unit);
                    keepalive_at = Instant::now() + KEEPALIVE_INTERVAL;
                }
                None => break source.failed(),
            },
            () = tokio::time::sleep_until(keepalive_at) => {
                hub.keepalive();
                keepalive_at = Instant::now() + KEEPALIVE_INTERVAL;
            }
        }
    };
    if failed {
        tracing::warn!("device media stream failed; closing its sockets");
    }
    hub.close(failed);
    source.stop(hub.device_ended.load(Ordering::Acquire)).await;
}

/// Writes each reset the hub asks for now; returns when a deferred one falls due, if any.
/// A write that fails or times out is deferred to the end of its window, so it is tried
/// again then if a socket still waits for a key unit.
async fn serve_resets<S: VideoSource>(hub: &MediaHub, source: &mut S) -> Option<Instant> {
    loop {
        match hub.next_reset(Instant::now()) {
            ResetStep::Write => {
                let written =
                    match tokio::time::timeout(RESET_WRITE_TIMEOUT, source.reset_video()).await {
                        Ok(Ok(())) => true,
                        Ok(Err(error)) => {
                            tracing::warn!(%error, "device media reset failed");
                            false
                        }
                        Err(_) => {
                            tracing::warn!("device media reset timed out");
                            false
                        }
                    };
                if !written {
                    hub.lock().reset_deferred = true;
                }
            }
            ResetStep::At(deadline) => return Some(deadline),
            ResetStep::Idle => return None,
        }
    }
}

/// One media socket's view of a device's stream.
pub(crate) struct MediaSubscriber {
    frames: mpsc::Receiver<QueuedFrame>,
    /// Cancelled when the stream ends for this socket.
    pub closed: CancellationToken,
    queued_bytes: Arc<AtomicUsize>,
    /// The slot's [`SubscriberSlot::media_bytes`].
    media_bytes: Arc<AtomicUsize>,
    generation: Arc<AtomicU64>,
    /// The slot's [`SubscriberSlot::missed_key`].
    missed_key: Arc<AtomicBool>,
    guard: MediaGuard,
}

impl MediaSubscriber {
    /// The next encoded frame; deltas queued before a drop are skipped. `None` once the hub
    /// closed this socket and its queue is empty.
    ///
    /// Taking the last queued frame of a socket that missed its key unit while its queue was
    /// full asks for a key unit once, under the hub's reset rate limit, if none has reached
    /// the socket since: the queue has room for the answer now, and a still screen would
    /// send no other.
    pub(crate) async fn next_frame(&mut self) -> Option<Bytes> {
        loop {
            let frame = self.frames.recv().await?;
            let queued = self
                .queued_bytes
                .fetch_sub(frame.bytes.len(), Ordering::AcqRel);
            if frame.media {
                self.media_bytes
                    .fetch_sub(frame.bytes.len(), Ordering::AcqRel);
            }
            if queued == frame.bytes.len()
                && self.missed_key.swap(false, Ordering::AcqRel)
                && let Some(hub) = &self.guard.hub
            {
                hub.request_missed_key(self.guard.id);
            }
            if frame.delta && frame.generation < self.generation.load(Ordering::Acquire) {
                continue;
            }
            return Some(frame.bytes);
        }
    }

    /// After a decoder error: drop the queued deltas, wait for the next key unit, and ask the
    /// encoder for one under the hub's reset rate limit. The drain in [`Self::next_frame`]
    /// does not call this: it leaves the wait alone.
    pub(crate) fn request_keyframe(&self) {
        if let Some(hub) = &self.guard.hub {
            hub.request_keyframe(self.guard.id);
        }
    }

    /// Whether the stream ended because its source failed.
    pub(crate) fn failed(&self) -> bool {
        self.guard.hub.as_ref().is_some_and(|hub| hub.failed())
    }

    /// The media socket's close reason once the stream ended: `failed` when its source
    /// failed, otherwise `stopped`, or `disabled` when device previews are off.
    pub(crate) fn end_reason(&self, enabled: bool) -> &'static str {
        if self.failed() {
            "failed"
        } else if enabled {
            "stopped"
        } else {
            "disabled"
        }
    }
}

/// Leaves the hub on drop; the last socket to leave stops it.
struct MediaGuard {
    /// Always `Some` until dropped.
    hub: Option<Arc<MediaHub>>,
    id: u64,
    inner: Weak<Inner>,
    device_id: String,
}

impl Drop for MediaGuard {
    fn drop(&mut self) {
        let Some(hub) = self.hub.take() else {
            return;
        };
        // Attaching takes the table lock too, so it cannot join a hub this drop closes.
        let inner = self.inner.upgrade();
        let mut table = inner.as_ref().map(|inner| inner.table());
        if hub.unsubscribe(self.id) {
            hub.stop();
            if let Some(slot) = table
                .as_mut()
                .and_then(|table| table.slots.get_mut(&self.device_id))
                && slot
                    .media
                    .as_ref()
                    .is_some_and(|media| Arc::ptr_eq(media, &hub))
            {
                slot.media = None;
            }
        }
        drop(table);
    }
}

// ---------------------------------------------------------------------------------------------
// Manager

/// Milliseconds since the epoch, as tickets and sessions keep their expiries.
pub(super) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
        .unwrap_or_default()
}

impl DeviceManager {
    /// A single-use ticket that lets one media socket on `channel` watch a `Ready` device,
    /// and its expiry in epoch milliseconds.
    pub fn create_media_ticket(
        &self,
        device_id: &str,
        principal: &str,
        channel: MediaChannel,
    ) -> Result<(String, i64), DeviceOpError> {
        if !self.enabled() {
            return Err(DeviceOpError::Disabled);
        }
        let (device, _) = self.inner.running(device_id)?;
        if let Some(reason) = device.video_unavailable() {
            return Err(DeviceOpError::NoVideo(reason));
        }
        drop(device);
        let issued_ms = now_ms();
        let (ticket, _) = self
            .inner
            .tickets
            .issue(device_id, principal, channel, Instant::now())
            .map_err(|error| {
                tracing::warn!(%error, "device media ticket could not be issued");
                DeviceOpError::Emulator(
                    "The server could not create a video ticket. Try again.".to_owned(),
                )
            })?;
        let ttl_ms = i64::try_from(TICKET_TTL.as_millis()).unwrap_or(i64::MAX);
        Ok((ticket, issued_ms.saturating_add(ttl_ms)))
    }

    /// The device and principal a ticket was minted for; a device this manager no longer
    /// knows is the wrong device.
    pub(crate) fn redeem_media_ticket(
        &self,
        ticket: &str,
        channel: MediaChannel,
    ) -> Result<(String, String), TicketError> {
        let (device_id, principal) = self.inner.tickets.redeem(ticket, channel, Instant::now())?;
        if !self.inner.table().slots.contains_key(&device_id) {
            return Err(TicketError::WrongDevice);
        }
        Ok((device_id, principal))
    }

    /// Joins a `Ready` device's H.264 stream, starting its video source for the first
    /// socket.
    pub(crate) fn attach_media(
        &self,
        device_id: &str,
    ) -> Result<MediaSubscriber, MediaAttachError> {
        if !self.enabled() {
            return Err(MediaAttachError::Disabled);
        }
        let mut table = self.inner.table();
        let slot = table
            .slots
            .get_mut(device_id)
            .ok_or(MediaAttachError::NotReady)?;
        let Some(device) = slot
            .running
            .clone()
            .filter(|_| slot.snapshot.state == super::catalog::DeviceState::Ready)
        else {
            return Err(MediaAttachError::NotReady);
        };
        let cancel = slot.cancel.clone();
        let start = || {
            let hub = MediaHub::new(cancel.child_token());
            let task_hub = hub.clone();
            let inner = Arc::downgrade(&self.inner);
            let task_device = device_id.to_owned();
            let device = device.clone();
            // Holds the device only until its source starts; a stop drops `start`. Boxed
            // here, where the source type is concrete, so the task below stays `Send`.
            let start = async move { device.video().await };
            let feed: BoxFuture<'static, ()> = Box::pin(run(task_hub.clone(), start));
            self.inner.tasks.spawn(async move {
                feed.await;
                // A hub that ended on its own makes room for a fresh one.
                if let Some(inner) = inner.upgrade() {
                    let mut table = inner.table();
                    if let Some(slot) = table.slots.get_mut(&task_device)
                        && slot
                            .media
                            .as_ref()
                            .is_some_and(|media| Arc::ptr_eq(media, &task_hub))
                    {
                        slot.media = None;
                    }
                }
            });
            hub
        };
        join_hub(&mut slot.media, start, |hub| {
            hub.subscribe(SOCKET_QUEUE_FRAMES, Arc::downgrade(&self.inner), device_id)
        })
    }

    /// Joins a device's H.264 stream as a media socket would, without a socket. Only for
    /// the opt-in real-device tests (`tests/ios_simulator.rs`).
    #[doc(hidden)]
    pub fn media_frames_for_tests(&self, device_id: &str) -> Option<MediaFramesForTests> {
        self.attach_media(device_id).ok().map(MediaFramesForTests)
    }
}

/// Joins `media`, a device's hub, while it is open, else a fresh one from `start`, which
/// takes its place. A hub's task closes it without the table lock (its source failed or
/// ended), so a hub found open can close before the join; it is replaced by a fresh one,
/// which the join tries once more, since the device is still ready.
fn join_hub(
    media: &mut Option<Arc<MediaHub>>,
    mut start: impl FnMut() -> Arc<MediaHub>,
    mut join: impl FnMut(&Arc<MediaHub>) -> Result<MediaSubscriber, MediaAttachError>,
) -> Result<MediaSubscriber, MediaAttachError> {
    let mut fresh = |media: &mut Option<Arc<MediaHub>>| {
        let hub = start();
        *media = Some(hub.clone());
        hub
    };
    let reused = match media {
        Some(hub) if hub.is_open() => hub.clone(),
        _ => return join(&fresh(media)),
    };
    match join(&reused) {
        Err(MediaAttachError::NotReady) => {
            tracing::debug!("a device media hub closed as a socket joined; starting a fresh one");
            join(&fresh(media))
        }
        joined => joined,
    }
}

/// A media socket's frames without the socket; see
/// [`DeviceManager::media_frames_for_tests`].
#[doc(hidden)]
pub struct MediaFramesForTests(MediaSubscriber);

impl MediaFramesForTests {
    /// The next frame as a socket sends it: `[kind u8][pts u64 BE][Annex B data]`.
    pub async fn next_frame(&mut self) -> Option<Bytes> {
        self.0.next_frame().await
    }

    /// Waits for the stream to end and returns the reason a media socket would close with.
    pub async fn end_reason(mut self, devices: &DeviceManager) -> &'static str {
        while self.0.next_frame().await.is_some() {}
        self.0.closed.cancelled().await;
        self.0.end_reason(devices.enabled())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Instant {
        Instant::now()
    }

    fn unit(kind: UnitKind, pts_us: u64) -> VideoUnit {
        VideoUnit {
            kind,
            pts_us,
            data: Bytes::from_static(&[0, 0, 0, 1, 0x41]),
        }
    }

    fn config() -> VideoUnit {
        unit(UnitKind::Config, 0)
    }

    fn key(pts_us: u64) -> VideoUnit {
        unit(UnitKind::Key, pts_us)
    }

    fn delta(pts_us: u64) -> VideoUnit {
        unit(UnitKind::Delta, pts_us)
    }

    /// How a [`ChannelSource`] takes its first `RESET_VIDEO` write; later ones succeed.
    #[derive(Clone, Copy, Default)]
    enum FirstReset {
        #[default]
        Succeeds,
        Fails,
        /// Takes longer than [`RESET_WRITE_TIMEOUT`].
        Stalls,
    }

    /// A video source fed through a channel, counting the resets it is asked for and
    /// recording how it was stopped.
    struct ChannelSource {
        units: mpsc::Receiver<VideoUnit>,
        resets: Arc<AtomicUsize>,
        stopped: Arc<std::sync::OnceLock<bool>>,
        first_reset: FirstReset,
    }

    impl VideoSource for ChannelSource {
        async fn next_unit(&mut self) -> Option<VideoUnit> {
            self.units.recv().await
        }

        async fn reset_video(&mut self) -> Result<(), DeviceError> {
            let earlier = self.resets.fetch_add(1, Ordering::AcqRel);
            match (earlier, self.first_reset) {
                (0, FirstReset::Fails) => Err(DeviceError::Io("control socket closed".to_owned())),
                (0, FirstReset::Stalls) => {
                    tokio::time::sleep(RESET_WRITE_TIMEOUT * 5).await;
                    Ok(())
                }
                _ => Ok(()),
            }
        }

        fn failed(&self) -> bool {
            false
        }

        async fn stop(self, device_ended: bool) {
            let _ = self.stopped.set(device_ended);
        }
    }

    /// Drives a [`MediaHub`] from a channel-fed source.
    struct HubFixture {
        hub: Arc<MediaHub>,
        units: mpsc::Sender<VideoUnit>,
        fed: std::cell::Cell<u64>,
        /// `RESET_VIDEO` writes the source received.
        written: Arc<AtomicUsize>,
        /// Whether the source was stopped for an ended device, once it was stopped.
        stopped: Arc<std::sync::OnceLock<bool>>,
    }

    struct Watcher {
        subscriber: MediaSubscriber,
    }

    impl HubFixture {
        fn new() -> Self {
            Self::starting_after(Duration::ZERO)
        }

        /// A hub whose source takes `delay` to start, as scrcpy does.
        fn starting_after(delay: Duration) -> Self {
            Self::with_source(delay, FirstReset::Succeeds)
        }

        /// A hub whose source's first `RESET_VIDEO` write goes as `first_reset` says.
        fn with_first_reset(first_reset: FirstReset) -> Self {
            Self::with_source(Duration::ZERO, first_reset)
        }

        fn with_source(delay: Duration, first_reset: FirstReset) -> Self {
            let hub = MediaHub::new(CancellationToken::new());
            let (units, receiver) = mpsc::channel(64);
            let written = Arc::<AtomicUsize>::default();
            let stopped = Arc::<std::sync::OnceLock<bool>>::default();
            let source = ChannelSource {
                units: receiver,
                resets: written.clone(),
                stopped: stopped.clone(),
                first_reset,
            };
            tokio::spawn(run(hub.clone(), async move {
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                Ok(source)
            }));
            Self {
                hub,
                units,
                fed: std::cell::Cell::new(0),
                written,
                stopped,
            }
        }

        fn try_subscribe(&self) -> Result<Watcher, MediaAttachError> {
            self.subscribe_with_capacity_result(SOCKET_QUEUE_FRAMES)
        }

        fn subscribe(&self) -> Watcher {
            self.try_subscribe().expect("subscribe")
        }

        fn subscribe_with_capacity(&self, capacity: usize) -> Watcher {
            self.subscribe_with_capacity_result(capacity)
                .expect("subscribe")
        }

        fn subscribe_with_capacity_result(
            &self,
            capacity: usize,
        ) -> Result<Watcher, MediaAttachError> {
            self.hub
                .subscribe(capacity, Weak::new(), "dev")
                .map(|subscriber| Watcher { subscriber })
        }

        /// Feeds `units` and waits until the hub has fanned out every one.
        async fn feed(&self, units: impl IntoIterator<Item = VideoUnit>) {
            for unit in units {
                self.units.send(unit).await.expect("hub running");
                self.fed.set(self.fed.get() + 1);
            }
            while self.hub.lock().dispatched < self.fed.get() {
                tokio::task::yield_now().await;
            }
        }

        fn resets(&self) -> u64 {
            self.hub.lock().resets
        }

        /// Waits, as paused time allows, until the source has received `count` resets.
        async fn wait_written(&self, count: usize) {
            for _ in 0..500 {
                if self.written.load(Ordering::Acquire) >= count {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!(
                "the source received {} resets, not {count}",
                self.written.load(Ordering::Acquire)
            );
        }
    }

    impl Watcher {
        /// Every frame queued for this socket, as `(kind, pts)`.
        async fn received(&mut self) -> Vec<(&'static str, u64)> {
            let mut received = Vec::new();
            while let Ok(Some(frame)) =
                tokio::time::timeout(Duration::from_millis(50), self.subscriber.next_frame()).await
            {
                let kind = match frame[0] {
                    0 => "config",
                    1 => "key",
                    2 => "delta",
                    3 => "keepalive",
                    other => panic!("unknown kind {other}"),
                };
                let pts = u64::from_be_bytes(frame[1..9].try_into().expect("pts"));
                received.push((kind, pts));
            }
            received
        }

        async fn kinds(&mut self) -> Vec<&'static str> {
            self.received()
                .await
                .into_iter()
                .map(|(kind, _)| kind)
                .collect()
        }
    }

    #[tokio::test]
    async fn an_erased_source_feeds_the_hub_like_the_concrete_one() {
        let (units, receiver) = mpsc::channel(8);
        let stopped = Arc::<std::sync::OnceLock<bool>>::default();
        let source = ChannelSource {
            units: receiver,
            resets: Arc::default(),
            stopped: stopped.clone(),
            first_reset: FirstReset::Succeeds,
        };
        let erased: Box<dyn crate::device::backend::ErasedVideoSource> = Box::new(source);
        let hub = MediaHub::new(CancellationToken::new());
        // Subscribed first, so the socket sees the config and the key, not only the cache.
        let mut watcher = Watcher {
            subscriber: hub
                .subscribe(SOCKET_QUEUE_FRAMES, Weak::new(), "dev")
                .expect("subscribe"),
        };
        let task = tokio::spawn(run(hub.clone(), async move { Ok(erased) }));
        units.send(config()).await.unwrap();
        units.send(key(1)).await.unwrap();
        assert_eq!(watcher.received().await, vec![("config", 0), ("key", 1)]);
        hub.stop();
        task.await.unwrap();
        // The erased source was stopped through its concrete `stop`.
        assert_eq!(stopped.get(), Some(&false));
    }

    #[test]
    fn a_keepalive_is_kind_3_with_no_payload() {
        // The bytes the TypeScript parser's keepalive case reads, too.
        assert_eq!(&KEEPALIVE_FRAME[..], &[3, 0, 0, 0, 0, 0, 0, 0, 0]);
    }

    /// The next frame's kind and when it arrived, waiting up to `within`.
    async fn next_kind(watcher: &mut Watcher, within: Duration) -> Option<(u8, Instant)> {
        tokio::time::timeout(within, watcher.subscriber.next_frame())
            .await
            .ok()
            .flatten()
            .map(|frame| (frame[0], Instant::now()))
    }

    #[tokio::test(start_paused = true)]
    async fn a_quiet_source_keeps_its_sockets_alive_once_a_second() {
        let hub = HubFixture::new();
        let mut watcher = hub.subscribe();
        hub.feed([config(), key(1)]).await;
        let fed = Instant::now();
        assert_eq!(watcher.kinds().await, ["config", "key"]);
        // Nothing more from the source: a keepalive within about a second, then one a second.
        let (kind, first) = next_kind(&mut watcher, Duration::from_millis(1_100))
            .await
            .expect("a keepalive within 1.1 s");
        assert_eq!(kind, 3);
        assert!(first - fed >= KEEPALIVE_INTERVAL);
        let (kind, second) = next_kind(&mut watcher, Duration::from_millis(1_100))
            .await
            .expect("another keepalive a second later");
        assert_eq!(kind, 3);
        assert_eq!(second - first, KEEPALIVE_INTERVAL);
        // Stopped: the socket's stream ends and no keepalive follows.
        hub.hub.stop();
        assert_eq!(next_kind(&mut watcher, Duration::from_secs(3)).await, None);
    }

    #[tokio::test(start_paused = true)]
    async fn no_keepalive_while_units_flow() {
        let hub = HubFixture::new();
        let mut watcher = hub.subscribe();
        hub.feed([config(), key(1)]).await;
        // A delta every half second for three seconds: the units alone prove liveness.
        for pts in 2..8 {
            tokio::time::sleep(Duration::from_millis(500)).await;
            hub.feed([delta(pts)]).await;
        }
        let kinds = watcher.kinds().await;
        assert!(!kinds.contains(&"keepalive"), "{kinds:?}");
        assert_eq!(kinds.len(), 8);
    }

    #[tokio::test(start_paused = true)]
    async fn a_keepalive_never_queues_behind_unsent_frames() {
        let hub = HubFixture::new();
        // A socket that reads nothing: its queue holds the config and the key.
        let watcher = hub.subscribe();
        hub.feed([config(), key(1)]).await;
        let queued = watcher.subscriber.queued_bytes.load(Ordering::Acquire);
        tokio::time::sleep(KEEPALIVE_INTERVAL * 3).await;
        assert_eq!(
            watcher.subscriber.queued_bytes.load(Ordering::Acquire),
            queued,
            "no keepalive joins a queue that still holds frames"
        );
    }

    #[test]
    fn frames_encode_kind_and_pts() {
        let frame = encode_frame(&VideoUnit {
            kind: UnitKind::Key,
            pts_us: 7,
            data: Bytes::from_static(&[0, 0, 0, 1, 0x65]),
        });
        assert_eq!(frame[0], 1);
        assert_eq!(u64::from_be_bytes(frame[1..9].try_into().unwrap()), 7);
        assert_eq!(&frame[9..], &[0, 0, 0, 1, 0x65]);
    }

    #[test]
    fn tickets_are_single_use_bound_and_expiring() {
        let tickets = TicketBook::default();
        let (ticket, _) = tickets
            .issue("dev", "session-a", MediaChannel::E2ee, now())
            .unwrap();
        assert_eq!(
            tickets.redeem(&ticket, MediaChannel::Plain, now()),
            Err(TicketError::WrongChannel)
        );
        let (ticket, _) = tickets
            .issue("dev", "session-a", MediaChannel::Plain, now())
            .unwrap();
        assert_eq!(
            tickets.redeem(&ticket, MediaChannel::Plain, now()),
            Ok(("dev".to_owned(), "session-a".to_owned()))
        );
        assert_eq!(
            tickets.redeem(&ticket, MediaChannel::Plain, now()),
            Err(TicketError::Reused)
        );
        let (ticket, _) = tickets
            .issue("dev", "session-a", MediaChannel::Plain, now())
            .unwrap();
        assert_eq!(
            tickets.redeem(
                &ticket,
                MediaChannel::Plain,
                now() + TICKET_TTL + Duration::from_secs(1)
            ),
            Err(TicketError::Expired)
        );
        assert_eq!(
            tickets.redeem("not-a-ticket", MediaChannel::Plain, now()),
            Err(TicketError::Unknown)
        );
    }

    #[test]
    fn tickets_are_256_bit_and_the_book_is_bounded() {
        let tickets = TicketBook::default();
        let (first, _) = tickets
            .issue("dev", "session-a", MediaChannel::Plain, now())
            .unwrap();
        let decoded = base64::Engine::decode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            first.as_bytes(),
        )
        .unwrap();
        assert_eq!(decoded.len(), 32);
        for index in 0..MAX_OUTSTANDING_TICKETS {
            tickets
                .issue(
                    "dev",
                    &format!("session-{index}"),
                    MediaChannel::Plain,
                    now(),
                )
                .unwrap();
        }
        assert_eq!(tickets.table().by_hash.len(), MAX_OUTSTANDING_TICKETS);
        // The oldest made room for the newest.
        assert_eq!(
            tickets.redeem(&first, MediaChannel::Plain, now()),
            Err(TicketError::Unknown)
        );
    }

    #[test]
    fn one_session_cannot_flood_the_ticket_book() {
        let tickets = TicketBook::default();
        let (other, _) = tickets
            .issue("dev", "session-b", MediaChannel::Plain, now())
            .unwrap();
        let (first, _) = tickets
            .issue("dev", "session-a", MediaChannel::Plain, now())
            .unwrap();
        let mut latest = String::new();
        for _ in 0..MAX_OUTSTANDING_TICKETS {
            latest = tickets
                .issue("dev", "session-a", MediaChannel::Plain, now())
                .unwrap()
                .0;
        }
        assert_eq!(tickets.table().by_hash.len(), MAX_TICKETS_PER_PRINCIPAL + 1);
        // The flooding session lost its own oldest ticket, never another session's.
        assert_eq!(
            tickets.redeem(&first, MediaChannel::Plain, now()),
            Err(TicketError::Unknown)
        );
        assert!(tickets.redeem(&latest, MediaChannel::Plain, now()).is_ok());
        assert!(tickets.redeem(&other, MediaChannel::Plain, now()).is_ok());
    }

    #[tokio::test]
    async fn slow_socket_skips_to_next_key_unit() {
        let hub = HubFixture::new();
        let mut fast = hub.subscribe();
        // Room for three frames: config, key 1, and delta 2; delta 3 overflows.
        let mut slow = hub.subscribe_with_capacity(3);
        hub.feed([config(), key(1), delta(2), delta(3)]).await;
        // The slow socket catches up only now.
        let mut slow_received = slow.received().await;
        hub.feed([delta(4), key(5), delta(6)]).await;
        slow_received.extend(slow.received().await);
        assert_eq!(
            fast.kinds().await,
            ["config", "key", "delta", "delta", "delta", "key", "delta"]
        );
        assert_eq!(slow_received.first().map(|(kind, _)| *kind), Some("config"));
        // After the first gap in PTS, the next unit is a key unit.
        let media: Vec<_> = slow_received
            .iter()
            .filter(|(kind, _)| *kind != "config")
            .collect();
        let gap = media
            .windows(2)
            .position(|pair| pair[1].1 != pair[0].1 + 1)
            .expect("the slow socket missed units");
        assert_eq!(media[gap + 1].0, "key", "{slow_received:?}");
        // Delta 2 was queued before the drop and was discarded with it.
        assert_eq!(
            slow_received,
            [("config", 0), ("key", 1), ("key", 5), ("delta", 6)]
        );
        assert!(hub.resets() >= 1);
    }

    #[tokio::test(start_paused = true)]
    async fn overflow_asks_the_encoder_for_a_key_unit() {
        let hub = HubFixture::new();
        let _fast = hub.subscribe();
        // Room for the config and the key unit, which ends its wait; delta 2 overflows.
        let _slow = hub.subscribe_with_capacity(2);
        assert_eq!(hub.resets(), 1);
        hub.feed([config(), key(1)]).await;
        tokio::time::advance(RESET_MIN_INTERVAL).await;
        hub.feed([delta(2)]).await;
        assert_eq!(hub.resets(), 2);
        while hub.written.load(Ordering::Acquire) < 2 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_stalled_socket_asks_for_one_reset_and_never_holds_up_the_rest() {
        let hub = HubFixture::new();
        let mut fast = hub.subscribe();
        // Never read: the config and key 1 fill it for good.
        let _stalled = hub.subscribe_with_capacity(2);
        hub.feed([config(), key(1)]).await;
        let mut expected = vec!["config", "key"];
        for second in 0..5_u64 {
            tokio::time::advance(RESET_MIN_INTERVAL).await;
            let base = second * 10 + 10;
            hub.feed([delta(base), key(base + 1), delta(base + 2), delta(base + 3)])
                .await;
            expected.extend(["delta", "key", "delta", "delta"]);
        }
        // One reset at subscribe, one when it fell behind, none while it stays stalled.
        assert_eq!(hub.resets(), 2);
        assert_eq!(fast.kinds().await, expected);
    }

    /// Fills a two-frame socket with the config and key 1, overflows it with delta 2, which
    /// asks for a reset, then drops the config and key 3 that answer it while the socket's
    /// queue is still full. Returns the socket and the reset count after the overflow.
    async fn drop_the_answer_to_an_overflow(hub: &HubFixture) -> (Watcher, u64) {
        let slow = hub.subscribe_with_capacity(2);
        hub.wait_written(1).await;
        hub.feed([config(), key(1)]).await;
        tokio::time::advance(RESET_MIN_INTERVAL).await;
        hub.feed([delta(2)]).await;
        let after_overflow = hub.resets();
        assert_eq!(
            after_overflow, 2,
            "one at subscribe, one when it fell behind"
        );
        hub.wait_written(2).await;
        hub.feed([config(), key(3)]).await;
        (slow, after_overflow)
    }

    /// Every frame queued for `watcher` except keepalives, which an empty queue gets once
    /// the source has been quiet for a second.
    async fn received_media(watcher: &mut Watcher) -> Vec<(&'static str, u64)> {
        let mut received = watcher.received().await;
        received.retain(|(kind, _)| *kind != "keepalive");
        received
    }

    /// Codex's PR review: the config and key unit that answer an overflow's reset arrive
    /// while the socket's queue is still full, so they are dropped too, and a still screen
    /// sends nothing more. Once its reader drains the queue, the socket asks again.
    #[tokio::test(start_paused = true)]
    async fn a_socket_that_missed_its_key_asks_again_once_it_drains() {
        let hub = HubFixture::new();
        let (mut slow, after_overflow) = drop_the_answer_to_an_overflow(&hub).await;
        // The source goes silent; while the socket reads nothing, it asks for nothing.
        tokio::time::sleep(RESET_MIN_INTERVAL * 3).await;
        assert_eq!(hub.resets(), after_overflow);
        // The reader catches up: it gets what was queued, and draining asks for a key unit.
        assert_eq!(received_media(&mut slow).await, [("config", 0), ("key", 1)]);
        assert_eq!(hub.resets(), after_overflow + 1, "draining asks again");
        hub.wait_written(3).await;
        // The encoder answers, and the socket leaves its key wait: the next delta reaches it.
        hub.feed([config(), key(4)]).await;
        assert_eq!(received_media(&mut slow).await, [("config", 0), ("key", 4)]);
        hub.feed([delta(5)]).await;
        assert_eq!(received_media(&mut slow).await, [("delta", 5)]);
    }

    /// A socket that never reads asks for one reset when it falls behind, and none after
    /// however many key units it drops; subscribing asked for the other one counted.
    #[tokio::test(start_paused = true)]
    async fn a_socket_that_stays_blocked_never_asks_again() {
        let hub = HubFixture::new();
        let (_blocked, after_overflow) = drop_the_answer_to_an_overflow(&hub).await;
        for second in 0..5_u64 {
            tokio::time::advance(RESET_MIN_INTERVAL).await;
            // Each config and key unit is dropped too, and still nothing is asked for.
            hub.feed([config(), key(10 + second)]).await;
        }
        tokio::time::sleep(RESET_MIN_INTERVAL * 3).await;
        assert_eq!(hub.resets(), after_overflow);
        assert_eq!(
            hub.written.load(Ordering::Acquire),
            usize::try_from(after_overflow).unwrap()
        );
        // However long it stays blocked: its queue never empties, so the hub's watchdog for
        // a drained socket that still waits never counts it.
        tokio::time::sleep(RESET_MIN_INTERVAL * 10).await;
        assert_eq!(hub.resets(), after_overflow);
        assert_eq!(
            hub.written.load(Ordering::Acquire),
            usize::try_from(after_overflow).unwrap()
        );
    }

    /// The audit's cross-cutting finding: a socket whose key unit was lost keeps reading
    /// keepalives, so the client sees a live stream, and a still screen sends nothing more.
    /// Once it has waited two reset windows with nothing queued, the hub asks again, then
    /// no more often than that, and not at all once a key unit reached it.
    #[tokio::test(start_paused = true)]
    async fn a_drained_socket_still_waiting_after_two_windows_asks_again() {
        let hub = HubFixture::new();
        let mut watcher = hub.subscribe();
        hub.wait_written(1).await;
        let first = Instant::now();
        // The source answers the reset with nothing; the reader takes every keepalive.
        let reader = tokio::spawn(async move {
            let mut kinds = Vec::new();
            while let Some(frame) = watcher.subscriber.next_frame().await {
                kinds.push(frame[0]);
            }
            kinds
        });
        tokio::time::sleep(RESET_MIN_INTERVAL * 2).await;
        assert_eq!(
            hub.written.load(Ordering::Acquire),
            1,
            "a short wait asks nothing"
        );
        hub.wait_written(2).await;
        let second = Instant::now();
        assert!(
            second - first > RESET_MIN_INTERVAL * 2
                && second - first <= RESET_MIN_INTERVAL * 2 + KEEPALIVE_INTERVAL,
            "asked again after {:?}",
            second - first
        );
        // The next ask waits two windows from this one.
        tokio::time::sleep(RESET_MIN_INTERVAL * 2 - Duration::from_millis(100)).await;
        assert_eq!(hub.written.load(Ordering::Acquire), 2);
        hub.wait_written(3).await;
        // A key unit ends the wait, and the asking with it.
        hub.feed([config(), key(1)]).await;
        let written = hub.written.load(Ordering::Acquire);
        tokio::time::sleep(RESET_MIN_INTERVAL * 6).await;
        assert_eq!(hub.written.load(Ordering::Acquire), written);
        hub.hub.stop();
        let kinds = reader.await.expect("reader");
        let key_at = kinds
            .iter()
            .position(|kind| *kind == 1)
            .expect("the key unit");
        assert!(
            kinds[..key_at - 1].iter().all(|kind| *kind == 3) && kinds[key_at - 1] == 0,
            "keepalives while it waited, then the config and key: {kinds:?}"
        );
        assert!(kinds[..key_at].len() > 3, "{kinds:?}");
    }

    /// A dropped config is made good by the key unit that follows it, with the config sent
    /// again just before it, so draining after that asks for nothing and the socket keeps
    /// its deltas.
    #[tokio::test(start_paused = true)]
    async fn a_key_that_lands_after_a_dropped_config_ends_the_miss() {
        let hub = HubFixture::new();
        let mut slow = hub.subscribe_with_capacity(3);
        hub.wait_written(1).await;
        hub.feed([config(), key(1)]).await;
        tokio::time::advance(RESET_MIN_INTERVAL).await;
        hub.feed([delta(2), delta(3)]).await;
        hub.wait_written(2).await;
        // The answer's config is dropped; the reader then takes two frames, so the config
        // sent again and its key both fit.
        hub.feed([config()]).await;
        for kind in [0, 1] {
            assert_eq!(
                slow.subscriber.next_frame().await.map(|frame| frame[0]),
                Some(kind)
            );
        }
        hub.feed([key(3)]).await;
        tokio::time::advance(RESET_MIN_INTERVAL).await;
        assert_eq!(received_media(&mut slow).await, [("config", 0), ("key", 3)]);
        hub.feed([delta(4)]).await;
        assert_eq!(received_media(&mut slow).await, [("delta", 4)]);
        assert_eq!(hub.resets(), 2, "the drain asked for nothing");
    }

    /// A config whose SPS and PPS differ from [`config`]'s, as after a rotation. Its pts 7
    /// tells it apart in [`Watcher::received`]; the hub sends a config frame as it got it.
    fn rotated_config() -> VideoUnit {
        VideoUnit {
            kind: UnitKind::Config,
            pts_us: 7,
            data: Bytes::from_static(&[0, 0, 0, 1, 0x67, 0x64, 0, 0, 0, 1, 0x68, 0xee]),
        }
    }

    /// Fills a two-frame socket with config 0 and key 1, then rotates the screen: the
    /// rotated config arrives while the queue is still full, so the socket drops it and
    /// falls behind, which asks for a reset.
    async fn drop_a_rotated_config(hub: &HubFixture) -> Watcher {
        let slow = hub.subscribe_with_capacity(2);
        hub.wait_written(1).await;
        hub.feed([config(), key(1)]).await;
        tokio::time::advance(RESET_MIN_INTERVAL).await;
        hub.feed([rotated_config()]).await;
        assert_eq!(hub.resets(), 2, "one at subscribe, one when it fell behind");
        hub.wait_written(2).await;
        slow
    }

    /// Codex's PR review: a socket drops a rotation's changed config while its queue is
    /// full, then has room for the key unit that follows. The client decodes a key unit
    /// with the last config it received, so the rotated config must come just before it.
    #[tokio::test(start_paused = true)]
    async fn a_socket_that_dropped_a_rotated_config_gets_it_before_the_next_key() {
        let hub = HubFixture::new();
        let mut slow = drop_a_rotated_config(&hub).await;
        assert_eq!(received_media(&mut slow).await, [("config", 0), ("key", 1)]);
        hub.feed([key(5)]).await;
        assert_eq!(
            received_media(&mut slow).await,
            [("config", 7), ("key", 5)],
            "the rotated config goes just before the key unit"
        );
        // Both reached it, so its wait is over: the next delta does too.
        hub.feed([delta(6)]).await;
        assert_eq!(received_media(&mut slow).await, [("delta", 6)]);
        assert_eq!(hub.resets(), 2);
    }

    /// The config sent again does not fit either: the key unit is held back with it, the
    /// socket keeps waiting and still needs the config, and its drain asks for a key unit.
    #[tokio::test(start_paused = true)]
    async fn a_resent_config_that_does_not_fit_keeps_the_socket_waiting() {
        let hub = HubFixture::new();
        let mut slow = drop_a_rotated_config(&hub).await;
        // Key 3 comes while the queue is still full, so neither it nor the config goes in.
        hub.feed([key(3), delta(4)]).await;
        tokio::time::sleep(RESET_MIN_INTERVAL * 3).await;
        assert_eq!(hub.resets(), 2, "a blocked socket asks for nothing");
        assert_eq!(received_media(&mut slow).await, [("config", 0), ("key", 1)]);
        assert_eq!(hub.resets(), 3, "draining asks again");
        hub.wait_written(3).await;
        // The answer's key unit comes after the rotated config, even with no config of its
        // own.
        hub.feed([key(5)]).await;
        assert_eq!(received_media(&mut slow).await, [("config", 7), ("key", 5)]);
        hub.feed([delta(6)]).await;
        assert_eq!(received_media(&mut slow).await, [("delta", 6)]);
    }

    /// The config sent again fits but its key unit does not. The socket keeps waiting and
    /// its drain asks again; the config is queued and reaches the client, so the next key
    /// unit comes without another copy.
    #[tokio::test(start_paused = true)]
    async fn a_resent_config_that_fits_without_its_key_is_not_sent_twice() {
        let hub = HubFixture::new();
        let mut slow = drop_a_rotated_config(&hub).await;
        // The reader takes one frame: room for the config, none for key 3.
        assert_eq!(
            slow.subscriber.next_frame().await.map(|frame| frame[0]),
            Some(0)
        );
        hub.feed([key(3), delta(4)]).await;
        assert_eq!(received_media(&mut slow).await, [("key", 1), ("config", 7)]);
        // Draining asks again, when the window of the last reset ends.
        hub.wait_written(3).await;
        assert_eq!(hub.resets(), 3);
        hub.feed([key(5)]).await;
        assert_eq!(received_media(&mut slow).await, [("key", 5)]);
        hub.feed([delta(6)]).await;
        assert_eq!(received_media(&mut slow).await, [("delta", 6)]);
    }

    /// A socket that lost no config gets none extra: the one beside the socket that dropped
    /// the rotated config sees each config once, and so does that socket once it has it.
    #[tokio::test(start_paused = true)]
    async fn a_socket_that_lost_no_config_gets_no_extra_one() {
        let hub = HubFixture::new();
        let mut fast = hub.subscribe();
        let mut slow = drop_a_rotated_config(&hub).await;
        assert_eq!(received_media(&mut slow).await, [("config", 0), ("key", 1)]);
        hub.feed([key(5)]).await;
        assert_eq!(received_media(&mut slow).await, [("config", 7), ("key", 5)]);
        hub.feed([delta(6), key(8)]).await;
        assert_eq!(received_media(&mut slow).await, [("delta", 6), ("key", 8)]);
        assert_eq!(
            received_media(&mut fast).await,
            [
                ("config", 0),
                ("key", 1),
                ("config", 7),
                ("key", 5),
                ("delta", 6),
                ("key", 8)
            ]
        );
    }

    /// A key unit can reach the socket between its reader taking the last frame (which
    /// found the miss) and the reader's request. The request then leaves the wait alone:
    /// it neither drops the deltas that follow the key unit nor asks the encoder again.
    #[tokio::test(start_paused = true)]
    async fn a_key_that_lands_before_the_drains_request_keeps_the_socket_out_of_the_wait() {
        let hub = HubFixture::new();
        let mut watcher = hub.subscribe();
        hub.wait_written(1).await;
        // The socket has its config and key unit, so it is out of the key wait.
        hub.feed([config(), key(1)]).await;
        tokio::time::advance(RESET_MIN_INTERVAL).await;
        // The request of a reader that found a miss before that key unit landed.
        hub.hub.request_missed_key(watcher.subscriber.guard.id);
        hub.feed([delta(2)]).await;
        assert_eq!(
            received_media(&mut watcher).await,
            [("config", 0), ("key", 1), ("delta", 2)],
            "the request put the socket back into its key wait"
        );
        tokio::time::sleep(RESET_MIN_INTERVAL * 2).await;
        assert_eq!(hub.resets(), 1, "the request asked the encoder again");
        assert_eq!(hub.written.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn a_full_byte_budget_skips_to_the_next_key_unit() {
        let large = |kind, pts_us| VideoUnit {
            kind,
            pts_us,
            data: Bytes::from(vec![0_u8; 900 * 1024]),
        };
        let hub = HubFixture::new();
        let mut watcher = hub.subscribe();
        // Two large units fit in the 2 MiB budget; the third does not.
        hub.feed([
            config(),
            large(UnitKind::Key, 1),
            large(UnitKind::Delta, 2),
            large(UnitKind::Delta, 3),
        ])
        .await;
        assert_eq!(watcher.received().await, [("config", 0), ("key", 1)]);
        hub.feed([large(UnitKind::Delta, 4), large(UnitKind::Key, 5)])
            .await;
        assert_eq!(watcher.received().await, [("key", 5)]);
    }

    /// A unit of `kind` with `bytes` of payload.
    fn sized(kind: UnitKind, pts_us: u64, bytes: usize) -> VideoUnit {
        VideoUnit {
            kind,
            pts_us,
            data: Bytes::from(vec![0_u8; bytes]),
        }
    }

    /// The audit's footnote and Codex's PR review: a config always goes out just before its
    /// key unit, so the "an empty queue takes any unit" rule never applied to a key unit,
    /// and one over the byte budget overflowed an idle socket every time. Only configs and
    /// keepalives queued ahead of it now leave the queue counted as empty.
    #[tokio::test(start_paused = true)]
    async fn an_idle_socket_takes_its_config_and_a_key_unit_over_the_budget() {
        let hub = HubFixture::new();
        let mut watcher = hub.subscribe();
        // A keepalive joins the empty queue first, and the reader runs only at the end.
        tokio::time::sleep(KEEPALIVE_INTERVAL + Duration::from_millis(100)).await;
        hub.feed([config(), sized(UnitKind::Key, 1, SOCKET_QUEUE_BYTES + 1024)])
            .await;
        assert_eq!(
            watcher.received().await,
            [("keepalive", 0), ("config", 0), ("key", 1)]
        );
        hub.feed([delta(2)]).await;
        assert_eq!(watcher.received().await, [("delta", 2)]);
    }

    /// The exception covers only configs and keepalives: a socket with a key unit or a
    /// delta still queued overflows on a key unit that does not fit, and deltas keep the
    /// byte budget as before.
    #[tokio::test(start_paused = true)]
    async fn a_socket_with_media_queued_still_overflows_on_a_key_unit_over_the_budget() {
        let hub = HubFixture::new();
        let mut watcher = hub.subscribe();
        hub.feed([config(), key(1), delta(2)]).await;
        hub.feed([sized(UnitKind::Key, 3, SOCKET_QUEUE_BYTES)])
            .await;
        assert_eq!(
            received_media(&mut watcher).await,
            [("config", 0), ("key", 1)],
            "the key unit overflowed and delta 2 was dropped with it"
        );
        // A delta over the budget after a config still overflows a socket that holds a key.
        hub.feed([config(), key(4)]).await;
        hub.feed([sized(UnitKind::Delta, 5, SOCKET_QUEUE_BYTES)])
            .await;
        assert_eq!(
            received_media(&mut watcher).await,
            [("config", 0), ("key", 4)]
        );
    }

    #[tokio::test]
    async fn late_joiner_gets_the_cached_config_then_waits_for_a_key() {
        let hub = HubFixture::new();
        let _first = hub.subscribe();
        hub.feed([config(), key(1), delta(2)]).await;
        let mut late = hub.subscribe();
        hub.feed([delta(3), key(4)]).await;
        assert_eq!(late.kinds().await, ["config", "key"]);
    }

    #[tokio::test]
    async fn keyframe_request_skips_deltas_until_the_next_key() {
        let hub = HubFixture::new();
        let mut watcher = hub.subscribe();
        hub.feed([config(), key(1), delta(2)]).await;
        watcher.subscriber.request_keyframe();
        hub.feed([delta(3), key(4), delta(5)]).await;
        assert_eq!(
            watcher.kinds().await,
            ["config", "key", "key", "delta"],
            "delta 2 was queued before the request and is dropped with it"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn resets_are_rate_limited_per_device() {
        let hub = HubFixture::new();
        let mut held: Vec<_> = (0..MAX_SOCKETS_PER_DEVICE)
            .map(|_| hub.subscribe())
            .collect();
        assert_eq!(hub.resets(), 1);
        // The window runs from the write, so the task writes the first reset before it ends.
        hub.wait_written(1).await;
        held.pop();
        tokio::time::advance(RESET_MIN_INTERVAL).await;
        held.push(hub.subscribe());
        assert_eq!(hub.resets(), 2);
    }

    /// Codex's case: a second viewer joins a still screen just after the first one's key
    /// unit. Its reset falls inside the window, and the screen sends nothing unasked, so
    /// the reset is sent when the window ends and the key unit follows.
    #[tokio::test(start_paused = true)]
    async fn a_reset_asked_for_inside_the_window_is_sent_when_it_ends() {
        let hub = HubFixture::new();
        let started = Instant::now();
        let mut first = hub.subscribe();
        hub.wait_written(1).await;
        hub.feed([config(), key(1)]).await;
        assert_eq!(first.kinds().await, ["config", "key"]);
        let mut second = hub.subscribe();
        assert_eq!(hub.resets(), 1, "the second request is rate limited");
        hub.wait_written(2).await;
        assert!(Instant::now() - started >= RESET_MIN_INTERVAL);
        assert_eq!(hub.resets(), 2);
        // The encoder answers the reset with a fresh config and key unit.
        hub.feed([config(), key(2)]).await;
        assert_eq!(second.kinds().await, ["config", "config", "key"]);
    }

    /// Review round 2, Minor 1: a viewer joins while the source still starts, and a second
    /// one 0.3 s later. The first reset is written only when the source is up, and the
    /// window runs from that write, so the second viewer's deferred reset waits a whole
    /// window after it instead of following it at once.
    #[tokio::test(start_paused = true)]
    async fn the_window_runs_from_the_write_of_a_reset_made_while_the_source_starts() {
        let hub = HubFixture::starting_after(Duration::from_millis(1_500));
        let _first = hub.subscribe();
        tokio::time::advance(Duration::from_millis(300)).await;
        let _second = hub.subscribe();
        assert_eq!(hub.resets(), 1, "the second request is deferred");
        hub.wait_written(1).await;
        let written = Instant::now();
        assert_eq!(
            hub.written.load(Ordering::Acquire),
            1,
            "the deferred reset does not follow the first at once"
        );
        // Both sockets still wait for a key unit, so the deferral is sent when the window
        // that started with the write ends.
        hub.wait_written(2).await;
        assert!(Instant::now() - written >= RESET_MIN_INTERVAL - Duration::from_millis(20));
        assert_eq!(hub.resets(), 2);
    }

    /// Audit Minor 3: a `RESET_VIDEO` write that fails, or stalls past its timeout, is tried
    /// again once the window ends while a socket still waits, instead of being lost. The
    /// socket reads nothing here, so the drained-socket watchdog plays no part.
    #[tokio::test(start_paused = true)]
    async fn a_reset_that_fails_to_write_is_tried_again_while_a_socket_waits() {
        for first_reset in [FirstReset::Fails, FirstReset::Stalls] {
            let hub = HubFixture::with_first_reset(first_reset);
            let _watcher = hub.subscribe();
            hub.wait_written(1).await;
            let first = Instant::now();
            hub.wait_written(2).await;
            let waited = Instant::now() - first;
            assert!(
                waited >= RESET_MIN_INTERVAL - Duration::from_millis(20)
                    && waited <= RESET_MIN_INTERVAL + Duration::from_millis(50),
                "tried again after {waited:?}"
            );
            // The second write went through: nothing more is asked for while no one asks.
            hub.feed([config(), key(1)]).await;
            tokio::time::sleep(RESET_MIN_INTERVAL * 3).await;
            assert_eq!(hub.written.load(Ordering::Acquire), 2);
        }
    }

    /// A failed write is retried only while a socket still waits for a key unit.
    #[tokio::test(start_paused = true)]
    async fn a_failed_reset_is_not_tried_again_once_every_socket_has_its_key() {
        let hub = HubFixture::with_first_reset(FirstReset::Fails);
        let mut watcher = hub.subscribe();
        hub.wait_written(1).await;
        hub.feed([config(), key(1)]).await;
        assert_eq!(
            received_media(&mut watcher).await,
            [("config", 0), ("key", 1)]
        );
        tokio::time::sleep(RESET_MIN_INTERVAL * 3).await;
        assert_eq!(hub.written.load(Ordering::Acquire), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn many_requests_inside_the_window_make_one_reset_when_it_ends() {
        let hub = HubFixture::new();
        let first = hub.subscribe();
        let second = hub.subscribe();
        let _third = hub.subscribe();
        first.subscriber.request_keyframe();
        second.subscriber.request_keyframe();
        assert_eq!(hub.resets(), 1);
        hub.wait_written(2).await;
        // Nothing is asked for after the window, so nothing more is sent.
        tokio::time::sleep(RESET_MIN_INTERVAL * 3).await;
        assert_eq!(hub.resets(), 2);
        assert_eq!(hub.written.load(Ordering::Acquire), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_deferred_reset_is_dropped_once_every_socket_has_its_key() {
        let hub = HubFixture::new();
        let _first = hub.subscribe();
        let _second = hub.subscribe();
        // A key unit reaches both sockets before the window ends.
        hub.feed([config(), key(1)]).await;
        tokio::time::sleep(RESET_MIN_INTERVAL * 2).await;
        assert_eq!(hub.resets(), 1);
        assert_eq!(hub.written.load(Ordering::Acquire), 1);
    }

    /// A fresh hub, counted in `started`, for [`join_hub`]'s tests.
    fn counted_start(started: &mut Vec<Arc<MediaHub>>) -> impl FnMut() -> Arc<MediaHub> + '_ {
        || {
            let hub = MediaHub::new(CancellationToken::new());
            started.push(hub.clone());
            hub
        }
    }

    fn join(hub: &Arc<MediaHub>) -> Result<MediaSubscriber, MediaAttachError> {
        hub.subscribe(SOCKET_QUEUE_FRAMES, Weak::new(), "dev")
    }

    #[tokio::test]
    async fn joining_reuses_an_open_hub_and_replaces_a_closed_one() {
        let open = MediaHub::new(CancellationToken::new());
        let mut media = Some(open.clone());
        let mut started = Vec::new();
        let _first = join_hub(&mut media, counted_start(&mut started), join).expect("join");
        assert!(started.is_empty(), "an open hub is joined as it is");
        assert!(Arc::ptr_eq(media.as_ref().unwrap(), &open));
        open.stop();
        let _second = join_hub(&mut media, counted_start(&mut started), join).expect("join");
        assert_eq!(started.len(), 1);
        assert!(Arc::ptr_eq(media.as_ref().unwrap(), &started[0]));
    }

    /// Audit Minor 5: the hub's task closes it without the table lock, so a hub found open
    /// can close (its source failed or ended) before the socket subscribes. The socket
    /// then joins a fresh hub instead of closing as `stopped` on a device that is still
    /// ready.
    #[tokio::test]
    async fn a_hub_that_closes_as_a_socket_joins_is_replaced() {
        let closing = MediaHub::new(CancellationToken::new());
        let mut media = Some(closing.clone());
        let mut started = Vec::new();
        let mut joins = 0;
        let joined = join_hub(&mut media, counted_start(&mut started), |hub| {
            joins += 1;
            if joins == 1 {
                // The source fails between the open check and the subscribe.
                hub.close(true);
            }
            join(hub)
        });
        assert!(joined.is_ok(), "the socket joined a fresh hub");
        assert_eq!(joins, 2);
        assert_eq!(started.len(), 1);
        assert!(Arc::ptr_eq(media.as_ref().unwrap(), &started[0]));
        assert!(!Arc::ptr_eq(&started[0], &closing));
    }

    #[tokio::test]
    async fn fifth_socket_is_refused() {
        let hub = HubFixture::new();
        let _held: Vec<_> = (0..MAX_SOCKETS_PER_DEVICE)
            .map(|_| hub.subscribe())
            .collect();
        assert!(matches!(hub.try_subscribe(), Err(MediaAttachError::Limit)));
    }

    #[tokio::test]
    async fn last_socket_leaving_stops_the_hub() {
        let hub = HubFixture::new();
        let first = hub.subscribe();
        let second = hub.subscribe();
        drop(first);
        assert!(hub.hub.is_open());
        drop(second);
        assert!(!hub.hub.is_open());
        assert!(hub.hub.cancel.is_cancelled());
    }

    #[tokio::test]
    async fn the_source_learns_whether_its_device_ended() {
        let stopped = |hub: &HubFixture| {
            let stopped = hub.stopped.clone();
            async move {
                for _ in 0..400 {
                    if let Some(device_ended) = stopped.get() {
                        return *device_ended;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                panic!("the source was never stopped");
            }
        };
        // Each source runs before its hub stops.
        let left = HubFixture::new();
        let watcher = left.subscribe();
        left.feed([config()]).await;
        drop(watcher);
        assert!(!stopped(&left).await);
        let ended = HubFixture::new();
        let _watcher = ended.subscribe();
        ended.feed([config()]).await;
        ended.hub.stop_device_ended();
        assert!(stopped(&ended).await);
    }

    #[tokio::test]
    async fn stopping_the_hub_closes_its_sockets() {
        let hub = HubFixture::new();
        let watcher = hub.subscribe();
        hub.hub.stop();
        assert!(watcher.subscriber.closed.is_cancelled());
        assert!(!watcher.subscriber.failed());
        assert!(matches!(
            hub.try_subscribe(),
            Err(MediaAttachError::NotReady)
        ));
    }
}
