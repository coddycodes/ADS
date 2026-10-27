//! Desktop-managed SSH environments end to end, with only the SSH hop faked.
//!
//! `SshEnvironmentManager` and its tunnel cache, the production launch, stop
//! and pairing scripts, a real `coddy-studio serve` / `coddy-studio pairing issue`, and
//! the production `/oauth/token` exchange run unchanged. The stand-in client
//! (`fixtures/ssh/fake_ssh.py`, Python 3) runs remote commands locally with
//! OpenSSH semantics: the remote argv is joined with spaces and run by
//! `/bin/sh -c`, as `ssh(1)` and `sshd(8)` specify. It never contacts a host.
//! By default it execs into the remote shell; `Harness::with_sshd_sessions`
//! makes it run each remote command in its own session and only relay stdio,
//! as sshd does without a PTY, so killing the client leaves the remote command
//! running and a remote leak is visible.
//!
//! Every test is `#[ignore]`d because it needs a fresh `coddy-studio`, which
//! `cargo test -p coddy-studio-desktop` neither builds nor checks:
//!
//! ```sh
//! cargo test -p coddy-studio-server --test cli_smoke --no-run -j 2
//! cargo test -p coddy-studio-desktop --test ssh_environment -- --ignored
//! ```
//!
//! Scenarios run one at a time (`SCENARIO_LOCK`), so the remote servers
//! never race for a port. `CODDY_STUDIO_SSH_FIXTURE_CODDY_STUDIO` overrides the binary (default
//! `target/debug/coddy-studio`); `CODDY_STUDIO_SSH_FIXTURE_PORT_START` overrides the
//! first remote port the fake remote scans (default 47310, away from 3773).
#![cfg(unix)]

use std::{
    collections::BTreeSet,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use coddy_studio_desktop_lib::{
    desktop_bridge_bootstrap_ssh_bearer_session,
    ssh::{
        SshEnvironmentBootstrap, SshEnvironmentEnsureOptions, SshEnvironmentManager,
        SshEnvironmentTarget, SshOperationDeadlines, SshPasswordPromptManager,
    },
};
use serde_json::Value;
use tauri::{
    App,
    test::{MockRuntime, mock_builder, mock_context, noop_assets},
};

#[path = "../../../server/tests/support/hermetic_providers.rs"]
mod hermetic_providers;

const STEP_TIMEOUT: Duration = Duration::from_secs(90);
/// Serializes scenarios across test threads; each owns real remote servers.
static SCENARIO_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
/// `AuthStandardClientScopes` in `packages/contracts/src/auth.ts`.
const STANDARD_SCOPES: [&str; 5] = [
    "orchestration:read",
    "orchestration:operate",
    "terminal:operate",
    "review:write",
    "relay:read",
];

const DEFAULT_PORT_START: u16 = 47310;
/// The fake spreads aliases over 40 x 5 ports and the launch script scans
/// 200 more, so the start must leave that much room below 65535.
const PORT_START_RANGE: std::ops::RangeInclusive<u16> = 1024..=65000;

/// Parses `CODDY_STUDIO_SSH_FIXTURE_PORT_START`; it is interpolated into the fake
/// ssh wrapper, so only a plain port number in range is accepted.
fn parse_port_start(value: Option<&str>) -> Result<u16, String> {
    let Some(value) = value else {
        return Ok(DEFAULT_PORT_START);
    };
    value
        .parse::<u16>()
        .ok()
        .filter(|port| PORT_START_RANGE.contains(port))
        .ok_or_else(|| {
            format!(
                "CODDY_STUDIO_SSH_FIXTURE_PORT_START must be a port from {} to {}; got {value:?}",
                PORT_START_RANGE.start(),
                PORT_START_RANGE.end()
            )
        })
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ssh")
}

fn real_coddy_studio() -> PathBuf {
    let coddy_studio = std::env::var_os("CODDY_STUDIO_SSH_FIXTURE_CODDY_STUDIO")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let target = std::env::var_os("CARGO_TARGET_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../target"));
            target.join("debug/coddy-studio")
        });
    assert!(
        coddy_studio.is_file(),
        "{} is missing; run `cargo build -p coddy-studio-server --bin coddy-studio` first",
        coddy_studio.display()
    );
    coddy_studio
}

fn standard_scopes() -> Vec<String> {
    STANDARD_SCOPES
        .iter()
        .map(|scope| scope.to_string())
        .collect()
}

