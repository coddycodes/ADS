//! Gateway admission and the per-thread target registry behind `preview.gatewayOpen`.
//!
//! A target is one gateway listener for one `(thread, upstream port)`. Opening is idempotent:
//! a second open reuses the running listener and mints a fresh capability. Targets close
//! when every preview tab of the thread that has shown that port has closed, after 10
//! minutes with no connections, when the thread is deleted, or on server shutdown.

use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::{
        Arc, Weak,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use tokio::{
    net::{TcpListener, TcpStream},
    sync::{Mutex, Semaphore, broadcast::error::RecvError},
};
use tokio_util::sync::CancellationToken;
use url::{Host, Url};

use super::{
    GatewaySessions,
    capability::CapabilityIssuer,
    proxy::{
        GLOBAL_CONNECTIONS, GatewayContext, GatewayLimits, GatewayTarget, PER_TARGET_CONNECTIONS,
        PRINCIPAL_CHECK_INTERVAL, PrincipalCheck, RunningTarget, start_target,
    },
};
use crate::{
    preview::{InternalPreviewEvents, PreviewEvent, PreviewManager, PreviewNavStatus},
    signed_token::now_millis,
};

const UPSTREAM_PROBE_TIMEOUT: Duration = Duration::from_millis(500);
const IDLE_SWEEP_INTERVAL: Duration = Duration::from_secs(60);
const IDLE_TIMEOUT_MS: u64 = 10 * 60 * 1000;
/// Running targets across every thread; more opens answer `unavailable`.
pub const MAX_TARGETS: usize = 64;
/// How long shutdown lets in-flight exchanges finish before cutting them. The server has no
/// shutdown deadline of its own, and the desktop gives the whole backend 5 s to stop.
pub const SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatewayOpenResult {
    pub gateway_port: u16,
    pub capability: String,
    pub expires_at_ms: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("not admitted")]
    NotAdmitted,
    #[error("https unsupported")]
    HttpsUnsupported,
    #[error("no upstream")]
    NoUpstream,
    #[error("{0}")]
    NotReachable(Unreachable),
    #[error("unavailable: {0}")]
    Unavailable(String),
}

/// Why a caller's address cannot reach a gateway listener.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Unreachable {
    /// The caller reached the server on a public address; gateway traffic is plain HTTP.
    #[error("Previews aren't available on a public address.")]
    Public,
    /// The caller reached a loopback socket under a non-loopback name: a reverse proxy or
    /// Tailscale Serve, which would not forward the gateway's ports.
    #[error("Previews aren't available through a proxied address.")]
    Proxied,
}

/// How the caller's connection reached this server: the local address its socket was
/// accepted on, and the `Host` it asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientReach {
    pub local_ip: IpAddr,
    pub host: Option<String>,
}

/// The address a gateway listener for `reach` binds: the address the caller reached the
/// server on, so the client reaches the listener the same way. `None` (the connection's
/// local address is unknown, as in tests) binds `fallback`, the server's configured host,
/// but only when it is `localhost` or a private address: a wildcard, public, or named host
/// could expose plain-HTTP gateway traffic, and names are never resolved.
pub fn gateway_bind_host(
    reach: Option<&ClientReach>,
    fallback: &str,
) -> Result<String, GatewayError> {
    let Some(reach) = reach else {
        let private = fallback.eq_ignore_ascii_case("localhost")
            || fallback
                .parse::<IpAddr>()
                .is_ok_and(|ip| is_private_reach(ip.to_canonical()));
        return if private {
            Ok(fallback.to_owned())
        } else {
            Err(GatewayError::Unavailable(
                "the server can't tell which address this client reached".to_owned(),
            ))
        };
    };
    let ip = reach.local_ip.to_canonical();
    if !is_private_reach(ip) {
        return Err(GatewayError::NotReachable(Unreachable::Public));
    }
    if ip.is_loopback() && !reach.host.as_deref().is_some_and(is_loopback_authority) {
        return Err(GatewayError::NotReachable(Unreachable::Proxied));
    }
    Ok(ip.to_string())
}

/// Loopback, RFC 1918, CGNAT `100.64.0.0/10` (tailnets), link-local, and IPv6 ULA
/// `fc00::/7`; anything else counts as public.
fn is_private_reach(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, ..] = ip.octets();
            ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || (a == 100 && (64..128).contains(&b))
        }
        IpAddr::V6(ip) => {
            let first = ip.segments()[0];
            ip.is_loopback() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
    }
}

