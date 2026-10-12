//! BiBCode Connect (relay) reach for the preview gateway. The Connect tunnel runtime
//! publishes the managed endpoint's hostname and its fixed preview hostnames
//! ([`RelayHosts`]). Gateway admission recognizes a relay caller by that hostname
//! ([`is_relay_reach`]) and gives each relay target one preview hostname
//! ([`RelaySlots`]); the main port serves those hostnames through
//! `PreviewGateway::relay_slot` and `PreviewGateway::serve_relay`.

use std::{
    net::IpAddr,
    sync::{Arc, Mutex, PoisonError, RwLock},
};

use super::proxy::TargetRoute;

/// Preview hostnames per environment; published names beyond this are ignored.
pub const RELAY_PREVIEW_SLOTS: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayPreviewHosts {
    /// The managed endpoint's hostname; relay RPC connections arrive under it.
    pub hostname: String,
    /// At most [`RELAY_PREVIEW_SLOTS`] preview hostnames, in slot order.
    pub previews: Vec<String>,
}

/// The current [`RelayPreviewHosts`], written by the Connect tunnel runtime and read by
/// the gateway. `None` while no managed tunnel runs.
#[derive(Clone, Default)]
pub struct RelayHosts(Arc<RwLock<Option<RelayPreviewHosts>>>);

impl RelayHosts {
    pub fn publish(&self, hosts: Option<RelayPreviewHosts>) {
        *self.0.write().unwrap_or_else(PoisonError::into_inner) = hosts;
    }

    #[must_use]
    pub fn current(&self) -> Option<RelayPreviewHosts> {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The slot whose preview hostname `host` (a `Host` header value) names.
    pub(crate) fn preview_slot(&self, host: &str) -> Option<usize> {
        let name = host_name(host)?;
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()?
            .previews
            .iter()
            .position(|preview| *preview == name)
    }
}

/// The lowercase name in a `Host` value, without port or trailing dot. Addresses and
/// malformed values never equal a published hostname.
fn host_name(host: &str) -> Option<String> {
    let authority = host.parse::<hyper::http::uri::Authority>().ok()?;
    Some(authority.host().trim_end_matches('.').to_ascii_lowercase())
}

/// A caller that came through the Connect tunnel: its connector runs on this machine, so
/// the socket is loopback, and the request names the managed endpoint's hostname.
pub(crate) fn is_relay_reach(
    hosts: &RelayPreviewHosts,
    local_ip: IpAddr,
    host: Option<&str>,
) -> bool {
    local_ip.to_canonical().is_loopback()
        && host
            .and_then(host_name)
            .is_some_and(|name| name == hosts.hostname)
}

/// The managed endpoint's hostname or one of its preview hostnames (a `Host` value).
pub(crate) fn is_relay_host(hosts: &RelayPreviewHosts, host: &str) -> bool {
    host_name(host).is_some_and(|name| name == hosts.hostname || hosts.previews.contains(&name))
}

/// The target each preview hostname serves; a slot whose target closed is free.
#[derive(Default)]
pub(crate) struct RelaySlots(Mutex<[Option<TargetRoute>; RELAY_PREVIEW_SLOTS]>);

impl RelaySlots {
    /// The slot for `route` among the first `available`: the one it holds, else one whose
    /// target has closed (or that never had one). `None` when every slot serves a live
    /// target. A live target never loses its slot: a page left open on that hostname
    /// carries the newer target's cookie too, so its requests would reach another app.
    pub(crate) fn assign(&self, route: &TargetRoute, available: usize) -> Option<usize> {
        let mut guard = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let slots = &mut guard[..available.min(RELAY_PREVIEW_SLOTS)];
        let index = slots
            .iter()
            .position(|slot| slot.as_ref().is_some_and(|held| held.same(route)))
            .or_else(|| {
                slots
                    .iter()
                    .position(|slot| slot.as_ref().is_none_or(TargetRoute::is_closed))
            })?;
        slots[index] = Some(route.clone());
        Some(index)
    }

    /// The live target in slot `index`.
    pub(crate) fn route(&self, index: usize) -> Option<TargetRoute> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(index)?
            .clone()
            .filter(|route| !route.is_closed())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures_util::future::BoxFuture;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::preview::gateway::{
        proxy::{GatewayContext, GatewayTarget, PrincipalCheck, RunningTarget, start_target},
        registry::gateway_context,
    };

