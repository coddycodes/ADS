# Remote Access

The macOS, Linux, and Windows desktop UI exposes saved remote environments and
host-sharing controls under **Settings → Remote Servers**. Windows also keeps
**Settings → Local environment** for WSL. Browser/hosted clients can connect to a
reachable server but cannot perform desktop-owned listener, firewall, or SSH operations.

Each saved server's row in that list shows its connection state. While a server
sends its settings slowly, the row reads "Receiving settings from <name> over a
slow connection…" until it connects.

Remote access connects a browser or another desktop app to the Coddy Studio server
running on a different machine. That server owns projects, files, Git state,
terminals, provider CLIs, credentials, and agent sessions.

Use a trusted private network such as a LAN or tailnet. Do not expose a plain
Coddy Studio HTTP endpoint directly to the public internet.

## Name a saved server

In **Settings → Remote Servers → Connect to a host → Add Server**, enter a
**Server alias (optional)** along with the pairing code. This name appears in
the saved-server list and environment rail on this device and persists across
restarts. Leave it blank to use the name supplied by the server. A headless
web-mode `coddy-studio serve` supplies its hostname (for example `ai-server`), or
"Local" if the machine has no usable hostname. `coddy-studio start`, bare `coddy-studio`,
and desktop-owned servers keep "Local". An alias does not rename the server for
other clients.

To change the name later, open the server's **⋯** menu in the saved-server list
and choose **Rename…**. The new name appears in the environment rail, the
selected server's card, and the saved-server list on this device, and survives
restarts. **Use the server's name** puts back the name the server reports while
it is connected. Renaming does not reconnect the server or change anything on
it.

Loopback pairing through a local connection or SSH tunnel retains the saved
connection when the server has already activated its standard credential;
administrative permissions are not required to finish that pairing.

## Desktop and browser network access

Use the Share controls to create an address-specific pairing offer:

1. Open **Settings → Remote Servers → Share this host**.
2. Choose **Another device** for desktop-managed private-network access. The
   desktop app offers automatic LAN only after it observes a usable private
   default route; creating the offer then restarts the backend with native
   network access.
3. Choose **Custom address** for an SSH tunnel, reverse proxy, public hostname,
   or separately launched server. Custom addresses are externally managed and
   never change the desktop listener or firewall.
4. Select an IP address in **Address**, or leave **Automatic (LAN)** selected.
   Hostname-based endpoints show their hostname. Generate the pairing offer.

Native interface observations are visible to the Share flow before widening,
but report unavailable while the listener is loopback-only. Public-only and
non-default private topologies fail closed instead of attempting a native
widen. Use a custom address backed by an externally managed listener or reverse
proxy on those hosts.

- A loopback URL works only on the server machine.
- A plain LAN or Tailnet HTTP URL can be used by a desktop client or by a page
  served over HTTP.
- An HTTPS-hosted web app cannot connect to an insecure HTTP/WS backend because
  browsers block mixed content. Use an HTTPS/WSS endpoint in that case.

### Tailscale endpoints

When the desktop app can read `tailscale status --json`, it can add the machine's
Tailnet IPv4 addresses and MagicDNS name to the endpoint list.

An HTTPS MagicDNS endpoint is shown as available only after its well-known
Coddy Studio endpoint responds successfully. Configure Tailscale Serve separately to
proxy the chosen HTTPS address to the local Coddy Studio backend, then let the desktop
app rescan it. The native `coddy-studio` CLI does not currently provide Tailscale
Serve setup flags.

### Reverse proxies

Run the reverse proxy on the same machine as the Coddy Studio server and have it send
the browser's host in `X-Forwarded-Host`. Coddy Studio trusts `X-Forwarded-Host` and
`X-Forwarded-Proto` only from a proxy on the same machine. For nginx:

```nginx
location / {
  proxy_pass http://127.0.0.1:3773;
  proxy_set_header X-Forwarded-Host $http_host;
  proxy_set_header X-Forwarded-Proto $scheme;
  proxy_http_version 1.1;
  proxy_set_header Upgrade $http_upgrade;
  proxy_set_header Connection "upgrade";
}
```

Without `X-Forwarded-Host`, nginx sends `Host: 127.0.0.1:3773`, and a browser
signed in with its session cookie gets `403` on every change and on the live
connection. Clients that use a bearer token are not affected.

Previews of a dev server that listens on the server's loopback are not available
through a reverse proxy, through Tailscale Serve, or on a public address. Their
ports are not proxied. Connect over the LAN, the tailnet IP, or an SSH tunnel to
use them.

