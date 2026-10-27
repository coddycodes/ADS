//! Preview tunnels (#69): a desktop whose preview profile routes loopback traffic through
//! SOCKS reaches this server's loopback ports over its own RPC connection.
//!
//! `preview.tunnelOpen { port }` connects to `127.0.0.1:<port>`, then `[::1]:<port>`, and
//! streams `Opened { tunnelId }` followed by `Data { data }` (base64) as the upstream sends;
//! the stream ends when the upstream closes. The transport sends the next chunk only after
//! the client acknowledged the last, so a slow page cannot pile data up here.
//! `preview.tunnelWrite` sends one chunk and returns once it is written, so a client that
//! waits for each write cannot pile data up either. `preview.tunnelClose`, the stream's
//! cancellation, the principal's revocation, and shutdown close a tunnel.

use std::{
    collections::HashMap,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicU16, Ordering},
    },
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, tcp::OwnedWriteHalf},
    sync::{Semaphore, mpsc},
};
use tokio_util::sync::CancellationToken;

use super::gateway::proxy::{PRINCIPAL_CHECK_INTERVAL, PrincipalCheck};
use crate::{rpc::RpcStreamChunk, signed_token::now_millis};

/// Open tunnels per Coddy Studio session; one page rarely holds more than a dozen connections.
pub const MAX_TUNNELS_PER_SESSION: usize = 64;
/// Open tunnels across every session of this server.
pub const MAX_TUNNELS: usize = 256;
const READ_CHUNK_BYTES: usize = 64 * 1024;
/// The largest decoded chunk one `preview.tunnelWrite` may carry.
pub const MAX_WRITE_BYTES: usize = 256 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// An upstream that takes no bytes for this long is stuck: the tunnel closes, so a page that
/// went away meanwhile cannot hold it.
const WRITE_STALL_TIMEOUT: Duration = Duration::from_secs(60);
const PRINCIPAL_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TunnelError {
    #[error("Previews can't open a tunnel to this port.")]
    NotAdmitted,
    #[error("Nothing is listening on that port on the server.")]
    NoUpstream,
    #[error("Too many preview connections are open.")]
    Busy,
    #[error("This preview connection is closed.")]
    Closed,
    #[error("The preview connection sent data the server can't accept.")]
    InvalidData,
    #[error("The session that opened this preview connection ended.")]
    Revoked,
}

impl TunnelError {
    /// The `PreviewTunnelError` the contract defines.
    pub fn to_value(self) -> Value {
        let reason = match self {
            Self::NotAdmitted => "not-admitted",
            Self::NoUpstream => "no-upstream",
            Self::Busy => "busy",
            Self::Closed => "closed",
            Self::InvalidData => "invalid-data",
            Self::Revoked => "revoked",
        };
        json!({ "_tag": "PreviewTunnelError", "reason": reason, "message": self.to_string() })
    }
}

/// The open tunnels of one server.
#[derive(Clone)]
pub struct PreviewTunnels {
    inner: Arc<Inner>,
}

struct Inner {
    /// The server's own port: reaching it from loopback would earn the trust the server gives
    /// local proxies (forwarded headers, pre-auth admission). The bound port, once known.
    own_port: AtomicU16,
    principal_check: Arc<dyn PrincipalCheck>,
    principal_check_interval: Duration,
    global: Arc<Semaphore>,
    state: Mutex<State>,
    shutdown: CancellationToken,
}

#[derive(Default)]
struct State {
    tunnels: HashMap<String, Tunnel>,
    /// Tunnels each session holds, counted from admission so concurrent opens cannot overshoot.
    per_session: HashMap<String, usize>,
}

struct Tunnel {
    session_id: String,
    writer: Arc<tokio::sync::Mutex<OwnedWriteHalf>>,
    closed: CancellationToken,
}

impl PreviewTunnels {
    pub fn new(own_port: u16, principal_check: Arc<dyn PrincipalCheck>) -> Self {
        Self::with_check_interval(own_port, principal_check, PRINCIPAL_CHECK_INTERVAL)
    }

