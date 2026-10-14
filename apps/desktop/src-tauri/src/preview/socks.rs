//! Preview tunnels, desktop side (#69). A remote environment's preview host routes through a
//! SOCKS5 server on `127.0.0.1` that belongs to that environment. A `CONNECT` to a loopback
//! destination becomes a tunnel that the renderer opens on the environment's own connection;
//! any other destination is connected from this computer, as an unproxied browser would.
//!
//! The renderer drives a tunnel: it answers the request (which writes the SOCKS reply), then
//! pulls what the page sent and writes what the environment sent, one chunk at a time, base64
//! as the tunnel RPC carries them.

use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::Serialize;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{
        TcpListener, TcpStream,
        tcp::{OwnedReadHalf, OwnedWriteHalf},
    },
    sync::Semaphore,
};
use tokio_util::sync::CancellationToken;

/// The renderer event that asks for a tunnel.
pub const REQUEST_EVENT: &str = "preview://tunnel-request";
/// Page connections one environment's SOCKS server serves at once.
pub const MAX_CONNECTIONS_PER_SERVER: usize = 64;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const ANSWER_TIMEOUT: Duration = Duration::from_secs(20);
const DIRECT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_CHUNK_BYTES: usize = 64 * 1024;
/// A page that takes no bytes for this long is stuck: its connection closes.
const WRITE_STALL_TIMEOUT: Duration = Duration::from_secs(60);
/// The largest decoded chunk one write may carry; the server reads 64 KiB at a time.
const MAX_WRITE_BYTES: usize = 256 * 1024;

const SOCKS_VERSION: u8 = 5;
const NO_AUTHENTICATION: u8 = 0;
const NO_ACCEPTABLE_METHOD: u8 = 0xff;
const CONNECT: u8 = 1;
const REPLY_SUCCEEDED: u8 = 0;
const REPLY_HOST_UNREACHABLE: u8 = 4;
const REPLY_CONNECTION_REFUSED: u8 = 5;
const REPLY_COMMAND_NOT_SUPPORTED: u8 = 7;
const REPLY_ADDRESS_NOT_SUPPORTED: u8 = 8;

/// A loopback connection a page opened, for the renderer to tunnel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelRequest {
    pub connection_id: String,
    pub environment_id: String,
    /// The loopback name or address the page asked for, as it asked.
    pub host: String,
    pub port: u16,
}

/// Every environment's SOCKS server and the page connections they hand to the renderer.
#[derive(Clone)]
pub struct PreviewTunnels {
    inner: Arc<Inner>,
}

struct Inner {
    emit: Box<dyn Fn(TunnelRequest) + Send + Sync>,
    servers: Mutex<HashMap<String, u16>>,
    connections: Mutex<HashMap<String, Arc<Connection>>>,
    /// Whether each environment's preview hosts route through SOCKS, from its first host.
    modes: Mutex<HashMap<String, bool>>,
}

/// How the renderer answered a tunnel request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    /// Through the tunnel the renderer opened.
    Opened,
    /// Connected on this computer: the environment's own endpoint here, such as an SSH
    /// environment's local forward, which the page reaches at the same loopback address.
    Direct,
    Refused,
}

impl Answer {
    pub fn parse(outcome: &str) -> Option<Self> {
        match outcome {
            "opened" => Some(Self::Opened),
            "direct" => Some(Self::Direct),
            "refused" => Some(Self::Refused),
            _ => None,
        }
    }
}