## Update a remote server

For a desktop-hosted release, choose **Update to v…** on its sidebar card or in
**Settings → Remote Servers**. The confirmation names the host and explains
that its running agents and terminals stop; conversations and queued messages
are kept. Counts may still be loading, and an unavailable count does not block
your choice.

The card and row show download progress, backup, restart, and version checking.
Closing Settings keeps the update running. On failure, **Retry** asks again;
Settings also offers **Dismiss**. If the host has not come back, inspect Coddy Studio
there and re-pair if its port changed. A restart on the old version is reported
explicitly. The host displays who requested the update; **Manage devices** opens
its sharing controls.

Headless and externally managed hosts use **Show update steps** and **Copy**.
Steps match their archive/package, OS, architecture, or SSH launch. Run them on
that host; verify an SSH pid still belongs to that server before stopping it.
The browser page offers **Reload** when its own server was updated by another
client. It preserves your unsent text until you choose Reload.

## Headless server

`coddy-studio start` and `coddy-studio serve` run the same native server. `start` opens the
startup URL in a browser by default; `serve` does not. Only headless web-mode
`serve` names itself after the machine's hostname, falling back to "Local" if
no usable hostname is reported. `start` (including `start --no-browser`), bare
`coddy-studio`, and the desktop app's own backend keep the name "Local".

Download the signed-release checksum set and the native archive or Linux package for
your host by following [Standalone server installation](./server-installation.md).
Published server distributions contain the matching built web client and discover it
automatically.

For example, bind to a trusted Tailnet address and serve built web assets:

```bash
coddy-studio serve \
  --host "$(tailscale ip -4)" \
  --static-dir /path/to/CoddyStudio/apps/web/dist
```

The server prints one JSON object to stdout. In authenticated web mode it has
this shape:

```json
{
  "address": "100.64.0.10:3773",
  "httpBaseUrl": "http://100.64.0.10:3773",
  "token": "one-time-pairing-credential",
  "pairingUrl": "http://100.64.0.10:3773/pair#token=one-time-pairing-credential",
  "pairingCode": "coddy-studio://pair?code=…"
}
```

`pairingCode` is a five-minute encrypted offer for the desktop app's
**Add Server** dialog; it is present only when the bound address is routable
(not loopback, not `0.0.0.0`). Pass `--no-startup-pairing-offer` (or set
`CODDY_STUDIO_NO_STARTUP_PAIRING_OFFER=1`) when stdout goes to a log, and mint
offers on demand with `coddy-studio pairing offer` instead. `pairingUrl` remains the
one-time owner bootstrap for a browser.