    pub fn with_check_interval(
        own_port: u16,
        principal_check: Arc<dyn PrincipalCheck>,
        principal_check_interval: Duration,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                own_port: AtomicU16::new(own_port),
                principal_check,
                principal_check_interval,
                global: Arc::new(Semaphore::new(MAX_TUNNELS)),
                state: Mutex::default(),
                shutdown: CancellationToken::new(),
            }),
        }
    }

    /// Opens a tunnel to `port` for Coddy Studio session `session_id` and returns its stream.
    pub fn open(
        &self,
        port: u16,
        session_id: String,
        cancellation: CancellationToken,
    ) -> mpsc::Receiver<RpcStreamChunk> {
        let (sender, receiver) = mpsc::channel(2);
        let inner = self.inner.clone();
        tokio::spawn(async move {
            if let Err(error) = inner
                .run(port, session_id, &sender, cancellation.clone())
                .await
            {
                // A client that stopped reading cannot hold this task either.
                tokio::select! {
                    () = cancellation.cancelled() => {}
                    () = inner.shutdown.cancelled() => {}
                    _ = sender.send(Err(error.to_value())) => {}
                }
            }
        });
        receiver
    }

    /// Writes one base64 chunk to tunnel `tunnel_id`, which `session_id` must have opened.
    pub async fn write(
        &self,
        tunnel_id: &str,
        session_id: &str,
        data: &str,
    ) -> Result<(), TunnelError> {
        if data.len() > MAX_WRITE_BYTES.div_ceil(3) * 4 {
            return Err(TunnelError::InvalidData);
        }
        let bytes = STANDARD
            .decode(data)
            .map_err(|_| TunnelError::InvalidData)?;
        let (writer, closed) = {
            let state = self.inner.lock();
            let tunnel = state
                .tunnels
                .get(tunnel_id)
                .filter(|tunnel| tunnel.session_id == session_id)
                .ok_or(TunnelError::Closed)?;
            (tunnel.writer.clone(), tunnel.closed.clone())
        };
        let mut writer = writer.lock().await;
        tokio::select! {
            () = closed.cancelled() => Err(TunnelError::Closed),
            written = tokio::time::timeout(WRITE_STALL_TIMEOUT, writer.write_all(&bytes)) => {
                match written {
                    Ok(Ok(())) => Ok(()),
                    _ => {
                        closed.cancel();
                        Err(TunnelError::Closed)
                    }
                }
            }
        }
    }

    /// Closes tunnel `tunnel_id` if `session_id` opened it; closing a closed one is fine.
    pub fn close(&self, tunnel_id: &str, session_id: &str) {
        if let Some(tunnel) = self
            .inner
            .lock()
            .tunnels
            .get(tunnel_id)
            .filter(|tunnel| tunnel.session_id == session_id)
        {
            tunnel.closed.cancel();
        }
    }

    /// Records the port the server is listening on: started with port 0, it learns it only
    /// once bound.
    pub fn set_own_port(&self, port: u16) {
        self.inner.own_port.store(port, Ordering::Relaxed);
    }

    /// Closes every tunnel and refuses new ones.
    pub fn shutdown(&self) {
        self.inner.shutdown.cancel();
    }

    #[cfg(test)]
    fn open_count(&self) -> usize {
        self.inner.lock().tunnels.len()
    }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Counts a tunnel against `session_id`'s share, or refuses it; [`Self::release`] undoes it.
    fn reserve(&self, session_id: &str) -> Result<(), TunnelError> {
        let mut state = self.lock();
        let held = state.per_session.entry(session_id.to_owned()).or_default();
        if *held >= MAX_TUNNELS_PER_SESSION {
            return Err(TunnelError::Busy);
        }
        *held += 1;
        Ok(())
    }

    fn release(&self, session_id: &str) {
        let mut state = self.lock();
        if let Some(held) = state.per_session.get_mut(session_id) {
            *held -= 1;
            if *held == 0 {
                state.per_session.remove(session_id);
            }
        }
    }

    async fn run(
        &self,
        port: u16,
        session_id: String,
        sender: &mpsc::Sender<RpcStreamChunk>,
        cancellation: CancellationToken,
    ) -> Result<(), TunnelError> {
        if port == 0
            || port == self.own_port.load(Ordering::Relaxed)
            || self.shutdown.is_cancelled()
        {
            return Err(TunnelError::NotAdmitted);
        }
        self.reserve(&session_id)?;
        let result = self
            .run_reserved(port, &session_id, sender, cancellation)
            .await;
        self.release(&session_id);
        result
    }

    async fn run_reserved(
        &self,
        port: u16,
        session_id: &str,
        sender: &mpsc::Sender<RpcStreamChunk>,
        cancellation: CancellationToken,
    ) -> Result<(), TunnelError> {
        let _permit = self
            .global
            .clone()
            .try_acquire_owned()
            .map_err(|_| TunnelError::Busy)?;
        // An abandoned open releases its reservations at once instead of after the dial.
        let upstream = tokio::select! {
            () = cancellation.cancelled() => return Ok(()),
            () = self.shutdown.cancelled() => return Ok(()),
            () = sender.closed() => return Ok(()),
            upstream = connect_loopback(port) => upstream?,
        };
        let (mut reader, writer) = upstream.into_split();
        let tunnel_id = uuid::Uuid::new_v4().to_string();
        let closed = CancellationToken::new();
        self.lock().tunnels.insert(
            tunnel_id.clone(),
            Tunnel {
                session_id: session_id.to_owned(),
                writer: Arc::new(tokio::sync::Mutex::new(writer)),
                closed: closed.clone(),
            },
        );
        // A send waits for the client to take the last chunk; closing, cancelling or shutting
        // down still ends the tunnel while it waits.
        // Revocation is checked apart from the stream, so a client that stopped reading cannot
        // keep a revoked session's tunnel open.
        let revoked = CancellationToken::new();
        let watcher = tokio::spawn(watch_principal(
            self.principal_check.clone(),
            self.principal_check_interval,
            session_id.to_owned(),
            revoked.clone(),
            closed.clone(),
        ));
        let send = |chunk: Value| {
            let (cancellation, closed, revoked) = (&cancellation, &closed, &revoked);
            async move {
                tokio::select! {
                    () = revoked.cancelled() => Err(TunnelError::Revoked),
                    () = cancellation.cancelled() => Ok(false),
                    () = self.shutdown.cancelled() => Ok(false),
                    () = closed.cancelled() => Ok(false),
                    // The transport dropped the stream (its byte budget ended it, say).
                    () = sender.closed() => Ok(false),
                    sent = sender.send(Ok(vec![chunk])) => {
                        sent.map(|()| true).map_err(|_| TunnelError::Closed)
                    }
                }
            }
        };
        let result = async {
            if !send(json!({ "_tag": "Opened", "tunnelId": tunnel_id })).await? {
                return Ok(());
            }
            let mut buffer = vec![0_u8; READ_CHUNK_BYTES];
            loop {
                let read = tokio::select! {
                    () = revoked.cancelled() => return Err(TunnelError::Revoked),
                    () = cancellation.cancelled() => return Ok(()),
                    () = self.shutdown.cancelled() => return Ok(()),
                    () = closed.cancelled() => return Ok(()),
                    () = sender.closed() => return Ok(()),
                    read = reader.read(&mut buffer) => read,
                };
                let Ok(read @ 1..) = read else {
                    // The upstream closed (or failed): the stream ends.
                    return Ok(());
                };
                let chunk = json!({ "_tag": "Data", "data": STANDARD.encode(&buffer[..read]) });
                if !send(chunk).await? {
                    return Ok(());
                }
            }
        }
        .await;
        closed.cancel();
        watcher.abort();
        let removed = self.lock().tunnels.remove(&tunnel_id);
        if let Some(tunnel) = removed {
            let _ = tunnel.writer.lock().await.shutdown().await;
        }
        result
    }
}