/// `localhost`, a `*.localhost` name (which browsers resolve to loopback themselves), a
/// loopback address, or the unspecified address (which browsers connect to loopback), with
/// or without a port. Names are never resolved.
fn is_loopback_authority(host: &str) -> bool {
    let Ok(authority) = host.parse::<hyper::http::uri::Authority>() else {
        return false;
    };
    let name = authority.host().to_ascii_lowercase();
    name == "localhost"
        || name.ends_with(".localhost")
        || name
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.to_canonical().is_loopback() || ip.is_unspecified())
}

/// The production gateway context: one capability issuer, one session table, and one
/// global connection semaphore shared by every target.
#[must_use]
pub fn gateway_context(
    principal: Arc<dyn PrincipalCheck>,
    secret: Vec<u8>,
    session_cookie_name: String,
) -> GatewayContext {
    GatewayContext {
        issuer: Arc::new(CapabilityIssuer::new(secret)),
        sessions: Arc::new(GatewaySessions::new()),
        principal,
        limits: GatewayLimits {
            per_target: PER_TARGET_CONNECTIONS,
            global: Arc::new(Semaphore::new(GLOBAL_CONNECTIONS)),
        },
        session_cookie_name,
        principal_check_interval: PRINCIPAL_CHECK_INTERVAL,
    }
}

/// `(thread, upstream port, bound address, upstream address)`: clients that reached the
/// server on different addresses each get their own listener, and `127.0.0.2:<port>` never
/// reuses the listener of `localhost:<port>` (`None` is `localhost`).
type TargetKey = (String, u16, String, Option<IpAddr>);

#[derive(Clone)]
pub struct PreviewGateway {
    inner: Arc<Inner>,
}

struct Inner {
    bind_host: String,
    environment_label: String,
    ctx: GatewayContext,
    preview: PreviewManager,
    targets: Mutex<HashMap<TargetKey, Target>>,
    /// Cancelled when shutdown starts: listeners stop accepting and idle connections close.
    draining: CancellationToken,
    /// Cancelled once draining ends: every connection and background task stops.
    shutdown: CancellationToken,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

struct Target {
    running: RunningTarget,
    /// Live preview tabs of the thread that have pointed at this port. A tab that navigated
    /// away keeps the target for its Back, so the target closes once all of them have closed.
    /// A target no tab ever pointed at (a browser-mode tab) is left to the idle sweep.
    owners: HashSet<String>,
}

impl Target {
    fn close(&self) {
        self.running.shutdown.cancel();
    }
}

impl PreviewGateway {
    /// Starts the idle sweeper and the preview-tab follower; both stop on [`Self::shutdown`].
    #[must_use]
    pub fn new(
        bind_host: impl Into<String>,
        environment_label: impl Into<String>,
        ctx: GatewayContext,
        preview: PreviewManager,
    ) -> Self {
        let gateway = Self {
            inner: Arc::new(Inner {
                bind_host: bind_host.into(),
                environment_label: environment_label.into(),
                ctx,
                preview: preview.clone(),
                targets: Mutex::new(HashMap::new()),
                draining: CancellationToken::new(),
                shutdown: CancellationToken::new(),
            }),
        };
        // The tasks hold the gateway weakly: dropping its last owner (a runtime that failed to
        // start) cancels them and every listener through `Inner`'s drop.
        let weak = Arc::downgrade(&gateway.inner);
        let shutdown = gateway.inner.shutdown.clone();
        tokio::spawn(Self::sweep_idle(weak.clone(), shutdown.clone()));
        // Subscribe before returning so no tab opened after construction is missed.
        let events = preview.subscribe_internal_events();
        tokio::spawn(Self::follow_tabs(weak, shutdown, events));
        gateway
    }

