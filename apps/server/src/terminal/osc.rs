//! Answers terminal theme-detection queries at the PTY layer, using the app's
//! resolved theme colors, so provider TUIs adopt the app's light/dark theme.
//!
//! Two protocols are handled:
//!
//! - **OSC 10/11/12 color queries** (`OSC <n> ; ? ST`) — a TUI asks for the
//!   terminal's foreground/background/cursor color.
//! - **DEC private mode 2031 + `DSR ? 996`** — the modern in-band dark/light
//!   scheme protocol (contour/foot/ghostty). Enabling mode 2031
//!   (`CSI ? 2031 h`) or querying with `CSI ? 996 n` is answered with
//!   `CSI ? 997 ; 1 n` only when the configured background is dark. Light
//!   schemes are left unanswered: reporting `997;2` forces Claude Code into a
//!   light fullscreen Ink path that fails under Coddy Studio's xterm host. Codex
//!   and similar agents detect light terminals via OSC 10/11 instead.
//!   OpenCode still receives dark-scheme reports when the terminal is dark.
//!
//! xterm.js implements neither reliably in this embedded terminal — an OSC
//! color reply would travel back through an async websocket + RPC path that
//! misses the provider's startup detection window, and mode 2031 is not
//! answered at all. Replying synchronously here, the moment the query is read
//! from the PTY, closes both gaps without depending on the client.

/// The three OSC "special colors" Coddy Studio answers queries for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SpecialColor {
    Foreground,
    Background,
    Cursor,
}

impl SpecialColor {
    fn from_code(code: u16) -> Option<Self> {
        match code {
            10 => Some(Self::Foreground),
            11 => Some(Self::Background),
            12 => Some(Self::Cursor),
            _ => None,
        }
    }

    fn code(self) -> u16 {
        match self {
            Self::Foreground => 10,
            Self::Background => 11,
            Self::Cursor => 12,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Terminator {
    Bel,
    St,
}

/// The resolved theme colors used to answer queries. Any color left unset is
/// simply not answered, so the client's own reply (if any) still applies.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OscColors {
    pub foreground: Option<[u8; 3]>,
    pub background: Option<[u8; 3]>,
    pub cursor: Option<[u8; 3]>,
}

impl OscColors {
    pub fn is_empty(&self) -> bool {
        self.foreground.is_none() && self.background.is_none() && self.cursor.is_none()
    }

    fn color_for(&self, color: SpecialColor) -> Option<[u8; 3]> {
        match color {
            SpecialColor::Foreground => self.foreground,
            SpecialColor::Background => self.background,
            SpecialColor::Cursor => self.cursor,
        }
    }

    /// Whether the background reads as dark (Rec. 601 luma below mid-gray).
    /// `None` when no background is configured, so the scheme is left unanswered.
    fn prefers_dark(&self) -> Option<bool> {
        let [r, g, b] = self.background?;
        let luma = 299 * u32::from(r) + 587 * u32::from(g) + 114 * u32::from(b);
        Some(luma < 128 * 1000)
    }
}

/// Reserved launch env keys carrying the app's resolved theme colors. They are
/// consumed by the PTY layer to answer OSC queries and stripped before spawning
/// the child, so they never leak into the provider's environment.
pub const OSC_BACKGROUND_ENV: &str = "CODDY_STUDIO_OSC_BACKGROUND";
pub const OSC_FOREGROUND_ENV: &str = "CODDY_STUDIO_OSC_FOREGROUND";
pub const OSC_CURSOR_ENV: &str = "CODDY_STUDIO_OSC_CURSOR";

/// Whether `key` is one of the exact reserved OSC color env keys.
pub fn is_reserved_osc_env_key(key: &str) -> bool {
    matches!(
        key,
        OSC_BACKGROUND_ENV | OSC_FOREGROUND_ENV | OSC_CURSOR_ENV
    )
}

/// Reads the reserved OSC color env keys from a launch environment. Malformed
/// values are ignored, leaving that color unanswered.
pub fn colors_from_env(env: &std::collections::BTreeMap<String, String>) -> OscColors {
    OscColors {
        foreground: environment_value(env, OSC_FOREGROUND_ENV)
            .and_then(|value| parse_rgb_triplet(value)),
        background: environment_value(env, OSC_BACKGROUND_ENV)
            .and_then(|value| parse_rgb_triplet(value)),
        cursor: environment_value(env, OSC_CURSOR_ENV).and_then(|value| parse_rgb_triplet(value)),
    }
}

fn environment_value<'a>(
    env: &'a std::collections::BTreeMap<String, String>,
    name: &str,
) -> Option<&'a String> {
    env.get(name).or_else(|| {
        env.iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value)
    })
}

