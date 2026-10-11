//! Pure request/response rewriting for the preview gateway: cookies, `Location`, origin
//! checks, and the minimal HTML pages the gateway serves itself.

use url::{Host, Position, Url};

pub const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

const GATEWAY_COOKIE_PREFIX: &str = "bibcode-gw-";
/// Prefix of every BiBCode session cookie: desktop mode names it `bibcode_session_<port>`,
/// so another instance's session on the same host must stay protected too.
const SESSION_COOKIE_PREFIX: &str = "bibcode_session";

pub fn gateway_cookie_name(gateway_port: u16) -> String {
    format!("{GATEWAY_COOKIE_PREFIX}{gateway_port}")
}

// Cookie headers are handled as bytes: browsers send non-ASCII cookie values as raw UTF-8
// (or worse), and one such cookie must not cost the request its gateway cookie or drop an
// upstream `Set-Cookie`. Every byte that is not a separator passes through unchanged.

/// Value of this listener's own gateway cookie; cookies for other gateway ports are ignored.
pub fn gateway_session_from_cookie(cookie_header: &[u8], gateway_port: u16) -> Option<String> {
    let wanted = gateway_cookie_name(gateway_port);
    cookie_pairs(cookie_header).find_map(|pair| {
        let (name, value) = split_pair(pair)?;
        (name == wanted.as_bytes())
            .then(|| String::from_utf8(value.trim_ascii().to_vec()).ok())
            .flatten()
    })
}

fn is_reserved_cookie(name: &[u8], session_cookie_name: &str) -> bool {
    name == session_cookie_name.as_bytes()
        || name.starts_with(SESSION_COOKIE_PREFIX.as_bytes())
        || name.starts_with(GATEWAY_COOKIE_PREFIX.as_bytes())
}

fn cookie_pairs(header: &[u8]) -> impl Iterator<Item = &[u8]> {
    header.split(|byte| *byte == b';').map(<[u8]>::trim_ascii)
}

fn split_pair(pair: &[u8]) -> Option<(&[u8], &[u8])> {
    let at = pair.iter().position(|byte| *byte == b'=')?;
    Some((pair[..at].trim_ascii(), &pair[at + 1..]))
}

fn cookie_name(pair: &[u8]) -> &[u8] {
    split_pair(pair).map_or(pair, |(name, _)| name)
}

fn join_pairs<'a>(pairs: impl Iterator<Item = &'a [u8]>) -> Vec<u8> {
    let mut joined = Vec::new();
    for pair in pairs {
        if !joined.is_empty() {
            joined.extend_from_slice(b"; ");
        }
        joined.extend_from_slice(pair);
    }
    joined
}

/// Request `Cookie` header without the BiBCode session cookie and any gateway cookie;
/// `None` when nothing is left to forward.
pub fn strip_request_cookies(cookie_header: &[u8], session_cookie_name: &str) -> Option<Vec<u8>> {
    let kept = join_pairs(cookie_pairs(cookie_header).filter(|pair| {
        !pair.is_empty() && !is_reserved_cookie(cookie_name(pair), session_cookie_name)
    }));
    (!kept.is_empty()).then_some(kept)
}

/// Upstream `Set-Cookie` without its `Domain` attribute; `None` when it would overwrite a
/// BiBCode session or gateway cookie, or when it is nameless. Browsers store a nameless
/// cookie and send its bare value, so `=bibcode_session=x` would arrive as a reserved cookie.
pub fn filter_set_cookie(value: &[u8], session_cookie_name: &str) -> Option<Vec<u8>> {
    let mut parts = cookie_pairs(value);
    let first = parts.next()?;
    let (name, _) = split_pair(first)?;
    if name.is_empty() || is_reserved_cookie(name, session_cookie_name) {
        return None;
    }
    Some(join_pairs(std::iter::once(first).chain(parts.filter(
        |attribute| !cookie_name(attribute).eq_ignore_ascii_case(b"domain"),
    ))))
}