    struct Live;

    impl PrincipalCheck for Live {
        fn session_expiry<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Option<i64>> {
            Box::pin(async { Some(i64::MAX) })
        }
    }

    fn hosts() -> RelayPreviewHosts {
        RelayPreviewHosts {
            hostname: "env.zone.test".into(),
            previews: (1..=RELAY_PREVIEW_SLOTS)
                .map(|slot| format!("env-p{slot}.zone.test"))
                .collect(),
        }
    }

    #[test]
    fn host_name_is_the_lowercase_name_without_port_or_trailing_dot() {
        assert_eq!(
            host_name("ENV-P1.Zone.Test:443").as_deref(),
            Some("env-p1.zone.test")
        );
        assert_eq!(
            host_name("env-p1.zone.test.").as_deref(),
            Some("env-p1.zone.test")
        );
        // An address keeps its brackets, so it never equals a published hostname.
        assert_eq!(host_name("[::1]:443").as_deref(), Some("[::1]"));
        assert_eq!(host_name("not a host"), None);
        assert_eq!(host_name(""), None);
    }

    #[test]
    fn preview_slot_names_the_slot_of_a_published_preview_hostname() {
        let relay = RelayHosts::default();
        assert_eq!(
            relay.preview_slot("env-p1.zone.test"),
            None,
            "nothing published"
        );

        relay.publish(Some(hosts()));
        assert_eq!(relay.current(), Some(hosts()));
        for slot in 0..RELAY_PREVIEW_SLOTS {
            let host = format!("ENV-P{}.zone.test:443", slot + 1);
            assert_eq!(relay.preview_slot(&host), Some(slot), "{host}");
        }
        assert_eq!(relay.preview_slot("env.zone.test"), None, "the endpoint");
        assert_eq!(
            relay.preview_slot("other-p1.zone.test"),
            None,
            "another environment"
        );
        assert_eq!(relay.preview_slot("[::1]:443"), None);
        assert_eq!(relay.preview_slot("not a host"), None);

        relay.publish(None);
        assert_eq!(relay.current(), None);
        assert_eq!(relay.preview_slot("env-p1.zone.test"), None, "withdrawn");
    }

    #[test]
    fn relay_reach_is_loopback_plus_the_endpoint_hostname() {
        let hosts = hosts();
        let reach =
            |ip: &str, host: Option<&str>| is_relay_reach(&hosts, ip.parse().unwrap(), host);
        for ip in ["127.0.0.1", "::1", "::ffff:127.0.0.1"] {
            assert!(reach(ip, Some("env.zone.test")), "{ip}");
            assert!(
                reach(ip, Some("ENV.zone.test:443")),
                "{ip} with port and case"
            );
        }
        assert!(!reach("192.168.1.5", Some("env.zone.test")), "LAN address");
        assert!(
            !reach("::ffff:192.168.1.5", Some("env.zone.test")),
            "mapped LAN address"
        );
        assert!(
            !reach("127.0.0.1", Some("env-p1.zone.test")),
            "a preview hostname"
        );
        assert!(!reach("127.0.0.1", Some("localhost:3000")));
        assert!(!reach("127.0.0.1", None));
    }

    fn context() -> GatewayContext {
        gateway_context(Arc::new(Live), vec![1; 32], "session".into())
    }

    async fn running_target_draining(
        ctx: &GatewayContext,
        draining: CancellationToken,
    ) -> RunningTarget {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        start_target(
            listener,
            GatewayTarget {
                thread_id: "t".into(),
                upstream: "127.0.0.1:9".parse().unwrap(),
                fallback: None,
                upstream_port: 9,
                environment_label: "devbox".into(),
            },
            ctx.clone(),
            CancellationToken::new(),
            draining,
        )
        .unwrap()
    }

    async fn running_target(ctx: &GatewayContext) -> RunningTarget {
        running_target_draining(ctx, CancellationToken::new()).await
    }

