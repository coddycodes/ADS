use bibcode_server::process::{configure_background_command, isolate_appimage_environment};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    env, fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter, Runtime};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, oneshot, watch};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
};
use uuid::Uuid;

const SSH_DIRECTORY_NAME: &str = ".ssh";
const SSH_CONFIG_FILE_NAME: &str = "config";
const KNOWN_HOSTS_FILE_NAME: &str = "known_hosts";
pub const SSH_PASSWORD_PROMPT_EVENT: &str = "desktop:ssh-password-prompt";
const DEFAULT_SSH_PASSWORD_PROMPT_TIMEOUT: Duration = Duration::from_secs(3 * 60);
const DEFAULT_REMOTE_PORT: u16 = 3773;
const SSH_READY_PATH: &str = "/.well-known/bibcode/environment";
const SSH_READY_TIMEOUT: Duration = Duration::from_secs(30);
const SSH_READY_INTERVAL: Duration = Duration::from_millis(250);
const SSH_READY_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const SSH_TUNNEL_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(1500);
const SSH_FORWARD_READY_INTERVAL: Duration = Duration::from_millis(50);
/// Port forwards kept per tunnel. A forward beyond it evicts the tunnel's
/// least recently used one once it is ready.
const SSH_MAX_PORT_FORWARDS_PER_TUNNEL: usize = 8;
/// Port forwards kept across all tunnels, so that with tunnels and the
/// replacements being started they stay well under
/// `SSH_CHILD_REAPER_CAPACITY` and leave room for reconnect, stop, and
/// pairing. A forward beyond it evicts the least recently used one of any
/// tunnel once it is ready.
const SSH_PORT_FORWARD_BUDGET: usize = 12;
const SSH_CONNECTION_NOT_ACTIVE: &str = "SSH connection is not active.";
/// How long the pipes of an exited SSH child may stay idle before the rest of
/// its output is given up on. A descendant that inherited them (a
/// ProxyCommand helper, a ControlPersist master on older OpenSSH, a
/// backgrounded process) can hold them open long after ssh itself has exited,
/// so end of file never arrives. Output that keeps arriving keeps the drain
/// going; the remote script deadline bounds it as a whole.
const SSH_OUTPUT_DRAIN_GRACE: Duration = Duration::from_secs(2);
/// Longest wait for an exited tunnel's stderr, which no script deadline
/// covers.
const SSH_STDERR_COLLECT_LIMIT: Duration = Duration::from_secs(10);
/// Output kept per pipe (the tail, where ssh and the remote scripts put their
/// result and their final error).
const SSH_OUTPUT_CAP: usize = 64 * 1024;
const SSH_OUTPUT_CUT_OFF: &str = "[output cut off]";
const SSH_CHILD_REAPER_CAPACITY: usize = 32;
/// Blocking-pool threads of the SSH I/O runtime. On Windows every child pipe
/// read or write holds one while in flight; a child has three pipes, so this
/// covers every child the reaper admits, with room to spare.
const SSH_IO_MAX_BLOCKING_THREADS: usize = 4 * SSH_CHILD_REAPER_CAPACITY;
/// How long a drain that gave up keeps cancelling the pipe reads it left in
/// flight (Windows), and how long it polls each read between cancels.
const SSH_PIPE_SETTLE_LIMIT: Duration = Duration::from_secs(1);
const SSH_PIPE_SETTLE_INTERVAL: Duration = Duration::from_millis(10);
/// Whether a drain that gives up must cancel its unfinished pipe reads. Only
/// Windows reads child pipes on blocking-pool threads, which dropping a read
/// does not release; elsewhere reads use the I/O driver.
const SETTLE_PIPE_READS: bool = cfg!(windows);
const REMOTE_PORT_SCAN_WINDOW: u16 = 200;
/// How long a server the launch script started gets to answer, in seconds.
const REMOTE_READY_TIMEOUT_SECS: u64 = 15;
/// How long a live recorded server gets to answer before the launch script
/// replaces it, in seconds. Long enough that a busy server is not mistaken
/// for a hung one; the host's whole-second clock adds at most one more.
const REMOTE_REUSE_READY_TIMEOUT_SECS: u64 = 10;
/// How long the stop script waits for a server to exit after TERM before it
/// leaves the server, and its record, to the next launch, in seconds.
const REMOTE_STOP_WAIT_SECS: u64 = 10;
const SSH_PAIRING_DEADLINE: Duration = Duration::from_secs(30);
const SSH_LAUNCH_DEADLINE: Duration = Duration::from_secs(60);
const SSH_STOP_DEADLINE: Duration = Duration::from_secs(30);
/// How far the remote pairing watchdog's bound sits above the local pairing
/// deadline. The bound only limits a leak on the host, so it must never
/// fire before the desktop has given up: `[ssh_timeout:pairing]` stays the
/// only failure a user sees.
const SSH_PAIRING_WATCHDOG_MARGIN: Duration = Duration::from_secs(5);

/// Script identity prefix, also used by test fixtures.
macro_rules! remote_script_header_prefix {
    () => {
        "# bibcode-ssh:"
    };
}

/// Full first-line script identity.
macro_rules! remote_script_header {
    ($kind:literal) => {
        concat!(remote_script_header_prefix!(), $kind, "\n")
    };
}

/// Shell helpers shared by every remote script. `wait_while SECONDS CMD…`
/// polls every 0.2 s while CMD succeeds and fails if it still succeeds after
/// SECONDS: the one wait loop the scripts use. `pid_running PID` is
/// `kill -0` without the noise. The poll's `sleep` children also make every
/// POSIX shell reap a background child that has exited, so `kill -0` stops
/// seeing it.
macro_rules! remote_wait_functions {
    () => {
        r#"wait_while() {
  wait_polls=$(( $1 * 5 ))
  shift
  while "$@"; do
    [ "$wait_polls" -gt 0 ] || return 1
    sleep 0.2
    wait_polls=$((wait_polls - 1))
  done
  return 0
}
pid_running() {
  kill -0 "$1" 2>/dev/null
}
"#
    };
}

/// Shell function shared by the pairing and launch scripts (needs
/// `remote_wait_functions`): `run_bounded LIMIT CMD…` runs CMD in the
/// background (stdin from `/dev/null`, so it cannot eat the script `sh -s` is
/// still reading) and polls it every 0.2 s. When the host's clock reaches
/// LIMIT whole seconds after the start (between LIMIT - 1 and LIMIT seconds
/// later), it sends TERM, sends KILL 2 s later if CMD is still running, and
/// returns 124; otherwise it returns CMD's status.
macro_rules! remote_run_bounded_function {
    () => {
        r#"run_bounded() {
  bounded_limit="$1"
  shift
  "$@" </dev/null &
  bounded_pid=$!
  bounded_deadline=$(( $(date +%s) + bounded_limit ))
  while pid_running "$bounded_pid"; do
    if [ "$(date +%s)" -ge "$bounded_deadline" ]; then
      kill -TERM "$bounded_pid" 2>/dev/null || true
      wait_while 2 pid_running "$bounded_pid" || kill -KILL "$bounded_pid" 2>/dev/null || true
      wait "$bounded_pid" 2>/dev/null || true
      return 124
    fi
    sleep 0.2
  done
  bounded_status=0
  wait "$bounded_pid" || bounded_status=$?
  return "$bounded_status"
}
"#
    };
}

/// Launch-state variables and helpers shared by the launch and stop
/// scripts. `$1` is the environment's state key.
///
/// `is_state_server PID PORT` succeeds only while PID runs this state's
/// `bibcode serve --host 127.0.0.1 --port PORT --base-dir ~/.bibcode`, read
/// from `/proc/PID/cmdline` (Linux, BusyBox included) or `ps -p PID -o
/// command=` (macOS). A pid file can outlive its server and the pid can then
/// belong to anything, so no recorded pid is trusted, stopped, or reused
/// without it. `clear_state_files PID` removes the state files only while the
/// pid file still names PID, so it never discards a newer launch's record.
/// `write_state_file FILE VALUE` replaces FILE in one step (a temporary file
/// and `mv`), so no reader ever sees it half written.
macro_rules! remote_launch_state {
    () => {
        r#"STATE_KEY="$1"
STATE_DIR="$HOME/.bibcode-ssh-launch/$STATE_KEY"
SERVER_HOME="$HOME/.bibcode"
PORT_FILE="$STATE_DIR/port"
PID_FILE="$STATE_DIR/pid"
MANAGED_FILE="$STATE_DIR/managed"
is_state_server() {
  case "$1" in ''|0*|*[!0-9]*) return 1 ;; esac
  pid_running "$1" || return 1
  if [ -r "/proc/$1/cmdline" ]; then
    state_server_command=$(tr '\000' ' ' <"/proc/$1/cmdline" 2>/dev/null) || return 1
    # /proc ends every argument with a NUL, which is now a trailing space.
    state_server_command=${state_server_command% }
  else
    state_server_command=$(ps -p "$1" -o command= 2>/dev/null) || return 1
  fi
  case "$state_server_command" in
    *" serve --host 127.0.0.1 --port $2 --base-dir $SERVER_HOME") return 0 ;;
  esac
  return 1
}
clear_state_files() {
  [ "$(cat "$PID_FILE" 2>/dev/null || true)" = "$1" ] || return 0
  rm -f "$PID_FILE" "$PORT_FILE" "$MANAGED_FILE"
}
write_state_file() {
  printf '%s\n' "$2" >"$1.tmp"
  mv -f "$1.tmp" "$1"
}
"#
    };
}
/// Remote script that mints the one-time SSH bootstrap pairing credential.
///
/// It travels on stdin to `sh -s --`, like the launch and stop scripts, so the
/// remote login shell only ever parses `sh -s --` (OpenSSH joins the remote
/// argv with spaces and hands the result to the login shell). It runs under
/// the same non-login `sh` and `PATH` as the launch script, so it uses the
/// `bibcode` that serves the environment. It must target the same
/// `--base-dir` the launch script passes to `serve` (`SERVER_HOME`), and must
/// print a JSON line with a `credential` field — see
/// `parse_remote_pairing_credential`.
///
/// The desktop runs it as `sh -s -- <bound>`, the pairing deadline plus
/// `SSH_PAIRING_WATCHDOG_MARGIN` in whole seconds (`pairing_watchdog_bound`).
/// sshd does not signal a command without a PTY when the client goes away, so
/// the script, not the desktop, ends a `pairing issue` that outlives it: at
/// the bound it sends TERM, then KILL 2 s later, and exits 124. Otherwise it
/// passes the command's output and status through.
pub const REMOTE_PAIRING_SCRIPT: &str = concat!(
    remote_script_header!("pairing"),
    "set -eu\n",
    remote_wait_functions!(),
    remote_run_bounded_function!(),
    r#"if ! command -v bibcode >/dev/null 2>&1; then
  printf 'Remote host is missing the native BiBCode CLI. Install the Rust bibcode binary before connecting.\n' >&2
  exit 1
fi
run_bounded "$1" bibcode pairing issue --base-dir "$HOME/.bibcode" --json
"#
);
const ASKPASS_POSIX_SCRIPT: &str = r#"#!/bin/sh
if [ "${BIBCODE_SSH_AUTH_SECRET+x}" = "x" ]; then
  printf "%s\n" "$BIBCODE_SSH_AUTH_SECRET"
  exit 0
fi
printf 'BiBCode ssh-askpass invoked without BIBCODE_SSH_AUTH_SECRET.\n' >&2
exit 1
"#;
const ASKPASS_WINDOWS_LAUNCHER_SCRIPT: &str = "@echo off\r\npowershell -NoProfile -ExecutionPolicy Bypass -File \"%~dp0ssh-askpass.ps1\" %*\r\n";
const ASKPASS_WINDOWS_SCRIPT: &str = r#"# Invoked by ssh via SSH_ASKPASS when BiBCode re-runs ssh with a cached password.
if ($null -ne $env:BIBCODE_SSH_AUTH_SECRET) {
  [Console]::Out.WriteLine($env:BIBCODE_SSH_AUTH_SECRET)
  exit 0
}
[Console]::Error.WriteLine("BiBCode ssh-askpass invoked without BIBCODE_SSH_AUTH_SECRET.")
exit 1
"#;

/// Starts or reuses this environment's managed `bibcode serve`, as
/// `sh -s -- <state key>`, and prints `{"remotePort":…,"serverKind":"managed"}`.
///
/// Every wait is bounded on the host's clock: a recorded server gets
/// `REMOTE_REUSE_READY_TIMEOUT_SECS` to answer and a new one
/// `REMOTE_READY_TIMEOUT_SECS`, each probe at most about a second. A recorded
/// server that is alive but silent is stopped before a replacement starts, so
/// two managed servers never share the data root; a server the script gave up
/// on is stopped (TERM, then KILL after 2 s) before any report is written.
/// SIGPIPE is ignored and every report is allowed to fail, because the channel
/// may already be closed when the script reaches it.
const REMOTE_LAUNCH_SCRIPT: &str = concat!(
    remote_script_header!("launch"),
    "set -eu\n",
    "trap '' PIPE\n",
    remote_wait_functions!(),
    remote_launch_state!(),
    remote_run_bounded_function!(),
    r#"LOG_FILE="$STATE_DIR/server.log"
RUNNER_FILE="$STATE_DIR/run-bibcode.sh"
if command -v curl >/dev/null 2>&1; then
  READY_PROBE=curl
elif command -v wget >/dev/null 2>&1; then
  READY_PROBE=wget
else
  printf 'Remote host requires curl or wget for readiness checks.\n' >&2 || true
  exit 1
fi
mkdir -p "$STATE_DIR"
cat >"$RUNNER_FILE" <<'SH'
#!/bin/sh
if command -v bibcode >/dev/null 2>&1; then
  exec bibcode "$@"
fi
printf 'Remote host is missing the native BiBCode CLI. Install the Rust bibcode binary before connecting.\n' >&2
exit 1
SH
chmod 700 "$RUNNER_FILE"
probe_ready() {
  if [ "$READY_PROBE" = curl ]; then
    curl --fail --silent --show-error --max-time 1 \
      "http://127.0.0.1:$1/.well-known/bibcode/environment" >/dev/null 2>&1
  else
    # wget retries a timed-out read many times; the watchdog caps the probe.
    run_bounded 2 wget --quiet --timeout=1 --output-document=/dev/null \
      "http://127.0.0.1:$1/.well-known/bibcode/environment" >/dev/null 2>&1
  fi
}
# Succeeds once port $1 answers. Gives up after at least $2 and at most $2 + 1
# seconds on the host's whole-second clock, plus the probe under way.
wait_ready() {
  ready_started=$(date +%s)
  while ! probe_ready "$1"; do
    [ $(( $(date +%s) - ready_started )) -le "$2" ] || return 1
    sleep 0.1
  done
  return 0
}
# Stops pid $1, this state's server on port $2: TERM, then KILL after 2 s.
# Fails only if it is still running afterwards.
stop_state_server() {
  kill -TERM "$1" 2>/dev/null || return 0
  wait_while 2 is_state_server "$1" "$2" && return 0
  kill -KILL "$1" 2>/dev/null || true
  wait_while 2 is_state_server "$1" "$2"
}
port_in_use() {
  port="$1"
  if command -v ss >/dev/null 2>&1; then
    ss -H -ltn "sport = :$port" 2>/dev/null | grep -q .
    return $?
  fi
  hex_port=$(printf '%04X' "$port")
  awk -v suffix=":$hex_port" \
    '$2 ~ suffix "$" && $4 == "0A" { found = 1 } END { exit found ? 0 : 1 }' \
    /proc/net/tcp /proc/net/tcp6 2>/dev/null
}
# Prints the first free port from $1 (or the default) on.
pick_port() {
  start="$1"
  case "$start" in
    ''|*[!0-9]*) start="@@DEFAULT_REMOTE_PORT@@" ;;
  esac
  end=$((start + @@REMOTE_PORT_SCAN_WINDOW@@))
  port="$start"
  while [ "$port" -lt "$end" ]; do
    if ! port_in_use "$port"; then
      printf '%s' "$port"
      return 0
    fi
    port=$((port + 1))
  done
  return 1
}
REMOTE_PID="$(cat "$PID_FILE" 2>/dev/null || true)"
REMOTE_PORT="$(cat "$PORT_FILE" 2>/dev/null || true)"
if is_state_server "$REMOTE_PID" "$REMOTE_PORT"; then
  if wait_ready "$REMOTE_PORT" "@@REMOTE_REUSE_READY_TIMEOUT@@"; then
    printf '{"remotePort":%s,"serverKind":"managed"}\n' "$REMOTE_PORT" || true
    exit 0
  fi
  if ! stop_state_server "$REMOTE_PID" "$REMOTE_PORT"; then
    printf 'Remote BiBCode server %s does not answer and did not stop; not starting another beside it.\n' "$REMOTE_PID" >&2 || true
    exit 1
  fi
fi
clear_state_files "$REMOTE_PID"
REMOTE_PORT="$(pick_port "$REMOTE_PORT")" || true
if [ -z "$REMOTE_PORT" ]; then
  printf 'Failed to find an available port on the remote host.\n' >&2 || true
  exit 1
fi
# The port is recorded before the server starts, and the server records its
# own pid before it becomes `bibcode serve`, so no running server is ever
# unrecorded or recorded with another port, however this script is cut short.
write_state_file "$PORT_FILE" "$REMOTE_PORT"
nohup sh -c 'printf "%s\n" "$$" >"$1.tmp" && mv -f "$1.tmp" "$1" && shift && exec "$@"' \
  bibcode-launch "$PID_FILE" "$RUNNER_FILE" serve --host 127.0.0.1 --port "$REMOTE_PORT" --base-dir "$SERVER_HOME" >>"$LOG_FILE" 2>&1 < /dev/null &
REMOTE_PID="$!"
write_state_file "$MANAGED_FILE" managed
if ! wait_ready "$REMOTE_PORT" "@@REMOTE_READY_TIMEOUT@@"; then
  if stop_state_server "$REMOTE_PID" "$REMOTE_PORT"; then
    clear_state_files "$REMOTE_PID"
  fi
  printf 'Remote BiBCode server did not become ready on 127.0.0.1:%s.\n' "$REMOTE_PORT" >&2 || true
  tail -n 80 "$LOG_FILE" >&2 2>/dev/null || true
  exit 1
fi
printf '{"remotePort":%s,"serverKind":"managed"}\n' "$REMOTE_PORT" || true
"#
);

