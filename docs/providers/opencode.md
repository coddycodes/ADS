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

BiBCode drives OpenCode 1.x through its V1 server API and OpenCode 2.x through
the `/api/*` API that replaced it. It picks the API from what the server
answers, so the same instance works with either version.

With OpenCode 2, chats work: messages and slash commands, streamed replies,
permission requests (including those of subagents), questions, Stop,
OpenCode's own follow-up turns that deliver a background subagent's result, and
subagent activity. The provider row lists OpenCode 2 models (with their
variants), agents, commands and skills; a slash skill attaches the skill to
your message; a command wins over a skill of the same name. When a configured
**Server URL** answers but its catalogs can't be read, the row stays ready with
a notice and chats use the server's default model. Reverting to a checkpoint
rewinds the OpenCode 2 conversation, then restores the files. The provider
terminal is observed too (see below).

The provider row reads OpenCode's `--version` on every check, including the
quick one, so a version BiBCode cannot drive never reads Ready. A version
BiBCode cannot drive (3.x or later) shows an error asking you to set
**Binary path** to an OpenCode 1.x or 2.x executable; a configured
**Server URL** that runs one reports the same error, naming **Server URL**.

## Updates and version advisories

BiBCode selects an update source from the resolved `opencode` executable and
its canonical target, not from a separate package-manager inventory. Native
OpenCode installations use the resolved `opencode upgrade` action; recognized
Homebrew installations use `brew upgrade anomalyco/tap/opencode`; recognized
npm, pnpm, Bun, and Vite+ paths use their matching global-install command.
Each recognized row gets its latest-version advisory from npm: OpenCode 1 is
published as `opencode-ai`, OpenCode 2 as `@opencode/cli`. A package-manager
install of OpenCode 2 (a path through `node_modules/@opencode/cli`) updates and
compares against `@opencode/cli`; where the path cannot tell (a native or
Homebrew install), an installed 2.x version compares against `@opencode/cli`.
The update action remains installation-specific.

On Windows, a recognized npm-backed `opencode.cmd` (or OpenCode 2's
`opencode2.cmd`) runs the package beside it: `node_modules/opencode-ai` selects
`npm install -g opencode-ai@latest`, `node_modules/@opencode/cli` selects
`npm install -g @opencode/cli@latest`, and BiBCode supervises that package's
native `opencode.exe`. The server resolves the platform's npm command shim from
the provider environment. This is path-based source selection, so Windows
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

BiBCode generates a temporary password, waits until the loopback endpoint
answers an API it speaks, and stops the child process with the provider
session. OpenCode 1 is ready when its authenticated `GET /global/health` answers
`{"healthy": true}`; OpenCode 2 when `GET /api/info` reports a 2.x version. Any
other answer, such as the HTML that OpenCode 2 serves for V1 paths, is not
readiness, and an unsupported version is stopped at once with its error.
Inventory
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
activity. An observed terminal runs against a private OpenCode server that
BiBCode starts for it: OpenCode 1 opens it with `opencode attach`, OpenCode 2
with `opencode --server … --session …`. OpenCode 2 has no `--model` flag, so
an unobserved OpenCode 2 terminal runs `opencode --standalone` (its own private
server) with the selected model in its configuration; OpenCode 1 runs
`opencode --model …`.