struct Connection {
    /// Where the page asked to connect, for a direct answer.
    host: String,
    port: u16,
    reader: tokio::sync::Mutex<OwnedReadHalf>,
    writer: tokio::sync::Mutex<OwnedWriteHalf>,
    answered: AtomicBool,
    closed: CancellationToken,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl PreviewTunnels {
    /// `emit` hands each tunnel request to the renderer.
    pub fn new(emit: impl Fn(TunnelRequest) + Send + Sync + 'static) -> Self {
        Self {
            inner: Arc::new(Inner {
                emit: Box::new(emit),
                servers: Mutex::default(),
                connections: Mutex::default(),
                modes: Mutex::default(),
            }),
        }
    }

    /// Whether `environment_id`'s preview hosts route through SOCKS. Its first host decides,
    /// for the app's lifetime: the WebKit profile keeps the proxy it was created with, and the
    /// renderer, which may have reloaded since, asks here instead of remembering.
    pub fn mode(&self, environment_id: &str, requested: bool) -> bool {
        *lock(&self.inner.modes)
            .entry(environment_id.to_owned())
            .or_insert(requested)
    }

    /// Closes every page connection: the renderer that drove them reloaded.
    pub async fn close_all(&self) {
        let ids: Vec<String> = lock(&self.inner.connections).keys().cloned().collect();
        for id in ids {
            self.close(&id).await;
        }
    }

    /// The port of `environment_id`'s SOCKS server, started on first use. It serves for the
    /// app's lifetime, like the preview host that points at it.
    pub async fn ensure_server(&self, environment_id: &str) -> std::io::Result<u16> {
        if let Some(port) = lock(&self.inner.servers).get(environment_id) {
            return Ok(*port);
        }
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let port = listener.local_addr()?.port();
        // Two first uses may race: the first listener recorded is the environment's.
        {
            let mut servers = lock(&self.inner.servers);
            if let Some(port) = servers.get(environment_id) {
                return Ok(*port);
            }
            servers.insert(environment_id.to_owned(), port);
        }
        let tunnels = self.clone();
        let environment_id = environment_id.to_owned();
        tokio::spawn(async move { tunnels.serve(listener, environment_id).await });
        Ok(port)
    }

    /// Answers a tunnel request: the SOCKS reply goes to the page before anything else does.
    pub async fn answer(&self, connection_id: &str, answer: Answer) {
        let Some(connection) = self.connection(connection_id) else {
            return;
        };
        if connection.answered.swap(true, Ordering::SeqCst) {
            return;
        }
        match answer {
            Answer::Opened => {
                if reply(&mut *connection.writer.lock().await, REPLY_SUCCEEDED)
                    .await
                    .is_err()
                {
                    self.close(connection_id).await;
                }
            }
            Answer::Refused => {
                let _ = reply(
                    &mut *connection.writer.lock().await,
                    REPLY_CONNECTION_REFUSED,
                )
                .await;
                self.close(connection_id).await;
            }
            Answer::Direct => {
                // No longer the renderer's: the relay below owns it until either side closes.
                lock(&self.inner.connections).remove(connection_id);
                tokio::spawn(relay_direct(connection));
            }
        }
    }

    /// The next bytes the page sent, base64; `None` once it closed its side, or the
    /// connection is gone.
    pub async fn read(&self, connection_id: &str) -> Option<String> {
        let connection = self.connection(connection_id)?;
        let mut reader = connection.reader.lock().await;
        let mut buffer = vec![0_u8; READ_CHUNK_BYTES];
        let read = tokio::select! {
            () = connection.closed.cancelled() => return None,
            read = reader.read(&mut buffer) => read,
        };
        drop(reader);
        match read {
            Ok(read @ 1..) => Some(STANDARD.encode(&buffer[..read])),
            // The page is done with it, whether or not the renderer closes it too.
            _ => {
                self.close(connection_id).await;
                None
            }
        }
    }

    /// Writes base64 `data` to the page; returns once it is written.
    pub async fn write(&self, connection_id: &str, data: &str) -> Result<(), String> {
        let connection = self
            .connection(connection_id)
            .ok_or_else(|| "the preview connection is closed".to_owned())?;
        if data.len() > MAX_WRITE_BYTES.div_ceil(3) * 4 {
            return Err("the preview chunk is too large".to_owned());
        }
        let bytes = STANDARD
            .decode(data)
            .map_err(|_| "the preview chunk is not base64".to_owned())?;
        let mut writer = connection.writer.lock().await;
        let written = tokio::select! {
            () = connection.closed.cancelled() => {
                return Err("the preview connection is closed".to_owned());
            }
            written = tokio::time::timeout(WRITE_STALL_TIMEOUT, writer.write_all(&bytes)) => written,
        };
        drop(writer);
        match written {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => {
                self.close(connection_id).await;
                Err(format!("the page's connection failed: {error}"))
            }
            Err(_) => {
                self.close(connection_id).await;
                Err("the page stopped reading".to_owned())
            }
        }
    }

    /// Closes the page's connection; closing a closed one is fine.
    pub async fn close(&self, connection_id: &str) {
        let removed = lock(&self.inner.connections).remove(connection_id);
        if let Some(connection) = removed {
            connection.closed.cancel();
            let _ = connection.writer.lock().await.shutdown().await;
        }
    }

    fn connection(&self, connection_id: &str) -> Option<Arc<Connection>> {
        lock(&self.inner.connections).get(connection_id).cloned()
    }

    async fn serve(self, listener: TcpListener, environment_id: String) {
        let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS_PER_SERVER));
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            // Over the limit, the page's connection waits for a slot instead of failing.
            let Ok(slot) = slots.clone().acquire_owned().await else {
                return;
            };
            let tunnels = self.clone();
            let environment_id = environment_id.clone();
            tokio::spawn(async move {
                let closed = tunnels.handle(stream, &environment_id).await;
                // The slot is the connection's until it closes.
                if let Some(closed) = closed {
                    closed.cancelled().await;
                }
                drop(slot);
            });
        }
    }

    /// Serves one page connection; for a tunnel, returns the token its close cancels.
    async fn handle(
        &self,
        mut stream: TcpStream,
        environment_id: &str,
    ) -> Option<CancellationToken> {
        let destination = tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake(&mut stream))
            .await
            .ok()?
            .ok()??;
        let (host, port) = destination;
        if !is_loopback(&host) {
            if let Err(error) = direct(stream, &host, port).await {
                tracing::debug!(%error, "direct preview connection ended");
            }
            return None;
        }
        let connection_id = uuid::Uuid::new_v4().to_string();
        let request_host = host.clone();
        let (reader, writer) = stream.into_split();
        let closed = CancellationToken::new();
        lock(&self.inner.connections).insert(
            connection_id.clone(),
            Arc::new(Connection {
                host,
                port,
                reader: tokio::sync::Mutex::new(reader),
                writer: tokio::sync::Mutex::new(writer),
                answered: AtomicBool::new(false),
                closed: closed.clone(),
            }),
        );
        (self.inner.emit)(TunnelRequest {
            connection_id: connection_id.clone(),
            environment_id: environment_id.to_owned(),
            host: request_host,
            port,
        });
        // A renderer that never answers (reloading, or not listening) refuses the page.
        let tunnels = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(ANSWER_TIMEOUT).await;
            tunnels.answer(&connection_id, Answer::Refused).await;
        });
        Some(closed)
    }
}

