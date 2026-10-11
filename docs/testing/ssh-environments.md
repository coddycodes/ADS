# Desktop-managed SSH environments

This runbook validates adding and reconnecting a desktop-managed SSH
environment: the remote launch, the pairing script, the saved standard-scope
bearer, the relaunch of a remote server that stopped under a live tunnel, and
previews of the remote host's dev servers through the preview gateway.
The behavior it checks is described in
[Remote access architecture](../architecture/remote.md#desktop-managed-ssh) and
the [user guide](../user/remote-access.md#desktop-managed-ssh).

Record results in a report created from the
[execution report template](./execution-report-template.md), including its
**SSH environment evidence** section.

## Automated harness (Linux and macOS)

`apps/desktop/src-tauri/tests/ssh_environment.rs` drives the production
`SshEnvironmentManager`, launch, stop, and pairing scripts, a real `bibcode`,
and the production `/oauth/token` exchange. Only the SSH hop is faked: the
stand-in client `tests/fixtures/ssh/fake_ssh.py` (Python 3) runs remote
commands locally with OpenSSH semantics (argv joined with spaces, run by
`/bin/sh -c`) under a private temporary "remote" home. It never contacts a
host and never reads `~/.ssh`.

The tests are `#[ignore]`d because they need a fresh `bibcode`, which
`cargo test -p bibcode-desktop` neither builds nor checks. From the repository
root:

```sh
cargo test -p bibcode-server --test cli_smoke --no-run -j 2
cargo test -p bibcode-desktop --test ssh_environment -- --ignored
```

The first command builds `bibcode` with the hermetic test guard through a dev
unit. An ordinary `cargo build` would replace it with a feature-off binary, so
do not substitute that command for this harness. CI's **Test** job runs the
same two commands. Each fake remote seeds the shared hermetic provider settings
before the real CLI boot, with disabled providers pinned to absent owned paths
and network update checks disabled. The harness checks the launch-state
`server.log` for guard diagnostics. Its stdin replay is a pipe, matching SSH
stream semantics on macOS and Linux; the finite feeder ends at EOF or when its
reader closes, while the fake still execs into the remote shell.
Prerequisites: `python3`, `curl`
or `wget`, `ps`, and `/bin/sh`. `BIBCODE_SSH_FIXTURE_BIBCODE` selects another
binary; `BIBCODE_SSH_FIXTURE_PORT_START` moves the fake remote's port scan
(default 47310, away from a local BiBCode on 3773). Each scenario stops the
remote servers and tunnels it started; after a run, no `fake_ssh.py`,
`bibcode serve --base-dir …/remote-home/.bibcode`, or `sleep 600` process
should remain (a stopped server can take a moment to exit).

The fake classifies each remote script by its first line,
`# bibcode-ssh:<launch|pairing|stop>`. By default it execs into the remote
shell, so killing the client also kills the remote command. The pairing-timeout
scenarios instead use its sshd-like mode (`SSH_FIXTURE_SSHD_SESSIONS=1`, set by
`Harness::with_sshd_sessions`): each remote command runs in its own session and
the fake only relays stdio, so, as with `sshd` without a PTY, killing the client
signals nothing. With a hung `bibcode pairing issue` (alias `hang-pairing…`),
the remote command must end within the pairing watchdog's bound, the pairing
deadline plus 5 s, plus 3 s, after the desktop has given up; three timed-out
attempts in a row must leave no remote command behind.

`cargo test -p bibcode-desktop` also runs the remote scripts themselves,
without SSH, under `/bin/sh` and each of `dash`, `bash --posix` and
`busybox sh` that is installed, once per shell binary: on Ubuntu CI `/bin/sh`
is dash, so dash and bash run; on Fedora `/bin/sh` is bash, so only bash runs;
macOS runs its `/bin/sh` and bash 3.2. They cover the
pairing watchdog, the launch script's wall-clock readiness limits (including a
`wget`-only `PATH`), its stopping of servers it gave up on even with its output
closed, its checks of recorded pids (malformed ones included, with `kill`
replaced by a recorder), its state records when a launch is cut short, and the
stop script's TERM-only stop, which waits for the server to exit, so a launch
right after it never runs beside a server still shutting down. On
Windows the same command runs two tests that saturate a Tokio blocking pool,
checking that SSH output still arrives and that no pipe read stays parked after
a drain gives up; only a Windows host or CI can run them.

The SIGTERM-ignoring pairing stand-in ignores TERM before executing Python,
so interpreter startup cannot bypass the watchdog's TERM-to-KILL escalation.
A delayed-interpreter regression keeps one PID through startup and verifies
the original escalation bound and final process cleanup.

The tests for concurrent preparation and disconnect use a real child process
serving HTTP on a numeric loopback address. Their Python fixture uses
`socketserver.TCPServer` and rejects reverse lookups: `http.server.HTTPServer`
resolves its server name before listening, which can make an otherwise local
fixture depend on host DNS.
The launch gate, tunnel readiness, operation order, and cleanup assertions keep
their normal deadlines.

The harness is compatibility evidence for OpenSSH join semantics. It does not
exercise askpass, a real `sshd`, a remote login shell other than `/bin/sh`, or
Windows `ssh.exe`; the live procedure below covers those. Its sshd-like mode
models only one property of `sshd`: a command without a PTY is not signalled
when the client goes away.

## Live procedure (Windows, Linux, and macOS desktops)

Use a host you control. Do not change its SSH or system configuration for the
test.

### Prerequisites

- A remote Linux or macOS host with a native `bibcode` matching the desktop's
  version on non-interactive `sh`'s `PATH`, plus `curl` or `wget`. Check with
  `ssh <host> 'command -v bibcode && bibcode --version'`.
- Two ways in: key authentication, and password authentication (a second
  account or alias that offers only `password`/`keyboard-interactive`). Run the
  scenarios once per method.
- A way to see the host's devices: the host's **Settings → Remote Servers →
  Share** tab, opened from an administrative client of that host.
- Before starting, note the host's device list and remove leftovers from
  earlier runs, so the count below is meaningful.

### Scenarios

Run in order on the desktop under test. For each, record the evidence class,
the environment row's state, and any exact error text.

| #   | Scenario                     | Steps                                                                                                                        | Expected                                                                                                               |
| --- | ---------------------------- | ---------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------- |
| 1   | Add                          | **Settings → Remote Servers → Connect → Add environment → SSH**, enter the host, add.                                        | Toast **Environment added**, "<label> is connecting over SSH."; the row reaches connected. Password auth prompts once. |
| 2   | Remote restart, tunnel alive | Stop the managed server for this environment only; see [Stopping the remote server](#stopping-the-remote-server). Then wait. | The row reconnects on its own; that environment's `pid` file holds a new pid. No new device appears on the host.       |
| 3   | Disconnect, then Connect     | Choose **Disconnect** on the row, then **Connect**.                                                                          | Connects without a pairing prompt or error. No new device.                                                             |
| 4   | Reload                       | Reload the window (or open a second window).                                                                                 | Connects with the saved credential. No new device.                                                                     |
| 5   | Desktop restart              | Quit the desktop app and start it again.                                                                                     | Connects (launch and tunnel run again; password auth prompts again). No new device.                                    |
| 6   | Dead link (optional)         | Cut the network path to the host for over 45 s, then restore it.                                                             | The tunnel ends after keepalive; the row reconnects after the link returns.                                            |
| 7   | Revocation (optional)        | Revoke the desktop's device on the host's Share tab, then wait for the desktop to reconnect.                                 | The desktop pairs again over SSH and connects; the old device is gone and one new device appears.                      |
| 8   | Hung pairing (optional)      | With the wrapper from [Hung pairing command](#hung-pairing-command) on the host, add the environment; let it retry 3 times.  | Each attempt fails with the pairing-timeout copy; about 37 s after each, its `sleep 6143` is gone. Nothing builds up.  |

### Stopping the remote server

Each environment has its own launch directory,
`~/.bibcode-ssh-launch/<state-key>`, and a host can hold several. The state key
is the first 16 hex digits of the SHA-256 of the environment's alias, hostname,
user name, and port, joined by NUL bytes, with an empty field for an unset user
or port. Use the values the desktop saved for the environment (an alias
without a separate host name uses the alias for both). On the desktop machine:

```sh
# Linux
printf '%s\0%s\0%s\0%s' '<alias>' '<hostname>' '<user>' '<port>' | sha256sum | cut -c1-16
# macOS
printf '%s\0%s\0%s\0%s' '<alias>' '<hostname>' '<user>' '<port>' | shasum -a 256 | cut -c1-16
```

Then, on the remote host, confirm that the recorded pid is that environment's
`bibcode serve` before stopping only that pid (`ps -p <pid> -o command=` works
on Linux and macOS):

```sh
ssh <host>
dir=~/.bibcode-ssh-launch/<state-key>
pid=$(cat "$dir/pid")
ps -p "$pid" -o command=   # must show: …bibcode serve --host 127.0.0.1 --port … --base-dir …/.bibcode
kill "$pid"
```

If `ps` shows anything other than `bibcode serve`, stop: the pid file is stale
and the pid may belong to another process. Record that and skip scenario 2.
The launch and stop scripts make the same check before they reuse or stop a
recorded pid, and leave any other process alone.

### Hung pairing command

Scenario 8 checks the remote pairing watchdog: `sshd` does not signal a command
without a PTY when the client goes away, so the pairing script itself ends a
`bibcode pairing issue` that outlives the desktop's 30 s limit, with TERM at
35 s and KILL 2 s later. Remove the environment and its device first, so that
adding it mints a credential. On the host, create a wrapper named `bibcode` in a
directory that precedes the real one on non-interactive `sh`'s `PATH`, without
changing any configuration (skip the scenario if no such directory exists):

```sh
#!/bin/sh
# Hangs on `pairing issue`; runs the real CLI otherwise.
if [ "$1" = pairing ]; then exec sleep 6143; fi
exec /path/to/the/real/bibcode "$@"
```

`ssh <host> 'command -v bibcode'` must print the wrapper. Add the environment,
note when each pairing timeout appears, and about 40 s after each run
`ssh <host> 'pgrep -fl "sleep 6143"'`: it must print nothing. After three
attempts, remove the environment and delete the wrapper.

After scenario 5 (or 7), the host must list exactly **one** device for this
desktop, labelled **BiBCode Tauri Desktop**, with standard access (not
administrative). Record the device count and access before and after.

A stuck scenario is a failure: record the environment row's message. A pairing
step that exceeds its limit reads "The remote host did not issue a pairing
credential within 30 seconds. Check the connection; BiBCode keeps trying." and
the row stays **Connecting…** while BiBCode retries.

### Preview gateway over SSH

Run these after scenario 1 with the environment connected, from a thread on
it. They check the [preview gateway](../architecture/remote.md#preview-gateway)
and its [SSH port forward](../architecture/remote.md#desktop-managed-ssh) end
to end; the user-facing behavior is in
[Previewing the server's dev servers](../user/workspace-ui.md#previewing-the-servers-dev-servers).
Record each as its own row (evidence class, result, exact message) in the
report's SSH environment evidence section.

**Dev server.** In the thread's terminal, run
`printf '<script>document.write(location.origin)</script>' > origin.html` and
then `python3 -m http.server 8123 --bind 127.0.0.1`. Make sure nothing on the
desktop machine listens on port 8123. With **Open links in** set to
**BiBCode browser**, click `http://localhost:8123/` in the chat:

- The BiBCode browser shows the directory listing, and its address bar shows
  `http://localhost:8123/`, never the `/__bibcode/bootstrap` hop. Open
  `http://localhost:8123/origin.html`: the page shows `http://localhost:8123`,
  the same origin as on the server.
- On the desktop machine, an
  `ssh … -N -L [::1]:8123:127.0.0.1:<remote> -L 127.0.0.1:8123:127.0.0.1:<remote>`
  child is running (only the `127.0.0.1` forward when the machine has no IPv6
  loopback), and port 8123 listens on loopback only (`ss -ltnp` on Linux,
  `lsof -nP -iTCP -sTCP:LISTEN` on macOS, `netstat -ano` on Windows).
- **Reload** loads the page again. Stop the server and press **Reload**: the
  gateway's listener is still open, so the tab shows the gateway's own `502`
  page, titled "Nothing is listening", with "Nothing is listening on port 8123
  on <environment>." The address bar still shows `http://localhost:8123/`.
  This is a page inside the tab, not BiBCode's "Can't show this page here"
  overlay, which appears only when the listener has already closed and
  BiBCode opens the address again. Start the server again and press
  **Reload**: the page returns.
- `https://localhost:8123/` shows "HTTPS dev servers can't be previewed through
  the gateway yet; serve over HTTP or open it on <environment> directly."
- Open a second preview tab on another address, such as `https://example.com`,
  then switch back to the first within a minute: no new forward `ssh` child
  starts. Stay on the second tab for over a minute: the first tab's forward
  `ssh` child exits.
- Close the preview tab: the forward's `ssh` child exits at once.
- **Busy local port.** Close the preview tab. On the desktop machine run
  `python3 -m http.server 8123 --bind 127.0.0.1`, then click
  `http://localhost:8123/origin.html` again: the address bar still shows
  `http://localhost:8123/origin.html`, the page shows
  `http://127.0.0.1:<random port>`, the forward binds `127.0.0.1:<random port>`,
  and a note "localhost:8123 is in use on this computer" appears once; another
  click on the same address shows no further note. Stop the local server.
  Repeat on port 8124 with the local server bound to IPv6 only
  (`--bind ::1`): the preview again opens on a random `127.0.0.1` port with the
  note for `localhost:8124`.
- **Separate preview storage.** Connect a second SSH environment, run the same
  `python3 -m http.server 8123 --bind 127.0.0.1` there, and preview
  `http://localhost:8123/` from a thread on each environment in turn. In the
  first, run `localStorage.setItem("probe", "first")` from the preview's
  developer tools; in the second, `localStorage.getItem("probe")` returns
  `null`. Switching back to the first environment's preview still returns
  `"first"`, and a preview from the local environment keeps the data it had
  before. Remove the second environment in **Settings → Remote servers**: its
  `preview-profiles/<hash>` directory (macOS: its data store) is gone when
  no preview of it was opened this session (otherwise it stays on disk).
- With **Open links in** set to **System browser**, the same click opens the
  system browser at `http://localhost:8123/` (at `http://127.0.0.1:<local port>/`
  when port 8123 is busy on the desktop machine), and the listing loads.
  That forward is held by a 5-minute lease: with no BiBCode browser tab on the
  same address, its `ssh` child exits about 5 minutes after the click, and the
  system-browser page stops loading.

**Brainstorming companion.** This needs Node.js on the remote host and the
superpowers plugin's brainstorming scripts copied there. Set **Open links in**
back to **BiBCode browser** first: the request opens like a clicked link. In
the thread's
terminal, run its `scripts/start-server.sh --open` with the default bind host
(the companion opens a browser only for a `127.0.0.1` or `localhost` bind, and
only when no browser is connected yet). Note `screen_dir` from the JSON it
prints, then write an HTML file into that directory:

- The companion runs `$BRAINSTORM_OPEN_CMD` with its keyed
  `http://localhost:<port>/?key=<hex>` address. The desktop opens it as a
  BiBCode browser tab in the same thread, and the companion's first screen
  loads instead of a 403 page.
- Write a second HTML file, or change the first: the page reloads on its own.
  That proves the companion's WebSocket, which requires `Origin` to match
  `Host`, works through the gateway.
- No browser opens on the remote host.

Stop the companion with its `stop-server.sh` when done.

**Revocation.** With the companion page (or another page that holds a
WebSocket) open, revoke the desktop's device on the host's Share tab. Within
30 s the page's WebSocket closes: a new screen file no longer reloads it. After
the desktop pairs again, as in scenario 7, **Reload** loads the page through a
new gateway session.

### Cleanup

1. Remove the environment on the desktop (row menu → **Remove server…**). This stops the
   tunnel and the managed remote server.
2. Revoke the test device on the host's Share tab.
3. Check that the host has no leftover managed server or pairing command:
   `ssh <host> 'ls ~/.bibcode-ssh-launch/*/pid 2>/dev/null; pgrep -fl "bibcode serve"; pgrep -fl "bibcode pairing"'`.
   Stop only a process this test started. A server that ignored the stop's
   TERM for 10 s keeps running and keeps its `pid` file; record that, and stop
   it as in [Stopping the remote server](#stopping-the-remote-server).
