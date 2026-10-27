//! The open-URL hook. Agents and terminals get `BROWSER=coddy-studio-open-url` and a `PATH` entry
//! holding that shim; the shim runs `<exe> open-url <url>`, which hands the URL to the Coddy Studio
//! client showing the thread through `POST /api/preview/open-url`.

use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fmt, io,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    time::{Duration, SystemTime},
};

use crate::production::connect_mcp::ConnectMcpService;

pub const SHIM_NAME: &str = "coddy-studio-open-url";
/// Not `*_TOKEN`: Codex's default `shell_environment_policy` drops variables whose names
/// contain `KEY`, `SECRET`, or `TOKEN` from the commands it runs.
pub const TOKEN_ENV: &str = "CODDY_STUDIO_OPEN_URL_AUTH";
pub const ENDPOINT_ENV: &str = "CODDY_STUDIO_OPEN_URL_ENDPOINT";
const POST_TIMEOUT: Duration = Duration::from_secs(5);
const PATH_SEPARATOR: char = if cfg!(windows) { ';' } else { ':' };

/// Puts `coddy-studio-open-url` into `dir`, running `exe`. POSIX gets a sh shim (mode 0755)
/// that runs `exe open-url "$1"`. Windows gets `coddy-studio-open-url.exe`, an alias of `exe`
/// that [`open_url_invocation`] recognizes by name: a batch shim would hand the URL to the
/// `cmd.exe` parser, and callers that skip the shell never find a `.cmd` anyway. Shims that
/// are already current are left alone.
pub fn write_shims(dir: &Path, exe: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let written = if cfg!(windows) {
        write_alias(&dir.join(format!("{SHIM_NAME}.exe")), exe)
    } else {
        exe.to_str()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("executable path is not UTF-8: {}", exe.display()),
                )
            })
            .and_then(|exe| write_if_changed(&dir.join(SHIM_NAME), &posix_shim(exe)))
    };
    // An earlier build wrote a batch shim everywhere, and the sh shim on Windows too, where
    // MSYS/Git Bash would find it before the alias. Never leave either on `PATH`.
    let stale: &[String] = if cfg!(windows) {
        &[format!("{SHIM_NAME}.cmd"), SHIM_NAME.to_owned()]
    } else {
        &[format!("{SHIM_NAME}.cmd")]
    };
    stale.iter().fold(written, |result, name| {
        let removed = match std::fs::remove_file(dir.join(name)) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        };
        result.and(removed)
    })
}

fn posix_shim(exe: &str) -> String {
    // Single quotes keep every byte of the path literal; an embedded quote closes, escapes, reopens.
    let exe = format!("'{}'", exe.replace('\'', r"'\''"));
    format!(
        "#!/bin/sh\n# Coddy Studio open-url shim: hands one URL to the Coddy Studio client showing this thread.\nexec {exe} open-url \"$1\"\n"
    )
}

