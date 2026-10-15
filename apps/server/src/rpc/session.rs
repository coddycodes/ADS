use std::{
    any::Any,
    collections::HashMap,
    future::Future,
    panic::AssertUnwindSafe,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum::extract::ws::{Message, WebSocket};
use futures_util::{FutureExt, Sink, Stream, StreamExt};
use serde_json::{Value, json};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time::{Instant, timeout, timeout_at},
};
use tokio_util::sync::CancellationToken;

use super::{
    byte_budget::{RpcOutboundBudget, RpcOutboundBytePermit},
    message::{ClientMessage, RequestId, RpcRequest, ServerMessage},
    methods::{ACTIVE_RPC_METHODS, MethodMode},
    transport::{
        self, CHUNKED_RPC_SUBPROTOCOL, CONTROL_LANE_CAPACITY, ConnectionEnd, ConnectionLiveness,
        OutboundFraming,
    },
};
use crate::{
    auth::{AuthService, ClientMetadata, Principal, authorization_error, required_scope},
    device::media::MediaChannel,
    diagnostics::{
        TraceDiagnosticsStore,
        profiler::{
            connections,
            latency::{self, FIRST_ITEM, RequestTimer},
        },
    },
    json_size::encoded_json_len,
    maintenance::{RpcAdmissionGate, RpcPermit, rpc_mutability},
    preview::gateway::registry::ClientReach,
};

const OUTBOUND_CAPACITY: usize = 64;
const MAX_IN_FLIGHT_REQUESTS: usize = 64;
const INPUT_RESERVED_CONTROL_REQUESTS: usize = 8;
const OUTBOUND_SEND_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const SOCKET_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const PUMP_JOIN_TIMEOUT: Duration = Duration::from_secs(1);

pub type RpcResult = Result<Value, Value>;
pub type RpcStreamChunk = Result<Vec<Value>, Value>;
type UnaryFuture = Pin<Box<dyn Future<Output = RpcUnaryResult> + Send + 'static>>;
type UnaryHandler =
    Arc<dyn Fn(RpcRequest, RpcSessionContext, CancellationToken) -> UnaryFuture + Send + Sync>;
type StreamHandler = Arc<
    dyn Fn(RpcRequest, RpcSessionContext, CancellationToken) -> mpsc::Receiver<RpcStreamChunk>
        + Send
        + Sync,
>;
type LatestStreamHandler = Arc<
    dyn Fn(
            RpcRequest,
            RpcSessionContext,
            CancellationToken,
        ) -> watch::Receiver<Option<RpcStreamChunk>>
        + Send
        + Sync,
>;

pub(crate) trait RpcResponseEnqueueGuard: Send {
    fn encoded_len_bound(&self, response: &ServerMessage) -> Result<usize, serde_json::Error> {
        encoded_server_message_len(response)
    }

    fn enqueue(self: Box<Self>, permit: RpcResponseEnqueuePermit, response: ServerMessage);
}

pub(crate) struct RpcOutboundFrame {
    payload: RpcOutboundPayload,
    _budget: Option<RpcOutboundBytePermit>,
    /// When the frame entered the queue; the writer records how long it waited.
    queued_at: Instant,
}

enum RpcOutboundPayload {
    Plain(ServerMessage),
    Encoded(Message),
    /// A control message too large for one record. It keeps ordinary data
    /// queueing and byte budgeting but never counts as write progress.
    Control(Message),
}

impl RpcOutboundFrame {
    pub(super) fn into_wire(self) -> Result<Self, serde_json::Error> {
        let payload = match self.payload {
            RpcOutboundPayload::Plain(message) => {
                RpcOutboundPayload::Encoded(Message::Text(serde_json::to_string(&message)?.into()))
            }
            payload @ (RpcOutboundPayload::Encoded(_) | RpcOutboundPayload::Control(_)) => payload,
        };
        Ok(Self {
            payload,
            _budget: self._budget,
            queued_at: self.queued_at,
        })
    }

    pub(super) fn queued_at(&self) -> Instant {
        self.queued_at
    }

    pub(crate) fn into_parts(self) -> (Message, Option<RpcOutboundBytePermit>) {
        let (RpcOutboundPayload::Encoded(message) | RpcOutboundPayload::Control(message)) =
            self.payload
        else {
            unreachable!("RPC writer encodes plain responses before the transport sink")
        };
        (message, self._budget)
    }

    pub(super) fn is_control(&self) -> bool {
        matches!(self.payload, RpcOutboundPayload::Control(_))
    }

    #[cfg(test)]
    pub(super) fn plain(message: ServerMessage) -> Self {
        Self {
            payload: RpcOutboundPayload::Plain(message),
            _budget: None,
            queued_at: Instant::now(),
        }
    }

    #[cfg(test)]
    pub(super) fn into_control(self) -> Result<Self, serde_json::Error> {
        let (message, budget) = self.into_wire()?.into_parts();
        Ok(Self {
            payload: RpcOutboundPayload::Control(message),
            _budget: budget,
            queued_at: Instant::now(),
        })
    }
}

pub(crate) struct RpcResponseEnqueuePermit {
    permit: mpsc::OwnedPermit<RpcOutboundFrame>,
    budget: Option<RpcOutboundBytePermit>,
    encoded_len_bound: usize,
}

pub(crate) struct PreparedRpcResponse {
    payload: RpcOutboundPayload,
    encoded_len: usize,
}

impl RpcResponseEnqueuePermit {
    pub(crate) fn prepare(&self, response: ServerMessage) -> Option<PreparedRpcResponse> {
        if self.budget.is_some() {
            let encoded = serde_json::to_string(&response).ok()?;
            (encoded.len() <= self.encoded_len_bound).then(|| PreparedRpcResponse {
                encoded_len: encoded.len(),
                payload: RpcOutboundPayload::Encoded(Message::Text(encoded.into())),
            })
        } else {
            Some(PreparedRpcResponse {
                payload: RpcOutboundPayload::Plain(response),
                encoded_len: 0,
            })
        }
    }

    pub(crate) fn send_prepared(mut self, prepared: PreparedRpcResponse) {
        if let Some(budget) = &mut self.budget
            && budget.shrink_to(prepared.encoded_len).is_err()
        {
            return;
        }
        self.permit.send(RpcOutboundFrame {
            payload: prepared.payload,
            _budget: self.budget,
            queued_at: Instant::now(),
        });
    }
}

#[derive(Clone)]
struct RpcOutboundQueue {
    sender: mpsc::Sender<RpcOutboundFrame>,
    /// Pong, interrupt exits, admission-failure terminals and client protocol
    /// errors. The writer drains it before each message and between records.
    control: mpsc::Sender<ServerMessage>,
    budget: Option<RpcOutboundBudget>,
    /// Largest message the peer can reassemble; `None` for legacy whole frames.
    max_message_bytes: Option<usize>,
}

impl RpcOutboundQueue {
    /// The largest encoded message this connection may send, if bounded.
    fn message_limit(&self) -> Option<usize> {
        let connection = self
            .budget
            .as_ref()
            .map(|budget| budget.connection_capacity);
        match (self.max_message_bytes, connection) {
            (Some(message), Some(connection)) => Some(message.min(connection)),
            (message, connection) => message.or(connection),
        }
    }

    async fn acquire_budget(
        &self,
        shutdown: &CancellationToken,
        bytes: usize,
        deadline: Instant,
    ) -> Result<Option<RpcOutboundBytePermit>, SendFailure> {
        let Some(budget) = &self.budget else {
            return Ok(None);
        };
        tokio::select! {
            () = shutdown.cancelled() => Err(SendFailure::Rejected),
            result = budget.acquire(bytes, deadline) => {
                result.map(Some).map_err(SendFailure::rejected)
            }
        }
    }

    #[cfg(test)]
    fn try_send(&self, message: ServerMessage) -> Result<(), ()> {
        let (payload, budget) = if let Some(budget) = &self.budget {
            let encoded = serde_json::to_string(&message).map_err(|_| ())?;
            let permit = budget.try_acquire(encoded.len())?;
            (
                RpcOutboundPayload::Encoded(Message::Text(encoded.into())),
                Some(permit),
            )
        } else {
            (RpcOutboundPayload::Plain(message), None)
        };
        self.sender
            .try_send(RpcOutboundFrame {
                payload,
                _budget: budget,
                queued_at: Instant::now(),
            })
            .map_err(|_| ())
    }
}

pub(crate) struct RpcUnaryResult {
    result: RpcResult,
    enqueue_guard: Option<Box<dyn RpcResponseEnqueueGuard>>,
}

impl RpcUnaryResult {
    pub(crate) fn plain(result: RpcResult) -> Self {
        Self {
            result,
            enqueue_guard: None,
        }
    }

