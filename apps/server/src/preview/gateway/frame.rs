//! Lets BiBCode's own UI frame gateway previews and follow their navigation:
//! the framing policy, the bootstrap's UI origin, and the same-origin
//! navigation reporter the gateway inserts into HTML pages.

use http_body_util::{BodyExt, StreamBody};
use hyper::body::{Body, Bytes, Frame};
use hyper::header::{CONTENT_SECURITY_POLICY, HeaderMap, HeaderValue, X_FRAME_OPTIONS};

/// Same-origin path of the navigation reporter.
pub const FRAME_SCRIPT_PATH: &str = "/__bibcode/frame.js";

/// `sessionStorage` key the bootstrap page stores the validated UI origin under.
pub const UI_ORIGIN_STORAGE_KEY: &str = "bibcode-ui-origin";

/// How far into an HTML response the gateway looks for `<head>`.
const INJECT_SCAN_LIMIT: usize = 64 * 1024;

const FRAME_SCRIPT_TAG: &[u8] = b"<script src=\"/__bibcode/frame.js\"></script>";

/// Posts the frame's URL, title, and history state to the BiBCode UI that
/// framed it (the origin the bootstrap stored), obeys back/forward/reload
/// commands from that origin only, sends links to other origins out of the
/// frame (asking BiBCode to open the server's localhost ones), and keeps the
/// reload keys to the frame. Does nothing when not framed.
pub const FRAME_SCRIPT: &str = r#"(() => {
  "use strict";
  // Only the frame BiBCode itself holds (BiBCode is always the top window): a
  // page's own nested frames keep their links and keys, and their reports
  // would reach only that page.
  if (window.top === window || window.parent !== window.top) return;
  let uiOrigin = null;
  try { uiOrigin = sessionStorage.getItem("bibcode-ui-origin"); } catch (_) {}
  if (!uiOrigin) return;
  // Only the Navigation API traverses this frame's own history; `history` would
  // move the whole tab, BiBCode included, so without it Back/Forward stay off.
  const nav = window.navigation;
  let lastTitle = null;
  const report = () => {
    lastTitle = document.title;
    try {
      window.parent.postMessage({
        type: "bibcode-preview-frame",
        url: location.href,
        title: document.title,
        canGoBack: nav ? nav.canGoBack : false,
        canGoForward: nav ? nav.canGoForward : false,
      }, uiOrigin);
    } catch (_) {}
  };
  for (const name of ["pushState", "replaceState"]) {
    const original = history[name];
    history[name] = function (...args) {
      const result = original.apply(this, args);
      queueMicrotask(report);
      return result;
    };
  }
  addEventListener("popstate", report);
  addEventListener("hashchange", report);
  addEventListener("load", report);
  if (nav) nav.addEventListener("navigatesuccess", report);
  const watchTitle = () => {
    report();
    // A title inserted, replaced, or edited later; only a real change reports.
    const root = document.head || document.documentElement;
    new MutationObserver(() => {
      if (document.title !== lastTitle) report();
    }).observe(root, { childList: true, characterData: true, subtree: true });
  };
  if (document.readyState === "loading") addEventListener("DOMContentLoaded", watchTitle);
  else watchTitle();
  // A link to another origin leaves what BiBCode can follow (and another host
  // is blocked by the UI's framing policy). One to the server's localhost (the
  // app's own absolute link, or another dev server) means the server, not this
  // computer, however it is opened: BiBCode opens it as a tab. A plain click to
  // any other origin opens a new browser tab instead.
  const followLink = (event) => {
    if (event.defaultPrevented || (event.type === "auxclick" && event.button !== 1)) return;
    const link = event.target instanceof Element ? event.target.closest("a[href]") : null;
    if (!link || link.hasAttribute("download")) return;
    let url;
    try { url = new URL(link.href, location.href); } catch (_) { return; }
    if (!/^https?:$/.test(url.protocol) || url.origin === location.origin) return;
    if (/^(localhost|.+\.localhost|127(\.\d+){3}|\[::1\]|0\.0\.0\.0|\[::\])$/.test(url.hostname)) {
      event.preventDefault();
      try { window.parent.postMessage({ type: "bibcode-preview-open", url: url.href }, uiOrigin); } catch (_) {}
      return;
    }
    if (event.type !== "click" || event.button !== 0) return;
    if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    if (typeof link.target !== "string" || (link.target && link.target !== "_self")) return;
    event.preventDefault();
    window.open(url.href, "_blank", "noopener");
  };
  addEventListener("click", followLink);
  addEventListener("auxclick", followLink);
  // The browser's reload keys would reload BiBCode itself from inside the frame.
  addEventListener("keydown", (event) => {
    const reloadKey = event.key === "F5" || ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "r");
    if (!reloadKey || event.defaultPrevented || event.altKey) return;
    event.preventDefault();
    location.reload();
  });
  addEventListener("message", (event) => {
    if (event.origin !== uiOrigin || event.source !== window.parent) return;
    const data = event.data;
    if (!data || data.type !== "bibcode-preview-command") return;
    if (data.command === "reload") location.reload();
    else if (nav && data.command === "back" && nav.canGoBack) nav.back();
    else if (nav && data.command === "forward" && nav.canGoForward) nav.forward();
  });
})();
"#;