    /// Admits `url` for `thread_id` and mints a capability bound to the caller's session. The
    /// listener binds the address the caller reached the server on ([`gateway_bind_host`]).
    pub async fn open(
        &self,
        thread_id: &str,
        url: &str,
        session_id: &str,
        session_expires_at_ms: i64,
        reach: Option<&ClientReach>,
    ) -> Result<GatewayOpenResult, GatewayError> {
        let admitted = admit(url)?;
        let upstream_port = admitted.port;
        let bind_host = gateway_bind_host(reach, &self.inner.bind_host)?;
        let session_expiry = u64::try_from(session_expires_at_ms)
            .ok()
            .filter(|expiry| *expiry > now_millis())
            .ok_or_else(|| GatewayError::Unavailable("the session has expired".to_owned()))?;
        let key = (thread_id.to_owned(), upstream_port, bind_host, admitted.ip);
        // The probe runs under the lock so a concurrent `close_thread` or `shutdown` cannot
        // slip between it and the insert and leave a listener for a closed thread.
        // ponytail: one global lock; a loopback probe is refused at once, but a dropped SYN
        // stalls other opens for up to 1 s. Use per-thread fences if that shows up.
        let mut targets = self.inner.targets.lock().await;
        if self.inner.draining.is_cancelled() {
            return Err(GatewayError::Unavailable(
                "the preview gateway is shut down".to_owned(),
            ));
        }
        if !targets.contains_key(&key) {
            if targets.len() >= MAX_TARGETS {
                return Err(GatewayError::Unavailable(
                    "too many previews are open through the gateway".to_owned(),
                ));
            }
            let candidates = admitted.upstream_candidates();
            let upstream = probe_upstream(&candidates)
                .await
                .ok_or(GatewayError::NoUpstream)?;
            let target = GatewayTarget {
                thread_id: thread_id.to_owned(),
                upstream,
                // A `localhost` dev server that restarts on the other address family stays
                // reachable: the proxy tries this once when `upstream` refuses.
                fallback: candidates
                    .into_iter()
                    .find(|candidate| *candidate != upstream),
                upstream_port,
                environment_label: self.inner.environment_label.clone(),
            };
            let running = bind_unused_port(&key.2, |port| {
                targets
                    .values()
                    .any(|target| target.running.gateway_port == port)
            })
            .await
            .and_then(|listener| {
                start_target(
                    listener,
                    target,
                    self.inner.ctx.clone(),
                    self.inner.shutdown.child_token(),
                    self.inner.draining.child_token(),
                )
            })
            .map_err(|error| GatewayError::Unavailable(error.to_string()))?;
            targets.insert(
                key.clone(),
                Target {
                    running,
                    owners: HashSet::new(),
                },
            );
        }
        let tabs = tab_ports_of(&self.inner.preview, thread_id).await;
        let target = targets.get_mut(&key).expect("target was just ensured");
        target.owners.extend(
            tabs.into_iter()
                .filter(|(_, port)| *port == Some(upstream_port))
                .map(|(tab_id, _)| tab_id),
        );
        Ok(self.mint(&target.running, &key, session_id, session_expiry))
    }

    pub async fn close_thread(&self, thread_id: &str) {
        self.inner
            .targets
            .lock()
            .await
            .retain(|(thread, ..), target| {
                let keep = thread != thread_id;
                if !keep {
                    target.close();
                }
                keep
            });
    }

    pub async fn close_target(&self, thread_id: &str, upstream_port: u16) {
        self.inner
            .targets
            .lock()
            .await
            .retain(|(thread, port, ..), target| {
                let keep = thread != thread_id || *port != upstream_port;
                if !keep {
                    target.close();
                }
                keep
            });
    }

    /// Stops accepting, lets in-flight requests finish for up to [`SHUTDOWN_DRAIN_TIMEOUT`],
    /// then cuts whatever is left. Idle connections close at once, and WebSocket tunnels
    /// close when draining starts (they would otherwise always run out the deadline).
    pub async fn shutdown(&self) {
        let active: Vec<Arc<AtomicUsize>> = {
            let mut targets = self.inner.targets.lock().await;
            self.inner.draining.cancel();
            targets
                .drain()
                .map(|(_, target)| target.running.active)
                .collect()
        };
        let deadline = tokio::time::Instant::now() + SHUTDOWN_DRAIN_TIMEOUT;
        // ponytail: polls every 10 ms; a per-target notify if shutdown latency ever matters.
        while active.iter().any(|count| count.load(Ordering::Relaxed) > 0)
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        self.inner.shutdown.cancel();
    }

    /// A fresh capability for `target`. Minting counts as activity, so a capability is
    /// never handed out for a target the idle sweep is about to close.
    fn mint(
        &self,
        target: &RunningTarget,
        (thread_id, upstream_port, ..): &TargetKey,
        session_id: &str,
        session_expiry: u64,
    ) -> GatewayOpenResult {
        let now = now_millis();
        target.last_activity_ms.store(now, Ordering::Relaxed);
        let (capability, expires_at_ms) = self.inner.ctx.issuer.issue(
            target.gateway_port,
            *upstream_port,
            thread_id,
            session_id,
            now,
        );
        GatewayOpenResult {
            gateway_port: target.gateway_port,
            capability,
            expires_at_ms: expires_at_ms.min(session_expiry),
        }
    }

