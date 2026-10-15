//! H.264 media fan-out: one scrcpy session per device while any media socket watches it, and
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
use sha2::{Digest, Sha256};
use tokio::{
    sync::{Notify, mpsc},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

use super::{
    DeviceError, DeviceManager, DeviceOpError, Inner,
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

/// `[kind u8][pts u64 BE][Annex-B data]`, kind 0 config, 1 key, 2 delta.
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
    bytes: Bytes,
}

/// The hub's end of one socket's queue.
struct SubscriberSlot {
    id: u64,
    sender: mpsc::Sender<QueuedFrame>,
    queued_bytes: Arc<AtomicUsize>,
    /// Bumped when a unit is dropped: deltas queued before it are discarded unsent.
    generation: Arc<AtomicU64>,
    waiting_for_key: bool,
    closed: CancellationToken,
}

enum Offer {
    Sent,
    Overflow,
    Closed,
}

impl SubscriberSlot {
    fn offer(&self, frame: &Bytes, delta: bool) -> Offer {
        let queued = self.queued_bytes.load(Ordering::Acquire);
        // An empty queue always takes the frame, so a unit larger than the budget still
        // reaches an idle socket.
        if queued > 0 && queued + frame.len() > SOCKET_QUEUE_BYTES {
            return Offer::Overflow;
        }
        self.queued_bytes.fetch_add(frame.len(), Ordering::AcqRel);
        let item = QueuedFrame {
            generation: self.generation.load(Ordering::Acquire),
            delta,
            bytes: frame.clone(),
        };
        match self.sender.try_send(item) {
            Ok(()) => Offer::Sent,
            Err(error) => {
                self.queued_bytes.fetch_sub(frame.len(), Ordering::AcqRel);
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
        self.generation.fetch_add(1, Ordering::AcqRel);
    }
}

#[derive(Default)]
struct HubState {
    subscribers: Vec<SubscriberSlot>,
    cached_config: Option<Bytes>,
    last_reset: Option<Instant>,
    /// `RESET_VIDEO` requests made, sent or not yet.
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
        let generation = Arc::new(AtomicU64::new(0));
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
                generation: generation.clone(),
                waiting_for_key: true,
                closed: closed.clone(),
            };
            if let Some(config) = &state.cached_config {
                // A fresh queue: only a closed receiver could refuse it, and it is ours.
                let _ = slot.offer(config, false);
            }
            state.subscribers.push(slot);
            self.maybe_reset(&mut state);
            id
        };
        Ok(MediaSubscriber {
            frames,
            closed,
            queued_bytes,
            generation,
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

    /// Asks the encoder for a fresh config and key unit, at most once per
    /// [`RESET_MIN_INTERVAL`] per device.
    fn maybe_reset(&self, state: &mut HubState) {
        let now = Instant::now();
        if state
            .last_reset
            .is_some_and(|last| now.duration_since(last) < RESET_MIN_INTERVAL)
        {
            return;
        }
        state.last_reset = Some(now);
        state.resets += 1;
        self.reset.notify_one();
    }

    /// Sends one unit to every socket: deltas skip sockets waiting for a key unit, and a
    /// socket whose queue is full drops what it has queued and waits for the next key.
    fn dispatch(&self, unit: &VideoUnit) {
        let frame = Bytes::from(encode_frame(unit));
        let delta = unit.kind == UnitKind::Delta;
        let mut state = self.lock();
        state.dispatched += 1;
        if unit.kind == UnitKind::Config {
            state.cached_config = Some(frame.clone());
        }
        let mut overflowed = false;
        state.subscribers.retain_mut(|slot| {
            if delta && slot.waiting_for_key {
                return true;
            }
            match slot.offer(&frame, delta) {
                Offer::Sent => {
                    if unit.kind == UnitKind::Key {
                        slot.waiting_for_key = false;
                    }
                    true
                }
                Offer::Overflow => {
                    // Only a socket that newly falls behind asks for a key unit: one that
                    // stays stalled must not make the encoder restart every second for
                    // everyone else.
                    overflowed |= !slot.waiting_for_key;
                    slot.skip_to_key();
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

/// What feeds a hub: the scrcpy session, or a channel in tests.
pub(super) trait VideoSource: Send + 'static {
    /// The next unit; `None` once the stream ended.
    fn next_unit(&mut self) -> impl Future<Output = Option<VideoUnit>> + Send;
    fn reset_video(&mut self) -> impl Future<Output = Result<(), DeviceError>> + Send;
    /// Whether the stream ended because it failed.
    fn failed(&self) -> bool;
    /// Ends the source; `device_ended` when its device stopped running.
    fn stop(self, device_ended: bool) -> impl Future<Output = ()> + Send;
}

impl VideoSource for super::android::scrcpy::ScrcpySession {
    async fn next_unit(&mut self) -> Option<VideoUnit> {
        self.units.recv().await
    }

    async fn reset_video(&mut self) -> Result<(), DeviceError> {
        Self::reset_video(self).await
    }

    fn failed(&self) -> bool {
        matches!(
            self.ended(),
            Some(super::android::scrcpy::StreamEnd::Failed(_))
        )
    }

    async fn stop(self, device_ended: bool) {
        Self::stop(self, device_ended).await;
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
    let failed = loop {
        tokio::select! {
            biased;
            () = hub.cancel.cancelled() => break false,
            () = hub.reset.notified() => {
                match tokio::time::timeout(RESET_WRITE_TIMEOUT, source.reset_video()).await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => tracing::warn!(%error, "device media reset failed"),
                    Err(_) => tracing::warn!("device media reset timed out"),
                }
            }
            unit = source.next_unit() => match unit {
                Some(unit) => hub.dispatch(&unit),
                None => break source.failed(),
            },
        }
    };
    if failed {
        tracing::warn!("device media stream failed; closing its sockets");
    }
    hub.close(failed);
    source.stop(hub.device_ended.load(Ordering::Acquire)).await;
}

/// One media socket's view of a device's stream.
pub(crate) struct MediaSubscriber {
    frames: mpsc::Receiver<QueuedFrame>,
    /// Cancelled when the stream ends for this socket.
    pub closed: CancellationToken,
    queued_bytes: Arc<AtomicUsize>,
    generation: Arc<AtomicU64>,
    guard: MediaGuard,
}

impl MediaSubscriber {
    /// The next encoded frame; deltas queued before a drop are skipped. `None` once the hub
    /// closed this socket and its queue is empty.
    pub(crate) async fn next_frame(&mut self) -> Option<Bytes> {
        loop {
            let frame = self.frames.recv().await?;
            self.queued_bytes
                .fetch_sub(frame.bytes.len(), Ordering::AcqRel);
            if frame.delta && frame.generation < self.generation.load(Ordering::Acquire) {
                continue;
            }
            return Some(frame.bytes);
        }
    }

    /// After a decoder error: wait for the next key unit and ask the encoder for one.
    pub(crate) fn request_keyframe(&self) {
        if let Some(hub) = &self.guard.hub {
            hub.request_keyframe(self.guard.id);
        }
    }

    /// Whether the stream ended because its source failed.
    pub(crate) fn failed(&self) -> bool {
        self.guard.hub.as_ref().is_some_and(|hub| hub.failed())
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

fn now_ms() -> i64 {
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
        self.client(device_id)?;
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

    /// Joins a `Ready` device's H.264 stream, starting its scrcpy session for the first
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
        let Some(serial) = slot
            .running
            .as_ref()
            .filter(|_| slot.snapshot.state == super::catalog::DeviceState::Ready)
            .map(|running| running.serial.clone())
        else {
            return Err(MediaAttachError::NotReady);
        };
        let hub = match &slot.media {
            Some(hub) if hub.is_open() => hub.clone(),
            _ => {
                let hub = MediaHub::new(slot.cancel.child_token());
                slot.media = Some(hub.clone());
                let adb = slot.adb.clone();
                let registry = self.inner.registry.clone();
                let task_hub = hub.clone();
                let inner = Arc::downgrade(&self.inner);
                let task_device = device_id.to_owned();
                self.inner.tasks.spawn(async move {
                    let start = async move {
                        let session =
                            super::android::scrcpy::ScrcpySession::start(&adb, &serial, &registry)
                                .await?;
                        tracing::debug!(
                            width = session.codec.width,
                            height = session.codec.height,
                            "device media stream started"
                        );
                        Ok(session)
                    };
                    run(task_hub.clone(), start).await;
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
            }
        };
        hub.subscribe(SOCKET_QUEUE_FRAMES, Arc::downgrade(&self.inner), device_id)
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

    /// A video source fed through a channel, counting the resets it is asked for and
    /// recording how it was stopped.
    struct ChannelSource {
        units: mpsc::Receiver<VideoUnit>,
        resets: Arc<AtomicUsize>,
        stopped: Arc<std::sync::OnceLock<bool>>,
    }

    impl VideoSource for ChannelSource {
        async fn next_unit(&mut self) -> Option<VideoUnit> {
            self.units.recv().await
        }

        async fn reset_video(&mut self) -> Result<(), DeviceError> {
            self.resets.fetch_add(1, Ordering::AcqRel);
            Ok(())
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
            let hub = MediaHub::new(CancellationToken::new());
            let (units, receiver) = mpsc::channel(64);
            let written = Arc::<AtomicUsize>::default();
            let stopped = Arc::<std::sync::OnceLock<bool>>::default();
            let source = ChannelSource {
                units: receiver,
                resets: written.clone(),
                stopped: stopped.clone(),
            };
            tokio::spawn(run(hub.clone(), async move { Ok(source) }));
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
        held.pop();
        tokio::time::advance(RESET_MIN_INTERVAL).await;
        held.push(hub.subscribe());
        assert_eq!(hub.resets(), 2);
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