/// Stops this environment's managed server, as `sh -s -- <state key>`. It
/// sends TERM only, and only to a pid that is still this state's server,
/// then waits up to `REMOTE_STOP_WAIT_SECS` for it to exit and clears the
/// state files only once it has: a launch that followed at once would
/// otherwise find no record and start a second server beside one still
/// shutting down. A server that outlives the wait is left running, never
/// killed mid-work, and keeps its record, so the next launch finds it and
/// stops it (TERM, then KILL) before starting a replacement. Prints
/// `{"stopped":true}`, or `{"stopped":false}` for such a server.
///
/// It still honours an `external` marker, which older host state may hold;
/// no current script writes one.
const REMOTE_STOP_SCRIPT: &str = concat!(
    remote_script_header!("stop"),
    "set -eu\n",
    "trap '' PIPE\n",
    remote_wait_functions!(),
    remote_launch_state!(),
    r#"REMOTE_MANAGED="$(cat "$MANAGED_FILE" 2>/dev/null || true)"
REMOTE_PID="$(cat "$PID_FILE" 2>/dev/null || true)"
REMOTE_PORT="$(cat "$PORT_FILE" 2>/dev/null || true)"
if [ "$REMOTE_MANAGED" != "external" ] && is_state_server "$REMOTE_PID" "$REMOTE_PORT"; then
  kill -TERM "$REMOTE_PID" 2>/dev/null || true
  if ! wait_while "@@REMOTE_STOP_WAIT@@" is_state_server "$REMOTE_PID" "$REMOTE_PORT"; then
    printf '{"stopped":false}\n' || true
    exit 0
  fi
fi
clear_state_files "$REMOTE_PID"
printf '{"stopped":true}\n' || true
"#
);

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SshEnvironmentTarget {
    pub alias: String,
    pub hostname: String,
    pub username: Option<String>,
    pub port: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshEnvironmentEnsureOptions {
    pub issue_pairing_token: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SshEnvironmentBootstrap {
    pub target: SshEnvironmentTarget,
    pub http_base_url: String,
    pub ws_base_url: String,
    pub pairing_token: Option<String>,
    pub remote_port: u16,
    pub remote_server_kind: &'static str,
}

impl SshEnvironmentBootstrap {
    pub fn new(
        target: SshEnvironmentTarget,
        remote_port: u16,
        http_base_url: String,
        ws_base_url: String,
        pairing_token: Option<String>,
        remote_server_kind: &'static str,
    ) -> Self {
        Self {
            target,
            http_base_url,
            ws_base_url,
            pairing_token,
            remote_port,
            remote_server_kind,
        }
    }

    pub fn external(
        target: SshEnvironmentTarget,
        remote_port: u16,
        http_base_url: String,
        ws_base_url: String,
        pairing_token: Option<String>,
    ) -> Self {
        Self::new(
            target,
            remote_port,
            http_base_url,
            ws_base_url,
            pairing_token,
            "external",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteLaunchResult {
    pub remote_port: u16,
    pub server_kind: String,
}

impl RemoteLaunchResult {
    fn server_kind_static(&self) -> &'static str {
        if self.server_kind == "external" {
            "external"
        } else {
            "managed"
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteLaunchResultDocument {
    remote_port: u64,
    server_kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshAuthOptions {
    auth_secret: Option<String>,
    batch_mode: &'static str,
    interactive_auth: bool,
}

impl SshAuthOptions {
    pub fn batch() -> Self {
        Self {
            auth_secret: None,
            batch_mode: "yes",
            interactive_auth: false,
        }
    }

    pub fn with_secret(auth_secret: String) -> Self {
        Self {
            auth_secret: Some(auth_secret),
            batch_mode: "no",
            interactive_auth: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshEnvironmentLaunchPlan {
    pub key: String,
    pub program: String,
    pub args: Vec<String>,
    pub target: SshEnvironmentTarget,
    pub local_port: u16,
    pub remote_port: u16,
    pub remote_server_kind: &'static str,
    pub http_base_url: String,
    pub ws_base_url: String,
}

impl SshEnvironmentLaunchPlan {
    pub fn external(target: SshEnvironmentTarget, local_port: u16) -> Result<Self, String> {
        Self::forward(
            target,
            local_port,
            RemoteLaunchResult {
                remote_port: DEFAULT_REMOTE_PORT,
                server_kind: "external".to_string(),
            },
        )
    }

    pub fn forward(
        target: SshEnvironmentTarget,
        local_port: u16,
        remote: RemoteLaunchResult,
    ) -> Result<Self, String> {
        Self::forward_with_auth(target, local_port, remote, &SshAuthOptions::batch())
    }

    pub fn forward_with_auth(
        target: SshEnvironmentTarget,
        local_port: u16,
        remote: RemoteLaunchResult,
        auth: &SshAuthOptions,
    ) -> Result<Self, String> {
        let target = normalize_ssh_environment_target(target)?;
        let key = target_connection_key(&target);
        let remote_port = remote.remote_port;
        let http_base_url = format!("http://127.0.0.1:{local_port}/");
        let ws_base_url = format!("ws://127.0.0.1:{local_port}/");
        let mut args = Vec::new();
        args.extend(base_ssh_args_with_auth(&target, auth));
        args.extend([
            "-o".to_string(),
            "ExitOnForwardFailure=yes".to_string(),
            "-o".to_string(),
            "ServerAliveInterval=15".to_string(),
            "-o".to_string(),
            "ServerAliveCountMax=3".to_string(),
            "-n".to_string(),
            "-N".to_string(),
            "-L".to_string(),
            format!("{local_port}:127.0.0.1:{remote_port}"),
        ]);
        args.push(build_ssh_host_spec(&target)?);

        Ok(Self {
            key,
            program: ssh_command().to_string(),
            args,
            target,
            local_port,
            remote_port,
            remote_server_kind: remote.server_kind_static(),
            http_base_url,
            ws_base_url,
        })
    }
}

struct ManagedSshTunnel {
    child: ManagedSshChild,
    bootstrap: SshEnvironmentBootstrap,
    /// Port forwards over this connection, keyed by remote port. They are
    /// reaped with the tunnel, or handed to the reaper when it is dropped, so
    /// none outlives it.
    forwards: HashMap<u16, SshPortForward>,
}

impl ManagedSshTunnel {
    async fn terminate_and_reap(&mut self) {
        for (_, mut forward) in self.forwards.drain() {
            forward.child.terminate_and_reap().await;
        }
        self.child.terminate_and_reap().await;
    }
}

/// A dedicated `ssh -N -L 127.0.0.1:<local>:127.0.0.1:<remote>` child.
struct SshPortForward {
    child: ManagedSshChild,
    local_port: u16,
    last_requested: tokio::time::Instant,
    /// Dropping it stops the child's stderr drain.
    _stderr_drain: oneshot::Sender<()>,
}

/// Per-operation deadlines for the remote scripts the manager runs over SSH.
/// On expiry the SSH child is terminated and reaped, and the operation fails
/// with a `[ssh_timeout:<operation>]` error the renderer classifies as
/// transient.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SshOperationDeadlines {
    pub pairing: Duration,
    pub launch: Duration,
    pub stop: Duration,
}

impl SshOperationDeadlines {
    /// The remote pairing watchdog's bound for these deadlines: the pairing
    /// deadline plus 5 s, rounded up to whole seconds. A host-side
    /// `bibcode pairing issue` gets TERM at this bound and KILL 2 s later
    /// (see `REMOTE_PAIRING_SCRIPT`).
    pub fn pairing_watchdog_bound(&self) -> Duration {
        Duration::from_secs(pairing_watchdog_bound(self.pairing))
    }
}

impl Default for SshOperationDeadlines {
    fn default() -> Self {
        Self {
            pairing: SSH_PAIRING_DEADLINE,
            launch: SSH_LAUNCH_DEADLINE,
            stop: SSH_STOP_DEADLINE,
        }
    }
}

pub struct SshEnvironmentManager {
    tunnels: Mutex<HashMap<String, ManagedSshTunnel>>,
    auth_secrets: Mutex<HashMap<String, String>>,
    askpass_temporary_base: PathBuf,
    askpass_launcher: Mutex<Weak<SshAskpassLauncherInner>>,
    child_reaper: SshChildReaper,
    ssh_program: String,
    #[cfg(test)]
    ssh_config_file_for_test: Option<PathBuf>,
    deadlines: SshOperationDeadlines,
    /// One async lock per target connection key. `ensure_environment` and
    /// `disconnect_environment` hold it for their whole remote sequence, so
    /// preparation and stop of one target never interleave. It is the only
    /// lock held across an await.
    target_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Where SSH children are spawned, fed, drained and reaped. Only
    /// `shutdown()` reaps every SSH child before stopping it. On a plain drop
    /// the tunnels above (declared first, so dropped first) are handed to the
    /// reaper, but this runtime then stops in the background and discards the
    /// reap tasks, so those children are killed (`kill_on_drop`), not awaited.
    io_runtime: SshIoRuntime,
}

impl Default for SshEnvironmentManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SshEnvironmentManager {
    pub fn new() -> Self {
        Self::with_options(
            env::temp_dir(),
            ssh_command().to_string(),
            SshOperationDeadlines::default(),
        )
    }

    /// A manager that runs `ssh_program` instead of the platform `ssh` and
    /// uses `deadlines` for remote scripts. For test harnesses that stand in
    /// for OpenSSH without changing `PATH`; the application uses `new`.
    #[doc(hidden)]
    pub fn with_ssh_program(
        ssh_program: impl Into<String>,
        deadlines: SshOperationDeadlines,
    ) -> Self {
        Self::with_options(env::temp_dir(), ssh_program.into(), deadlines)
    }

    fn with_options(
        askpass_temporary_base: PathBuf,
        ssh_program: String,
        deadlines: SshOperationDeadlines,
    ) -> Self {
        Self {
            tunnels: Mutex::new(HashMap::new()),
            auth_secrets: Mutex::new(HashMap::new()),
            askpass_temporary_base,
            askpass_launcher: Mutex::new(Weak::new()),
            child_reaper: SshChildReaper::new(),
            ssh_program,
            #[cfg(test)]
            ssh_config_file_for_test: None,
            deadlines,
            target_locks: Mutex::new(HashMap::new()),
            io_runtime: SshIoRuntime::new(SSH_IO_MAX_BLOCKING_THREADS),
        }
    }

    #[cfg(test)]
    fn with_askpass_temp_base(askpass_temporary_base: PathBuf) -> Self {
        Self::with_options(
            askpass_temporary_base,
            ssh_command().to_string(),
            SshOperationDeadlines::default(),
        )
    }

    #[cfg(test)]
    pub(crate) fn with_ssh_config_file_for_test(mut self, path: PathBuf) -> Self {
        self.ssh_config_file_for_test = Some(path);
        self
    }

    fn remote_runner(&self, operation: RemoteOperation) -> Result<RemoteScriptRunner, String> {
        Ok(RemoteScriptRunner {
            program: self.ssh_program.clone(),
            #[cfg(test)]
            ssh_config_file_for_test: self.ssh_config_file_for_test.clone(),
            deadline: match operation {
                RemoteOperation::Pairing => self.deadlines.pairing,
                RemoteOperation::Launch => self.deadlines.launch,
                RemoteOperation::Stop => self.deadlines.stop,
            },
            io_runtime: self.io_runtime.handle()?,
        })
    }

    fn askpass_launcher(&self) -> Result<SshAskpassLauncher, String> {
        if let Some(existing) = self
            .askpass_launcher
            .lock()
            .map_err(|error| format!("Could not access SSH askpass owner: {error}"))?
            .upgrade()
        {
            return Ok(SshAskpassLauncher {
                inner: existing,
                child_reaper: self.child_reaper.clone(),
            });
        }

        let created =
            SshAskpassLauncher::create_in(&self.askpass_temporary_base, self.child_reaper.clone())?;
        let mut cached = self
            .askpass_launcher
            .lock()
            .map_err(|error| format!("Could not access SSH askpass owner: {error}"))?;
        if let Some(existing) = cached.upgrade() {
            return Ok(SshAskpassLauncher {
                inner: existing,
                child_reaper: self.child_reaper.clone(),
            });
        }
        *cached = Arc::downgrade(&created.inner);
        Ok(created)
    }

    pub(crate) async fn shutdown(&self) {
        self.child_reaper.close();
        let tunnels = self
            .tunnels
            .lock()
            .map(|mut tunnels| tunnels.drain().map(|(_, tunnel)| tunnel).collect())
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "could not drain SSH tunnels during shutdown");
                Vec::new()
            });
        for mut tunnel in tunnels {
            tunnel.terminate_and_reap().await;
        }
        self.child_reaper.wait().await;
        // Every SSH child is reaped, so the I/O runtime has nothing left to
        // drive. It stops in the background: this runs inside async code.
        self.io_runtime.stop();
    }

    pub async fn ensure_environment<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        prompts: &SshPasswordPromptManager,
        target: SshEnvironmentTarget,
        options: Option<SshEnvironmentEnsureOptions>,
    ) -> Result<SshEnvironmentBootstrap, String> {
        if !self.child_reaper.accepting() {
            return Err("SSH process owner is shutting down.".to_string());
        }
        let target = normalize_ssh_environment_target(target)?;
        let key = target_connection_key(&target);
        let issue_pairing_token = options
            .as_ref()
            .and_then(|options| options.issue_pairing_token)
            .unwrap_or(false);
        // Serialise the whole probe, launch-or-reuse, tunnel and publish
        // sequence per target: a second caller waits here and then finds the
        // first caller's tunnel on the cache-hit path. The wait is bounded
        // because everything the holder awaits is:
        // - the probe: one request of at most 2 s;
        // - each remote script: its deadline (launch 60 s, pairing 30 s,
        //   stop 30 s), which also covers draining its output; after ssh
        //   exits, the drain also stops once the pipes are idle for 2 s, even
        //   if a descendant holds them open;
        // - the tunnel: 30 s of readiness polling (each request at most 2 s),
        //   and at most 10 s (2 s idle) to collect an exited tunnel's stderr;
        // - each terminate-and-reap: 1.5 s, then the retained reaper owns it;
        // - each password prompt: 3 min, at most two per step, so a step runs
        //   at most three times.
        let target_lock = self.target_lock(&key)?;
        let _serialised = target_lock.lock().await;
        if !self.child_reaper.accepting() {
            return Err("SSH process owner is shutting down.".to_string());
        }
        if let Some(cached) = self.take_existing_bootstrap_if_running(&key)? {
            // A live ssh process does not prove a live remote server: probe it
            // through the tunnel so a server that stopped under a live SSH
            // session is relaunched by the full path below.
            if tunnel_endpoint_responds(&cached.http_base_url).await {
                return self
                    .reuse_cached_tunnel(app, prompts, &key, &target, cached, issue_pairing_token)
                    .await;
            }
            tracing::info!("cached SSH tunnel endpoint did not respond; reconnecting");
            self.drop_cached_tunnel(&key).await;
        }

        let local_port = portpicker::pick_unused_port()
            .ok_or_else(|| "Could not find an available local SSH tunnel port.".to_string())?;
        let askpass_launcher = self.askpass_launcher()?;
        let launch_runner = self.remote_runner(RemoteOperation::Launch)?;
        let io_runtime = launch_runner.io_runtime.clone();
        let remote_launch = self
            .run_with_ssh_auth(app, prompts, &key, &target, |auth| {
                let target = target.clone();
                let askpass_launcher = askpass_launcher.clone();
                let runner = launch_runner.clone();
                async move {
                    launch_or_reuse_remote_server(&runner, &target, &auth, askpass_launcher).await
                }
            })
            .await?;
        let tunnel_result = self
            .run_with_ssh_auth(app, prompts, &key, &target, |auth| {
                let askpass_launcher = askpass_launcher.clone();
                let plan = self.forward_plan(&target, local_port, remote_launch.clone(), &auth);
                let io_runtime = io_runtime.clone();
                async move {
                    let plan = plan?;
                    let tunnel_plan = plan.clone();
                    let child = run_on_ssh_io(&io_runtime, async move {
                        start_ssh_tunnel(
                            &tunnel_plan,
                            &auth,
                            askpass_launcher,
                            SshForwardKind::Tunnel,
                        )
                        .await
                    })
                    .await?;
                    Ok((plan, child))
                }
            })
            .await;
        let (plan, child) = match tunnel_result {
            Ok(result) => result,
            Err(error) => {
                let cleanup_auth = self
                    .cached_auth_secret(&key)
                    .map(SshAuthOptions::with_secret)
                    .unwrap_or_else(SshAuthOptions::batch);
                if let Ok(stop_runner) = self.remote_runner(RemoteOperation::Stop) {
                    let _ =
                        stop_remote_server(&stop_runner, &target, &cleanup_auth, askpass_launcher)
                            .await;
                }
                return Err(error);
            }
        };

        let pairing_token = if issue_pairing_token {
            Some(
                self.mint_pairing_token(app, prompts, &key, &target, &askpass_launcher)
                    .await?,
            )
        } else {
            None
        };
        let bootstrap = SshEnvironmentBootstrap::new(
            target,
            plan.remote_port,
            plan.http_base_url,
            plan.ws_base_url,
            pairing_token,
            plan.remote_server_kind,
        );
        if let Err((error, mut child)) = self.publish_tunnel(key, child, bootstrap.clone()) {
            child.terminate_and_reap().await;
            return Err(error);
        }
        Ok(bootstrap)
    }

    /// The `ssh -N -L <local>:127.0.0.1:<remote>` plan for this manager's
    /// SSH program.
    fn forward_plan(
        &self,
        target: &SshEnvironmentTarget,
        local_port: u16,
        remote: RemoteLaunchResult,
        auth: &SshAuthOptions,
    ) -> Result<SshEnvironmentLaunchPlan, String> {
        let mut plan =
            SshEnvironmentLaunchPlan::forward_with_auth(target.clone(), local_port, remote, auth)?;
        plan.program = self.ssh_program.clone();
        #[cfg(test)]
        if let Some(path) = &self.ssh_config_file_for_test {
            plan.args.splice(
                0..0,
                ["-F".to_string(), path.to_string_lossy().into_owned()],
            );
        }
        Ok(plan)
    }

    /// A preview port forward's plan: the tunnel's arguments, forwarding to
    /// `remote_port` on the host's loopback, and hardened against inherited
    /// SSH configuration. The local end binds to loopback explicitly, since
    /// `GatewayPorts yes` would otherwise expose it to the network, and
    /// `ControlPath=none` keeps the forward in this child: a multiplexed
    /// client would hand it to a master it does not own and exit.
    fn port_forward_plan(
        &self,
        target: &SshEnvironmentTarget,
        bind: ForwardBind,
        remote_port: u16,
        auth: &SshAuthOptions,
    ) -> Result<SshEnvironmentLaunchPlan, String> {
        let remote = RemoteLaunchResult {
            remote_port,
            // Not a server; the plan's server kind goes unused.
            server_kind: "external".to_string(),
        };
        let mut plan = self.forward_plan(target, bind.port, remote, auth)?;
        let spec = format!("{}:127.0.0.1:{remote_port}", bind.port);
        let index = plan
            .args
            .iter()
            .position(|arg| *arg == spec)
            .ok_or_else(|| "SSH port forward plan lost its forward.".to_string())?;
        plan.args[index] = format!("127.0.0.1:{spec}");
        if bind.ipv6 {
            // Before the IPv4 forward, which the readiness probe connects to.
            plan.args.splice(
                index - 1..index - 1,
                ["-L".to_string(), format!("[::1]:{spec}")],
            );
        }
        plan.args
            .splice(0..0, ["-o".to_string(), "ControlPath=none".to_string()]);
        Ok(plan)
    }

    /// Forwards `remote_port` on the target host's loopback to a free local
    /// port over a dedicated `ssh -N -L` child, and returns the local port.
    ///
    /// It needs the target's live tunnel and ends with it. Per remote port it
    /// is idempotent: a live forward is reused and a dead one replaced. At
    /// most `SSH_MAX_PORT_FORWARDS_PER_TUNNEL` forwards are kept per tunnel
    /// and `SSH_PORT_FORWARD_BUDGET` across tunnels; the least recently used
    /// one goes only after its replacement is ready.
    pub async fn ensure_port_forward<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        prompts: &SshPasswordPromptManager,
        target: SshEnvironmentTarget,
        remote_port: u16,
        preferred_local_port: Option<u16>,
    ) -> Result<u16, String> {
        if !self.child_reaper.accepting() {
            return Err("SSH process owner is shutting down.".to_string());
        }
        if remote_port == 0 {
            return Err("SSH port forward needs a remote port from 1 to 65535.".to_string());
        }
        let target = normalize_ssh_environment_target(target)?;
        let key = target_connection_key(&target);
        // Same lock as `ensure_environment`: the tunnel cannot be replaced or
        // stopped while its forwards change. The wait is bounded by
        // `ensure_environment`'s, or here by at most three 30 s readiness
        // waits around two password prompts and the evictions (1.5 s each).
        let target_lock = self.target_lock(&key)?;
        let _serialised = target_lock.lock().await;
        if self.take_existing_bootstrap_if_running(&key)?.is_none() {
            return Err(SSH_CONNECTION_NOT_ACTIVE.to_string());
        }
        let stale = {
            let mut tunnels = self
                .tunnels
                .lock()
                .map_err(|error| format!("Could not access SSH tunnels: {error}"))?;
            let tunnel = tunnels
                .get_mut(&key)
                .ok_or_else(|| SSH_CONNECTION_NOT_ACTIVE.to_string())?;
            if let Some(forward) = tunnel.forwards.get_mut(&remote_port)
                && matches!(forward.child.child_mut().try_wait(), Ok(None))
            {
                forward.last_requested = tokio::time::Instant::now();
                return Ok(forward.local_port);
            }
            // Only a dead forward goes now; live ones are evicted after the
            // new one is ready, so a failed start costs none of them.
            tunnel.forwards.remove(&remote_port)
        };
        if let Some(mut forward) = stale {
            forward.child.terminate_and_reap().await;
        }

        // A live forward was reused above whatever its local port, so an open
        // tab never switches origin; the preference only shapes a new one.
        let preferred = preferred_local_port.filter(|port| *port >= 1024);
        let (local_port, mut child) = match self
            .spawn_port_forward(app, prompts, &key, &target, remote_port, preferred)
            .await
        {
            // Another process took the preferred port after the attempt's probe.
            Err(error) if preferred.is_some() && is_forward_bind_failure(&error) => {
                self.spawn_port_forward(app, prompts, &key, &target, remote_port, None)
                    .await?
            }
            result => result?,
        };
        let io_runtime = self.io_runtime.handle()?;
        let stderr_drain = drain_forward_stderr(&io_runtime, &mut child);
        let forward = SshPortForward {
            child,
            local_port,
            last_requested: tokio::time::Instant::now(),
            _stderr_drain: stderr_drain,
        };
        match self.publish_port_forward(&key, remote_port, forward) {
            Ok(evicted) => {
                for mut forward in evicted {
                    forward.child.terminate_and_reap().await;
                }
                Ok(local_port)
            }
            Err((error, mut forward)) => {
                forward.child.terminate_and_reap().await;
                Err(error)
            }
        }
    }

    /// Spawns one forward child to `remote_port` on the `preferred` local port
    /// when it is free, otherwise on a random `127.0.0.1` port.
    async fn spawn_port_forward<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        prompts: &SshPasswordPromptManager,
        key: &str,
        target: &SshEnvironmentTarget,
        remote_port: u16,
        preferred: Option<u16>,
    ) -> Result<(u16, ManagedSshChild), String> {
        let askpass_launcher = self.askpass_launcher()?;
        let io_runtime = self.io_runtime.handle()?;
        self.run_with_ssh_auth(app, prompts, key, target, |auth| {
            // Each attempt checks or picks its port just before spawning, so a
            // password prompt never hands ssh a port another process took
            // meanwhile (the readiness probe could then reach that listener).
            let plan = preferred
                .and_then(probe_preferred_port)
                .map(Ok)
                .unwrap_or_else(|| {
                    portpicker::pick_unused_port()
                        .map(|port| ForwardBind { port, ipv6: false })
                        .ok_or_else(|| {
                            "Could not find an available local SSH port forward port.".to_string()
                        })
                })
                .and_then(|bind| self.port_forward_plan(target, bind, remote_port, &auth));
            let askpass_launcher = askpass_launcher.clone();
            let io_runtime = io_runtime.clone();
            async move {
                let plan = plan?;
                let local_port = plan.local_port;
                let child = run_on_ssh_io(&io_runtime, async move {
                    start_ssh_tunnel(&plan, &auth, askpass_launcher, SshForwardKind::PortForward)
                        .await
                })
                .await?;
                Ok((local_port, child))
            }
        })
        .await
    }

    /// Terminates and reaps the forward of `remote_port`, if there is one.
    pub async fn release_port_forward(
        &self,
        target: SshEnvironmentTarget,
        remote_port: u16,
    ) -> Result<(), String> {
        let target = normalize_ssh_environment_target(target)?;
        let key = target_connection_key(&target);
        let target_lock = self.target_lock(&key)?;
        let _serialised = target_lock.lock().await;
        let forward = self
            .tunnels
            .lock()
            .map_err(|error| format!("Could not access SSH tunnels: {error}"))?
            .get_mut(&key)
            .and_then(|tunnel| tunnel.forwards.remove(&remote_port));
        if let Some(mut forward) = forward {
            forward.child.terminate_and_reap().await;
        }
        Ok(())
    }

    /// Records a ready forward on its tunnel and returns the least recently
    /// used forwards it evicts to stay within the per-tunnel and global
    /// limits, for the caller to reap. Callers hold the target lock, so only
    /// `shutdown` or the tunnel's own exit can have ended the tunnel since
    /// they checked it; the forward is then refused, never orphaned, and
    /// nothing is evicted.
    fn publish_port_forward(
        &self,
        key: &str,
        remote_port: u16,
        forward: SshPortForward,
    ) -> Result<Vec<SshPortForward>, (String, Box<SshPortForward>)> {
        let mut tunnels = match self.tunnels.lock() {
            Ok(tunnels) => tunnels,
            Err(error) => {
                return Err((
                    format!("Could not record SSH port forward: {error}"),
                    Box::new(forward),
                ));
            }
        };
        if !self.child_reaper.accepting() {
            return Err((
                "SSH process owner is shutting down.".to_string(),
                Box::new(forward),
            ));
        }
        let Some(tunnel) = tunnels.get_mut(key) else {
            return Err((SSH_CONNECTION_NOT_ACTIVE.to_string(), Box::new(forward)));
        };
        if !matches!(tunnel.child.child_mut().try_wait(), Ok(None)) {
            return Err((SSH_CONNECTION_NOT_ACTIVE.to_string(), Box::new(forward)));
        }
        // The caller removed any previous forward; a replaced one is reaped
        // with the evicted ones.
        let mut evicted: Vec<SshPortForward> = tunnel
            .forwards
            .insert(remote_port, forward)
            .into_iter()
            .collect();
        while tunnel.forwards.len() > SSH_MAX_PORT_FORWARDS_PER_TUNNEL {
            let Some((_, oldest)) = least_recent_forward(
                tunnel
                    .forwards
                    .iter()
                    .map(|(port, forward)| ((key, *port), forward)),
                (key, remote_port),
            ) else {
                break;
            };
            evicted.extend(tunnel.forwards.remove(&oldest));
        }
        while tunnels
            .values()
            .map(|tunnel| tunnel.forwards.len())
            .sum::<usize>()
            > SSH_PORT_FORWARD_BUDGET
        {
            let Some((oldest_key, oldest_port)) = least_recent_forward(
                tunnels.iter().flat_map(|(tunnel_key, tunnel)| {
                    tunnel
                        .forwards
                        .iter()
                        .map(move |(port, forward)| ((tunnel_key.as_str(), *port), forward))
                }),
                (key, remote_port),
            )
            .map(|(tunnel_key, port)| (tunnel_key.to_string(), port)) else {
                break;
            };
            evicted.extend(
                tunnels
                    .get_mut(&oldest_key)
                    .and_then(|tunnel| tunnel.forwards.remove(&oldest_port)),
            );
        }
        Ok(evicted)
    }

    fn target_lock(&self, key: &str) -> Result<Arc<tokio::sync::Mutex<()>>, String> {
        let mut locks = self
            .target_locks
            .lock()
            .map_err(|error| format!("Could not access SSH target locks: {error}"))?;
        Ok(locks.entry(key.to_string()).or_default().clone())
    }

    /// Returns a cached, responding tunnel. Cached bootstraps never carry a
    /// token; every request for one mints a fresh one-time credential.
    async fn reuse_cached_tunnel<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        prompts: &SshPasswordPromptManager,
        key: &str,
        target: &SshEnvironmentTarget,
        cached: SshEnvironmentBootstrap,
        issue_pairing_token: bool,
    ) -> Result<SshEnvironmentBootstrap, String> {
        if !issue_pairing_token {
            return Ok(cached);
        }
        let askpass_launcher = self.askpass_launcher()?;
        let pairing_token = self
            .mint_pairing_token(app, prompts, key, target, &askpass_launcher)
            .await?;
        Ok(SshEnvironmentBootstrap {
            pairing_token: Some(pairing_token),
            ..cached
        })
    }

    async fn mint_pairing_token<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        prompts: &SshPasswordPromptManager,
        key: &str,
        target: &SshEnvironmentTarget,
        askpass_launcher: &SshAskpassLauncher,
    ) -> Result<String, String> {
        let runner = self.remote_runner(RemoteOperation::Pairing)?;
        self.run_with_ssh_auth(app, prompts, key, target, |auth| {
            let target = target.clone();
            let askpass_launcher = askpass_launcher.clone();
            let runner = runner.clone();
            async move { issue_remote_pairing_token(&runner, &target, &auth, askpass_launcher).await }
        })
        .await
    }

    /// Removes and reaps the cached tunnel for `key`. Callers hold the
    /// target lock, so the entry is the one they inspected.
    async fn drop_cached_tunnel(&self, key: &str) {
        let stale = match self.tunnels.lock() {
            Ok(mut tunnels) => tunnels.remove(key),
            Err(error) => {
                tracing::warn!(%error, "could not access SSH tunnels to drop a stale tunnel");
                None
            }
        };
        if let Some(mut stale) = stale {
            stale.terminate_and_reap().await;
        }
    }

    pub async fn disconnect_environment<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        prompts: &SshPasswordPromptManager,
        target: SshEnvironmentTarget,
    ) -> Result<(), String> {
        let target = normalize_ssh_environment_target(target)?;
        let key = target_connection_key(&target);
        // Same lock as `ensure_environment`: a stop never interleaves with a
        // preparation of this target.
        let target_lock = self.target_lock(&key)?;
        let _serialised = target_lock.lock().await;
        let tunnel = self
            .tunnels
            .lock()
            .map_err(|error| format!("Could not access SSH tunnels: {error}"))?
            .remove(&key);
        let askpass_launcher = match tunnel {
            Some(mut tunnel) => {
                let askpass_launcher = tunnel.child.askpass_launcher().clone();
                tunnel.terminate_and_reap().await;
                askpass_launcher
            }
            None => self.askpass_launcher()?,
        };
        let stop_runner = self.remote_runner(RemoteOperation::Stop)?;
        self.run_with_ssh_auth(app, prompts, &key, &target, |auth| {
            let target = target.clone();
            let askpass_launcher = askpass_launcher.clone();
            let runner = stop_runner.clone();
            async move { stop_remote_server(&runner, &target, &auth, askpass_launcher).await }
        })
        .await?;
        Ok(())
    }

    fn cached_auth_secret(&self, key: &str) -> Option<String> {
        self.auth_secrets.lock().ok()?.get(key).cloned()
    }

    fn remember_auth_secret(&self, key: &str, secret: String) -> Result<(), String> {
        self.auth_secrets
            .lock()
            .map_err(|error| format!("Could not cache SSH authentication secret: {error}"))?
            .insert(key.to_string(), secret);
        Ok(())
    }

    fn clear_auth_secret(&self, key: &str) {
        if let Ok(mut secrets) = self.auth_secrets.lock() {
            secrets.remove(key);
        }
    }

    async fn prompt_for_password<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        prompts: &SshPasswordPromptManager,
        target: &SshEnvironmentTarget,
        attempt: u8,
    ) -> Result<String, String> {
        let destination = build_ssh_host_spec(target)?;
        let prompt = if attempt == 1 {
            format!("Enter the SSH password for {destination}.")
        } else {
            format!("SSH authentication failed. Enter the password for {destination} again.")
        };
        prompts
            .request_password(
                app,
                SshPasswordRequest {
                    destination,
                    username: target.username.clone(),
                    prompt,
                },
            )
            .await
            .map_err(password_prompt_failure)
    }

    async fn run_with_ssh_auth<R: Runtime, T, F, Fut>(
        &self,
        app: &AppHandle<R>,
        prompts: &SshPasswordPromptManager,
        key: &str,
        target: &SshEnvironmentTarget,
        mut operation: F,
    ) -> Result<T, String>
    where
        F: FnMut(SshAuthOptions) -> Fut,
        Fut: std::future::Future<Output = Result<T, String>>,
    {
        let mut prompted_attempts = 0_u8;
        let mut auth = self
            .cached_auth_secret(key)
            .map(SshAuthOptions::with_secret)
            .unwrap_or_else(SshAuthOptions::batch);

        loop {
            match operation(auth.clone()).await {
                Ok(result) => return Ok(result),
                Err(error) if is_ssh_auth_failure(&error) => {
                    if auth.auth_secret.is_some() {
                        self.clear_auth_secret(key);
                    }
                    if prompted_attempts >= 2 {
                        return Err(error);
                    }
                    prompted_attempts += 1;
                    let secret = self
                        .prompt_for_password(app, prompts, target, prompted_attempts)
                        .await?;
                    self.remember_auth_secret(key, secret.clone())?;
                    auth = SshAuthOptions::with_secret(secret);
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn take_existing_bootstrap_if_running(
        &self,
        key: &str,
    ) -> Result<Option<SshEnvironmentBootstrap>, String> {
        let mut tunnels = self
            .tunnels
            .lock()
            .map_err(|error| format!("Could not access SSH tunnels: {error}"))?;
        let Some(tunnel) = tunnels.get_mut(key) else {
            return Ok(None);
        };
        match tunnel
            .child
            .child_mut()
            .try_wait()
            .map_err(|error| format!("Could not inspect SSH tunnel process: {error}"))?
        {
            None => Ok(Some(tunnel.bootstrap.clone())),
            Some(_status) => {
                let mut stale = tunnels.remove(key);
                drop(tunnels);
                if let Some(stale) = stale.as_mut() {
                    stale.child.release_reaped();
                }
                drop(stale);
                Ok(None)
            }
        }
    }

    /// Records a live tunnel. Callers hold the target lock, so the slot is
    /// empty or holds a tunnel this caller has already dropped.
    ///
    /// The cached bootstrap never keeps a pairing token: one-time credentials
    /// are consumed by the first exchange, so a cached copy could only ever be
    /// rejected.
    fn publish_tunnel(
        &self,
        key: String,
        child: ManagedSshChild,
        mut bootstrap: SshEnvironmentBootstrap,
    ) -> Result<(), (String, Box<ManagedSshChild>)> {
        bootstrap.pairing_token = None;
        let mut tunnels = match self.tunnels.lock() {
            Ok(tunnels) => tunnels,
            Err(error) => {
                return Err((
                    format!("Could not record SSH tunnel: {error}"),
                    Box::new(child),
                ));
            }
        };
        if !self.child_reaper.accepting() {
            return Err((
                "SSH process owner is shutting down.".to_string(),
                Box::new(child),
            ));
        }
        let replaced = tunnels.insert(
            key,
            ManagedSshTunnel {
                child,
                bootstrap,
                forwards: HashMap::new(),
            },
        );
        drop(tunnels);
        // Dropping hands a replaced child to the retained reaper.
        drop(replaced);
        Ok(())
    }
}

fn ssh_command() -> &'static str {
    if cfg!(windows) { "ssh.exe" } else { "ssh" }
}

fn normalize_ssh_environment_target(
    mut target: SshEnvironmentTarget,
) -> Result<SshEnvironmentTarget, String> {
    target.alias = target.alias.trim().to_string();
    target.hostname = target.hostname.trim().to_string();
    target.username = target
        .username
        .map(|username| username.trim().to_string())
        .filter(|username| !username.is_empty());
    if target.alias.is_empty() {
        target.alias = target.hostname.clone();
    }
    if target.hostname.is_empty() {
        target.hostname = target.alias.clone();
    }
    if target.alias.is_empty() || target.hostname.is_empty() {
        return Err("SSH target is missing its alias/hostname.".to_string());
    }
    Ok(target)
}

fn target_connection_key(target: &SshEnvironmentTarget) -> String {
    format!(
        "{}\u{0}{}\u{0}{}\u{0}{}",
        target.alias,
        target.hostname,
        target.username.as_deref().unwrap_or_default(),
        target.port.map(|port| port.to_string()).unwrap_or_default()
    )
}

fn remote_state_key(target: &SshEnvironmentTarget) -> String {
    let digest = Sha256::digest(target_connection_key(target).as_bytes());
    digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn build_ssh_host_spec(target: &SshEnvironmentTarget) -> Result<String, String> {
    let destination = if target.alias.trim().is_empty() {
        target.hostname.trim()
    } else {
        target.alias.trim()
    };
    if destination.is_empty() {
        return Err("SSH target is missing its alias/hostname.".to_string());
    }
    Ok(match target.username.as_deref() {
        Some(username) => format!("{username}@{destination}"),
        None => destination.to_string(),
    })
}

fn base_ssh_args_with_auth(target: &SshEnvironmentTarget, auth: &SshAuthOptions) -> Vec<String> {
    let mut args = vec![
        "-o".to_string(),
        format!("BatchMode={}", auth.batch_mode),
        "-o".to_string(),
        "ConnectTimeout=10".to_string(),
    ];
    if let Some(port) = target.port {
        args.push("-p".to_string());
        args.push(port.to_string());
    }
    args
}

fn build_ssh_child_environment(
    auth: &SshAuthOptions,
    askpass_launcher: &Path,
) -> HashMap<String, String> {
    if !auth.interactive_auth {
        return HashMap::new();
    }
    let mut environment = HashMap::new();
    environment.insert(
        "SSH_ASKPASS".to_string(),
        askpass_launcher.to_string_lossy().into_owned(),
    );
    environment.insert("SSH_ASKPASS_REQUIRE".to_string(), "force".to_string());
    if let Some(secret) = &auth.auth_secret {
        environment.insert("BIBCODE_SSH_AUTH_SECRET".to_string(), secret.clone());
    }
    if !cfg!(windows) && env::var_os("DISPLAY").is_none() {
        environment.insert("DISPLAY".to_string(), "bibcode".to_string());
    }
    environment
}

fn is_ssh_auth_failure(message: &str) -> bool {
    let normalized = message.to_lowercase();
    normalized.contains("authentication failed")
        || normalized.contains("too many authentication failures")
        || (normalized.contains("permission denied (")
            && (normalized.contains("password")
                || normalized.contains("keyboard-interactive")
                || normalized.contains("publickey")
                || normalized.contains("hostbased")
                || normalized.contains("gssapi-with-mic")))
}

#[derive(Clone)]
struct SshAskpassLauncher {
    inner: Arc<SshAskpassLauncherInner>,
    child_reaper: SshChildReaper,
}

struct SshAskpassLauncherInner {
    root: PathBuf,
    directory: PathBuf,
    files: Vec<PathBuf>,
    launcher: PathBuf,
    cleanup_sender: watch::Sender<bool>,
}

impl SshAskpassLauncher {
    fn create_in(temporary_base: &Path, child_reaper: SshChildReaper) -> Result<Self, String> {
        let root = temporary_base.join(format!(
            "bibcode-ssh-runtime-{}-{}",
            std::process::id(),
            Uuid::new_v4().simple()
        ));
        fs::create_dir(&root)
            .map_err(|error| format!("Failed to create SSH askpass root: {error}"))?;
        let directory = root.join("bibcode-ssh-askpass");
        let launcher = if cfg!(windows) {
            directory.join("ssh-askpass.cmd")
        } else {
            directory.join("ssh-askpass.sh")
        };
        let mut files = vec![launcher.clone()];
        if cfg!(windows) {
            files.push(directory.join("ssh-askpass.ps1"));
        }
        let (cleanup_sender, _) = watch::channel(false);
        let inner = SshAskpassLauncherInner {
            root,
            directory,
            files,
            launcher,
            cleanup_sender,
        };

        set_ssh_askpass_directory_permissions(&inner.root)?;
        fs::create_dir(&inner.directory)
            .map_err(|error| format!("Failed to create SSH askpass directory: {error}"))?;
        set_ssh_askpass_directory_permissions(&inner.directory)?;
        if cfg!(windows) {
            write_askpass_file(&inner.launcher, ASKPASS_WINDOWS_LAUNCHER_SCRIPT, None)?;
            write_askpass_file(
                &inner.directory.join("ssh-askpass.ps1"),
                ASKPASS_WINDOWS_SCRIPT,
                None,
            )?;
        } else {
            write_askpass_file(&inner.launcher, ASKPASS_POSIX_SCRIPT, Some(0o700))?;
        }
        Ok(Self {
            inner: Arc::new(inner),
            child_reaper,
        })
    }

    fn path(&self) -> &Path {
        &self.inner.launcher
    }

    #[cfg(test)]
    fn root(&self) -> &Path {
        &self.inner.root
    }

    #[cfg(test)]
    fn cleanup_observer(&self) -> watch::Receiver<bool> {
        self.inner.cleanup_sender.subscribe()
    }

    fn reserve_child(&self) -> Result<SshChildReaperPermit, String> {
        self.child_reaper.reserve()
    }
}

impl Drop for SshAskpassLauncherInner {
    fn drop(&mut self) {
        for file in &self.files {
            if let Err(error) = fs::remove_file(file)
                && error.kind() != io::ErrorKind::NotFound
            {
                tracing::warn!(%error, "failed to remove an exact SSH askpass file");
            }
        }
        for directory in [&self.directory, &self.root] {
            if let Err(error) = fs::remove_dir(directory)
                && error.kind() != io::ErrorKind::NotFound
            {
                tracing::warn!(%error, "failed to remove an exact SSH askpass directory");
            }
        }
        let _ = self.cleanup_sender.send(true);
    }
}

#[derive(Clone)]
struct SshChildReaper {
    inner: Arc<SshChildReaperInner>,
}

struct SshChildReaperInner {
    accepting: AtomicBool,
    active: AtomicUsize,
    capacity: Arc<Semaphore>,
    idle: Notify,
    #[cfg(test)]
    admitted: Notify,
    shutdown_sender: watch::Sender<bool>,
}

struct SshChildReaperPermit {
    inner: Arc<SshChildReaperInner>,
    capacity: Option<OwnedSemaphorePermit>,
    runtime: tokio::runtime::Handle,
    shutdown_receiver: watch::Receiver<bool>,
}

impl SshChildReaper {
    fn new() -> Self {
        let (shutdown_sender, _) = watch::channel(false);
        Self {
            inner: Arc::new(SshChildReaperInner {
                accepting: AtomicBool::new(true),
                active: AtomicUsize::new(0),
                capacity: Arc::new(Semaphore::new(SSH_CHILD_REAPER_CAPACITY)),
                idle: Notify::new(),
                #[cfg(test)]
                admitted: Notify::new(),
                shutdown_sender,
            }),
        }
    }

    fn reserve(&self) -> Result<SshChildReaperPermit, String> {
        if !self.inner.accepting.load(Ordering::Acquire) {
            return Err("SSH process owner is shutting down.".to_string());
        }
        let capacity = self
            .inner
            .capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| "SSH process owner capacity was exceeded.".to_string())?;
        self.inner.active.fetch_add(1, Ordering::AcqRel);
        #[cfg(test)]
        self.inner.admitted.notify_waiters();
        let permit = SshChildReaperPermit {
            inner: self.inner.clone(),
            capacity: Some(capacity),
            runtime: tokio::runtime::Handle::current(),
            shutdown_receiver: self.inner.shutdown_sender.subscribe(),
        };
        if !self.inner.accepting.load(Ordering::Acquire) {
            drop(permit);
            return Err("SSH process owner is shutting down.".to_string());
        }
        Ok(permit)
    }

    fn close(&self) {
        self.inner.accepting.store(false, Ordering::Release);
        self.inner.capacity.close();
        let _ = self.inner.shutdown_sender.send(true);
    }

    fn accepting(&self) -> bool {
        self.inner.accepting.load(Ordering::Acquire)
    }

    async fn wait(&self) {
        loop {
            let idle = self.inner.idle.notified();
            tokio::pin!(idle);
            idle.as_mut().enable();
            if self.inner.active.load(Ordering::Acquire) == 0 {
                return;
            }
            idle.await;
        }
    }

    #[cfg(test)]
    fn active(&self) -> usize {
        self.inner.active.load(Ordering::Acquire)
    }

    #[cfg(test)]
    async fn wait_until_active(&self) {
        loop {
            let admitted = self.inner.admitted.notified();
            tokio::pin!(admitted);
            admitted.as_mut().enable();
            if self.active() > 0 {
                return;
            }
            admitted.await;
        }
    }
}

impl SshChildReaperPermit {
    fn shutdown_receiver(&self) -> watch::Receiver<bool> {
        self.shutdown_receiver.clone()
    }

    fn spawn_reap(self, mut child: Child, askpass_launcher: SshAskpassLauncher) {
        let runtime = self.runtime.clone();
        runtime.spawn(async move {
            let _ = child.start_kill();
            let _ = child.wait().await;
            drop(askpass_launcher);
            drop(self);
        });
    }
}

impl Drop for SshChildReaperPermit {
    fn drop(&mut self) {
        self.capacity.take();
        if self.inner.active.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.inner.idle.notify_waiters();
        }
    }
}

struct ManagedSshChild {
    child: Option<Child>,
    askpass_launcher: Option<SshAskpassLauncher>,
    reaper_permit: Option<SshChildReaperPermit>,
}

impl ManagedSshChild {
    fn new(
        child: Child,
        askpass_launcher: SshAskpassLauncher,
        reaper_permit: SshChildReaperPermit,
    ) -> Self {
        Self {
            child: Some(child),
            askpass_launcher: Some(askpass_launcher),
            reaper_permit: Some(reaper_permit),
        }
    }

    fn child_mut(&mut self) -> &mut Child {
        self.child.as_mut().expect("managed SSH child is live")
    }

    fn askpass_launcher(&self) -> &SshAskpassLauncher {
        self.askpass_launcher
            .as_ref()
            .expect("managed SSH child retains askpass ownership")
    }

    fn release_reaped(&mut self) {
        self.child.take();
        self.askpass_launcher.take();
        self.reaper_permit.take();
    }

    async fn terminate_and_reap(&mut self) {
        let _ = self.child_mut().start_kill();
        let waited =
            tokio::time::timeout(SSH_TUNNEL_SHUTDOWN_TIMEOUT, self.child_mut().wait()).await;
        if matches!(waited, Ok(Ok(_))) {
            self.release_reaped();
            return;
        }
        self.transfer_to_reaper();
    }

    /// Waits for the child and collects its output. Both pipes are read while
    /// the child runs, keeping the last `SSH_OUTPUT_CAP` bytes of each. After
    /// the child exits, reading continues until end of file or until the
    /// pipes have been idle for `SSH_OUTPUT_DRAIN_GRACE`. Output cut short is
    /// marked at the end of stderr.
    ///
    /// At `deadline` it fails with `WaitError::DeadlinePassed` and leaves the
    /// still-running child to the caller. Whenever reading stops short, any pipe read still in
    /// flight is settled first (`settle_unfinished_reads`).
    async fn wait_with_output(
        &mut self,
        deadline: Option<tokio::time::Instant>,
    ) -> Result<std::process::Output, WaitError> {
        enum FirstDone<S, D> {
            Status(S),
            Drained(D),
        }

        let mut stdout = self.child_mut().stdout.take();
        let mut stderr = self.child_mut().stderr.take();
        let mut stdout_bytes = CappedOutput::default();
        let mut stderr_bytes = CappedOutput::default();
        let activity = OutputActivity::new();
        let mut shutdown = self
            .reaper_permit
            .as_ref()
            .expect("managed SSH child retains bounded cleanup ownership")
            .shutdown_receiver();
        // The drain after exit honours the same shutdown signal as the wait.
        let mut drain_shutdown = shutdown.clone();
        let child = self.child.as_mut().expect("managed SSH child is live");
        let finished = {
            let status_task = async {
                tokio::select! {
                    status = child.wait() => status.map(|status| (status, false)),
                    _ = wait_for_ssh_shutdown(&mut shutdown) => {
                        let _ = child.start_kill();
                        child.wait().await.map(|status| (status, true))
                    }
                }
            };
            let drain = async {
                tokio::try_join!(
                    stdout_bytes.read_to_end(stdout.as_mut(), &activity),
                    stderr_bytes.read_to_end(stderr.as_mut(), &activity),
                )
            };
            tokio::pin!(status_task, drain);
            let run = async {
                let first = tokio::select! {
                    status = &mut status_task => FirstDone::Status(status),
                    drained = &mut drain => FirstDone::Drained(drained),
                };
                match first {
                    FirstDone::Status(status) => tokio::select! {
                        drained = drain_until_idle(&mut drain, &activity, None) => {
                            (status, drained, false)
                        }
                        _ = wait_for_ssh_shutdown(&mut drain_shutdown) => (status, None, true),
                    },
                    FirstDone::Drained(drained) => ((&mut status_task).await, Some(drained), false),
                }
            };
            run_before_deadline(deadline, run).await
        };
        // The readers are gone. A read they left in flight would keep a
        // Windows pool thread until a descendant holding the pipe exits.
        let mut unfinished: Vec<&mut dyn CancellablePipe> = Vec::new();
        if !stdout_bytes.finished
            && let Some(pipe) = stdout.as_mut()
        {
            unfinished.push(pipe);
        }
        if !stderr_bytes.finished
            && let Some(pipe) = stderr.as_mut()
        {
            unfinished.push(pipe);
        }
        settle_unfinished_reads(unfinished).await;
        let Some((status, drained, drain_interrupted)) = finished else {
            return Err(WaitError::DeadlinePassed);
        };
        let (status, interrupted) = status?;
        let interrupted = interrupted || drain_interrupted;
        self.release_reaped();
        if interrupted {
            return Err(WaitError::Io(io::Error::new(
                io::ErrorKind::Interrupted,
                "SSH process owner is shutting down",
            )));
        }
        let reached_end = match drained {
            Some(Ok(_)) => true,
            Some(Err(error)) => return Err(error.into()),
            None => false,
        };
        let (stdout, stdout_truncated) = stdout_bytes.finish();
        let (mut stderr, stderr_truncated) = stderr_bytes.finish();
        if !reached_end || stdout_truncated || stderr_truncated {
            stderr.extend_from_slice(format!("\n{SSH_OUTPUT_CUT_OFF}").as_bytes());
        }
        Ok(std::process::Output {
            status,
            stdout,
            stderr,
        })
    }

    fn transfer_to_reaper(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        let askpass_launcher = self.askpass_launcher.take();
        let reaper_permit = self.reaper_permit.take();
        let _ = child.start_kill();
        if let (Some(askpass_launcher), Some(reaper_permit)) = (askpass_launcher, reaper_permit) {
            reaper_permit.spawn_reap(child, askpass_launcher);
        } else {
            drop(child);
        }
    }

    fn shutdown_receiver(&self) -> watch::Receiver<bool> {
        self.reaper_permit
            .as_ref()
            .expect("managed SSH child retains bounded cleanup ownership")
            .shutdown_receiver()
    }
}

impl Drop for ManagedSshChild {
    fn drop(&mut self) {
        self.transfer_to_reaper();
    }
}

async fn wait_for_ssh_shutdown(shutdown: &mut watch::Receiver<bool>) {
    loop {
        let closed = *shutdown.borrow_and_update();
        if closed {
            return;
        }
        if shutdown.changed().await.is_err() {
            return;
        }
    }
}

fn spawn_managed_ssh_child(
    mut command: Command,
    askpass_launcher: SshAskpassLauncher,
    operation: &str,
) -> Result<ManagedSshChild, String> {
    let reaper_permit = askpass_launcher.reserve_child()?;
    isolate_appimage_environment(&mut command);
    let child = command
        .spawn()
        .map_err(|error| format!("Failed to {operation}: {error}"))?;
    Ok(ManagedSshChild::new(child, askpass_launcher, reaper_permit))
}

fn set_ssh_askpass_directory_permissions(directory: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("Failed to chmod SSH askpass directory: {error}"))?;
    }
    #[cfg(not(unix))]
    let _ = directory;
    Ok(())
}

fn write_askpass_file(path: &Path, contents: &str, mode: Option<u32>) -> Result<(), String> {
    let existing = fs::read_to_string(path).ok();
    if existing.as_deref() != Some(contents) {
        fs::write(path, contents)
            .map_err(|error| format!("Failed to write SSH askpass helper: {error}"))?;
    }
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .map_err(|error| format!("Failed to chmod SSH askpass helper: {error}"))?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    Ok(())
}