    /// Closes targets with no connections and no activity for [`IDLE_TIMEOUT_MS`].
    async fn close_idle(&self, now_ms: u64) {
        self.inner.targets.lock().await.retain(|_, target| {
            let running = &target.running;
            let idle = running.active.load(Ordering::Relaxed) == 0
                && now_ms.saturating_sub(running.last_activity_ms.load(Ordering::Relaxed))
                    >= IDLE_TIMEOUT_MS;
            if idle {
                target.close();
            }
            !idle
        });
    }

    async fn sweep_idle(gateway: Weak<Inner>, shutdown: CancellationToken) {
        let mut ticks = tokio::time::interval(IDLE_SWEEP_INTERVAL);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return,
                _ = ticks.tick() => {}
            }
            let Some(inner) = gateway.upgrade() else {
                return;
            };
            Self { inner }.close_idle(now_millis()).await;
        }
    }

    /// Reconciles the thread's targets with its live tabs: a live tab pointing at a target's
    /// port becomes one of its owners. With `close_unowned`, closed tabs stop owning it, and
    /// a target whose owners have all closed closes. Live tabs are read
    /// under the registry lock (the same order as `open`), and event snapshots are never
    /// trusted, so a stale or replayed event cannot mark or close a target against the
    /// current tab state. A thread without targets is skipped.
    async fn sync_thread(&self, thread_id: &str, close_unowned: bool) {
        let mut targets = self.inner.targets.lock().await;
        if !targets.keys().any(|(thread, ..)| thread == thread_id) {
            return;
        }
        let live = tab_ports_of(&self.inner.preview, thread_id).await;
        targets.retain(|(thread, port, ..), target| {
            if thread != thread_id {
                return true;
            }
            let tabbed = !target.owners.is_empty();
            // Owners are dropped only on a pass that may close: a pass that only marks
            // (a queued `resized` read after its tab closed) must leave the close to come.
            if close_unowned {
                target
                    .owners
                    .retain(|owner| live.iter().any(|(tab_id, _)| tab_id == owner));
            }
            target.owners.extend(
                live.iter()
                    .filter(|(_, tab_port)| *tab_port == Some(*port))
                    .map(|(tab_id, _)| tab_id.clone()),
            );
            let close = close_unowned && tabbed && target.owners.is_empty();
            if close {
                target.close();
            }
            !close
        });
    }

    /// Follows preview tabs. Tab events record which live tabs have shown each target, a
    /// closed tab closes its thread's targets whose last such tab it was, and missed events
    /// re-check every thread with a target. A tab that opened and closed before the follower
    /// saw it live (both events queued or missed) leaves its target unowned, so the idle
    /// sweep closes it instead.
    /// That is the price of never trusting event snapshots, which would let a stale event
    /// close a fresh tabless target.
    async fn follow_tabs(
        gateway: Weak<Inner>,
        shutdown: CancellationToken,
        mut events: InternalPreviewEvents,
    ) {
        loop {
            let event = tokio::select! {
                () = shutdown.cancelled() => return,
                event = events.receiver.recv() => event,
            };
            let Some(inner) = gateway.upgrade() else {
                return;
            };
            let gateway = Self { inner };
            match event {
                // Navigating away never closes the old target: the desktop reports no native
                // load failures, so Back to its gateway origin would fail silently. Tab close,
                // thread delete, and the idle sweep reap it.
                Ok(
                    PreviewEvent::Opened { thread_id, .. }
                    | PreviewEvent::Navigated { thread_id, .. }
                    | PreviewEvent::Resized { thread_id, .. }
                    | PreviewEvent::Failed { thread_id, .. },
                ) => gateway.sync_thread(&thread_id, false).await,
                Ok(PreviewEvent::Closed { thread_id, .. }) => {
                    gateway.sync_thread(&thread_id, true).await;
                }
                Ok(PreviewEvent::OpenRequested { .. }) => {}
                Err(RecvError::Lagged(_)) => {
                    let threads: HashSet<String> = gateway
                        .inner
                        .targets
                        .lock()
                        .await
                        .keys()
                        .map(|(thread, ..)| thread.clone())
                        .collect();
                    for thread_id in threads {
                        gateway.sync_thread(&thread_id, true).await;
                    }
                }
                Err(RecvError::Closed) => return,
            }
        }
    }
}