    pub(crate) fn guarded(
        result: RpcResult,
        enqueue_guard: impl RpcResponseEnqueueGuard + 'static,
    ) -> Self {
        Self {
            result,
            enqueue_guard: Some(Box::new(enqueue_guard)),
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct RpcSessionContext {
    principal: Option<Principal>,
    auth: Option<AuthService>,
    admission: Option<RpcPermit>,
    pairing_confirmation: Option<PairingConfirmationLatch>,
    connection: Arc<RpcConnectionLifetime>,
    /// How this connection reached the server; `None` without a socket (tests).
    reach: Option<ClientReach>,
    /// The transport this connection runs on, which media tickets are bound to.
    channel: MediaChannel,
}

struct RpcConnectionLifetime {
    id: uuid::Uuid,
    closed: CancellationToken,
}

impl Default for RpcConnectionLifetime {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4(),
            closed: CancellationToken::new(),
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct PairingConfirmationLatch(Arc<AtomicBool>);

impl PairingConfirmationLatch {
    pub(crate) fn mark_confirmed(&self) {
        self.0.store(true, Ordering::Release);
    }

    #[must_use]
    pub(crate) fn is_confirmed(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

impl RpcSessionContext {
    #[must_use]
    pub(crate) fn unauthenticated() -> Self {
        Self::default()
    }

    #[must_use]
    pub(crate) fn authenticated(principal: Principal, auth: AuthService) -> Self {
        Self {
            principal: Some(principal),
            auth: Some(auth),
            admission: None,
            pairing_confirmation: None,
            connection: Arc::default(),
            reach: None,
            channel: MediaChannel::Plain,
        }
    }

    #[must_use]
    pub(crate) fn authenticated_pending_pairing(
        principal: Principal,
        auth: AuthService,
        pairing_confirmation: PairingConfirmationLatch,
    ) -> Self {
        Self {
            principal: Some(principal),
            auth: Some(auth),
            admission: None,
            pairing_confirmation: Some(pairing_confirmation),
            connection: Arc::default(),
            reach: None,
            channel: MediaChannel::Plain,
        }
    }

    #[must_use]
    pub(crate) fn with_reach(mut self, reach: Option<ClientReach>) -> Self {
        self.reach = reach;
        self
    }

    #[must_use]
    pub(crate) fn with_channel(mut self, channel: MediaChannel) -> Self {
        self.channel = channel;
        self
    }

    /// The transport this connection runs on: plain WebSocket or Noise E2EE.
    #[must_use]
    pub(crate) fn channel(&self) -> MediaChannel {
        self.channel
    }

    /// How this connection reached the server: its socket's local address and `Host`.
    #[must_use]
    pub(crate) fn reach(&self) -> Option<&ClientReach> {
        self.reach.as_ref()
    }

    #[must_use]
    fn with_admission(mut self, admission: RpcPermit) -> Self {
        self.admission = Some(admission);
        self
    }

    #[must_use]
    pub(crate) fn admission_permit(&self) -> Option<RpcPermit> {
        self.admission.clone()
    }

    /// The authenticated principal of this connection; `None` on an unauthenticated server.
    #[must_use]
    pub(crate) fn principal(&self) -> Option<&Principal> {
        self.principal.as_ref()
    }

    #[must_use]
    pub(crate) fn current_session_id(&self) -> Option<&str> {
        self.principal
            .as_ref()
            .map(|principal| principal.session_id.as_str())
    }

    /// The paired-client metadata of this connection's session, for audit and host
    /// notices. `None` on an unauthenticated server or for an unknown session.
    pub(crate) async fn current_client_metadata(&self) -> Option<ClientMetadata> {
        let (Some(principal), Some(auth)) = (&self.principal, &self.auth) else {
            return None;
        };
        auth.list_clients(&principal.session_id)
            .await
            .into_iter()
            .find(|client| client.current)
            .map(|client| client.client)
    }

    pub(crate) fn connection_id(&self) -> uuid::Uuid {
        self.connection.id
    }

    pub(crate) fn connection_closed(&self) -> CancellationToken {
        self.connection.closed.clone()
    }

    fn with_connection(mut self, closed: CancellationToken) -> Self {
        self.connection = Arc::new(RpcConnectionLifetime {
            id: uuid::Uuid::new_v4(),
            closed,
        });
        self
    }

    fn has_pending_pairing_capability_for(&self, method: &str) -> bool {
        self.pairing_confirmation.is_some() && method == "auth.confirmPairing"
    }

    pub(crate) async fn confirm_current_pairing(&self) -> bool {
        let (Some(principal), Some(auth)) = (&self.principal, &self.auth) else {
            return false;
        };
        let session_id = principal.session_id.clone();
        let auth = auth.clone();
        let pairing_confirmation = self.pairing_confirmation.clone();
        let activation = tokio::spawn(async move {
            let Ok(true) = auth.confirm_pending_pairing_session(&session_id).await else {
                return false;
            };
            if let Some(latch) = pairing_confirmation {
                latch.mark_confirmed();
            }
            true
        });
        match activation.await {
            Ok(confirmed) => confirmed,
            Err(error) => {
                tracing::error!(%error, "pairing-session activation task failed");
                false
            }
        }
    }

    pub(crate) async fn is_currently_authorized(&self, required_scope: &str) -> bool {
        match (&self.principal, &self.auth) {
            (Some(principal), Some(auth)) => auth
                .authorize_session(&principal.session_id, required_scope)
                .await
                .is_ok(),
            (None, None) => true,
            _ => false,
        }
    }
}

#[derive(Clone)]
enum RpcMethod {
    Unary(UnaryHandler),
    Stream(StreamHandler),
    LatestStream(LatestStreamHandler),
}

#[derive(Clone, Default)]
pub struct RpcRegistry {
    methods: HashMap<String, RpcMethod>,
    trace_diagnostics: Option<TraceDiagnosticsStore>,
    admission_gate: RpcAdmissionGate,
}

impl RpcRegistry {
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_trace_diagnostics(trace_diagnostics: TraceDiagnosticsStore) -> Self {
        Self {
            methods: HashMap::new(),
            trace_diagnostics: Some(trace_diagnostics),
            admission_gate: RpcAdmissionGate::new(),
        }
    }

    pub(crate) fn trace_diagnostics(&self) -> Option<TraceDiagnosticsStore> {
        self.trace_diagnostics.clone()
    }

    pub(crate) fn admission_gate(&self) -> RpcAdmissionGate {
        self.admission_gate.clone()
    }

    pub fn register_unary<F, Fut>(&mut self, name: impl Into<String>, handler: F)
    where
        F: Fn(RpcRequest, CancellationToken) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = RpcResult> + Send + 'static,
    {
        let handler = Arc::new(move |request, _context, cancellation| {
            let future = handler(request, cancellation);
            Box::pin(async move { RpcUnaryResult::plain(future.await) }) as UnaryFuture
        });
        self.register_unary_handler(name.into(), handler);
    }

    pub(crate) fn register_unary_with_context<F, Fut>(
        &mut self,
        name: impl Into<String>,
        handler: F,
    ) where
        F: Fn(RpcRequest, RpcSessionContext, CancellationToken) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = RpcResult> + Send + 'static,
    {
        let handler = Arc::new(move |request, context, cancellation| {
            let future = handler(request, context, cancellation);
            Box::pin(async move { RpcUnaryResult::plain(future.await) }) as UnaryFuture
        });
        self.register_unary_handler(name.into(), handler);
    }

    pub(crate) fn register_guarded_unary<F, Fut>(&mut self, name: impl Into<String>, handler: F)
    where
        F: Fn(RpcRequest, CancellationToken) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = RpcUnaryResult> + Send + 'static,
    {
        let handler = Arc::new(move |request, _context, cancellation| {
            Box::pin(handler(request, cancellation)) as UnaryFuture
        });
        self.register_unary_handler(name.into(), handler);
    }

    fn register_unary_handler(&mut self, name: String, handler: UnaryHandler) {
        let trace_diagnostics = self.trace_diagnostics.clone();
        let diagnostic_name = name.clone();
        self.methods.insert(
            name,
            RpcMethod::Unary(Arc::new(move |request, context, cancellation| {
                let started = tokio::time::Instant::now();
                // Read before the handler takes the request; `None` while the profiler is off.
                let timer = RequestTimer::unary(&diagnostic_name, &request.headers);
                let future = handler(request, context, cancellation);
                let trace_diagnostics = trace_diagnostics.clone();
                let diagnostic_name = diagnostic_name.clone();
                Box::pin(async move {
                    let result = future.await;
                    if let Some(timer) = timer {
                        let outcome = if result.result.is_ok() {
                            "success"
                        } else {
                            "failure"
                        };
                        timer.finish(&diagnostic_name, outcome);
                    }
                    if let (Some(trace_diagnostics), Err(error)) =
                        (&trace_diagnostics, &result.result)
                        && let Err(write_error) = trace_diagnostics.record_timed_failure(
                            &diagnostic_name,
                            error,
                            started.elapsed(),
                        )
                    {
                        tracing::warn!(
                            method = diagnostic_name,
                            error = %write_error,
                            "failed to persist RPC diagnostics"
                        );
                    }
                    result
                })
            })),
        );
    }

    pub fn register_stream<F>(&mut self, name: impl Into<String>, handler: F)
    where
        F: Fn(RpcRequest, CancellationToken) -> mpsc::Receiver<RpcStreamChunk>
            + Send
            + Sync
            + 'static,
    {
        self.methods.insert(
            name.into(),
            RpcMethod::Stream(Arc::new(move |request, _context, cancellation| {
                handler(request, cancellation)
            })),
        );
    }

    pub fn register_latest_stream<F>(&mut self, name: impl Into<String>, handler: F)
    where
        F: Fn(RpcRequest, CancellationToken) -> watch::Receiver<Option<RpcStreamChunk>>
            + Send
            + Sync
            + 'static,
    {
        self.methods.insert(
            name.into(),
            RpcMethod::LatestStream(Arc::new(move |request, _context, cancellation| {
                handler(request, cancellation)
            })),
        );
    }

    pub(crate) fn register_stream_with_context<F>(&mut self, name: impl Into<String>, handler: F)
    where
        F: Fn(RpcRequest, RpcSessionContext, CancellationToken) -> mpsc::Receiver<RpcStreamChunk>
            + Send
            + Sync
            + 'static,
    {
        self.methods
            .insert(name.into(), RpcMethod::Stream(Arc::new(handler)));
    }

    fn get(&self, name: &str) -> Option<RpcMethod> {
        self.methods.get(name).cloned()
    }

    pub fn validate_complete(&self) -> Result<(), String> {
        let mut issues = Vec::new();
        for spec in ACTIVE_RPC_METHODS {
            match (spec.mode, self.methods.get(spec.name)) {
                (MethodMode::Unary, Some(RpcMethod::Unary(_)))
                | (MethodMode::Stream, Some(RpcMethod::Stream(_) | RpcMethod::LatestStream(_))) => {
                }
                (_, None) => issues.push(format!("missing {}", spec.name)),
                (expected, Some(_)) => {
                    issues.push(format!(
                        "wrong mode for {}: expected {expected:?}",
                        spec.name
                    ));
                }
            }
        }
        if issues.is_empty() {
            Ok(())
        } else {
            Err(issues.join(", "))
        }
    }
}

struct InFlight {
    cancellation: CancellationToken,
    acknowledgements: Option<mpsc::Sender<()>>,
    task: JoinHandle<()>,
}

struct DispatchContext<'a> {
    registry: &'a RpcRegistry,
    session: &'a RpcSessionContext,
    outbound: &'a RpcOutboundQueue,
    completed: &'a mpsc::Sender<RequestId>,
    shutdown: &'a CancellationToken,
}

trait RpcInboundGuard: Send + Sync {}

impl<T: Send + Sync> RpcInboundGuard for T {}

type SharedRpcInboundGuard = Arc<dyn RpcInboundGuard>;

pub(crate) struct RpcInboundFrame {
    message: Message,
    guard: Option<SharedRpcInboundGuard>,
}

impl RpcInboundFrame {
    fn plain(message: Message) -> Self {
        Self {
            message,
            guard: None,
        }
    }

    pub(crate) fn guarded(message: Message, guard: impl Send + Sync + 'static) -> Self {
        Self {
            message,
            guard: Some(Arc::new(guard)),
        }
    }
}

pub(crate) async fn run_session(
    socket: WebSocket,
    registry: RpcRegistry,
    context: RpcSessionContext,
    session_shutdown: CancellationToken,
) {
    let framing = if socket
        .protocol()
        .is_some_and(|protocol| protocol.as_bytes() == CHUNKED_RPC_SUBPROTOCOL.as_bytes())
    {
        OutboundFraming::PlainRecords
    } else {
        OutboundFraming::Whole
    };
    let liveness = ConnectionLiveness::new();
    let (socket_writer, socket_reader) = socket.split();
    let socket_reader = transport::observe_inbound(Arc::clone(&liveness), socket_reader)
        .map(|frame| frame.map(RpcInboundFrame::plain));
    run_session_split_budgeted(
        socket_writer,
        framing,
        liveness,
        socket_reader,
        registry,
        context,
        session_shutdown,
        None,
    )
    .await;
}

#[allow(
    clippy::too_many_arguments,
    reason = "Explicit shared session transport handoff"
)]
pub(crate) async fn run_session_split_budgeted<W, R>(
    socket_writer: W,
    framing: OutboundFraming,
    liveness: Arc<ConnectionLiveness>,
    socket_reader: R,
    registry: RpcRegistry,
    context: RpcSessionContext,
    session_shutdown: CancellationToken,
    outbound_budget: Option<RpcOutboundBudget>,
) where
    W: Sink<Message> + Unpin + Send + 'static,
    R: Stream<Item = Result<RpcInboundFrame, axum::Error>> + Send,
{
    let context = context.with_connection(session_shutdown.clone());
    let _connection_guard = session_shutdown.clone().drop_guard();
    // Boxed so the read half can be dropped as soon as the loop ends.
    let mut socket_reader = Box::pin(socket_reader);
    let (outbound_sender, outbound_receiver) = mpsc::channel::<RpcOutboundFrame>(OUTBOUND_CAPACITY);
    let (control_sender, control_receiver) = mpsc::channel::<ServerMessage>(CONTROL_LANE_CAPACITY);
    let max_message_bytes = framing.max_message_bytes();
    let transport = framing.transport();
    let outbound = RpcOutboundQueue {
        sender: outbound_sender,
        control: control_sender,
        budget: outbound_budget,
        max_message_bytes,
    };
    let mut writer = tokio::spawn(transport::run_writer(
        socket_writer,
        framing,
        Arc::clone(&liveness),
        outbound_receiver,
        control_receiver,
        session_shutdown.clone(),
    ));
    let mut heartbeat = tokio::spawn(transport::run_heartbeat(
        Arc::clone(&liveness),
        session_shutdown.clone(),
    ));
    let (completed_sender, mut completed_receiver) =
        mpsc::channel::<RequestId>(MAX_IN_FLIGHT_REQUESTS);
    let mut in_flight = HashMap::<RequestId, InFlight>::new();
    let mut received_eof = false;
    // Why the loop ended, when the loop itself saw it; otherwise the server, the heartbeat
    // or the writer cancelled the session.
    let mut end = None;
    // The profiler client id of the first request that names one.
    let mut client_id: Option<String> = None;
    {
        let dispatch = DispatchContext {
            registry: &registry,
            session: &context,
            outbound: &outbound,
            completed: &completed_sender,
            shutdown: &session_shutdown,
        };

        loop {
            if received_eof && in_flight.is_empty() {
                end.get_or_insert(ConnectionEnd::ClientEof);
                break;
            }

            tokio::select! {
                () = session_shutdown.cancelled() => break,
                completed = completed_receiver.recv(), if !in_flight.is_empty() => {
                    let Some(request_id) = completed else {
                        break;
                    };
                    if let Some(in_flight_request) = in_flight.remove(&request_id) {
                        let _ = in_flight_request.task.await;
                    }
                }
                frame = socket_reader.next() => {
                    let Some(Ok(frame)) = frame else {
                        end = Some(ConnectionEnd::ClientGone);
                        break;
                    };
                    let RpcInboundFrame { message: frame, guard } = frame;
                    let decoded = match frame {
                        Message::Text(text) => decode_client_messages(text.as_bytes()),
                        Message::Binary(bytes) => decode_client_messages(&bytes),
                        Message::Close(frame) => {
                            end = Some(ConnectionEnd::ClientClosed(
                                frame.map(|frame| frame.code),
                            ));
                            break;
                        }
                        Message::Ping(_) | Message::Pong(_) => continue,
                    };
                    let messages = match decoded {
                        Ok(messages) => messages,
                        Err(error) => {
                            if send_unbudgeted_server_message(
                                &outbound,
                                &session_shutdown,
                                client_protocol_error(error.to_string()),
                            )
                            .await
                            .is_err()
                            {
                                break;
                            }
                            continue;
                        }
                    };
                    for message in messages {
                        if client_id.is_none()
                            && let ClientMessage::Request { headers, .. } = &message
                            && let Some(id) = connections::request_client_id(headers)
                        {
                            connections::record_client_identified(&id, transport, liveness.age());
                            client_id = Some(id);
                        }
                        if process_client_message(
                            message,
                            &dispatch,
                            &mut in_flight,
                            &mut received_eof,
                            guard.clone(),
                        )
                        .await
                        .is_err()
                        {
                            // The server could not queue an answer: wind down.
                            end = Some(ConnectionEnd::Server);
                            received_eof = true;
                            break;
                        }
                    }
                }
            }
        }
    }
    liveness.report_end(
        end.unwrap_or(ConnectionEnd::Server),
        transport,
        outbound.sender.max_capacity() - outbound.sender.capacity(),
        in_flight.len(),
        client_id.as_deref(),
    );

    // Release the read half at once: after a writer failure both halves must go
    // so the connection closes promptly.
    drop(socket_reader);
    session_shutdown.cancel();
    for request in in_flight.values() {
        request.cancellation.cancel();
    }
    join_request_tasks(
        in_flight
            .into_values()
            .map(|request| request.task)
            .collect(),
    )
    .await;
    drop(outbound);
    drop(completed_sender);
    if timeout(PUMP_JOIN_TIMEOUT, &mut writer).await.is_err() {
        writer.abort();
        let _ = writer.await;
    }
    if timeout(PUMP_JOIN_TIMEOUT, &mut heartbeat).await.is_err() {
        heartbeat.abort();
        let _ = heartbeat.await;
    }
}

/// Joins the cancelled request tasks. Cancelled handlers are expected to exit promptly; a
/// handler that ignores cancellation must not pin the session (and every budget permit riding
/// on it) forever, so the join is bounded and stragglers are aborted. A joined task leaves the
/// list at once, because awaiting a `JoinHandle` again after it returned panics (#84).
async fn join_request_tasks(mut request_tasks: Vec<JoinHandle<()>>) {
    let join_all = async {
        while let Some(task) = request_tasks.last_mut() {
            let _ = task.await;
            request_tasks.pop();
        }
    };
    if timeout(PUMP_JOIN_TIMEOUT, join_all).await.is_err() {
        for task in &request_tasks {
            task.abort();
        }
        for task in request_tasks {
            let _ = task.await;
        }
    }
}

async fn process_client_message(
    message: ClientMessage,
    dispatch: &DispatchContext<'_>,
    in_flight: &mut HashMap<RequestId, InFlight>,
    received_eof: &mut bool,
    inbound_guard: Option<SharedRpcInboundGuard>,
) -> Result<(), SendFailure> {
    if *received_eof && matches!(message, ClientMessage::Request { .. }) {
        return Ok(());
    }

    match message {
        ClientMessage::Ping => {
            // A full lane drops this Pong; the client's liveness counts any
            // inbound data, and a Ping must never end the read loop.
            let _ = dispatch.outbound.control.try_send(ServerMessage::Pong);
            Ok(())
        }
        ClientMessage::Eof => {
            *received_eof = true;
            Ok(())
        }
        ClientMessage::Ack { request_id } => {
            if let Some(sender) = in_flight
                .get(&request_id)
                .and_then(|request| request.acknowledgements.as_ref())
            {
                let _ = sender.try_send(());
            }
            Ok(())
        }
        ClientMessage::Interrupt { request_id } => {
            if let Some(request) = in_flight.get(&request_id) {
                request.cancellation.cancel();
                return Ok(());
            }
            send_unbudgeted_server_message(
                dispatch.outbound,
                dispatch.shutdown,
                ServerMessage::interrupt(request_id),
            )
            .await
        }
        ClientMessage::Request {
            id,
            tag,
            payload,
            headers,
            trace_id,
            span_id,
            sampled,
        } => {
            let request = RpcRequest {
                id,
                tag,
                payload,
                headers,
                trace_id,
                span_id,
                sampled,
            };
            if in_flight.contains_key(&request.id) {
                return Ok(());
            }
            if matches!(
                request.tag.as_str(),
                "terminal.beginInput" | "terminal.writeInput"
            ) && in_flight.len() >= MAX_IN_FLIGHT_REQUESTS - INPUT_RESERVED_CONTROL_REQUESTS
            {
                return send_server_message(
                    dispatch.outbound,
                    dispatch.shutdown,
                    ServerMessage::failure(request.id, json!({
                        "_tag": "TerminalInputError",
                        "code": "capacity",
                        "message": "Terminal input is paused because the connection is busy. Reconnect the terminal before typing again.",
                    })),
                ).await;
            }
            if in_flight.len() >= MAX_IN_FLIGHT_REQUESTS {
                return send_server_message(
                    dispatch.outbound,
                    dispatch.shutdown,
                    ServerMessage::connection_defect("RPC in-flight request limit exceeded"),
                )
                .await;
            }
            let Some(method) = dispatch.registry.get(&request.tag) else {
                return send_server_message(
                    dispatch.outbound,
                    dispatch.shutdown,
                    ServerMessage::connection_defect(format!(
                        "Unknown request tag: {}",
                        request.tag
                    )),
                )
                .await;
            };
            let scope = match dispatch.session.principal {
                Some(_) => {
                    let Some(scope) = required_scope(&request.tag) else {
                        return send_server_message(
                            dispatch.outbound,
                            dispatch.shutdown,
                            ServerMessage::connection_defect(format!(
                                "RPC method {} has no declared authorization scope",
                                request.tag
                            )),
                        )
                        .await;
                    };
                    Some(scope)
                }
                None => None,
            };
            spawn_request(request, method, scope, inbound_guard, dispatch, in_flight);
            Ok(())
        }
    }
}

/// Starts the request's task, which authorizes it (`scope` is the method's declared scope on an
/// authenticated session) and takes its maintenance admission before the handler runs. The
/// read loop never waits on the auth store, so it keeps answering Pings while a periodic task
/// holds the database worker. The inbound guard (E2EE byte permits) stays held until the
/// request is admitted, as it was while the read loop authorized it.
fn spawn_request(
    request: RpcRequest,
    method: RpcMethod,
    scope: Option<&'static str>,
    inbound_guard: Option<SharedRpcInboundGuard>,
    dispatch: &DispatchContext<'_>,
    in_flight: &mut HashMap<RequestId, InFlight>,
) {
    let request_id = request.id.clone();
    let cancellation = CancellationToken::new();
    let request_cancellation = cancellation.clone();
    let context = dispatch.session.clone();
    let admission_gate = dispatch.registry.admission_gate();
    let outbound = dispatch.outbound.clone();
    let completed = dispatch.completed.clone();
    let session_shutdown = dispatch.shutdown.clone();
    let (acknowledgements, acknowledgement_receiver) = match method {
        RpcMethod::Unary(_) => (None, None),
        RpcMethod::Stream(_) | RpcMethod::LatestStream(_) => {
            let (sender, receiver) = mpsc::channel(1);
            (Some(sender), Some(receiver))
        }
    };
    let completion_id = request_id.clone();
    let panic_outbound = outbound.clone();
    let request_shutdown = session_shutdown.clone();
    let task = tokio::spawn(async move {
        let execution = AssertUnwindSafe(async move {
            let admitted = admit_request(
                &request,
                scope,
                &context,
                &admission_gate,
                &request_cancellation,
                &request_shutdown,
                &outbound,
            )
            .await;
            drop(inbound_guard);
            let Some(admission) = admitted else {
                return;
            };
            let context = context.with_admission(admission.clone());
            let _admission = admission;
            match method {
                RpcMethod::Unary(handler) => {
                    run_unary(
                        request,
                        handler,
                        context,
                        request_cancellation,
                        request_shutdown.clone(),
                        outbound,
                    )
                    .await;
                }
                RpcMethod::Stream(handler) => {
                    let Some(acknowledgement_receiver) = acknowledgement_receiver else {
                        return;
                    };
                    run_stream(
                        request,
                        handler,
                        context,
                        request_cancellation,
                        request_shutdown.clone(),
                        acknowledgement_receiver,
                        outbound,
                    )
                    .await;
                }
                RpcMethod::LatestStream(handler) => {
                    let Some(acknowledgement_receiver) = acknowledgement_receiver else {
                        return;
                    };
                    run_latest_stream(
                        request,
                        handler,
                        context,
                        request_cancellation,
                        request_shutdown.clone(),
                        acknowledgement_receiver,
                        outbound,
                    )
                    .await;
                }
            }
        })
        .catch_unwind()
        .await;
        if let Err(payload) = execution {
            let _ = send_server_message(
                &panic_outbound,
                &session_shutdown,
                ServerMessage::connection_defect(panic_payload_message(payload.as_ref())),
            )
            .await;
        }
        let _ = completed.send(completion_id).await;
    });
    in_flight.insert(
        request_id,
        InFlight {
            cancellation,
            acknowledgements,
            task,
        },
    );
}

/// Authorizes `request` against the auth store, unless the session is unauthenticated or the
/// method is the pending-pairing capability, then takes its maintenance admission. `None` once a
/// refusal or, for an interrupted request, its interrupt exit is queued.
async fn admit_request(
    request: &RpcRequest,
    scope: Option<&'static str>,
    context: &RpcSessionContext,
    admission_gate: &RpcAdmissionGate,
    cancellation: &CancellationToken,
    session_shutdown: &CancellationToken,
    outbound: &RpcOutboundQueue,
) -> Option<RpcPermit> {
    if let (Some(scope), Some(principal), Some(auth)) = (scope, &context.principal, &context.auth)
        && !context.has_pending_pairing_capability_for(&request.tag)
    {
        let authorized = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                let _ = try_send_control_message(
                    outbound,
                    ServerMessage::interrupt(request.id.clone()),
                );
                return None;
            }
            authorized = auth.authorize_session(&principal.session_id, scope) => authorized,
        };
        let refusal = match authorized {
            Ok(()) => None,
            Err(crate::auth::AuthError::ScopeRequired(_)) => Some(ServerMessage::failure(
                request.id.clone(),
                authorization_error(scope),
            )),
            Err(_) => Some(ServerMessage::connection_defect(
                "Authenticated session is no longer valid",
            )),
        };
        if let Some(refusal) = refusal {
            send_refusal(outbound, session_shutdown, refusal).await;
            return None;
        }
    }
    match admission_gate.admit_named(rpc_mutability(&request.tag), request.tag.clone()) {
        Ok(admission) => Some(admission),
        Err(error) => {
            send_refusal(
                outbound,
                session_shutdown,
                ServerMessage::failure(
                    request.id.clone(),
                    json!({
                        "_tag": "UpdateMaintenanceActiveError",
                        "message": error.to_string(),
                    }),
                ),
            )
            .await;
            None
        }
    }
}

/// Queues a refusal like any response, then through the control lane past a full byte budget.
/// A refusal that reaches neither ends the session: the client must never wait on a request
/// the server has dropped.
async fn send_refusal(
    outbound: &RpcOutboundQueue,
    session_shutdown: &CancellationToken,
    refusal: ServerMessage,
) {
    if send_server_message(outbound, session_shutdown, refusal.clone())
        .await
        .is_err()
        && send_unbudgeted_server_message(outbound, session_shutdown, refusal)
            .await
            .is_err()
    {
        session_shutdown.cancel();
    }
}

fn panic_payload_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_owned();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "RPC handler panicked with a non-string payload".to_owned()
}