/// The launch script's readiness limits, in whole seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RemoteLaunchLimits {
    /// How long a server the script started gets to answer.
    ready: u64,
    /// How long a live recorded server gets to answer before it is replaced.
    reuse: u64,
}

const REMOTE_LAUNCH_LIMITS: RemoteLaunchLimits = RemoteLaunchLimits {
    ready: REMOTE_READY_TIMEOUT_SECS,
    reuse: REMOTE_REUSE_READY_TIMEOUT_SECS,
};

fn build_remote_launch_script() -> String {
    build_remote_launch_script_with(REMOTE_LAUNCH_LIMITS, DEFAULT_REMOTE_PORT)
}

/// The launch script with `limits`, scanning for a free port from
/// `default_port` when no port is recorded.
fn build_remote_launch_script_with(limits: RemoteLaunchLimits, default_port: u16) -> String {
    REMOTE_LAUNCH_SCRIPT
        .replace("@@DEFAULT_REMOTE_PORT@@", &default_port.to_string())
        .replace(
            "@@REMOTE_PORT_SCAN_WINDOW@@",
            &REMOTE_PORT_SCAN_WINDOW.to_string(),
        )
        .replace("@@REMOTE_REUSE_READY_TIMEOUT@@", &limits.reuse.to_string())
        .replace("@@REMOTE_READY_TIMEOUT@@", &limits.ready.to_string())
}

/// The remote pairing watchdog's bound for a local pairing `deadline`: the
/// deadline plus `SSH_PAIRING_WATCHDOG_MARGIN`, rounded up to whole seconds,
/// the resolution of the host clock the script reads. It is capped at
/// `i32::MAX` seconds (68 years), so the script's shell arithmetic can add it
/// to the current time without overflowing.
fn pairing_watchdog_bound(deadline: Duration) -> u64 {
    let bound = deadline.saturating_add(SSH_PAIRING_WATCHDOG_MARGIN);
    bound
        .as_secs()
        .saturating_add(u64::from(bound.subsec_nanos() > 0))
        .min(i32::MAX as u64)
}

fn build_remote_stop_script() -> String {
    build_remote_stop_script_with(REMOTE_STOP_WAIT_SECS)
}

/// The stop script, waiting up to `wait` seconds for the server to exit.
fn build_remote_stop_script_with(wait: u64) -> String {
    REMOTE_STOP_SCRIPT.replace("@@REMOTE_STOP_WAIT@@", &wait.to_string())
}

/// The remote scripts the manager runs, each with its own deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteOperation {
    Pairing,
    Launch,
    Stop,
}

/// The SSH program and deadline for one remote script operation.
#[derive(Debug, Clone)]
struct RemoteScriptRunner {
    program: String,
    #[cfg(test)]
    ssh_config_file_for_test: Option<PathBuf>,
    deadline: Duration,
    /// The SSH I/O runtime, where the script's child runs.
    io_runtime: tokio::runtime::Handle,
}

enum RemoteScriptFailure {
    /// The SSH child is still running and must be terminated.
    Write(String),
    /// Waiting failed; the child is already reaped, or goes to the retained
    /// reaper when dropped.
    Wait(String),
    TimedOut,
}

/// The error for a remote script that outlived its deadline. The
/// `[ssh_timeout:<operation>]` prefix is part of the bridge contract: the
/// renderer classifies it as transient and owns the user-facing copy.
fn remote_script_timeout_error(operation: &str, deadline: Duration) -> String {
    let limit = if deadline.subsec_millis() == 0 && deadline.as_secs() > 0 {
        format!("{} seconds", deadline.as_secs())
    } else {
        format!("{} ms", deadline.as_millis())
    };
    let what = match operation {
        "pairing" => "issue a pairing credential",
        "launch" => "start BiBCode",
        "stop" => "stop BiBCode",
        _ => "finish the SSH command",
    };
    format!("[ssh_timeout:{operation}] The remote host did not {what} within {limit}.")
}

async fn run_remote_ssh_script(
    runner: &RemoteScriptRunner,
    target: &SshEnvironmentTarget,
    script: &str,
    script_args: &[String],
    auth: &SshAuthOptions,
    askpass_launcher: SshAskpassLauncher,
    operation: &str,
) -> Result<String, String> {
    let host_spec = build_ssh_host_spec(target)?;
    let mut args = base_ssh_args_with_auth(target, auth);
    #[cfg(test)]
    if let Some(path) = &runner.ssh_config_file_for_test {
        args.splice(
            0..0,
            ["-F".to_string(), path.to_string_lossy().into_owned()],
        );
    }
    args.push(host_spec);
    args.extend(["sh".to_string(), "-s".to_string(), "--".to_string()]);
    args.extend(script_args.iter().cloned());
    let exchange = RemoteScriptExchange {
        program: runner.program.clone(),
        deadline: runner.deadline,
        args,
        environment: build_ssh_child_environment(auth, askpass_launcher.path()),
        script: script.to_string(),
        operation: operation.to_string(),
    };
    run_on_ssh_io(&runner.io_runtime, exchange.run(askpass_launcher)).await
}

/// Why `ManagedSshChild::wait_with_output` returned no output.
#[derive(Debug)]
enum WaitError {
    /// The caller's deadline passed first; the child is still running. The
    /// only source of `[ssh_timeout:…]`: an I/O error of the `TimedOut` kind
    /// is not a deadline (on Windows a cancelled pipe read decodes to it).
    DeadlinePassed,
    /// Waiting or reading failed, or (`Interrupted`) the owner is shutting
    /// down.
    Io(io::Error),
}

impl From<io::Error> for WaitError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// How a failed wait for a remote script is reported, whatever the error's
/// kind: only `WaitError::DeadlinePassed` becomes a timeout.
fn wait_failure(error: io::Error, operation: &str) -> RemoteScriptFailure {
    RemoteScriptFailure::Wait(format!(
        "Failed to wait for SSH {operation} command: {error}"
    ))
}

/// One remote script over SSH, owned so it can run on the SSH I/O runtime.
struct RemoteScriptExchange {
    program: String,
    deadline: Duration,
    args: Vec<String>,
    environment: HashMap<String, String>,
    script: String,
    operation: String,
}

impl RemoteScriptExchange {
    /// Starts ssh, writes the script to its stdin, and returns its stdout.
    /// At the deadline the SSH child is terminated and reaped, and the error
    /// is `remote_script_timeout_error`.
    async fn run(self, askpass_launcher: SshAskpassLauncher) -> Result<String, String> {
        let Self {
            program,
            deadline,
            args,
            environment,
            script,
            operation,
        } = self;
        // `None` only for a deadline too far away to represent: no limit.
        let expires_at = tokio::time::Instant::now().checked_add(deadline);
        let mut command = Command::new(&program);
        configure_background_command(&mut command);
        command
            .args(args)
            .envs(environment)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = spawn_managed_ssh_child(
            command,
            askpass_launcher,
            &format!("run SSH {operation} command"),
        )?;
        let mut stdin = child
            .child_mut()
            .stdin
            .take()
            .ok_or_else(|| format!("SSH {operation} command did not expose stdin."))?;
        let mut shutdown = child.shutdown_receiver();
        let written = tokio::select! {
            result = run_before_deadline(expires_at, stdin.write_all(script.as_bytes())) => match result {
                // ssh stopped reading its stdin, usually because it exited
                // (it could not connect): its status and stderr say why.
                Some(Err(error)) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
                Some(result) => result.map_err(|error| {
                    RemoteScriptFailure::Write(format!(
                        "Failed to write SSH {operation} script: {error}"
                    ))
                }),
                None => Err(RemoteScriptFailure::TimedOut),
            },
            _ = wait_for_ssh_shutdown(&mut shutdown) => Err(RemoteScriptFailure::Write(
                "SSH process owner is shutting down.".to_string(),
            )),
        };
        drop(stdin);
        let output = match written {
            Ok(()) => child
                .wait_with_output(expires_at)
                .await
                .map_err(|error| match error {
                    WaitError::DeadlinePassed => RemoteScriptFailure::TimedOut,
                    WaitError::Io(error) => wait_failure(error, &operation),
                }),
            Err(failure) => Err(failure),
        };
        let output = match output {
            Ok(output) => output,
            Err(RemoteScriptFailure::Write(error)) => {
                child.terminate_and_reap().await;
                return Err(error);
            }
            Err(RemoteScriptFailure::Wait(error)) => return Err(error),
            Err(RemoteScriptFailure::TimedOut) => {
                child.terminate_and_reap().await;
                return Err(remote_script_timeout_error(&operation, deadline));
            }
        };
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "SSH {operation} command failed with status {}: {}",
                output.status,
                stderr.trim()
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

fn last_non_empty_line(output: &str) -> Option<&str> {
    output.lines().map(str::trim).rfind(|line| !line.is_empty())
}

pub fn parse_remote_pairing_credential(output: &str) -> Result<String, String> {
    let line = last_non_empty_line(output)
        .ok_or_else(|| "SSH pairing did not return a credential.".to_string())?;
    let value: Value = serde_json::from_str(line)
        .map_err(|error| format!("SSH pairing returned unparseable output: {error}"))?;
    let credential = value
        .get("credential")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if credential.is_empty() {
        return Err("SSH pairing command returned an invalid credential.".to_string());
    }
    Ok(credential)
}

pub fn parse_remote_launch_result(output: &str) -> Result<RemoteLaunchResult, String> {
    let line = last_non_empty_line(output)
        .ok_or_else(|| "SSH launch did not return a remote port.".to_string())?;
    let value: RemoteLaunchResultDocument = serde_json::from_str(line)
        .map_err(|error| format!("SSH launch returned unparseable output: {error}"))?;
    let remote_port = u16::try_from(value.remote_port)
        .ok()
        .filter(|port| *port > 0)
        .ok_or_else(|| {
            format!(
                "SSH launch returned an invalid remote port: {}.",
                value.remote_port
            )
        })?;
    let server_kind = value.server_kind.unwrap_or_else(|| "managed".to_string());
    if !matches!(server_kind.as_str(), "external" | "managed") {
        return Err(format!(
            "SSH launch returned an invalid remote server kind: {server_kind}."
        ));
    }
    Ok(RemoteLaunchResult {
        remote_port,
        server_kind,
    })
}

fn parse_remote_stop_result(output: &str) -> Result<bool, &'static str> {
    let line = last_non_empty_line(output).ok_or("SSH stop did not return a result.")?;
    let value: Value =
        serde_json::from_str(line).map_err(|_| "SSH stop returned unparseable output.")?;
    value
        .get("stopped")
        .and_then(Value::as_bool)
        .ok_or("SSH stop returned an invalid stopped result.")
}

async fn launch_or_reuse_remote_server(
    runner: &RemoteScriptRunner,
    target: &SshEnvironmentTarget,
    auth: &SshAuthOptions,
    askpass_launcher: SshAskpassLauncher,
) -> Result<RemoteLaunchResult, String> {
    let state_key = remote_state_key(target);
    let output = run_remote_ssh_script(
        runner,
        target,
        &build_remote_launch_script(),
        &[state_key],
        auth,
        askpass_launcher,
        "launch",
    )
    .await?;
    parse_remote_launch_result(&output)
}

async fn stop_remote_server(
    runner: &RemoteScriptRunner,
    target: &SshEnvironmentTarget,
    auth: &SshAuthOptions,
    askpass_launcher: SshAskpassLauncher,
) -> Result<(), String> {
    let state_key = remote_state_key(target);
    let output = run_remote_ssh_script(
        runner,
        target,
        &build_remote_stop_script(),
        &[state_key],
        auth,
        askpass_launcher,
        "stop",
    )
    .await?;
    match parse_remote_stop_result(&output) {
        Ok(true) => {}
        Ok(false) => tracing::warn!(
            environment = %target.alias,
            "The remote BiBCode server is still running; its record was kept for the next launch."
        ),
        Err(error) => tracing::warn!(
            environment = %target.alias,
            error,
            "Could not confirm whether the remote BiBCode server stopped."
        ),
    }
    Ok(())
}

async fn issue_remote_pairing_token(
    runner: &RemoteScriptRunner,
    target: &SshEnvironmentTarget,
    auth: &SshAuthOptions,
    askpass_launcher: SshAskpassLauncher,
) -> Result<String, String> {
    let bound = pairing_watchdog_bound(runner.deadline).to_string();
    let output = run_remote_ssh_script(
        runner,
        target,
        REMOTE_PAIRING_SCRIPT,
        &[bound],
        auth,
        askpass_launcher,
        "pairing",
    )
    .await?;
    parse_remote_pairing_credential(&output)
}

/// What a forwarding SSH child serves, which decides how it proves ready.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SshForwardKind {
    /// The environment tunnel: the remote server answers through it.
    Tunnel,
    /// A preview port forward: its local end accepts TCP connections.
    PortForward,
}

async fn start_ssh_tunnel(
    plan: &SshEnvironmentLaunchPlan,
    auth: &SshAuthOptions,
    askpass_launcher: SshAskpassLauncher,
    kind: SshForwardKind,
) -> Result<ManagedSshChild, String> {
    let mut command = Command::new(&plan.program);
    configure_background_command(&mut command);
    command
        .args(&plan.args)
        .envs(build_ssh_child_environment(auth, askpass_launcher.path()))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let operation = match kind {
        SshForwardKind::Tunnel => "start SSH tunnel",
        SshForwardKind::PortForward => "start SSH port forward",
    };
    let mut child = spawn_managed_ssh_child(command, askpass_launcher, operation)?;

    let mut shutdown = child.shutdown_receiver();
    let ready = async {
        match kind {
            SshForwardKind::Tunnel => {
                wait_for_ssh_tunnel_ready(child.child_mut(), &plan.http_base_url).await
            }
            SshForwardKind::PortForward => {
                wait_for_ssh_port_forward_ready(child.child_mut(), plan.local_port).await
            }
        }
    };
    let ready_result = tokio::select! {
        result = ready => result,
        _ = wait_for_ssh_shutdown(&mut shutdown) => {
            Err("SSH process owner is shutting down.".to_string())
        }
    };
    if let Err(error) = ready_result {
        child.terminate_and_reap().await;
        return Err(error);
    }

    Ok(child)
}

/// Discards a long-lived forward's stderr on the SSH I/O runtime, so the
/// messages ssh writes for each channel it cannot open (nothing listening on
/// the remote port) never fill the pipe and block it. The drain ends at end
/// of file or when the returned sender is dropped with its forward; then a
/// read still in flight is settled, since a descendant that inherited the
/// pipe can keep it open after ssh exits.
fn drain_forward_stderr(
    io_runtime: &tokio::runtime::Handle,
    child: &mut ManagedSshChild,
) -> oneshot::Sender<()> {
    let (stop, stopped) = oneshot::channel::<()>();
    if let Some(mut stderr) = child.child_mut().stderr.take() {
        io_runtime.spawn(async move {
            let mut sink = tokio::io::sink();
            let finished = tokio::select! {
                _ = tokio::io::copy(&mut stderr, &mut sink) => true,
                _ = stopped => false,
            };
            if !finished {
                settle_unfinished_reads(vec![&mut stderr]).await;
            }
        });
    }
    stop
}

/// One readiness probe of the remote server through a cached tunnel.
async fn tunnel_endpoint_responds(http_base_url: &str) -> bool {
    let Ok(mut url) = url::Url::parse(http_base_url) else {
        return false;
    };
    url.set_path(SSH_READY_PATH);
    url.set_query(None);
    url.set_fragment(None);
    let Ok(client) = reqwest::Client::builder()
        .timeout(SSH_READY_REQUEST_TIMEOUT)
        .build()
    else {
        return false;
    };
    client
        .get(url)
        .send()
        .await
        .is_ok_and(|response| response.status().is_success())
}

async fn wait_for_ssh_tunnel_ready(child: &mut Child, http_base_url: &str) -> Result<(), String> {
    let mut url = url::Url::parse(http_base_url)
        .map_err(|error| format!("Could not parse SSH tunnel URL: {error}"))?;
    url.set_path(SSH_READY_PATH);
    url.set_query(None);
    url.set_fragment(None);
    let client = reqwest::Client::builder()
        .timeout(SSH_READY_REQUEST_TIMEOUT)
        .build()
        .map_err(|error| format!("Could not create SSH readiness client: {error}"))?;
    let start = std::time::Instant::now();
    let mut last_error = String::new();
    while start.elapsed() <= SSH_READY_TIMEOUT {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("Could not inspect SSH tunnel process: {error}"))?
        {
            let stderr = read_child_stderr(child).await;
            return Err(format!(
                "SSH tunnel exited before becoming ready with status {status}: {stderr}"
            ));
        }
        match client.get(url.clone()).send().await {
            Ok(response) if response.status().is_success() => return Ok(()),
            Ok(response) => {
                last_error = format!("HTTP {}", response.status().as_u16());
            }
            Err(error) => {
                last_error = error.to_string();
            }
        }
        tokio::time::sleep(SSH_READY_INTERVAL).await;
    }
    Err(format!(
        "SSH tunnel did not become ready at {http_base_url}: {last_error}"
    ))
}

/// The least recently used forward other than `keep`, by tunnel key and
/// remote port.
fn least_recent_forward<'a>(
    forwards: impl Iterator<Item = ((&'a str, u16), &'a SshPortForward)>,
    keep: (&str, u16),
) -> Option<(&'a str, u16)> {
    forwards
        .filter(|(id, _)| *id != keep)
        .min_by_key(|(_, forward)| forward.last_requested)
        .map(|(id, _)| id)
}

/// The local end of a preview forward: a port on `127.0.0.1`, and on `[::1]`
/// too when `ipv6`, so `localhost` cannot reach another local server on the
/// other family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ForwardBind {
    port: u16,
    ipv6: bool,
}

/// The bind for a preferred local port when it is free on IPv4 and, when the
/// client has IPv6 loopback, on IPv6. `None` means fall back to a random port.
fn probe_preferred_port(port: u16) -> Option<ForwardBind> {
    if port < 1024 {
        return None;
    }
    std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).ok()?;
    let ipv6 = match std::net::TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, port)) {
        Ok(_) => true,
        // No IPv6 loopback at all: nothing else can answer `localhost` there.
        Err(_) if std::net::TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, 0)).is_err() => false,
        Err(_) => return None,
    };
    Some(ForwardBind { port, ipv6 })
}

/// Whether a failed forward lost its local bind (another process took the
/// port after the probe), as opposed to an authentication or network failure.
fn is_forward_bind_failure(error: &str) -> bool {
    error.contains("Address already in use") || error.contains("Could not request local forwarding")
}

/// Polls until the forward's local port accepts a TCP connection while the
/// child is still running. ssh opens the listener only after authenticating,
/// and with `ExitOnForwardFailure` exits when it cannot bind.
async fn wait_for_ssh_port_forward_ready(child: &mut Child, local_port: u16) -> Result<(), String> {
    // Forwards behind a ProxyCommand or bastion need the tunnel's allowance.
    let deadline = tokio::time::Instant::now() + SSH_READY_TIMEOUT;
    loop {
        let connected = tokio::time::timeout_at(
            deadline,
            tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, local_port)),
        )
        .await
        .is_ok_and(|connection| connection.is_ok());
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("Could not inspect SSH port forward process: {error}"))?
        {
            let stderr = read_child_stderr(child).await;
            return Err(format!(
                "SSH port forward exited before becoming ready with status {status}: {stderr}"
            ));
        }
        if connected {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "SSH port forward did not become ready at 127.0.0.1:{local_port}."
            ));
        }
        tokio::time::sleep(SSH_FORWARD_READY_INTERVAL).await;
    }
}

/// Collects an exited child's stderr for an error message: reading stops at
/// end of file, after `SSH_OUTPUT_DRAIN_GRACE` without new bytes, or after
/// `SSH_STDERR_COLLECT_LIMIT` in all, and keeps the last `SSH_OUTPUT_CAP`
/// bytes. Output cut short is marked.
async fn read_child_stderr(child: &mut Child) -> String {
    let Some(mut stderr) = child.stderr.take() else {
        return String::new();
    };
    let mut output = CappedOutput::default();
    let activity = OutputActivity::new();
    let limit = tokio::time::Instant::now() + SSH_STDERR_COLLECT_LIMIT;
    let reached_end = {
        let read = output.read_to_end(Some(&mut stderr), &activity);
        tokio::pin!(read);
        matches!(
            drain_until_idle(&mut read, &activity, Some(limit)).await,
            Some(Ok(()))
        )
    };
    if !output.finished {
        settle_unfinished_reads(vec![&mut stderr]).await;
    }
    let (bytes, truncated) = output.finish();
    let text = String::from_utf8_lossy(&bytes);
    let text = text.trim();
    match (reached_end && !truncated, text.is_empty()) {
        (true, _) => text.to_string(),
        (false, true) => SSH_OUTPUT_CUT_OFF.to_string(),
        (false, false) => format!("{text}\n{SSH_OUTPUT_CUT_OFF}"),
    }
}

/// When a child's pipes last delivered bytes, shared by its pipe readers.
struct OutputActivity {
    started: tokio::time::Instant,
    last_millis: AtomicU64,
}

impl OutputActivity {
    fn new() -> Self {
        Self {
            started: tokio::time::Instant::now(),
            last_millis: AtomicU64::new(0),
        }
    }

    fn touch(&self) {
        let elapsed = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.last_millis.fetch_max(elapsed, Ordering::Relaxed);
    }

    fn last(&self) -> tokio::time::Instant {
        self.started + Duration::from_millis(self.last_millis.load(Ordering::Relaxed))
    }
}

/// Polls `drain` (pipe readers that report to `activity`) until it finishes,
/// or until the pipes have been idle for `SSH_OUTPUT_DRAIN_GRACE`, or until
/// `limit`. The idle window starts no earlier than the call, so a read still
/// being scheduled when the child exits gets a full window. Bytes read before
/// giving up stay with the readers. Returns `None` when it gave up.
async fn drain_until_idle<F: std::future::Future + Unpin>(
    drain: &mut F,
    activity: &OutputActivity,
    limit: Option<tokio::time::Instant>,
) -> Option<F::Output> {
    activity.touch();
    loop {
        let idle_deadline = activity.last() + SSH_OUTPUT_DRAIN_GRACE;
        let wake = limit.map_or(idle_deadline, |limit| idle_deadline.min(limit));
        if let Ok(output) = tokio::time::timeout_at(wake, &mut *drain).await {
            return Some(output);
        }
        let still_arriving = activity.last() + SSH_OUTPUT_DRAIN_GRACE > idle_deadline;
        let within_limit = limit.is_none_or(|limit| tokio::time::Instant::now() < limit);
        if !still_arriving || !within_limit {
            return None;
        }
    }
}

/// The tail of a child's output. It keeps the last `SSH_OUTPUT_CAP` bytes and
/// discards older ones while still reading, so a chatty child neither grows
/// memory without bound nor blocks on a full pipe.
#[derive(Default)]
struct CappedOutput {
    bytes: Vec<u8>,
    truncated: bool,
    /// Whether `read_to_end` returned (end of file or an error). While it is
    /// false after the future was dropped, a read may still be in flight.
    finished: bool,
}

impl CappedOutput {
    /// Reads `reader` to end of file. Cancel-safe: every chunk read before the
    /// future is dropped has already been kept.
    async fn read_to_end<R: tokio::io::AsyncRead + Unpin>(
        &mut self,
        reader: Option<&mut R>,
        activity: &OutputActivity,
    ) -> io::Result<()> {
        let result = match reader {
            Some(reader) => self.read_chunks(reader, activity).await,
            None => Ok(()),
        };
        self.finished = true;
        result
    }

    async fn read_chunks<R: tokio::io::AsyncRead + Unpin>(
        &mut self,
        reader: &mut R,
        activity: &OutputActivity,
    ) -> io::Result<()> {
        let mut chunk = vec![0_u8; 8 * 1024];
        loop {
            let read = reader.read(&mut chunk).await?;
            if read == 0 {
                return Ok(());
            }
            activity.touch();
            self.bytes.extend_from_slice(&chunk[..read]);
            // Trim in batches so a long stream costs amortised O(1) per byte.
            if self.bytes.len() > 2 * SSH_OUTPUT_CAP {
                self.keep_tail();
            }
        }
    }

    fn keep_tail(&mut self) {
        if self.bytes.len() > SSH_OUTPUT_CAP {
            let excess = self.bytes.len() - SSH_OUTPUT_CAP;
            self.bytes.drain(..excess);
            self.truncated = true;
        }
    }

    /// The kept bytes and whether older output was discarded.
    fn finish(mut self) -> (Vec<u8>, bool) {
        self.keep_tail();
        (self.bytes, self.truncated)
    }
}

/// A child pipe whose pending read the OS can cancel.
trait CancellablePipe: tokio::io::AsyncRead + Unpin + Send {
    /// Asks the OS to end the read pending on this pipe, if there is one.
    fn cancel_pending_read(&self);
}

impl CancellablePipe for tokio::process::ChildStdout {
    fn cancel_pending_read(&self) {
        cancel_pending_pipe_read(self);
    }
}

impl CancellablePipe for tokio::process::ChildStderr {
    fn cancel_pending_read(&self) {
        cancel_pending_pipe_read(self);
    }
}

/// `CancelIoEx` on the pipe's handle, which completes every read this
/// process has pending on it with `ERROR_OPERATION_ABORTED`. The standard
/// library opens child pipes for overlapped I/O, and the blocking read Tokio
/// runs on a pool thread waits on the handle, so it returns once its read is
/// cancelled. A read not issued yet is unaffected (`ERROR_NOT_FOUND`), which
/// is why `settle_pipe_reads` repeats the call.
#[cfg(windows)]
fn cancel_pending_pipe_read(pipe: &impl std::os::windows::io::AsRawHandle) {
    // SAFETY: the caller holds a reference to the pipe, so its handle stays
    // open for this call; `CancelIoEx` only marks this process's pending I/O
    // on that handle as cancelled and touches no memory of ours.
    unsafe {
        windows_sys::Win32::System::IO::CancelIoEx(pipe.as_raw_handle(), std::ptr::null());
    }
}

/// Outside Windows a pending pipe read belongs to the I/O driver, and
/// dropping it ends it: there is nothing to cancel.
#[cfg(not(windows))]
fn cancel_pending_pipe_read<T>(_pipe: &T) {}

/// Settles the pipe reads a drain left in flight when it stopped short (the
/// pipes went idle, the deadline passed, or the owner is shutting down).
///
/// On Windows a child pipe read occupies a blocking-pool thread until it
/// returns, and dropping the read does not end it, so a descendant that keeps
/// the pipe open would hold that thread until it exits. Elsewhere reads use
/// the I/O driver, dropping them is enough, and this returns at once.
async fn settle_unfinished_reads(pipes: Vec<&mut dyn CancellablePipe>) {
    if SETTLE_PIPE_READS && !pipes.is_empty() {
        settle_pipe_reads(pipes, SSH_PIPE_SETTLE_LIMIT).await;
    }
}

/// Cancels each pipe's pending read and polls it until it returns, repeating
/// the cancel every `SSH_PIPE_SETTLE_INTERVAL` for up to `limit`: a read still
/// queued for a pool thread has no pending I/O to cancel until it starts, and
/// then it would block until end of file. A pipe whose read has returned (cut
/// off, at end of file, or with bytes nobody reads any more) is never polled
/// again, which would start a new read. Returns how many reads were still
/// pending at the limit.
async fn settle_pipe_reads(mut pipes: Vec<&mut dyn CancellablePipe>, limit: Duration) -> usize {
    let limit = tokio::time::Instant::now() + limit;
    let mut scratch = [0_u8; 1024];
    while !pipes.is_empty() {
        for pipe in &pipes {
            pipe.cancel_pending_read();
        }
        let mut pending = Vec::with_capacity(pipes.len());
        for pipe in pipes {
            let read = tokio::time::timeout(SSH_PIPE_SETTLE_INTERVAL, pipe.read(&mut scratch));
            if read.await.is_err() {
                pending.push(pipe);
            }
        }
        pipes = pending;
        if tokio::time::Instant::now() >= limit {
            break;
        }
    }
    if !pipes.is_empty() {
        tracing::warn!(
            pending = pipes.len(),
            "SSH pipe reads were still pending after cancellation"
        );
    }
    pipes.len()
}

/// The runtime that owns SSH child I/O.
///
/// On Windows Tokio reads and writes child pipes on its blocking pool. On the
/// shared Tauri runtime that pool also serves the in-process server's
/// blocking work and its provider and Git pipes, so a busy pool could delay
/// SSH output past the drain's idle window and cut it off. Script children
/// are spawned, fed, drained, waited on and reaped here instead, on one worker
/// thread and a blocking pool of their own, and tunnels are spawned and their
/// stderr read here. A published tunnel's terminate-and-reap is awaited on
/// the caller's runtime; a child wait needs no blocking-pool thread (a
/// registered wait on Windows, this runtime's driver elsewhere). Prompts and
/// events stay on the caller's runtime. It starts on first use and stops with
/// `SshEnvironmentManager::shutdown`, or when dropped, without blocking, since
/// a runtime dropped inside async code panics.
struct SshIoRuntime {
    max_blocking_threads: usize,
    state: Mutex<SshIoRuntimeState>,
}

enum SshIoRuntimeState {
    NotStarted,
    Running(tokio::runtime::Runtime),
    Stopped,
}

impl SshIoRuntime {
    fn new(max_blocking_threads: usize) -> Self {
        Self {
            max_blocking_threads,
            state: Mutex::new(SshIoRuntimeState::NotStarted),
        }
    }

    /// The runtime's handle, starting it on first use. Fails once stopped.
    fn handle(&self) -> Result<tokio::runtime::Handle, String> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(*state, SshIoRuntimeState::NotStarted) {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .max_blocking_threads(self.max_blocking_threads)
                .thread_name("bibcode-ssh-io")
                .enable_all()
                .build()
                .map_err(|error| format!("Could not start the SSH I/O runtime: {error}"))?;
            *state = SshIoRuntimeState::Running(runtime);
        }
        match &*state {
            SshIoRuntimeState::Running(runtime) => Ok(runtime.handle().clone()),
            _ => Err("SSH process owner is shutting down.".to_string()),
        }
    }

    /// Stops the runtime for good without waiting for its threads, which is
    /// safe inside async code. Tasks still on it are dropped.
    fn stop(&self) {
        let previous = std::mem::replace(
            &mut *self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            SshIoRuntimeState::Stopped,
        );
        if let SshIoRuntimeState::Running(runtime) = previous {
            runtime.shutdown_background();
        }
    }
}

impl Drop for SshIoRuntime {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Runs `future` to completion, or until `deadline` when there is one;
/// `None` when the deadline came first.
async fn run_before_deadline<F: std::future::Future>(
    deadline: Option<tokio::time::Instant>,
    future: F,
) -> Option<F::Output> {
    match deadline {
        Some(deadline) => tokio::time::timeout_at(deadline, future).await.ok(),
        None => Some(future.await),
    }
}

/// Aborts the task when the waiting future is dropped.
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Runs `operation` on the SSH I/O runtime and waits for it. Dropping the
/// returned future aborts the operation, whose SSH child then goes to the
/// retained reaper as on any other cancellation. An operation the stopped
/// runtime never ran fails like a refused spawn.
async fn run_on_ssh_io<T: Send + 'static>(
    io_runtime: &tokio::runtime::Handle,
    operation: impl std::future::Future<Output = Result<T, String>> + Send + 'static,
) -> Result<T, String> {
    let mut task = AbortOnDrop(io_runtime.spawn(operation));
    match (&mut task.0).await {
        Ok(result) => result,
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(_cancelled) => Err("SSH process owner is shutting down.".to_string()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshPasswordRequest {
    pub destination: String,
    pub username: Option<String>,
    pub prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SshPasswordPromptPayload {
    pub request_id: String,
    pub destination: String,
    pub username: Option<String>,
    pub prompt: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshPasswordPromptResolution {
    pub request_id: String,
    pub password: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SshPasswordPromptRequestError {
    Presentation {
        request_id: String,
        destination: String,
        operation: &'static str,
        message: String,
    },
    TimedOut {
        request_id: String,
        destination: String,
    },
    Cancelled {
        request_id: String,
        destination: String,
    },
    ServiceStopped {
        request_id: String,
        destination: String,
    },
}

impl std::fmt::Display for SshPasswordPromptRequestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Presentation {
                destination,
                operation,
                message,
                ..
            } => write!(
                formatter,
                "Failed to present SSH password prompt for {destination} during {operation}: {message}"
            ),
            Self::TimedOut { destination, .. } => {
                write!(formatter, "SSH authentication timed out for {destination}.")
            }
            Self::Cancelled { destination, .. } => {
                write!(formatter, "SSH authentication cancelled for {destination}.")
            }
            Self::ServiceStopped { .. } => {
                formatter.write_str("SSH password prompt service stopped.")
            }
        }
    }
}

impl std::error::Error for SshPasswordPromptRequestError {}

/// The bridge error for a failed password prompt. A cancellation carries the
/// `[ssh_cancelled]` prefix, part of the bridge contract like
/// `[ssh_http:<status>]`, so the renderer can tell the user's own cancel from
/// any other failure without matching words that a host name could contain.
fn password_prompt_failure(error: SshPasswordPromptRequestError) -> String {
    match error {
        SshPasswordPromptRequestError::Cancelled { .. } => format!("[ssh_cancelled] {error}"),
        error => error.to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SshPasswordPromptResolveError {
    InvalidRequestId,
    Expired { request_id: String },
}

impl std::fmt::Display for SshPasswordPromptResolveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRequestId => formatter.write_str("Invalid SSH password prompt id."),
            Self::Expired { .. } => {
                formatter.write_str("SSH password prompt expired. Try connecting again.")
            }
        }
    }
}

impl std::error::Error for SshPasswordPromptResolveError {}

type PendingPromptResult = Result<String, SshPasswordPromptRequestError>;

struct PendingSshPasswordPrompt {
    destination: String,
    sender: oneshot::Sender<PendingPromptResult>,
}

#[derive(Clone)]
pub struct SshPasswordPromptManager {
    pending: Arc<Mutex<HashMap<String, PendingSshPasswordPrompt>>>,
    timeout: Duration,
}

impl Default for SshPasswordPromptManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SshPasswordPromptManager {
    pub fn new() -> Self {
        Self::with_timeout(DEFAULT_SSH_PASSWORD_PROMPT_TIMEOUT)
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
            timeout,
        }
    }

    pub async fn request_password<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        request: SshPasswordRequest,
    ) -> PendingPromptResult {
        let request_id = Uuid::new_v4().simple().to_string();
        self.request_password_with(request_id, request, SystemTime::now(), |payload| {
            app.emit(SSH_PASSWORD_PROMPT_EVENT, payload)
                .map_err(|error| error.to_string())
        })
        .await
    }

    pub(crate) async fn request_password_with(
        &self,
        request_id: String,
        request: SshPasswordRequest,
        requested_at: SystemTime,
        emit: impl FnOnce(SshPasswordPromptPayload) -> Result<(), String>,
    ) -> PendingPromptResult {
        let expires_at = format_system_time(
            requested_at
                .checked_add(self.timeout)
                .unwrap_or(requested_at),
        );
        let payload = SshPasswordPromptPayload {
            request_id: request_id.clone(),
            destination: request.destination.clone(),
            username: request.username.clone(),
            prompt: request.prompt,
            expires_at,
        };
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = self.pending.lock().map_err(|error| {
                SshPasswordPromptRequestError::Presentation {
                    request_id: request_id.clone(),
                    destination: request.destination.clone(),
                    operation: "lock-pending-prompts",
                    message: error.to_string(),
                }
            })?;
            pending.insert(
                request_id.clone(),
                PendingSshPasswordPrompt {
                    destination: request.destination.clone(),
                    sender,
                },
            );
        }

        if let Err(message) = emit(payload) {
            self.remove_pending(&request_id);
            return Err(SshPasswordPromptRequestError::Presentation {
                request_id,
                destination: request.destination,
                operation: "send-prompt-request",
                message,
            });
        }

        match tokio::time::timeout(self.timeout, receiver).await {
            Ok(Ok(result)) => result,
            Ok(Err(_closed)) => Err(SshPasswordPromptRequestError::ServiceStopped {
                request_id,
                destination: request.destination,
            }),
            Err(_elapsed) => {
                self.remove_pending(&request_id);
                Err(SshPasswordPromptRequestError::TimedOut {
                    request_id,
                    destination: request.destination,
                })
            }
        }
    }

    pub fn resolve(
        &self,
        input: SshPasswordPromptResolution,
    ) -> Result<(), SshPasswordPromptResolveError> {
        let request_id = input.request_id.trim().to_string();
        if request_id.is_empty() {
            return Err(SshPasswordPromptResolveError::InvalidRequestId);
        }
        let Some(pending) = self.remove_pending(&request_id) else {
            return Err(SshPasswordPromptResolveError::Expired { request_id });
        };
        let result = match input.password {
            Some(password) => Ok(password),
            None => Err(SshPasswordPromptRequestError::Cancelled {
                request_id: request_id.clone(),
                destination: pending.destination.clone(),
            }),
        };
        let _ = pending.sender.send(result);
        Ok(())
    }

    fn remove_pending(&self, request_id: &str) -> Option<PendingSshPasswordPrompt> {
        self.pending.lock().ok()?.remove(request_id)
    }
}