/// Reads the client greeting and the request, answering the greeting. `None` when the client
/// was answered with a failure (an unsupported method, command or address type).
async fn handshake(stream: &mut TcpStream) -> std::io::Result<Option<(String, u16)>> {
    let [version, method_count] = read_array(stream).await?;
    if version != SOCKS_VERSION {
        return Ok(None);
    }
    let mut methods = vec![0_u8; usize::from(method_count)];
    stream.read_exact(&mut methods).await?;
    if !methods.contains(&NO_AUTHENTICATION) {
        stream
            .write_all(&[SOCKS_VERSION, NO_ACCEPTABLE_METHOD])
            .await?;
        return Ok(None);
    }
    stream
        .write_all(&[SOCKS_VERSION, NO_AUTHENTICATION])
        .await?;
    let [version, command, _reserved, address_type] = read_array(stream).await?;
    if version != SOCKS_VERSION {
        return Ok(None);
    }
    let host = match address_type {
        1 => Ipv4Addr::from(read_array::<4>(stream).await?).to_string(),
        3 => {
            let [length] = read_array(stream).await?;
            let mut name = vec![0_u8; usize::from(length)];
            stream.read_exact(&mut name).await?;
            String::from_utf8(name).map_err(|_| std::io::Error::other("invalid host name"))?
        }
        4 => Ipv6Addr::from(read_array::<16>(stream).await?).to_string(),
        _ => {
            reply(stream, REPLY_ADDRESS_NOT_SUPPORTED).await?;
            return Ok(None);
        }
    };
    let port = u16::from_be_bytes(read_array(stream).await?);
    if command != CONNECT {
        reply(stream, REPLY_COMMAND_NOT_SUPPORTED).await?;
        return Ok(None);
    }
    Ok(Some((host, port)))
}

async fn read_array<const N: usize>(stream: &mut TcpStream) -> std::io::Result<[u8; N]> {
    let mut bytes = [0_u8; N];
    stream.read_exact(&mut bytes).await?;
    Ok(bytes)
}

/// A reply with an unspecified bound address, which browsers do not use.
async fn reply(stream: &mut (impl AsyncWriteExt + Unpin), code: u8) -> std::io::Result<()> {
    stream
        .write_all(&[SOCKS_VERSION, code, 0, 1, 0, 0, 0, 0, 0, 0])
        .await
}

/// `localhost`, `*.localhost`, `127.0.0.0/8` and `::1`: the environment's own loopback.
pub fn is_loopback(host: &str) -> bool {
    let host = host.trim_matches(['[', ']']).trim_end_matches('.');
    if host.eq_ignore_ascii_case("localhost") || host.to_ascii_lowercase().ends_with(".localhost") {
        return true;
    }
    host.parse::<IpAddr>()
        .is_ok_and(|ip| ip.to_canonical().is_loopback())
}