    #[tokio::test]
    async fn a_target_keeps_its_slot_and_the_next_one_takes_the_next() {
        let ctx = context();
        let (first, second) = (running_target(&ctx).await, running_target(&ctx).await);
        let slots = RelaySlots::default();
        assert_eq!(slots.assign(&first.route, 4), Some(0));
        assert_eq!(slots.assign(&second.route, 4), Some(1));
        assert_eq!(slots.assign(&first.route, 4), Some(0));
        assert_eq!(slots.assign(&second.route, 4), Some(1));
        assert!(slots.route(0).is_some_and(|route| route.same(&first.route)));
        assert!(
            slots
                .route(1)
                .is_some_and(|route| route.same(&second.route))
        );
        assert!(slots.route(2).is_none());
        assert!(slots.route(RELAY_PREVIEW_SLOTS).is_none());
    }

    #[tokio::test]
    async fn a_live_target_never_loses_its_slot() {
        let ctx = context();
        let (first, second, third) = (
            running_target(&ctx).await,
            running_target(&ctx).await,
            running_target(&ctx).await,
        );
        let slots = RelaySlots::default();
        assert_eq!(slots.assign(&first.route, 2), Some(0));
        assert_eq!(slots.assign(&second.route, 2), Some(1));
        // Both targets are idle (no connections), and still keep their slots.
        assert_eq!(slots.assign(&third.route, 2), None, "every slot is live");
        assert_eq!(
            slots.assign(&third.route, 1),
            None,
            "only the available slots count"
        );

        second.shutdown.cancel();
        assert_eq!(
            slots.assign(&third.route, 2),
            Some(1),
            "a closed target yields"
        );
        assert!(slots.route(0).is_some_and(|route| route.same(&first.route)));
    }

    #[tokio::test]
    async fn a_closed_or_draining_target_serves_nothing() {
        let ctx = context();
        let draining = CancellationToken::new();
        let first = running_target(&ctx).await;
        let second = running_target_draining(&ctx, draining.clone()).await;
        let (third, fourth) = (running_target(&ctx).await, running_target(&ctx).await);
        let slots = RelaySlots::default();
        assert_eq!(slots.assign(&first.route, 2), Some(0));
        assert_eq!(slots.assign(&second.route, 2), Some(1));

        first.shutdown.cancel();
        assert!(slots.route(0).is_none(), "a closed target serves nothing");
        assert!(slots.route(1).is_some());
        assert_eq!(slots.assign(&third.route, 2), Some(0));

        draining.cancel();
        assert!(slots.route(1).is_none(), "a draining target serves nothing");
        assert_eq!(slots.assign(&fourth.route, 2), Some(1));
    }

    /// Requests that reach the main port under a preview hostname, end to end: a real
    /// upstream, a front server that serves preview slots as the main port does.
    mod routed {
        use std::{
            convert::Infallible,
            net::{Ipv4Addr, SocketAddr},
        };

        use axum::{
            Router,
            extract::ws::{Message as UpstreamMessage, WebSocketUpgrade},
            http::{HeaderMap as UpstreamHeaders, StatusCode as UpstreamStatus},
            response::{IntoResponse, Redirect},
            routing::{get, post},
        };
        use futures_util::{SinkExt, StreamExt};
        use http_body_util::{BodyExt, Empty};
        use hyper::{
            Request, Response,
            body::{Bytes, Incoming},
        };
        use hyper_util::rt::TokioIo;
        use reqwest::{Client, StatusCode, header};
        use tokio_tungstenite::{
            connect_async,
            tungstenite::{Message, client::IntoClientRequest},
        };

        use super::*;
        use crate::preview::{
            PreviewManager,
            gateway::registry::{ClientReach, GatewayOpenResult, PreviewGateway},
        };

