use std::{
    any::Any,
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::Duration,
};

use serde::Serialize;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};
use tokio::time::{Instant, sleep_until};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::manager::{TerminalError, TerminalSessionIdentity};

const MAX_FRAMES: usize = 16;
const MAX_FRAME_BYTES: usize = 16 * 1024;
const MAX_CONNECTION_BYTES: usize = 256 * 1024;
const MAX_CONNECTIONS: usize = 1024;
const MAX_LEASES: usize = 1024;
const MAX_CONNECTION_LEASES: usize = 64;
const MAX_SEQUENCE: u64 = 9_007_199_254_740_991;
const GAP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, thiserror::Error, Serialize)]
#[error("{message}")]
pub struct TerminalInputError {
    pub code: &'static str,
    pub message: String,
}

impl TerminalInputError {
    fn new(code: &'static str, message: &str) -> Self {
        Self {
            code,
            message: message.to_owned(),
        }
    }

    fn closed() -> Self {
        Self::new(
            "closed",
            "Terminal input is no longer attached. Reconnect the terminal before typing again.",
        )
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TerminalInputLease {
    pub input_id: String,
}

/// Work admission an input session holds until the session ends or a re-admission replaces
/// it, so its frames need no admission of their own. Cancelling `lost` while the admission is
/// current seals the session like a closed connection.
pub(crate) struct TerminalInputAdmission {
    lost: CancellationToken,
    hold: Arc<dyn Any + Send + Sync>,
    /// The project whose root the admitted workspace is, when the thread has no worktree of
    /// its own: a move of that root re-admits the session.
    inherited_root_of: Option<String>,
}

impl TerminalInputAdmission {
    pub(crate) fn new(
        lost: CancellationToken,
        hold: impl Any + Send + Sync,
        inherited_root_of: Option<String>,
    ) -> Self {
        Self {
            lost,
            hold: Arc::new(hold),
            inherited_root_of,
        }
    }
}

/// Which live input sessions a re-admission round covers.
#[derive(Clone, Copy)]
pub(crate) enum InputReadmissionScope<'a> {
    /// The thread's sessions: its workspace may have changed.
    Thread(&'a str),
    /// Sessions admitted for this project's root: the root may have moved. Sessions whose
    /// workspace is still being re-resolved are included, as their origin is not known yet.
    InheritedRootOf(&'a str),
    /// Every session: the change is unknown.
    All,
    /// One session, by input id: a begin catching up with a change that raced it.
    Session(&'a str),
}

impl InputReadmissionScope<'_> {
    fn covers(self, input_id: &str, thread_id: &str, admission: &LeaseAdmission) -> bool {
        match self {
            Self::Thread(thread) => thread == thread_id,
            Self::Session(session) => session == input_id,
            Self::InheritedRootOf(project) => {
                admission.pending > admission.revision
                    || admission
                        .current
                        .as_ref()
                        .and_then(|admission| admission.inherited_root_of.as_deref())
                        == Some(project)
            }
            Self::All => true,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TerminalInputAcknowledgement {
    pub input_id: String,
    pub sequence: u64,
}

#[derive(Default)]
struct RegistryState {
    leases: HashMap<String, Arc<InputLease>>,
    connections: HashMap<Uuid, Connection>,
    /// Counts re-admission rounds, so a begin can tell one started while it resolved.
    readmissions: u64,
}

struct Connection {
    closed: CancellationToken,
    frames: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
    leases: HashMap<(String, String), String>,
}

#[derive(Clone, Default)]
pub(super) struct TerminalInputRegistry(Arc<Mutex<RegistryState>>);

impl TerminalInputRegistry {
    pub(super) fn begin(
        &self,
        connection_id: Uuid,
        connection_closed: CancellationToken,
        manager_closed: CancellationToken,
        identity: TerminalSessionIdentity,
        attachment_sequence: u64,
        admission: Option<TerminalInputAdmission>,
    ) -> Result<TerminalInputLease, TerminalInputError> {
        if connection_closed.is_cancelled() || manager_closed.is_cancelled() {
            return Err(TerminalInputError::closed());
        }
        if attachment_sequence > MAX_SEQUENCE {
            return Err(TerminalInputError::new(
                "sequence",
                "Invalid terminal attachment sequence.",
            ));
        }
        let key = identity.input_key();
        let generation_closed = identity.input_cancellation();
        if generation_closed.is_cancelled() {
            return Err(TerminalInputError::closed());
        }
        let mut state = self.0.lock().expect("terminal input registry");
        if !state.connections.contains_key(&connection_id) {
            if state.connections.len() >= MAX_CONNECTIONS {
                return Err(TerminalInputError::new(
                    "capacity",
                    "Too many terminal input connections.",
                ));
            }
            state.connections.insert(
                connection_id,
                Connection {
                    closed: connection_closed.clone(),
                    frames: Arc::new(Semaphore::new(MAX_FRAMES)),
                    bytes: Arc::new(Semaphore::new(MAX_CONNECTION_BYTES)),
                    leases: HashMap::new(),
                },
            );
            let registry = Arc::downgrade(&self.0);
            let closed = connection_closed.clone();
            let shutdown = manager_closed.clone();
            tokio::spawn(async move {
                tokio::select! {
                    () = closed.cancelled() => {},
                    () = shutdown.cancelled() => {},
                }
                if let Some(registry) = registry.upgrade() {
                    let mut state = registry.lock().expect("terminal input registry");
                    if let Some(connection) = state.connections.remove(&connection_id) {
                        for input_id in connection.leases.into_values() {
                            if let Some(lease) = state.leases.remove(&input_id) {
                                lease.fail(TerminalInputError::closed());
                            }
                        }
                    }
                }
            });
        }
        let connection = state
            .connections
            .get(&connection_id)
            .expect("input connection");
        if connection.closed.is_cancelled() {
            return Err(TerminalInputError::closed());
        }
        let previous = connection.leases.get(&key).cloned();
        if previous
            .as_ref()
            .and_then(|input_id| state.leases.get(input_id))
            .is_some_and(|lease| lease.attachment_sequence >= attachment_sequence)
        {
            return Err(TerminalInputError::new(
                "closed",
                "A newer terminal input attachment is already active.",
            ));
        }
        if previous.is_none()
            && (connection.leases.len() >= MAX_CONNECTION_LEASES
                || state.leases.len() >= MAX_LEASES)
        {
            return Err(TerminalInputError::new(
                "capacity",
                "Too many attached terminal input leases.",
            ));
        }
        let lease = Arc::new(InputLease {
            connection_id,
            attachment_sequence,
            key: key.clone(),
            identity,
            connection_closed,
            generation_closed,
            admission: Mutex::new(LeaseAdmission {
                current: admission,
                revision: 0,
                pending: 0,
                sealed_by: None,
            }),
            admission_changed: Notify::new(),
            closed: CancellationToken::new(),
            frames: connection.frames.clone(),
            bytes: connection.bytes.clone(),
            state: Mutex::new(SequenceState::default()),
            changed: Notify::new(),
        });
        if let Some(previous) = previous.and_then(|input_id| state.leases.remove(&input_id)) {
            previous.fail(TerminalInputError::closed());
        }
        let input_id = Uuid::new_v4().to_string();
        state
            .connections
            .get_mut(&connection_id)
            .expect("input connection")
            .leases
            .insert(key, input_id.clone());
        state.leases.insert(input_id.clone(), lease.clone());
        drop(state);

        let registry = Arc::downgrade(&self.0);
        let cleanup_id = input_id.clone();
        tokio::spawn(async move {
            loop {
                let replaced = lease.admission_changed.notified();
                tokio::pin!(replaced);
                replaced.as_mut().enable();
                let lost = lease.admission_lost();
                tokio::select! {
                    () = lease.connection_closed.cancelled() => break,
                    () = lease.generation_closed.cancelled() => break,
                    () = lease.closed.cancelled() => break,
                    () = manager_closed.cancelled() => break,
                    // A replaced admission's loss no longer concerns this session.
                    () = async { if let Some(lost) = &lost { lost.cancelled().await } else { std::future::pending().await } } => {
                        if lease.seal_on_current_admission_loss() {
                            break;
                        }
                    }
                    () = &mut replaced => {}
                }
            }
            // Removing the lease drops its admission once in-flight frames settle.
            lease.fail(TerminalInputError::closed());
            if let Some(registry) = registry.upgrade() {
                let mut state = registry.lock().expect("terminal input registry");
                state.leases.remove(&cleanup_id);
                if let Some(connection) = state.connections.get_mut(&connection_id)
                    && connection.leases.get(&lease.key) == Some(&cleanup_id)
                {
                    connection.leases.remove(&lease.key);
                }
            }
        });
        Ok(TerminalInputLease { input_id })
    }

    fn get(
        &self,
        connection_id: Uuid,
        thread_id: &str,
        terminal_id: &str,
        input_id: &str,
    ) -> Result<Arc<InputLease>, TerminalInputError> {
        let state = self.0.lock().expect("terminal input registry");
        let lease = state
            .leases
            .get(input_id)
            .ok_or_else(TerminalInputError::closed)?;
        if lease.connection_id != connection_id
            || lease.key.0 != thread_id
            || lease.key.1 != terminal_id
        {
            return Err(TerminalInputError::closed());
        }
        Ok(lease.clone())
    }

    pub(super) fn cancel(
        &self,
        connection_id: Uuid,
        thread_id: &str,
        terminal_id: &str,
        input_id: &str,
    ) -> Result<(), TerminalInputError> {
        let state = self.0.lock().expect("terminal input registry");
        let Some(lease) = state.leases.get(input_id) else {
            return Ok(());
        };
        if lease.connection_id != connection_id
            || lease.key.0 != thread_id
            || lease.key.1 != terminal_id
        {
            return Err(TerminalInputError::closed());
        }
        lease.fail(TerminalInputError::closed());
        Ok(())
    }

    pub(super) fn readmission_epoch(&self) -> u64 {
        self.0.lock().expect("terminal input registry").readmissions
    }

    /// Starts re-admission round `revision` for the live sessions in `scope`, and returns it
    /// with their `(input_id, thread_id)`. Their frames wait until the round's result arrives;
    /// other sessions are untouched. A begin that registers after this call sees the epoch
    /// move and re-admits itself.
    pub(super) fn start_readmission(
        &self,
        scope: InputReadmissionScope<'_>,
    ) -> (u64, Vec<(String, String)>) {
        let mut state = self.0.lock().expect("terminal input registry");
        state.readmissions += 1;
        let revision = state.readmissions;
        let sessions = state
            .leases
            .iter()
            .filter_map(|(input_id, lease)| {
                let mut admission = lease.admission.lock().expect("terminal input admission");
                scope.covers(input_id, &lease.key.0, &admission).then(|| {
                    admission.pending = revision;
                    (input_id.clone(), lease.key.0.clone())
                })
            })
            .collect();
        (revision, sessions)
    }

    /// Applies the result of re-admission round `revision`: replaces the session's admission,
    /// keeping its frames and their order, or seals the session when no admission could be
    /// taken (`None`). A result older than one already applied, or than the latest round
    /// started for the session, is ignored. Returns whether the result applied.
    pub(super) fn readmit(
        &self,
        input_id: &str,
        revision: u64,
        admission: Option<TerminalInputAdmission>,
    ) -> bool {
        let Some(lease) = self
            .0
            .lock()
            .expect("terminal input registry")
            .leases
            .get(input_id)
            .cloned()
        else {
            return false;
        };
        let mut current = lease.admission.lock().expect("terminal input admission");
        // An applied result is newer, or a newer round is still resolving and decides.
        if revision <= current.revision || revision < current.pending {
            return false;
        }
        current.revision = revision;
        let previous = match admission {
            Some(admission) => current.current.replace(admission),
            None => {
                // Sealed under the admission lock, so no newer result can interleave.
                lease.fail(TerminalInputError::closed());
                None
            }
        };
        drop(current);
        drop(previous);
        lease.admission_changed.notify_waiters();
        lease.changed.notify_waiters();
        true
    }

    /// Whether the session is still registered and not sealed.
    pub(super) fn check_open(&self, input_id: &str) -> Result<(), TerminalInputError> {
        let lease = self
            .0
            .lock()
            .expect("terminal input registry")
            .leases
            .get(input_id)
            .cloned()
            .ok_or_else(TerminalInputError::closed)?;
        let state = lease.state.lock().expect("terminal input sequence");
        lease.failure(&state).map_or(Ok(()), Err)
    }

    pub(super) fn prepare_write<'a>(
        &self,
        connection_id: Uuid,
        thread_id: &str,
        terminal_id: &str,
        input_id: &'a str,
        sequence: u64,
        data: &'a str,
    ) -> Result<PreparedTerminalInput<'a>, TerminalError> {
        let lease = self.get(connection_id, thread_id, terminal_id, input_id)?;
        let frame = lease.admit(sequence, data.len())?;
        Ok(PreparedTerminalInput {
            frame,
            input_id,
            sequence,
            data,
        })
    }
}

pub(crate) struct TerminalInputSeal(Arc<InputLease>);

impl TerminalInputSeal {
    /// What the lost admission that sealed the session holds, if a loss sealed it.
    pub(crate) fn lost_admission(&self) -> Option<Arc<dyn Any + Send + Sync>> {
        self.0
            .admission
            .lock()
            .expect("terminal input admission")
            .sealed_by
            .clone()
    }
}

pub(crate) struct PreparedTerminalInput<'a> {
    frame: InputFrame,
    input_id: &'a str,
    sequence: u64,
    data: &'a str,
}

impl PreparedTerminalInput<'_> {
    /// Tells, once the write has failed, whether the session was sealed by the loss of its
    /// current admission.
    pub(crate) fn seal(&self) -> TerminalInputSeal {
        TerminalInputSeal(self.frame.lease.clone())
    }

    pub(crate) async fn write(mut self) -> Result<TerminalInputAcknowledgement, TerminalError> {
        let lease = self.frame.lease.clone();
        let sequence = self.sequence;
        let data = self.data;
        lease.wait_turn(sequence).await?;
        // Loss of the current admission stops the write at once, without waiting for the
        // cleanup task; a replaced admission's loss does not.
        let written = loop {
            let replaced = lease.admission_changed.notified();
            tokio::pin!(replaced);
            replaced.as_mut().enable();
            let lost = lease.admission_lost();
            let write = lease
                .identity
                .write_input(data, &lease.closed, &lease.connection_closed);
            tokio::select! {
                biased;
                () = async { if let Some(lost) = &lost { lost.cancelled().await } else { std::future::pending().await } } => {
                    if lease.seal_on_current_admission_loss() {
                        break Err(TerminalError::PublicationCancelled);
                    }
                }
                // Watch the replacement's loss instead; nothing was published yet.
                () = &mut replaced => {}
                written = write => break written,
            }
        };
        if let Err(error) = written {
            let failure = TerminalInputError::new(
                "write",
                &format!(
                    "Terminal input failed: {error}. Reconnect the terminal before typing again."
                ),
            );
            lease.fail(failure.clone());
            return Err(failure.into());
        }
        {
            let mut state = lease.state.lock().expect("terminal input sequence");
            if let Some(error) = lease.failure(&state) {
                return Err(error.into());
            }
            state.pending.remove(&sequence);
            state.next += 1;
            state.writing = false;
            state.update_gap();
        }
        self.frame.settled = true;
        lease.changed.notify_waiters();
        Ok(TerminalInputAcknowledgement {
            input_id: self.input_id.to_owned(),
            sequence,
        })
    }
}

#[derive(Default)]
struct SequenceState {
    next: u64,
    pending: HashSet<u64>,
    writing: bool,
    failure: Option<TerminalInputError>,
    gap_deadline: Option<Instant>,
}

impl SequenceState {
    fn update_gap(&mut self) {
        if !self.writing && !self.pending.is_empty() && !self.pending.contains(&self.next) {
            self.gap_deadline
                .get_or_insert_with(|| Instant::now() + GAP_TIMEOUT);
        } else {
            self.gap_deadline = None;
        }
    }
}

struct LeaseAdmission {
    current: Option<TerminalInputAdmission>,
    /// The re-admission round whose result `current` is, or 0 for the begin's own.
    revision: u64,
    /// The latest re-admission round started for this session.
    pending: u64,
    /// What the admission whose loss sealed the session holds.
    sealed_by: Option<Arc<dyn Any + Send + Sync>>,
}

struct InputLease {
    connection_id: Uuid,
    attachment_sequence: u64,
    key: (String, String),
    identity: TerminalSessionIdentity,
    connection_closed: CancellationToken,
    generation_closed: CancellationToken,
    admission: Mutex<LeaseAdmission>,
    admission_changed: Notify,
    closed: CancellationToken,
    frames: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
    state: Mutex<SequenceState>,
    changed: Notify,
}

impl InputLease {
    fn admission_lost(&self) -> Option<CancellationToken> {
        self.admission
            .lock()
            .expect("terminal input admission")
            .current
            .as_ref()
            .map(|admission| admission.lost.clone())
    }

    /// Seals the session if its current admission is lost, recording which, atomically with
    /// re-admission. Returns whether it sealed.
    fn seal_on_current_admission_loss(&self) -> bool {
        let mut admission = self.admission.lock().expect("terminal input admission");
        if admission.sealed_by.is_some() {
            return true;
        }
        let Some(lost) = admission
            .current
            .as_ref()
            .filter(|current| current.lost.is_cancelled())
            .map(|current| current.hold.clone())
        else {
            return false;
        };
        admission.sealed_by = Some(lost);
        self.fail(TerminalInputError::closed());
        true
    }

    fn failure(&self, state: &SequenceState) -> Option<TerminalInputError> {
        state.failure.clone().or_else(|| {
            (self.closed.is_cancelled()
                || self.connection_closed.is_cancelled()
                || self.generation_closed.is_cancelled())
            .then(TerminalInputError::closed)
        })
    }

    fn fail(&self, error: TerminalInputError) {
        let mut state = self.state.lock().expect("terminal input sequence");
        state.failure.get_or_insert(error);
        state.pending.clear();
        drop(state);
        self.closed.cancel();
        self.changed.notify_waiters();
    }

    fn admit(
        self: &Arc<Self>,
        sequence: u64,
        bytes: usize,
    ) -> Result<InputFrame, TerminalInputError> {
        let capacity = || {
            TerminalInputError::new(
                "capacity",
                "Terminal input exceeded its buffer limit. Reconnect the terminal before typing again.",
            )
        };
        if bytes == 0 || bytes > MAX_FRAME_BYTES {
            let error = capacity();
            self.fail(error.clone());
            return Err(error);
        }
        let permits = self.frames.clone().try_acquire_owned().and_then(|frame| {
            self.bytes
                .clone()
                .try_acquire_many_owned(u32::try_from(bytes).expect("bounded frame bytes"))
                .map(|bytes| (frame, bytes))
        });
        let Ok((frame, bytes)) = permits else {
            let error = capacity();
            self.fail(error.clone());
            return Err(error);
        };
        let mut state = self.state.lock().expect("terminal input sequence");
        if let Some(error) = self.failure(&state) {
            return Err(error);
        }
        if state
            .gap_deadline
            .is_some_and(|deadline| deadline <= Instant::now())
        {
            drop(state);
            let error = TerminalInputError::new(
                "sequence",
                "Terminal input is incomplete. Reconnect the terminal before typing again.",
            );
            self.fail(error.clone());
            return Err(error);
        }
        if sequence > MAX_SEQUENCE
            || sequence < state.next
            || sequence - state.next >= MAX_FRAMES as u64
            || !state.pending.insert(sequence)
        {
            drop(state);
            let error = TerminalInputError::new(
                "sequence",
                "Terminal input arrived with an invalid or duplicate sequence. Reconnect the terminal before typing again.",
            );
            self.fail(error.clone());
            return Err(error);
        }
        state.update_gap();
        drop(state);
        self.changed.notify_waiters();
        Ok(InputFrame {
            lease: self.clone(),
            settled: false,
            _frame: frame,
            _bytes: bytes,
        })
    }

    async fn wait_turn(&self, sequence: u64) -> Result<(), TerminalInputError> {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            // A lost current admission seals here, without waiting for the cleanup task.
            self.seal_on_current_admission_loss();
            let deadline = {
                // Admission before `state`, as sealing takes them; held across the claim so no
                // re-admission round can start between its check and the turn.
                let admission = self.admission.lock().expect("terminal input admission");
                let mut state = self.state.lock().expect("terminal input sequence");
                if let Some(error) = self.failure(&state) {
                    return Err(error);
                }
                if !state.writing
                    && state.next == sequence
                    && admission.pending <= admission.revision
                {
                    state.writing = true;
                    state.gap_deadline = None;
                    return Ok(());
                }
                state.gap_deadline
            };
            tokio::select! {
                biased;
                () = self.closed.cancelled() => return Err(TerminalInputError::closed()),
                () = self.connection_closed.cancelled() => return Err(TerminalInputError::closed()),
                () = self.generation_closed.cancelled() => return Err(TerminalInputError::closed()),
                () = &mut notified => {},
                () = async { if let Some(deadline) = deadline { sleep_until(deadline).await; } else { std::future::pending::<()>().await; } } => {
                    let expired = self.state.lock().expect("terminal input sequence").gap_deadline.is_some_and(|deadline| deadline <= Instant::now());
                    if expired {
                        let error = TerminalInputError::new("sequence", "Terminal input is incomplete. Reconnect the terminal before typing again.");
                        self.fail(error.clone());
                        return Err(error);
                    }
                }
            }
        }
    }
}

struct InputFrame {
    lease: Arc<InputLease>,
    settled: bool,
    _frame: OwnedSemaphorePermit,
    _bytes: OwnedSemaphorePermit,
}

impl Drop for InputFrame {
    fn drop(&mut self) {
        if !self.settled {
            self.lease.fail(TerminalInputError::closed());
        }
    }
}