fn format_system_time(system_time: SystemTime) -> String {
    let duration = system_time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = i64::try_from(duration.as_secs()).unwrap_or(i64::MAX);
    OffsetDateTime::from_unix_timestamp(seconds)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredSshHost {
    pub alias: String,
    pub hostname: String,
    pub username: Option<String>,
    pub port: Option<u16>,
    pub source: &'static str,
}

impl DiscoveredSshHost {
    pub fn to_value(&self) -> Value {
        json!({
            "alias": &self.alias,
            "hostname": &self.hostname,
            "username": &self.username,
            "port": self.port,
            "source": self.source,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SshConfigLineParseError {
    InvalidQuotes,
}

fn split_directive_args(value: &str) -> Result<Vec<String>, SshConfigLineParseError> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut characters = value.chars().peekable();

    while let Some(character) = characters.next() {
        if let Some(delimiter) = quote {
            match character {
                '\\' if characters.peek().is_some_and(|next| *next == delimiter) => {
                    current.push(delimiter);
                    characters.next();
                }
                value if value == delimiter => quote = None,
                value => current.push(value),
            }
            continue;
        }

        match character {
            '\\' if characters
                .peek()
                .is_some_and(|next| next.is_whitespace() || *next == '#') =>
            {
                current.push(
                    characters
                        .next()
                        .expect("peeked escaped value should exist"),
                );
            }
            '\'' | '"' => quote = Some(character),
            '#' if current.is_empty() => break,
            '#' => current.push(character),
            '=' if args.is_empty() && !current.is_empty() => {
                args.push(std::mem::take(&mut current));
            }
            '=' if args.len() == 1 && current.is_empty() => {}
            value if value.is_whitespace() => {
                if !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
            }
            value => current.push(value),
        }
    }

    if quote.is_some() {
        return Err(SshConfigLineParseError::InvalidQuotes);
    }
    if !current.is_empty() {
        args.push(current);
    }
    Ok(args)
}

fn has_ssh_pattern(value: &str) -> bool {
    value.contains('*') || value.contains('?') || value.starts_with('!')
}

fn expand_home_path(input: &str, home_dir: &Path) -> PathBuf {
    if input == "~" {
        return home_dir.to_path_buf();
    }
    if let Some(rest) = input
        .strip_prefix("~/")
        .or_else(|| input.strip_prefix("~\\"))
    {
        return home_dir.join(rest);
    }
    PathBuf::from(input)
}

fn resolve_ssh_config_include_pattern(include_pattern: &str, home_dir: &Path) -> PathBuf {
    let expanded_pattern = expand_home_path(include_pattern, home_dir);
    if expanded_pattern.is_absolute() {
        expanded_pattern
    } else {
        home_dir.join(SSH_DIRECTORY_NAME).join(expanded_pattern)
    }
}

fn wildcard_matches(pattern: &str, value: &str) -> bool {
    fn matches_inner(pattern: &[char], value: &[char]) -> bool {
        match pattern.split_first() {
            None => value.is_empty(),
            Some(('*', rest)) => {
                matches_inner(rest, value)
                    || (!value.is_empty() && matches_inner(pattern, &value[1..]))
            }
            Some(('?', rest)) => !value.is_empty() && matches_inner(rest, &value[1..]),
            Some((expected, rest)) => value
                .split_first()
                .is_some_and(|(actual, tail)| actual == expected && matches_inner(rest, tail)),
        }
    }

    matches_inner(
        &pattern.chars().collect::<Vec<_>>(),
        &value.chars().collect::<Vec<_>>(),
    )
}

fn expand_glob(pattern: &Path) -> io::Result<Vec<PathBuf>> {
    let pattern_text = pattern.to_string_lossy();
    if !pattern_text.contains('*') && !pattern_text.contains('?') {
        return Ok(if pattern.exists() {
            vec![pattern.to_path_buf()]
        } else {
            Vec::new()
        });
    }

    let directory = pattern.parent().unwrap_or_else(|| Path::new("."));
    let Some(file_pattern) = pattern.file_name().and_then(|value| value.to_str()) else {
        return Ok(Vec::new());
    };
    if !directory.exists() {
        return Ok(Vec::new());
    }

    let mut paths = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        if wildcard_matches(file_pattern, file_name) {
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
}

fn collect_ssh_config_aliases_from_file(
    file_path: &Path,
    home_dir: &Path,
    visited: &mut BTreeSet<PathBuf>,
) -> io::Result<BTreeSet<String>> {
    let resolved_path = file_path.to_path_buf();
    if visited.contains(&resolved_path) || !resolved_path.exists() {
        return Ok(BTreeSet::new());
    }
    visited.insert(resolved_path.clone());

    let mut aliases = BTreeSet::new();
    let raw = fs::read_to_string(&resolved_path)?;
    for line in raw.lines() {
        let Ok(parsed_args) = split_directive_args(line) else {
            continue;
        };
        let mut args = parsed_args.into_iter();
        let directive = args.next().unwrap_or_default().to_ascii_lowercase();
        if directive == "include" {
            for include_pattern in args {
                let resolved_pattern =
                    resolve_ssh_config_include_pattern(&include_pattern, home_dir);
                for included_path in expand_glob(&resolved_pattern)? {
                    aliases.extend(collect_ssh_config_aliases_from_file(
                        &included_path,
                        home_dir,
                        visited,
                    )?);
                }
            }
            continue;
        }

        if directive != "host" {
            continue;
        }

        for alias in args {
            if alias.is_empty() || has_ssh_pattern(&alias) {
                continue;
            }
            aliases.insert(alias);
        }
    }

    Ok(aliases)
}

fn normalize_known_hosts_hostname(raw_host: &str) -> String {
    if let Some(rest) = raw_host.strip_prefix('[')
        && let Some((host, _port)) = rest.split_once("]:")
    {
        return host.to_string();
    }

    if !raw_host.contains(':') {
        return raw_host.to_string();
    }

    let first_colon_index = raw_host.find(':');
    let last_colon_index = raw_host.rfind(':');
    if first_colon_index == last_colon_index {
        raw_host
            .split_once(':')
            .map_or_else(|| raw_host.to_string(), |(host, _port)| host.to_string())
    } else {
        raw_host.to_string()
    }
}

pub fn parse_known_hosts_hostnames(raw: &str) -> BTreeSet<String> {
    let mut hostnames = BTreeSet::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let without_marker = if trimmed.starts_with('@') {
            trimmed
                .split_whitespace()
                .skip(1)
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            trimmed.to_string()
        };
        let host_field = without_marker.split_whitespace().next().unwrap_or_default();
        if host_field.is_empty() || host_field.starts_with('|') {
            continue;
        }

        for raw_host in host_field.split(',') {
            let host = normalize_known_hosts_hostname(raw_host).trim().to_string();
            if host.is_empty() || has_ssh_pattern(&host) {
                continue;
            }
            hostnames.insert(host);
        }
    }

    hostnames
}

fn read_known_hosts_hostnames(file_path: &Path) -> io::Result<BTreeSet<String>> {
    match fs::read_to_string(file_path) {
        Ok(raw) => Ok(parse_known_hosts_hostnames(&raw)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(BTreeSet::new()),
        Err(error) => Err(error),
    }
}

pub fn default_home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .or_else(|| env::var_os("USERPROFILE").filter(|value| !value.is_empty()))
        .map(PathBuf::from)
}

pub fn discover_ssh_hosts(home_dir: Option<PathBuf>) -> Result<Vec<DiscoveredSshHost>, String> {
    let Some(home_dir) = home_dir else {
        return Ok(Vec::new());
    };
    if home_dir.as_os_str().is_empty() {
        return Ok(Vec::new());
    }

    let ssh_directory = home_dir.join(SSH_DIRECTORY_NAME);
    let config_aliases = collect_ssh_config_aliases_from_file(
        &ssh_directory.join(SSH_CONFIG_FILE_NAME),
        &home_dir,
        &mut BTreeSet::new(),
    )
    .map_err(|error| format!("Failed to read SSH config hosts: {error}"))?;
    let known_hosts = read_known_hosts_hostnames(&ssh_directory.join(KNOWN_HOSTS_FILE_NAME))
        .map_err(|error| format!("Failed to read known SSH hosts: {error}"))?;
    let mut discovered = BTreeMap::new();

    for alias in config_aliases {
        discovered.insert(
            alias.clone(),
            DiscoveredSshHost {
                alias: alias.clone(),
                hostname: alias,
                username: None,
                port: None,
                source: "ssh-config",
            },
        );
    }

    for hostname in known_hosts {
        discovered
            .entry(hostname.clone())
            .or_insert_with(|| DiscoveredSshHost {
                alias: hostname.clone(),
                hostname,
                username: None,
                port: None,
                source: "known-hosts",
            });
    }

    Ok(discovered.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn managed_ssh_child_ignores_appimage_environment() {
        crate::test_support::with_appimage_test_environment_async(
            "ssh::tests::managed_ssh_child_ignores_appimage_environment",
            async {
                let temporary_base = tempfile::tempdir().expect("askpass temporary base");
                let manager = SshEnvironmentManager::with_askpass_temp_base(
                    temporary_base.path().to_path_buf(),
                );
                let launcher = manager.askpass_launcher().expect("askpass launcher");
                let askpass_path = launcher.path().to_string_lossy().into_owned();
                let auth = SshAuthOptions::with_secret("fixture-password".to_string());
                let mut command = Command::new("/usr/bin/env");
                command
                    .args(["-0"])
                    .envs(build_ssh_child_environment(&auth, launcher.path()))
                    .env("PYTHONHOME", "/opt/host/python")
                    .env_remove("BIBCODE_FUTURE_PATH")
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .kill_on_drop(true);

                let mut child =
                    spawn_managed_ssh_child(command, launcher, "probe SSH child environment")
                        .expect("SSH child should start");
                let output = child
                    .wait_with_output(None)
                    .await
                    .expect("SSH child output");
                assert!(output.status.success());
                let environment = output
                    .stdout
                    .split(|byte| *byte == 0)
                    .filter_map(|entry| {
                        let separator = entry.iter().position(|byte| *byte == b'=')?;
                        Some((&entry[..separator], &entry[separator + 1..]))
                    })
                    .collect::<BTreeMap<_, _>>();
                for name in [
                    "APPDIR",
                    "APPIMAGE",
                    "ARGV0",
                    "OWD",
                    "GTK_THEME",
                    "GDK_BACKEND",
                    "PYTHONDONTWRITEBYTECODE",
                    "BIBCODE_FUTURE_PATH",
                ] {
                    assert!(
                        !environment.contains_key(name.as_bytes()),
                        "{name} leaked into SSH"
                    );
                }
                for (name, expected) in [
                    ("LD_LIBRARY_PATH", "/usr/lib"),
                    ("PYTHONHOME", "/opt/host/python"),
                    ("SSH_AUTH_SOCK", "/run/user/1000/bibcode-test-agent.sock"),
                    ("SSH_ASKPASS", askpass_path.as_str()),
                    ("SSH_ASKPASS_REQUIRE", "force"),
                    ("BIBCODE_SSH_AUTH_SECRET", "fixture-password"),
                ] {
                    assert_eq!(
                        environment.get(name.as_bytes()).copied(),
                        Some(expected.as_bytes()),
                        "{name}"
                    );
                }
                manager.shutdown().await;
            },
        )
        .await;
    }

    fn unique_temp_home() -> PathBuf {
        std::env::temp_dir().join(format!(
            "bibcode-tauri-ssh-test-{}-{}",
            std::process::id(),
            Uuid::new_v4().simple()
        ))
    }

    fn process_is_alive(pid: u32) -> bool {
        #[cfg(unix)]
        {
            // SAFETY: signal zero does not modify the target process.
            unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
        }
        #[cfg(windows)]
        {
            std::process::Command::new("powershell.exe")
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    &format!("if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ exit 0 }} else {{ exit 1 }}"),
                ])
                .status()
                .is_ok_and(|status| status.success())
        }
    }

    #[test]
    fn askpass_launcher_last_lease_removes_exact_root_without_persisting_secret() {
        let temporary_base = tempfile::tempdir().expect("askpass temporary base");
        let manager =
            SshEnvironmentManager::with_askpass_temp_base(temporary_base.path().to_path_buf());
        let launcher = manager
            .askpass_launcher()
            .expect("askpass launcher should create");
        let root = launcher.root().to_path_buf();
        let retained = launcher.clone();

        assert!(launcher.path().is_file());
        assert!(root.starts_with(temporary_base.path()));
        assert!(
            !fs::read_to_string(launcher.path())
                .expect("askpass launcher should read")
                .contains("fixture-password"),
            "askpass files must not persist authentication secrets"
        );

        drop(launcher);
        assert!(root.exists(), "a retained lease must keep its helper root");
        drop(retained);
        assert!(!root.exists(), "the final lease must remove its exact root");
        assert_eq!(
            fs::read_dir(temporary_base.path())
                .expect("temporary base should remain readable")
                .count(),
            0
        );
    }

    #[test]
    fn askpass_launcher_cleanup_preserves_unexpected_foreign_entries() {
        let temporary_base = tempfile::tempdir().expect("askpass temporary base");
        let manager =
            SshEnvironmentManager::with_askpass_temp_base(temporary_base.path().to_path_buf());
        let launcher = manager
            .askpass_launcher()
            .expect("askpass launcher should create");
        let root = launcher.root().to_path_buf();
        let directory = launcher
            .path()
            .parent()
            .expect("askpass directory")
            .to_path_buf();
        let foreign = directory.join("foreign-owner.txt");
        fs::write(&foreign, "foreign-owner-data").expect("foreign fixture should write");

        drop(launcher);

        assert_eq!(
            fs::read_to_string(&foreign).expect("foreign entry must remain"),
            "foreign-owner-data"
        );
        assert!(root.exists(), "a nonempty foreign-owned root must remain");
        assert!(
            !directory.join("ssh-askpass.sh").exists()
                && !directory.join("ssh-askpass.cmd").exists()
                && !directory.join("ssh-askpass.ps1").exists(),
            "only the exact created helper files should be removed"
        );
    }

    #[test]
    fn concurrent_askpass_requests_share_one_live_unique_lease() {
        let temporary_base = tempfile::tempdir().expect("askpass temporary base");
        let manager = Arc::new(SshEnvironmentManager::with_askpass_temp_base(
            temporary_base.path().to_path_buf(),
        ));
        let barrier = Arc::new(std::sync::Barrier::new(9));
        let owners = (0..8)
            .map(|_| {
                let manager = manager.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    manager
                        .askpass_launcher()
                        .expect("parallel askpass launcher")
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let owners = owners
            .into_iter()
            .map(|owner| owner.join().expect("parallel askpass owner"))
            .collect::<Vec<_>>();
        let expected_root = owners[0].root();

        assert!(owners.iter().all(|owner| owner.root() == expected_root));
        assert_eq!(
            fs::read_dir(temporary_base.path())
                .expect("temporary base should remain readable")
                .count(),
            1,
            "concurrent requests must converge on one live helper root"
        );
        drop(owners);
        assert_eq!(
            fs::read_dir(temporary_base.path())
                .expect("temporary base should remain readable")
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn cancelled_askpass_owner_removes_root_after_join() {
        let temporary_base = tempfile::tempdir().expect("askpass temporary base");
        let manager =
            SshEnvironmentManager::with_askpass_temp_base(temporary_base.path().to_path_buf());
        let launcher = manager
            .askpass_launcher()
            .expect("askpass launcher should create");
        let root = launcher.root().to_path_buf();
        let (entered_sender, entered_receiver) = oneshot::channel();
        let owner = tokio::spawn(async move {
            let _launcher = launcher;
            let _ = entered_sender.send(());
            std::future::pending::<()>().await;
        });
        entered_receiver
            .await
            .expect("cancelled owner should publish readiness");

        owner.abort();
        assert!(
            owner
                .await
                .expect_err("owner should be cancelled")
                .is_cancelled()
        );
        assert!(
            !root.exists(),
            "joining cancellation must release the helper root"
        );
        assert_eq!(
            fs::read_dir(temporary_base.path())
                .expect("temporary base should remain readable")
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn cancelled_active_ssh_child_reaps_before_releasing_askpass_root() {
        let temporary_base = tempfile::tempdir().expect("askpass temporary base");
        let manager =
            SshEnvironmentManager::with_askpass_temp_base(temporary_base.path().to_path_buf());
        let launcher = manager
            .askpass_launcher()
            .expect("askpass launcher should create");
        let root = launcher.root().to_path_buf();
        let mut cleaned = launcher.cleanup_observer();
        let mut command = if cfg!(windows) {
            let mut command = Command::new("powershell.exe");
            command.args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "[Console]::Out.Write('ready!'); Start-Sleep -Seconds 60",
            ]);
            command
        } else {
            let mut command = Command::new("sh");
            command.args(["-c", "printf 'ready!'; exec sleep 60"]);
            command
        };
        command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let reaper_permit = launcher
            .reserve_child()
            .expect("active SSH child should reserve cleanup ownership");
        let child = command.spawn().expect("active SSH child fixture");
        let pid = child.id().expect("active SSH child PID");
        let mut child = ManagedSshChild::new(child, launcher, reaper_permit);
        let mut stdout = child
            .child_mut()
            .stdout
            .take()
            .expect("active SSH child stdout");
        let mut readiness = [0_u8; 6];
        stdout
            .read_exact(&mut readiness)
            .await
            .expect("active SSH child readiness");
        assert_eq!(&readiness, b"ready!");

        let owner = tokio::spawn(async move {
            let _child = child;
            std::future::pending::<()>().await;
        });
        owner.abort();
        assert!(
            owner
                .await
                .expect_err("active owner should be cancelled")
                .is_cancelled()
        );
        tokio::time::timeout(Duration::from_secs(3), async {
            tokio::join!(manager.shutdown(), manager.shutdown());
        })
        .await
        .expect("concurrent manager shutdown should drain active SSH cleanup");
        cleaned
            .changed()
            .await
            .expect("cleanup observer should remain live");

        assert!(
            *cleaned.borrow(),
            "cleanup must publish only after child reap"
        );
        assert!(
            !root.exists(),
            "reap completion must release the helper root"
        );
        assert!(
            !process_is_alive(pid),
            "cleanup event must follow exact child exit"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn manager_shutdown_terminates_child_owned_by_retained_waiter() {
        let temporary_base = tempfile::tempdir().expect("askpass temporary base");
        let manager =
            SshEnvironmentManager::with_askpass_temp_base(temporary_base.path().to_path_buf());
        let launcher = manager
            .askpass_launcher()
            .expect("askpass launcher should create");
        let root = launcher.root().to_path_buf();
        let mut command = if cfg!(windows) {
            let mut command = Command::new("powershell.exe");
            command.args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "[Console]::Out.Write('ready!'); Start-Sleep -Seconds 60",
            ]);
            command
        } else {
            let mut command = Command::new("sh");
            command.args(["-c", "printf 'ready!'; exec sleep 60"]);
            command
        };
        command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let reaper_permit = launcher
            .reserve_child()
            .expect("active SSH child should reserve cleanup ownership");
        let child = command.spawn().expect("active SSH child fixture");
        let pid = child.id().expect("active SSH child PID");
        let mut child = ManagedSshChild::new(child, launcher, reaper_permit);
        let mut stdout = child
            .child_mut()
            .stdout
            .take()
            .expect("active SSH child stdout");
        let mut readiness = [0_u8; 6];
        stdout
            .read_exact(&mut readiness)
            .await
            .expect("active SSH child readiness");
        assert_eq!(&readiness, b"ready!");
        let owner = tokio::spawn(async move { child.wait_with_output(None).await });

        if tokio::time::timeout(Duration::from_secs(3), async {
            tokio::join!(manager.shutdown(), manager.shutdown());
        })
        .await
        .is_err()
        {
            owner.abort();
            let _ = owner.await;
            let _ = tokio::time::timeout(Duration::from_secs(3), manager.shutdown()).await;
            panic!("manager shutdown must interrupt a retained active SSH waiter");
        }
        let wait_error = owner
            .await
            .expect("retained SSH waiter should join")
            .expect_err("manager shutdown should interrupt the SSH wait");

        assert!(
            matches!(&wait_error, WaitError::Io(error) if error.kind() == io::ErrorKind::Interrupted),
            "{wait_error:?}"
        );
        assert!(!root.exists(), "shutdown must release the askpass root");
        assert!(!process_is_alive(pid), "shutdown must reap the exact child");
        tokio::time::timeout(Duration::from_secs(1), manager.shutdown())
            .await
            .expect("completed manager shutdown must remain idempotent");
    }

    #[tokio::test]
    async fn manager_shutdown_interrupts_unpublished_tunnel_readiness() {
        let temporary_base = tempfile::tempdir().expect("askpass temporary base");
        let manager = Arc::new(SshEnvironmentManager::with_askpass_temp_base(
            temporary_base.path().to_path_buf(),
        ));
        let launcher = manager
            .askpass_launcher()
            .expect("askpass launcher should create");
        let root = launcher.root().to_path_buf();
        let (program, args) = if cfg!(windows) {
            (
                "powershell.exe".to_string(),
                vec![
                    "-NoProfile".to_string(),
                    "-NonInteractive".to_string(),
                    "-Command".to_string(),
                    "Start-Sleep -Seconds 60".to_string(),
                ],
            )
        } else {
            (
                "sh".to_string(),
                vec!["-c".to_string(), "exec sleep 60".to_string()],
            )
        };
        let target = SshEnvironmentTarget {
            alias: "fixture".to_string(),
            hostname: "fixture.invalid".to_string(),
            username: Some("fixture-user".to_string()),
            port: None,
        };
        let plan = SshEnvironmentLaunchPlan {
            key: "shutdown-readiness".to_string(),
            program,
            args,
            target,
            local_port: 9,
            remote_port: 9,
            remote_server_kind: "fixture",
            http_base_url: "http://127.0.0.1:9/".to_string(),
            ws_base_url: "ws://127.0.0.1:9/".to_string(),
        };
        let owner = tokio::spawn(async move {
            start_ssh_tunnel(
                &plan,
                &SshAuthOptions::batch(),
                launcher,
                SshForwardKind::Tunnel,
            )
            .await
        });
        tokio::time::timeout(
            Duration::from_secs(3),
            manager.child_reaper.wait_until_active(),
        )
        .await
        .expect("unpublished tunnel should reserve ownership before readiness");

        tokio::time::timeout(Duration::from_secs(3), manager.shutdown())
            .await
            .expect("shutdown should interrupt unpublished tunnel readiness");
        let error = match owner.await.expect("unpublished tunnel owner should join") {
            Ok(mut child) => {
                child.terminate_and_reap().await;
                panic!("shutdown must reject unpublished tunnel readiness");
            }
            Err(error) => error,
        };

        assert_eq!(error, "SSH process owner is shutting down.");
        assert_eq!(manager.child_reaper.active(), 0);
        assert!(!root.exists(), "shutdown must release the askpass root");
    }

    #[tokio::test]
    async fn ssh_child_reaper_capacity_refuses_spawn_admission_and_recovers() {
        let temporary_base = tempfile::tempdir().expect("askpass temporary base");
        let manager =
            SshEnvironmentManager::with_askpass_temp_base(temporary_base.path().to_path_buf());
        let launcher = manager
            .askpass_launcher()
            .expect("askpass launcher should create");
        let permits = (0..SSH_CHILD_REAPER_CAPACITY)
            .map(|_| launcher.reserve_child().expect("bounded child ownership"))
            .collect::<Vec<_>>();
        let marker = temporary_base.path().join("spawned.txt");
        let mut command = if cfg!(windows) {
            let mut command = Command::new("powershell.exe");
            command.args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "[IO.File]::WriteAllText($env:TASK9I_MARKER, 'spawned')",
            ]);
            command
        } else {
            let mut command = Command::new("sh");
            command.args(["-c", "printf spawned > \"$TASK9I_MARKER\""]);
            command
        };
        command.env("TASK9I_MARKER", &marker).kill_on_drop(true);

        assert_eq!(manager.child_reaper.active(), SSH_CHILD_REAPER_CAPACITY);
        assert_eq!(
            launcher
                .reserve_child()
                .err()
                .expect("capacity must reject before process spawn"),
            "SSH process owner capacity was exceeded."
        );
        assert_eq!(
            spawn_managed_ssh_child(command, launcher.clone(), "start capacity fixture")
                .err()
                .expect("capacity must fail before spawning"),
            "SSH process owner capacity was exceeded."
        );
        assert!(!marker.exists(), "a refused child must never be spawned");

        drop(permits);
        assert_eq!(manager.child_reaper.active(), 0);
        assert!(launcher.reserve_child().is_ok());
        manager.shutdown().await;
        assert_eq!(
            launcher
                .reserve_child()
                .err()
                .expect("shutdown must close child admission"),
            "SSH process owner is shutting down."
        );
    }

    #[test]
    fn environment_manager_caches_clears_and_misses_auth_and_tunnels() {
        let manager = SshEnvironmentManager::default();
        assert_eq!(manager.cached_auth_secret("target"), None);
        manager
            .remember_auth_secret("target", "secret".to_string())
            .expect("authentication secret should cache");
        assert_eq!(
            manager.cached_auth_secret("target").as_deref(),
            Some("secret")
        );
        manager.clear_auth_secret("target");
        assert_eq!(manager.cached_auth_secret("target"), None);
        assert_eq!(
            manager
                .take_existing_bootstrap_if_running("missing-target")
                .expect("missing tunnel should be inspectable"),
            None,
        );
    }

    #[tokio::test]
    async fn environment_manager_reports_unreachable_ssh_targets() {
        use tauri::test::{mock_builder, mock_context, noop_assets};

        let askpass_temporary_base = tempfile::tempdir().expect("askpass temporary base");
        let ssh_config = tempfile::NamedTempFile::new().expect("empty SSH config");
        let app = mock_builder()
            .build(mock_context(noop_assets()))
            .expect("mock Tauri app");
        let manager = SshEnvironmentManager::with_askpass_temp_base(
            askpass_temporary_base.path().to_path_buf(),
        )
        .with_ssh_config_file_for_test(ssh_config.path().to_path_buf());
        let prompts = SshPasswordPromptManager::with_timeout(Duration::ZERO);
        let target = SshEnvironmentTarget {
            alias: String::new(),
            hostname: "127.0.0.1".to_string(),
            username: None,
            port: Some(1),
        };

        let ensure_error = manager
            .ensure_environment(app.handle(), &prompts, target.clone(), None)
            .await
            .expect_err("an unreachable SSH target should not launch");
        assert!(ensure_error.contains("SSH launch command failed"));
        assert_eq!(
            fs::read_dir(askpass_temporary_base.path())
                .expect("askpass temporary base should remain readable")
                .count(),
            0,
            "failed ensure must release its askpass root"
        );

        let disconnect_error = manager
            .disconnect_environment(app.handle(), &prompts, target)
            .await
            .expect_err("an unreachable SSH target should not disconnect remotely");
        assert!(disconnect_error.contains("SSH stop command failed"));
        assert_eq!(
            fs::read_dir(askpass_temporary_base.path())
                .expect("askpass temporary base should remain readable")
                .count(),
            0,
            "failed disconnect must release its askpass root"
        );
        manager.shutdown().await;
    }

    #[test]
    fn discovers_ssh_config_hosts_across_included_files() {
        let home_dir = unique_temp_home();
        let ssh_dir = home_dir.join(".ssh");
        fs::create_dir_all(ssh_dir.join("config.d")).expect("ssh config dir should create");
        fs::write(
            ssh_dir.join("config"),
            [
                "Host devbox",
                "  HostName devbox.example.com",
                "Host=equalsbox",
                "Include=config.d/*.conf",
                "",
            ]
            .join("\n"),
        )
        .expect("ssh config should write");
        fs::write(
            ssh_dir.join("config.d").join("team.conf"),
            [
                "Host staging",
                "  HostName staging.example.com",
                "Host *",
                "  ServerAliveInterval 30",
                "",
            ]
            .join("\n"),
        )
        .expect("included ssh config should write");
        fs::write(
            ssh_dir.join("known_hosts"),
            [
                "known.example.com ssh-ed25519 AAAA",
                "|1|hashed|entry ssh-ed25519 AAAA",
                "[bastion.example.com]:2222 ssh-ed25519 AAAA",
                "",
            ]
            .join("\n"),
        )
        .expect("known hosts should write");

        let hosts = discover_ssh_hosts(Some(home_dir.clone())).expect("hosts should discover");

        assert_eq!(
            hosts,
            vec![
                DiscoveredSshHost {
                    alias: "bastion.example.com".to_string(),
                    hostname: "bastion.example.com".to_string(),
                    username: None,
                    port: None,
                    source: "known-hosts",
                },
                DiscoveredSshHost {
                    alias: "devbox".to_string(),
                    hostname: "devbox".to_string(),
                    username: None,
                    port: None,
                    source: "ssh-config",
                },
                DiscoveredSshHost {
                    alias: "equalsbox".to_string(),
                    hostname: "equalsbox".to_string(),
                    username: None,
                    port: None,
                    source: "ssh-config",
                },
                DiscoveredSshHost {
                    alias: "known.example.com".to_string(),
                    hostname: "known.example.com".to_string(),
                    username: None,
                    port: None,
                    source: "known-hosts",
                },
                DiscoveredSshHost {
                    alias: "staging".to_string(),
                    hostname: "staging".to_string(),
                    username: None,
                    port: None,
                    source: "ssh-config",
                },
            ]
        );

        let _ = fs::remove_dir_all(home_dir);
    }

    #[test]
    fn discovers_ssh_config_hosts_from_quoted_include_paths() {
        let home_dir = unique_temp_home();
        let ssh_dir = home_dir.join(".ssh");
        let include_dir = ssh_dir.join("config dir");
        fs::create_dir_all(&include_dir).expect("quoted include dir should create");
        fs::write(ssh_dir.join("config"), "Include \"config dir/team.conf\"\n")
            .expect("ssh config should write");
        fs::write(include_dir.join("team.conf"), "Host quoted-include\n")
            .expect("included ssh config should write");

        let hosts = discover_ssh_hosts(Some(home_dir.clone())).expect("hosts should discover");

        assert_eq!(
            hosts,
            vec![DiscoveredSshHost {
                alias: "quoted-include".to_string(),
                hostname: "quoted-include".to_string(),
                username: None,
                port: None,
                source: "ssh-config",
            }]
        );

        let _ = fs::remove_dir_all(home_dir);
    }

    #[test]
    fn preserves_hashes_inside_quoted_ssh_include_paths() {
        let home_dir = unique_temp_home();
        let ssh_dir = home_dir.join(".ssh");
        let include_dir = ssh_dir.join("config #archive");
        fs::create_dir_all(&include_dir).expect("quoted include dir should create");
        fs::write(
            ssh_dir.join("config"),
            "Include \"config #archive/team.conf\" # trailing comment\n",
        )
        .expect("ssh config should write");
        fs::write(include_dir.join("team.conf"), "Host hash-include\n")
            .expect("included ssh config should write");

        let hosts = discover_ssh_hosts(Some(home_dir.clone())).expect("hosts should discover");

        assert_eq!(
            hosts,
            vec![DiscoveredSshHost {
                alias: "hash-include".to_string(),
                hostname: "hash-include".to_string(),
                username: None,
                port: None,
                source: "ssh-config",
            }]
        );

        let _ = fs::remove_dir_all(home_dir);
    }

    #[test]
    fn preserves_windows_backslashes_in_quoted_include_paths() {
        assert_eq!(
            split_directive_args(
                r#"Include "C:\Users\mauro\.ssh\config dir\team.conf" # trailing comment"#,
            ),
            Ok(vec![
                "Include".to_string(),
                r"C:\Users\mauro\.ssh\config dir\team.conf".to_string(),
            ])
        );
    }

    #[test]
    fn unquoted_backslash_escaped_whitespace_stays_in_one_include_token() {
        assert_eq!(
            split_directive_args(r"Include config\ dir/*.conf"),
            Ok(vec!["Include".to_string(), "config dir/*.conf".to_string(),])
        );
    }

    #[test]
    fn escaped_hash_stays_inside_an_unquoted_include_path() {
        assert_eq!(
            split_directive_args(r"Include config\#archive\team.conf"),
            Ok(vec![
                "Include".to_string(),
                r"config#archive\team.conf".to_string(),
            ])
        );
    }

    #[test]
    fn hash_starts_comments_only_at_token_boundaries() {
        assert_eq!(split_directive_args("# full-line comment"), Ok(Vec::new()));
        assert_eq!(
            split_directive_args("Include # token-leading comment"),
            Ok(vec!["Include".to_string()])
        );
        assert_eq!(
            split_directive_args("Include config#archive.conf # trailing comment"),
            Ok(vec![
                "Include".to_string(),
                "config#archive.conf".to_string(),
            ])
        );
        assert_eq!(
            split_directive_args(r"Include \#literal.conf # trailing comment"),
            Ok(vec!["Include".to_string(), "#literal.conf".to_string()])
        );
    }

    #[cfg(windows)]
    #[test]
    fn discovers_unquoted_escaped_windows_include_globs_before_trailing_comments() {
        let home_dir = unique_temp_home();
        let ssh_dir = home_dir.join(".ssh");
        let include_dir = ssh_dir.join("config dir");
        fs::create_dir_all(&include_dir).expect("include directory should create");
        fs::write(
            ssh_dir.join("config"),
            r"  Include config\ dir\config\#*.conf # trailing comment",
        )
        .expect("ssh config should write");
        fs::write(
            include_dir.join("config#team.conf"),
            "Host escaped-windows-glob\n",
        )
        .expect("included config should write");
        fs::write(include_dir.join("config-team.conf"), "Host ignored\n")
            .expect("non-matching config should write");

        let hosts = discover_ssh_hosts(Some(home_dir.clone())).expect("hosts should discover");

        assert_eq!(
            hosts,
            vec![DiscoveredSshHost {
                alias: "escaped-windows-glob".to_string(),
                hostname: "escaped-windows-glob".to_string(),
                username: None,
                port: None,
                source: "ssh-config",
            }]
        );
        let _ = fs::remove_dir_all(home_dir);
    }

    #[cfg(windows)]
    #[test]
    fn discovers_windows_style_include_globs_with_whitespace_and_comments() {
        let home_dir = unique_temp_home();
        let ssh_dir = home_dir.join(".ssh");
        let include_dir = ssh_dir.join("config dir");
        fs::create_dir_all(&include_dir).expect("include directory should create");
        fs::write(
            ssh_dir.join("config"),
            "  Include   \"config dir\\*.conf\"   # trailing comment\n",
        )
        .expect("ssh config should write");
        fs::write(include_dir.join("alpha.conf"), "Host windows-alpha\n")
            .expect("alpha config should write");
        fs::write(include_dir.join("beta.txt"), "Host ignored\n")
            .expect("non-matching config should write");

        let hosts = discover_ssh_hosts(Some(home_dir.clone())).expect("hosts should discover");

        assert_eq!(
            hosts,
            vec![DiscoveredSshHost {
                alias: "windows-alpha".to_string(),
                hostname: "windows-alpha".to_string(),
                username: None,
                port: None,
                source: "ssh-config",
            }]
        );
        let _ = fs::remove_dir_all(home_dir);
    }

    #[test]
    fn preserves_equals_inside_include_filenames() {
        assert_eq!(
            split_directive_args("  Include   config=name.conf   # comment"),
            Ok(vec!["Include".to_string(), "config=name.conf".to_string(),])
        );
        assert_eq!(
            split_directive_args("Include=config=name.conf # comment"),
            Ok(vec!["Include".to_string(), "config=name.conf".to_string(),])
        );
    }

    #[test]
    fn discovers_include_globs_with_equals_in_filenames() {
        let home_dir = unique_temp_home();
        let ssh_dir = home_dir.join(".ssh");
        fs::create_dir_all(&ssh_dir).expect("ssh directory should create");
        fs::write(
            ssh_dir.join("config"),
            "  Include   config=*.conf   # trailing comment\n",
        )
        .expect("ssh config should write");
        fs::write(ssh_dir.join("config=team.conf"), "Host equals-glob\n")
            .expect("included config should write");

        let hosts = discover_ssh_hosts(Some(home_dir.clone())).expect("hosts should discover");

        assert_eq!(
            hosts,
            vec![DiscoveredSshHost {
                alias: "equals-glob".to_string(),
                hostname: "equals-glob".to_string(),
                username: None,
                port: None,
                source: "ssh-config",
            }]
        );
        let _ = fs::remove_dir_all(home_dir);
    }

    #[test]
    fn rejects_unterminated_ssh_config_quotes() {
        assert_eq!(
            split_directive_args(r#"Include "config dir/*.conf"#),
            Err(SshConfigLineParseError::InvalidQuotes)
        );
        assert_eq!(
            split_directive_args("Host 'unterminated"),
            Err(SshConfigLineParseError::InvalidQuotes)
        );
    }

    #[test]
    fn ignores_entire_include_line_when_any_quote_is_unterminated() {
        let home_dir = unique_temp_home();
        let ssh_dir = home_dir.join(".ssh");
        let include_dir = ssh_dir.join("config.d");
        fs::create_dir_all(&include_dir).expect("include directory should create");
        fs::write(
            ssh_dir.join("config"),
            [
                "# keep comments independent from malformed directives",
                "  Include config.d/*.conf \"unterminated#still-quoted",
                "Host direct-host",
                "",
            ]
            .join("\n"),
        )
        .expect("ssh config should write");
        fs::write(include_dir.join("leaked.conf"), "Host must-not-leak\n")
            .expect("included config should write");

        let hosts = discover_ssh_hosts(Some(home_dir.clone())).expect("hosts should discover");

        assert_eq!(
            hosts,
            vec![DiscoveredSshHost {
                alias: "direct-host".to_string(),
                hostname: "direct-host".to_string(),
                username: None,
                port: None,
                source: "ssh-config",
            }]
        );
        let _ = fs::remove_dir_all(home_dir);
    }

    #[test]
    fn parses_known_hosts_entries_without_hashed_hosts() {
        assert_eq!(
            parse_known_hosts_hostnames(
                [
                    "github.com ssh-ed25519 AAAA",
                    "gitlab.com,gitlab-alias ssh-ed25519 BBBB",
                    "|1|hashed|entry ssh-ed25519 CCCC",
                    "@cert-authority *.example.com ssh-ed25519 DDDD",
                    "[ssh.example.com]:2200 ssh-ed25519 EEEE",
                    "port.example.com:22 ssh-ed25519 HHHH",
                    "::1 ssh-ed25519 FFFF",
                    "2001:db8::1 ssh-ed25519 GGGG",
                    "",
                ]
                .join("\n")
                .as_str(),
            ),
            BTreeSet::from([
                "::1".to_string(),
                "2001:db8::1".to_string(),
                "github.com".to_string(),
                "gitlab-alias".to_string(),
                "gitlab.com".to_string(),
                "port.example.com".to_string(),
                "ssh.example.com".to_string(),
            ])
        );
    }

    #[tokio::test]
    async fn password_prompt_request_emits_payload_and_resolves_with_password() {
        let manager = SshPasswordPromptManager::with_timeout(std::time::Duration::from_secs(30));
        let resolver = manager.clone();
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let emitted_for_request = emitted.clone();
        let request = SshPasswordRequest {
            destination: "example.com".to_string(),
            username: Some("alice".to_string()),
            prompt: "alice@example.com's password:".to_string(),
        };

        let task = tokio::spawn(async move {
            manager
                .request_password_with(
                    "req-1".to_string(),
                    request,
                    std::time::SystemTime::UNIX_EPOCH,
                    move |payload| {
                        emitted_for_request
                            .lock()
                            .expect("emitted mutex")
                            .push(payload);
                        Ok(())
                    },
                )
                .await
        });
        tokio::task::yield_now().await;

        assert_eq!(emitted.lock().expect("emitted mutex").len(), 1);
        assert_eq!(
            emitted.lock().expect("emitted mutex")[0],
            SshPasswordPromptPayload {
                request_id: "req-1".to_string(),
                destination: "example.com".to_string(),
                username: Some("alice".to_string()),
                prompt: "alice@example.com's password:".to_string(),
                expires_at: "1970-01-01T00:00:30Z".to_string(),
            }
        );

        assert_eq!(
            resolver.resolve(SshPasswordPromptResolution {
                request_id: "req-1".to_string(),
                password: Some("hunter2".to_string()),
            }),
            Ok(())
        );
        assert_eq!(task.await.expect("prompt task"), Ok("hunter2".to_string()));
    }

    #[tokio::test]
    async fn password_prompt_resolution_rejects_blank_or_expired_ids() {
        let manager = SshPasswordPromptManager::default();

        assert_eq!(
            manager.resolve(SshPasswordPromptResolution {
                request_id: "   ".to_string(),
                password: Some("ignored".to_string()),
            }),
            Err(SshPasswordPromptResolveError::InvalidRequestId)
        );
        assert_eq!(
            manager.resolve(SshPasswordPromptResolution {
                request_id: "missing".to_string(),
                password: None,
            }),
            Err(SshPasswordPromptResolveError::Expired {
                request_id: "missing".to_string(),
            })
        );
    }

    #[test]
    fn builds_external_ssh_tunnel_launch_plan_with_exact_arguments() {
        let target = SshEnvironmentTarget {
            alias: "devbox".to_string(),
            hostname: "devbox.internal".to_string(),
            username: Some("alice".to_string()),
            port: Some(2222),
        };

        let plan = SshEnvironmentLaunchPlan::external(target.clone(), 45123)
            .expect("launch plan should build");

        assert_eq!(plan.key, "devbox\u{0}devbox.internal\u{0}alice\u{0}2222");
        assert_eq!(plan.program, ssh_command());
        assert_eq!(
            plan.args,
            vec![
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                "-p",
                "2222",
                "-o",
                "ExitOnForwardFailure=yes",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=3",
                "-n",
                "-N",
                "-L",
                "45123:127.0.0.1:3773",
                "alice@devbox",
            ]
        );
        assert_eq!(plan.target, target);
        assert_eq!(plan.remote_port, 3773);
        assert_eq!(plan.http_base_url, "http://127.0.0.1:45123/");
        assert_eq!(plan.ws_base_url, "ws://127.0.0.1:45123/");
    }

    #[test]
    fn detects_ssh_password_auth_failures() {
        assert!(is_ssh_auth_failure(
            "Permission denied (publickey,password,keyboard-interactive)."
        ));
        assert!(is_ssh_auth_failure("Authentication failed."));
        assert!(is_ssh_auth_failure("Too many authentication failures"));
        assert!(!is_ssh_auth_failure("Connection timed out."));
    }

    #[test]
    fn builds_batch_mode_args_from_auth_options() {
        let target = SshEnvironmentTarget {
            alias: "devbox".to_string(),
            hostname: "devbox.internal".to_string(),
            username: Some("alice".to_string()),
            port: Some(2222),
        };

        assert_eq!(
            base_ssh_args_with_auth(&target, &SshAuthOptions::batch()),
            vec![
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                "-p",
                "2222",
            ]
        );
        assert_eq!(
            base_ssh_args_with_auth(&target, &SshAuthOptions::with_secret("hunter2".to_string()))
                [1],
            "BatchMode=no",
        );
    }

    #[test]
    fn builds_askpass_environment_for_cached_password() {
        let environment = build_ssh_child_environment(
            &SshAuthOptions::with_secret("hunter2".to_string()),
            Path::new("C:/tmp/bibcode-ssh/ssh-askpass.cmd"),
        );

        assert_eq!(
            environment
                .get("BIBCODE_SSH_AUTH_SECRET")
                .map(String::as_str),
            Some("hunter2")
        );
        assert_eq!(
            environment.get("SSH_ASKPASS_REQUIRE").map(String::as_str),
            Some("force")
        );
        assert_eq!(
            environment.get("SSH_ASKPASS").map(String::as_str),
            Some("C:/tmp/bibcode-ssh/ssh-askpass.cmd")
        );
    }

    #[test]
    fn remote_state_key_matches_typescript_manager() {
        let target = SshEnvironmentTarget {
            alias: "devbox".to_string(),
            hostname: "devbox.internal".to_string(),
            username: Some("alice".to_string()),
            port: Some(2222),
        };

        assert_eq!(remote_state_key(&target), "a39af6c8b8cc1930");
    }

    #[test]
    fn remote_launch_requires_the_native_bibcode_runtime() {
        for forbidden in [
            "node -",
            "command -v node",
            "command -v npm",
            "command -v npx",
            "bibcode@latest",
        ] {
            assert!(
                !REMOTE_LAUNCH_SCRIPT.contains(forbidden),
                "remote launch script must not contain {forbidden}"
            );
        }
        assert!(REMOTE_LAUNCH_SCRIPT.contains("command -v bibcode"));
        assert!(REMOTE_LAUNCH_SCRIPT.contains("native BiBCode CLI"));
    }

    #[test]
    fn only_a_cancelled_password_prompt_carries_the_cancellation_marker() {
        let cancelled = SshPasswordPromptRequestError::Cancelled {
            request_id: "id".to_string(),
            destination: "cancelbox".to_string(),
        };
        assert_eq!(
            password_prompt_failure(cancelled),
            "[ssh_cancelled] SSH authentication cancelled for cancelbox."
        );
        let timed_out = SshPasswordPromptRequestError::TimedOut {
            request_id: "id".to_string(),
            destination: "cancelbox".to_string(),
        };
        assert_eq!(
            password_prompt_failure(timed_out),
            "SSH authentication timed out for cancelbox."
        );
    }

    /// Test stand-ins for ssh classify each script by its first line.
    #[test]
    fn remote_scripts_start_with_a_header_naming_their_kind() {
        for (script, kind) in [
            (REMOTE_PAIRING_SCRIPT.to_string(), "pairing"),
            (build_remote_launch_script(), "launch"),
            (build_remote_stop_script(), "stop"),
        ] {
            assert_eq!(
                script.lines().next(),
                Some(format!("{}{kind}", remote_script_header_prefix!()).as_str())
            );
            assert_eq!(script.lines().nth(1), Some("set -eu"), "{kind}");
        }
        let launch = build_remote_launch_script();
        assert!(!launch.contains("@@"), "every placeholder is filled");
        assert!(launch.contains("\ntrap '' PIPE\n"));
        let stop = build_remote_stop_script();
        assert!(!stop.contains("@@"), "every placeholder is filled");
        assert!(stop.contains("\ntrap '' PIPE\n"));
    }

    #[test]
    fn launch_script_runs_serve_without_environment_assignments() {
        let script = build_remote_launch_script();
        let launch = script
            .lines()
            .skip_while(|line| !line.starts_with("nohup "))
            .take(2)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(launch.starts_with("nohup sh -c "), "{launch}");
        assert!(
            launch.contains(
                r#"bibcode-launch "$PID_FILE" "$RUNNER_FILE" serve --host 127.0.0.1 --port "$REMOTE_PORT" --base-dir "$SERVER_HOME" >>"#
            ),
            "serve must start without environment assignments: {launch}"
        );
        assert!(!script.contains("BIBCODE_NO_BROWSER"));
    }

    /// The remote scripts run directly, without SSH, under every POSIX shell
    /// a host may use for `sh -s`, with a stand-in `bibcode` first on `PATH`
    /// and a private `HOME`. These pin the scripts' own process handling.
    #[cfg(unix)]
    mod remote_scripts {
        use super::*;
        use std::io::Write as _;
        use std::process::{Command as StdCommand, Stdio};

        const STATE_KEY: &str = "0123456789abcdef";
        const CREDENTIAL_LINE: &str = r#"{"credential":"fixture-credential"}"#;

        /// A stand-in `bibcode`. `serve` records its pid and runs
        /// `serve.py`; `pairing` records its pid and behaves as
        /// `FIXTURE_PAIRING` says.
        const FAKE_BIBCODE: &str = r#"#!/bin/sh
dir='@@DIR@@'
case "$1" in
  serve)
    printf '%s\n' "$$" >>"$dir/serve.pids"
    if [ -n "${FIXTURE_OLD_PID:-}" ]; then
      case "$(ps -o stat= -p "$FIXTURE_OLD_PID" 2>/dev/null)" in
        ''|Z*) echo old-gone ;;
        *) echo old-alive ;;
      esac >>"$dir/serve-starts.log"
    fi
    if [ -n "${FIXTURE_REWRITE_PID_FILE:-}" ]; then
      (sleep 0.5; printf '4242\n' >"$FIXTURE_REWRITE_PID_FILE") &
    fi
    exec python3 "$dir/serve.py" "$@"
    ;;
  pairing)
    printf '%s\n' "$$" >>"$dir/pairing.pids"
    case "${FIXTURE_PAIRING:-ok}" in
      hang) exec sleep 600 ;;
      ignore-term)
        # Ignore TERM before interpreter startup; exec preserves SIG_IGN.
        trap '' TERM
        exec python3 -c 'import signal, time; signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(600)'
        ;;
      fail) printf '{"credential":"fixture-credential"}\n'; exit 7 ;;
      *) printf '{"credential":"fixture-credential"}\n'; exit 0 ;;
    esac
    ;;