/// Connects a page connection answered `direct` to its destination on this computer.
async fn relay_direct(connection: Arc<Connection>) {
    let host = connection.host.trim_matches(['[', ']']).to_owned();
    let connected = tokio::time::timeout(
        DIRECT_CONNECT_TIMEOUT,
        TcpStream::connect((host.as_str(), connection.port)),
    )
    .await;
    let mut writer = connection.writer.lock().await;
    let Ok(Ok(upstream)) = connected else {
        let _ = reply(&mut *writer, REPLY_CONNECTION_REFUSED).await;
        connection.closed.cancel();
        return;
    };
    if reply(&mut *writer, REPLY_SUCCEEDED).await.is_ok() {
        let mut reader = connection.reader.lock().await;
        let (mut upstream_reader, mut upstream_writer) = upstream.into_split();
        let up = async {
            let _ = tokio::io::copy(&mut *reader, &mut upstream_writer).await;
            let _ = upstream_writer.shutdown().await;
        };
        let down = async {
            let _ = tokio::io::copy(&mut upstream_reader, &mut *writer).await;
            let _ = writer.shutdown().await;
        };
        tokio::join!(up, down);
    }
    connection.closed.cancel();
}

/// Connects any other destination from this computer and relays both ways.
async fn direct(mut stream: TcpStream, host: &str, port: u16) -> std::io::Result<()> {
    let connected =
        tokio::time::timeout(DIRECT_CONNECT_TIMEOUT, TcpStream::connect((host, port))).await;
    let mut upstream = match connected {
        Ok(Ok(upstream)) => upstream,
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
            return reply(&mut stream, REPLY_CONNECTION_REFUSED).await;
        }
        _ => return reply(&mut stream, REPLY_HOST_UNREACHABLE).await,
    };
    reply(&mut stream, REPLY_SUCCEEDED).await?;
    tokio::io::copy_bidirectional(&mut stream, &mut upstream).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    fn tunnels() -> (PreviewTunnels, mpsc::Receiver<TunnelRequest>) {
        let (sender, requests) = mpsc::channel();
        let tunnels = PreviewTunnels::new(move |request| {
            let _ = sender.send(request);
        });
        (tunnels, requests)
    }

    /// Greets with no authentication and asks to connect to `host:port` by name.
    async fn socks_connect(port: u16, host: &str, target_port: u16) -> TcpStream {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        stream.write_all(&[5, 1, 0]).await.unwrap();
        let mut greeting = [0_u8; 2];
        stream.read_exact(&mut greeting).await.unwrap();
        assert_eq!(greeting, [5, 0]);
        let mut request = vec![5, 1, 0, 3, u8::try_from(host.len()).unwrap()];
        request.extend_from_slice(host.as_bytes());
        request.extend_from_slice(&target_port.to_be_bytes());
        stream.write_all(&request).await.unwrap();
        stream
    }

    async fn reply_code(stream: &mut TcpStream) -> u8 {
        let mut reply = [0_u8; 10];
        stream.read_exact(&mut reply).await.unwrap();
        reply[1]
    }

    fn next_request(requests: &mpsc::Receiver<TunnelRequest>) -> TunnelRequest {
        requests.recv_timeout(Duration::from_secs(5)).unwrap()
    }

    #[test]
    fn loopback_names_and_addresses_go_through_the_tunnel() {
        for host in [
            "localhost",
            "LOCALHOST.",
            "app.localhost",
            "127.0.0.1",
            "127.9.9.9",
            "::1",
            "[::1]",
        ] {
            assert!(is_loopback(host), "{host}");
        }
        for host in ["example.com", "localhost.example.com", "10.0.0.1", "::"] {
            assert!(!is_loopback(host), "{host}");
        }
    }

    #[tokio::test]
    async fn a_loopback_connection_waits_for_the_renderer_then_carries_bytes_both_ways() {
        let (tunnels, requests) = tunnels();
        let port = tunnels.ensure_server("env-1").await.unwrap();
        assert_eq!(tunnels.ensure_server("env-1").await.unwrap(), port);
        let mut page = socks_connect(port, "localhost", 5173).await;
        let request = tokio::task::spawn_blocking(move || next_request(&requests))
            .await
            .unwrap();
        assert_eq!(
            (
                request.environment_id.as_str(),
                request.host.as_str(),
                request.port
            ),
            ("env-1", "localhost", 5173)
        );

        tunnels.answer(&request.connection_id, Answer::Opened).await;
        assert_eq!(reply_code(&mut page).await, REPLY_SUCCEEDED);

        page.write_all(b"GET / HTTP/1.1\r\n").await.unwrap();
        let sent = tunnels.read(&request.connection_id).await.unwrap();
        assert_eq!(STANDARD.decode(sent).unwrap(), b"GET / HTTP/1.1\r\n");

        tunnels
            .write(
                &request.connection_id,
                &STANDARD.encode("HTTP/1.1 200 OK\r\n"),
            )
            .await
            .unwrap();
        let mut answer = [0_u8; 17];
        page.read_exact(&mut answer).await.unwrap();
        assert_eq!(&answer, b"HTTP/1.1 200 OK\r\n");
        assert!(
            tunnels
                .write(&request.connection_id, "not base64!")
                .await
                .is_err()
        );

        drop(page);
        assert_eq!(
            tunnels.read(&request.connection_id).await,
            None,
            "the page closed"
        );
        assert!(
            tunnels.write(&request.connection_id, "aGk=").await.is_err(),
            "a closed page's connection is reclaimed without the renderer"
        );
    }

    #[test]
    fn an_environments_first_preview_host_decides_its_mode() {
        let (tunnels, _) = tunnels();
        assert!(tunnels.mode("env-1", true));
        assert!(
            tunnels.mode("env-1", false),
            "a reloaded renderer learns the real mode"
        );
        assert!(!tunnels.mode("env-2", false));
        assert!(!tunnels.mode("env-2", true));
    }

    #[tokio::test]
    async fn a_reloaded_renderer_leaves_no_connection_behind() {
        let (tunnels, requests) = tunnels();
        let port = tunnels.ensure_server("env-1").await.unwrap();
        let mut page = socks_connect(port, "localhost", 5173).await;
        let request = tokio::task::spawn_blocking(move || next_request(&requests))
            .await
            .unwrap();
        tunnels.answer(&request.connection_id, Answer::Opened).await;
        assert_eq!(reply_code(&mut page).await, REPLY_SUCCEEDED);
        tunnels.close_all().await;
        let mut rest = Vec::new();
        page.read_to_end(&mut rest).await.unwrap();
        assert!(rest.is_empty(), "the page sees its connection close");
        assert_eq!(tunnels.read(&request.connection_id).await, None);
    }

    #[tokio::test]
    async fn a_refused_tunnel_refuses_the_page() {
        let (tunnels, requests) = tunnels();
        let port = tunnels.ensure_server("env-1").await.unwrap();
        let mut page = socks_connect(port, "127.0.0.1", 5173).await;
        let request = tokio::task::spawn_blocking(move || next_request(&requests))
            .await
            .unwrap();
        tunnels
            .answer(&request.connection_id, Answer::Refused)
            .await;
        assert_eq!(reply_code(&mut page).await, REPLY_CONNECTION_REFUSED);
        assert_eq!(tunnels.read(&request.connection_id).await, None);
    }

    #[tokio::test]
    async fn a_direct_answer_connects_the_page_here() {
        let (tunnels, requests) = tunnels();
        let port = tunnels.ensure_server("env-1").await.unwrap();
        let local = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let local_port = local.local_addr().unwrap().port();
        let mut page = socks_connect(port, "127.0.0.1", local_port).await;
        let request = tokio::task::spawn_blocking(move || next_request(&requests))
            .await
            .unwrap();
        tunnels.answer(&request.connection_id, Answer::Direct).await;
        let (mut upstream, _) = local.accept().await.unwrap();
        assert_eq!(reply_code(&mut page).await, REPLY_SUCCEEDED);
        page.write_all(b"ping").await.unwrap();
        let mut received = [0_u8; 4];
        upstream.read_exact(&mut received).await.unwrap();
        assert_eq!(&received, b"ping");
        assert_eq!(
            tunnels.read(&request.connection_id).await,
            None,
            "a direct connection is not the renderer's"
        );
    }

    #[tokio::test]
    async fn other_destinations_connect_from_this_computer() {
        let (tunnels, requests) = tunnels();
        let port = tunnels.ensure_server("env-1").await.unwrap();
        // A non-loopback name the resolver here cannot find: answered without a tunnel.
        let mut page = socks_connect(port, "bibcode-preview-test.invalid", 80).await;
        assert_eq!(reply_code(&mut page).await, REPLY_HOST_UNREACHABLE);
        assert!(requests.try_recv().is_err(), "no tunnel was asked for");
    }
}
