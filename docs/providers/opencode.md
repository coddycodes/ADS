# OpenCode

Install OpenCode on the machine running the BiBCode server, then authenticate its
providers:

```bash
opencode auth login
```

The default BiBCode instance can remain:

```text
Display name: OpenCode
Binary path: opencode
Server URL: empty
Server password: empty
```

## Supported versions

BiBCode drives OpenCode 1.x through its V1 server API. OpenCode 2 replaced that
API with `/api/*` routes and is not supported yet. When the resolved `opencode`
reports version 2 or later, the provider shows an error asking you to set
**Binary path** to an OpenCode 1.x executable, and inventory does not start an
OpenCode 2 server. A configured **Server URL** that runs OpenCode 2 reports the
same error, naming **Server URL** instead.

## Updates and version advisories

BiBCode selects an update source from the resolved `opencode` executable and
its canonical target, not from a separate package-manager inventory. Native
OpenCode installations use the resolved `opencode upgrade` action; recognized
Homebrew installations use `brew upgrade anomalyco/tap/opencode`; recognized
npm, pnpm, Bun, and Vite+ paths use their matching global-install command.
Each recognized row gets its latest-version advisory from npm's `opencode-ai`
latest document, while the update action remains installation-specific.

On Windows, a recognized npm-backed `opencode.cmd` path selects `npm install
-g opencode-ai@latest`; the server resolves the platform's npm command shim
from the provider environment. This is path-based source selection, so Windows
runtime behavior still depends on the actual host's npm shim and `PATH`.

An arbitrary custom executable or wrapper is manual-only. BiBCode does not
publish a latest-version advisory or execute an update action for an unproven
path. Following a zero-exit action, it reprobes the same instance and reports
success only when a fresh advisory is current or the installed version provably
advanced.

## Managed local server

When **Server URL** is empty, BiBCode starts a private OpenCode server for the
session:

```text
opencode serve --hostname=127.0.0.1 --port=<available-port>
```

BiBCode generates a temporary password, waits until the loopback endpoint's
authenticated `GET /global/health` answers `{"healthy": true}`, and stops the
child process with the provider session. Any other answer, such as the HTML that
OpenCode 2 serves for V1 paths, is not readiness. An OpenCode 2 server is
identified by its `GET /api/info` version and stopped at once with the
unsupported-version error. Inventory
uses the same managed-server approach to discover authentication state, models,
agents, and commands. On Windows, a standard npm `opencode.cmd` installation is
resolved to its packaged native `opencode.exe` for managed launches so the
server remains inside BiBCode's Job Object through shutdown; custom wrappers
and non-Windows launch paths remain unchanged.

Chat capability discovery sends the active workspace directory to OpenCode's
command and agent endpoints. Skills from its user and repository configuration
appear in the `/` menu's Skills group, separately from ordinary commands. The
same directory selection applies to managed servers and configured endpoints.

## Existing OpenCode server

Set **Server URL** to connect to an OpenCode server you already operate. Set
**Server password** when that endpoint requires one.

The OpenCode server password field is currently stored in plain text in BiBCode's
settings file. Prefer the managed local server, or otherwise protect the server's
state directory and use a narrowly scoped credential. Provider-instance
Environment variables marked Sensitive use BiBCode's separate secret storage,
but the dedicated Server password field does not.

## Provider terminal and activity

The provider terminal launches `opencode` in the active worktree. BiBCode adds a
small terminal-only theme override so the TUI follows the embedded terminal's
colors; it does not replace your normal OpenCode configuration.

OpenCode chats and provider terminals can publish structured activity when the
installed provider path supports observation. Failure to prepare terminal
observation leaves the normal terminal command available without structured
activity.