async fn run_unary(
    request: RpcRequest,
    handler: UnaryHandler,
    context: RpcSessionContext,
    cancellation: CancellationToken,
    session_shutdown: CancellationToken,
    outbound: RpcOutboundQueue,
) {
    let request_id = request.id.clone();
    let method = request.tag.clone();
    let result = tokio::select! {
        biased;
        () = cancellation.cancelled() => {
            let _ = try_send_control_message(
                &outbound,
                ServerMessage::interrupt(request_id),
            );
            return;
        }
        result = handler(request, context, cancellation.clone()) => result,
    };
    let RpcUnaryResult {
        result,
        enqueue_guard,
    } = result;
    let response = match result {
        Ok(value) => ServerMessage::success(request_id.clone(), Some(value)),
        Err(error) => ServerMessage::failure(request_id.clone(), error),
    };
    if let Some(enqueue_guard) = enqueue_guard {
        let encoded_len_bound = if outbound.message_limit().is_some() {
            let Ok(encoded_len_bound) = enqueue_guard.encoded_len_bound(&response) else {
                return;
            };
            encoded_len_bound
        } else {
            0
        };
        match reserve_server_message(&outbound, &session_shutdown, encoded_len_bound).await {
            // Not counted: the guard picks the reply it sends after this, so only a bound
            // is known here.
            Ok(permit) => enqueue_guard.enqueue(permit, response),
            Err(failure) => {
                let _ = send_unbudgeted_server_message(
                    &outbound,
                    &session_shutdown,
                    ServerMessage::failure(request_id, failure.into_error(&method)),
                )
                .await;
            }
        }
    } else if let Err(failure) =
        send_method_message(&outbound, &session_shutdown, response, Some(&method)).await
    {
        let _ = send_unbudgeted_server_message(
            &outbound,
            &session_shutdown,
            ServerMessage::failure(request_id, failure.into_error(&method)),
        )
        .await;
    }
}