/// Parses an `"r,g,b"` (0-255 decimal per channel) color, as supplied through
/// the reserved launch env vars. Returns `None` for any malformed value.
pub fn parse_rgb_triplet(value: &str) -> Option<[u8; 3]> {
    let mut channels = value.split(',');
    let mut rgb = [0u8; 3];
    for slot in &mut rgb {
        *slot = channels.next()?.trim().parse::<u8>().ok()?;
    }
    if channels.next().is_some() {
        return None;
    }
    Some(rgb)
}

/// A theme-detection query recognized in the PTY output stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ThemeQuery {
    /// OSC 10/11/12 color query, to answer with the matching color.
    Color(SpecialColor, Terminator),
    /// DEC 2031 enable or `DSR ? 996`, to answer with the current dark/light
    /// scheme.
    ColorScheme,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScanState {
    /// Not inside a candidate sequence.
    Ground,
    /// Saw ESC.
    Escape,
    /// Saw ESC ] , collecting the OSC numeric code.
    Code { code: u16, digits: u8 },
    /// Saw ESC ] <code> ; , expecting the `?` query marker.
    ExpectQuery { code: u16 },
    /// Saw ESC ] <code> ; ? , expecting BEL or ST (ESC \).
    ExpectTerminator { color: SpecialColor },
    /// Saw the ESC of a candidate ST terminator.
    TerminatorEscape { color: SpecialColor },
    /// Saw ESC [ ? , collecting the CSI private-mode parameter.
    CsiPrivate { param: u16, digits: u8 },
}

/// Streaming recognizer for the theme-detection queries in [`ThemeQuery`].
/// Bytes may be fed in arbitrary chunks; sequence state carries across calls.
struct OscQueryScanner {
    state: ScanState,
}

impl OscQueryScanner {
    fn new() -> Self {
        Self {
            state: ScanState::Ground,
        }
    }

    /// Feeds raw PTY output bytes and returns each completed query in arrival
    /// order. Non-query bytes are ignored; the caller still forwards the
    /// original stream to the client unchanged.
    fn push(&mut self, bytes: &[u8]) -> Vec<ThemeQuery> {
        let mut queries = Vec::new();
        for &byte in bytes {
            self.state = match (self.state, byte) {
                // ESC while awaiting a terminator begins a candidate ST; ESC in
                // any other state (re)starts a sequence. These ESC arms come
                // first so they win over the state-specific arms below.
                (ScanState::ExpectTerminator { color }, 0x1b) => {
                    ScanState::TerminatorEscape { color }
                }
                (_, 0x1b) => ScanState::Escape,
                (ScanState::Escape, b']') => ScanState::Code { code: 0, digits: 0 },
                (ScanState::Escape, b'[') => ScanState::CsiPrivate {
                    param: 0,
                    digits: 0,
                },
                // CSI private modes: ESC [ ? <param> <final>. `?` only leads.
                (
                    ScanState::CsiPrivate {
                        param: 0,
                        digits: 0,
                    },
                    b'?',
                ) => ScanState::CsiPrivate {
                    param: 0,
                    digits: 0,
                },
                (ScanState::CsiPrivate { param, digits }, b'0'..=b'9') if digits < 4 => {
                    ScanState::CsiPrivate {
                        param: param.saturating_mul(10) + u16::from(byte - b'0'),
                        digits: digits + 1,
                    }
                }
                (ScanState::CsiPrivate { param, digits }, b'h') if digits > 0 && param == 2031 => {
                    queries.push(ThemeQuery::ColorScheme);
                    ScanState::Ground
                }
                (ScanState::CsiPrivate { param, digits }, b'n') if digits > 0 && param == 996 => {
                    queries.push(ThemeQuery::ColorScheme);
                    ScanState::Ground
                }
                (ScanState::Code { code, digits }, b'0'..=b'9') if digits < 3 => ScanState::Code {
                    code: code * 10 + u16::from(byte - b'0'),
                    digits: digits + 1,
                },
                (ScanState::Code { code, digits }, b';') if digits > 0 => {
                    ScanState::ExpectQuery { code }
                }
                (ScanState::ExpectQuery { code }, b'?') => match SpecialColor::from_code(code) {
                    Some(color) => ScanState::ExpectTerminator { color },
                    None => ScanState::Ground,
                },
                (ScanState::ExpectTerminator { color }, 0x07) => {
                    queries.push(ThemeQuery::Color(color, Terminator::Bel));
                    ScanState::Ground
                }
                (ScanState::TerminatorEscape { color }, b'\\') => {
                    queries.push(ThemeQuery::Color(color, Terminator::St));
                    ScanState::Ground
                }
                _ => ScanState::Ground,
            };
        }
        queries
    }
}

/// Answers terminal theme-detection queries found in a PTY output stream using
/// fixed theme colors. Feed output bytes to [`OscColorResponder::process`];
/// write any returned bytes back to the PTY input.
pub struct OscColorResponder {
    colors: OscColors,
    scanner: OscQueryScanner,
}