/// Makes `alias` a hard link to `exe`, or a copy when linking fails (another volume), via a
/// temporary file and a rename. A current alias is kept without reading the large binary.
fn write_alias(alias: &Path, exe: &Path) -> io::Result<()> {
    let source = std::fs::metadata(exe)?;
    if let Ok(existing) = std::fs::metadata(alias)
        && let (Ok(source_modified), Ok(alias_modified)) = (source.modified(), existing.modified())
        && alias_is_current(
            (source.len(), source_modified),
            (existing.len(), alias_modified),
        )
    {
        return Ok(());
    }
    let name = alias.file_name().unwrap_or_default().to_string_lossy();
    let temporary = alias.with_file_name(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
    let result = std::fs::hard_link(exe, &temporary)
        .or_else(|_| {
            std::fs::copy(exe, &temporary)?;
            // A copy carries the source's modified time, so freshness stays an equality.
            std::fs::File::options()
                .write(true)
                .open(&temporary)?
                .set_modified(source.modified()?)
        })
        .and_then(|()| std::fs::rename(&temporary, alias));
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// An alias with its source's length and modified time is that source: a hard link shares the
/// source's metadata, and a copy is stamped with it. Equality, not "no older", so rolling back
/// to an older build of the same size still replaces the alias.
fn alias_is_current(source: (u64, SystemTime), alias: (u64, SystemTime)) -> bool {
    source == alias
}

/// [`open_url_invocation`] for this process: its arguments and its executable's name.
#[must_use]
pub fn current_open_url_invocation() -> Option<String> {
    let args = std::env::args_os().collect::<Vec<_>>();
    let exe = std::env::current_exe().ok();
    open_url_invocation(&args, exe.as_deref().and_then(Path::file_stem))
}

/// The URL to open when this process is an open-url invocation, checked before anything
/// else starts: `<exe> open-url <url>` (the POSIX shim), or the Windows alias
/// `coddy-studio-open-url[.exe] <url>`, recognized by `argv[0]` or by `exe_stem`, the running
/// executable's file stem (argv[0] is whatever the caller chose). A missing URL yields `""`,
/// which [`run_open_url`] rejects with exit 2.
#[must_use]
pub fn open_url_invocation(args: &[OsString], exe_stem: Option<&OsStr>) -> Option<String> {
    let is_alias = |stem: &OsStr| stem.eq_ignore_ascii_case(SHIM_NAME);
    let invoked_as_alias = args
        .first()
        .and_then(|program| Path::new(program).file_stem())
        .is_some_and(is_alias)
        || exe_stem.is_some_and(is_alias);
    let url = if invoked_as_alias {
        args.get(1)
    } else if args.get(1).is_some_and(|command| command == "open-url") {
        args.get(2)
    } else {
        return None;
    };
    Some(
        url.map(|url| url.to_string_lossy().into_owned())
            .unwrap_or_default(),
    )
}

fn write_if_changed(path: &Path, contents: &str) -> io::Result<()> {
    if std::fs::read(path).is_ok_and(|current| current == contents.as_bytes()) {
        return set_executable(path);
    }
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let temporary = path.with_file_name(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
    let result = std::fs::write(&temporary, contents)
        .and_then(|()| set_executable(&temporary))
        .and_then(|()| std::fs::rename(&temporary, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(unix)]
fn set_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// The variables a provider or terminal session gets: the credential, the route, the two
/// browser hooks, and `PATH` with the shim directory first.
#[must_use]
pub fn session_env(
    endpoint: &str,
    token: &str,
    shim_dir: &Path,
    base_path: Option<&OsStr>,
) -> BTreeMap<String, String> {
    let shim_dir = shim_dir.to_string_lossy();
    let path = match base_path.filter(|base| !base.is_empty()) {
        Some(base) => format!("{shim_dir}{PATH_SEPARATOR}{}", base.to_string_lossy()),
        None => shim_dir.into_owned(),
    };
    BTreeMap::from([
        (TOKEN_ENV.to_owned(), token.to_owned()),
        (ENDPOINT_ENV.to_owned(), endpoint.to_owned()),
        ("BROWSER".to_owned(), SHIM_NAME.to_owned()),
        ("BRAINSTORM_OPEN_CMD".to_owned(), SHIM_NAME.to_owned()),
        ("PATH".to_owned(), path),
    ])
}

/// One issued open-url credential, ready to merge into a launch environment.
#[derive(Clone, Eq, PartialEq)]
pub struct OpenUrlSession {
    pub endpoint: String,
    pub token: String,
    pub shim_dir: PathBuf,
}

impl fmt::Debug for OpenUrlSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenUrlSession")
            .field("endpoint", &self.endpoint)
            .field("token", &"[redacted]")
            .field("shim_dir", &self.shim_dir)
            .finish()
    }
}

impl OpenUrlSession {
    /// Merges the session variables into `env`; they win over the caller's values. `PATH`
    /// prefixes the caller's own `PATH`, else the server's.
    pub fn apply(&self, env: &mut BTreeMap<String, String>) {
        let base_path = env
            .iter()
            .find(|(key, _)| same_env_key(key, "PATH"))
            .map(|(_, value)| OsString::from(value))
            .or_else(|| std::env::var_os("PATH"));
        for (key, value) in session_env(
            &self.endpoint,
            &self.token,
            &self.shim_dir,
            base_path.as_deref(),
        ) {
            env.retain(|existing, _| !same_env_key(existing, &key));
            env.insert(key, value);
        }
    }
}

/// Windows keys ignore case. `PATH` ignores case everywhere, as provider environment
/// normalization and PTY executable lookup already treat it.
fn same_env_key(left: &str, right: &str) -> bool {
    left == right || ((cfg!(windows) || right == "PATH") && left.eq_ignore_ascii_case(right))
}

/// Issues open-url sessions once the server's credential store is attached. Until then (and
/// in servers without one) sessions launch without the hook.
#[derive(Clone)]
pub struct OpenUrlEnvironment {
    shim_dir: PathBuf,
    connect: Arc<OnceLock<Arc<ConnectMcpService>>>,
}

impl fmt::Debug for OpenUrlEnvironment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenUrlEnvironment")
            .field("shim_dir", &self.shim_dir)
            .field("bound", &self.connect.get().is_some())
            .finish()
    }
}

impl OpenUrlEnvironment {
    #[must_use]
    pub fn new(shim_dir: PathBuf) -> Self {
        Self {
            shim_dir,
            connect: Arc::new(OnceLock::new()),
        }
    }

    pub fn bind(&self, connect: Arc<ConnectMcpService>) {
        let _ = self.connect.set(connect);
    }

    /// A fresh credential for `thread_id`. Issuing never revokes another holder's token, so a
    /// provider and each terminal of one thread hold their own.
    pub async fn issue(&self, thread_id: &str) -> Option<OpenUrlSession> {
        let connect = self.connect.get()?;
        match connect.issue_open_url_credential(thread_id).await {
            Ok(credential) => Some(OpenUrlSession {
                endpoint: connect.open_url_endpoint().to_owned(),
                token: credential.token,
                shim_dir: self.shim_dir.clone(),
            }),
            Err(error) => {
                tracing::warn!(?error, thread_id, "could not issue an open-url credential");
                None
            }
        }
    }
}

/// `coddy-studio open-url <url>`: asks the Coddy Studio client showing this session's thread to open
/// `url`. Returns the process exit code. A tool calling `$BROWSER` must never fail because
/// Coddy Studio is unreachable, so every failure after validation prints the URL and exits 0, and
/// so does a request no client could see (or one refused because too many are pending).
pub async fn run_open_url(url: &str) -> i32 {
    let endpoint = std::env::var(ENDPOINT_ENV).ok();
    let token = std::env::var(TOKEN_ENV).ok();
    open_url(url, endpoint.as_deref(), token.as_deref()).await
}

async fn open_url(url: &str, endpoint: Option<&str>, token: Option<&str>) -> i32 {
    if !url::Url::parse(url).is_ok_and(|parsed| matches!(parsed.scheme(), "http" | "https")) {
        eprintln!("coddy-studio open-url: expected an http(s) URL");
        return 2;
    }
    let (Some(endpoint), Some(token)) = (
        endpoint.filter(|value| !value.is_empty()),
        token.filter(|value| !value.is_empty()),
    ) else {
        println!("{url}");
        return 0;
    };
    match post_open_request(url, endpoint, token).await {
        Ok(true) => {}
        Ok(false) => println!("{url}"),
        Err(error) => {
            println!("{url}");
            eprintln!("coddy-studio open-url: {error}");
        }
    }
    0
}

/// Whether a Coddy Studio client received the request.
async fn post_open_request(url: &str, endpoint: &str, token: &str) -> Result<bool, String> {
    // The endpoint is this machine's own server; a user's HTTP proxy must not intercept it.
    let client = reqwest::Client::builder()
        .timeout(POST_TIMEOUT)
        .no_proxy()
        .build()
        .map_err(|error| error.to_string())?;
    let response = client
        .post(endpoint)
        .bearer_auth(token)
        .json(&serde_json::json!({ "url": url }))
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if response.status() != reqwest::StatusCode::ACCEPTED {
        return Err(format!("Coddy Studio answered {}", response.status()));
    }
    let body: serde_json::Value = response.json().await.map_err(|error| error.to_string())?;
    Ok(body["delivered"].as_bool().unwrap_or(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn shim_passes_metacharacters_as_one_argument() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().join("argv");
        // A quote and a space in the executable path prove the shim's own quoting.
        let exe = temp.path().join("it's the exe");
        crate::test_support::TestSandbox::write_executable(
            &exe,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$#\" > '{}'\nfor arg in \"$@\"; do printf '%s\\0' \"$arg\" >> '{}'; done\n",
                output.display(),
                output.display()
            ),
        );
        let shims = temp.path().join("shims");
        write_shims(&shims, &exe).unwrap();
        let shim = shims.join(SHIM_NAME);
        assert_eq!(
            std::fs::metadata(&shim).unwrap().permissions().mode() & 0o777,
            0o755
        );
        let url = "http://h/?a=$(x)&b=`y`;c' \"d\" $HOME";

        // `sh <shim>` reads the script instead of exec'ing it, so a sibling test's fork can
        // never hold the freshly written shim busy (ETXTBSY).
        let status = std::process::Command::new("/bin/sh")
            .arg(&shim)
            .arg(url)
            .status()
            .unwrap();

        assert!(status.success());
        let recorded = std::fs::read(&output).unwrap();
        let (count, args) = recorded.split_at(recorded.iter().position(|b| *b == b'\n').unwrap());
        assert_eq!(count, b"2");
        let args = args[1..]
            .split(|b| *b == 0)
            .filter(|arg| !arg.is_empty())
            .map(|arg| String::from_utf8(arg.to_vec()).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(args, vec!["open-url".to_owned(), url.to_owned()]);
    }

    #[cfg(unix)]
    #[test]
    fn posix_shim_is_rewritten_only_when_its_content_changes() {
        let temp = tempfile::tempdir().unwrap();
        // A batch shim left by an earlier build is removed.
        std::fs::write(temp.path().join(format!("{SHIM_NAME}.cmd")), "@echo off").unwrap();
        write_shims(temp.path(), Path::new("/opt/coddy-studio/coddy-studio")).unwrap();
        let shim = temp.path().join(SHIM_NAME);
        let first = std::fs::metadata(&shim).unwrap().modified().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        write_shims(temp.path(), Path::new("/opt/coddy-studio/coddy-studio")).unwrap();
        assert_eq!(std::fs::metadata(&shim).unwrap().modified().unwrap(), first);

        write_shims(temp.path(), Path::new("/opt/100%/coddy-studio")).unwrap();
        assert_eq!(
            std::fs::read_to_string(&shim).unwrap(),
            "#!/bin/sh\n# Coddy Studio open-url shim: hands one URL to the Coddy Studio client showing this thread.\nexec '/opt/100%/coddy-studio' open-url \"$1\"\n"
        );
        // Only the shim remains: no batch shim, no temporary file.
        let names = std::fs::read_dir(temp.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(names, vec![OsString::from(SHIM_NAME)]);
    }

    #[cfg(windows)]
    #[test]
    fn windows_gets_an_exe_alias_and_no_batch_shim() {
        let temp = tempfile::tempdir().unwrap();
        let exe = temp.path().join("coddy-studio.exe");
        std::fs::write(&exe, b"not really an executable").unwrap();
        let shims = temp.path().join("shims");
        std::fs::create_dir(&shims).unwrap();
        std::fs::write(shims.join(format!("{SHIM_NAME}.cmd")), "@echo off").unwrap();
        // The sh shim an earlier build wrote here too; Git Bash would prefer it.
        std::fs::write(shims.join(SHIM_NAME), "#!/bin/sh\n").unwrap();

        write_shims(&shims, &exe).unwrap();

        assert_eq!(
            std::fs::read(shims.join(format!("{SHIM_NAME}.exe"))).unwrap(),
            b"not really an executable"
        );
        assert!(!shims.join(format!("{SHIM_NAME}.cmd")).exists());
        assert!(!shims.join(SHIM_NAME).exists());
    }

    #[test]
    fn alias_is_written_atomically_and_replaced_only_when_stale() {
        let temp = tempfile::tempdir().unwrap();
        let exe = temp.path().join("coddy-studio");
        std::fs::write(&exe, b"version one").unwrap();
        let shims = temp.path().join("shims");
        std::fs::create_dir(&shims).unwrap();
        let alias = shims.join(format!("{SHIM_NAME}.exe"));

        write_alias(&alias, &exe).unwrap();
        assert_eq!(std::fs::read(&alias).unwrap(), b"version one");
        // A current alias is kept without a byte compare. Rewriting the hard link would
        // change the source too, so a separate file with the source's length and modified
        // time stands in for it.
        let source_modified = std::fs::metadata(&exe).unwrap().modified().unwrap();
        stand_in_with_time(&alias, source_modified);
        write_alias(&alias, &exe).unwrap();
        assert_eq!(std::fs::read(&alias).unwrap(), b"version 1!!");

        // A same-size source with another modified time (a rollback) replaces it.
        stand_in_with_time(&alias, source_modified + Duration::from_secs(60));
        write_alias(&alias, &exe).unwrap();
        assert_eq!(std::fs::read(&alias).unwrap(), b"version one");

        // A new source of another length replaces it; no temporary file survives.
        std::fs::remove_file(&exe).unwrap();
        std::fs::write(&exe, b"version two, longer").unwrap();
        write_alias(&alias, &exe).unwrap();
        assert_eq!(std::fs::read(&alias).unwrap(), b"version two, longer");
        assert_eq!(std::fs::read_dir(&shims).unwrap().count(), 1);
    }

    /// Replaces `alias` with an unrelated file of the same length, stamped `modified`.
    fn stand_in_with_time(alias: &Path, modified: SystemTime) {
        std::fs::remove_file(alias).unwrap();
        std::fs::write(alias, b"version 1!!").unwrap();
        std::fs::File::options()
            .write(true)
            .open(alias)
            .unwrap()
            .set_modified(modified)
            .unwrap();
    }

    #[test]
    fn alias_freshness_compares_length_and_modified_time() {
        let earlier = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let later = earlier + Duration::from_secs(1);
        // A hard link shares the source's metadata; a copy is stamped with it.
        assert!(alias_is_current((10, earlier), (10, earlier)));
        // A rebuilt source, or a rollback to an older build of the same size.
        assert!(!alias_is_current((10, later), (10, earlier)));
        assert!(!alias_is_current((10, earlier), (10, later)));
        assert!(!alias_is_current((10, earlier), (11, earlier)));
    }

    #[test]
    fn open_url_invocation_recognizes_the_subcommand_and_the_alias() {
        let args = |values: &[&str]| values.iter().map(OsString::from).collect::<Vec<_>>();
        let url = "http://h/?a=$(x)&b=\"c\"";
        assert_eq!(
            open_url_invocation(
                &args(&["/opt/coddy-studio/coddy-studio", "open-url", url]),
                None
            ),
            Some(url.to_owned())
        );
        for alias in [
            "coddy-studio-open-url",
            "/state/runtime/open-url/coddy-studio-open-url.exe",
            "/state/runtime/open-url/CODDY_STUDIO-OPEN-URL.EXE",
        ] {
            assert_eq!(
                open_url_invocation(&args(&[alias, url]), None),
                Some(url.to_owned()),
                "{alias}"
            );
        }
        // A missing URL still runs open-url, which exits 2 with its usage message.
        assert_eq!(
            open_url_invocation(&args(&["coddy-studio-open-url.exe"]), None),
            Some(String::new())
        );
        assert_eq!(
            open_url_invocation(&args(&["coddy-studio", "open-url"]), None),
            Some(String::new())
        );
        // Anything else starts the program normally.
        assert_eq!(
            open_url_invocation(&args(&["coddy-studio", "serve"]), None),
            None
        );
        assert_eq!(
            open_url_invocation(&args(&["coddy-studio-desktop"]), None),
            None
        );
        assert_eq!(
            open_url_invocation(
                &args(&["coddy-studio-desktop", "coddy-studio://pair?code=x"]),
                None
            ),
            None
        );
        assert_eq!(open_url_invocation(&[], None), None);

        // The running executable's name counts too: a caller may pass any argv[0].
        let alias_stem = Some(OsStr::new("Coddy-Studio-Open-Url"));
        assert_eq!(
            open_url_invocation(&args(&["browser", url]), alias_stem),
            Some(url.to_owned())
        );
        assert_eq!(
            open_url_invocation(&args(&["browser"]), alias_stem),
            Some(String::new())
        );
        assert_eq!(
            open_url_invocation(
                &args(&["coddy-studio", "serve"]),
                Some(OsStr::new("coddy-studio"))
            ),
            None
        );
    }

    #[test]
    fn session_env_prefixes_path_and_sets_browser() {
        let shim_dir = Path::new("/state/runtime/open-url");
        let env = session_env(
            "http://127.0.0.1:3773/api/preview/open-url",
            "token-1",
            shim_dir,
            Some(OsStr::new("/usr/bin")),
        );
        let separator = PATH_SEPARATOR;
        assert_eq!(
            env,
            BTreeMap::from([
                (
                    "CODDY_STUDIO_OPEN_URL_AUTH".to_owned(),
                    "token-1".to_owned()
                ),
                (
                    "CODDY_STUDIO_OPEN_URL_ENDPOINT".to_owned(),
                    "http://127.0.0.1:3773/api/preview/open-url".to_owned()
                ),
                ("BROWSER".to_owned(), "coddy-studio-open-url".to_owned()),
                (
                    "BRAINSTORM_OPEN_CMD".to_owned(),
                    "coddy-studio-open-url".to_owned()
                ),
                (
                    "PATH".to_owned(),
                    format!("/state/runtime/open-url{separator}/usr/bin")
                ),
            ])
        );
        assert_eq!(
            session_env("e", "t", shim_dir, None)["PATH"],
            "/state/runtime/open-url"
        );
    }

    #[test]
    fn session_apply_wins_over_the_callers_values_and_keeps_its_path() {
        let session = OpenUrlSession {
            endpoint: "http://127.0.0.1:1/api/preview/open-url".to_owned(),
            token: "secret-token-value".to_owned(),
            shim_dir: PathBuf::from("/shims"),
        };
        let mut env = BTreeMap::from([
            ("BROWSER".to_owned(), "firefox".to_owned()),
            ("Path".to_owned(), "/custom/bin".to_owned()),
            ("KEEP".to_owned(), "1".to_owned()),
        ]);
        session.apply(&mut env);
        assert_eq!(env["BROWSER"], "coddy-studio-open-url");
        assert_eq!(env["PATH"], format!("/shims{PATH_SEPARATOR}/custom/bin"));
        assert!(!env.contains_key("Path"));
        assert_eq!(env["KEEP"], "1");
        assert_eq!(env[TOKEN_ENV], "secret-token-value");
        assert!(!format!("{session:?}").contains("secret-token-value"));
    }

    #[tokio::test]
    async fn run_open_url_rejects_non_http() {
        for url in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "",
            "localhost:5173",
        ] {
            assert_eq!(run_open_url(url).await, 2, "{url}");
        }
    }

    #[tokio::test]
    async fn open_url_without_a_session_or_server_still_succeeds() {
        assert_eq!(open_url("http://localhost:5173/", None, None).await, 0);
        // Nothing listens on port 9 of loopback: the failure is reported, not returned.
        assert_eq!(
            open_url(
                "http://localhost:5173/",
                Some("http://127.0.0.1:9/api/preview/open-url"),
                Some("token")
            )
            .await,
            0
        );
    }
}