esac
printf 'fixture: unexpected bibcode %s\n' "$*" >&2
exit 97
"#;

        /// A stand-in `bibcode serve`: it listens on `--port` and answers
        /// every request with `200 {}`, or with `FIXTURE_SERVE_MODE=silent`
        /// accepts connections and never answers.
        /// `FIXTURE_IGNORE_TERM=1` makes it ignore SIGTERM, and
        /// `FIXTURE_TERM_DELAY=<s>` makes it exit that long after SIGTERM,
        /// still listening meanwhile, like a graceful shutdown.
        const SERVE_PY: &str = r#"import os, signal, socket, sys, time
if os.environ.get("FIXTURE_IGNORE_TERM") == "1":
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
term_delay = float(os.environ.get("FIXTURE_TERM_DELAY", "0"))
if term_delay:
    def shut_down_slowly(signum, frame):
        time.sleep(term_delay)
        os._exit(0)
    signal.signal(signal.SIGTERM, shut_down_slowly)
args = sys.argv[1:]
port = int(args[args.index("--port") + 1])
silent = os.environ.get("FIXTURE_SERVE_MODE") == "silent"
listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
listener.bind(("127.0.0.1", port))
listener.listen(16)
held = []
while True:
    connection, _ = listener.accept()
    if silent:
        held.append(connection)
        continue
    try:
        connection.recv(4096)
        connection.sendall(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}")
    except OSError:
        pass
    connection.close()
"#;

        /// Prepended to a script under test: every `kill` it makes is
        /// recorded in `kill.log`, and only signal 0 is delivered, so a test
        /// can prove no signal would be sent without ever sending one.
        const KILL_SHIM: &str = r#"kill() {
  printf '%s\n' "$*" >>'@@DIR@@/kill.log'
  case "$1" in
    -0) command kill "$@" ;;
    *) return 0 ;;
  esac
}
"#;

        /// Recorded pids a corrupt or hand-edited pid file could hold. "-1"
        /// would reach every process the user owns.
        const MALFORMED_PIDS: [&str; 7] = ["", "0", "-1", "12a", " 12", "1 2", "007"];

        /// `/bin/sh` (dash on Debian and Ubuntu, where CI runs), plus
        /// `dash`, `bash --posix` and `busybox sh` when installed, each shell
        /// binary once: where `/bin/sh` is bash (Fedora) or dash (Ubuntu), the
        /// matching entry would only run every test twice in the same shell.
        fn shells() -> Vec<Vec<String>> {
            let mut shells = Vec::new();
            let mut binaries = Vec::new();
            let candidates = [
                (Some(PathBuf::from("/bin/sh")), &[][..]),
                (find_on_path("dash"), &[][..]),
                (find_on_path("bash"), &["--posix"][..]),
                (find_on_path("busybox"), &["sh"][..]),
            ];
            for (program, args) in candidates {
                // An absolute path, because some tests narrow the child's PATH.
                let Some(program) = program else {
                    continue;
                };
                let binary = fs::canonicalize(&program).unwrap_or_else(|_| program.clone());
                if binaries.contains(&binary) {
                    continue;
                }
                binaries.push(binary);
                let mut shell = vec![program.display().to_string()];
                shell.extend(args.iter().map(|part| part.to_string()));
                shells.push(shell);
            }
            shells
        }

        fn find_on_path(program: &str) -> Option<PathBuf> {
            env::split_paths(&env::var_os("PATH")?)
                .map(|directory| directory.join(program))
                .find(|candidate| candidate.is_file())
        }

        fn free_port() -> u16 {
            crate::test_support::free_test_port()
        }

        fn write_executable(path: &Path, contents: &str) {
            crate::test_support::write_executable_fixture(path, contents, 0o755);
        }

        async fn wait_until_gone(pid: u32, limit: Duration) -> bool {
            let deadline = std::time::Instant::now() + limit;
            loop {
                if fixture_process_is_gone(pid) {
                    return true;
                }
                if std::time::Instant::now() >= deadline {
                    return false;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }

        enum ScriptOutput {
            /// stdout and stderr go to files the test reads afterwards.
            Files,
            /// stdout and stderr are pipes whose read ends are already
            /// closed, as after the SSH channel went away.
            Closed,
        }

        struct ScriptRun {
            shell: String,
            status: Option<std::process::ExitStatus>,
            stdout: String,
            stderr: String,
            elapsed: Duration,
        }

        impl ScriptRun {
            fn code(&self) -> Option<i32> {
                self.status.and_then(|status| status.code())
            }
        }

        impl std::fmt::Debug for ScriptRun {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(
                    formatter,
                    "`{}` ended with {:?} after {:?}\n--- stdout\n{}\n--- stderr\n{}",
                    self.shell, self.status, self.elapsed, self.stdout, self.stderr
                )
            }
        }

        /// A private "remote host": a HOME, a `bin` holding the stand-in
        /// `bibcode`, and the files the stand-ins record.
        struct ScriptHost {
            dir: tempfile::TempDir,
            /// How long a script may run before the test kills it.
            limit: Duration,
            /// Where the launch script's port scan starts when no port is
            /// recorded: a free port, never the 3773 a local BiBCode uses.
            default_port: u16,
        }

        impl ScriptHost {
            fn new() -> Self {
                let dir = tempfile::tempdir().expect("script host directory");
                let root = dir.path().display().to_string();
                fs::create_dir_all(dir.path().join("home")).expect("fixture home");
                fs::create_dir_all(dir.path().join("bin")).expect("fixture bin");
                write_executable(
                    &dir.path().join("bin/bibcode"),
                    &FAKE_BIBCODE.replace("@@DIR@@", &root),
                );
                fs::write(dir.path().join("serve.py"), SERVE_PY).expect("fixture server");
                Self {
                    dir,
                    limit: Duration::from_secs(60),
                    default_port: free_port(),
                }
            }

            fn launch_script(&self, limits: RemoteLaunchLimits) -> String {
                build_remote_launch_script_with(limits, self.default_port)
            }

            /// `script` with `KILL_SHIM` in front of it.
            fn with_kill_shim(&self, script: &str) -> String {
                let root = self.dir.path().display().to_string();
                format!("{}{script}", KILL_SHIM.replace("@@DIR@@", &root))
            }

            /// Records `pid` verbatim, however malformed, with `port`.
            fn seed_raw_state(&self, pid: &str, port: u16) {
                self.seed_state(None, port);
                fs::write(self.state_dir().join("pid"), format!("{pid}\n"))
                    .expect("seed raw pid file");
                fs::write(self.state_dir().join("managed"), "managed\n")
                    .expect("seed managed file");
            }

            /// Recorded servers still running.
            fn live_servers(&self) -> Vec<u32> {
                self.pids("serve.pids")
                    .into_iter()
                    .filter(|pid| !fixture_process_is_gone(*pid))
                    .collect()
            }

            fn with_limit(limit: Duration) -> Self {
                Self {
                    limit,
                    ..Self::new()
                }
            }

            fn home(&self) -> PathBuf {
                self.dir.path().join("home")
            }

            fn server_home(&self) -> String {
                self.home().join(".bibcode").display().to_string()
            }

            fn state_dir(&self) -> PathBuf {
                self.home().join(".bibcode-ssh-launch").join(STATE_KEY)
            }

            fn state_file(&self, name: &str) -> Option<String> {
                fs::read_to_string(self.state_dir().join(name))
                    .ok()
                    .map(|contents| contents.trim().to_string())
            }

            /// Records a managed server as an earlier launch would: `pid`
            /// (when given), `port`, and the `managed` marker.
            fn seed_state(&self, pid: Option<u32>, port: u16) {
                fs::create_dir_all(self.state_dir()).expect("fixture state directory");
                if let Some(pid) = pid {
                    fs::write(self.state_dir().join("pid"), format!("{pid}\n"))
                        .expect("seed pid file");
                    fs::write(self.state_dir().join("managed"), "managed\n")
                        .expect("seed managed file");
                }
                fs::write(self.state_dir().join("port"), format!("{port}\n"))
                    .expect("seed port file");
            }

            fn recorded(&self, name: &str) -> Vec<String> {
                fs::read_to_string(self.dir.path().join(name))
                    .unwrap_or_default()
                    .lines()
                    .map(str::to_string)
                    .collect()
            }

            fn pids(&self, name: &str) -> Vec<u32> {
                self.recorded(name)
                    .iter()
                    .filter_map(|pid| pid.trim().parse().ok())
                    .collect()
            }

            /// Kills every stand-in this host started, even when a test
            /// panics.
            fn cleanup(&self) -> [BackgroundedFixtureProcesses; 2] {
                [
                    BackgroundedFixtureProcesses {
                        pids_file: self.dir.path().join("serve.pids"),
                        marker: "serve.py",
                    },
                    BackgroundedFixtureProcesses {
                        pids_file: self.dir.path().join("pairing.pids"),
                        marker: "600",
                    },
                ]
            }

            /// Starts a `bibcode serve` stand-in outside the script, with
            /// the command line the launch script gives its server.
            fn spawn_recorded_server(&self, port: u16, env: &[(&str, &str)]) -> RecordedServer {
                let mut command = StdCommand::new("python3");
                command
                    .arg(self.dir.path().join("serve.py"))
                    .args(["serve", "--host", "127.0.0.1", "--port"])
                    .arg(port.to_string())
                    .arg("--base-dir")
                    .arg(self.server_home())
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                for (name, value) in env {
                    command.env(name, value);
                }
                let mut child = command.spawn().expect("recorded server stand-in");
                let pid = child.id();
                // Reap it as soon as it exits, so `kill -0` stops seeing it.
                let reaper = std::thread::spawn(move || {
                    let _ = child.wait();
                });
                let listening = std::time::Instant::now() + Duration::from_secs(10);
                while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
                    assert!(
                        std::time::Instant::now() < listening,
                        "recorded server never listened on {port}"
                    );
                    std::thread::sleep(Duration::from_millis(50));
                }
                RecordedServer {
                    pid,
                    reaper: Some(reaper),
                }
            }

            fn run(
                &self,
                shell: &[String],
                script: &str,
                args: &[&str],
                env: &[(&str, String)],
                output: ScriptOutput,
            ) -> ScriptRun {
                self.run_with_path(shell, script, args, env, output, None)
            }

            fn run_with_path(
                &self,
                shell: &[String],
                script: &str,
                args: &[&str],
                env: &[(&str, String)],
                output: ScriptOutput,
                search_path: Option<String>,
            ) -> ScriptRun {
                let bin = self.dir.path().join("bin").display().to_string();
                let search_path = search_path
                    .unwrap_or_else(|| format!("{bin}:{}", env::var("PATH").unwrap_or_default()));
                let stdout_path = self.dir.path().join("script.stdout");
                let stderr_path = self.dir.path().join("script.stderr");
                let mut command = StdCommand::new(&shell[0]);
                command
                    .args(&shell[1..])
                    .args(["-s", "--"])
                    .args(args)
                    .env_clear()
                    .env("HOME", self.home())
                    .env("PATH", search_path)
                    .current_dir(self.home())
                    .stdin(Stdio::piped());
                match output {
                    ScriptOutput::Files => {
                        command
                            .stdout(fs::File::create(&stdout_path).expect("stdout file"))
                            .stderr(fs::File::create(&stderr_path).expect("stderr file"));
                    }
                    ScriptOutput::Closed => {
                        command.stdout(Stdio::piped()).stderr(Stdio::piped());
                    }
                }
                for (name, value) in env {
                    command.env(name, value);
                }
                let started = std::time::Instant::now();
                let mut child = command.spawn().expect("script shell");
                drop(child.stdout.take());
                drop(child.stderr.take());
                child
                    .stdin
                    .take()
                    .expect("script stdin")
                    .write_all(script.as_bytes())
                    .expect("write the script");
                let limit = started + self.limit;
                let status = loop {
                    if let Some(status) = child.try_wait().expect("script status") {
                        break Some(status);
                    }
                    if std::time::Instant::now() >= limit {
                        let _ = child.kill();
                        let _ = child.wait();
                        break None;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                };
                ScriptRun {
                    shell: shell.join(" "),
                    status,
                    stdout: fs::read_to_string(&stdout_path).unwrap_or_default(),
                    stderr: fs::read_to_string(&stderr_path).unwrap_or_default(),
                    elapsed: started.elapsed(),
                }
            }

            async fn assert_servers_gone(&self, run: &ScriptRun) {
                let servers = self.pids("serve.pids");
                assert!(!servers.is_empty(), "{run:?}: the script started no server");
                for pid in servers {
                    assert!(
                        wait_until_gone(pid, Duration::from_secs(3)).await,
                        "{run:?}: server {pid} was left running"
                    );
                }
            }
        }

        /// A stand-in server started by the test; killed on drop if still
        /// running.
        struct RecordedServer {
            pid: u32,
            reaper: Option<std::thread::JoinHandle<()>>,
        }

        impl Drop for RecordedServer {
            fn drop(&mut self) {
                if !fixture_process_is_gone(self.pid) {
                    // SAFETY: signals the test's own child, which the reaper
                    // thread has not reaped yet, so the pid is still its own.
                    unsafe {
                        libc::kill(self.pid as libc::pid_t, libc::SIGKILL);
                    }
                }
                if let Some(reaper) = self.reaper.take() {
                    let _ = reaper.join();
                }
            }
        }

        #[tokio::test]
        async fn pairing_watchdog_ends_a_command_that_never_exits() {
            for shell in shells() {
                // `hang` dies on TERM; `ignore-term` needs the KILL 2 s later.
                for (mode, least, most) in [("hang", 0, 3), ("ignore-term", 2, 5)] {
                    let host = ScriptHost::with_limit(Duration::from_secs(10));
                    let _cleanup = host.cleanup();
                    let run = host.run(
                        &shell,
                        REMOTE_PAIRING_SCRIPT,
                        &["1"],
                        &[("FIXTURE_PAIRING", mode.to_string())],
                        ScriptOutput::Files,
                    );
                    assert_eq!(run.code(), Some(124), "{mode}: {run:?}");
                    assert!(
                        run.elapsed >= Duration::from_secs(least)
                            && run.elapsed < Duration::from_secs(most),
                        "{mode}: {run:?}"
                    );
                    let stand_ins = host.pids("pairing.pids");
                    assert_eq!(stand_ins.len(), 1, "{mode}: {run:?}");
                    for pid in stand_ins {
                        assert!(
                            wait_until_gone(pid, Duration::from_millis(500)).await,
                            "{mode}: {run:?}: stand-in {pid} was left behind"
                        );
                    }
                }
            }
        }

        fn delayed_interpreter_pairing_script() -> String {
            // The one-second watchdog may fire immediately at a clock boundary.
            // This fixture deliberately ignores TERM, so establish that inherited
            // disposition before the stand-in's first interpreter can start.
            format!("trap '' TERM\n{REMOTE_PAIRING_SCRIPT}")
        }

        #[test]
        fn delayed_pairing_fixture_inherits_term_immunity_before_its_first_interpreter() {
            let python = find_on_path("python3").expect("Python fixture interpreter");
            let python = fs::canonicalize(python).expect("absolute Python fixture interpreter");
            for shell in shells() {
                let host = ScriptHost::new();
                write_executable(
                    &host.dir.path().join("bin/bibcode"),
                    r#"#!/bin/sh
exec "$BIBCODE_FIXTURE_PYTHON" -c 'import signal
print("ignored" if signal.getsignal(signal.SIGTERM) == signal.SIG_IGN else "default")'
"#,
                );
                let script = delayed_interpreter_pairing_script();
                let run = host.run(
                    &shell,
                    &script,
                    &["35"],
                    &[("BIBCODE_FIXTURE_PYTHON", python.display().to_string())],
                    ScriptOutput::Files,
                );
                assert_eq!(run.code(), Some(0), "{run:?}");
                assert_eq!(
                    run.stdout.trim(),
                    "ignored",
                    "TERM immunity must be inherited before the stand-in can execute any trap"
                );
            }
        }

        #[tokio::test]
        async fn pairing_watchdog_escalates_when_the_interpreter_starts_late() {
            let python = find_on_path("python3").expect("Python fixture interpreter");
            let python = fs::canonicalize(python).expect("absolute Python fixture interpreter");
            for shell in shells() {
                let host = ScriptHost::with_limit(Duration::from_secs(10));
                let _cleanup = host.cleanup();
                // Keep one PID through the delay and both execs, so the
                // stand-in must inherit TERM immunity before Python starts.
                write_executable(
                    &host.dir.path().join("bin/python3"),
                    r#"#!/bin/sh
exec "$BIBCODE_FIXTURE_PYTHON" -c 'import os, sys, time
time.sleep(1.2)
os.execv(sys.argv[1], sys.argv[1:])' "$BIBCODE_FIXTURE_PYTHON" "$@"
"#,
                );
                let script = delayed_interpreter_pairing_script();
                let run = host.run(
                    &shell,
                    &script,
                    &["1"],
                    &[
                        ("FIXTURE_PAIRING", "ignore-term".to_string()),
                        ("BIBCODE_FIXTURE_PYTHON", python.display().to_string()),
                    ],
                    ScriptOutput::Files,
                );
                assert_eq!(run.code(), Some(124), "{run:?}");
                assert!(
                    run.elapsed >= Duration::from_secs(2) && run.elapsed < Duration::from_secs(5),
                    "the delayed interpreter must survive TERM until watchdog KILL: {run:?}"
                );
                let stand_ins = host.pids("pairing.pids");
                assert_eq!(stand_ins.len(), 1, "{run:?}");
                for pid in stand_ins {
                    assert!(
                        wait_until_gone(pid, Duration::from_millis(500)).await,
                        "{run:?}: stand-in {pid} was left behind"
                    );
                }
            }
        }

        #[test]
        fn pairing_watchdog_passes_the_command_output_and_status_through() {
            for shell in shells() {
                for (mode, code) in [("ok", 0), ("fail", 7)] {
                    let host = ScriptHost::new();
                    let _cleanup = host.cleanup();
                    let run = host.run(
                        &shell,
                        REMOTE_PAIRING_SCRIPT,
                        &["35"],
                        &[("FIXTURE_PAIRING", mode.to_string())],
                        ScriptOutput::Files,
                    );
                    assert_eq!(run.code(), Some(code), "{mode}: {run:?}");
                    assert_eq!(run.stdout.trim(), CREDENTIAL_LINE, "{mode}: {run:?}");
                    assert!(run.elapsed < Duration::from_secs(3), "{mode}: {run:?}");
                }
            }
        }

        #[tokio::test]
        async fn launch_readiness_ends_at_its_wall_clock_limit_when_every_probe_hangs() {
            let limits = RemoteLaunchLimits { ready: 2, reuse: 2 };
            for shell in shells() {
                let host = ScriptHost::new();
                let script = host.launch_script(limits);
                let _cleanup = host.cleanup();
                host.seed_state(None, free_port());
                let run = host.run(
                    &shell,
                    &script,
                    &[STATE_KEY],
                    &[("FIXTURE_SERVE_MODE", "silent".to_string())],
                    ScriptOutput::Files,
                );
                assert_eq!(run.code(), Some(1), "{run:?}");
                assert!(run.stderr.contains("did not become ready"), "{run:?}");
                // At least the limit; at most the limit, the clock's whole
                // second, one probe and the stop.
                assert!(
                    run.elapsed >= Duration::from_secs(2) && run.elapsed < Duration::from_secs(8),
                    "{run:?}"
                );
                host.assert_servers_gone(&run).await;
                assert_eq!(host.state_file("pid"), None, "{run:?}");
                assert_eq!(host.state_file("port"), None, "{run:?}");
            }
        }

        /// The readiness wait with only `wget` on `PATH`. GNU wget retries a
        /// timed-out read many times, so each probe runs under the watchdog.
        #[tokio::test]
        async fn launch_readiness_with_wget_ends_at_its_wall_clock_limit() {
            let Some(_) = find_on_path("wget") else {
                eprintln!("skipped: wget is not installed");
                return;
            };
            let limits = RemoteLaunchLimits { ready: 2, reuse: 2 };
            for shell in shells() {
                let host = ScriptHost::new();
                let script = host.launch_script(limits);
                let _cleanup = host.cleanup();
                // A PATH with the stand-in `bibcode` and the tools the
                // scripts use, but no curl.
                let tools = host.dir.path().join("tools");
                fs::create_dir_all(&tools).expect("tools directory");
                for tool in [
                    "awk", "cat", "chmod", "date", "grep", "mkdir", "mv", "nohup", "ps", "python3",
                    "rm", "sh", "sleep", "ss", "tail", "tr", "wget",
                ] {
                    if let Some(found) = find_on_path(tool) {
                        std::os::unix::fs::symlink(found, tools.join(tool)).expect("link tool");
                    }
                }
                let search_path = format!(
                    "{}:{}",
                    host.dir.path().join("bin").display(),
                    tools.display()
                );
                host.seed_state(None, free_port());
                let run = host.run_with_path(
                    &shell,
                    &script,
                    &[STATE_KEY],
                    &[("FIXTURE_SERVE_MODE", "silent".to_string())],
                    ScriptOutput::Files,
                    Some(search_path),
                );
                assert_eq!(run.code(), Some(1), "{run:?}");
                assert!(run.stderr.contains("did not become ready"), "{run:?}");
                assert!(
                    run.elapsed >= Duration::from_secs(2) && run.elapsed < Duration::from_secs(10),
                    "{run:?}"
                );
                host.assert_servers_gone(&run).await;
            }
        }

        #[tokio::test]
        async fn launch_not_ready_stops_its_server_even_when_its_output_is_closed() {
            let limits = RemoteLaunchLimits { ready: 1, reuse: 1 };
            for shell in shells() {
                let host = ScriptHost::new();
                let script = host.launch_script(limits);
                let _cleanup = host.cleanup();
                host.seed_state(None, free_port());
                let run = host.run(
                    &shell,
                    &script,
                    &[STATE_KEY],
                    &[("FIXTURE_SERVE_MODE", "silent".to_string())],
                    ScriptOutput::Closed,
                );
                assert!(run.status.is_some(), "{run:?}");
                host.assert_servers_gone(&run).await;
                assert_eq!(host.state_file("pid"), None, "{run:?}");
                assert_eq!(host.state_file("port"), None, "{run:?}");
                assert_eq!(host.state_file("managed"), None, "{run:?}");
            }
        }

        #[tokio::test]
        async fn launch_stops_a_live_silent_recorded_server_before_starting_one_replacement() {
            let limits = RemoteLaunchLimits {
                ready: 10,
                reuse: 2,
            };
            for shell in shells() {
                let host = ScriptHost::new();
                let script = host.launch_script(limits);
                let _cleanup = host.cleanup();
                let port = free_port();
                let old = host.spawn_recorded_server(port, &[("FIXTURE_SERVE_MODE", "silent")]);
                host.seed_state(Some(old.pid), port);
                let run = host.run(
                    &shell,
                    &script,
                    &[STATE_KEY],
                    &[("FIXTURE_OLD_PID", old.pid.to_string())],
                    ScriptOutput::Files,
                );
                assert_eq!(run.code(), Some(0), "{run:?}");
                let launched = parse_remote_launch_result(&run.stdout).expect("launch result");
                assert_eq!(
                    host.recorded("serve-starts.log"),
                    ["old-gone"],
                    "{run:?}: exactly one replacement, started after the old server stopped"
                );
                assert!(fixture_process_is_gone(old.pid), "{run:?}");
                let replacement = host.pids("serve.pids");
                assert_eq!(
                    host.state_file("pid"),
                    replacement.first().map(u32::to_string),
                    "{run:?}"
                );
                assert_eq!(
                    host.state_file("port"),
                    Some(launched.remote_port.to_string()),
                    "{run:?}"
                );
            }
        }

        #[test]
        fn launch_never_kills_a_recorded_pid_that_is_not_its_server() {
            let limits = RemoteLaunchLimits {
                ready: 10,
                reuse: 2,
            };
            for shell in shells() {
                let host = ScriptHost::new();
                let script = host.launch_script(limits);
                let _cleanup = host.cleanup();
                let mut stale = StdCommand::new("sleep")
                    .arg("60")
                    .spawn()
                    .expect("unrelated process");
                host.seed_state(Some(stale.id()), free_port());
                let run = host.run(&shell, &script, &[STATE_KEY], &[], ScriptOutput::Files);
                let survived = stale
                    .try_wait()
                    .expect("unrelated process status")
                    .is_none();
                let _ = stale.kill();
                let _ = stale.wait();
                assert_eq!(run.code(), Some(0), "{run:?}");
                assert!(survived, "{run:?}: the unrelated process was killed");
                assert_eq!(
                    host.state_file("pid"),
                    host.pids("serve.pids").first().map(u32::to_string),
                    "{run:?}"
                );
            }
        }

        #[tokio::test]
        async fn launch_keeps_state_files_that_name_another_pid() {
            let limits = RemoteLaunchLimits { ready: 2, reuse: 2 };
            for shell in shells() {
                let host = ScriptHost::new();
                let script = host.launch_script(limits);
                let _cleanup = host.cleanup();
                let port = free_port();
                host.seed_state(None, port);
                let pid_file = host.state_dir().join("pid").display().to_string();
                let run = host.run(
                    &shell,
                    &script,
                    &[STATE_KEY],
                    &[
                        ("FIXTURE_SERVE_MODE", "silent".to_string()),
                        ("FIXTURE_REWRITE_PID_FILE", pid_file),
                    ],
                    ScriptOutput::Files,
                );
                assert_eq!(run.code(), Some(1), "{run:?}");
                host.assert_servers_gone(&run).await;
                assert_eq!(host.state_file("pid").as_deref(), Some("4242"), "{run:?}");
                assert!(host.state_file("port").is_some(), "{run:?}");
            }
        }

        #[test]
        fn stop_never_kills_a_recorded_pid_that_is_not_its_server() {
            for shell in shells() {
                let host = ScriptHost::new();
                let mut stale = StdCommand::new("sleep")
                    .arg("60")
                    .spawn()
                    .expect("unrelated process");
                host.seed_state(Some(stale.id()), free_port());
                let run = host.run(
                    &shell,
                    &build_remote_stop_script_with(2),
                    &[STATE_KEY],
                    &[],
                    ScriptOutput::Files,
                );
                std::thread::sleep(Duration::from_millis(200));
                let survived = stale
                    .try_wait()
                    .expect("unrelated process status")
                    .is_none();
                let _ = stale.kill();
                let _ = stale.wait();
                assert_eq!(run.code(), Some(0), "{run:?}");
                assert_eq!(run.stdout.trim(), r#"{"stopped":true}"#, "{run:?}");
                assert!(survived, "{run:?}: the unrelated process was killed");
                assert_eq!(host.state_file("pid"), None, "{run:?}");
                assert_eq!(host.state_file("port"), None, "{run:?}");
            }
        }

        #[tokio::test]
        async fn stop_terminates_its_recorded_server_and_never_forces_it() {
            for shell in shells() {
                for ignores_term in [false, true] {
                    let host = ScriptHost::new();
                    let port = free_port();
                    let mut env = vec![("FIXTURE_SERVE_MODE", "silent")];
                    if ignores_term {
                        env.push(("FIXTURE_IGNORE_TERM", "1"));
                    }
                    let server = host.spawn_recorded_server(port, &env);
                    host.seed_state(Some(server.pid), port);
                    let run = host.run(
                        &shell,
                        &build_remote_stop_script_with(2),
                        &[STATE_KEY],
                        &[],
                        ScriptOutput::Files,
                    );
                    assert_eq!(run.code(), Some(0), "{run:?}");
                    let gone = wait_until_gone(server.pid, Duration::from_secs(3)).await;
                    assert_eq!(
                        gone, !ignores_term,
                        "{run:?}: TERM stops the server; nothing forces one that ignores it"
                    );
                    if ignores_term {
                        assert_eq!(run.stdout.trim(), r#"{"stopped":false}"#, "{run:?}");
                        assert_eq!(
                            host.state_file("pid"),
                            Some(server.pid.to_string()),
                            "{run:?}: a server still running keeps its record"
                        );
                    } else {
                        assert_eq!(run.stdout.trim(), r#"{"stopped":true}"#, "{run:?}");
                        assert_eq!(host.state_file("pid"), None, "{run:?}");
                    }
                }
            }
        }

        #[test]
        fn launch_never_signals_or_trusts_a_malformed_recorded_pid() {
            let limits = RemoteLaunchLimits {
                ready: 10,
                reuse: 2,
            };
            for shell in shells() {
                for pid in MALFORMED_PIDS {
                    let host = ScriptHost::new();
                    let _cleanup = host.cleanup();
                    host.seed_raw_state(pid, free_port());
                    let script = host.with_kill_shim(&host.launch_script(limits));
                    let run = host.run(&shell, &script, &[STATE_KEY], &[], ScriptOutput::Files);
                    assert_eq!(run.code(), Some(0), "{pid:?}: {run:?}");
                    assert_eq!(
                        host.recorded("kill.log"),
                        Vec::<String>::new(),
                        "{pid:?}: {run:?}: no kill may be attempted"
                    );
                    let started = host.pids("serve.pids");
                    assert_eq!(
                        started.len(),
                        1,
                        "{pid:?}: {run:?}: one new server replaces the untrusted record"
                    );
                    assert_eq!(
                        host.state_file("pid"),
                        started.first().map(u32::to_string),
                        "{pid:?}: {run:?}"
                    );
                }
            }
        }

        #[test]
        fn stop_never_signals_a_malformed_recorded_pid() {
            for shell in shells() {
                for pid in MALFORMED_PIDS {
                    let host = ScriptHost::new();
                    host.seed_raw_state(pid, free_port());
                    let script = host.with_kill_shim(&build_remote_stop_script_with(2));
                    let run = host.run(&shell, &script, &[STATE_KEY], &[], ScriptOutput::Files);
                    assert_eq!(run.code(), Some(0), "{pid:?}: {run:?}");
                    assert_eq!(run.stdout.trim(), r#"{"stopped":true}"#, "{pid:?}: {run:?}");
                    assert_eq!(
                        host.recorded("kill.log"),
                        Vec::<String>::new(),
                        "{pid:?}: {run:?}: no kill may be attempted"
                    );
                    assert_eq!(
                        host.state_file("pid"),
                        None,
                        "{pid:?}: {run:?}: the untrusted record is dropped"
                    );
                }
            }
        }

        /// Disconnect, then Connect: the launch that follows a stop must not
        /// start a server beside one that is still shutting down.
        #[tokio::test]
        async fn a_launch_right_after_stop_never_runs_beside_the_stopping_server() {
            let limits = RemoteLaunchLimits {
                ready: 10,
                reuse: 2,
            };
            for shell in shells() {
                let host = ScriptHost::new();
                let _cleanup = host.cleanup();
                let port = free_port();
                let old = host.spawn_recorded_server(port, &[("FIXTURE_TERM_DELAY", "2")]);
                host.seed_state(Some(old.pid), port);

                let stop = host.run(
                    &shell,
                    &build_remote_stop_script_with(5),
                    &[STATE_KEY],
                    &[],
                    ScriptOutput::Files,
                );
                let launch = host.run(
                    &shell,
                    &host.launch_script(limits),
                    &[STATE_KEY],
                    &[("FIXTURE_OLD_PID", old.pid.to_string())],
                    ScriptOutput::Files,
                );

                assert_eq!(stop.code(), Some(0), "{stop:?}");
                assert_eq!(stop.stdout.trim(), r#"{"stopped":true}"#, "{stop:?}");
                assert_eq!(launch.code(), Some(0), "{launch:?}");
                assert_eq!(
                    host.recorded("serve-starts.log"),
                    ["old-gone"],
                    "{stop:?}\n{launch:?}: one replacement, started after the stopped server exited"
                );
            }
        }

        /// Stop never forces a server; one that outlives its wait keeps its
        /// record, and the next launch stops it (TERM, then KILL) before it
        /// starts exactly one replacement.
        #[tokio::test]
        async fn a_server_that_outlives_stop_is_replaced_by_the_next_launch_alone() {
            let limits = RemoteLaunchLimits {
                ready: 10,
                reuse: 2,
            };
            for shell in shells() {
                let host = ScriptHost::new();
                let _cleanup = host.cleanup();
                let port = free_port();
                let old = host.spawn_recorded_server(
                    port,
                    &[
                        ("FIXTURE_SERVE_MODE", "silent"),
                        ("FIXTURE_IGNORE_TERM", "1"),
                    ],
                );
                host.seed_state(Some(old.pid), port);

                let stop = host.run(
                    &shell,
                    &build_remote_stop_script_with(2),
                    &[STATE_KEY],
                    &[],
                    ScriptOutput::Files,
                );
                assert_eq!(stop.code(), Some(0), "{stop:?}");
                assert_eq!(stop.stdout.trim(), r#"{"stopped":false}"#, "{stop:?}");
                assert!(
                    !fixture_process_is_gone(old.pid),
                    "{stop:?}: stop never forces it"
                );
                assert_eq!(
                    host.state_file("pid"),
                    Some(old.pid.to_string()),
                    "{stop:?}"
                );

                let launch = host.run(
                    &shell,
                    &host.launch_script(limits),
                    &[STATE_KEY],
                    &[("FIXTURE_OLD_PID", old.pid.to_string())],
                    ScriptOutput::Files,
                );
                assert_eq!(launch.code(), Some(0), "{launch:?}");
                assert_eq!(
                    host.recorded("serve-starts.log"),
                    ["old-gone"],
                    "{launch:?}: exactly one replacement, after the old server was killed"
                );
                assert!(
                    wait_until_gone(old.pid, Duration::from_secs(3)).await,
                    "{launch:?}"
                );
            }
        }

        /// A launch cut short while it records its server (here a state
        /// write fails) must never leave a running server unrecorded, or
        /// recorded with another port: the next launch would not recognise it
        /// and would start a second server beside it. Case "state directory":
        /// the first state write fails. Case "pid record": the server's own
        /// pid write fails.
        #[tokio::test]
        async fn a_launch_cut_short_while_recording_its_server_leaves_one_server() {
            use std::os::unix::fs::PermissionsExt;

            // SAFETY: `geteuid` only reads this process's effective user id.
            if unsafe { libc::geteuid() } == 0 {
                eprintln!("skipped: permissions do not stop root");
                return;
            }
            let limits = RemoteLaunchLimits { ready: 2, reuse: 2 };
            for shell in shells() {
                for case in ["state directory", "pid record"] {
                    let host = ScriptHost::new();
                    let _cleanup = host.cleanup();
                    let state_dir = host.state_dir();
                    fs::create_dir_all(&state_dir).expect("state directory");
                    let blocked = state_dir.join("pid.tmp");
                    if case == "state directory" {
                        // Files the launch rewrites in place stay writable;
                        // creating any new file in the directory fails.
                        fs::write(state_dir.join("run-bibcode.sh"), "").expect("runner file");
                        fs::write(state_dir.join("server.log"), "").expect("log file");
                        fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o555))
                            .expect("read-only state directory");
                    } else {
                        fs::create_dir(&blocked).expect("block the pid record");
                    }

                    let cut_short = host.run(
                        &shell,
                        &host.launch_script(limits),
                        &[STATE_KEY],
                        &[],
                        ScriptOutput::Files,
                    );
                    fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o755))
                        .expect("writable state directory");
                    let _ = fs::remove_dir(&blocked);
                    let next = host.run(
                        &shell,
                        &host.launch_script(limits),
                        &[STATE_KEY],
                        &[],
                        ScriptOutput::Files,
                    );

                    assert_ne!(cut_short.code(), Some(0), "{case}: {cut_short:?}");
                    assert_eq!(next.code(), Some(0), "{case}: {next:?}");
                    assert_eq!(
                        host.live_servers().len(),
                        1,
                        "{case}: {cut_short:?}\n{next:?}: never two servers on one data root"
                    );
                }
            }
        }
    }

    /// Stand-ins for the OpenSSH client. Each fake records the remote command
    /// line it receives (options stripped, the rest joined with spaces, as
    /// `ssh(1)` sends it) and then runs a shell body. Nothing leaves the host.
    #[cfg(unix)]
    mod fake_ssh {
        use super::*;

        const PRELUDE: &str = r#"#!/bin/sh
dir='@@DIR@@'
while [ "$#" -gt 0 ]; do
  case "$1" in
    -L) forward="$2"; shift 2 ;;
    -o|-p) shift 2 ;;
    -n|-N|-T) shift ;;
    -*) printf 'fake ssh: unsupported option %s\n' "$1" >&2; exit 255 ;;
    *) break ;;
  esac