/// Only the BiBCode UI that opened the preview, and the preview's own pages
/// (frames nested inside it), may frame it. `None` for an IPv6 origin: browsers
/// don't parse IPv6 host sources, so the gateway cookie (which another site's
/// frame never carries) is the only guard there.
pub fn frame_ancestors_policy(ui_origin: &str) -> Option<String> {
    (!ui_origin.contains("://[")).then(|| format!("frame-ancestors 'self' {ui_origin}"))
}

/// For a preview no BiBCode UI frames (a desktop view, a system-browser tab):
/// the upstream's own rules stay, and only the preview's own pages may frame it,
/// so a same-site sibling cannot frame it with the gateway cookie.
pub fn apply_self_only_framing(headers: &mut HeaderMap) {
    headers.append(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("frame-ancestors 'self'"),
    );
}

/// An upstream CSP header value without `frame-ancestors` directives; one value
/// can list several comma-separated policies, and each keeps its other
/// directives. Works on bytes, so a value with non-ASCII bytes keeps its other
/// directives too. `None` when nothing else is left.
pub fn strip_frame_ancestors(value: &[u8]) -> Option<Vec<u8>> {
    fn trim(bytes: &[u8]) -> &[u8] {
        let start = bytes
            .iter()
            .position(|byte| !byte.is_ascii_whitespace())
            .unwrap_or(bytes.len());
        let end = bytes
            .iter()
            .rposition(|byte| !byte.is_ascii_whitespace())
            .map_or(start, |end| end + 1);
        &bytes[start..end]
    }
    let policies: Vec<Vec<u8>> = value
        .split(|byte| *byte == b',')
        .filter_map(|policy| {
            let kept: Vec<&[u8]> = policy
                .split(|byte| *byte == b';')
                .map(trim)
                .filter(|directive| {
                    let name = directive
                        .split(u8::is_ascii_whitespace)
                        .next()
                        .unwrap_or_default();
                    !directive.is_empty() && !name.eq_ignore_ascii_case(b"frame-ancestors")
                })
                .collect();
            (!kept.is_empty()).then(|| kept.join(b"; ".as_slice()))
        })
        .collect();
    (!policies.is_empty()).then(|| policies.join(b", ".as_slice()))
}

/// Lets `ui_origin` (a validated BiBCode UI origin) frame the response:
/// replaces the upstream's framing restrictions with the gateway's policy.
pub fn apply_frame_policy(headers: &mut HeaderMap, ui_origin: &str) {
    headers.remove(X_FRAME_OPTIONS);
    let policies: Vec<HeaderValue> = headers
        .get_all(CONTENT_SECURITY_POLICY)
        .iter()
        .filter_map(|value| strip_frame_ancestors(value.as_bytes()))
        .filter_map(|policy| HeaderValue::from_bytes(&policy).ok())
        .collect();
    headers.remove(CONTENT_SECURITY_POLICY);
    for policy in policies {
        headers.append(CONTENT_SECURITY_POLICY, policy);
    }
    if let Some(policy) = frame_ancestors_policy(ui_origin)
        && let Ok(policy) = HeaderValue::from_str(&policy)
    {
        headers.append(CONTENT_SECURITY_POLICY, policy);
    }
}

