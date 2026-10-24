# Headless pairing offers and per-user service install: design

Status: approved in conversation on 2026-09-03 ("recommended 2 plus 1", then
"ok, do everything"). Implementation plans:
`docs/superpowers/plans/2026-09-03-headless-pairing-offer-cli.md` and
`docs/superpowers/plans/2026-09-03-headless-service-install.md`.

## Problem

The Remote Servers spec (§4.6) scoped v1 so that a headless `bibcode serve`
mints encrypted pairing offers only from the Share tab of an already-paired
browser session. The CLI's only startup output is the legacy owner bootstrap
(`pairingUrl` with `#token=`), which the desktop's primary **Add Server** path
cannot consume: that dialog expects a `bibcode://pair?code=…` offer and its help
copy says "from the server's Share tab". A headless box therefore needs a
three-hop detour (open the bootstrap URL in a browser, mint from the Share tab,
paste into the desktop) for the flagship flow, and the fallback the CLI does
support is the bearer route that the UI presents as unencrypted.

Two adjacent gaps surfaced during the same investigation:

- The headless server has no supported way to survive a reboot. The Linux
  packages deliberately install no service, and the provider CLIs it spawns
  find their credentials per user, so a system service running as root or a
  service account would not work either.
- `bibcode` reports a bind failure as "failed to bind the server listener"
  without the address or the OS error.

## Decisions

### D1. `bibcode pairing offer` mints an encrypted share offer against the data root

A new `pairing offer` subcommand mints the same grant shape the Share tab mints
through `POST /api/auth/pairing-offer`, writes it into `auth_pairing_links`,
and prints the `bibcode://pair?code=…` link. It works beside a running server
because the store runtime lock is shared and grant consumption reads the
database (`consume_grant` goes straight to `consume_auth_pairing_link` when
repositories exist), exactly the mechanism `bibcode pairing issue` already
relies on for the desktop SSH bootstrap.

Shape of the grant, deliberately the share-offer shape rather than the
administrative-link shape: subject `one-time-token`, `STANDARD_SCOPES`,
`reach` set, `off_host` set to `reach == another-device`, five-minute TTL,
self-enforced `MAX_ACTIVE_PAIRINGS`. The administrative shape (`reach: None`)
would silently disable the off-host confirmation guard in
`exchange_pairing_bootstrap` and would not count toward `desired_exposure`.
No idempotency row is written; every CLI call mints a fresh offer.

Inputs: `--endpoint <http(s) URL>` is required and never derived. `--reach`
defaults to `another-device`; `this-computer` requires a loopback endpoint and
`another-device` a non-loopback one, validated by the same function the HTTP
handler uses (extracted from the handler so the two paths cannot drift).
`--name` defaults to the machine hostname. `--label` is optional. `--json`
prints exactly one JSON line; plain mode prints the link and the expiry.

Fail-closed rules: no data store at the root, or no persisted host identity
key, both stop with "start the server on this data root first". The host key
is read, never generated, so the CLI can never create a key a running server
has not loaded. The storage identity is read from the `environment-id` marker
through a crate-visible wrapper over the existing private `read_marker`, not
through `inspect_store`, which takes the offline operation lock. The CLI
always resolves the `userdata` state directory; a `--dev-url` server's `dev`
directory is out of scope.

Scope note: `STANDARD_SCOPES` excludes `access:write`, so a device paired
through a CLI offer cannot mint further offers. This matches the Share tab's
default grant.

### D2. `bibcode serve` prints a ready-to-paste offer at startup

When the server runs in web mode with authentication, binds a non-loopback,
non-unspecified address, and startup offers are not disabled, `start_internal`
mints one share offer through the live `AuthService`
(`issue_share_pairing(default_standard_scopes(), Some(label), "another-device", true)`)
and `run_server` prints it as `pairingCode` in the existing single JSON
startup line. The value is the full `bibcode://pair?code=…` link. The startup
token is **not** embedded: it is an administrative bootstrap with `reach: None`
and must keep serving the browser `/pair` route only.

`--host 0.0.0.0` yields no `pairingCode` because the advertised endpoint is
unknown; the docs point to `pairing offer` for that case. Desktop-mode servers
are unchanged (`startup_access` is `None`).