impl OscColorResponder {
    pub fn new(colors: OscColors) -> Self {
        Self {
            colors,
            scanner: OscQueryScanner::new(),
        }
    }

    /// Scans `bytes` for theme queries and returns the reply bytes to write back
    /// to the PTY. Empty when there is nothing to answer.
    pub fn process(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut reply = Vec::new();
        for query in self.scanner.push(bytes) {
            match query {
                ThemeQuery::Color(color, terminator) => {
                    if let Some(rgb) = self.colors.color_for(color) {
                        append_color_report(&mut reply, color, rgb, terminator);
                    }
                }
                ThemeQuery::ColorScheme => {
                    // Only report dark schemes. A light `CSI ? 997 ; 2 n` reply
                    // pushes Claude Code into its light fullscreen Ink path,
                    // which repeatedly fails inside Coddy Studio's xterm host and
                    // leaves a garbled fallback UI. Codex (and similar) detect
                    // light/dark via OSC 10/11 instead, so leaving light
                    // unanswered keeps both agents working.
                    if self.colors.prefers_dark() == Some(true) {
                        reply.extend_from_slice(b"\x1b[?997;1n");
                    }
                }
            }
        }
        reply
    }
}

fn append_color_report(
    out: &mut Vec<u8>,
    color: SpecialColor,
    rgb: [u8; 3],
    terminator: Terminator,
) {
    // Scale each 8-bit channel to 16-bit the way xterm does (v * 0x101), so a
    // full channel reports ffff and matches what real terminals emit.
    let scaled = rgb.map(|channel| u16::from(channel) * 0x101);
    let body = format!(
        "\x1b]{};rgb:{:04x}/{:04x}/{:04x}",
        color.code(),
        scaled[0],
        scaled[1],
        scaled[2],
    );
    out.extend_from_slice(body.as_bytes());
    match terminator {
        Terminator::Bel => out.push(0x07),
        Terminator::St => out.extend_from_slice(b"\x1b\\"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bg(rgb: [u8; 3]) -> OscColors {
        OscColors {
            background: Some(rgb),
            ..OscColors::default()
        }
    }

    #[test]
    fn answers_background_query_with_bel_terminator() {
        let mut responder = OscColorResponder::new(bg([255, 255, 255]));
        let reply = responder.process(b"\x1b]11;?\x07");
        assert_eq!(reply, b"\x1b]11;rgb:ffff/ffff/ffff\x07");
    }

    #[test]
    fn answers_background_query_with_st_terminator_matching_the_query() {
        let mut responder = OscColorResponder::new(bg([14, 18, 24]));
        let reply = responder.process(b"\x1b]11;?\x1b\\");
        assert_eq!(reply, b"\x1b]11;rgb:0e0e/1212/1818\x1b\\".to_vec());
    }

    #[test]
    fn recognizes_a_query_split_across_chunks() {
        let mut responder = OscColorResponder::new(bg([255, 255, 255]));
        assert!(responder.process(b"\x1b]1").is_empty());
        assert!(responder.process(b"1;").is_empty());
        let reply = responder.process(b"?\x07");
        assert_eq!(reply, b"\x1b]11;rgb:ffff/ffff/ffff\x07");
    }

    #[test]
    fn answers_foreground_and_cursor_when_configured() {
        let mut responder = OscColorResponder::new(OscColors {
            foreground: Some([28, 33, 41]),
            background: Some([255, 255, 255]),
            cursor: Some([180, 203, 255]),
        });
        assert_eq!(
            responder.process(b"\x1b]10;?\x07"),
            b"\x1b]10;rgb:1c1c/2121/2929\x07"
        );
        assert_eq!(
            responder.process(b"\x1b]12;?\x07"),
            b"\x1b]12;rgb:b4b4/cbcb/ffff\x07"
        );
    }

    #[test]
    fn answers_codex_batched_startup_probe_with_both_colors() {
        let mut responder = OscColorResponder::new(OscColors {
            foreground: Some([28, 33, 41]),
            background: Some([255, 255, 255]),
            cursor: Some([38, 56, 78]),
        });
        let query = b"\x1b[6n\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[?u\x1b[c";

        assert_eq!(
            responder.process(query),
            b"\x1b]10;rgb:1c1c/2121/2929\x1b\\\x1b]11;rgb:ffff/ffff/ffff\x1b\\"
        );
    }

    #[test]
    fn answers_codex_batched_probe_across_every_chunk_boundary() {
        let query = b"\x1b[6n\x1b]10;?\x1b\\\x1b]11;?\x1b\\";
        let expected = b"\x1b]10;rgb:1c1c/2121/2929\x1b\\\x1b]11;rgb:ffff/ffff/ffff\x1b\\";
        for split in 1..query.len() {
            let mut responder = OscColorResponder::new(OscColors {
                foreground: Some([28, 33, 41]),
                background: Some([255, 255, 255]),
                cursor: None,
            });
            let mut reply = responder.process(&query[..split]);
            reply.extend(responder.process(&query[split..]));
            assert_eq!(reply, expected, "split at byte {split}");
        }
    }

    #[test]
    fn ignores_queries_for_unset_colors() {
        let mut responder = OscColorResponder::new(bg([255, 255, 255]));
        // Only background is set; a foreground query is left for the client.
        assert!(responder.process(b"\x1b]10;?\x07").is_empty());
    }

    #[test]
    fn ignores_color_set_sequences_and_titles() {
        let mut responder = OscColorResponder::new(bg([255, 255, 255]));
        // Setting the background (rgb:, not ?) must not be answered.
        assert!(responder.process(b"\x1b]11;rgb:00/00/00\x07").is_empty());
        // Window title (OSC 0) must not be answered.
        assert!(responder.process(b"\x1b]0;a title\x07").is_empty());
    }

    #[test]
    fn answers_a_query_embedded_in_surrounding_output() {
        let mut responder = OscColorResponder::new(bg([255, 255, 255]));
        let reply = responder.process(b"hello\x1b]11;?\x07world");
        assert_eq!(reply, b"\x1b]11;rgb:ffff/ffff/ffff\x07");
    }

    #[test]
    fn answers_color_scheme_mode_enable_for_dark_background() {
        // OpenCode enables DEC mode 2031 to get in-band dark/light reports; a
        // dark background must report scheme 1.
        let mut responder = OscColorResponder::new(bg([14, 18, 24]));
        assert_eq!(responder.process(b"\x1b[?2031h"), b"\x1b[?997;1n");
    }

    #[test]
    fn leaves_color_scheme_unanswered_for_light_background() {
        // Light `997;2` replies force Claude's fullscreen renderer into a path
        // that fails under xterm.js; OSC 10/11 still answer for Codex.
        let mut responder = OscColorResponder::new(bg([255, 255, 255]));
        assert!(responder.process(b"\x1b[?2031h").is_empty());
        assert!(responder.process(b"\x1b[?996n").is_empty());
    }

    #[test]
    fn answers_color_scheme_query_for_dark_background() {
        let mut responder = OscColorResponder::new(bg([14, 18, 24]));
        assert_eq!(responder.process(b"\x1b[?996n"), b"\x1b[?997;1n");
    }

    #[test]
    fn recognizes_color_scheme_enable_split_across_chunks() {
        let mut responder = OscColorResponder::new(bg([14, 18, 24]));
        assert!(responder.process(b"\x1b[?20").is_empty());
        assert_eq!(responder.process(b"31h"), b"\x1b[?997;1n");
    }

    #[test]
    fn ignores_unrelated_private_modes() {
        let mut responder = OscColorResponder::new(bg([255, 255, 255]));
        // hide cursor, alt screen, bracketed paste, focus reporting.
        assert!(responder.process(b"\x1b[?25l").is_empty());
        assert!(responder.process(b"\x1b[?1049h").is_empty());
        assert!(responder.process(b"\x1b[?2004h").is_empty());
        assert!(responder.process(b"\x1b[?1004h").is_empty());
    }

    #[test]
    fn does_not_answer_color_scheme_without_a_configured_background() {
        let mut responder = OscColorResponder::new(OscColors {
            foreground: Some([1, 2, 3]),
            ..OscColors::default()
        });
        assert!(responder.process(b"\x1b[?2031h").is_empty());
    }

    #[test]
    fn parse_rgb_triplet_accepts_valid_and_rejects_malformed() {
        assert_eq!(parse_rgb_triplet("255,255,255"), Some([255, 255, 255]));
        assert_eq!(parse_rgb_triplet(" 14 , 18 , 24 "), Some([14, 18, 24]));
        assert_eq!(parse_rgb_triplet("256,0,0"), None);
        assert_eq!(parse_rgb_triplet("1,2"), None);
        assert_eq!(parse_rgb_triplet("1,2,3,4"), None);
        assert_eq!(parse_rgb_triplet("x,y,z"), None);
    }

    #[test]
    fn recognizes_only_exact_reserved_osc_environment_keys() {
        for key in [OSC_BACKGROUND_ENV, OSC_FOREGROUND_ENV, OSC_CURSOR_ENV] {
            assert!(is_reserved_osc_env_key(key));
            assert!(!is_reserved_osc_env_key(&key.to_ascii_lowercase()));
        }
        assert!(!is_reserved_osc_env_key(
            "CODDY_STUDIO_WINDOWS_CONSOLE_THEME"
        ));
    }
}