/// An admissible upstream: its port, and its loopback address (`None` for `localhost`).
#[derive(Debug, PartialEq, Eq)]
struct Admitted {
    port: u16,
    ip: Option<IpAddr>,
}

impl Admitted {
    /// Where to probe, in order: a literal address only itself, `localhost` IPv4 then IPv6.
    fn upstream_candidates(&self) -> Vec<SocketAddr> {
        let ips = match self.ip {
            Some(ip) => vec![ip],
            None => vec![
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                IpAddr::V6(Ipv6Addr::LOCALHOST),
            ],
        };
        ips.into_iter()
            .map(|ip| SocketAddr::new(ip, self.port))
            .collect()
    }
}

/// A plain-HTTP URL on `localhost`, `127.0.0.0/8`, or `::1`.
fn admit(url: &str) -> Result<Admitted, GatewayError> {
    let url = Url::parse(url).map_err(|_| GatewayError::NotAdmitted)?;
    let ip = match url.host() {
        Some(Host::Domain(domain)) if domain.eq_ignore_ascii_case("localhost") => None,
        Some(Host::Ipv4(ip)) if ip.is_loopback() => Some(IpAddr::V4(ip)),
        Some(Host::Ipv6(ip)) if ip.is_loopback() => Some(IpAddr::V6(ip)),
        _ => return Err(GatewayError::NotAdmitted),
    };
    match url.scheme() {
        "http" => {}
        "https" => return Err(GatewayError::HttpsUnsupported),
        _ => return Err(GatewayError::NotAdmitted),
    }
    let port = url
        .port_or_known_default()
        .filter(|port| *port != 0)
        .ok_or(GatewayError::NotAdmitted)?;
    Ok(Admitted { port, ip })
}

fn nav_port(status: &PreviewNavStatus) -> Option<u16> {
    match status {
        PreviewNavStatus::Idle => None,
        PreviewNavStatus::Loading { url, .. }
        | PreviewNavStatus::Success { url, .. }
        | PreviewNavStatus::LoadFailed { url, .. } => admit(url).ok().map(|admitted| admitted.port),
    }
}

/// The thread's live tabs and the admissible port each points at.
async fn tab_ports_of(preview: &PreviewManager, thread_id: &str) -> Vec<(String, Option<u16>)> {
    preview
        .list(thread_id)
        .await
        .sessions
        .into_iter()
        .map(|tab| {
            let port = nav_port(&tab.nav_status);
            (tab.tab_id, port)
        })
        .collect()
}

/// Binds `host:0` on a port no running target uses. Listeners on different addresses can
/// draw the same ephemeral port, but capabilities and gateway sessions name a listener by
/// its port, so closing one would end the other's sessions.
async fn bind_unused_port(host: &str, taken: impl Fn(u16) -> bool) -> std::io::Result<TcpListener> {
    for _ in 0..8 {
        let listener = TcpListener::bind((host, 0)).await?;
        if !taken(listener.local_addr()?.port()) {
            return Ok(listener);
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AddrInUse,
        "no unused gateway port",
    ))
}