/// The BiBCode UI origin from the bootstrap link, kept only when it is a bare
/// `http`/`https` origin on the gateway's own host, and returned as browsers
/// serialize it (lowercase host, no default port) so `MessageEvent.origin`
/// matches it exactly.
pub fn validated_ui_origin(ui: &str, host_header: &str) -> Option<String> {
    if ui.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    let parsed = url::Url::parse(ui).ok()?;
    let bare = matches!(parsed.scheme(), "http" | "https")
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.query().is_none()
        && parsed.fragment().is_none()
        && parsed.path() == "/";
    if !bare {
        return None;
    }
    let host = match parsed.host()? {
        url::Host::Ipv6(address) => format!("[{address}]"),
        host => host.to_string(),
    };
    let gateway_host = url::Url::parse(&format!("http://{host_header}"))
        .ok()
        .and_then(|gateway| {
            gateway.host().map(|host| match host {
                url::Host::Ipv6(address) => format!("[{address}]"),
                host => host.to_string(),
            })
        })?;
    (host == gateway_host).then(|| parsed.origin().ascii_serialization())
}

/// Raw-text elements whose contents are never markup.
const RAW_TEXT_ELEMENTS: [&[u8]; 6] = [
    b"script",
    b"style",
    b"title",
    b"textarea",
    b"noscript",
    b"template",
];

/// Tags that may come before `<head>`; any other tag means the head was omitted.
fn may_precede_head(name: &[u8]) -> bool {
    name.eq_ignore_ascii_case(b"html")
        || RAW_TEXT_ELEMENTS
            .iter()
            .any(|raw| name.eq_ignore_ascii_case(raw))
        || name.eq_ignore_ascii_case(b"meta")
        || name.eq_ignore_ascii_case(b"link")
        || name.eq_ignore_ascii_case(b"base")
}

/// Index just after the end of the tag starting at `start` (a `<`), skipping
/// quoted attribute values; `None` when the tag is incomplete.
fn tag_end(html: &[u8], start: usize) -> Option<usize> {
    let mut quote = None;
    for (offset, byte) in html[start + 1..].iter().enumerate() {
        match (quote, byte) {
            (None, b'"' | b'\'') => quote = Some(*byte),
            (Some(open), _) if *byte == open => quote = None,
            (None, b'>') => return Some(start + 1 + offset + 1),
            _ => {}
        }
    }
    None
}

/// Where the document's head starts, as far as the bytes so far tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadScan {
    /// Index just after the `<head …>` start tag.
    Found(usize),
    /// Body content came first: the document omitted its head.
    Absent,
    /// More bytes are needed to decide.
    Incomplete,
}

/// Finds the document's `<head …>` start tag. Tags are read with their quoted
/// attribute values; comments, doctypes, and raw-text contents (scripts,
/// styles) are skipped; body content (a body-level tag or text) before any
/// `<head>` means the head was omitted.
pub fn scan_for_head(html: &[u8]) -> HeadScan {
    const BOM: &[u8] = b"\xEF\xBB\xBF";
    if html.len() < BOM.len() && BOM.starts_with(html) {
        return HeadScan::Incomplete;
    }
    let mut index = if html.starts_with(BOM) { BOM.len() } else { 0 };
    loop {
        let Some(offset) = html[index..].iter().position(|byte| *byte == b'<') else {
            return if html[index..].iter().all(u8::is_ascii_whitespace) {
                HeadScan::Incomplete
            } else {
                HeadScan::Absent
            };
        };
        if !html[index..index + offset]
            .iter()
            .all(u8::is_ascii_whitespace)
        {
            return HeadScan::Absent;
        }
        let start = index + offset;
        let rest = &html[start..];
        if rest.starts_with(b"<!--") {
            let Some(end) = rest[4..].windows(3).position(|window| window == b"-->") else {
                return HeadScan::Incomplete;
            };
            index = start + 4 + end + 3;
            continue;
        }
        if rest.len() < 2 {
            return HeadScan::Incomplete;
        }
        if rest.starts_with(b"<!") || rest.starts_with(b"<?") || rest.starts_with(b"</") {
            let Some(end) = tag_end(html, start) else {
                return HeadScan::Incomplete;
            };
            index = end;
            continue;
        }
        let name_length = rest[1..]
            .iter()
            .take_while(|byte| byte.is_ascii_alphanumeric())
            .count();
        if name_length == 0 {
            // A lone `<` is text.
            return HeadScan::Absent;
        }
        let name = &rest[1..=name_length];
        let Some(end) = tag_end(html, start) else {
            return HeadScan::Incomplete;
        };
        if name.eq_ignore_ascii_case(b"head") {
            return HeadScan::Found(end);
        }
        if !may_precede_head(name) {
            return HeadScan::Absent;
        }
        index = end;
        if let Some(raw) = RAW_TEXT_ELEMENTS
            .iter()
            .find(|raw| name.eq_ignore_ascii_case(raw))
        {
            // Skip the element's contents up to its end tag (`</script>`, not `</scripture>`).
            let close = html[index..].windows(raw.len() + 3).position(|window| {
                window[..2] == *b"</"
                    && window[2..2 + raw.len()].eq_ignore_ascii_case(raw)
                    && matches!(
                        window[2 + raw.len()],
                        b'>' | b'/' | b' ' | b'\t' | b'\n' | b'\r' | b'\x0c'
                    )
            });
            let Some(close) = close else {
                return HeadScan::Incomplete;
            };
            // HTML's escaped script states can hide end tags; leave such pages alone.
            if html[index..index + close]
                .windows(4)
                .any(|window| window == b"<!--")
            {
                return HeadScan::Absent;
            }
            index += close;
        }
    }
}