Trade-off: under a service manager, stdout lands in a journal, so every restart
would leave a fresh five-minute off-host credential in the log and could hold
`desired_exposure` wide for that window. A new global flag
`--no-startup-pairing-offer` (env `BIBCODE_NO_STARTUP_PAIRING_OFFER`) turns
the startup offer off, and `service install` bakes it into the service
definition. The legacy `pairingUrl` already has the weaker form of this
exposure and is unchanged.

### D3. `bibcode service install | uninstall | status` runs the server as the user

The server must run under the human user's identity: provider CLIs resolve
from the process `PATH` (`process/executable.rs`), and Claude Code and Codex
keep their credentials under the user's home. So the command installs a
**per-user** service, never a system one, and nothing ships inside the
`.deb`/`.rpm` (the package contract test forbids `systemd` in `nfpm.yaml`).

Per platform:

- Linux: writes `~/.config/systemd/user/bibcode.service`, runs
  `loginctl enable-linger` (no argument: the calling user), then
  `systemctl --user daemon-reload` and `systemctl --user enable --now
  bibcode.service`. Over a plain SSH session there may be no user bus yet;
  the command then prints the three exact commands to run once lingering
  takes effect instead of failing opaquely.
- macOS: writes `~/Library/LaunchAgents/com.bibcode.server.plist` and runs
  `launchctl bootstrap gui/<uid> <plist>`. A LaunchAgent needs a logged-in
  GUI session; the docs say to enable automatic login on a server Mac. A
  LaunchDaemon would survive without a session but cannot reach the user's
  keychain, where Claude Code stores its token, so it is rejected.
- Windows: `schtasks /Create /SC ONLOGON /RL LIMITED` running the executable
  as the current user. "Run whether user is logged on or not" needs a stored
  password and is deferred; this iteration starts the server at logon.

The service definition runs the absolute path of the current executable with
`serve --host <host> --port <port> [--base-dir <dir>] [--static-dir <dir>]
--no-startup-pairing-offer`. Only those `ServerArgs` are honoured;
`--mode` and `--bootstrap-fd` are ignored by the service command even though
they are global flags.

`PATH` is captured from the installing process, which is the user's
interactive shell, and written into the service definition. No login-shell
probe is added to the server crate. Consequence, documented: re-run
`service install` after installing a provider CLI in a new location.

Testing seam: pure `render_*` functions per platform plus a `CommandRunner`
trait with an injected fake, so tests assert file contents and the exact
command sequence. Real service-manager execution is runbook validation.

### D4. Bind errors name the address and the OS error

`ServerError::Bind` becomes `Bind { address, source }` and renders
`failed to bind the server listener on <address>: <os error>`.

## Alternatives considered

- **Startup-only offer (no subcommand).** Offers expire after five minutes,
  so every later pairing would still need the browser detour. Rejected alone;
  kept as the convenience half of D2.
- **Embedding the startup token in a code.** Wrong grant shape (see D1);
  would degrade the confirmation guard. Rejected.
- **Local control socket into the running server.** More machinery than the
  database-as-authority pattern the SSH bootstrap already proves. Rejected.
- **System service with a dedicated user.** Breaks provider credential
  discovery and file ownership; also forbidden by the package contract.
  Rejected.
- **Login-shell `PATH` probe in the server.** Would duplicate desktop code and
  fire on every start; capturing the installer's `PATH` once is simpler and
  honest. Deferred; revisit if unit-file `PATH` maintenance becomes a support
  burden.

## Invariants preserved

- Pairing survives restart on the same data root: host key, grants, and
  device sessions are persisted; covered by a new integration test that mints
  through the CLI, pairs, restarts, and reconnects.
- Generic unavailable connection databases stay non-destructive (unrelated
  work landed the same day; listed to keep the doc set consistent).
- `cli_smoke.rs` help assertions and its JSON readiness-line parse keep
  passing: all new fields are additive and the startup line stays a single
  JSON object.

## Documentation changed by the plans

`docs/user/remote-access.md` (headless section, pairing section, the "no
`auth` subcommands" sentence), `docs/user/server-installation.md` (the "does
not create a service" promise and a new service section),
`docs/architecture/remote.md` (CLI authority surface beside the SSH bootstrap
paragraph), `docs/operations/observability.md` (service manager mention),
the three OS runbooks under `docs/testing/`, and the Add Server dialog copy in
`apps/web`.