To keep the server running across reboots, install it as a per-user service
with `coddy-studio service install --host <address>`; see
[Standalone server installation](./server-installation.md#run-as-a-per-user-service).

The CLI does not print a QR code and has no `project` subcommand. Pairing
credentials come from `coddy-studio pairing offer` (encrypted offers for the desktop
app) and `coddy-studio pairing issue` (the desktop SSH bootstrap). Use
`coddy-studio serve --help` for the implemented options.

`--static-dir` explicitly overrides packaged web discovery and must contain
`index.html`. A source-built or intentionally API-only server can omit static assets. A
separately hosted HTTPS web app can connect directly to an HTTPS/WSS backend without the
backend serving static files.

## Pairing

On startup in authenticated web mode, the server issues an owner pairing
credential. The credential is carried in the URL fragment so it is not sent as
part of the initial HTTP request:

```text
http://server.example:3773/pair#token=PAIRING_CREDENTIAL
```

The client exchanges the credential for a session. Treat the credential and
pairing URL as passwords until the exchange completes.

For a configured hosted web app, use a URL shaped like:

```text
https://app.example.com/pair?host=https://backend.example.com#token=PAIRING_CREDENTIAL
```

The hosted app saves the backend address, but it does not proxy traffic. The
browser still connects directly to the backend, which must therefore be
reachable over HTTPS/WSS from that browser.

Create and revoke additional access from **Settings → Remote Servers**. On a
headless server, mint an encrypted offer for the desktop app from the CLI:

```sh
coddy-studio pairing offer --endpoint http://100.64.0.10:3773
```

It prints a `coddy-studio://pair?code=…` link that expires after five minutes. Paste
it into **Settings → Remote Servers → Connect → Add Server** on the other
device. `--reach this-computer` requires a loopback endpoint and is for tunnels;
`--name` sets the display name (default: this machine's hostname); `--json`
prints one JSON line. The command works while the server is running on the same
data root and refuses to run before the server has ever started there. Revoke
the resulting device from the Share tab like any other client. The focused
`coddy-studio pairing issue` command remains for desktop-managed SSH bootstrap.

A server started with `--dev-url` keeps its data in `<base>/dev`; without it,
the server uses `<base>/userdata`. Pass the same `--dev-url`, or set
`VITE_DEV_SERVER_URL`, for `coddy-studio pairing offer` and `coddy-studio pairing issue`
to select that server's state, including when both stores exist.

## Desktop-managed SSH

The desktop contains an SSH launcher that can install a small runner under
`~/.coddy/studio/ssh-launch/<state-key>`, start or reuse `coddy-studio serve` on remote
loopback, and create a local port forward. The remote host must provide:

- a compatible native `coddy-studio` executable on non-interactive `sh`'s `PATH`;
- `curl` or `wget` for the readiness probe; and
- each provider CLI and its credentials.

The launcher does not install Node.js, npm, npx, package-manager shims, or a
Coddy Studio binary on the remote host. Install a matching standalone server release first.

Adding the environment pairs this desktop with the remote server over SSH. The
desktop then appears as a device with standard access on the host's **Share**
tab, and it keeps that credential for reconnects, so reconnecting while the SSH
tunnel is up runs no SSH command. The success toast says the environment is
connecting; its row shows whether it connected.

SSH access decides who can connect. If you revoke the desktop's device on the
host, the desktop pairs again over SSH on its next connection. To cut a desktop
off, remove its SSH access to the host or remove the environment on that
desktop. If the host rejects a credential the desktop has just received, the
environment stops retrying and asks you to connect again; if that keeps
failing, remove the environment and add it again.

Each remote step has a time limit: issuing the pairing credential 30 seconds,
starting the server 60 seconds, stopping it 30 seconds. A step that runs out
reports the limit (for example "The remote host did not issue a pairing
credential within 30 seconds. Check the connection; Coddy Studio keeps trying.").
Coddy Studio keeps retrying on its own; the environment shows **Connecting…** until a
step succeeds.

While a remote environment is not connected, its chats show the reason once in
the banner above the composer, with **Open Remote Servers**, which opens this
settings page, where you connect, rename, or remove the environment. The
sidebar notice names the environment and offers the same action; hover or
focus it for the reason.

If a network fetch fails before receiving a server response, its connection
notice asks you to check that the server is running and reachable. Coddy Studio keeps
reconnecting automatically. Framework transport diagnostics and request URLs
stay out of this fetch-failure notice; the original transport cause remains
available to diagnostic handling. Timeout and other failure notices keep their
own guidance.

## Windows Subsystem for Linux

The optional WSL backend runs a native Linux `coddy-studio` binary. It does not invoke
WSL Node.js, npm, npx, or a JavaScript server package.

Prerequisites:

- Windows Subsystem for Linux and an installed distribution;
- `wsl.exe` available to the desktop process;
- a `coddy-studio` Linux binary matching the distribution architecture; and
- provider CLIs and credentials installed inside the distribution.

For source development, the desktop searches
`target/x86_64-unknown-linux-gnu/(debug|release)/coddy-studio` and
`target/aarch64-unknown-linux-gnu/(debug|release)/coddy-studio`. Set
`CODDY_STUDIO_WSL_SERVER_BINARY` to a different Windows-side binary path when needed;
the desktop translates it with `wslpath` for the selected distribution.

The WSL launcher uses a fixed system `PATH` and starts `coddy-studio serve` directly.
Verify a custom binary from Windows with:

```powershell
wsl.exe -d <distribution> -- /path/to/coddy-studio --version
```

When **WSL only** is enabled, a missing distribution, binary, or failed WSL
startup leaves the local backend unavailable; the desktop does not silently
start the Windows backend. In **Settings → Local environment**, choose another
distribution or **Retry WSL** after correcting the prerequisite. Use
**Diagnostics** to save diagnostic logs. **Switch to Windows** is the explicit
way to make the native Windows backend primary; the normal restart and storage
identity checks then apply.

## Security notes

- Bind the server only to a trusted private address.
- Prefer HTTPS/WSS whenever the browser page itself is served over HTTPS.
- Treat pairing URLs and credentials as secrets.
- In the browser/hosted UI, review and revoke sessions you no longer trust in
  **Settings → Connections**.
- Credentials can leak through browser history, screenshots, logs, or copied
  text even when they are placed in a URL fragment.