async fn run_stream(
    request: RpcRequest,
    handler: StreamHandler,
    context: RpcSessionContext,
    cancellation: CancellationToken,
    session_shutdown: CancellationToken,
    mut acknowledgements: mpsc::Receiver<()>,
    outbound: RpcOutboundQueue,
) {
    let request_id = request.id.clone();
    let method = request.tag.clone();
    let mut first_item = RequestTimer::stream(&request.headers);
    let mut stream = handler(request, context, cancellation.clone());
    loop {
        let item = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                let _ = try_send_control_message(
                    &outbound,
                    ServerMessage::interrupt(request_id),
                );
                return;
            }
            item = stream.recv() => item,
        };
        let Some(item) = item else {
            send_stream_terminal(
                &outbound,
                &session_shutdown,
                ServerMessage::success(request_id.clone(), None),
                request_id,
                &method,
            )
            .await;
            return;
        };
        if let Some(timer) = first_item.take() {
            timer.finish(&method, FIRST_ITEM);
        }
        match item {
            Err(error) => {
                send_stream_terminal(
                    &outbound,
                    &session_shutdown,
                    ServerMessage::failure(request_id.clone(), error),
                    request_id,
                    &method,
                )
                .await;
                return;
            }
            Ok(values) => {
                if values.is_empty() {
                    let _ = send_server_message(
                        &outbound,
                        &session_shutdown,
                        ServerMessage::connection_defect("RPC stream produced an empty Chunk"),
                    )
                    .await;
                    return;
                }
                if let Err(failure) = send_method_message(
                    &outbound,
                    &session_shutdown,
                    ServerMessage::Chunk {
                        request_id: request_id.clone(),
                        values,
                    },
                    Some(&method),
                )
                .await
                {
                    // The chunk lost admission or cannot fit the connection. Ending
                    // the subscription silently would strand the client, so deliver
                    // an explicit terminal through the control lane.
                    let _ = send_unbudgeted_server_message(
                        &outbound,
                        &session_shutdown,
                        ServerMessage::failure(request_id, failure.into_error(&method)),
                    )
                    .await;
                    return;
                }
                tokio::select! {
                    biased;
                    () = cancellation.cancelled() => {
                        let _ = try_send_control_message(
                            &outbound,
                            ServerMessage::interrupt(request_id),
                        );
                        return;
                    }
                    acknowledgement = acknowledgements.recv() => {
                        if acknowledgement.is_none() {
                            return;
                        }
                    }
                }
            }
        }
    }
}

async fn run_latest_stream(
    request: RpcRequest,
    handler: LatestStreamHandler,
    context: RpcSessionContext,
    cancellation: CancellationToken,
    session_shutdown: CancellationToken,
    mut acknowledgements: mpsc::Receiver<()>,
    outbound: RpcOutboundQueue,
) {
    let request_id = request.id.clone();
    let method = request.tag.clone();
    let mut first_item = RequestTimer::stream(&request.headers);
    let mut stream = handler(request, context, cancellation.clone());
    loop {
        let item = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                let _ = try_send_control_message(&outbound, ServerMessage::interrupt(request_id));
                return;
            }
            changed = stream.changed() => {
                if changed.is_err() {
                    send_latest_stream_terminal(
                        &outbound,
                        &session_shutdown,
                        &cancellation,
                        ServerMessage::success(request_id.clone(), None),
                        request_id,
                        &method,
                    ).await;
                    return;
                }
                stream.borrow_and_update().clone()
            }
        };
        let Some(item) = item else {
            continue;
        };
        if let Some(timer) = first_item.take() {
            timer.finish(&method, FIRST_ITEM);
        }
        match item {
            Err(error) => {
                send_latest_stream_terminal(
                    &outbound,
                    &session_shutdown,
                    &cancellation,
                    ServerMessage::failure(request_id.clone(), error),
                    request_id,
                    &method,
                )
                .await;
                return;
            }
            Ok(values) => {
                if values.is_empty() {
                    let _ = send_latest_stream_message(
                        &outbound,
                        &session_shutdown,
                        &cancellation,
                        ServerMessage::connection_defect("RPC stream produced an empty Chunk"),
                        None,
                    )
                    .await;
                    return;
                }
                if let Err(failure) = send_latest_stream_message(
                    &outbound,
                    &session_shutdown,
                    &cancellation,
                    ServerMessage::Chunk {
                        request_id: request_id.clone(),
                        values,
                    },
                    Some(&method),
                )
                .await
                {
                    if cancellation.is_cancelled() {
                        let _ = try_send_control_message(
                            &outbound,
                            ServerMessage::interrupt(request_id),
                        );
                    } else {
                        let _ = send_unbudgeted_server_message(
                            &outbound,
                            &session_shutdown,
                            ServerMessage::failure(request_id, failure.into_error(&method)),
                        )
                        .await;
                    }
                    return;
                }
                tokio::select! {
                    biased;
                    () = cancellation.cancelled() => {
                        let _ = try_send_control_message(
                            &outbound,
                            ServerMessage::interrupt(request_id),
                        );
                        return;
                    }
                    acknowledgement = acknowledgements.recv() => {
                        if acknowledgement.is_none() {
                            return;
                        }
                    }
                }
            }
        }
    }
}

async fn send_latest_stream_terminal(
    outbound: &RpcOutboundQueue,
    session_shutdown: &CancellationToken,
    cancellation: &CancellationToken,
    primary: ServerMessage,
    request_id: RequestId,
    method: &str,
) {
    if let Err(failure) =
        send_latest_stream_message(outbound, session_shutdown, cancellation, primary, None).await
    {
        if cancellation.is_cancelled() {
            let _ = try_send_control_message(outbound, ServerMessage::interrupt(request_id));
        } else {
            let _ = send_unbudgeted_server_message(
                outbound,
                session_shutdown,
                ServerMessage::failure(request_id, failure.into_error(method)),
            )
            .await;
        }
    }
}

async fn send_latest_stream_message(
    outbound: &RpcOutboundQueue,
    session_shutdown: &CancellationToken,
    cancellation: &CancellationToken,
    message: ServerMessage,
    method: Option<&str>,
) -> Result<(), SendFailure> {
    tokio::select! {
        biased;
        () = cancellation.cancelled() => Err(SendFailure::Rejected),
        result = send_method_message(outbound, session_shutdown, message, method) => result,
    }
}

async fn send_server_message(
    outbound: &RpcOutboundQueue,
    session_shutdown: &CancellationToken,
    message: ServerMessage,
) -> Result<(), SendFailure> {
    send_method_message(outbound, session_shutdown, message, None).await
}

/// [`send_server_message`] for a reply of `method`, whose encoded bytes the Profiler counts
/// where the connection encodes before admission (E2EE and record-framed sockets).
async fn send_method_message(
    outbound: &RpcOutboundQueue,
    session_shutdown: &CancellationToken,
    message: ServerMessage,
    method: Option<&str>,
) -> Result<(), SendFailure> {
    let deadline = Instant::now() + OUTBOUND_SEND_TIMEOUT;
    let mut counted = None;
    let limit = outbound.message_limit();
    let frame = if limit.is_some() {
        // Encode exactly once and drop the value tree before the admission
        // wait, so the memory resident while waiting is precisely the bytes
        // that will be charged.
        let encoded = serde_json::to_string(&message).map_err(SendFailure::rejected)?;
        drop(message);
        check_fits(encoded.len(), limit)?;
        counted = method.map(|method| (method, encoded.len()));
        let budget = outbound
            .acquire_budget(session_shutdown, encoded.len(), deadline)
            .await?;
        RpcOutboundFrame {
            payload: RpcOutboundPayload::Encoded(Message::Text(encoded.into())),
            _budget: budget,
            queued_at: Instant::now(),
        }
    } else {
        RpcOutboundFrame {
            payload: RpcOutboundPayload::Plain(message),
            _budget: None,
            queued_at: Instant::now(),
        }
    };
    admit_before(session_shutdown, deadline, outbound.sender.send(frame)).await?;
    if let Some((method, bytes)) = counted {
        latency::record_outbound_bytes(method, bytes);
    }
    Ok(())
}

/// Waits for `admission` until `deadline`. The session ending, the deadline
/// passing and the queue closing all reject the message.
async fn admit_before<T, E>(
    session_shutdown: &CancellationToken,
    deadline: Instant,
    admission: impl Future<Output = Result<T, E>>,
) -> Result<T, SendFailure> {
    tokio::select! {
        () = session_shutdown.cancelled() => Err(SendFailure::Rejected),
        result = timeout_at(deadline, admission) => {
            match result {
                Ok(Ok(admitted)) => Ok(admitted),
                Ok(Err(_)) | Err(_) => Err(SendFailure::Rejected),
            }
        }
    }
}

/// Sends a stream terminal, falling back to an explicit unbudgeted admission
/// failure when the budgeted send cannot be admitted — a subscription must
/// never end silently.
async fn send_stream_terminal(
    outbound: &RpcOutboundQueue,
    session_shutdown: &CancellationToken,
    primary: ServerMessage,
    request_id: RequestId,
    method: &str,
) {
    if let Err(failure) = send_server_message(outbound, session_shutdown, primary).await {
        let _ = send_unbudgeted_server_message(
            outbound,
            session_shutdown,
            ServerMessage::failure(request_id, failure.into_error(method)),
        )
        .await;
    }
}

/// Delivers an interrupt through the control lane without byte budgeting and
/// without waiting. Oversized controls use data framing and budgeting,
/// but remain control-class for all progress accounting.
fn try_send_control_message(
    outbound: &RpcOutboundQueue,
    message: ServerMessage,
) -> Result<(), SendFailure> {
    let encoded = serde_json::to_string(&message).map_err(SendFailure::rejected)?;
    if encoded.len() <= super::e2ee::MAX_E2EE_CHUNK_BYTES {
        return outbound
            .control
            .try_send(message)
            .map_err(SendFailure::rejected);
    }
    // A control larger than one record takes the budgeted data queue, so the
    // connection's message limit applies to it like any other message.
    check_fits(encoded.len(), outbound.message_limit())?;
    let permit = outbound
        .budget
        .as_ref()
        .map(|budget| budget.try_acquire(encoded.len()))
        .transpose()
        .map_err(SendFailure::rejected)?;
    outbound
        .sender
        .try_send(RpcOutboundFrame {
            payload: RpcOutboundPayload::Control(Message::Text(encoded.into())),
            _budget: permit,
            queued_at: Instant::now(),
        })
        .map_err(SendFailure::rejected)
}

/// Sends a bounded terminal or protocol error through the control lane,
/// bypassing the byte budget but waiting for lane capacity under the shared
/// deadline. Used when the budgeted path already failed, so the client still
/// observes a terminal.
async fn send_unbudgeted_server_message(
    outbound: &RpcOutboundQueue,
    session_shutdown: &CancellationToken,
    message: ServerMessage,
) -> Result<(), SendFailure> {
    let deadline = Instant::now() + OUTBOUND_SEND_TIMEOUT;
    let encoded = serde_json::to_string(&message).map_err(SendFailure::rejected)?;
    if encoded.len() > super::e2ee::MAX_E2EE_CHUNK_BYTES {
        drop(message);
        check_fits(encoded.len(), outbound.message_limit())?;
        let budget = outbound
            .acquire_budget(session_shutdown, encoded.len(), deadline)
            .await?;
        let frame = RpcOutboundFrame {
            payload: RpcOutboundPayload::Control(Message::Text(encoded.into())),
            _budget: budget,
            queued_at: Instant::now(),
        };
        return admit_before(session_shutdown, deadline, outbound.sender.send(frame)).await;
    }
    admit_before(session_shutdown, deadline, outbound.control.send(message)).await
}

/// Why an outbound message could not be queued.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SendFailure {
    /// The message could not be encoded, its queue was full or closed, its
    /// deadline passed, or the session ended.
    Rejected,
    /// The encoded message exceeds what this connection can deliver.
    TooLarge { bytes: usize, limit: usize },
}

impl SendFailure {
    /// The `map_err` adapter that turns any refusal not about size into
    /// [`Self::Rejected`], discarding its detail.
    fn rejected<E>(_refusal: E) -> Self {
        Self::Rejected
    }

    /// The typed failure the client receives for the request.
    fn into_error(self, method: &str) -> Value {
        match self {
            Self::Rejected => outbound_admission_failure(),
            Self::TooLarge { bytes, limit } => response_too_large_failure(method, bytes, limit),
        }
    }
}

/// Refuses a message of `bytes` over the connection's `limit`
/// ([`RpcOutboundQueue::message_limit`]; `None` admits any size). The only
/// place a [`SendFailure::TooLarge`] is built: it carries the exact `bytes`
/// and the limit they broke.
fn check_fits(bytes: usize, limit: Option<usize>) -> Result<(), SendFailure> {
    match limit {
        Some(limit) if bytes > limit => Err(SendFailure::TooLarge { bytes, limit }),
        _ => Ok(()),
    }
}

/// `RpcResponseTooLargeError` in `packages/contracts/src/rpcTransport.ts`.
pub(crate) fn response_too_large_failure(method: &str, bytes: usize, limit_bytes: usize) -> Value {
    json!({
        "_tag": "RpcResponseTooLargeError",
        "method": method,
        "bytes": bytes,
        "limitBytes": limit_bytes,
    })
}

fn outbound_admission_failure() -> Value {
    json!({
        "_tag": "RpcOutboundAdmissionError",
        "message": "The response could not be admitted to the outbound byte \
                    budget before its deadline.",
    })
}