        /// Redirects `/` to an absolute `localhost` URL, answers `POST /echo`, and echoes
        /// WebSocket text only when `Origin` is `http://` plus `Host`, as the brainstorming
        /// companion checks.
        async fn upstream() -> u16 {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let app = Router::new()
                .route(
                    "/",
                    get(move || async move {
                        Redirect::temporary(&format!("http://localhost:{port}/next"))
                    }),
                )
                .route("/echo", post(|| async { "posted" }))
                .route(
                    "/main.js",
                    get(|| async {
                        (
                            [(
                                axum::http::header::CACHE_CONTROL,
                                "public, max-age=3600, immutable",
                            )],
                            "console.log(1)",
                        )
                    }),
                )
                .route(
                    "/private",
                    get(|| async { ([(axum::http::header::CACHE_CONTROL, "no-store")], "secret") }),
                )
                .route(
                    "/stream",
                    get(|| async {
                        // Never ends on its own, like server-sent events.
                        axum::body::Body::from_stream(futures_util::stream::unfold(
                            (),
                            |()| async {
                                tokio::time::sleep(Duration::from_millis(50)).await;
                                Some((Ok::<_, Infallible>(Bytes::from_static(b"tick\n")), ()))
                            },
                        ))
                    }),
                )
                .route(
                    "/ws",
                    get(
                        |headers: UpstreamHeaders, ws: WebSocketUpgrade| async move {
                            let header = |name: &str| {
                                headers
                                    .get(name)
                                    .and_then(|value| value.to_str().ok())
                                    .unwrap_or_default()
                                    .to_owned()
                            };
                            if header("origin") != format!("http://{}", header("host")) {
                                return UpstreamStatus::FORBIDDEN.into_response();
                            }
                            ws.on_upgrade(|mut socket| async move {
                                while let Some(Ok(UpstreamMessage::Text(text))) =
                                    socket.recv().await
                                {
                                    if socket.send(UpstreamMessage::Text(text)).await.is_err() {
                                        break;
                                    }
                                }
                            })
                        },
                    ),
                );
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            port
        }