/// Cancels `revoked` once `session_id` is no longer active, checking every `interval` until
/// `closed`. A check that times out keeps the tunnel: the next one decides.
async fn watch_principal(
    principal_check: Arc<dyn PrincipalCheck>,
    interval: Duration,
    session_id: String,
    revoked: CancellationToken,
    closed: CancellationToken,
) {
    let mut ticks = tokio::time::interval(interval);
    ticks.tick().await;
    loop {
        tokio::select! {
            () = closed.cancelled() => return,
            _ = ticks.tick() => {}
        }
        let expiry = tokio::time::timeout(
            PRINCIPAL_CHECK_TIMEOUT,
            principal_check.session_expiry(&session_id),
        )
        .await;
        let Ok(expiry) = expiry else {
            continue;
        };
        let now = i64::try_from(now_millis()).unwrap_or(i64::MAX);
        if !expiry.is_some_and(|expires_at| expires_at > now) {
            revoked.cancel();
            return;
        }
    }
}

/// `127.0.0.1:port`, then `[::1]:port`: a dev server may listen on either.
async fn connect_loopback(port: u16) -> Result<TcpStream, TunnelError> {
    for ip in [
        std::net::IpAddr::V4(Ipv4Addr::LOCALHOST),
        std::net::IpAddr::V6(Ipv6Addr::LOCALHOST),
    ] {
        if let Ok(Ok(stream)) = tokio::time::timeout(
            CONNECT_TIMEOUT,
            TcpStream::connect(SocketAddr::new(ip, port)),
        )
        .await
        {
            let _ = stream.set_nodelay(true);
            return Ok(stream);
        }
    }
    Err(TunnelError::NoUpstream)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use futures_util::future::BoxFuture;
    use tokio::net::TcpListener;

    use super::*;

    struct Sessions(AtomicBool);

    impl PrincipalCheck for Sessions {
        fn session_expiry<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Option<i64>> {
            let live = self.0.load(Ordering::SeqCst);
            Box::pin(async move { live.then_some(i64::MAX) })
        }
    }

    fn tunnels(own_port: u16) -> (PreviewTunnels, Arc<Sessions>) {
        let sessions = Arc::new(Sessions(AtomicBool::new(true)));
        let tunnels = PreviewTunnels::with_check_interval(
            own_port,
            sessions.clone(),
            Duration::from_millis(50),
        );
        (tunnels, sessions)
    }

    /// An upstream that echoes what it reads, upper-cased.
    async fn echo_upstream() -> u16 {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buffer = [0_u8; 1024];
                    while let Ok(read @ 1..) = socket.read(&mut buffer).await {
                        let upper = buffer[..read].to_ascii_uppercase();
                        if socket.write_all(&upper).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        port
    }

    async fn next(receiver: &mut mpsc::Receiver<RpcStreamChunk>) -> Result<Value, Value> {
        let chunk = tokio::time::timeout(Duration::from_secs(5), receiver.recv())
            .await
            .expect("a chunk in time")
            .expect("the stream is open");
        chunk.map(|mut values| values.remove(0))
    }

    #[tokio::test]
    async fn a_tunnel_carries_bytes_both_ways_for_its_own_session() {
        let (tunnels, _) = tunnels(1);
        let port = echo_upstream().await;
        let mut stream = tunnels.open(port, "s1".into(), CancellationToken::new());
        let opened = next(&mut stream).await.unwrap();
        assert_eq!(opened["_tag"], "Opened");
        let id = opened["tunnelId"].as_str().unwrap().to_owned();

        assert_eq!(
            tunnels.write(&id, "s2", &STANDARD.encode("hi")).await,
            Err(TunnelError::Closed),
            "another session cannot write to it"
        );
        tunnels
            .write(&id, "s1", &STANDARD.encode("hello"))
            .await
            .unwrap();
        let data = next(&mut stream).await.unwrap();
        assert_eq!(data["_tag"], "Data");
        assert_eq!(
            STANDARD.decode(data["data"].as_str().unwrap()).unwrap(),
            b"HELLO"
        );
        assert_eq!(
            tunnels.write(&id, "s1", "not base64!").await,
            Err(TunnelError::InvalidData)
        );

        tunnels.close(&id, "s2");
        assert_eq!(tunnels.open_count(), 1, "another session cannot close it");
        tunnels.close(&id, "s1");
        assert!(
            tokio::time::timeout(Duration::from_secs(5), stream.recv())
                .await
                .unwrap()
                .is_none(),
            "closing ends the stream"
        );
        assert_eq!(tunnels.open_count(), 0);
        assert_eq!(
            tunnels.write(&id, "s1", &STANDARD.encode("x")).await,
            Err(TunnelError::Closed)
        );
    }

    #[tokio::test]
    async fn the_servers_own_port_and_closed_ports_are_refused() {
        // Started with port 0, the server learns its port once bound.
        let (tunnels, _) = tunnels(0);
        tunnels.set_own_port(4242);
        let error = |chunk: Result<Value, Value>| chunk.unwrap_err()["reason"].clone();
        let mut own = tunnels.open(4242, "s1".into(), CancellationToken::new());
        assert_eq!(error(next(&mut own).await), "not-admitted");
        let mut zero = tunnels.open(0, "s1".into(), CancellationToken::new());
        assert_eq!(error(next(&mut zero).await), "not-admitted");

        let closed_port = {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            listener.local_addr().unwrap().port()
        };
        let mut nothing = tunnels.open(closed_port, "s1".into(), CancellationToken::new());
        assert_eq!(error(next(&mut nothing).await), "no-upstream");
    }

    #[tokio::test]
    async fn the_upstream_closing_or_the_session_ending_ends_the_stream() {
        let (tunnels, sessions) = tunnels(1);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut stream = tunnels.open(port, "s1".into(), CancellationToken::new());
        let (upstream, _) = listener.accept().await.unwrap();
        assert_eq!(next(&mut stream).await.unwrap()["_tag"], "Opened");
        drop(upstream);
        assert!(
            tokio::time::timeout(Duration::from_secs(5), stream.recv())
                .await
                .unwrap()
                .is_none()
        );

        let port = echo_upstream().await;
        let mut revoked = tunnels.open(port, "s1".into(), CancellationToken::new());
        assert_eq!(next(&mut revoked).await.unwrap()["_tag"], "Opened");
        sessions.0.store(false, Ordering::SeqCst);
        assert_eq!(next(&mut revoked).await.unwrap_err()["reason"], "revoked");
        assert_eq!(tunnels.open_count(), 0);
    }

    #[tokio::test]
    async fn closing_a_tunnel_releases_it_while_the_client_is_not_reading() {
        let (tunnels, _) = tunnels(1);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut stream = tunnels.open(port, "s1".into(), CancellationToken::new());
        let (mut upstream, _) = listener.accept().await.unwrap();
        let id = next(&mut stream).await.unwrap()["tunnelId"]
            .as_str()
            .unwrap()
            .to_owned();
        // More than the stream holds while nobody reads it.
        for _ in 0..8 {
            upstream.write_all(&[7_u8; 1024]).await.unwrap();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tunnels.close(&id, "s1");
        tokio::time::timeout(Duration::from_secs(5), async {
            while tunnels.open_count() > 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the tunnel is released although its stream is full");
        drop(stream);
    }

    #[tokio::test]
    async fn revoking_the_session_releases_a_tunnel_whose_client_stopped_reading() {
        let (tunnels, sessions) = tunnels(1);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut stream = tunnels.open(port, "s1".into(), CancellationToken::new());
        let (mut upstream, _) = listener.accept().await.unwrap();
        assert_eq!(next(&mut stream).await.unwrap()["_tag"], "Opened");
        for _ in 0..8 {
            upstream.write_all(&[7_u8; 1024]).await.unwrap();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        sessions.0.store(false, Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(5), async {
            while tunnels.open_count() > 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("a revoked session's tunnel is released although its stream is full");
        drop(stream);
    }

    #[tokio::test]
    async fn a_dropped_stream_releases_its_tunnel() {
        let (tunnels, _) = tunnels(1);
        let port = echo_upstream().await;
        let mut stream = tunnels.open(port, "s1".into(), CancellationToken::new());
        assert_eq!(next(&mut stream).await.unwrap()["_tag"], "Opened");
        drop(stream);
        tokio::time::timeout(Duration::from_secs(5), async {
            while tunnels.open_count() > 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the tunnel is released once nobody can read its stream");
    }

    #[tokio::test]
    async fn a_session_holds_at_most_its_share_of_tunnels() {
        let (tunnels, _) = tunnels(1);
        let port = echo_upstream().await;
        let mut streams = Vec::new();
        for _ in 0..MAX_TUNNELS_PER_SESSION {
            let mut stream = tunnels.open(port, "s1".into(), CancellationToken::new());
            assert_eq!(next(&mut stream).await.unwrap()["_tag"], "Opened");
            streams.push(stream);
        }
        let mut over = tunnels.open(port, "s1".into(), CancellationToken::new());
        assert_eq!(next(&mut over).await.unwrap_err()["reason"], "busy");
        let mut other = tunnels.open(port, "s2".into(), CancellationToken::new());
        assert_eq!(next(&mut other).await.unwrap()["_tag"], "Opened");

        tunnels.shutdown();
        for mut stream in streams {
            assert!(
                tokio::time::timeout(Duration::from_secs(5), stream.recv())
                    .await
                    .unwrap()
                    .is_none(),
                "shutdown ends every tunnel"
            );
        }
    }
}