fn issue_token() -> Option<SshEnvironmentEnsureOptions> {
    Some(SshEnvironmentEnsureOptions {
        issue_pairing_token: Some(true),
    })
}

/// Runs `ps -o <field>= -p <pid>`, portable across Linux and macOS.
fn ps_field(pid: u32, field: &str) -> String {
    Command::new("ps")
        .args(["-o", &format!("{field}="), "-p", &pid.to_string()])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_default()
}

fn process_is_running(pid: u32) -> bool {
    let state = ps_field(pid, "stat");
    !state.is_empty() && !state.starts_with('Z')
}

async fn wait_until_exited(pid: u32) -> bool {
    wait_until_exited_within(pid, Duration::from_secs(10)).await
}

async fn wait_until_exited_within(pid: u32, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if !process_is_running(pid) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

struct Harness {
    _scenario: tokio::sync::MutexGuard<'static, ()>,
    alias: &'static str,
    state: PathBuf,
    app: App<MockRuntime>,
    prompts: SshPasswordPromptManager,
    ssh_program: PathBuf,
    // Dropped last, after `Drop` has stopped every process started here.
    _root: tempfile::TempDir,
}

impl Harness {
    async fn new(alias: &'static str) -> Self {
        Self::create(alias, false).await
    }

    /// A harness whose fake ssh runs remote commands in their own sessions,
    /// as sshd does without a PTY: killing the client leaves them running.
    async fn with_sshd_sessions(alias: &'static str) -> Self {
        Self::create(alias, true).await
    }

    async fn create(alias: &'static str, sshd_sessions: bool) -> Self {
        let scenario = SCENARIO_LOCK.lock().await;
        let coddy_studio = real_coddy_studio();
        let root = tempfile::Builder::new()
            .prefix("coddy-studio-ssh-environment-")
            .tempdir()
            .expect("fixture root");
        let state = root.path().join(alias);
        fs::create_dir_all(&state).expect("fixture state directory");
        hermetic_providers::write_hermetic_settings(
            &state.join("remote-home/.coddy/studio/userdata"),
            serde_json::json!({ "providers": {
                "codex": { "enabled": false },
                "claudeAgent": { "enabled": false },
                "cursor": { "enabled": false },
                "grok": { "enabled": false },
                "opencode": { "enabled": false }
            }}),
        );
        let port_start = parse_port_start(
            std::env::var("CODDY_STUDIO_SSH_FIXTURE_PORT_START")
                .ok()
                .as_deref(),
        )
        .unwrap_or_else(|error| panic!("{error}"));
        let ssh_program = root.path().join("ssh");
        let quote =
            |path: &Path| format!("'{}'", path.display().to_string().replace('\'', r"'\''"));
        fs::write(
            &ssh_program,
            format!(
                "#!/bin/sh\n\
                 SSH_FIXTURE_ROOT={root} SSH_FIXTURE_CODDY_STUDIO={coddy_studio} \
                 SSH_FIXTURE_REMOTE_BIN={remote_bin} SSH_FIXTURE_REMOTE_PORT_START={port_start} \
                 SSH_FIXTURE_SSHD_SESSIONS={sshd_sessions} \
                 exec python3 {fake} \"$@\"\n",
                root = quote(root.path()),
                coddy_studio = quote(&coddy_studio),
                remote_bin = quote(&fixtures_dir().join("remote-bin")),
                sshd_sessions = u8::from(sshd_sessions),
                fake = quote(&fixtures_dir().join("fake_ssh.py")),
            ),
        )
        .expect("write fake ssh wrapper");
        fs::set_permissions(&ssh_program, fs::Permissions::from_mode(0o755))
            .expect("chmod fake ssh wrapper");
        let app = mock_builder()
            .build(mock_context(noop_assets()))
            .expect("mock Tauri app");
        Self {
            _scenario: scenario,
            alias,
            state,
            app,
            prompts: SshPasswordPromptManager::with_timeout(Duration::from_millis(10)),
            ssh_program,
            _root: root,
        }
    }

    fn manager(&self, deadlines: SshOperationDeadlines) -> SshEnvironmentManager {
        SshEnvironmentManager::with_ssh_program(self.ssh_program.display().to_string(), deadlines)
    }

    fn target(&self) -> SshEnvironmentTarget {
        SshEnvironmentTarget {
            alias: self.alias.to_owned(),
            hostname: self.alias.to_owned(),
            username: None,
            port: None,
        }
    }

    async fn ensure(
        &self,
        manager: &SshEnvironmentManager,
        options: Option<SshEnvironmentEnsureOptions>,
    ) -> Result<SshEnvironmentBootstrap, String> {
        let result = tokio::time::timeout(
            STEP_TIMEOUT,
            manager.ensure_environment(self.app.handle(), &self.prompts, self.target(), options),
        )
        .await
        .unwrap_or_else(|_| Err(format!("ensure_environment exceeded {STEP_TIMEOUT:?}")));
        for entry in fs::read_dir(self.remote_home().join(".coddy/studio/ssh-launch"))
            .into_iter()
            .flatten()
            .flatten()
        {
            let path = entry.path().join("server.log");
            if let Ok(log) = fs::read_to_string(&path) {
                assert!(
                    !log.contains("hermetic-test-guard:"),
                    "remote CLI fixture must remain hermetic at {}:\n{log}",
                    path.display()
                );
            }
        }
        result
    }

    async fn disconnect(&self, manager: &SshEnvironmentManager) {
        let result = tokio::time::timeout(
            STEP_TIMEOUT,
            manager.disconnect_environment(self.app.handle(), &self.prompts, self.target()),
        )
        .await;
        eprintln!("[{}] disconnect_environment: {result:?}", self.alias);
    }

    fn invocations(&self) -> Vec<Value> {
        fs::read_to_string(self.state.join("ssh-invocations.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str::<Value>(line).expect("fake ssh invocation record"))
            .collect()
    }

    /// Kinds of SSH commands the manager has run so far, as logged by the fake.
    fn ssh_kinds(&self) -> Vec<String> {
        self.invocations()
            .iter()
            .map(|record| record["kind"].as_str().unwrap_or("?").to_owned())
            .collect()
    }

    fn remote_coddy_studio_log(&self) -> String {
        fs::read_to_string(self.state.join("remote-coddy-studio.log")).unwrap_or_default()
    }

    fn tunnel_pids(&self) -> Vec<u32> {
        self.json_pids_in("tunnels.jsonl")
    }

    fn remote_home(&self) -> PathBuf {
        self.state.join("remote-home")
    }

    fn lines_in(&self, file: &str) -> Vec<String> {
        fs::read_to_string(self.state.join(file))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn pid_lines_in(&self, file: &str) -> Vec<u32> {
        self.lines_in(file)
            .into_iter()
            .filter_map(|line| line.trim().parse().ok())
            .collect()
    }

    fn json_pids_in(&self, file: &str) -> Vec<u32> {
        self.lines_in(file)
            .into_iter()
            .filter_map(|line| serde_json::from_str::<Value>(&line).ok()?["pid"].as_u64())
            .filter_map(|pid| u32::try_from(pid).ok())
            .collect()
    }

    /// Pids of every hung remote `coddy-studio pairing` stand-in (`sleep 600`).
    fn hang_pids(&self) -> Vec<u32> {
        self.pid_lines_in("hang.pid")
    }

    /// Pids of the sshd-like remote sessions the fake started.
    fn remote_session_pids(&self) -> Vec<u32> {
        self.json_pids_in("remote-sessions.jsonl")
    }

    /// Pids of the managed remote servers, from the launch script's pid files.
    fn remote_server_pids(&self) -> Vec<u32> {
        fs::read_dir(self.remote_home().join(".coddy/studio/ssh-launch"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| fs::read_to_string(entry.path().join("pid")).ok())
            .filter_map(|pid| pid.trim().parse::<u32>().ok())
            .collect()
    }

    /// Sends SIGTERM to `pid` only if its command line names `marker`, so a
    /// recycled pid never belongs to someone else.
    fn terminate_owned(&self, pid: u32, marker: &str) -> bool {
        if !ps_field(pid, "command").contains(marker) {
            return false;
        }
        Command::new("kill")
            .arg(pid.to_string())
            .status()
            .is_ok_and(|status| status.success())
    }
}

impl Drop for Harness {
    /// Last-resort cleanup of processes this harness started, for panics
    /// before the explicit `disconnect_environment`: remote `coddy-studio serve`
    /// (pid files written by the launch script), fake tunnel forwarders, hung
    /// remote pairing stand-ins, and sshd-like remote sessions still running.
    fn drop(&mut self) {
        let remote_root = self.remote_home().display().to_string();
        for pid in self.remote_server_pids() {
            self.terminate_owned(pid, &remote_root);
        }
        for pid in self.tunnel_pids() {
            self.terminate_owned(pid, "fake_ssh.py");
        }
        for pid in self.hang_pids() {
            self.terminate_owned(pid, "sleep 600");
        }
        for pid in self.remote_session_pids() {
            self.terminate_owned(pid, "sh -s --");
        }
    }
}

/// The production token exchange with standard scopes. Returns the granted
/// scope set; the access token itself is never printed.
async fn exchange(
    bootstrap: &SshEnvironmentBootstrap,
    credential: &str,
) -> Result<BTreeSet<String>, String> {
    desktop_bridge_bootstrap_ssh_bearer_session(
        bootstrap.http_base_url.clone(),
        credential.to_owned(),
        Some(standard_scopes()),
    )
    .await
    .map(|body| {
        body["scope"]
            .as_str()
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_owned)
            .collect()
    })
}

fn standard_scope_set() -> BTreeSet<String> {
    STANDARD_SCOPES
        .iter()
        .map(|scope| scope.to_string())
        .collect()
}

#[test]
fn port_start_override_must_be_a_port_that_leaves_room_for_the_scan() {
    assert_eq!(parse_port_start(None), Ok(DEFAULT_PORT_START));
    assert_eq!(parse_port_start(Some("47310")), Ok(47310));
    for invalid in [
        "",
        "abc",
        "1; touch /tmp/pwned",
        "80",
        "65535",
        "70000",
        "-1",
    ] {
        let error = parse_port_start(Some(invalid)).expect_err(invalid);
        assert!(
            error.contains("CODDY_STUDIO_SSH_FIXTURE_PORT_START"),
            "{invalid}: {error}"
        );
    }
}

#[tokio::test]
#[ignore = "needs a guarded coddy-studio: cargo test -p coddy-studio-server --test cli_smoke --no-run"]
async fn add_then_reconnect_mints_fresh_tokens_with_standard_scopes() {
    let harness = Harness::new("sshd-add").await;
    let manager = harness.manager(SshOperationDeadlines::default());

    // Add: onboarding ensures with a token and exchanges it.
    let onboarding = harness
        .ensure(&manager, issue_token())
        .await
        .unwrap_or_else(|error| {
            panic!(
                "onboarding ensure: {error}\nremote coddy-studio:\n{}",
                harness.remote_coddy_studio_log()
            )
        });
    let first = onboarding.pairing_token.clone().expect("onboarding token");
    assert_eq!(harness.ssh_kinds(), ["launch", "tunnel", "pairing"]);
    assert!(
        harness.remote_coddy_studio_log().contains(&format!(
            "[pairing] [issue] [--base-dir] [{}/.coddy/studio] [--json]",
            harness.remote_home().display()
        )),
        "the pairing command must survive OpenSSH joining:\n{}",
        harness.remote_coddy_studio_log()
    );
    assert_eq!(
        exchange(&onboarding, &first).await,
        Ok(standard_scope_set()),
        "the SSH exchange must grant exactly the standard scopes"
    );

    // A reconnect with a saved bearer ensures the tunnel without a token and
    // runs no SSH command while the tunnel lives.
    let started = Instant::now();
    let reuse = harness
        .ensure(&manager, None)
        .await
        .expect("no-token ensure");
    eprintln!("[sshd-add] no-token ensure took {:?}", started.elapsed());
    assert_eq!(reuse.pairing_token, None);
    assert_eq!(reuse.http_base_url, onboarding.http_base_url);
    assert_eq!(harness.ssh_kinds().len(), 3, "{:?}", harness.ssh_kinds());

    // A mint on the live tunnel issues a new token instead of the consumed one.
    let minted = harness
        .ensure(&manager, issue_token())
        .await
        .expect("mint on the live tunnel");
    let second = minted.pairing_token.clone().expect("fresh token");
    assert_ne!(second, first);
    assert_eq!(
        harness.ssh_kinds(),
        ["launch", "tunnel", "pairing", "pairing"]
    );
    assert_eq!(exchange(&minted, &second).await, Ok(standard_scope_set()));

    // The one-time property the renderer relies on: a consumed token is refused.
    let reused = exchange(&minted, &first).await;
    assert!(
        reused
            .as_ref()
            .is_err_and(|error| error.starts_with("[ssh_http:401]")),
        "{reused:?}"
    );

    harness.disconnect(&manager).await;
}

#[tokio::test]
#[ignore = "needs a guarded coddy-studio: cargo test -p coddy-studio-server --test cli_smoke --no-run"]
async fn desktop_restart_mints_a_fresh_token() {
    let harness = Harness::new("sshd-restart").await;
    let before_restart = harness.manager(SshOperationDeadlines::default());
    let onboarding = harness
        .ensure(&before_restart, issue_token())
        .await
        .expect("onboarding ensure");
    let first = onboarding.pairing_token.clone().expect("onboarding token");
    exchange(&onboarding, &first)
        .await
        .expect("onboarding exchange");
    let old_tunnels = harness.tunnel_pids();

    // Quitting the desktop drops the in-memory tunnel map and its ssh child.
    drop(before_restart);
    for pid in old_tunnels {
        assert!(wait_until_exited(pid).await, "old tunnel {pid} must exit");
    }

    let after_restart = harness.manager(SshOperationDeadlines::default());
    let restarted = harness
        .ensure(&after_restart, issue_token())
        .await
        .expect("post-restart ensure");
    let token = restarted.pairing_token.clone().expect("post-restart token");
    assert_ne!(token, first);
    assert_eq!(exchange(&restarted, &token).await, Ok(standard_scope_set()));

    harness.disconnect(&after_restart).await;
}

#[tokio::test]
#[ignore = "needs a guarded coddy-studio: cargo test -p coddy-studio-server --test cli_smoke --no-run"]
async fn dead_tunnel_reconnects_and_mints_a_fresh_token() {
    let harness = Harness::new("sshd-deadtunnel").await;
    let manager = harness.manager(SshOperationDeadlines::default());
    let onboarding = harness
        .ensure(&manager, issue_token())
        .await
        .expect("onboarding ensure");
    let first = onboarding.pairing_token.clone().expect("onboarding token");
    exchange(&onboarding, &first)
        .await
        .expect("onboarding exchange");

    // The tunnel dies, as after ServerAliveInterval x ServerAliveCountMax.
    let tunnel = *harness.tunnel_pids().last().expect("a live tunnel");
    assert!(harness.terminate_owned(tunnel, "fake_ssh.py"));
    assert!(wait_until_exited(tunnel).await);

    let reconnected = harness
        .ensure(&manager, None)
        .await
        .expect("ensure after the tunnel died");
    assert_eq!(reconnected.pairing_token, None);
    assert_eq!(
        harness.ssh_kinds(),
        ["launch", "tunnel", "pairing", "launch", "tunnel"]
    );
    let minted = harness
        .ensure(&manager, issue_token())
        .await
        .expect("mint on the new tunnel");
    let token = minted.pairing_token.clone().expect("fresh token");
    assert_ne!(token, first);
    assert_eq!(exchange(&minted, &token).await, Ok(standard_scope_set()));

    harness.disconnect(&manager).await;
}

#[tokio::test]
#[ignore = "needs a guarded coddy-studio: cargo test -p coddy-studio-server --test cli_smoke --no-run"]
async fn remote_server_stopped_under_a_live_tunnel_is_relaunched() {
    let harness = Harness::new("sshd-relaunch").await;
    let manager = harness.manager(SshOperationDeadlines::default());
    harness
        .ensure(&manager, issue_token())
        .await
        .expect("onboarding ensure");
    let old_tunnel = *harness.tunnel_pids().last().expect("a live tunnel");
    let old_server = *harness
        .remote_server_pids()
        .first()
        .expect("a managed remote server");

    // The remote server stops (update, crash) while the SSH session lives.
    assert!(harness.terminate_owned(old_server, &harness.remote_home().display().to_string()));
    assert!(wait_until_exited(old_server).await);
    assert!(
        process_is_running(old_tunnel),
        "the tunnel must still be alive"
    );

    let relaunched = harness
        .ensure(&manager, None)
        .await
        .expect("ensure relaunches the remote server");
    assert_eq!(
        harness.ssh_kinds(),
        ["launch", "tunnel", "pairing", "launch", "tunnel"],
        "a failed probe takes the full launch-or-reuse path"
    );
    let new_server = *harness
        .remote_server_pids()
        .first()
        .expect("a relaunched remote server");
    assert_ne!(new_server, old_server);
    assert!(process_is_running(new_server));
    assert!(
        wait_until_exited(old_tunnel).await,
        "the stale tunnel is dropped"
    );
    assert_eq!(relaunched.pairing_token, None);

    let minted = harness
        .ensure(&manager, issue_token())
        .await
        .expect("mint on the relaunched server");
    let token = minted.pairing_token.clone().expect("fresh token");
    assert_eq!(exchange(&minted, &token).await, Ok(standard_scope_set()));

    harness.disconnect(&manager).await;
}

/// Real sshd does not signal a remote command without a PTY when the client
/// goes away, so a hung `coddy-studio pairing issue` would outlive the desktop's
/// deadline. The remote watchdog ends it within its bound
/// (`SshOperationDeadlines::pairing_watchdog_bound`) plus the TERM-to-KILL
/// grace; this allows 3 s past the bound.
#[tokio::test]
#[ignore = "needs a guarded coddy-studio: cargo test -p coddy-studio-server --test cli_smoke --no-run"]
async fn pairing_timeout_terminates_the_ssh_child_and_the_remote_command() {
    let harness = Harness::with_sshd_sessions("hang-pairing").await;
    let deadlines = SshOperationDeadlines {
        pairing: Duration::from_secs(3),
        ..SshOperationDeadlines::default()
    };
    let manager = harness.manager(deadlines);

    let started = Instant::now();
    let error = harness
        .ensure(&manager, issue_token())
        .await
        .expect_err("a hung pairing command must fail at its deadline");
    let elapsed = started.elapsed();

    assert!(
        error.starts_with("[ssh_timeout:pairing] The remote host did not issue a pairing credential within 3 seconds."),
        "{error}"
    );
    assert_eq!(harness.ssh_kinds(), ["launch", "tunnel", "pairing"]);
    let pairing_pid = harness
        .invocations()
        .iter()
        .find(|record| record["kind"] == "pairing")
        .and_then(|record| record["pid"].as_u64())
        .and_then(|pid| u32::try_from(pid).ok())
        .expect("pairing invocation pid");
    assert!(
        wait_until_exited(pairing_pid).await,
        "the pairing SSH child must be terminated and reaped"
    );
    eprintln!("[hang-pairing] failed after {elapsed:?}");

    // Asserted before `disconnect` and `Drop`, which would kill it anyway.
    let client_gone = Instant::now();
    let stand_ins = harness.hang_pids();
    assert_eq!(stand_ins.len(), 1, "one hung remote pairing command");
    let limit = deadlines.pairing_watchdog_bound() + Duration::from_secs(3);
    for pid in stand_ins {
        assert!(
            wait_until_exited_within(pid, limit).await,
            "the remote pairing command {pid} outlived the watchdog bound ({limit:?} after the client died)"
        );
    }
    eprintln!(
        "[hang-pairing] remote pairing command ended {:?} after the client",
        client_gone.elapsed()
    );

    harness.disconnect(&manager).await;
}

#[tokio::test]
#[ignore = "needs a guarded coddy-studio: cargo test -p coddy-studio-server --test cli_smoke --no-run"]
async fn timed_out_pairing_retries_leave_no_remote_command() {
    let harness = Harness::with_sshd_sessions("hang-pairing-retries").await;
    let deadlines = SshOperationDeadlines {
        pairing: Duration::from_secs(3),
        ..SshOperationDeadlines::default()
    };
    let manager = harness.manager(deadlines);

    for attempt in 1..=3 {
        let error = harness
            .ensure(&manager, issue_token())
            .await
            .expect_err("a hung pairing command must fail at its deadline");
        assert!(
            error.starts_with("[ssh_timeout:pairing] "),
            "attempt {attempt}: {error}"
        );
    }
    assert_eq!(
        harness.ssh_kinds(),
        [
            "launch", "tunnel", "pairing", "launch", "tunnel", "pairing", "launch", "tunnel",
            "pairing"
        ]
    );

    let last_client_gone = Instant::now();
    let stand_ins = harness.hang_pids();
    assert_eq!(
        stand_ins.len(),
        3,
        "one hung remote pairing command per attempt"
    );
    let limit = deadlines.pairing_watchdog_bound() + Duration::from_secs(3);
    for pid in stand_ins {
        assert!(
            wait_until_exited_within(pid, limit.saturating_sub(last_client_gone.elapsed())).await,
            "the remote pairing command {pid} outlived the watchdog bound"
        );
    }

    harness.disconnect(&manager).await;
}