/// The first of `candidates` that accepts a connection.
async fn probe_upstream(candidates: &[SocketAddr]) -> Option<SocketAddr> {
    for addr in candidates {
        if let Ok(Ok(_)) =
            tokio::time::timeout(UPSTREAM_PROBE_TIMEOUT, TcpStream::connect(addr)).await
        {
            return Some(*addr);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use futures_util::future::BoxFuture;

    use super::*;

    struct Live;

    impl PrincipalCheck for Live {
        fn session_expiry<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Option<i64>> {
            Box::pin(async { Some(i64::MAX) })
        }
    }

    #[test]
    fn admits_only_plain_http_loopback_urls() {
        let admitted = |port, ip: Option<&str>| Admitted {
            port,
            ip: ip.map(|ip| ip.parse().unwrap()),
        };
        assert_eq!(admit("http://localhost/").unwrap(), admitted(80, None));
        assert_eq!(
            admit("http://LOCALHOST:5173/x").unwrap(),
            admitted(5173, None)
        );
        assert_eq!(
            admit("http://127.1.2.3:8080").unwrap(),
            admitted(8080, Some("127.1.2.3"))
        );
        assert_eq!(
            admit("http://[::1]:3000/").unwrap(),
            admitted(3000, Some("::1"))
        );
        assert!(matches!(
            admit("https://localhost:5173/"),
            Err(GatewayError::HttpsUnsupported)
        ));
        assert!(matches!(
            admit("https://example.com/"),
            Err(GatewayError::NotAdmitted)
        ));
    }

    #[tokio::test]
    async fn gateway_ports_are_unique_across_bound_addresses() {
        let first = std::sync::Mutex::new(None);
        let listener = bind_unused_port("127.0.0.1", |port| {
            first.lock().unwrap().get_or_insert(port) == &port
        })
        .await
        .unwrap();
        let first = first.lock().unwrap().unwrap();
        assert_ne!(listener.local_addr().unwrap().port(), first);
        assert!(
            bind_unused_port("127.0.0.1", |_| true).await.is_err(),
            "gives up instead of spinning"
        );
    }

    #[test]
    fn a_literal_upstream_is_probed_only_at_itself() {
        let addrs = |url: &str| {
            admit(url)
                .unwrap()
                .upstream_candidates()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        };
        assert_eq!(addrs("http://127.0.0.2:5173/"), ["127.0.0.2:5173"]);
        assert_eq!(addrs("http://[::1]:5173/"), ["[::1]:5173"]);
        assert_eq!(
            addrs("http://localhost:5173/"),
            ["127.0.0.1:5173", "[::1]:5173"]
        );
    }

    fn reach(local_ip: &str, host: Option<&str>) -> ClientReach {
        ClientReach {
            local_ip: local_ip.parse().unwrap(),
            host: host.map(str::to_owned),
        }
    }

    #[test]
    fn binds_the_private_address_the_caller_reached() {
        for (local, host, bound) in [
            ("192.168.1.5", "box.lan:3773", "192.168.1.5"),
            ("10.0.0.2", "10.0.0.2:3773", "10.0.0.2"),
            (
                "100.101.102.103",
                "box.tailnet.ts.net:3773",
                "100.101.102.103",
            ),
            ("169.254.1.1", "169.254.1.1:3773", "169.254.1.1"),
            ("fd00::1", "[fd00::1]:3773", "fd00::1"),
            ("fe80::1", "box.local:3773", "fe80::1"),
            ("::ffff:192.168.1.5", "box.lan:3773", "192.168.1.5"),
            // SSH tunnels and WSL arrive on loopback with a loopback `Host`.
            ("127.0.0.1", "127.0.0.1:45123", "127.0.0.1"),
            ("127.0.0.1", "localhost:3773", "127.0.0.1"),
            ("127.0.0.1", "0.0.0.0:3773", "127.0.0.1"),
            ("127.0.0.1", "app.localhost:3773", "127.0.0.1"),
            ("127.0.0.1", "App.Localhost", "127.0.0.1"),
            ("::1", "[::1]:3773", "::1"),
        ] {
            assert_eq!(
                gateway_bind_host(Some(&reach(local, Some(host))), "0.0.0.0").unwrap(),
                bound,
                "{local} {host}"
            );
        }
    }

    #[test]
    fn an_unknown_local_address_binds_only_a_private_configured_host() {
        for host in [
            "127.0.0.1",
            "::1",
            "localhost",
            "192.168.1.5",
            "100.101.102.103",
        ] {
            assert_eq!(gateway_bind_host(None, host).unwrap(), host);
        }
        // A wildcard or public bind would expose plain-HTTP gateway traffic on every
        // interface, and a host name is never resolved.
        for host in ["0.0.0.0", "::", "203.0.113.7", "box.example.com"] {
            assert!(
                matches!(
                    gateway_bind_host(None, host),
                    Err(GatewayError::Unavailable(_))
                ),
                "{host}"
            );
        }
    }

    #[test]
    fn refuses_public_and_proxied_reach() {
        for local in ["203.0.113.7", "8.8.8.8", "100.128.0.1", "2001:db8::1"] {
            assert!(
                matches!(
                    gateway_bind_host(Some(&reach(local, Some("example.com"))), "0.0.0.0"),
                    Err(GatewayError::NotReachable(Unreachable::Public))
                ),
                "{local}"
            );
        }
        // A reverse proxy or Tailscale Serve reaches a loopback socket under its own name.
        for host in [
            Some("box.example.com"),
            Some("box.tail1234.ts.net:443"),
            None,
        ] {
            assert!(
                matches!(
                    gateway_bind_host(Some(&reach("127.0.0.1", host)), "127.0.0.1"),
                    Err(GatewayError::NotReachable(Unreachable::Proxied))
                ),
                "{host:?}"
            );
        }
        assert_eq!(
            GatewayError::NotReachable(Unreachable::Public).to_string(),
            "Previews aren't available on a public address."
        );
        assert_eq!(
            GatewayError::NotReachable(Unreachable::Proxied).to_string(),
            "Previews aren't available through a proxied address."
        );
    }

    #[tokio::test]
    async fn idle_sweep_closes_only_targets_idle_for_ten_minutes() {
        let upstream = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://localhost:{}/",
            upstream.local_addr().unwrap().port()
        );
        let gateway = PreviewGateway::new(
            "127.0.0.1",
            "devbox",
            gateway_context(Arc::new(Live), vec![1; 32], "session".into()),
            PreviewManager::new(),
        );
        gateway.open("t", &url, "s", i64::MAX, None).await.unwrap();
        let now = now_millis();
        gateway.close_idle(now + IDLE_TIMEOUT_MS - 1_000).await;
        assert_eq!(gateway.inner.targets.lock().await.len(), 1);
        gateway.close_idle(now + IDLE_TIMEOUT_MS + 1_000).await;
        assert!(gateway.inner.targets.lock().await.is_empty());
        gateway.shutdown().await;
    }

    #[tokio::test]
    async fn a_marking_pass_after_the_tab_closed_leaves_the_close_to_come() {
        let upstream = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://localhost:{}/",
            upstream.local_addr().unwrap().port()
        );
        let preview = PreviewManager::new();
        let gateway = PreviewGateway::new(
            "127.0.0.1",
            "devbox",
            gateway_context(Arc::new(Live), vec![1; 32], "session".into()),
            preview.clone(),
        );
        let tab = preview.open("t", Some(&url)).await.unwrap();
        gateway.open("t", &url, "s", i64::MAX, None).await.unwrap();

        // A queued `resized` handled after its tab closed, then the `closed` event.
        preview.close("t", Some(&tab.tab_id)).await.unwrap();
        gateway.sync_thread("t", false).await;
        gateway.sync_thread("t", true).await;

        assert!(gateway.inner.targets.lock().await.is_empty());
        gateway.shutdown().await;
    }

    #[tokio::test]
    async fn gateway_sessions_outlive_the_drain_and_end_with_shutdown() {
        let upstream = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://localhost:{}/",
            upstream.local_addr().unwrap().port()
        );
        let gateway = PreviewGateway::new(
            "127.0.0.1",
            "devbox",
            gateway_context(Arc::new(Live), vec![1; 32], "session".into()),
            PreviewManager::new(),
        );
        let port = gateway
            .open("t", &url, "s", i64::MAX, None)
            .await
            .unwrap()
            .gateway_port;
        let sessions = gateway.inner.ctx.sessions.clone();
        let id = sessions.create(
            &super::super::capability::GatewayCapabilityClaims {
                gateway_port: port,
                upstream_port: 1,
                thread_id: "t".into(),
                session_id: "s".into(),
                expires_at: 0,
                jti: "j".into(),
            },
            u64::MAX,
            None,
        );

        gateway.inner.draining.cancel();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            sessions.get(&id, now_millis()).is_some(),
            "an exchange still draining keeps its session"
        );

        gateway.inner.shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(5), async {
            while sessions.get(&id, now_millis()).is_some() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("shutdown drops the listener's sessions");
    }

    #[tokio::test]
    async fn dropping_the_last_handle_closes_listeners() {
        let upstream = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://localhost:{}/",
            upstream.local_addr().unwrap().port()
        );
        let gateway = PreviewGateway::new(
            "127.0.0.1",
            "devbox",
            gateway_context(Arc::new(Live), vec![1; 32], "session".into()),
            PreviewManager::new(),
        );
        let port = gateway
            .open("t", &url, "s", i64::MAX, None)
            .await
            .unwrap()
            .gateway_port;
        let inner = Arc::downgrade(&gateway.inner);
        drop(gateway);
        assert!(inner.upgrade().is_none(), "background tasks hold no handle");
        tokio::time::timeout(Duration::from_secs(5), async {
            while TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the listener stops");
    }
}