        /// Serves preview slots on loopback as the main port does, answering 404 for any
        /// other request.
        async fn front(gateway: PreviewGateway) -> SocketAddr {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        return;
                    };
                    let gateway = gateway.clone();
                    tokio::spawn(async move {
                        let service =
                            hyper::service::service_fn(move |request: Request<Incoming>| {
                                let gateway = gateway.clone();
                                async move {
                                    let host = request
                                        .headers()
                                        .get("host")
                                        .and_then(|value| value.to_str().ok());
                                    let slot =
                                        gateway.relay_slot(Some(Ipv4Addr::LOCALHOST.into()), host);
                                    Ok::<_, Infallible>(match slot {
                                        Some(slot) => gateway.serve_relay(slot, request).await,
                                        None => Response::builder()
                                            .status(404)
                                            .body(
                                                Empty::<Bytes>::new()
                                                    .map_err(|never| match never {})
                                                    .boxed(),
                                            )
                                            .unwrap(),
                                    })
                                }
                            });
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(TokioIo::new(stream), service)
                            .with_upgrades()
                            .await;
                    });
                }
            });
            addr
        }

        fn gateway(previews: usize) -> PreviewGateway {
            let relay = RelayHosts::default();
            relay.publish(Some(RelayPreviewHosts {
                hostname: "env.zone.test".into(),
                previews: (1..=previews)
                    .map(|slot| format!("env-p{slot}.zone.test"))
                    .collect(),
            }));
            PreviewGateway::new(
                "127.0.0.1",
                "devbox",
                context(),
                PreviewManager::new(),
                relay,
            )
        }

        async fn open(gateway: &PreviewGateway, upstream_port: u16) -> GatewayOpenResult {
            let reach = ClientReach {
                local_ip: Ipv4Addr::LOCALHOST.into(),
                host: Some("env.zone.test".into()),
            };
            gateway
                .open(
                    "t",
                    &format!("http://localhost:{upstream_port}/"),
                    "s",
                    i64::MAX,
                    Some(&reach),
                )
                .await
                .unwrap()
        }

        fn http() -> Client {
            Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap()
        }

        /// Bootstraps `capability` under `host` and returns the `Set-Cookie` header.
        async fn bootstrap(front: SocketAddr, host: &str, capability: &str) -> String {
            let response = http()
                .get(format!(
                    "http://{front}/__bibcode/bootstrap?cap={capability}&to=%2F"
                ))
                .header(header::HOST, host)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            response.headers()[header::SET_COOKIE]
                .to_str()
                .unwrap()
                .to_owned()
        }

        fn pair(set_cookie: &str) -> String {
            set_cookie.split(';').next().unwrap().to_owned()
        }

        #[tokio::test]
        async fn a_preview_hostname_bootstraps_an_https_facing_session() {
            let upstream_port = upstream().await;
            let gateway = gateway(4);
            let front = front(gateway.clone()).await;
            let opened = open(&gateway, upstream_port).await;
            assert_eq!(opened.origin.as_deref(), Some("https://env-p1.zone.test"));

            let set_cookie = bootstrap(front, "env-p1.zone.test", &opened.capability).await;
            assert!(set_cookie.contains("HttpOnly"), "{set_cookie}");
            assert!(set_cookie.contains("; Secure;"), "{set_cookie}");
            assert!(set_cookie.contains("SameSite=Lax"), "{set_cookie}");
            let cookie = pair(&set_cookie);

            let page = http()
                .get(format!("http://{front}/"))
                .header(header::HOST, "env-p1.zone.test")
                .header(header::COOKIE, &cookie)
                .send()
                .await
                .unwrap();
            assert_eq!(
                page.headers()[header::LOCATION],
                "https://env-p1.zone.test/next",
                "an upstream redirect to localhost lands on the https origin"
            );
            assert_eq!(
                page.headers()["cloudflare-cdn-cache-control"],
                "no-store",
                "an authenticated upstream response never enters the edge cache"
            );

            let post = |origin: &'static str| {
                http()
                    .post(format!("http://{front}/echo"))
                    .header(header::HOST, "env-p1.zone.test")
                    .header(header::COOKIE, &cookie)
                    .header(header::ORIGIN, origin)
                    .send()
            };
            assert_eq!(
                post("http://env-p1.zone.test").await.unwrap().status(),
                StatusCode::FORBIDDEN,
                "the client-facing origin is https"
            );
            let posted = post("https://env-p1.zone.test").await.unwrap();
            assert_eq!(posted.status(), StatusCode::OK);
            assert_eq!(posted.text().await.unwrap(), "posted");

            let get = |path: &'static str| {
                http()
                    .get(format!("http://{front}{path}"))
                    .header(header::HOST, "env-p1.zone.test")
                    .header(header::COOKIE, &cookie)
                    .send()
            };
            assert_eq!(
                get("/main.js").await.unwrap().headers()[header::CACHE_CONTROL],
                "no-store",
                "nothing one target served stays cached for the next one on this hostname"
            );
            assert_eq!(
                get("/private").await.unwrap().headers()[header::CACHE_CONTROL],
                "no-store"
            );

            // A browser fetching a service worker script sends `Service-Worker: script`.
            let worker = http()
                .get(format!("http://{front}/sw.js"))
                .header(header::HOST, "env-p1.zone.test")
                .header(header::COOKIE, &cookie)
                .header("service-worker", "script")
                .send()
                .await
                .unwrap();
            assert_eq!(
                worker.status(),
                StatusCode::FORBIDDEN,
                "no service worker outlives its target on a reusable hostname"
            );
            gateway.shutdown().await;
        }

        #[tokio::test]
        async fn a_websocket_upgrade_passes_through_the_router() {
            let upstream_port = upstream().await;
            let gateway = gateway(4);
            let front = front(gateway.clone()).await;
            let opened = open(&gateway, upstream_port).await;
            let cookie = pair(&bootstrap(front, "env-p1.zone.test", &opened.capability).await);

            let mut request = format!("ws://{front}/ws").into_client_request().unwrap();
            let headers = request.headers_mut();
            headers.insert("host", "env-p1.zone.test".parse().unwrap());
            headers.insert("origin", "https://env-p1.zone.test".parse().unwrap());
            headers.insert("cookie", cookie.parse().unwrap());
            let (mut socket, _) = connect_async(request).await.unwrap();
            socket.send(Message::text("ping")).await.unwrap();
            let echoed = tokio::time::timeout(Duration::from_secs(5), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(echoed.into_text().unwrap(), "ping");
            gateway.shutdown().await;
        }

        #[tokio::test]
        async fn gateway_shutdown_ends_a_routed_exchange_that_never_finishes() {
            let upstream_port = upstream().await;
            let gateway = gateway(4);
            let front = front(gateway.clone()).await;
            let opened = open(&gateway, upstream_port).await;
            let cookie = pair(&bootstrap(front, "env-p1.zone.test", &opened.capability).await);
            let mut stream = http()
                .get(format!("http://{front}/stream"))
                .header(header::HOST, "env-p1.zone.test")
                .header(header::COOKIE, &cookie)
                .send()
                .await
                .unwrap();
            assert!(stream.chunk().await.unwrap().is_some(), "the stream flows");

            // The main server's graceful shutdown waits for this exchange; the gateway's
            // drain is what ends it.
            gateway.shutdown().await;
            tokio::time::timeout(Duration::from_secs(5), async {
                while let Ok(Some(_)) = stream.chunk().await {}
            })
            .await
            .expect("the routed exchange ends once the gateway shuts down");
        }

        #[tokio::test]
        async fn a_second_shutdown_call_never_cuts_the_first_drain_short() {
            let upstream_port = upstream().await;
            let gateway = gateway(4);
            let front = front(gateway.clone()).await;
            let opened = open(&gateway, upstream_port).await;
            let cookie = pair(&bootstrap(front, "env-p1.zone.test", &opened.capability).await);
            let mut stream = http()
                .get(format!("http://{front}/stream"))
                .header(header::HOST, "env-p1.zone.test")
                .header(header::COOKIE, &cookie)
                .send()
                .await
                .unwrap();
            assert!(stream.chunk().await.unwrap().is_some());

            // The server starts the drain when shutdown is signaled; the runtime calls again.
            let first = tokio::spawn({
                let gateway = gateway.clone();
                async move { gateway.shutdown().await }
            });
            tokio::time::sleep(Duration::from_millis(50)).await;
            let second = tokio::spawn({
                let gateway = gateway.clone();
                async move { gateway.shutdown().await }
            });
            tokio::time::sleep(Duration::from_millis(300)).await;
            assert!(
                matches!(stream.chunk().await, Ok(Some(_))),
                "the in-flight exchange still drains"
            );
            assert!(!second.is_finished(), "the second call waits for the drain");
            first.await.unwrap();
            second.await.unwrap();
        }

        #[tokio::test]
        async fn only_published_preview_hostnames_on_loopback_are_routed() {
            let gateway = gateway(2);
            let front = front(gateway.clone()).await;
            let status = |host: &'static str| {
                http()
                    .get(format!("http://{front}/"))
                    .header(header::HOST, host)
                    .send()
            };
            let empty = status("env-p2.zone.test").await.unwrap();
            assert_eq!(empty.status(), StatusCode::UNAUTHORIZED, "an empty slot");
            assert_eq!(empty.headers()["cloudflare-cdn-cache-control"], "no-store");
            assert!(
                empty
                    .text()
                    .await
                    .unwrap()
                    .contains("This preview link expired")
            );
            for host in ["env.zone.test", "other-p1.zone.test", "localhost:3773"] {
                assert_eq!(
                    status(host).await.unwrap().status(),
                    StatusCode::NOT_FOUND,
                    "{host} is handed back"
                );
            }
            assert_eq!(
                gateway.relay_slot(
                    Some("192.168.1.5".parse().unwrap()),
                    Some("env-p1.zone.test")
                ),
                None,
                "a non-loopback socket is never routed"
            );
            assert_eq!(gateway.relay_slot(None, Some("env-p1.zone.test")), None);
            assert_eq!(
                gateway.relay_slot(
                    Some(Ipv4Addr::LOCALHOST.into()),
                    Some("ENV-P1.zone.test:443")
                ),
                Some(0)
            );
        }

        #[tokio::test]
        async fn a_slot_changes_hands_only_after_its_target_closes() {
            let (first_port, second_port) = (upstream().await, upstream().await);
            let gateway = gateway(1);
            let front = front(gateway.clone()).await;
            let first = open(&gateway, first_port).await;
            let cookie = pair(&bootstrap(front, "env-p1.zone.test", &first.capability).await);

            let reach = ClientReach {
                local_ip: Ipv4Addr::LOCALHOST.into(),
                host: Some("env.zone.test".into()),
            };
            let second_url = format!("http://localhost:{second_port}/");
            assert!(
                matches!(
                    gateway
                        .open("t", &second_url, "s", i64::MAX, Some(&reach))
                        .await,
                    Err(crate::preview::gateway::registry::GatewayError::RelayBusy)
                ),
                "an open but idle target keeps its slot"
            );

            gateway.close_target("t", first_port).await;
            let second = open(&gateway, second_port).await;
            assert_eq!(
                second.origin, first.origin,
                "the closed target's slot is free"
            );
            let response = http()
                .get(format!("http://{front}/"))
                .header(header::HOST, "env-p1.zone.test")
                .header(header::COOKIE, &cookie)
                .send()
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "the closed target's cookie authenticates nothing on the new one"
            );
            gateway.shutdown().await;
        }
    }
}