/// Points an upstream redirect at the client-facing gateway origin when it targets the
/// upstream's own loopback port; every other `Location` is returned unchanged.
pub fn rewrite_location(location: &str, upstream_port: u16, client_origin: &str) -> String {
    // Browsers treat `\` like `/` in special-scheme URLs, so `\\host`, `/\host` and `\/host`
    // are all protocol-relative.
    let mut leading = location.chars();
    let protocol_relative = matches!(
        (leading.next(), leading.next()),
        (Some('/' | '\\'), Some('/' | '\\'))
    );
    let absolute = if protocol_relative {
        format!("http://{}", &location[2..])
    } else {
        location.to_owned()
    };
    let Ok(url) = Url::parse(&absolute) else {
        return location.to_owned();
    };
    let is_upstream_loopback = url.scheme() == "http"
        && url.port_or_known_default() == Some(upstream_port)
        && match url.host() {
            Some(Host::Domain(domain)) => domain == "localhost",
            Some(Host::Ipv4(ip)) => ip.octets() == [127, 0, 0, 1],
            Some(Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        };
    if is_upstream_loopback {
        format!("{client_origin}{}", &url[Position::BeforePath..])
    } else {
        location.to_owned()
    }
}

pub fn client_origin(scheme: &str, host_header: &str) -> String {
    format!("{scheme}://{host_header}")
}

/// Gateway origin rule: unsafe or upgrade requests must carry exactly the origin the
/// client used to reach the gateway.
pub fn gateway_origin_allowed(origin: Option<&str>, client_origin: &str) -> bool {
    origin == Some(client_origin)
}

/// `to` when it is a same-origin absolute path: no scheme, no host, no `//`, no backslash,
/// and no control characters that browsers strip or reinterpret inside URLs.
pub fn bootstrap_target(to: &str) -> Option<String> {
    let is_path = to.starts_with('/')
        && !to.starts_with("//")
        && !to.contains('\\')
        && !to.chars().any(char::is_control);
    is_path.then(|| to.to_owned())
}

/// Page that replaces the bootstrap URL, and its capability, with `to`. An invalid `to`
/// falls back to `/` so the page can never navigate off the gateway origin. A validated
/// BiBCode UI origin is kept in the frame's `sessionStorage` for the navigation reporter.
pub fn bootstrap_page(to: &str, ui_origin: Option<&str>) -> String {
    let script_literal = |value: &str| {
        serde_json::to_string(value)
            .expect("a string always serializes")
            .replace('<', "\\u003c")
    };
    let target = script_literal(&bootstrap_target(to).unwrap_or_else(|| "/".to_owned()));
    let remember = ui_origin.map_or_else(String::new, |origin| {
        format!(
            "try{{sessionStorage.setItem({},{})}}catch(e){{}};",
            script_literal(super::frame::UI_ORIGIN_STORAGE_KEY),
            script_literal(origin)
        )
    });
    format!(
        "<!doctype html><meta charset=\"utf-8\"><meta name=\"referrer\" content=\"no-referrer\">\
<title>Opening preview</title><script>{remember}location.replace({target})</script>"
    )
}

fn escape_html(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// Minimal self-contained page for gateway-generated 401/403/502/503 responses.
pub fn status_page(title: &str, body: &str) -> String {
    let (title, body) = (escape_html(title), escape_html(body));
    format!(
        "<!doctype html><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\">\
<meta name=\"color-scheme\" content=\"light dark\"><title>{title}</title>\
<body style=\"font:16px system-ui,sans-serif;max-width:32rem;margin:4rem auto;padding:0 1rem\">\
<h1 style=\"font-size:1.25rem\">{title}</h1><p>{body}</p>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_keeps_query_and_fragment() {
        assert_eq!(
            bootstrap_target("/?key=ab12#x").as_deref(),
            Some("/?key=ab12#x")
        );
        for bad in [
            "https://evil.test/",
            "//evil.test/",
            "/\\evil.test",
            "/\t/evil.test",
            "/a\r\nb",
            "javascript:alert(1)",
            "",
        ] {
            assert_eq!(bootstrap_target(bad), None, "{bad:?}");
        }
        let page = bootstrap_page("/a?b=\"</script>", None);
        assert!(page.contains("location.replace("));
        assert!(!page.contains("</script>\""));
        assert_eq!(page.matches("</script>").count(), 1);
    }

    #[test]
    fn bootstrap_page_never_navigates_off_path() {
        let page = bootstrap_page("//evil.test/", None);
        assert!(page.contains("location.replace(\"/\")"));
    }

    fn strip(header: &str, session: &str) -> Option<String> {
        strip_request_cookies(header.as_bytes(), session)
            .map(|kept| String::from_utf8(kept).unwrap())
    }

    fn filter(value: &str, session: &str) -> Option<String> {
        filter_set_cookie(value.as_bytes(), session).map(|kept| String::from_utf8(kept).unwrap())
    }

    #[test]
    fn cookies_are_stripped_and_filtered() {
        assert_eq!(
            strip(
                "bibcode_session=x; app=1; bibcode-gw-40001=y",
                "bibcode_session"
            )
            .as_deref(),
            Some("app=1")
        );
        assert_eq!(strip("bibcode-gw-1=a", "bibcode_session"), None);
        assert_eq!(
            filter("sid=1; Domain=evil.test; Path=/", "bibcode_session").as_deref(),
            Some("sid=1; Path=/")
        );
        assert_eq!(
            filter("bibcode_session=evil; Path=/", "bibcode_session"),
            None
        );
        assert_eq!(filter("bibcode-gw-40001=evil", "bibcode_session"), None);
        assert_eq!(
            gateway_session_from_cookie(b"a=1; bibcode-gw-40001=sess; bibcode-gw-40002=o", 40001)
                .as_deref(),
            Some("sess")
        );
        assert_eq!(gateway_cookie_name(40001), "bibcode-gw-40001");
    }

    #[test]
    fn non_ascii_cookies_pass_through_byte_for_byte() {
        // Raw UTF-8 and a lone Latin-1 byte, as some apps and browsers send them.
        let header =
            b"name=Jos\xc3\xa9; bibcode-gw-40001=sess; legacy=\xe9t\xe9; bibcode_session=x";
        assert_eq!(
            gateway_session_from_cookie(header, 40001).as_deref(),
            Some("sess")
        );
        assert_eq!(
            strip_request_cookies(header, "bibcode_session").as_deref(),
            Some(&b"name=Jos\xc3\xa9; legacy=\xe9t\xe9"[..])
        );
        assert_eq!(
            filter_set_cookie(
                b"pref=\xe9t\xe9; Domain=evil.test; Path=/",
                "bibcode_session"
            )
            .as_deref(),
            Some(&b"pref=\xe9t\xe9; Path=/"[..])
        );
        assert_eq!(
            filter_set_cookie(b"bibcode-gw-1=\xe9", "bibcode_session"),
            None
        );
    }

    #[test]
    fn nameless_set_cookies_are_dropped() {
        for nameless in [
            "=bibcode_session=evil",
            "=bibcode-gw-40001=x",
            "=x",
            "x",
            "; Path=/",
            "",
        ] {
            assert_eq!(filter(nameless, "bibcode_session"), None, "{nameless:?}");
        }
    }

    #[test]
    fn instance_scoped_session_cookies_are_reserved() {
        assert_eq!(
            filter("bibcode_session_3773=evil; Path=/", "bibcode_session_4000"),
            None
        );
        assert_eq!(
            strip(
                "bibcode_session_3773=a; app=1; bibcode_session_4000=b",
                "bibcode_session_4000"
            )
            .as_deref(),
            Some("app=1")
        );
    }

    #[test]
    fn location_rewrites_only_the_upstream_loopback() {
        let origin = "http://127.0.0.1:41000";
        assert_eq!(
            rewrite_location("http://localhost:5173/a?b", 5173, origin),
            "http://127.0.0.1:41000/a?b"
        );
        assert_eq!(
            rewrite_location("http://127.0.0.1:5173/", 5173, origin),
            "http://127.0.0.1:41000/"
        );
        assert_eq!(
            rewrite_location("http://[::1]:5173/x", 5173, origin),
            "http://127.0.0.1:41000/x"
        );
        assert_eq!(
            rewrite_location("//localhost:5173/x#f", 5173, origin),
            "http://127.0.0.1:41000/x#f"
        );
        for backslash_form in [
            "\\\\localhost:5173/x#f",
            "/\\localhost:5173/x#f",
            "\\/localhost:5173/x#f",
        ] {
            assert_eq!(
                rewrite_location(backslash_form, 5173, origin),
                "http://127.0.0.1:41000/x#f",
                "{backslash_form}"
            );
        }
        assert_eq!(
            rewrite_location("\\\\localhost:9999/x", 5173, origin),
            "\\\\localhost:9999/x"
        );
        assert_eq!(rewrite_location("/relative", 5173, origin), "/relative");
        assert_eq!(
            rewrite_location("http://localhost:9999/", 5173, origin),
            "http://localhost:9999/"
        );
        assert_eq!(
            rewrite_location("http://example.test:5173/", 5173, origin),
            "http://example.test:5173/"
        );
    }

    #[test]
    fn gateway_origin_check_requires_exact_client_origin() {
        assert!(gateway_origin_allowed(
            Some("http://127.0.0.1:41000"),
            "http://127.0.0.1:41000"
        ));
        assert!(!gateway_origin_allowed(
            Some("http://127.0.0.1:41001"),
            "http://127.0.0.1:41000"
        ));
        assert!(!gateway_origin_allowed(None, "http://127.0.0.1:41000"));
        assert_eq!(client_origin("http", "h:41000"), "http://h:41000");
    }

    #[test]
    fn status_page_escapes_text() {
        let page = status_page("<b>t</b>", "a & \"<i>\"");
        assert!(!page.contains("<b>") && !page.contains("<i>"));
        assert!(
            page.contains("&lt;b&gt;t&lt;/b&gt;") && page.contains("a &amp; &quot;&lt;i&gt;&quot;")
        );
    }
}