async fn reserve_server_message(
    outbound: &RpcOutboundQueue,
    session_shutdown: &CancellationToken,
    encoded_len_bound: usize,
) -> Result<RpcResponseEnqueuePermit, SendFailure> {
    check_fits(encoded_len_bound, outbound.message_limit())?;
    let deadline = Instant::now() + OUTBOUND_SEND_TIMEOUT;
    let budget = outbound
        .acquire_budget(session_shutdown, encoded_len_bound, deadline)
        .await?;
    let permit = admit_before(
        session_shutdown,
        deadline,
        outbound.sender.clone().reserve_owned(),
    )
    .await?;
    Ok(RpcResponseEnqueuePermit {
        permit,
        budget,
        encoded_len_bound,
    })
}

pub(crate) fn encoded_server_message_len(
    message: &ServerMessage,
) -> Result<usize, serde_json::Error> {
    encoded_json_len(message)
}

fn decode_client_messages(bytes: &[u8]) -> Result<Vec<ClientMessage>, serde_json::Error> {
    let value: Value = serde_json::from_slice(bytes)?;
    match value {
        Value::Array(messages) => messages.into_iter().map(serde_json::from_value).collect(),
        message => serde_json::from_value(message).map(|message| vec![message]),
    }
}

fn client_protocol_error(message: String) -> ServerMessage {
    ServerMessage::ClientProtocolError {
        error: json!({
            "_tag": "RpcClientError",
            "reason": {
                "_tag": "RpcClientDefect",
                "message": message,
                "cause": message,
            }
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::byte_budget::RpcOutboundProcessBudget;
    use futures_util::stream;
    use std::{
        convert::Infallible,
        pin::Pin,
        task::{Context, Poll},
    };

    #[test]
    fn pending_pairing_capability_is_limited_to_confirmation() {
        let context = RpcSessionContext {
            pairing_confirmation: Some(PairingConfirmationLatch::default()),
            ..RpcSessionContext::default()
        };

        assert!(context.has_pending_pairing_capability_for("auth.confirmPairing"));
        assert!(!context.has_pending_pairing_capability_for("auth.rotateCredential"));
        assert!(
            !RpcSessionContext::default().has_pending_pairing_capability_for("auth.confirmPairing")
        );
    }

    #[derive(Default)]
    struct BlockedSocketSink {
        pending: Option<Message>,
    }

    impl Sink<Message> for BlockedSocketSink {
        type Error = Infallible;

        fn poll_ready(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
            assert!(
                self.pending.is_none(),
                "blocked sink owns one pending frame"
            );
            self.pending = Some(item);
            Ok(())
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Pending
        }

        fn poll_close(
            mut self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            self.pending = None;
            Poll::Ready(Ok(()))
        }
    }

    /// Accepts one frame and never finishes writing it, like a peer that stopped reading.
    struct StalledSocketSink {
        started: Option<tokio::sync::oneshot::Sender<()>>,
        dropped: Option<tokio::sync::oneshot::Sender<()>>,
    }

    impl StalledSocketSink {
        fn new() -> (
            Self,
            tokio::sync::oneshot::Receiver<()>,
            tokio::sync::oneshot::Receiver<()>,
        ) {
            let (started, started_receiver) = tokio::sync::oneshot::channel();
            let (dropped, dropped_receiver) = tokio::sync::oneshot::channel();
            (
                Self {
                    started: Some(started),
                    dropped: Some(dropped),
                },
                started_receiver,
                dropped_receiver,
            )
        }
    }

    impl Drop for StalledSocketSink {
        fn drop(&mut self) {
            if let Some(dropped) = self.dropped.take() {
                let _ = dropped.send(());
            }
        }
    }

    impl<T> Sink<T> for StalledSocketSink {
        type Error = Infallible;

        fn poll_ready(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(mut self: Pin<&mut Self>, _item: T) -> Result<(), Self::Error> {
            if let Some(started) = self.started.take() {
                let _ = started.send(());
            }
            Ok(())
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Pending
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Pending
        }
    }

    struct EnqueueNotification(mpsc::UnboundedSender<RequestId>);

    impl RpcResponseEnqueueGuard for EnqueueNotification {
        fn enqueue(self: Box<Self>, permit: RpcResponseEnqueuePermit, response: ServerMessage) {
            let request_id = response
                .request_id()
                .cloned()
                .expect("test response request id");
            let prepared = permit.prepare(response).expect("prepare test response");
            permit.send_prepared(prepared);
            let _ = self.0.send(request_id);
        }
    }

    struct InboundDropNotification(Option<tokio::sync::oneshot::Sender<()>>);

    impl Drop for InboundDropNotification {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    fn request_frame(ids: &[&str], tag: &str) -> RpcInboundFrame {
        RpcInboundFrame::plain(Message::Text(
            serde_json::to_string(
                &ids.iter()
                    .map(|id| {
                        json!({
                            "_tag": "Request",
                            "id": id,
                            "tag": tag,
                            "payload": {},
                            "headers": []
                        })
                    })
                    .collect::<Vec<_>>(),
            )
            .expect("request JSON")
            .into(),
        ))
    }

    fn unbudgeted_outbound(
        capacity: usize,
    ) -> (
        RpcOutboundQueue,
        mpsc::Receiver<RpcOutboundFrame>,
        mpsc::Receiver<ServerMessage>,
    ) {
        let (sender, receiver) = mpsc::channel(capacity);
        let (control, control_receiver) = mpsc::channel(CONTROL_LANE_CAPACITY);
        (
            RpcOutboundQueue {
                sender,
                control,
                budget: None,
                max_message_bytes: None,
            },
            receiver,
            control_receiver,
        )
    }

    #[tokio::test]
    async fn session_shutdown_unblocks_a_full_outbound_queue() {
        let (outbound, _receiver, _control) = unbudgeted_outbound(1);
        outbound.try_send(ServerMessage::Pong).expect("fill queue");
        let shutdown = CancellationToken::new();
        shutdown.cancel();

        timeout(
            Duration::from_millis(100),
            send_server_message(&outbound, &shutdown, ServerMessage::Pong),
        )
        .await
        .expect("send observes cancellation")
        .expect_err("cancelled session rejects outbound messages");
    }

    #[tokio::test]
    async fn inbound_guard_is_released_after_dispatch_not_handler_completion() {
        let (handler_started_tx, handler_started_rx) = tokio::sync::oneshot::channel();
        let handler_started_tx = Arc::new(std::sync::Mutex::new(Some(handler_started_tx)));
        let mut registry = RpcRegistry::empty();
        registry.register_unary("test.pending", move |_request, _cancellation| {
            let handler_started_tx = Arc::clone(&handler_started_tx);
            async move {
                if let Some(sender) = handler_started_tx
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take()
                {
                    let _ = sender.send(());
                }
                std::future::pending::<RpcResult>().await
            }
        });
        let (guard_dropped_tx, guard_dropped_rx) = tokio::sync::oneshot::channel();
        let plain = request_frame(&["1"], "test.pending");
        let guarded = RpcInboundFrame::guarded(
            plain.message,
            InboundDropNotification(Some(guard_dropped_tx)),
        );
        let (inbound_tx, inbound_rx) = mpsc::channel(1);
        let reader = stream::unfold(inbound_rx, |mut receiver| async {
            receiver.recv().await.map(|item| (Ok(item), receiver))
        });
        let shutdown = CancellationToken::new();
        let session = tokio::spawn(run_session_split_budgeted(
            BlockedSocketSink::default(),
            OutboundFraming::Whole,
            ConnectionLiveness::new(),
            reader,
            registry,
            RpcSessionContext::unauthenticated(),
            shutdown.clone(),
            None,
        ));

        inbound_tx.send(guarded).await.expect("guarded request");
        timeout(Duration::from_secs(1), handler_started_rx)
            .await
            .expect("handler dispatches")
            .expect("handler start signal");
        timeout(Duration::from_secs(1), guard_dropped_rx)
            .await
            .expect("input budget releases after dispatch")
            .expect("guard drop signal");

        shutdown.cancel();
        drop(inbound_tx);
        timeout(Duration::from_secs(2), session)
            .await
            .expect("session cleanup deadline")
            .expect("session joins");
    }

    #[tokio::test]
    async fn latest_stream_request_cancellation_unblocks_a_full_outbound_queue() {
        let handler: LatestStreamHandler = Arc::new(|_request, _context, _cancellation| {
            let (sender, receiver) = watch::channel(None);
            sender.send_replace(Some(Ok(vec![json!({ "generation": 1 })])));
            receiver
        });
        let request = RpcRequest {
            id: RequestId::try_from("1").expect("request id"),
            tag: "subscribeWorktreeCatalog".to_owned(),
            payload: json!({}),
            headers: Vec::new(),
            trace_id: None,
            span_id: None,
            sampled: None,
        };
        let (outbound, _receiver, _control) = unbudgeted_outbound(1);
        outbound.try_send(ServerMessage::Pong).expect("fill queue");
        let (_acknowledgements, acknowledgement_receiver) = mpsc::channel(1);
        let cancellation = CancellationToken::new();
        let task = tokio::spawn(run_latest_stream(
            request,
            handler,
            RpcSessionContext::unauthenticated(),
            cancellation.clone(),
            CancellationToken::new(),
            acknowledgement_receiver,
            outbound,
        ));
        tokio::task::yield_now().await;
        cancellation.cancel();

        timeout(Duration::from_millis(100), task)
            .await
            .expect("request cancellation unblocks the outbound capacity wait")
            .expect("latest stream task joins");
    }

    #[tokio::test(start_paused = true)]
    async fn stream_admission_expiry_delivers_a_terminal_failure() {
        let process = RpcOutboundProcessBudget::new(1024);
        let _blocker = process.try_acquire(1024).expect("hold process capacity");
        let (sender, _receiver) = mpsc::channel(8);
        let (control, mut control_receiver) = mpsc::channel(CONTROL_LANE_CAPACITY);
        let outbound = RpcOutboundQueue {
            sender,
            control,
            budget: Some(RpcOutboundBudget::new(process.clone(), 1024)),
            max_message_bytes: None,
        };
        let (chunk_sender, chunk_receiver) = mpsc::channel(1);
        chunk_sender
            .try_send(Ok(vec![json!({ "payload": "x".repeat(128) })]))
            .expect("queue stream chunk");
        let chunk_receiver = Arc::new(std::sync::Mutex::new(Some(chunk_receiver)));
        let handler: StreamHandler = Arc::new(move |_request, _context, _cancellation| {
            chunk_receiver
                .lock()
                .expect("stream receiver lock")
                .take()
                .expect("single stream invocation")
        });
        let request = RpcRequest {
            id: RequestId::try_from("1").expect("request id"),
            tag: "test.stream".to_owned(),
            payload: json!({}),
            headers: Vec::new(),
            trace_id: None,
            span_id: None,
            sampled: None,
        };
        let (_ack_sender, ack_receiver) = mpsc::channel(1);
        let task = tokio::spawn(run_stream(
            request,
            handler,
            RpcSessionContext::unauthenticated(),
            CancellationToken::new(),
            CancellationToken::new(),
            ack_receiver,
            outbound,
        ));
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(6)).await;

        let terminal = timeout(Duration::from_secs(1), control_receiver.recv())
            .await
            .expect("a terminal reaches the control lane")
            .expect("terminal message");
        let text = serde_json::to_string(&terminal).expect("terminal JSON");
        assert!(
            text.contains("RpcOutboundAdmissionError"),
            "admission expiry must surface as an explicit stream failure: {text}"
        );
        let decoded: Value = serde_json::from_str(&text).expect("terminal JSON");
        assert_eq!(decoded["requestId"], "1");
        drop(chunk_sender);
        timeout(Duration::from_secs(1), task)
            .await
            .expect("stream task ends after the terminal")
            .expect("stream task joins");
    }

    #[tokio::test]
    async fn latest_stream_cancellation_delivers_an_interrupt_past_queued_budget_waiters() {
        let process = RpcOutboundProcessBudget::new(64);
        let _blocker = process.try_acquire(64).expect("hold process capacity");
        let (sender, _receiver) = mpsc::channel(8);
        let (control, mut control_receiver) = mpsc::channel(CONTROL_LANE_CAPACITY);
        let outbound = RpcOutboundQueue {
            sender,
            control,
            budget: Some(RpcOutboundBudget::new(process.clone(), 64)),
            max_message_bytes: None,
        };
        let waiter_outbound = outbound.clone();
        let waiter_shutdown = CancellationToken::new();
        let waiter = tokio::spawn(async move {
            let _ =
                send_server_message(&waiter_outbound, &waiter_shutdown, ServerMessage::Pong).await;
        });
        tokio::task::yield_now().await;

        let keep_streams = Arc::new(std::sync::Mutex::new(Vec::new()));
        let handler_streams = Arc::clone(&keep_streams);
        let handler: LatestStreamHandler = Arc::new(move |_request, _context, _cancellation| {
            let (stream_sender, stream_receiver) = watch::channel(None);
            handler_streams
                .lock()
                .expect("stream keep-alive lock")
                .push(stream_sender);
            stream_receiver
        });
        let request = RpcRequest {
            id: RequestId::try_from("1").expect("request id"),
            tag: "test.latest".to_owned(),
            payload: json!({}),
            headers: Vec::new(),
            trace_id: None,
            span_id: None,
            sampled: None,
        };
        let (_ack_sender, ack_receiver) = mpsc::channel(1);
        let cancellation = CancellationToken::new();
        let task = tokio::spawn(run_latest_stream(
            request,
            handler,
            RpcSessionContext::unauthenticated(),
            cancellation.clone(),
            CancellationToken::new(),
            ack_receiver,
            outbound,
        ));
        tokio::task::yield_now().await;
        cancellation.cancel();

        let interrupt = timeout(Duration::from_millis(500), control_receiver.recv())
            .await
            .expect("the interrupt is delivered despite queued budget waiters")
            .expect("interrupt message");
        let text = serde_json::to_string(&interrupt).expect("interrupt JSON");
        assert!(
            text.contains("Interrupt"),
            "cancellation must surface as an interrupt exit: {text}"
        );
        timeout(Duration::from_secs(1), task)
            .await
            .expect("latest stream task ends after the interrupt")
            .expect("latest stream task joins");
        waiter.abort();
        let _ = waiter.await;
    }

    #[tokio::test]
    async fn session_teardown_is_bounded_when_a_handler_ignores_cancellation() {
        let mut registry = RpcRegistry::empty();
        registry.register_unary("test.hang", |_request, _cancellation| async {
            std::future::pending::<RpcResult>().await
        });
        let (inbound_sender, inbound_receiver) = mpsc::channel(1);
        let reader = stream::unfold(inbound_receiver, |mut receiver| async {
            receiver.recv().await.map(|item| (Ok(item), receiver))
        });
        let shutdown = CancellationToken::new();
        let session = tokio::spawn(run_session_split_budgeted(
            BlockedSocketSink::default(),
            OutboundFraming::Whole,
            ConnectionLiveness::new(),
            reader,
            registry,
            RpcSessionContext::unauthenticated(),
            shutdown.clone(),
            None,
        ));
        inbound_sender
            .send(request_frame(&["1"], "test.hang"))
            .await
            .expect("send hanging request");
        tokio::task::yield_now().await;
        drop(inbound_sender);

        timeout(Duration::from_secs(3), session)
            .await
            .expect("teardown bounds in-flight joins and aborts stragglers")
            .expect("session task joins");
    }

    /// Issue #84: the bounded join awaited a handle that already returned when the timeout
    /// fired after it, and tokio panics on a second await of a finished `JoinHandle`.
    #[tokio::test(start_paused = true)]
    async fn teardown_joins_each_finished_request_once_and_aborts_the_rest() {
        let (aborted_sender, aborted) = tokio::sync::oneshot::channel();
        // Ignores cancellation: only an abort ends it.
        let stubborn = tokio::spawn(async move {
            let _aborted = InboundDropNotification(Some(aborted_sender));
            std::future::pending::<()>().await;
        });
        let first = tokio::spawn(async {});
        let last = tokio::spawn(async {});
        tokio::task::yield_now().await;
        let started = Instant::now();

        join_request_tasks(vec![first, stubborn, last]).await;

        assert_eq!(started.elapsed(), PUMP_JOIN_TIMEOUT);
        timeout(Duration::from_secs(1), aborted)
            .await
            .expect("the straggler is aborted")
            .expect("abort signal");
    }

    #[tokio::test]
    async fn slow_socket_cannot_hide_more_than_one_large_response_in_the_session_queue() {
        let response = Arc::new("x".repeat(40 * 1024 * 1024));
        let (enqueued, mut enqueue_events) = mpsc::unbounded_channel();
        let mut registry = RpcRegistry::empty();
        registry.register_guarded_unary("test.largeResponse", move |_request, _cancellation| {
            let response = Arc::clone(&response);
            let enqueued = enqueued.clone();
            async move {
                RpcUnaryResult::guarded(
                    Ok(json!({ "value": response.as_str() })),
                    EnqueueNotification(enqueued),
                )
            }
        });
        let (inbound_sender, inbound_receiver) = mpsc::channel(1);
        let reader = stream::unfold(inbound_receiver, |mut receiver| async {
            receiver.recv().await.map(|item| (Ok(item), receiver))
        });
        let shutdown = CancellationToken::new();
        let session = tokio::spawn(run_session_split_budgeted(
            BlockedSocketSink::default(),
            OutboundFraming::Whole,
            ConnectionLiveness::new(),
            reader,
            registry,
            RpcSessionContext::unauthenticated(),
            shutdown.clone(),
            Some(RpcOutboundBudget::new(
                RpcOutboundProcessBudget::new(128 * 1024 * 1024),
                64 * 1024 * 1024,
            )),
        ));
        inbound_sender
            .send(request_frame(&["1", "2"], "test.largeResponse"))
            .await
            .expect("send two large requests");

        timeout(Duration::from_secs(1), enqueue_events.recv())
            .await
            .expect("first response is enqueued")
            .expect("first enqueue notification");
        assert!(
            timeout(Duration::from_millis(100), enqueue_events.recv())
                .await
                .is_err(),
            "the connection byte budget must stop the second response before enqueue"
        );

        shutdown.cancel();
        drop(inbound_sender);
        timeout(Duration::from_secs(2), session)
            .await
            .expect("session cleanup deadline")
            .expect("session task joins");
    }

    #[tokio::test]
    async fn slow_sockets_share_one_process_outbound_plaintext_budget() {
        let process_budget = RpcOutboundProcessBudget::new(128 * 1024 * 1024);
        let response = Arc::new("x".repeat(48 * 1024 * 1024));
        let (enqueued, mut enqueue_events) = mpsc::unbounded_channel();
        let mut sessions = HashMap::new();

        for request_id in ["1", "2", "3"] {
            let mut registry = RpcRegistry::empty();
            let response = Arc::clone(&response);
            let enqueued = enqueued.clone();
            registry.register_guarded_unary(
                "test.largeResponse",
                move |_request, _cancellation| {
                    let response = Arc::clone(&response);
                    let enqueued = enqueued.clone();
                    async move {
                        RpcUnaryResult::guarded(
                            Ok(json!({ "value": response.as_str() })),
                            EnqueueNotification(enqueued),
                        )
                    }
                },
            );
            let (inbound_sender, inbound_receiver) = mpsc::channel(1);
            let reader = stream::unfold(inbound_receiver, |mut receiver| async {
                receiver.recv().await.map(|item| (Ok(item), receiver))
            });
            let shutdown = CancellationToken::new();
            let task = tokio::spawn(run_session_split_budgeted(
                BlockedSocketSink::default(),
                OutboundFraming::Whole,
                ConnectionLiveness::new(),
                reader,
                registry,
                RpcSessionContext::unauthenticated(),
                shutdown.clone(),
                Some(RpcOutboundBudget::new(
                    process_budget.clone(),
                    64 * 1024 * 1024,
                )),
            ));
            inbound_sender
                .send(request_frame(&[request_id], "test.largeResponse"))
                .await
                .expect("send large request");
            sessions.insert(request_id.to_owned(), (shutdown, inbound_sender, task));
        }

        let first = timeout(Duration::from_secs(2), enqueue_events.recv())
            .await
            .expect("first process-budgeted response")
            .expect("first enqueue event");
        let second = timeout(Duration::from_secs(2), enqueue_events.recv())
            .await
            .expect("second process-budgeted response")
            .expect("second enqueue event");
        assert_ne!(first, second);
        assert!(
            timeout(Duration::from_millis(100), enqueue_events.recv())
                .await
                .is_err(),
            "the process byte budget must stop the third slow-reader response before enqueue"
        );

        let first_id = first.as_str().to_owned();
        let (shutdown, inbound, task) = sessions.remove(&first_id).expect("first session owner");
        shutdown.cancel();
        drop(inbound);
        timeout(Duration::from_secs(2), task)
            .await
            .expect("cancelled session cleanup deadline")
            .expect("cancelled session joins");
        let third = timeout(Duration::from_secs(2), enqueue_events.recv())
            .await
            .expect("released process bytes admit the third response")
            .expect("third enqueue event");
        assert_ne!(third, first);
        assert_ne!(third, second);

        for (_, (shutdown, inbound, task)) in sessions {
            shutdown.cancel();
            drop(inbound);
            timeout(Duration::from_secs(2), task)
                .await
                .expect("session cleanup deadline")
                .expect("session joins");
        }
    }

    #[tokio::test]
    async fn response_larger_than_the_connection_budget_fails_only_its_request() {
        let response = Arc::new("x".repeat(2 * 1024));
        let (enqueued, mut enqueue_events) = mpsc::unbounded_channel();
        let mut registry = RpcRegistry::empty();
        registry.register_guarded_unary(
            "test.oversizedResponse",
            move |_request, _cancellation| {
                let response = Arc::clone(&response);
                let enqueued = enqueued.clone();
                async move {
                    RpcUnaryResult::guarded(
                        Ok(json!({ "value": response.as_str() })),
                        EnqueueNotification(enqueued),
                    )
                }
            },
        );
        registry.register_unary("test.small", |_request, _cancellation| async {
            Ok(json!({ "ok": true }))
        });
        let recorded = Arc::new(std::sync::Mutex::new(Vec::<Message>::new()));
        let sink_recorded = Arc::clone(&recorded);
        let sink = Box::pin(futures_util::sink::unfold(
            (),
            move |(), message: Message| {
                let recorded = Arc::clone(&sink_recorded);
                async move {
                    recorded.lock().expect("recorded frames").push(message);
                    Ok::<_, Infallible>(())
                }
            },
        ));
        let (inbound_sender, inbound_receiver) = mpsc::channel(1);
        let reader = stream::unfold(inbound_receiver, |mut receiver| async {
            receiver.recv().await.map(|item| (Ok(item), receiver))
        });
        let shutdown = CancellationToken::new();
        let session = tokio::spawn(run_session_split_budgeted(
            sink,
            OutboundFraming::Whole,
            ConnectionLiveness::new(),
            reader,
            registry,
            RpcSessionContext::unauthenticated(),
            shutdown.clone(),
            Some(RpcOutboundBudget::new(
                RpcOutboundProcessBudget::new(4 * 1024),
                1024,
            )),
        ));
        inbound_sender
            .send(request_frame(&["1"], "test.oversizedResponse"))
            .await
            .expect("send oversized response request");
        inbound_sender
            .send(request_frame(&["2"], "test.small"))
            .await
            .expect("send small request");

        let frames = timeout(Duration::from_secs(2), async {
            loop {
                let frames: Vec<Value> = recorded
                    .lock()
                    .expect("recorded frames")
                    .iter()
                    .filter_map(|message| match message {
                        Message::Text(text) => serde_json::from_str(text).ok(),
                        _ => None,
                    })
                    .collect();
                if frames.len() >= 2 {
                    return frames;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("both requests are answered");
        assert!(
            !shutdown.is_cancelled(),
            "an oversized response keeps the session open"
        );
        assert!(
            enqueue_events.try_recv().is_err(),
            "the oversized response is never enqueued"
        );
        let oversized = frames
            .iter()
            .find(|frame| frame["requestId"] == "1")
            .expect("a failure for request 1");
        let error = &oversized["exit"]["cause"][0]["error"];
        assert_eq!(error["_tag"], "RpcResponseTooLargeError");
        assert_eq!(error["method"], "test.oversizedResponse");
        assert_eq!(error["limitBytes"], 1024);
        let refused = ServerMessage::success(
            RequestId::try_from("1").expect("request id"),
            Some(json!({ "value": "x".repeat(2 * 1024) })),
        );
        assert_eq!(
            error["bytes"].as_u64(),
            Some(
                u64::try_from(
                    serde_json::to_string(&refused)
                        .expect("response JSON")
                        .len()
                )
                .expect("size fits u64")
            ),
            "the failure reports the refused response's exact encoded size"
        );
        let small = frames
            .iter()
            .find(|frame| frame["requestId"] == "2")
            .expect("a response for request 2");
        assert_eq!(small["exit"]["_tag"], "Success");
        shutdown.cancel();
        drop(inbound_sender);
        timeout(Duration::from_secs(2), session)
            .await
            .expect("session cleanup deadline")
            .expect("session joins");
    }

    #[tokio::test]
    async fn profiler_counts_reply_bytes_by_method_where_they_are_encoded() {
        use crate::diagnostics::profiler::latency::{self, PinnedProfiling};
        let _profiling = PinnedProfiling::new(true);
        let (sender, mut receiver) = mpsc::channel(4);
        let (control, _control_receiver) = mpsc::channel(CONTROL_LANE_CAPACITY);
        let outbound = RpcOutboundQueue {
            sender,
            control,
            budget: None,
            max_message_bytes: Some(1 << 20),
        };
        let reply = ServerMessage::success(
            RequestId::try_from("1").expect("request id"),
            Some(json!({ "data": "x".repeat(100) })),
        );
        let encoded = serde_json::to_string(&reply).expect("reply JSON").len();
        let shutdown = CancellationToken::new();
        for _ in 0..2 {
            send_method_message(
                &outbound,
                &shutdown,
                reply.clone(),
                Some("test.profiler.bytes"),
            )
            .await
            .expect("admitted");
            receiver.recv().await.expect("queued frame");
        }
        send_server_message(&outbound, &shutdown, reply)
            .await
            .expect("admitted");

        let payloads = latency::global_latency().outbound_bytes_for_test(u64::MAX);
        let counted = payloads
            .iter()
            .find(|payload| payload["method"] == "test.profiler.bytes")
            .expect("rpc.bytes for the method");
        assert_eq!(counted["messages"], 2);
        assert_eq!(counted["bytes"], 2 * encoded);
        assert_eq!(counted["maxBytes"], encoded);
    }

    #[tokio::test]
    async fn record_framed_plain_sessions_refuse_messages_over_the_limit() {
        let (sender, _receiver) = mpsc::channel(1);
        let (control, _control_receiver) = mpsc::channel(CONTROL_LANE_CAPACITY);
        let outbound = RpcOutboundQueue {
            sender,
            control,
            budget: None,
            max_message_bytes: Some(1024),
        };
        let response = |data_bytes: usize| {
            ServerMessage::success(
                RequestId::try_from("1").expect("request id"),
                Some(json!({ "data": "x".repeat(data_bytes) })),
            )
        };
        let envelope = serde_json::to_string(&response(0))
            .expect("response JSON")
            .len();
        let at_limit = response(1024 - envelope);
        assert_eq!(
            serde_json::to_string(&at_limit)
                .expect("response JSON")
                .len(),
            1024
        );
        send_server_message(&outbound, &CancellationToken::new(), at_limit)
            .await
            .expect("a message of exactly the limit is sent");

        let failure = send_server_message(
            &outbound,
            &CancellationToken::new(),
            response(1025 - envelope),
        )
        .await;
        assert_eq!(
            failure,
            Err(SendFailure::TooLarge {
                bytes: 1025,
                limit: 1024
            }),
            "the failure reports the exact encoded size and the limit"
        );
    }

    #[test]
    fn response_too_large_failure_matches_the_typescript_wire_fixture() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../../packages/contracts/fixtures/rpc-wire/exit-response-too-large.json"
        ))
        .expect("fixture JSON");
        let message = ServerMessage::failure(
            RequestId::try_from("900719925474099312345").expect("request id"),
            response_too_large_failure("gitManager.getCommits", 70_000_000, 67_108_864),
        );
        assert_eq!(
            serde_json::to_value(message).expect("message JSON"),
            fixture
        );
    }

    #[tokio::test(start_paused = true)]
    async fn byte_and_queue_admission_share_one_five_second_deadline() {
        let process = RpcOutboundProcessBudget::new(1024);
        let process_blocker = process.try_acquire(1024).expect("hold process capacity");
        let (sender, _receiver) = mpsc::channel(1);
        sender
            .try_send(RpcOutboundFrame::plain(ServerMessage::Pong))
            .expect("fill response queue");
        let (control, _control_receiver) = mpsc::channel(CONTROL_LANE_CAPACITY);
        let outbound = RpcOutboundQueue {
            sender,
            control,
            budget: Some(RpcOutboundBudget::new(process.clone(), 1024)),
            max_message_bytes: None,
        };
        let shutdown = CancellationToken::new();
        let send = tokio::spawn(async move {
            send_server_message(&outbound, &shutdown, ServerMessage::Pong).await
        });

        tokio::time::advance(Duration::from_secs(4)).await;
        drop(process_blocker);
        tokio::task::yield_now().await;
        assert!(
            !send.is_finished(),
            "queue admission still owns the final second"
        );
        tokio::time::advance(Duration::from_millis(999)).await;
        tokio::task::yield_now().await;
        assert!(!send.is_finished(), "the shared deadline has not elapsed");
        tokio::time::advance(Duration::from_millis(1)).await;
        assert_eq!(
            send.await.expect("send task joins"),
            Err(SendFailure::Rejected)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn unary_rpc_failures_are_persisted_for_restart_diagnostics() {
        let directory = tempfile::tempdir().expect("temporary diagnostics directory");
        let trace_path = directory.path().join("server.trace.ndjson");
        let diagnostics = TraceDiagnosticsStore::new(trace_path.clone());
        let mut registry = RpcRegistry::with_trace_diagnostics(diagnostics);
        registry.register_unary("git.createWorktree", |_request, _cancellation| async {
            tokio::time::sleep(Duration::from_millis(25)).await;
            Err(json!({
                "_tag": "GitCommandError",
                "detail": "fatal: bad config line 3 in .gitmodules"
            }))
        });
        let request = RpcRequest {
            id: RequestId::try_from("1").expect("request id"),
            tag: "git.createWorktree".to_owned(),
            payload: json!({}),
            headers: Vec::new(),
            trace_id: None,
            span_id: None,
            sampled: None,
        };
        let RpcMethod::Unary(handler) = registry.get("git.createWorktree").expect("handler") else {
            panic!("expected unary handler");
        };

        handler(
            request,
            RpcSessionContext::unauthenticated(),
            CancellationToken::new(),
        )
        .await
        .result
        .expect_err("fixture RPC fails");

        let after_restart = TraceDiagnosticsStore::new(trace_path).read();
        assert_eq!(after_restart["failureCount"], 1);
        assert_eq!(after_restart["latestFailures"][0]["durationMeasured"], true);
        assert_eq!(after_restart["latestFailures"][0]["durationMs"], 25.0);
        assert_eq!(
            after_restart["latestFailures"][0]["cause"],
            "fatal: bad config line 3 in .gitmodules"
        );
    }

    #[tokio::test]
    async fn stacked_action_failure_is_recorded_once_by_its_owner() {
        let directory = tempfile::tempdir().expect("temporary diagnostics directory");
        let store = TraceDiagnosticsStore::new(directory.path().join("server.trace.ndjson"));
        let mut registry = RpcRegistry::with_trace_diagnostics(store.clone());
        crate::production::git_vcs::register_git_vcs_rpc(
            &mut registry,
            crate::production::git_vcs::GitVcsRpcServices::default(),
        );
        let RpcMethod::Stream(handler) = registry
            .get("git.runStackedAction")
            .expect("stacked handler")
        else {
            panic!("expected stream handler");
        };
        let mut stream = handler(
            RpcRequest {
                id: RequestId::try_from("1").unwrap(),
                tag: "git.runStackedAction".into(),
                payload: json!({"actionId":"private-action-sentinel", "cwd":directory.path(), "action":"create_pr"}),
                headers: Vec::new(),
                trace_id: None,
                span_id: None,
                sampled: None,
            },
            RpcSessionContext::unauthenticated(),
            CancellationToken::new(),
        );
        let events = tokio::time::timeout(Duration::from_secs(2), async {
            let mut events = Vec::new();
            while let Some(chunk) = stream.recv().await {
                events.extend(chunk.expect("domain errors remain progress events"));
            }
            events
        })
        .await
        .expect("owned stream settles");
        assert_eq!(
            events
                .iter()
                .filter(|event| event["kind"] == "action_failed")
                .count(),
            1
        );
        assert!(
            !events
                .iter()
                .any(|event| event["kind"] == "action_finished")
        );
        let result = store.read();
        assert_eq!(result["failureCount"], 1);
        assert_eq!(result["latestFailures"][0]["name"], "git.runStackedAction");
        assert_eq!(result["latestFailures"][0]["durationMeasured"], true);
        assert!(
            !result["latestFailures"][0]["cause"]
                .as_str()
                .unwrap()
                .contains("sentinel")
        );
    }

    #[tokio::test]
    async fn diagnostics_write_failure_preserves_unary_and_stacked_action_errors() {
        let directory = tempfile::tempdir().expect("temporary diagnostics directory");
        let blocked_path = directory.path().join("directory-instead-of-trace-file");
        std::fs::create_dir(&blocked_path).expect("owned unwritable trace target");
        let store = TraceDiagnosticsStore::new(blocked_path);
        assert!(
            store
                .record_failure("fixture", &json!({"detail":"fixture"}))
                .is_err()
        );
        let mut registry = RpcRegistry::with_trace_diagnostics(store);
        let expected_error = json!({"_tag":"FixtureError", "detail":"original failure"});
        let handler_error = expected_error.clone();
        registry.register_unary("test.failure", move |_request, _cancellation| {
            let error = handler_error.clone();
            async move { Err(error) }
        });
        crate::production::git_vcs::register_git_vcs_rpc(
            &mut registry,
            crate::production::git_vcs::GitVcsRpcServices::default(),
        );
        let request = |tag: &str, payload| RpcRequest {
            id: RequestId::try_from("1").unwrap(),
            tag: tag.into(),
            payload,
            headers: Vec::new(),
            trace_id: None,
            span_id: None,
            sampled: None,
        };
        let RpcMethod::Unary(unary) = registry.get("test.failure").unwrap() else {
            panic!("expected unary handler");
        };
        let result = unary(
            request("test.failure", json!({})),
            RpcSessionContext::unauthenticated(),
            CancellationToken::new(),
        )
        .await;
        assert_eq!(result.result, Err(expected_error));

        let RpcMethod::Stream(stacked) = registry.get("git.runStackedAction").unwrap() else {
            panic!("expected stream handler");
        };
        let mut stream = stacked(
            request(
                "git.runStackedAction",
                json!({
                    "actionId":"owned-action", "cwd":directory.path(), "action":"create_pr",
                }),
            ),
            RpcSessionContext::unauthenticated(),
            CancellationToken::new(),
        );
        let events = tokio::time::timeout(Duration::from_secs(2), async {
            let mut events = Vec::new();
            while let Some(chunk) = stream.recv().await {
                events.extend(chunk.expect("domain failure keeps its progress event"));
            }
            events
        })
        .await
        .expect("stream still settles when diagnostics cannot be written");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["kind"], "action_started");
        assert_eq!(events[1]["kind"], "action_failed");
        assert_eq!(
            events[1]["message"],
            "Select a target branch before creating a pull request."
        );
    }

    #[tokio::test(start_paused = true)]
    async fn writer_failure_ends_the_session_and_drops_the_socket_within_one_second() {
        let mut registry = RpcRegistry::empty();
        registry.register_unary("test.echo", |_request, _cancellation| async {
            Ok(json!({ "value": "x".repeat(1024) }))
        });
        let (inbound_sender, inbound_receiver) = mpsc::channel(1);
        let reader = stream::unfold(inbound_receiver, |mut receiver| async {
            receiver.recv().await.map(|item| (Ok(item), receiver))
        });
        let (sink, started, dropped) = StalledSocketSink::new();
        let shutdown = CancellationToken::new();
        let session = tokio::spawn(run_session_split_budgeted(
            sink,
            OutboundFraming::Whole,
            ConnectionLiveness::new(),
            reader,
            registry,
            RpcSessionContext::unauthenticated(),
            shutdown.clone(),
            None,
        ));
        inbound_sender
            .send(request_frame(&["1"], "test.echo"))
            .await
            .expect("send request");
        timeout(Duration::from_secs(1), started)
            .await
            .expect("the response reaches the socket")
            .expect("start signal");

        tokio::time::advance(Duration::from_secs(31)).await;
        timeout(Duration::from_secs(1), shutdown.cancelled())
            .await
            .expect("a failed write ends the session");
        timeout(Duration::from_secs(1), dropped)
            .await
            .expect("the socket is released within one second")
            .expect("drop signal");
        timeout(Duration::from_secs(1), session)
            .await
            .expect("session ends")
            .expect("session joins");
        drop(inbound_sender);
    }

    #[tokio::test]
    async fn a_ping_flood_fills_the_control_lane_without_ending_the_read_loop() {
        let (started_sender, started_receiver) = tokio::sync::oneshot::channel();
        let started_sender = Arc::new(std::sync::Mutex::new(Some(started_sender)));
        let mut registry = RpcRegistry::empty();
        registry.register_unary("test.after", move |_request, _cancellation| {
            let started_sender = Arc::clone(&started_sender);
            async move {
                if let Some(sender) = started_sender
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take()
                {
                    let _ = sender.send(());
                }
                Ok(json!({}))
            }
        });
        let pings =
            serde_json::to_string(&vec![json!({ "_tag": "Ping" }); CONTROL_LANE_CAPACITY * 2])
                .expect("ping batch");
        let (inbound_sender, inbound_receiver) = mpsc::channel(2);
        let reader = stream::unfold(inbound_receiver, |mut receiver| async {
            receiver.recv().await.map(|item| (Ok(item), receiver))
        });
        let shutdown = CancellationToken::new();
        let session = tokio::spawn(run_session_split_budgeted(
            BlockedSocketSink::default(),
            OutboundFraming::Whole,
            ConnectionLiveness::new(),
            reader,
            registry,
            RpcSessionContext::unauthenticated(),
            shutdown.clone(),
            None,
        ));
        inbound_sender
            .send(RpcInboundFrame::plain(Message::Text(pings.into())))
            .await
            .expect("send pings");
        inbound_sender
            .send(request_frame(&["1"], "test.after"))
            .await
            .expect("send request");

        timeout(Duration::from_secs(1), started_receiver)
            .await
            .expect("the read loop survives a full control lane")
            .expect("handler start signal");
        assert!(!shutdown.is_cancelled());
        shutdown.cancel();
        drop(inbound_sender);
        timeout(Duration::from_secs(2), session)
            .await
            .expect("session cleanup deadline")
            .expect("session joins");
    }

    /// A periodic task that holds the single database worker must not stop the read loop:
    /// request authorization reads the database, and a read loop waiting on it answers no
    /// Ping, so a client closes a live connection after its 30 s liveness limit.
    #[tokio::test]
    async fn pings_are_answered_while_request_authorization_waits_on_the_database() {
        use crate::{
            auth::{SecretStore, SessionTransport},
            config::ServerConfig,
            persistence::{Database, Repositories, run_migrations},
        };

        let database = Database::open_in_memory().await.expect("database opens");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations apply");
        let secrets = tempfile::tempdir().expect("secret store directory");
        let config = ServerConfig::new(".")
            .with_bind("127.0.0.1", 3773)
            .with_desktop("liveness-test-seed")
            .expect("desktop config");
        let auth = AuthService::new_with_persistence(
            &config,
            vec![7_u8; 32],
            SecretStore::new(secrets.path())
                .await
                .expect("secret store opens"),
            Repositories::new(database.clone()),
        )
        .await
        .expect("auth service starts");
        let issued = auth
            .exchange_bootstrap(
                "liveness-test-seed",
                None,
                ClientMetadata::default(),
                None,
                SessionTransport::Plain,
            )
            .await
            .expect("desktop session issues");
        let mut registry = RpcRegistry::empty();
        registry.register_unary("assets.createUrl", |_request, _cancellation| async {
            Ok(json!({}))
        });
        let (outbound_sender, mut outbound) = mpsc::unbounded_channel::<Message>();
        let sink = Box::pin(futures_util::sink::unfold(
            outbound_sender,
            |sender, message: Message| async move {
                let _ = sender.send(message);
                Ok::<_, Infallible>(sender)
            },
        ));
        let (inbound_sender, inbound_receiver) = mpsc::channel(4);
        let reader = stream::unfold(inbound_receiver, |mut receiver| async {
            receiver.recv().await.map(|item| (Ok(item), receiver))
        });
        let shutdown = CancellationToken::new();
        let session = tokio::spawn(run_session_split_budgeted(
            sink,
            OutboundFraming::Whole,
            ConnectionLiveness::new(),
            reader,
            registry,
            RpcSessionContext::authenticated(issued.principal, auth),
            shutdown.clone(),
            None,
        ));

        // A periodic task holds the database worker.
        let (busy_sender, busy_receiver) = tokio::sync::oneshot::channel();
        let (release_sender, release_receiver) = std::sync::mpsc::channel::<()>();
        let busy_database = database.clone();
        let busy = tokio::spawn(async move {
            busy_database
                .call(move |_connection| {
                    let _ = busy_sender.send(());
                    let _ = release_receiver.recv();
                    Ok(())
                })
                .await
        });
        busy_receiver.await.expect("the worker is busy");
        // The request's inbound budget is held until it is authorized.
        let (guard_dropped_sender, mut guard_dropped) = tokio::sync::oneshot::channel();
        inbound_sender
            .send(RpcInboundFrame::guarded(
                request_frame(&["1"], "assets.createUrl").message,
                InboundDropNotification(Some(guard_dropped_sender)),
            ))
            .await
            .expect("send request");
        inbound_sender
            .send(RpcInboundFrame::plain(Message::Text(
                r#"{"_tag":"Ping"}"#.into(),
            )))
            .await
            .expect("send ping");
        let tag = |message: Message| {
            let Message::Text(text) = message else {
                panic!("legacy framing writes text frames");
            };
            let value: Value = serde_json::from_str(&text).expect("frame JSON");
            value["_tag"].as_str().unwrap_or_default().to_owned()
        };
        let answer = timeout(Duration::from_secs(2), outbound.recv())
            .await
            .expect("the Ping is answered while the database is busy")
            .expect("outbound frame");
        assert_eq!(tag(answer), "Pong");
        assert!(
            guard_dropped.try_recv().is_err(),
            "the inbound budget stays held while authorization waits"
        );

        release_sender.send(()).expect("release the worker");
        busy.await.expect("busy task joins").expect("busy call");
        let response = timeout(Duration::from_secs(2), outbound.recv())
            .await
            .expect("the request completes once the database is free")
            .expect("outbound frame");
        assert_eq!(tag(response), "Exit");
        timeout(Duration::from_secs(1), guard_dropped)
            .await
            .expect("the inbound budget is released once the request is admitted")
            .expect("guard drop signal");
        shutdown.cancel();
        drop(inbound_sender);
        timeout(Duration::from_secs(2), session)
            .await
            .expect("session cleanup deadline")
            .expect("session joins");
        database.close().await;
    }

    #[tokio::test(start_paused = true)]
    async fn control_messages_overtake_queued_responses() {
        let mut registry = RpcRegistry::empty();
        registry.register_unary("test.large", |_request, _cancellation| async {
            Ok(json!({ "data": "x".repeat(8 * 1024) }))
        });
        let recorded = Arc::new(std::sync::Mutex::new(Vec::<Message>::new()));
        let sink_recorded = Arc::clone(&recorded);
        let sink = Box::pin(futures_util::sink::unfold(
            (),
            move |(), message: Message| {
                let recorded = Arc::clone(&sink_recorded);
                async move {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    recorded.lock().expect("recorded frames").push(message);
                    Ok::<_, Infallible>(())
                }
            },
        ));
        let (inbound_sender, inbound_receiver) = mpsc::channel(4);
        let reader = stream::unfold(inbound_receiver, |mut receiver| async {
            receiver.recv().await.map(|item| (Ok(item), receiver))
        });
        let shutdown = CancellationToken::new();
        let session = tokio::spawn(run_session_split_budgeted(
            sink,
            OutboundFraming::Whole,
            ConnectionLiveness::new(),
            reader,
            registry,
            RpcSessionContext::unauthenticated(),
            shutdown.clone(),
            None,
        ));
        inbound_sender
            .send(request_frame(&["1", "2"], "test.large"))
            .await
            .expect("send requests");
        tokio::time::sleep(Duration::from_millis(500)).await;
        inbound_sender
            .send(RpcInboundFrame::plain(Message::Text(
                r#"{"_tag":"Ping"}"#.into(),
            )))
            .await
            .expect("send ping");
        tokio::time::sleep(Duration::from_secs(5)).await;

        let tags: Vec<String> = recorded
            .lock()
            .expect("recorded frames")
            .iter()
            .map(|message| {
                let Message::Text(text) = message else {
                    panic!("legacy framing writes text frames");
                };
                let value: Value = serde_json::from_str(text).expect("frame JSON");
                value["_tag"].as_str().unwrap_or_default().to_owned()
            })
            .collect();
        assert_eq!(
            tags,
            vec!["Exit", "Pong", "Exit"],
            "the Pong overtakes the queued response"
        );
        shutdown.cancel();
        drop(inbound_sender);
        timeout(Duration::from_secs(2), session)
            .await
            .expect("session cleanup deadline")
            .expect("session joins");
    }

    #[tokio::test]
    async fn oversized_controls_use_the_data_lane() {
        let (outbound, mut data, mut control) = unbudgeted_outbound(2);
        let message = ServerMessage::success(
            RequestId::try_from("1").expect("id"),
            Some(json!({ "data": "x".repeat(64 * 1024) })),
        );
        try_send_control_message(&outbound, message.clone()).expect("sync data admission");
        assert!(control.try_recv().is_err());
        assert!(data.try_recv().expect("sync fallback frame").is_control());
        timeout(
            Duration::from_secs(1),
            send_unbudgeted_server_message(&outbound, &CancellationToken::new(), message),
        )
        .await
        .expect("bounded admission")
        .expect("async data admission");
        assert!(control.try_recv().is_err());
        assert!(data.try_recv().expect("async fallback frame").is_control());
    }

    /// Runs one frame of `requests` through a session and returns the response frames once
    /// `responses` of them were written. Each request is observed before its response is sent.
    async fn answer_frame(registry: RpcRegistry, requests: Value, responses: usize) -> Vec<Value> {
        let (written, mut frames) = mpsc::unbounded_channel();
        let sink = Box::pin(futures_util::sink::unfold(
            written,
            |written, message: Message| async move {
                if let Message::Text(text) = &message
                    && let Ok(frame) = serde_json::from_str::<Value>(text)
                {
                    let _ = written.send(frame);
                }
                Ok::<_, Infallible>(written)
            },
        ));
        let (inbound_sender, inbound_receiver) = mpsc::channel(1);
        let reader = stream::unfold(inbound_receiver, |mut receiver| async {
            receiver.recv().await.map(|item| (Ok(item), receiver))
        });
        let shutdown = CancellationToken::new();
        let session = tokio::spawn(run_session_split_budgeted(
            sink,
            OutboundFraming::Whole,
            ConnectionLiveness::new(),
            reader,
            registry,
            RpcSessionContext::unauthenticated(),
            shutdown.clone(),
            None,
        ));
        inbound_sender
            .send(RpcInboundFrame::plain(Message::Text(
                requests.to_string().into(),
            )))
            .await
            .expect("send requests");
        let mut answered = Vec::new();
        while answered.len() < responses {
            answered.push(
                timeout(Duration::from_secs(2), frames.recv())
                    .await
                    .expect("every request is answered")
                    .expect("response frame"),
            );
        }
        shutdown.cancel();
        drop(inbound_sender);
        timeout(Duration::from_secs(2), session)
            .await
            .expect("session cleanup deadline")
            .expect("session joins");
        answered
    }

    /// Registers `method` to answer whether it saw itself in the profiler's in-flight registry.
    fn register_in_flight_probe(registry: &mut RpcRegistry, method: &'static str) {
        registry.register_unary(method, move |_request, _cancellation| async move {
            Ok(json!(crate::diagnostics::profiler::latency::in_flight(
                method
            )))
        });
    }

    fn answer_of<'a>(frames: &'a [Value], id: &str) -> &'a Value {
        &frames
            .iter()
            .find(|frame| frame["requestId"] == id)
            .expect("a response for the request")["exit"]["value"]
    }

    // `#[tokio::test]` runs the session and its request tasks on this thread, which the pin
    // reaches; the process-global profiler handle belongs to whichever test started first.
    #[tokio::test]
    async fn profiler_joins_a_request_by_its_correlation_header_only() {
        use crate::diagnostics::profiler::latency::{self, PinnedProfiling};
        let _profiling = PinnedProfiling::new(true);
        let mut registry = RpcRegistry::empty();
        register_in_flight_probe(&mut registry, "test.profiler.joined");
        register_in_flight_probe(&mut registry, "test.profiler.unjoined");
        registry.register_stream("test.profiler.stream", |_request, _cancellation| {
            let (sender, receiver) = mpsc::channel(1);
            let _ = sender.try_send(Ok(vec![json!(1)]));
            receiver
        });

        let frames = answer_frame(
            registry,
            json!([
                {
                    "_tag": "Request", "id": "1", "tag": "test.profiler.joined", "payload": {},
                    "headers": [["X-Bibcode-Profiler-Rid", "t9:c1:7"]]
                },
                {
                    "_tag": "Request", "id": "2", "tag": "test.profiler.unjoined", "payload": {},
                    "headers": [["traceparent", "00-t9"]]
                },
                {
                    "_tag": "Request", "id": "3", "tag": "test.profiler.stream", "payload": {},
                    "headers": [["x-bibcode-profiler-rid", "t9:s:1"]]
                }
            ]),
            3,
        )
        .await;

        assert_eq!(
            answer_of(&frames, "1"),
            &json!(true),
            "registered while running"
        );
        assert!(
            !latency::in_flight("test.profiler.joined"),
            "removed on completion"
        );
        let book = latency::global_latency();
        let joined = book.find_join("t9:c1:7").expect("joined by the header");
        assert_eq!(joined.method, "test.profiler.joined");
        assert_eq!(
            book.find_join("t9:s:1").map(|entry| entry.method),
            Some("test.profiler.stream".to_owned()),
            "a stream is joined at its first item"
        );
        assert!(
            book.windows
                .lock()
                .expect("windows")
                .contains_key(&("test.profiler.unjoined".to_owned(), "success")),
            "the request without the header was still timed"
        );
        assert!(
            !book
                .joins
                .lock()
                .expect("join log")
                .entries
                .iter()
                .any(|entry| entry.method == "test.profiler.unjoined"),
            "a request without the header is not joined"
        );
    }

    #[tokio::test]
    async fn profiler_off_leaves_requests_untouched() {
        use crate::diagnostics::profiler::latency::{self, PinnedProfiling};
        let _profiling = PinnedProfiling::new(false);
        let mut registry = RpcRegistry::empty();
        register_in_flight_probe(&mut registry, "test.profiler.off");

        let frames = answer_frame(
            registry,
            json!([{
                "_tag": "Request", "id": "1", "tag": "test.profiler.off", "payload": {},
                "headers": [["x-bibcode-profiler-rid", "t9:off:1"]]
            }]),
            1,
        )
        .await;

        assert_eq!(answer_of(&frames, "1"), &json!(false), "never registered");
        let book = latency::global_latency();
        assert!(book.find_join("t9:off:1").is_none());
        assert!(
            !book
                .windows
                .lock()
                .expect("windows")
                .keys()
                .any(|(method, _)| method == "test.profiler.off")
        );
    }
}