done
shift
printf '%s\n' "$*" >>"$dir/invocations.log"
"#;

        /// `sshd(8)` semantics: the joined command runs under `/bin/sh -c`
        /// with the fixture's remote HOME and a PATH holding only the
        /// recording `bibcode`.
        const JOINING_BODY: &str = r#"exec env -i HOME="$dir/remote-home" PATH="$dir/remote-bin:/usr/bin:/bin" /bin/sh -c "$*"
"#;

        const RECORDING_BIBCODE: &str = r#"#!/bin/sh
dir='@@DIR@@'
if [ "$#" -eq 0 ]; then
  printf 'fixture: bare bibcode would start a foreground server\n' >&2
  exit 97
fi
{
  for arg in "$@"; do printf '[%s] ' "$arg"; done
  printf '\n'
} >>"$dir/bibcode-argv.log"
count=$(wc -l <"$dir/bibcode-argv.log" | tr -d ' ')
printf '{"credential":"fixture-credential-%s"}\n' "$count"
"#;

        pub(super) struct FakeSsh {
            pub(super) dir: tempfile::TempDir,
            pub(super) program: PathBuf,
        }

        impl FakeSsh {
            pub(super) fn joining() -> Self {
                Self::with_body(JOINING_BODY)
            }

            pub(super) fn with_body(body: &str) -> Self {
                let dir = tempfile::tempdir().expect("fake ssh directory");
                let root = dir.path().display().to_string();
                fs::create_dir_all(dir.path().join("remote-home")).expect("fake remote home");
                fs::create_dir_all(dir.path().join("remote-bin")).expect("fake remote bin");
                let program = dir.path().join("ssh");
                write_executable(
                    &program,
                    &format!("{PRELUDE}{body}").replace("@@DIR@@", &root),
                );
                write_executable(
                    &dir.path().join("remote-bin/bibcode"),
                    &RECORDING_BIBCODE.replace("@@DIR@@", &root),
                );
                Self { dir, program }
            }

            pub(super) fn manager(
                &self,
                deadlines: SshOperationDeadlines,
            ) -> SshEnvironmentManager {
                SshEnvironmentManager::with_options(
                    self.dir.path().to_path_buf(),
                    self.program.display().to_string(),
                    deadlines,
                )
            }

            pub(super) fn invocations(&self) -> Vec<String> {
                read_lines(&self.dir.path().join("invocations.log"))
            }

            pub(super) fn bibcode_argv(&self) -> Vec<String> {
                read_lines(&self.dir.path().join("bibcode-argv.log"))
            }

            pub(super) fn remote_home(&self) -> PathBuf {
                self.dir.path().join("remote-home")
            }

            pub(super) fn path(&self, name: &str) -> PathBuf {
                self.dir.path().join(name)
            }
        }

        fn read_lines(path: &Path) -> Vec<String> {
            fs::read_to_string(path)
                .unwrap_or_default()
                .lines()
                .map(|line| line.trim_end().to_string())
                .filter(|line| !line.is_empty())
                .collect()
        }

        fn write_executable(path: &Path, contents: &str) {
            crate::test_support::write_executable_fixture(path, contents, 0o755);
        }
    }

    fn fixture_target() -> SshEnvironmentTarget {
        SshEnvironmentTarget {
            alias: "fixture-host".to_string(),
            hostname: "fixture-host".to_string(),
            username: None,
            port: None,
        }
    }

    #[cfg(unix)]
    #[derive(Clone, Default)]
    struct StopWarningRecorder(Arc<Mutex<Vec<String>>>);

    #[cfg(unix)]
    impl tracing::Subscriber for StopWarningRecorder {
        fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
            *metadata.level() == tracing::Level::WARN
        }

        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            let mut fields = String::new();
            event.record(
                &mut |field: &tracing::field::Field, value: &dyn std::fmt::Debug| {
                    use std::fmt::Write as _;
                    write!(&mut fields, "{}={value:?} ", field.name()).expect("record warning");
                },
            );
            self.0.lock().expect("stop warnings").push(fields);
        }

        fn enter(&self, _: &tracing::span::Id) {}

        fn exit(&self, _: &tracing::span::Id) {}
    }

    #[cfg(unix)]
    async fn disconnect_with_stop_output(output: &str) -> Vec<String> {
        use tracing::instrument::WithSubscriber as _;

        let fake = fake_ssh::FakeSsh::with_body("cat >/dev/null\ncat \"$dir/stop-output\"\n");
        fs::write(fake.path("stop-output"), output).expect("stop script output");
        let manager = fake.manager(SshOperationDeadlines::default());
        let app = mock_app();
        let warnings = StopWarningRecorder::default();
        let target = fixture_target();
        manager
            .remember_auth_secret(
                &target_connection_key(&target),
                "fixture-password".to_string(),
            )
            .expect("cache fixture authentication");

        let result = manager
            .disconnect_environment(app.handle(), &SshPasswordPromptManager::new(), target)
            .with_subscriber(warnings.clone())
            .await;
        manager.shutdown().await;

        assert_eq!(result, Ok(()), "a stop result must not fail Disconnect");
        let warnings = warnings.0.lock().expect("stop warnings").clone();
        assert!(
            warnings
                .iter()
                .all(|warning| !warning.contains("fixture-password"))
        );
        warnings
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn disconnect_warns_once_when_remote_stop_reports_a_surviving_server() {
        let warnings = disconnect_with_stop_output("banner\n{\"stopped\":false}\n\n").await;

        assert_eq!(warnings.len(), 1, "a surviving server needs one warning");
        assert!(warnings[0].contains("fixture-host"), "{warnings:?}");
        assert!(warnings[0].contains("still running"), "{warnings:?}");
        assert!(warnings[0].contains("record was kept"), "{warnings:?}");
        assert!(warnings[0].contains("next launch"), "{warnings:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn disconnect_warns_once_when_remote_stop_output_is_unparseable() {
        let warnings = disconnect_with_stop_output("garbage fixture-output-secret").await;

        assert_eq!(warnings.len(), 1, "an unparseable result needs one warning");
        assert!(warnings[0].contains("fixture-host"), "{warnings:?}");
        assert!(warnings[0].contains("stop"), "{warnings:?}");
        assert!(warnings[0].contains("unparseable"), "{warnings:?}");
        assert!(
            !warnings[0].contains("fixture-output-secret"),
            "{warnings:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn disconnect_succeeds_without_warning_when_remote_stop_reports_success() {
        let warnings = disconnect_with_stop_output("banner\n{\"stopped\":true}\n\n").await;

        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn parses_remote_stop_result_from_last_non_empty_line() {
        assert_eq!(
            parse_remote_stop_result("banner\n{\"stopped\":true}\n\n"),
            Ok(true)
        );
        assert_eq!(
            parse_remote_stop_result("banner\n{\"stopped\":false}\n\n"),
            Ok(false)
        );
        for output in [
            "garbage",
            "",
            "{}",
            "{\"stopped\":\"false\"}",
            "{\"stopped\":null}",
        ] {
            assert!(parse_remote_stop_result(output).is_err(), "{output}");
        }
    }

    #[cfg(unix)]
    fn mock_app() -> tauri::App<tauri::test::MockRuntime> {
        use tauri::test::{mock_builder, mock_context, noop_assets};
        mock_builder()
            .build(mock_context(noop_assets()))
            .expect("mock Tauri app")
    }

    /// A loopback endpoint that answers every request with `200 {}`, standing
    /// in for the remote server behind a live tunnel.
    #[cfg(unix)]
    async fn spawn_ready_endpoint() -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture endpoint");
        let port = listener
            .local_addr()
            .expect("fixture endpoint address")
            .port();
        let server = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut request = [0_u8; 2048];
                    let _ = stream.read(&mut request).await;
                    let _ = stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}",
                        )
                        .await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        (format!("http://127.0.0.1:{port}/"), server)
    }

    /// Publishes a live stand-in tunnel (a sleeping child) whose bootstrap
    /// points at `http_base_url`, as a completed `ensure_environment` would.
    #[cfg(unix)]
    fn publish_fixture_tunnel(
        manager: &SshEnvironmentManager,
        http_base_url: &str,
        pairing_token: Option<&str>,
    ) -> (String, u32) {
        publish_tunnel_for(manager, fixture_target(), http_base_url, pairing_token)
    }

    #[cfg(unix)]
    fn publish_tunnel_for(
        manager: &SshEnvironmentManager,
        target: SshEnvironmentTarget,
        http_base_url: &str,
        pairing_token: Option<&str>,
    ) -> (String, u32) {
        let launcher = manager.askpass_launcher().expect("askpass launcher");
        let mut command = Command::new("sh");
        command
            .args(["-c", "exec sleep 60"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let child = spawn_managed_ssh_child(command, launcher, "start fixture tunnel")
            .expect("fixture tunnel child");
        let pid = child
            .child
            .as_ref()
            .and_then(Child::id)
            .expect("fixture tunnel pid");
        let target = normalize_ssh_environment_target(target).expect("fixture target");
        let key = target_connection_key(&target);
        let bootstrap = SshEnvironmentBootstrap::new(
            target,
            4000,
            http_base_url.to_string(),
            http_base_url.replace("http://", "ws://"),
            pairing_token.map(str::to_string),
            "managed",
        );
        if manager
            .publish_tunnel(key.clone(), child, bootstrap)
            .is_err()
        {
            panic!("fixture tunnel should publish");
        }
        (key, pid)
    }

    /// The remote command line of a pairing script with default deadlines:
    /// `sh -s --` and the watchdog bound, 30 s + 5 s.
    #[cfg(unix)]
    fn pairing_invocation() -> String {
        format!(
            "sh -s -- {}",
            pairing_watchdog_bound(SshOperationDeadlines::default().pairing)
        )
    }

    /// Only the operation's own deadline may produce `[ssh_timeout:…]`. An
    /// I/O error that merely has the `TimedOut` kind (on Windows a cancelled
    /// pipe read, `ERROR_OPERATION_ABORTED`, decodes to it) is a failure.
    #[test]
    fn an_io_error_of_the_timed_out_kind_is_not_the_deadline() {
        let failure = wait_failure(
            io::Error::new(io::ErrorKind::TimedOut, "pipe read cancelled"),
            "pairing",
        );
        match failure {
            RemoteScriptFailure::Wait(message) => {
                assert!(!message.starts_with("[ssh_timeout:"), "{message}");
                assert!(message.contains("pipe read cancelled"), "{message}");
            }
            RemoteScriptFailure::Write(message) => panic!("not a write failure: {message}"),
            RemoteScriptFailure::TimedOut => {
                panic!("a TimedOut-kind I/O error must not be reported as the deadline")
            }
        }
    }

    #[test]
    fn pairing_watchdog_bound_sits_above_the_deadline_in_whole_seconds() {
        assert_eq!(pairing_watchdog_bound(SSH_PAIRING_DEADLINE), 35);
        assert_eq!(pairing_watchdog_bound(Duration::from_millis(300)), 6);
        assert_eq!(pairing_watchdog_bound(Duration::from_secs(3)), 8);
        assert_eq!(pairing_watchdog_bound(Duration::MAX), i32::MAX as u64);
        assert_eq!(
            SshOperationDeadlines::default().pairing_watchdog_bound(),
            Duration::from_secs(35)
        );
    }

    #[cfg(unix)]
    async fn wait_until_exited(pid: u32) -> bool {
        for _ in 0..60 {
            if !process_is_alive(pid) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        false
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pairing_script_survives_openssh_argument_joining() {
        let fake = fake_ssh::FakeSsh::joining();
        let manager = fake.manager(SshOperationDeadlines::default());
        let launcher = manager.askpass_launcher().expect("askpass launcher");

        let credential = tokio::time::timeout(
            Duration::from_secs(10),
            issue_remote_pairing_token(
                &manager
                    .remote_runner(RemoteOperation::Pairing)
                    .expect("SSH runner"),
                &fixture_target(),
                &SshAuthOptions::batch(),
                launcher,
            ),
        )
        .await
        .expect("pairing must finish");

        assert_eq!(
            fake.bibcode_argv(),
            vec![format!(
                "[pairing] [issue] [--base-dir] [{}/.bibcode] [--json]",
                fake.remote_home().display()
            )],
            "the remote login shell must run the full pairing command (invocations: {:?})",
            fake.invocations()
        );
        assert_eq!(credential.as_deref(), Ok("fixture-credential-1"));
        assert_eq!(fake.invocations(), vec![pairing_invocation()]);
        manager.shutdown().await;
    }

    /// A pipe stand-in for `settle_pipe_reads`. Its read stays pending until
    /// it has been cancelled more than `ignored_cancels` times, as a Windows
    /// read still queued for a pool thread ignores a cancel, and then returns
    /// the cancellation error.
    struct PendingPipe {
        ignored_cancels: usize,
        cancels: AtomicUsize,
        returned: AtomicBool,
        polls_after_return: AtomicUsize,
        waker: Mutex<Option<std::task::Waker>>,
    }

    impl PendingPipe {
        fn new(ignored_cancels: usize) -> Self {
            Self {
                ignored_cancels,
                cancels: AtomicUsize::new(0),
                returned: AtomicBool::new(false),
                polls_after_return: AtomicUsize::new(0),
                waker: Mutex::new(None),
            }
        }
    }

    impl tokio::io::AsyncRead for PendingPipe {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            context: &mut std::task::Context<'_>,
            _buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            if self.returned.load(Ordering::SeqCst) {
                self.polls_after_return.fetch_add(1, Ordering::SeqCst);
            }
            if self.cancels.load(Ordering::SeqCst) > self.ignored_cancels {
                self.returned.store(true, Ordering::SeqCst);
                return std::task::Poll::Ready(Err(io::Error::from(io::ErrorKind::Interrupted)));
            }
            *self.waker.lock().expect("pending pipe waker") = Some(context.waker().clone());
            std::task::Poll::Pending
        }
    }

    impl CancellablePipe for PendingPipe {
        fn cancel_pending_read(&self) {
            self.cancels.fetch_add(1, Ordering::SeqCst);
            if let Some(waker) = self.waker.lock().expect("pending pipe waker").take() {
                waker.wake();
            }
        }
    }

    #[tokio::test]
    async fn settling_repeats_the_cancel_until_each_read_returns_and_never_polls_it_again() {
        let mut issued = PendingPipe::new(0);
        let mut queued = PendingPipe::new(2);
        let started = tokio::time::Instant::now();

        let pending =
            settle_pipe_reads(vec![&mut issued, &mut queued], Duration::from_secs(5)).await;

        assert_eq!(pending, 0);
        assert_eq!(issued.cancels.load(Ordering::SeqCst), 1);
        assert_eq!(
            queued.cancels.load(Ordering::SeqCst),
            3,
            "a read that ignores a cancel gets another"
        );
        assert_eq!(
            issued.polls_after_return.load(Ordering::SeqCst)
                + queued.polls_after_return.load(Ordering::SeqCst),
            0,
            "a read that returned is never polled again: that would start a new one"
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn settling_gives_up_on_a_read_that_never_returns() {
        let mut stuck = PendingPipe::new(usize::MAX);
        let started = tokio::time::Instant::now();

        let pending = settle_pipe_reads(vec![&mut stuck], Duration::from_millis(200)).await;

        let elapsed = started.elapsed();
        assert_eq!(pending, 1);
        assert!(
            elapsed >= Duration::from_millis(200) && elapsed < Duration::from_secs(2),
            "{elapsed:?}"
        );
        assert!(
            stuck.cancels.load(Ordering::SeqCst) > 1,
            "the cancel repeats"
        );
    }

    /// Outside Windows pipe reads belong to the I/O driver: a drain that gives
    /// up cancels nothing and adds no delay.
    #[cfg(not(windows))]
    #[tokio::test]
    async fn outside_windows_a_drain_that_gives_up_settles_nothing() {
        let mut stuck = PendingPipe::new(usize::MAX);
        tokio::time::timeout(
            Duration::from_millis(100),
            settle_unfinished_reads(vec![&mut stuck]),
        )
        .await
        .expect("returns at once");
        assert_eq!(stuck.cancels.load(Ordering::SeqCst), 0);
    }

    #[cfg(unix)]
    fn io_runtime_state(manager: &SshEnvironmentManager) -> &'static str {
        match *manager
            .io_runtime
            .state
            .lock()
            .expect("SSH I/O runtime state")
        {
            SshIoRuntimeState::NotStarted => "not started",
            SshIoRuntimeState::Running(_) => "running",
            SshIoRuntimeState::Stopped => "stopped",
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ssh_io_runtime_starts_on_first_use_and_stops_with_the_manager() {
        let fake = fake_ssh::FakeSsh::joining();
        let manager = fake.manager(SshOperationDeadlines::default());
        assert_eq!(io_runtime_state(&manager), "not started");

        let runner = manager
            .remote_runner(RemoteOperation::Pairing)
            .expect("SSH runner");
        assert_eq!(io_runtime_state(&manager), "running");
        let worker = run_on_ssh_io(&runner.io_runtime, async {
            Ok(std::thread::current().name().map(str::to_owned))
        })
        .await;
        assert_eq!(worker, Ok(Some("bibcode-ssh-io".to_string())));
        let credential = issue_remote_pairing_token(
            &runner,
            &fixture_target(),
            &SshAuthOptions::batch(),
            manager.askpass_launcher().expect("askpass launcher"),
        )
        .await;
        assert_eq!(credential.as_deref(), Ok("fixture-credential-1"));

        manager.shutdown().await;
        assert_eq!(io_runtime_state(&manager), "stopped");
        assert_eq!(
            manager.remote_runner(RemoteOperation::Pairing).err(),
            Some("SSH process owner is shutting down.".to_string())
        );
        assert_eq!(
            run_on_ssh_io(&runner.io_runtime, async { Ok(()) }).await,
            Err("SSH process owner is shutting down.".to_string()),
            "a handle taken earlier runs nothing once the runtime has stopped"
        );
    }

    /// A Tokio runtime dropped inside async code panics; the manager's drop
    /// must stop its I/O runtime in the background instead.
    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_a_manager_inside_async_code_stops_its_io_runtime() {
        let fake = fake_ssh::FakeSsh::joining();
        let manager = fake.manager(SshOperationDeadlines::default());
        let runner = manager
            .remote_runner(RemoteOperation::Pairing)
            .expect("SSH runner");
        issue_remote_pairing_token(
            &runner,
            &fixture_target(),
            &SshAuthOptions::batch(),
            manager.askpass_launcher().expect("askpass launcher"),
        )
        .await
        .expect("pairing on the I/O runtime");

        drop(manager);

        assert_eq!(
            run_on_ssh_io(&runner.io_runtime, async { Ok(()) }).await,
            Err("SSH process owner is shutting down.".to_string())
        );
    }

    /// The operation runs on the I/O runtime; abandoning the caller's future
    /// must still end it and reap its SSH child, as before the move.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_abandoned_remote_script_reaps_its_ssh_child() {
        let fake =
            fake_ssh::FakeSsh::with_body("printf '%s\\n' \"$$\" >\"$dir/pid\"\nexec sleep 60\n");
        let manager = fake.manager(SshOperationDeadlines::default());
        let runner = manager
            .remote_runner(RemoteOperation::Pairing)
            .expect("SSH runner");
        let launcher = manager.askpass_launcher().expect("askpass launcher");
        let operation = tokio::spawn(async move {
            issue_remote_pairing_token(
                &runner,
                &fixture_target(),
                &SshAuthOptions::batch(),
                launcher,
            )
            .await
        });
        let recorded = tokio::time::Instant::now() + Duration::from_secs(10);
        let pid = loop {
            if let Some(pid) = fs::read_to_string(fake.path("pid"))
                .ok()
                .and_then(|pid| pid.trim().parse::<u32>().ok())
            {
                break pid;
            }
            assert!(
                tokio::time::Instant::now() < recorded,
                "the fake ssh never started"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        };

        operation.abort();
        let _ = operation.await;

        assert!(
            wait_until_exited(pid).await,
            "the abandoned operation's SSH child must be killed and reaped"
        );
        tokio::time::timeout(Duration::from_secs(3), manager.child_reaper.wait())
            .await
            .expect("its cleanup ownership is released");
        manager.shutdown().await;
    }

    /// Windows reads and writes child pipes on a Tokio blocking pool. These
    /// run a stand-in ssh while the caller's pool is saturated, and check
    /// that SSH child I/O neither waits for it nor leaves a read parked on its
    /// own pool (Part 3 of the 2026-09-26 SSH watchdog design).
    #[cfg(windows)]
    mod windows_drain {
        use super::*;

        /// A stand-in ssh: a `.cmd` that reads its stdin to end of file, as
        /// ssh does, and prints a pairing line. With `hold_pipes` it first starts a
        /// PowerShell descendant that inherits stdout and stderr, records its
        /// pid, and keeps them open for 30 s.
        struct WindowsFakeSsh {
            dir: tempfile::TempDir,
            program: PathBuf,
        }

        impl WindowsFakeSsh {
            fn new(hold_pipes: bool) -> Self {
                let dir = tempfile::tempdir().expect("fake ssh directory");
                let program = dir.path().join("ssh.cmd");
                let mut script = String::from("@echo off\r\n");
                if hold_pipes {
                    script.push_str(
                        "start \"\" /b powershell -NoProfile -NonInteractive -Command \
                         \"Set-Content -LiteralPath '%~dp0descendant.pid' -Value $PID; \
                         Start-Sleep -Seconds 30\"\r\n",
                    );
                }
                // Read the script to end of file, as ssh does, before answering.
                script.push_str("findstr \"^\" >nul\r\n");
                script.push_str("echo {\"credential\":\"fixture-credential\"}\r\n");
                fs::write(&program, script).expect("write fake ssh");
                Self { dir, program }
            }

            fn manager(&self) -> SshEnvironmentManager {
                SshEnvironmentManager::with_options(
                    self.dir.path().to_path_buf(),
                    self.program.display().to_string(),
                    SshOperationDeadlines::default(),
                )
            }

            fn descendant_pid(&self) -> Option<u32> {
                fs::read_to_string(self.dir.path().join("descendant.pid"))
                    .ok()?
                    .trim()
                    .parse()
                    .ok()
            }
        }

        impl Drop for WindowsFakeSsh {
            fn drop(&mut self) {
                if let Some(pid) = self.descendant_pid() {
                    let _ = std::process::Command::new("taskkill")
                        .args(["/PID", &pid.to_string(), "/T", "/F"])
                        .status();
                }
            }
        }

        async fn issue(
            manager: &SshEnvironmentManager,
            runner: &RemoteScriptRunner,
        ) -> Result<String, String> {
            issue_remote_pairing_token(
                runner,
                &fixture_target(),
                &SshAuthOptions::batch(),
                manager.askpass_launcher().expect("askpass launcher"),
            )
            .await
        }

        #[test]
        fn pairing_output_is_parsed_while_the_callers_blocking_pool_is_saturated() {
            let caller = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .max_blocking_threads(1)
                .enable_all()
                .build()
                .expect("single-slot blocking runtime");
            caller.block_on(async {
                let (held_tx, held_rx) = std::sync::mpsc::sync_channel(1);
                let (release_tx, release_rx) = std::sync::mpsc::sync_channel::<()>(1);
                let blocker = tokio::task::spawn_blocking(move || {
                    held_tx.send(()).expect("signal the held slot");
                    let _ = release_rx.recv_timeout(Duration::from_secs(30));
                });
                held_rx
                    .recv_timeout(Duration::from_secs(5))
                    .expect("the caller's only blocking slot is held");

                let fake = WindowsFakeSsh::new(false);
                let manager = fake.manager();
                let runner = manager
                    .remote_runner(RemoteOperation::Pairing)
                    .expect("SSH runner");
                let started = std::time::Instant::now();
                let credential = issue(&manager, &runner).await;
                let elapsed = started.elapsed();

                let _ = release_tx.send(());
                blocker.await.expect("the blocker joins");
                manager.shutdown().await;
                assert_eq!(
                    credential.as_deref(),
                    Ok("fixture-credential"),
                    "SSH output must not queue behind the caller's blocking pool"
                );
                assert!(elapsed < Duration::from_secs(10), "took {elapsed:?}");
            });
        }

        #[test]
        fn a_descendant_holding_the_pipes_leaves_no_read_parked() {
            let caller = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .expect("caller runtime");
            caller.block_on(async {
                let holding = WindowsFakeSsh::new(true);
                let mut manager = holding.manager();
                // Two pool threads: exactly what a descendant holding stdout
                // and stderr could park.
                manager.io_runtime = SshIoRuntime::new(2);
                let runner = manager
                    .remote_runner(RemoteOperation::Pairing)
                    .expect("SSH runner");

                let started = std::time::Instant::now();
                let first = issue(&manager, &runner).await;
                let first_elapsed = started.elapsed();
                assert_eq!(first.as_deref(), Ok("fixture-credential"));
                assert!(
                    first_elapsed >= SSH_OUTPUT_DRAIN_GRACE,
                    "the descendant must hold the pipes until the drain gives up \
                     (took {first_elapsed:?})"
                );
                let recorded = std::time::Instant::now() + Duration::from_secs(10);
                while holding.descendant_pid().is_none() && std::time::Instant::now() < recorded {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }

                // A read still parked on either pipe would leave this run's
                // write and reads queued, and its output would be cut off.
                let plain = WindowsFakeSsh::new(false);
                let runner = RemoteScriptRunner {
                    program: plain.program.display().to_string(),
                    ..runner
                };
                let started = std::time::Instant::now();
                let second = issue(&manager, &runner).await;
                let second_elapsed = started.elapsed();

                manager.shutdown().await;
                assert_eq!(
                    second.as_deref(),
                    Ok("fixture-credential"),
                    "no pipe read may stay parked after the drain gives up"
                );
                assert!(
                    second_elapsed < Duration::from_secs(10),
                    "took {second_elapsed:?}"
                );
            });
        }
    }

    /// ssh that fails before reading its stdin (it could not connect) closes
    /// the pipe the script is being written to. Its exit status and stderr
    /// say why; a broken pipe must not replace them.
    #[cfg(unix)]
    #[tokio::test]
    async fn ssh_that_exits_before_reading_its_script_reports_its_own_error() {
        let fake = fake_ssh::FakeSsh::with_body(
            "printf 'fake ssh: connection refused\\n' >&2\nexit 255\n",
        );
        let manager = fake.manager(SshOperationDeadlines::default());
        // More than a pipe buffer holds, so the write cannot finish before
        // ssh exits.
        let script = format!(
            "{}#{}\n",
            remote_script_header!("pairing"),
            "x".repeat(256 * 1024)
        );

        let error = run_remote_ssh_script(
            &manager
                .remote_runner(RemoteOperation::Pairing)
                .expect("SSH runner"),
            &fixture_target(),
            &script,
            &[],
            &SshAuthOptions::batch(),
            manager.askpass_launcher().expect("askpass launcher"),
            "pairing",
        )
        .await
        .expect_err("ssh failed");

        assert!(
            error.starts_with("SSH pairing command failed with status"),
            "{error}"
        );
        assert!(error.contains("fake ssh: connection refused"), "{error}");
        manager.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_deadline_too_far_away_to_represent_means_no_limit() {
        let fake = fake_ssh::FakeSsh::joining();
        let manager = fake.manager(SshOperationDeadlines {
            pairing: Duration::MAX,
            ..SshOperationDeadlines::default()
        });
        let credential = issue_remote_pairing_token(
            &manager
                .remote_runner(RemoteOperation::Pairing)
                .expect("SSH runner"),
            &fixture_target(),
            &SshAuthOptions::batch(),
            manager.askpass_launcher().expect("askpass launcher"),
        )
        .await;

        assert_eq!(credential.as_deref(), Ok("fixture-credential-1"));
        assert_eq!(fake.invocations(), vec![format!("sh -s -- {}", i32::MAX)]);
        manager.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn never_exiting_ssh_fails_at_the_deadline_and_is_reaped() {
        let fake =
            fake_ssh::FakeSsh::with_body("printf '%s\\n' \"$$\" >\"$dir/pid\"\nexec sleep 60\n");
        let manager = fake.manager(SshOperationDeadlines {
            pairing: Duration::from_millis(300),
            ..SshOperationDeadlines::default()
        });
        let launcher = manager.askpass_launcher().expect("askpass launcher");

        let started = std::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            issue_remote_pairing_token(
                &manager
                    .remote_runner(RemoteOperation::Pairing)
                    .expect("SSH runner"),
                &fixture_target(),
                &SshAuthOptions::batch(),
                launcher,
            ),
        )
        .await;
        let pid = fs::read_to_string(fake.path("pid"))
            .ok()
            .and_then(|pid| pid.trim().parse::<u32>().ok())
            .expect("the fake ssh recorded its pid");

        let error = result
            .expect("the pairing deadline must end the operation")
            .expect_err("a never-exiting ssh must fail");
        assert!(
            error.starts_with("[ssh_timeout:pairing] "),
            "unexpected error: {error}"
        );
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(
            !process_is_alive(pid),
            "the SSH child must be terminated and reaped at the deadline"
        );
        assert_eq!(manager.child_reaper.active(), 0);
        manager.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cached_tunnel_bootstrap_never_carries_a_pairing_token() {
        let fake = fake_ssh::FakeSsh::joining();
        let manager = fake.manager(SshOperationDeadlines::default());
        let (endpoint, server) = spawn_ready_endpoint().await;
        publish_fixture_tunnel(&manager, &endpoint, Some("already-exchanged"));
        let app = mock_app();
        let prompts = SshPasswordPromptManager::with_timeout(Duration::ZERO);

        for options in [
            None,
            Some(SshEnvironmentEnsureOptions {
                issue_pairing_token: Some(false),
            }),
        ] {
            let bootstrap = manager
                .ensure_environment(app.handle(), &prompts, fixture_target(), options)
                .await
                .expect("a live tunnel is reused");
            assert_eq!(bootstrap.http_base_url, endpoint);
            assert_eq!(bootstrap.pairing_token, None);
        }
        assert!(
            fake.invocations().is_empty(),
            "reusing a live tunnel without a token must not run SSH: {:?}",
            fake.invocations()
        );
        server.abort();
        manager.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn issue_pairing_token_on_a_live_tunnel_mints_a_fresh_token() {
        let fake = fake_ssh::FakeSsh::joining();
        let manager = fake.manager(SshOperationDeadlines::default());
        let (endpoint, server) = spawn_ready_endpoint().await;
        publish_fixture_tunnel(&manager, &endpoint, Some("already-exchanged"));
        let app = mock_app();
        let prompts = SshPasswordPromptManager::with_timeout(Duration::ZERO);
        let issue = || {
            Some(SshEnvironmentEnsureOptions {
                issue_pairing_token: Some(true),
            })
        };

        let first = manager
            .ensure_environment(app.handle(), &prompts, fixture_target(), issue())
            .await
            .expect("first mint on the live tunnel");
        let second = manager
            .ensure_environment(app.handle(), &prompts, fixture_target(), issue())
            .await
            .expect("second mint on the live tunnel");
        let reused = manager
            .ensure_environment(app.handle(), &prompts, fixture_target(), None)
            .await
            .expect("reuse after minting");

        assert_eq!(first.pairing_token.as_deref(), Some("fixture-credential-1"));
        assert_eq!(
            second.pairing_token.as_deref(),
            Some("fixture-credential-2")
        );
        assert_eq!(first.http_base_url, endpoint);
        assert_eq!(reused.pairing_token, None, "a minted token is never cached");
        assert_eq!(
            fake.invocations(),
            vec![pairing_invocation(), pairing_invocation()],
            "each mint runs one pairing script and nothing else"
        );
        server.abort();
        manager.shutdown().await;
    }

    /// Records each remote script's kind in `kinds.log`. A launch waits for
    /// `$dir/gate`; a stop answers at once; a `-N` tunnel answers every
    /// request on its forwarded local port with `200 {}`.
    #[cfg(unix)]
    const GATED_LAUNCH_AND_SERVING_TUNNEL: &str = concat!(
        r##"if [ -z "$*" ]; then
  printf 'tunnel\n' >>"$dir/kinds.log"
  exec python3 -c 'import http.server, socket, socketserver, sys
def unexpected_reverse_lookup(host):
    raise AssertionError("numeric loopback fixture attempted reverse lookup: " + host)
socket.getfqdn = unexpected_reverse_lookup
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header("content-length", "2")
        self.end_headers()
        self.wfile.write(b"{}")
    def log_message(self, *args):
        pass
socketserver.TCPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()' "${forward%%:*}"
fi
script=$(cat)
case "$script" in
  ""##,
        remote_script_header_prefix!(),
        r##"launch"*)
    printf 'launch\n' >>"$dir/kinds.log"
    while [ ! -e "$dir/gate" ]; do sleep 0.05; done
    printf '{"remotePort":4000,"serverKind":"managed"}\n'
    ;;
  ""##,
        remote_script_header_prefix!(),
        r##"stop"*)
    printf 'stop\n' >>"$dir/kinds.log"
    printf '{"stopped":true}\n'
    ;;
  *)
    printf 'other\n' >>"$dir/kinds.log"
    exit 1
    ;;
esac
"##
    );

    #[cfg(unix)]
    fn cached_http_base_url(manager: &SshEnvironmentManager, key: &str) -> Option<String> {
        manager
            .tunnels
            .lock()
            .expect("tunnels")
            .get(key)
            .map(|tunnel| tunnel.bootstrap.http_base_url.clone())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn concurrent_ensures_on_one_target_launch_once_and_share_the_tunnel() {
        let fake = fake_ssh::FakeSsh::with_body(GATED_LAUNCH_AND_SERVING_TUNNEL);
        let manager = fake.manager(SshOperationDeadlines::default());
        let app = mock_app();
        let prompts = SshPasswordPromptManager::with_timeout(Duration::ZERO);
        let kinds = || {
            fs::read_to_string(fake.path("kinds.log"))
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect::<Vec<_>>()
        };

        // Both callers find no tunnel. The first launch waits at the gate long
        // enough for an unserialised second caller to start its own launch.
        let (first, second, ()) = tokio::join!(
            tokio::time::timeout(
                Duration::from_secs(20),
                manager.ensure_environment(app.handle(), &prompts, fixture_target(), None),
            ),
            tokio::time::timeout(
                Duration::from_secs(20),
                manager.ensure_environment(app.handle(), &prompts, fixture_target(), None),
            ),
            async {
                while !kinds().iter().any(|kind| kind == "launch") {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
                fs::write(fake.path("gate"), "").expect("open the launch gate");
            }
        );

        let first = first.expect("first caller finishes").expect("first tunnel");
        let second = second
            .expect("second caller finishes")
            .expect("second tunnel");
        assert_eq!(
            kinds(),
            ["launch", "tunnel"],
            "one launch, one tunnel, no stop"
        );
        assert_eq!(first.http_base_url, second.http_base_url);
        let key =
            target_connection_key(&normalize_ssh_environment_target(fixture_target()).unwrap());
        assert_eq!(
            cached_http_base_url(&manager, &key),
            Some(first.http_base_url)
        );
        manager.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn disconnect_waits_for_an_in_flight_preparation_of_the_same_target() {
        let fake = fake_ssh::FakeSsh::with_body(GATED_LAUNCH_AND_SERVING_TUNNEL);
        let manager = fake.manager(SshOperationDeadlines::default());
        let app = mock_app();
        let prompts = SshPasswordPromptManager::with_timeout(Duration::ZERO);
        let kinds = || {
            fs::read_to_string(fake.path("kinds.log"))
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect::<Vec<_>>()
        };

        let (prepared, disconnected, ()) = tokio::join!(
            tokio::time::timeout(
                Duration::from_secs(20),
                manager.ensure_environment(app.handle(), &prompts, fixture_target(), None),
            ),
            async {
                while !kinds().iter().any(|kind| kind == "launch") {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                tokio::time::timeout(
                    Duration::from_secs(20),
                    manager.disconnect_environment(app.handle(), &prompts, fixture_target()),
                )
                .await
            },
            async {
                while !kinds().iter().any(|kind| kind == "launch") {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
                fs::write(fake.path("gate"), "").expect("open the launch gate");
            }
        );

        prepared
            .expect("prepare finishes")
            .expect("prepared tunnel");
        disconnected
            .expect("disconnect finishes")
            .expect("disconnected");
        assert_eq!(
            kinds(),
            ["launch", "tunnel", "stop"],
            "the stop runs only after the preparation it would otherwise interleave with"
        );
        let key =
            target_connection_key(&normalize_ssh_environment_target(fixture_target()).unwrap());
        assert_eq!(cached_http_base_url(&manager, &key), None);
        manager.shutdown().await;
    }

    /// The fake ssh exits at once while a background subshell it leaves
    /// behind writes five stdout chunks 700 ms apart, longer in total than
    /// the 2 s idle grace but never idle that long.
    #[cfg(unix)]
    const SLOW_STDOUT_AFTER_EXIT: &str = r#"cat >/dev/null
(
  for chunk in 1 2 3 4 5; do
    sleep 0.7
    printf 'chunk-%s\n' "$chunk"
  done
) &
exit 0
"#;

    #[cfg(unix)]
    #[tokio::test]
    async fn output_still_arriving_after_ssh_exits_is_kept() {
        let fake = fake_ssh::FakeSsh::with_body(SLOW_STDOUT_AFTER_EXIT);
        let manager = fake.manager(SshOperationDeadlines::default());
        let launcher = manager.askpass_launcher().expect("askpass launcher");

        let stdout = tokio::time::timeout(
            Duration::from_secs(20),
            run_remote_ssh_script(
                &manager
                    .remote_runner(RemoteOperation::Pairing)
                    .expect("SSH runner"),
                &fixture_target(),
                "exit 0\n",
                &[],
                &SshAuthOptions::batch(),
                launcher,
                "pairing",
            ),
        )
        .await
        .expect("the drain is bounded")
        .expect("the script succeeds");

        assert_eq!(
            stdout.lines().collect::<Vec<_>>(),
            ["chunk-1", "chunk-2", "chunk-3", "chunk-4", "chunk-5"],
            "every chunk written after ssh exited must be kept"
        );
        manager.shutdown().await;
    }

    /// `ps -o stat=` for `pid` (Linux and macOS); empty when there is no
    /// such process.
    #[cfg(unix)]
    fn fixture_process_state(pid: u32) -> String {
        std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
            .unwrap_or_default()
    }

    /// Whether a fixture process that is not this test's child has ended. A
    /// zombie counts as ended: an orphan is reaped by PID 1 or a subreaper,
    /// which some environments lack. A live process never counts as ended.
    #[cfg(unix)]
    fn fixture_process_is_gone(pid: u32) -> bool {
        let state = fixture_process_state(pid);
        state.is_empty() || state.starts_with('Z') || state.starts_with('X')
    }

    #[cfg(unix)]
    async fn wait_until_fixture_gone(pid: u32) -> bool {
        for _ in 0..60 {
            if fixture_process_is_gone(pid) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        false
    }

    #[cfg(unix)]
    #[test]
    fn fixture_liveness_counts_a_zombie_as_gone_but_not_a_live_process() {
        // An unreaped child is a zombie: `kill(pid, 0)` still succeeds, which
        // is why orphaned fixtures cannot be checked with it where no PID 1
        // reaps them.
        let mut exited = std::process::Command::new("sh")
            .args(["-c", "exit 0"])
            .spawn()
            .expect("short-lived child");
        let zombie = exited.id();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !fixture_process_state(zombie).starts_with('Z') {
            assert!(
                std::time::Instant::now() < deadline,
                "child never became a zombie"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            process_is_alive(zombie),
            "kill(pid, 0) succeeds for a zombie"
        );
        assert!(fixture_process_is_gone(zombie), "a zombie is gone");
        exited.wait().expect("reap the zombie");
        assert!(fixture_process_is_gone(zombie), "a reaped process is gone");

        let mut sleeping = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("live child");
        let live = sleeping.id();
        assert!(!fixture_process_is_gone(live), "a live process is not gone");
        let _ = sleeping.kill();
        sleeping.wait().expect("reap the live child");
    }

    /// A fake remote step that fails after leaving a backgrounded `sleep`
    /// holding its inherited output pipes, as a ProxyCommand helper or an
    /// older ControlPersist master can. `@@FAILING@@` selects the step
    /// (`tunnel` or `launch`); the other steps succeed.
    #[cfg(unix)]
    const FAILS_WITH_HELD_PIPES: &str = concat!(
        r##"hold_pipes_and_fail() {
  # The helper leads its own process group so cleanup can kill all of it.
  python3 -c 'import os; os.setpgid(0, 0); os.execvp("sleep", ["sleep", "600"])' &
  printf '%s\n' "$!" >>"$dir/background.pids"
  printf 'fake ssh: %s failed while a helper kept stderr open\n' "$1" >&2
  exit 255
}
if [ -z "$*" ]; then
  [ '@@FAILING@@' = tunnel ] && hold_pipes_and_fail tunnel
  exec sleep 60
fi
script=$(cat)
case "$script" in
  ""##,
        remote_script_header_prefix!(),
        r##"launch"*)
    [ '@@FAILING@@' = launch ] && hold_pipes_and_fail launch
    printf '{"remotePort":4000,"serverKind":"managed"}\n'
    ;;
  ""##,
        remote_script_header_prefix!(),
        r##"stop"*) printf '{"stopped":true}\n' ;;
  *) exit 1 ;;
esac
"##
    );

    /// Kills the backgrounded fixture processes a fake recorded in
    /// `background.pids`, even when the test panics. Only a pid whose command
    /// line still contains `marker` is signalled, so a recycled pid is safe;
    /// when it leads its own process group, the whole group is killed.
    #[cfg(unix)]
    struct BackgroundedFixtureProcesses {
        pids_file: PathBuf,
        /// Text the recorded process's command line must still contain.
        marker: &'static str,
    }

    #[cfg(unix)]
    impl BackgroundedFixtureProcesses {
        fn pids(&self) -> Vec<u32> {
            fs::read_to_string(&self.pids_file)
                .unwrap_or_default()
                .lines()
                .filter_map(|pid| pid.trim().parse::<u32>().ok())
                .collect()
        }
    }

    #[cfg(unix)]
    impl Drop for BackgroundedFixtureProcesses {
        fn drop(&mut self) {
            for pid in self.pids() {
                let command = std::process::Command::new("ps")
                    .args(["-o", "command=", "-p", &pid.to_string()])
                    .output()
                    .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
                    .unwrap_or_default();
                if !command.contains(self.marker) {
                    continue;
                }
                let group = std::process::Command::new("ps")
                    .args(["-o", "pgid=", "-p", &pid.to_string()])
                    .output()
                    .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
                    .unwrap_or_default();
                let target = if group == pid.to_string() {
                    -(pid as libc::pid_t)
                } else {
                    pid as libc::pid_t
                };
                // SAFETY: signals only the fixture's own recorded process, or
                // the process group it leads.
                unsafe {
                    libc::kill(target, libc::SIGKILL);
                }
            }
        }
    }

    /// The fake ssh exits at once, leaving a writer in its own process group
    /// that prints to the inherited stdout every 200 ms, so the post-exit
    /// drain never goes idle.
    #[cfg(unix)]
    const ENDLESS_STDOUT_AFTER_EXIT: &str = r#"cat >/dev/null
python3 -c 'import os, sys, time
os.setpgid(0, 0)
while True:
    sys.stdout.write("tick\n")
    sys.stdout.flush()
    time.sleep(0.2)' fixture-writer &
printf '%s\n' "$!" >>"$dir/background.pids"
exit 0
"#;

    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_interrupts_a_post_exit_drain_that_never_goes_idle() {
        let fake = fake_ssh::FakeSsh::with_body(ENDLESS_STDOUT_AFTER_EXIT);
        let writer = BackgroundedFixtureProcesses {
            pids_file: fake.path("background.pids"),
            marker: "fixture-writer",
        };
        let manager = fake.manager(SshOperationDeadlines::default());
        let runner = manager
            .remote_runner(RemoteOperation::Launch)
            .expect("SSH runner");
        let launcher = manager.askpass_launcher().expect("askpass launcher");
        let operation = tokio::spawn(async move {
            run_remote_ssh_script(
                &runner,
                &fixture_target(),
                "exit 0\n",
                &[],
                &SshAuthOptions::batch(),
                launcher,
                "launch",
            )
            .await
        });
        while writer.pids().is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // The fake has exited; the drain is now following the writer.
        tokio::time::sleep(Duration::from_millis(500)).await;

        let shutdown = tokio::time::timeout(Duration::from_secs(2), manager.shutdown()).await;
        let result = tokio::time::timeout(Duration::from_secs(2), operation).await;

        assert!(
            shutdown.is_ok(),
            "shutdown must not wait for the drain to reach the launch deadline"
        );
        let error = result
            .expect("the operation ends with the shutdown")
            .expect("operation task")
            .expect_err("a drain cut short by shutdown fails");
        assert!(
            error.contains("SSH process owner is shutting down"),
            "{error}"
        );
        let pids = writer.pids();
        drop(writer);
        for pid in pids {
            assert!(
                wait_until_fixture_gone(pid).await,
                "writer {pid} must be gone"
            );
        }
    }

    /// Runs `ensure_environment` twice on one target whose `failing` step
    /// leaves its pipes held open, and checks that each call fails fast with
    /// the step's own stderr, so the target lock is never held indefinitely.
    #[cfg(unix)]
    async fn assert_held_pipes_fail_fast(failing: &str) {
        let fake =
            fake_ssh::FakeSsh::with_body(&FAILS_WITH_HELD_PIPES.replace("@@FAILING@@", failing));
        let background = BackgroundedFixtureProcesses {
            pids_file: fake.path("background.pids"),
            marker: "sleep 600",
        };
        let manager = fake.manager(SshOperationDeadlines::default());
        let app = mock_app();
        let prompts = SshPasswordPromptManager::with_timeout(Duration::ZERO);

        for attempt in ["first", "second"] {
            let started = std::time::Instant::now();
            let result = tokio::time::timeout(
                Duration::from_secs(10),
                manager.ensure_environment(app.handle(), &prompts, fixture_target(), None),
            )
            .await;
            let elapsed = started.elapsed();
            let error = result
                .unwrap_or_else(|_| {
                    panic!("{attempt} {failing} failure must not wait for the held pipes to close")
                })
                .expect_err("the step fails");
            assert!(
                error.contains(&format!(
                    "fake ssh: {failing} failed while a helper kept stderr open"
                )),
                "{attempt}: {error}"
            );
            assert!(error.contains("[output cut off]"), "{attempt}: {error}");
            assert!(
                elapsed < Duration::from_secs(6),
                "{attempt} took {elapsed:?}"
            );
        }

        assert_eq!(
            background.pids().len(),
            2,
            "each attempt left one held pipe"
        );
        let pids = background.pids();
        drop(background);
        for pid in pids {
            assert!(
                wait_until_fixture_gone(pid).await,
                "backgrounded sleep {pid} must be gone"
            );
        }
        manager.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exited_tunnel_with_held_stderr_fails_fast_and_releases_the_target() {
        assert_held_pipes_fail_fast("tunnel").await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exited_remote_script_with_held_pipes_fails_fast_and_releases_the_target() {
        assert_held_pipes_fail_fast("launch").await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dead_tunnel_endpoint_drops_the_cached_tunnel() {
        let fake = fake_ssh::FakeSsh::with_body(
            "printf 'fake ssh: connection refused\\n' >&2\nexit 255\n",
        );
        let manager = fake.manager(SshOperationDeadlines::default());
        let closed_port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .expect("reserve a closed port")
            .port();
        let (key, tunnel_pid) =
            publish_fixture_tunnel(&manager, &format!("http://127.0.0.1:{closed_port}/"), None);
        let app = mock_app();
        let prompts = SshPasswordPromptManager::with_timeout(Duration::ZERO);

        let error = manager
            .ensure_environment(app.handle(), &prompts, fixture_target(), None)
            .await
            .expect_err("a dead endpoint must not be reused; the full path then fails here");

        assert!(error.contains("SSH launch command failed"), "{error}");
        assert_eq!(
            manager
                .take_existing_bootstrap_if_running(&key)
                .expect("inspect tunnels"),
            None
        );
        assert!(
            wait_until_exited(tunnel_pid).await,
            "the dropped tunnel child must be reaped"
        );
        assert_eq!(fake.invocations().len(), 1, "{:?}", fake.invocations());
        manager.shutdown().await;
    }

    /// A `-N -L` fake that records its forward spec in `forwards.log` and then
    /// accepts TCP connections on the forward's local port.
    #[cfg(unix)]
    const LISTENING_PORT_FORWARD: &str = r##"if [ -z "$*" ]; then
  case "$forward" in *:9999) printf 'refused\n' >&2; exit 255 ;; esac
  printf '%s\n' "$forward" >>"$dir/forwards.log"
  local_port=${forward#127.0.0.1:}
  exec python3 -c 'import socket, sys
s = socket.socket()
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", int(sys.argv[1])))
s.listen(16)
while True:
    s.accept()[0].close()' "${local_port%%:*}"
fi
exit 1
"##;

    #[cfg(unix)]
    fn forward_pid(manager: &SshEnvironmentManager, key: &str, remote_port: u16) -> Option<u32> {
        manager
            .tunnels
            .lock()
            .expect("tunnels")
            .get(key)?
            .forwards
            .get(&remote_port)?
            .child
            .child
            .as_ref()
            .and_then(Child::id)
    }

    #[cfg(unix)]
    fn forward_count(manager: &SshEnvironmentManager, key: &str) -> usize {
        manager
            .tunnels
            .lock()
            .expect("tunnels")
            .get(key)
            .map_or(0, |tunnel| tunnel.forwards.len())
    }

    #[cfg(unix)]
    fn recorded_forwards(fake: &fake_ssh::FakeSsh) -> Vec<String> {
        fs::read_to_string(fake.path("forwards.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Kills the tunnel's child, or one of its forwards, and waits until the
    /// manager's handle observes the exit (the zombie stays for the manager
    /// to reap).
    #[cfg(unix)]
    async fn kill_managed_child(manager: &SshEnvironmentManager, key: &str, forward: Option<u16>) {
        let exited = || {
            let mut tunnels = manager.tunnels.lock().expect("tunnels");
            let tunnel = tunnels.get_mut(key).expect("tunnel");
            let child = match forward {
                Some(port) => &mut tunnel.forwards.get_mut(&port).expect("forward").child,
                None => &mut tunnel.child,
            };
            let pid = child.child_mut().id().map(|pid| pid as libc::pid_t);
            if let Some(pid) = pid {
                // SAFETY: the child is owned by this test's manager.
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
            matches!(child.child_mut().try_wait(), Ok(Some(_)))
        };
        for _ in 0..60 {
            if exited() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("killed SSH child did not exit");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn port_forward_requires_live_tunnel() {
        let fake = fake_ssh::FakeSsh::with_body(LISTENING_PORT_FORWARD);
        let manager = fake.manager(SshOperationDeadlines::default());
        let app = mock_app();
        let prompts = SshPasswordPromptManager::with_timeout(Duration::ZERO);

        let error = manager
            .ensure_port_forward(app.handle(), &prompts, fixture_target(), 5173, None)
            .await
            .expect_err("no tunnel, no forward");

        assert_eq!(error, "SSH connection is not active.");
        assert!(recorded_forwards(&fake).is_empty());
        assert_eq!(manager.child_reaper.active(), 0);

        // A tunnel whose ssh exited is not live either, and takes its
        // forwards with it.
        let (key, _) = publish_fixture_tunnel(&manager, "http://127.0.0.1:9/", None);
        manager
            .ensure_port_forward(app.handle(), &prompts, fixture_target(), 5173, None)
            .await
            .expect("forward over the live tunnel");
        let forward = forward_pid(&manager, &key, 5173).expect("forward pid");
        kill_managed_child(&manager, &key, None).await;

        let error = manager
            .ensure_port_forward(app.handle(), &prompts, fixture_target(), 5174, None)
            .await
            .expect_err("a dead tunnel carries no forward");
        assert_eq!(error, "SSH connection is not active.");
        assert!(
            wait_until_exited(forward).await,
            "the dead tunnel's forward must be reaped"
        );
        manager.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn port_forward_is_idempotent_per_remote_port() {
        let fake = fake_ssh::FakeSsh::with_body(LISTENING_PORT_FORWARD);
        let manager = fake.manager(SshOperationDeadlines::default());
        let app = mock_app();
        let prompts = SshPasswordPromptManager::with_timeout(Duration::ZERO);
        let (key, _) = publish_fixture_tunnel(&manager, "http://127.0.0.1:9/", None);
        let ensure = |remote_port| {
            manager.ensure_port_forward(app.handle(), &prompts, fixture_target(), remote_port, None)
        };

        let first = ensure(5173).await.expect("first forward");
        let again = ensure(5173).await.expect("same forward");
        let other = ensure(8080).await.expect("other forward");
        assert_eq!(first, again);
        assert_ne!(first, other);
        assert_eq!(
            recorded_forwards(&fake).len(),
            2,
            "a live forward is reused"
        );

        let dead = forward_pid(&manager, &key, 5173).expect("forward pid");
        kill_managed_child(&manager, &key, Some(5173)).await;
        let replaced = ensure(5173).await.expect("a dead forward is replaced");
        let replacement = forward_pid(&manager, &key, 5173).expect("replacement pid");

        assert_ne!(replacement, dead);
        assert_eq!(recorded_forwards(&fake).len(), 3);
        assert_eq!(
            recorded_forwards(&fake).last(),
            Some(&format!("127.0.0.1:{replaced}:127.0.0.1:5173"))
        );
        assert_eq!(forward_count(&manager, &key), 2);
        // The tunnel and two live forwards; the dead one was reaped.
        assert_eq!(manager.child_reaper.active(), 3);
        manager.shutdown().await;
    }

    #[test]
    fn port_forward_args_forward_to_loopback_remote_port() {
        let target = SshEnvironmentTarget {
            alias: "devbox".to_string(),
            hostname: "devbox.internal".to_string(),
            username: Some("alice".to_string()),
            port: Some(2222),
        };
        let manager = SshEnvironmentManager::with_ssh_program(
            "fixture-ssh",
            SshOperationDeadlines::default(),
        );

        let plan = manager
            .port_forward_plan(
                &target,
                ForwardBind {
                    port: 45123,
                    ipv6: false,
                },
                5173,
                &SshAuthOptions::batch(),
            )
            .expect("port forward plan");

        assert_eq!(plan.program, "fixture-ssh");
        assert_eq!(
            plan.args,
            vec![
                "-o",
                "ControlPath=none",
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                "-p",
                "2222",
                "-o",
                "ExitOnForwardFailure=yes",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=3",
                "-n",
                "-N",
                "-L",
                "127.0.0.1:45123:127.0.0.1:5173",
                "alice@devbox",
            ]
        );
    }

    #[test]
    fn preferred_port_forward_binds_both_loopback_families() {
        let manager = SshEnvironmentManager::with_ssh_program(
            "fixture-ssh",
            SshOperationDeadlines::default(),
        );
        let forwards = |ipv6| {
            let plan = manager
                .port_forward_plan(
                    &fixture_target(),
                    ForwardBind { port: 5173, ipv6 },
                    41000,
                    &SshAuthOptions::batch(),
                )
                .expect("plan");
            plan.args
                .windows(2)
                .filter(|pair| pair[0] == "-L")
                .map(|pair| pair[1].clone())
                .collect::<Vec<_>>()
        };
        // IPv4 last: the fake ssh records and binds the last -L.
        assert_eq!(
            forwards(true),
            [
                "[::1]:5173:127.0.0.1:41000",
                "127.0.0.1:5173:127.0.0.1:41000"
            ]
        );
        assert_eq!(
            forwards(false),
            ["127.0.0.1:5173:127.0.0.1:41000"],
            "no IPv6 loopback, one IPv4 bind"
        );
    }

    #[test]
    fn preferred_port_probe() {
        assert_eq!(
            probe_preferred_port(80),
            None,
            "below 1024 is never preferred"
        );
        let free = portpicker::pick_unused_port().expect("free port");
        assert_eq!(probe_preferred_port(free).map(|bind| bind.port), Some(free));
        let v4 = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("v4");
        assert_eq!(probe_preferred_port(v4.local_addr().unwrap().port()), None);
    }

    #[test]
    fn preferred_port_busy_on_ipv6_falls_back() {
        let Ok(v6) = std::net::TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, 0)) else {
            return; // no IPv6 loopback on this host
        };
        let port = v6.local_addr().unwrap().port();
        if std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).is_err() {
            return; // the IPv4 side happens to be taken too; nothing to assert
        }
        assert_eq!(probe_preferred_port(port), None);
    }

    #[test]
    fn forward_bind_failures_are_recognised() {
        assert!(is_forward_bind_failure(
            "SSH port forward exited before becoming ready with status exit status: 255: bind [127.0.0.1]:5173: Address already in use"
        ));
        assert!(is_forward_bind_failure(
            "Could not request local forwarding."
        ));
        assert!(!is_forward_bind_failure(
            "Permission denied (publickey,password)."
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn preferred_port_forward_uses_the_canonical_local_port() {
        let fake = fake_ssh::FakeSsh::with_body(LISTENING_PORT_FORWARD);
        let manager = fake.manager(SshOperationDeadlines::default());
        let app = mock_app();
        let prompts = SshPasswordPromptManager::with_timeout(Duration::ZERO);
        publish_fixture_tunnel(&manager, "http://127.0.0.1:9/", None);
        let preferred = portpicker::pick_unused_port().expect("free port");

        let local_port = manager
            .ensure_port_forward(
                app.handle(),
                &prompts,
                fixture_target(),
                41000,
                Some(preferred),
            )
            .await
            .expect("forward");

        assert_eq!(local_port, preferred);
        assert_eq!(
            recorded_forwards(&fake),
            vec![format!("127.0.0.1:{preferred}:127.0.0.1:41000")]
        );
        manager.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn busy_preferred_port_falls_back_to_a_random_port() {
        let fake = fake_ssh::FakeSsh::with_body(LISTENING_PORT_FORWARD);
        let manager = fake.manager(SshOperationDeadlines::default());
        let app = mock_app();
        let prompts = SshPasswordPromptManager::with_timeout(Duration::ZERO);
        publish_fixture_tunnel(&manager, "http://127.0.0.1:9/", None);
        let busy = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("busy");
        let busy_port = busy.local_addr().unwrap().port();

        let local_port = manager
            .ensure_port_forward(
                app.handle(),
                &prompts,
                fixture_target(),
                41000,
                Some(busy_port),
            )
            .await
            .expect("forward");

        assert_ne!(local_port, busy_port);
        assert_eq!(
            recorded_forwards(&fake),
            vec![format!("127.0.0.1:{local_port}:127.0.0.1:41000")]
        );
        manager.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn preferred_port_lost_to_a_race_retries_on_a_random_port() {
        // The fake refuses the first forward with ssh's bind error, then listens.
        let fake = fake_ssh::FakeSsh::with_body(&format!(
            r#"if [ -z "$*" ] && [ ! -e "$dir/raced" ]; then
  : >"$dir/raced"
  printf 'bind [127.0.0.1]:%s: Address already in use\n' "$forward" >&2
  exit 255
fi
{LISTENING_PORT_FORWARD}"#
        ));
        let manager = fake.manager(SshOperationDeadlines::default());
        let app = mock_app();
        let prompts = SshPasswordPromptManager::with_timeout(Duration::ZERO);
        publish_fixture_tunnel(&manager, "http://127.0.0.1:9/", None);
        let preferred = portpicker::pick_unused_port().expect("free port");

        let local_port = manager
            .ensure_port_forward(
                app.handle(),
                &prompts,
                fixture_target(),
                41000,
                Some(preferred),
            )
            .await
            .expect("the race falls back instead of failing");

        assert_ne!(local_port, preferred);
        assert_eq!(
            recorded_forwards(&fake),
            vec![format!("127.0.0.1:{local_port}:127.0.0.1:41000")]
        );
        manager.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn auth_failure_is_not_retried_on_a_random_port() {
        let fake = fake_ssh::FakeSsh::with_body(
            "printf 'Permission denied (publickey).\\n' >&2\nprintf x >>\"$dir/attempts\"\nexit 255\n",
        );
        let manager = fake.manager(SshOperationDeadlines::default());
        let app = mock_app();
        let prompts = SshPasswordPromptManager::with_timeout(Duration::ZERO);
        publish_fixture_tunnel(&manager, "http://127.0.0.1:9/", None);
        let preferred = portpicker::pick_unused_port().expect("free port");

        manager
            .ensure_port_forward(
                app.handle(),
                &prompts,
                fixture_target(),
                41000,
                Some(preferred),
            )
            .await
            .expect_err("auth failure");

        let attempts = fs::read_to_string(fake.path("attempts")).unwrap_or_default();
        assert_eq!(attempts.len(), 1, "a non-bind failure is not retried");
        manager.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn preferred_port_taken_before_an_attempt_falls_back() {
        // Each authentication attempt checks the port again: one taken while a
        // password prompt was open must not be handed to ssh (whose readiness
        // probe could then reach the other listener).
        let fake = fake_ssh::FakeSsh::with_body(LISTENING_PORT_FORWARD);
        let manager = fake.manager(SshOperationDeadlines::default());
        let app = mock_app();
        let prompts = SshPasswordPromptManager::with_timeout(Duration::ZERO);
        let (key, _) = publish_fixture_tunnel(&manager, "http://127.0.0.1:9/", None);
        let taken = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("taken");
        let taken_port = taken.local_addr().unwrap().port();

        let (local_port, mut child) = manager
            .spawn_port_forward(
                app.handle(),
                &prompts,
                &key,
                &fixture_target(),
                41000,
                Some(taken_port),
            )
            .await
            .expect("forward on another port");

        assert_ne!(local_port, taken_port);
        child.terminate_and_reap().await;
        manager.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn live_forward_is_reused_whatever_its_local_port() {
        let fake = fake_ssh::FakeSsh::with_body(LISTENING_PORT_FORWARD);
        let manager = fake.manager(SshOperationDeadlines::default());
        let app = mock_app();
        let prompts = SshPasswordPromptManager::with_timeout(Duration::ZERO);
        publish_fixture_tunnel(&manager, "http://127.0.0.1:9/", None);
        let busy = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("busy");
        let preferred = busy.local_addr().unwrap().port();
        let ensure = || {
            manager.ensure_port_forward(
                app.handle(),
                &prompts,
                fixture_target(),
                41000,
                Some(preferred),
            )
        };

        let fallback = ensure().await.expect("fallback");
        drop(busy); // the preferred port frees up
        let again = ensure().await.expect("reuse");

        assert_eq!(again, fallback, "an open tab keeps its origin");
        assert_eq!(recorded_forwards(&fake).len(), 1);
        manager.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn port_forward_spawns_ssh_forwarding_to_loopback_remote_port() {
        let fake = fake_ssh::FakeSsh::with_body(LISTENING_PORT_FORWARD);
        let manager = fake.manager(SshOperationDeadlines::default());
        let app = mock_app();
        let prompts = SshPasswordPromptManager::with_timeout(Duration::ZERO);
        publish_fixture_tunnel(&manager, "http://127.0.0.1:9/", None);

        let local_port = manager
            .ensure_port_forward(app.handle(), &prompts, fixture_target(), 5173, None)
            .await
            .expect("forward");

        assert_eq!(
            recorded_forwards(&fake),
            vec![format!("127.0.0.1:{local_port}:127.0.0.1:5173")]
        );
        tokio::net::TcpStream::connect(("127.0.0.1", local_port))
            .await
            .expect("the returned local port accepts connections");
        manager.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn port_forward_stderr_never_fills_up() {
        // After the readiness probe the fake writes far more than a pipe
        // holds to stderr, as ssh does for many refused channels, and only
        // then marks that it went on.
        let fake = fake_ssh::FakeSsh::with_body(
            r##"if [ -z "$*" ]; then
  local_port=${forward#127.0.0.1:}
  exec python3 -c 'import socket, sys
s = socket.socket()
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", int(sys.argv[1])))
s.listen(16)
s.accept()[0].close()
sys.stderr.write("channel 3: open failed: connect failed: Connection refused\n" * 8192)
sys.stderr.flush()
open(sys.argv[2], "w").close()
while True:
    s.accept()[0].close()' "${local_port%%:*}" "$dir/flooded"
fi
exit 1
"##,
        );
        let manager = fake.manager(SshOperationDeadlines::default());
        let app = mock_app();
        let prompts = SshPasswordPromptManager::with_timeout(Duration::ZERO);
        publish_fixture_tunnel(&manager, "http://127.0.0.1:9/", None);

        manager
            .ensure_port_forward(app.handle(), &prompts, fixture_target(), 5173, None)
            .await
            .expect("forward");

        let flooded = fake.path("flooded");
        let mut drained = false;
        for _ in 0..100 {
            if flooded.exists() {
                drained = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(drained, "a forward's stderr must be drained");
        manager.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn port_forward_that_exits_before_ready_reports_its_error() {
        let fake = fake_ssh::FakeSsh::with_body(
            "printf 'bind [127.0.0.1]: Address already in use\\n' >&2\nexit 255\n",
        );
        let manager = fake.manager(SshOperationDeadlines::default());
        let app = mock_app();
        let prompts = SshPasswordPromptManager::with_timeout(Duration::ZERO);
        let (key, _) = publish_fixture_tunnel(&manager, "http://127.0.0.1:9/", None);

        let error = manager
            .ensure_port_forward(app.handle(), &prompts, fixture_target(), 5173, None)
            .await
            .expect_err("an exited forward is not ready");

        assert!(error.contains("Address already in use"), "{error}");
        assert_eq!(forward_count(&manager, &key), 0);
        assert_eq!(manager.child_reaper.active(), 1, "only the tunnel remains");
        manager.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn forwards_are_reaped_with_the_tunnel() {
        let fake = fake_ssh::FakeSsh::with_body(LISTENING_PORT_FORWARD);
        let manager = fake.manager(SshOperationDeadlines::default());
        let app = mock_app();
        let prompts = SshPasswordPromptManager::with_timeout(Duration::ZERO);

        // Dropping a cached tunnel reaps its forwards.
        let (key, tunnel) = publish_fixture_tunnel(&manager, "http://127.0.0.1:9/", None);
        manager
            .ensure_port_forward(app.handle(), &prompts, fixture_target(), 5173, None)
            .await
            .expect("forward");
        let forward = forward_pid(&manager, &key, 5173).expect("forward pid");
        manager.drop_cached_tunnel(&key).await;
        assert!(!process_is_alive(tunnel));
        assert!(!process_is_alive(forward), "drop must reap the forward");
        assert_eq!(manager.child_reaper.active(), 0);

        // Shutdown reaps them too.
        publish_fixture_tunnel(&manager, "http://127.0.0.1:9/", None);
        manager
            .ensure_port_forward(app.handle(), &prompts, fixture_target(), 5173, None)
            .await
            .expect("forward");
        let forward = forward_pid(&manager, &key, 5173).expect("forward pid");
        manager.shutdown().await;
        assert!(!process_is_alive(forward), "shutdown must reap the forward");
        assert_eq!(manager.child_reaper.active(), 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn release_reaps_one_forward() {
        let fake = fake_ssh::FakeSsh::with_body(LISTENING_PORT_FORWARD);
        let manager = fake.manager(SshOperationDeadlines::default());
        let app = mock_app();
        let prompts = SshPasswordPromptManager::with_timeout(Duration::ZERO);
        let (key, tunnel) = publish_fixture_tunnel(&manager, "http://127.0.0.1:9/", None);
        for remote_port in [5173, 8080] {
            manager
                .ensure_port_forward(app.handle(), &prompts, fixture_target(), remote_port, None)
                .await
                .expect("forward");
        }
        let released = forward_pid(&manager, &key, 5173).expect("forward pid");
        let kept = forward_pid(&manager, &key, 8080).expect("forward pid");

        manager
            .release_port_forward(fixture_target(), 5173)
            .await
            .expect("release");

        assert!(!process_is_alive(released), "release must reap the forward");
        assert!(process_is_alive(kept));
        assert!(process_is_alive(tunnel));
        assert_eq!(forward_count(&manager, &key), 1);
        assert_eq!(manager.child_reaper.active(), 2);
        manager
            .release_port_forward(fixture_target(), 5173)
            .await
            .expect("releasing a released forward is a no-op");
        manager.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ninth_forward_evicts_least_recent() {
        let fake = fake_ssh::FakeSsh::with_body(LISTENING_PORT_FORWARD);
        let manager = fake.manager(SshOperationDeadlines::default());
        let app = mock_app();
        let prompts = SshPasswordPromptManager::with_timeout(Duration::ZERO);
        let (key, _) = publish_fixture_tunnel(&manager, "http://127.0.0.1:9/", None);
        let ensure = |remote_port| {
            manager.ensure_port_forward(app.handle(), &prompts, fixture_target(), remote_port, None)
        };
        for remote_port in 5001..=5008 {
            ensure(remote_port).await.expect("forward");
        }
        // Requesting 5001 again makes 5002 the least recently requested.
        ensure(5001).await.expect("reused forward");
        let evicted = forward_pid(&manager, &key, 5002).expect("forward pid");

        ensure(5009).await.expect("ninth forward");

        assert!(!process_is_alive(evicted), "eviction must reap the forward");
        assert_eq!(forward_pid(&manager, &key, 5002), None);
        assert!(forward_pid(&manager, &key, 5001).is_some());
        assert!(forward_pid(&manager, &key, 5009).is_some());
        assert_eq!(forward_count(&manager, &key), 8);
        assert_eq!(manager.child_reaper.active(), 9);
        manager.shutdown().await;
    }

    #[cfg(unix)]
    fn second_target() -> SshEnvironmentTarget {
        SshEnvironmentTarget {
            alias: "second-host".to_string(),
            hostname: "second-host".to_string(),
            username: None,
            port: None,
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn global_forward_budget_evicts_least_recent_across_tunnels() {
        let fake = fake_ssh::FakeSsh::with_body(LISTENING_PORT_FORWARD);
        let manager = fake.manager(SshOperationDeadlines::default());
        let app = mock_app();
        let prompts = SshPasswordPromptManager::with_timeout(Duration::ZERO);
        let (first, _) = publish_fixture_tunnel(&manager, "http://127.0.0.1:9/", None);
        let (second, _) =
            publish_tunnel_for(&manager, second_target(), "http://127.0.0.1:9/", None);
        let per_second = SSH_PORT_FORWARD_BUDGET - SSH_MAX_PORT_FORWARDS_PER_TUNNEL;
        for remote_port in 5001..5001 + SSH_MAX_PORT_FORWARDS_PER_TUNNEL as u16 {
            manager
                .ensure_port_forward(app.handle(), &prompts, fixture_target(), remote_port, None)
                .await
                .expect("first tunnel forward");
        }
        for remote_port in 6001..6001 + per_second as u16 {
            manager
                .ensure_port_forward(app.handle(), &prompts, second_target(), remote_port, None)
                .await
                .expect("second tunnel forward");
        }
        // Requesting 5001 again makes 5002 the least recently used anywhere.
        manager
            .ensure_port_forward(app.handle(), &prompts, fixture_target(), 5001, None)
            .await
            .expect("reused forward");
        let evicted = forward_pid(&manager, &first, 5002).expect("forward pid");

        manager
            .ensure_port_forward(app.handle(), &prompts, second_target(), 7001, None)
            .await
            .expect("forward over budget");

        assert!(!process_is_alive(evicted), "budget eviction must reap");
        assert_eq!(forward_pid(&manager, &first, 5002), None);
        assert!(forward_pid(&manager, &first, 5001).is_some());
        assert!(forward_pid(&manager, &second, 7001).is_some());
        assert_eq!(
            forward_count(&manager, &first) + forward_count(&manager, &second),
            SSH_PORT_FORWARD_BUDGET
        );
        assert_eq!(manager.child_reaper.active(), 2 + SSH_PORT_FORWARD_BUDGET);
        manager.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_forward_over_the_limit_keeps_the_least_recent() {
        let fake = fake_ssh::FakeSsh::with_body(LISTENING_PORT_FORWARD);
        let manager = fake.manager(SshOperationDeadlines::default());
        let app = mock_app();
        let prompts = SshPasswordPromptManager::with_timeout(Duration::ZERO);
        let (key, _) = publish_fixture_tunnel(&manager, "http://127.0.0.1:9/", None);
        for remote_port in 5001..5001 + SSH_MAX_PORT_FORWARDS_PER_TUNNEL as u16 {
            manager
                .ensure_port_forward(app.handle(), &prompts, fixture_target(), remote_port, None)
                .await
                .expect("forward");
        }
        let least_recent = forward_pid(&manager, &key, 5001).expect("forward pid");

        manager
            .ensure_port_forward(app.handle(), &prompts, fixture_target(), 9999, None)
            .await
            .expect_err("the fake refuses 9999");

        assert!(
            process_is_alive(least_recent),
            "a failed replacement evicts nothing"
        );
        assert_eq!(
            forward_count(&manager, &key),
            SSH_MAX_PORT_FORWARDS_PER_TUNNEL
        );
        assert_eq!(
            manager.child_reaper.active(),
            1 + SSH_MAX_PORT_FORWARDS_PER_TUNNEL
        );
        manager.shutdown().await;
    }

    #[test]
    fn serializes_external_ssh_bootstrap_shape() {
        let target = SshEnvironmentTarget {
            alias: "devbox".to_string(),
            hostname: "devbox.internal".to_string(),
            username: None,
            port: None,
        };
        let bootstrap = SshEnvironmentBootstrap::external(
            target.clone(),
            3773,
            "http://127.0.0.1:45123/".to_string(),
            "ws://127.0.0.1:45123/".to_string(),
            Some("pairing-token".to_string()),
        );

        assert_eq!(
            serde_json::to_value(&bootstrap).expect("bootstrap should serialize"),
            json!({
                "target": {
                    "alias": "devbox",
                    "hostname": "devbox.internal",
                    "username": null,
                    "port": null,
                },
                "httpBaseUrl": "http://127.0.0.1:45123/",
                "wsBaseUrl": "ws://127.0.0.1:45123/",
                "pairingToken": "pairing-token",
                "remotePort": 3773,
                "remoteServerKind": "external",
            })
        );
    }

    #[test]
    fn parses_remote_launch_json_from_last_non_empty_line() {
        assert_eq!(
            parse_remote_launch_result(
                "banner\n{\"remotePort\":4111,\"serverKind\":\"managed\"}\n"
            )
            .expect("launch result should parse"),
            RemoteLaunchResult {
                remote_port: 4111,
                server_kind: "managed".to_string(),
            }
        );
        assert!(parse_remote_launch_result("{\"remotePort\":0}\n").is_err());
        assert!(
            parse_remote_launch_result("{\"remotePort\":3773,\"serverKind\":\"bogus\"}\n").is_err()
        );
    }

    #[test]
    fn serializes_managed_ssh_bootstrap_shape() {
        let target = SshEnvironmentTarget {
            alias: "devbox".to_string(),
            hostname: "devbox.internal".to_string(),
            username: None,
            port: None,
        };
        let bootstrap = SshEnvironmentBootstrap::new(
            target.clone(),
            4111,
            "http://127.0.0.1:45123/".to_string(),
            "ws://127.0.0.1:45123/".to_string(),
            None,
            "managed",
        );

        assert_eq!(
            serde_json::to_value(&bootstrap).expect("bootstrap should serialize"),
            json!({
                "target": {
                    "alias": "devbox",
                    "hostname": "devbox.internal",
                    "username": null,
                    "port": null,
                },
                "httpBaseUrl": "http://127.0.0.1:45123/",
                "wsBaseUrl": "ws://127.0.0.1:45123/",
                "pairingToken": null,
                "remotePort": 4111,
                "remoteServerKind": "managed",
            })
        );
    }

    #[test]
    fn parses_remote_pairing_json_from_last_non_empty_line() {
        assert_eq!(
            parse_remote_pairing_credential(
                "warning: shell banner\n{\"credential\":\"pairing-token\"}\n"
            ),
            Ok("pairing-token".to_string())
        );
        assert!(parse_remote_pairing_credential("{\"credential\":\"\"}\n").is_err());
    }

    #[test]
    fn normalizes_targets_and_builds_managed_password_launch_plans() {
        let hostname_only = normalize_ssh_environment_target(SshEnvironmentTarget {
            alias: "  ".to_string(),
            hostname: " host.internal ".to_string(),
            username: Some("  ".to_string()),
            port: None,
        })
        .expect("hostname-only target should normalize");
        assert_eq!(hostname_only.alias, "host.internal");
        assert_eq!(hostname_only.hostname, "host.internal");
        assert_eq!(hostname_only.username, None);

        let alias_only = normalize_ssh_environment_target(SshEnvironmentTarget {
            alias: " alias ".to_string(),
            hostname: String::new(),
            username: Some(" alice ".to_string()),
            port: None,
        })
        .expect("alias-only target should normalize");
        assert_eq!(alias_only.hostname, "alias");
        assert_eq!(alias_only.username.as_deref(), Some("alice"));
        assert!(
            normalize_ssh_environment_target(SshEnvironmentTarget {
                alias: " ".to_string(),
                hostname: " ".to_string(),
                username: None,
                port: None,
            })
            .is_err()
        );

        let plan = SshEnvironmentLaunchPlan::forward_with_auth(
            alias_only,
            41000,
            RemoteLaunchResult {
                remote_port: 42000,
                server_kind: "unexpected".to_string(),
            },
            &SshAuthOptions::with_secret("secret".to_string()),
        )
        .expect("managed password plan should build");
        assert_eq!(plan.remote_server_kind, "managed");
        assert_eq!(plan.remote_port, 42000);
        assert_eq!(plan.args[1], "BatchMode=no");
        assert!(!plan.args.iter().any(|argument| argument == "-p"));
        assert_eq!(plan.args.last().map(String::as_str), Some("alice@alias"));
    }

    #[test]
    fn remote_output_parsers_cover_defaults_and_error_context() {
        assert_eq!(last_non_empty_line(" \n first \n\n"), Some("first"));
        assert_eq!(last_non_empty_line(" \n\t"), None);
        assert!(
            parse_remote_pairing_credential("")
                .unwrap_err()
                .contains("credential")
        );
        assert!(
            parse_remote_pairing_credential("not-json")
                .unwrap_err()
                .contains("unparseable")
        );
        assert!(
            parse_remote_pairing_credential("{\"credential\":42}")
                .unwrap_err()
                .contains("invalid credential")
        );
        assert_eq!(
            parse_remote_pairing_credential("{\"credential\":\" token \"}"),
            Ok("token".to_string())
        );

        assert!(
            parse_remote_launch_result("")
                .unwrap_err()
                .contains("remote port")
        );
        assert!(
            parse_remote_launch_result("not-json")
                .unwrap_err()
                .contains("unparseable")
        );
        assert!(
            parse_remote_launch_result("{\"remotePort\":65536}")
                .unwrap_err()
                .contains("65536")
        );
        assert_eq!(
            parse_remote_launch_result("{\"remotePort\":3773}")
                .expect("missing kind should default"),
            RemoteLaunchResult {
                remote_port: 3773,
                server_kind: "managed".to_string(),
            }
        );

        let script = build_remote_launch_script();
        assert!(!script.contains("@@"));
        assert!(script.contains(&DEFAULT_REMOTE_PORT.to_string()));
        assert!(script.contains(&REMOTE_PORT_SCAN_WINDOW.to_string()));
    }

    #[test]
    fn auth_helpers_cover_noninteractive_and_permission_denied_variants() {
        assert!(
            build_ssh_child_environment(&SshAuthOptions::batch(), Path::new("unused")).is_empty()
        );
        for mechanism in [
            "password",
            "keyboard-interactive",
            "publickey",
            "hostbased",
            "gssapi-with-mic",
        ] {
            assert!(is_ssh_auth_failure(&format!(
                "PERMISSION DENIED ({mechanism})"
            )));
        }
        assert!(!is_ssh_auth_failure("Permission denied (certificate)"));
        assert!(!is_ssh_auth_failure("Permission denied"));
    }

    #[test]
    fn askpass_file_writes_are_idempotent_and_report_invalid_parents() {
        let directory = unique_temp_home();
        fs::create_dir_all(&directory).expect("temp directory should create");
        let helper = directory.join("askpass.cmd");
        write_askpass_file(&helper, "first", None).expect("helper should write");
        write_askpass_file(&helper, "first", None).expect("matching helper should be reused");
        assert_eq!(
            fs::read_to_string(&helper).expect("helper should read"),
            "first"
        );
        write_askpass_file(&helper, "second", None).expect("changed helper should rewrite");
        assert_eq!(
            fs::read_to_string(&helper).expect("helper should read"),
            "second"
        );

        let blocking_parent = directory.join("not-a-directory");
        fs::write(&blocking_parent, "file").expect("blocking file should write");
        assert!(
            write_askpass_file(&blocking_parent.join("child"), "value", None)
                .unwrap_err()
                .contains("Failed to write SSH askpass helper")
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn password_prompt_reports_presentation_cancellation_and_service_stop() {
        let request = || SshPasswordRequest {
            destination: "host".to_string(),
            username: None,
            prompt: "Password".to_string(),
        };

        let manager = SshPasswordPromptManager::with_timeout(Duration::from_secs(30));
        let presentation = manager
            .request_password_with(
                "emit-failure".to_string(),
                request(),
                UNIX_EPOCH,
                |_payload| Err("renderer unavailable".to_string()),
            )
            .await;
        assert!(matches!(
            presentation,
            Err(SshPasswordPromptRequestError::Presentation {
                operation: "send-prompt-request",
                ..
            })
        ));
        assert!(manager.remove_pending("emit-failure").is_none());

        let resolver = manager.clone();
        let cancellation = tokio::spawn(async move {
            manager
                .request_password_with("cancel".to_string(), request(), UNIX_EPOCH, |_| Ok(()))
                .await
        });
        tokio::task::yield_now().await;
        resolver
            .resolve(SshPasswordPromptResolution {
                request_id: " cancel ".to_string(),
                password: None,
            })
            .expect("prompt should cancel");
        assert!(matches!(
            cancellation.await.expect("cancellation task"),
            Err(SshPasswordPromptRequestError::Cancelled { request_id, .. }) if request_id == "cancel"
        ));

        let manager = SshPasswordPromptManager::with_timeout(Duration::from_secs(30));
        let dropper = manager.clone();
        let stopped = manager
            .request_password_with(
                "stopped".to_string(),
                request(),
                UNIX_EPOCH,
                move |payload| {
                    drop(dropper.remove_pending(&payload.request_id));
                    Ok(())
                },
            )
            .await;
        assert!(matches!(
            stopped,
            Err(SshPasswordPromptRequestError::ServiceStopped { request_id, .. }) if request_id == "stopped"
        ));
    }

    #[test]
    fn prompt_errors_and_time_formatting_keep_stable_messages() {
        let presentation = SshPasswordPromptRequestError::Presentation {
            request_id: "id".to_string(),
            destination: "host".to_string(),
            operation: "emit",
            message: "closed".to_string(),
        };
        assert_eq!(
            presentation.to_string(),
            "Failed to present SSH password prompt for host during emit: closed"
        );
        assert_eq!(
            SshPasswordPromptRequestError::TimedOut {
                request_id: "id".to_string(),
                destination: "host".to_string(),
            }
            .to_string(),
            "SSH authentication timed out for host."
        );
        assert_eq!(
            SshPasswordPromptRequestError::Cancelled {
                request_id: "id".to_string(),
                destination: "host".to_string(),
            }
            .to_string(),
            "SSH authentication cancelled for host."
        );
        assert_eq!(
            SshPasswordPromptRequestError::ServiceStopped {
                request_id: "id".to_string(),
                destination: "host".to_string(),
            }
            .to_string(),
            "SSH password prompt service stopped."
        );
        assert_eq!(
            SshPasswordPromptResolveError::InvalidRequestId.to_string(),
            "Invalid SSH password prompt id."
        );
        assert_eq!(
            SshPasswordPromptResolveError::Expired {
                request_id: "id".to_string(),
            }
            .to_string(),
            "SSH password prompt expired. Try connecting again."
        );
        assert_eq!(format_system_time(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        assert_eq!(
            format_system_time(UNIX_EPOCH - Duration::from_secs(1)),
            "1970-01-01T00:00:00Z"
        );
    }

    #[test]
    fn ssh_config_helpers_cover_quotes_assignments_paths_and_wildcards() {
        assert_eq!(
            split_directive_args(" Host foo # comment "),
            Ok(vec!["Host".to_string(), "foo".to_string()])
        );
        assert_eq!(
            split_directive_args("Include \"config #archive/file\" # comment"),
            Ok(vec![
                "Include".to_string(),
                "config #archive/file".to_string(),
            ])
        );
        assert_eq!(
            split_directive_args("Include=\"dir with spaces/file=name\""),
            Ok(vec![
                "Include".to_string(),
                "dir with spaces/file=name".to_string(),
            ])
        );
        assert_eq!(
            split_directive_args("Host 'one' two\\ three"),
            Ok(vec![
                "Host".to_string(),
                "one".to_string(),
                "two three".to_string(),
            ])
        );
        assert_eq!(
            split_directive_args("Host trailing\\"),
            Ok(vec!["Host".to_string(), "trailing\\".to_string()])
        );
        assert_eq!(
            split_directive_args(r#"Host "quoted\"alias""#),
            Ok(vec!["Host".to_string(), "quoted\"alias".to_string()])
        );
        assert!(
            split_directive_args("  ")
                .expect("blank directive should parse")
                .is_empty()
        );

        assert!(has_ssh_pattern("*.example.com"));
        assert!(has_ssh_pattern("host?"));
        assert!(has_ssh_pattern("!blocked"));
        assert!(!has_ssh_pattern("host"));
        assert!(wildcard_matches("*.conf", "team.conf"));
        assert!(wildcard_matches("host?", "host1"));
        assert!(!wildcard_matches("host?", "host"));
        assert!(!wildcard_matches("*.conf", "team.txt"));

        let home = unique_temp_home();
        assert_eq!(expand_home_path("~", &home), home);
        assert_eq!(expand_home_path("~/config", &home), home.join("config"));
        assert_eq!(expand_home_path("~\\config", &home), home.join("config"));
        assert_eq!(expand_home_path("plain", &home), PathBuf::from("plain"));
        assert_eq!(
            resolve_ssh_config_include_pattern("relative.conf", &home),
            home.join(".ssh").join("relative.conf")
        );
        let absolute = home.join("absolute.conf");
        assert_eq!(
            resolve_ssh_config_include_pattern(absolute.to_str().expect("utf-8 path"), &home),
            absolute
        );
    }

    #[test]
    fn config_globs_and_include_cycles_are_deterministic() {
        let home = unique_temp_home();
        let ssh_dir = home.join(".ssh");
        let include_dir = ssh_dir.join("config.d");
        fs::create_dir_all(&include_dir).expect("include directory should create");
        let alpha = include_dir.join("a.conf");
        let beta = include_dir.join("b.conf");
        fs::write(&alpha, "Host alpha\nInclude ../config\n").expect("alpha should write");
        fs::write(&beta, "Host beta\n").expect("beta should write");
        fs::write(ssh_dir.join("config"), "Include config.d/*.conf\n")
            .expect("config should write");

        assert_eq!(
            expand_glob(&alpha).expect("exact glob should resolve"),
            vec![alpha.clone()]
        );
        assert!(
            expand_glob(&include_dir.join("missing.conf"))
                .expect("missing exact path should resolve")
                .is_empty()
        );
        assert!(
            expand_glob(&ssh_dir.join("missing").join("*.conf"))
                .expect("missing glob directory should resolve")
                .is_empty()
        );
        assert_eq!(
            expand_glob(&include_dir.join("*.conf")).expect("glob should resolve"),
            vec![alpha, beta]
        );
        assert_eq!(
            collect_ssh_config_aliases_from_file(
                &ssh_dir.join("config"),
                &home,
                &mut BTreeSet::new(),
            )
            .expect("cyclic config should terminate"),
            BTreeSet::from(["alpha".to_string(), "beta".to_string()])
        );
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn discovery_handles_empty_inputs_precedence_values_and_io_errors() {
        if let Some(home) = default_home_dir() {
            assert!(!home.as_os_str().is_empty());
        }
        assert_eq!(discover_ssh_hosts(None), Ok(Vec::new()));
        assert_eq!(discover_ssh_hosts(Some(PathBuf::new())), Ok(Vec::new()));

        let home = unique_temp_home();
        assert_eq!(discover_ssh_hosts(Some(home.clone())), Ok(Vec::new()));
        let ssh_dir = home.join(".ssh");
        fs::create_dir_all(&ssh_dir).expect("ssh directory should create");
        fs::write(ssh_dir.join("config"), "Host duplicate\n").expect("config should write");
        fs::write(
            ssh_dir.join("known_hosts"),
            "duplicate ssh-ed25519 AAAA\nknown ssh-ed25519 BBBB\n",
        )
        .expect("known hosts should write");
        let hosts = discover_ssh_hosts(Some(home.clone())).expect("hosts should discover");
        assert_eq!(hosts.len(), 2);
        assert_eq!(hosts[0].alias, "duplicate");
        assert_eq!(hosts[0].source, "ssh-config");
        assert_eq!(
            hosts[0].to_value(),
            json!({
                "alias": "duplicate",
                "hostname": "duplicate",
                "username": null,
                "port": null,
                "source": "ssh-config",
            })
        );
        let _ = fs::remove_dir_all(&home);

        let config_error_home = unique_temp_home();
        fs::create_dir_all(config_error_home.join(".ssh").join("config"))
            .expect("config directory should create");
        assert!(
            discover_ssh_hosts(Some(config_error_home.clone()))
                .unwrap_err()
                .contains("Failed to read SSH config hosts")
        );
        let _ = fs::remove_dir_all(config_error_home);

        let known_hosts_error_home = unique_temp_home();
        let ssh_dir = known_hosts_error_home.join(".ssh");
        fs::create_dir_all(ssh_dir.join("known_hosts"))
            .expect("known hosts directory should create");
        assert!(
            discover_ssh_hosts(Some(known_hosts_error_home.clone()))
                .unwrap_err()
                .contains("Failed to read known SSH hosts")
        );
        let _ = fs::remove_dir_all(known_hosts_error_home);
    }

    #[test]
    fn known_hosts_parser_covers_markers_patterns_and_host_normalization() {
        assert_eq!(normalize_known_hosts_hostname("[host]:2222"), "host");
        assert_eq!(normalize_known_hosts_hostname("host:22"), "host");
        assert_eq!(normalize_known_hosts_hostname("2001:db8::1"), "2001:db8::1");
        assert_eq!(normalize_known_hosts_hostname("[incomplete"), "[incomplete");
        assert_eq!(
            parse_known_hosts_hostnames(
                "# comment\n@revoked revoked.example ssh-ed25519 AAAA\n*.wild ssh-ed25519 BBBB\n!blocked ssh-ed25519 CCCC\n@marker\n"
            ),
            BTreeSet::from(["revoked.example".to_string()])
        );
        let missing = unique_temp_home().join("known_hosts");
        assert_eq!(
            read_known_hosts_hostnames(&missing).expect("missing known hosts should be empty"),
            BTreeSet::new()
        );
    }
}