enum InjectState<B> {
    Scanning {
        body: B,
        buffer: Vec<u8>,
    },
    /// A non-data frame (trailers) that ended the scan, sent after the buffered bytes.
    Pending {
        body: B,
        frame: Frame<Bytes>,
    },
    Streaming {
        body: B,
    },
    Done,
}

/// Streams `body`, inserting the reporter script tag right after `<head>` when
/// it appears in the first 64 KiB; otherwise the bytes pass through unchanged.
pub fn inject_frame_script<B>(body: B) -> impl Body<Data = Bytes, Error = B::Error> + Send + 'static
where
    B: Body<Data = Bytes> + Send + Unpin + 'static,
    B::Error: Send,
{
    let state = InjectState::Scanning {
        body,
        buffer: Vec::new(),
    };
    let frames = futures_util::stream::unfold(state, |state| async move {
        match state {
            InjectState::Scanning {
                mut body,
                mut buffer,
            } => loop {
                match body.frame().await {
                    Some(Ok(frame)) => match frame.into_data() {
                        Ok(data) => {
                            buffer.extend_from_slice(&data);
                            let scan = scan_for_head(&buffer);
                            if let HeadScan::Found(position) = scan {
                                let mut injected =
                                    Vec::with_capacity(buffer.len() + FRAME_SCRIPT_TAG.len());
                                injected.extend_from_slice(&buffer[..position]);
                                injected.extend_from_slice(FRAME_SCRIPT_TAG);
                                injected.extend_from_slice(&buffer[position..]);
                                return Some((
                                    Ok(Frame::data(Bytes::from(injected))),
                                    InjectState::Streaming { body },
                                ));
                            }
                            // A head-less page, or one with no head early on, streams as is.
                            if scan == HeadScan::Absent || buffer.len() >= INJECT_SCAN_LIMIT {
                                return Some((
                                    Ok(Frame::data(Bytes::from(buffer))),
                                    InjectState::Streaming { body },
                                ));
                            }
                        }
                        Err(frame) if buffer.is_empty() => {
                            return Some((Ok(frame), InjectState::Streaming { body }));
                        }
                        Err(frame) => {
                            return Some((
                                Ok(Frame::data(Bytes::from(buffer))),
                                InjectState::Pending { body, frame },
                            ));
                        }
                    },
                    Some(Err(error)) => return Some((Err(error), InjectState::Done)),
                    None if buffer.is_empty() => return None,
                    None => {
                        return Some((Ok(Frame::data(Bytes::from(buffer))), InjectState::Done));
                    }
                }
            },
            InjectState::Pending { body, frame } => {
                Some((Ok(frame), InjectState::Streaming { body }))
            }
            InjectState::Streaming { mut body } => body
                .frame()
                .await
                .map(|frame| (frame, InjectState::Streaming { body })),
            InjectState::Done => None,
        }
    });
    StreamBody::new(frames)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::Infallible;

    async fn injected(chunks: &[&str]) -> String {
        let frames: Vec<Result<Frame<Bytes>, Infallible>> = chunks
            .iter()
            .map(|chunk| Ok(Frame::data(Bytes::from(chunk.to_string()))))
            .collect();
        let body = StreamBody::new(futures_util::stream::iter(frames));
        let bytes = inject_frame_script(body)
            .collect()
            .await
            .expect("body")
            .to_bytes();
        String::from_utf8(bytes.to_vec()).expect("utf-8")
    }

    const TAG: &str = r#"<script src="/__bibcode/frame.js"></script>"#;

    #[test]
    fn frame_ancestors_allow_only_the_ui_that_opened_the_preview() {
        assert_eq!(
            frame_ancestors_policy("http://box.lan:3773").as_deref(),
            Some("frame-ancestors 'self' http://box.lan:3773")
        );
        // Browsers don't parse IPv6 host sources; the cookie alone keeps other sites out.
        assert_eq!(frame_ancestors_policy("http://[::1]:3773"), None);
    }

    #[test]
    fn upstream_frame_ancestors_directives_are_removed() {
        let strip = |policy: &str| {
            strip_frame_ancestors(policy.as_bytes()).map(|kept| String::from_utf8(kept).unwrap())
        };
        assert_eq!(
            strip("default-src 'self'; frame-ancestors 'none'; script-src 'self'").as_deref(),
            Some("default-src 'self'; script-src 'self'")
        );
        assert_eq!(strip("Frame-Ancestors 'none';"), None);
        // One header can carry several policies; each keeps its other directives.
        assert_eq!(
            strip("frame-ancestors 'none', script-src 'none'").as_deref(),
            Some("script-src 'none'")
        );
        assert_eq!(
            strip("default-src 'self', frame-ancestors 'none'; img-src *").as_deref(),
            Some("default-src 'self', img-src *")
        );
    }

    #[test]
    fn a_non_ascii_upstream_policy_keeps_its_other_directives() {
        let mut headers = HeaderMap::new();
        headers.append(
            CONTENT_SECURITY_POLICY,
            HeaderValue::from_bytes(
                "script-src 'none'; frame-ancestors 'none'; report-uri /résultats".as_bytes(),
            )
            .unwrap(),
        );
        apply_frame_policy(&mut headers, "http://box.lan:3773");
        let policies: Vec<&[u8]> = headers
            .get_all(CONTENT_SECURITY_POLICY)
            .iter()
            .map(HeaderValue::as_bytes)
            .collect();
        assert_eq!(
            policies,
            [
                "script-src 'none'; report-uri /résultats".as_bytes(),
                b"frame-ancestors 'self' http://box.lan:3773".as_slice()
            ]
        );
    }

    #[test]
    fn ipv6_hosts_get_no_gateway_frame_ancestors() {
        let mut headers = HeaderMap::new();
        headers.insert(X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
        apply_frame_policy(&mut headers, "http://[fd00::1]:3773");
        assert!(headers.get(X_FRAME_OPTIONS).is_none());
        assert!(headers.get(CONTENT_SECURITY_POLICY).is_none());
    }

    #[test]
    fn framing_policy_replaces_upstream_restrictions() {
        let mut headers = HeaderMap::new();
        headers.insert(X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
        headers.append(
            CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("frame-ancestors 'none'; img-src *"),
        );
        headers.append(
            CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("frame-ancestors 'self'"),
        );
        apply_frame_policy(&mut headers, "http://box.lan:3773");
        assert!(headers.get(X_FRAME_OPTIONS).is_none());
        let policies: Vec<&str> = headers
            .get_all(CONTENT_SECURITY_POLICY)
            .iter()
            .map(|value| value.to_str().unwrap())
            .collect();
        assert_eq!(
            policies,
            ["img-src *", "frame-ancestors 'self' http://box.lan:3773"]
        );
    }

    #[test]
    fn ui_origin_must_be_a_bare_origin_on_the_gateway_host() {
        let host = "box.lan:41000";
        assert_eq!(
            validated_ui_origin("http://box.lan:3773", host).as_deref(),
            Some("http://box.lan:3773")
        );
        // Stored as browsers serialize it, so `MessageEvent.origin` matches.
        assert_eq!(
            validated_ui_origin("https://BOX.lan:443", host).as_deref(),
            Some("https://box.lan")
        );
        assert_eq!(
            validated_ui_origin("http://Box.Lan:80", host).as_deref(),
            Some("http://box.lan")
        );
        for refused in [
            "http://evil.example:3773",
            "http://box.lan:3773/path",
            "http://box.lan:3773?x",
            "javascript://box.lan",
            "http://user@box.lan",
            "box.lan:3773",
            "http://",
        ] {
            assert_eq!(validated_ui_origin(refused, host), None, "{refused}");
        }
    }

    #[test]
    fn head_position_is_after_the_head_start_tag_only() {
        use HeadScan::{Absent, Found, Incomplete};
        assert_eq!(scan_for_head(b"<html><head><title>"), Found(12));
        assert_eq!(scan_for_head(b"<HTML><HEAD lang=en>x"), Found(20));
        assert_eq!(scan_for_head(b"<header>no head</header>"), Absent);
        assert_eq!(scan_for_head(b"<html><head"), Incomplete);
        assert_eq!(scan_for_head(b"<!doctype html>\n<html>\n"), Incomplete);
        // A `>` inside a quoted attribute value does not end the tag.
        let quoted = br#"<head data-note="a > b" x='>'><title>"#;
        assert_eq!(scan_for_head(quoted), Found(quoted.len() - 7));
        // A `<head>` inside a comment is not the head.
        let commented = b"<!-- <head> --><html><head>x";
        assert_eq!(scan_for_head(commented), Found(commented.len() - 1));
        assert_eq!(scan_for_head(b"<!-- <head> unterminated"), Incomplete);
        // `<head>` text inside an attribute value or a script is not a tag.
        let attribute = br#"<html data-note="<head>"><head>x"#;
        assert_eq!(scan_for_head(attribute), Found(attribute.len() - 1));
        let script = b"<script>var s = '<head>';</script><head>x";
        assert_eq!(scan_for_head(script), Found(script.len() - 1));
        // `</scripture>` does not end a script.
        let lookalike = b"<script>const s = '</scripture><head>';</script><head>x";
        assert_eq!(scan_for_head(lookalike), Found(lookalike.len() - 1));
        // An omitted head: body content arrives first and nothing is injected.
        assert_eq!(scan_for_head(b"<html><body><head>"), Absent);
        assert_eq!(scan_for_head(b"Working..."), Absent);
        // Browsers drop a leading UTF-8 byte order mark, even one split across chunks.
        let bom = b"\xEF\xBB\xBF<!doctype html><html><head>x";
        assert_eq!(scan_for_head(bom), Found(bom.len() - 1));
        assert_eq!(scan_for_head(b"\xEF\xBB"), Incomplete);
        // A pre-head script in HTML's escaped script state is left alone.
        assert_eq!(
            scan_for_head(br#"<script><!-- s = "<script></script><head>"; --></script><head>"#),
            Absent
        );
    }

    #[tokio::test]
    async fn a_headless_stream_is_forwarded_without_waiting() {
        let (sender, receiver) =
            tokio::sync::mpsc::unbounded_channel::<Result<Frame<Bytes>, Infallible>>();
        let frames = futures_util::stream::unfold(receiver, |mut receiver| async move {
            receiver.recv().await.map(|frame| (frame, receiver))
        });
        let mut body = std::pin::pin!(inject_frame_script(StreamBody::new(Box::pin(frames))));
        sender
            .send(Ok(Frame::data(Bytes::from_static(b"<body>Working"))))
            .unwrap();
        // The upstream is still sending; the first bytes must not wait for more.
        let first = tokio::time::timeout(std::time::Duration::from_secs(1), body.frame())
            .await
            .expect("forwarded at once")
            .expect("a frame")
            .expect("data");
        assert_eq!(first.into_data().unwrap(), "<body>Working");
        drop(sender);
    }

    #[tokio::test]
    async fn the_reporter_follows_head_even_across_chunks() {
        assert_eq!(
            injected(&["<!doctype html><html><he", "ad lang=en><title>t</title>"]).await,
            format!("<!doctype html><html><head lang=en>{TAG}<title>t</title>")
        );
        assert_eq!(
            injected(&["<html><head>", "<body>", "rest"]).await,
            format!("<html><head>{TAG}<body>rest")
        );
    }

    #[tokio::test]
    async fn pages_without_an_early_head_pass_through_unchanged() {
        assert_eq!(injected(&["plain ", "text"]).await, "plain text");
        let preamble = "x".repeat(70 * 1024);
        let page = injected(&[&preamble, "<head>late"]).await;
        assert_eq!(page.len(), preamble.len() + "<head>late".len());
        assert!(!page.contains(TAG));
    }
}
