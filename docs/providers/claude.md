# Claude

Install Claude Code, make sure `claude` is on the BiBCode server's `PATH`, and
sign in:

```bash
claude auth login
```

The default provider instance can remain simple:

```text
Display name: Claude
Binary path: claude
```

BiBCode probes `claude auth status --json` and shows the reported account when
available. Account text is blurred by default in Settings.

## Local account and usage status

The Claude CLI owns the local account and its credential lifecycle. BiBCode's
server is a read-only observer: it does not exchange refresh tokens, rotate
access tokens, rewrite `.credentials.json`, or update macOS Keychain items.
The **Agents** authentication state remains the result of `claude auth status
--json`; the status-bar usage request is a separate quota observation and does
not replace that readiness probe.

Every Claude usage fetch reads the current credential sources again. On macOS
the order is:

1. `Claude Code-credentials-<suffix>`, where `<suffix>` is the first eight
   hexadecimal characters of SHA-256 over the exact UTF-8 Claude config
   directory string;
2. the legacy `Claude Code-credentials` Keychain service;
3. `<config-directory>/.credentials.json`.

The config directory is `CLAUDE_CONFIG_DIR` when set and otherwise
`$HOME/.claude`. An explicit `CLAUDE_CONFIG_DIR` still participates in scoped
Keychain lookup. A non-UTF-8 directory skips only the scoped service, and
`BIBCODE_CLAUDE_KEYCHAIN_ACCESS=disabled` disables all Keychain reads while
preserving file lookup. Other platforms use the credential file.

BiBCode sends the current non-empty access token to Anthropic's usage endpoint
even when the locally recorded `expiresAt` is in the past; the server response
is authoritative. An HTTP 401 may try the next distinct credential source.
Other HTTP, transport, and JSON failures are returned without switching
credentials.

Status-bar polling uses the normal throttled refresh. Clicking its refresh
button is an explicit forced fetch and may overlap a background fetch. Enabling
a previously disabled Claude provider in **Settings → Agents** waits for the
settings update and readiness probe to succeed, then performs the same forced
Claude-only usage refresh. After signing in with the Claude CLI, either action
therefore observes the newly written local credentials without restarting
BiBCode.

## Context-window usage

Claude stream events provide the live context-usage fallback. After a successful
turn result, BiBCode sends the official response-correlated
`get_context_usage` control query and bounds the whole write-and-response
operation to two seconds. A successful response supplies `totalTokens` as the
active context usage, `maxTokens` as its maximum, and
`isAutoCompactEnabled` as the automatic-compaction indicator, and
`autoCompactThreshold`, when present, as `autoCompactTokens`: the context size at
which Claude compacts automatically (about 967K on native 1M-token models). The
context meter names it. The response is applied only while the completed turn
is still current; late responses for an earlier turn are rejected.

Unsupported, malformed, timed-out, cancelled, EOF, or write-failed control
queries are nonfatal. They leave the last good stream/result-derived context
usage in place and release completion without waiting longer. Pending control
response waiters are removed on timeout and shutdown. Accumulated totals in
Claude result messages are lifetime processed metadata only; they never replace
active context-window usage.

## Compaction and the context limit

Claude reports a compaction, automatic or from `/compact`, with `system`
`status` messages: `status: "compacting"` while it runs, then another status
(`null` or `requesting`) with `compact_result` (`success` or `failed`) and, on
failure, `compact_error`. BiBCode shows it in the turn as a **Conversation
compaction** step, failed with Claude's reason when compaction fails, so a
compaction near a full 1M-token window (often over a minute) does not look like
a stalled turn.

When a conversation no longer fits, Claude ends the turn with a context-limit
`terminal_reason` (`blocking_limit`, `prompt_too_long` or
`rapid_refill_breaker`) after a synthetic assistant message whose `error` is
`invalid_request` and whose text is `Prompt is too long` (or `Prompt is too long
· automatic compaction failed: <cause>`). BiBCode fails that turn with the
`context_limit` error class and the message "Claude's context window is full.
Compact the conversation, then send your message again.", followed by Claude's
text. The thread's error, titled "Claude ran out of context", offers **Compact
conversation**, which sends `/compact` to the same session as an ordinary
message; it is disabled while the thread is working, which includes a
compaction already running. Send your message again once the compaction
finishes. BiBCode does not compact or resend on its own.

Other failed turns prefer, in order: the result's `errors`, the text of
Claude's synthetic error message (an `authentication_failed`,
`oauth_org_not_allowed`, `verification_required` or `cloud_credential_error`
error is classed `permission_error`), the upstream API error Claude was
retrying, and finally `Claude turn failed (<terminal_reason>, HTTP
<api_error_status>).`. Every documented `terminal_reason` is listed in one
table in `apps/server/src/provider/claude/failure.rs`; a reason missing from it
is shown by name. These shapes were checked against Claude Code 2.1.306.

## Updates and version advisories

BiBCode resolves the configured executable before selecting a release source
and action, so a package-manager inventory cannot update a different Claude
installation. The recognized installation rows are:

| Installation                                    | Latest source                                             | Action in BiBCode                                      |
| ----------------------------------------------- | --------------------------------------------------------- | ------------------------------------------------------ |
| Native `~/.local/bin/claude`                    | Claude `stable` or `latest` channel; defaults to `latest` | Resolved `claude update`                               |
| Homebrew `claude-code` cask                     | Claude `stable`                                           | `brew upgrade --cask claude-code`                      |
| Homebrew `claude-code@latest` cask              | Claude `latest`                                           | `brew upgrade --cask claude-code@latest`               |
| WinGet `Anthropic.ClaudeCode`                   | Claude `latest`                                           | `winget upgrade Anthropic.ClaudeCode`                  |
| Marked apt, dnf, or apk repository installation | Repository's stable/latest channel                        | Display-only system command; BiBCode never executes it |
| Recognized npm/pnpm/Bun/Vite+ path              | npm                                                       | Matching package-manager command                       |

For Linux repository installations, the display-only guidance is respectively
`sudo apt update && sudo apt upgrade claude-code`, `sudo dnf upgrade
claude-code`, or `apk update && apk upgrade claude-code`. It is provided for a
user to run with the required privileges, not as a server update action.

Channel selection defaults native Claude to `latest` when user settings are
absent or malformed. Managed settings take precedence, and only managed
`autoUpdatesChannel` strings `stable` and `latest` are valid; every other
present value, including null or another JSON type, leaves latest metadata
unavailable instead of guessing.

BiBCode reads only regular local settings and repository evidence. File size,
managed-fragment count and aggregate size, and I/O time are bounded; special
files, metadata races, timeouts, overflow, and read errors fail closed.
`DISABLE_UPDATES=1`, whether in the effective provider environment or settings,
disables executable update actions but does not suppress recognized advisory
metadata or display-only guidance. `DISABLE_AUTOUPDATER` alone does not control
this BiBCode action.

Custom executable paths and wrappers are manual-only. BiBCode does not infer an
updater for them. A zero command exit is also advisory only: the post-update
probe must show a current advisory or a provable installed-version advance
from a fresh exact-target pre-command probe before the update is reported as
successful.

When the command exits successfully but the exact executable remains outdated,
BiBCode reports the resolved executable path together with the detected and
expected versions. Provider settings retain the captured updater output and a
**Recheck** action so an externally repaired installation can be verified
without rerunning the update. The server log records the provider, instance,
executable, before/after/expected versions, exit code, and verification status
as structured `provider.maintenance.update.verify` fields.

## Supported instance customization

The following provider-instance settings are applied by the current runtime:

- **Binary path** selects the Claude executable.
- **Environment variables** are passed to inventory probes and chat sessions.
- Variables marked **Sensitive** are stored separately as server secrets. The
  saved value is redacted when settings are returned to the app.

This supports Claude-compatible gateways and routing tools without a special
BiBCode driver. Copy the environment variables required by the gateway into the
Claude provider instance, mark credentials as Sensitive, and use the gateway's
current documentation for endpoint and model values.

Avoid placing provider-specific credentials in global shell startup files when
only one Claude instance needs them.

## Current settings limitations

The settings schema currently displays **Claude HOME path** and **Launch
arguments**, but the native runtime does not apply either field when probing or
starting Claude. Do not rely on those fields for account isolation or extra CLI
arguments.

If an advanced setup needs a different process home today, set `HOME` explicitly
in that provider instance's Environment variables (`USERPROFILE` on Windows) and
authenticate Claude under that same home. This is process-environment behavior;
BiBCode does not validate or migrate Claude's files.

Existing-thread model pickers currently lock to the Claude driver, not to a
configured Claude home. Prefer the provider instance that created the thread;
the UI does not guarantee that switching to another Claude instance can resume
the same provider session.

## Model options

The Claude session applies three model options:

- **Reasoning** (`effort`), passed to Claude as `--effort`;
- **Fast Mode** (`fastMode`), passed in the session settings. Turning it on
  needs a model that offers it: one Claude reported with Fast Mode, including
  aliases such as `opus`, or, for a model Claude did not report, one BiBCode's
  built-in catalog offers it for. Turning it off is always accepted;
- **Agent** (`agent`), passed as `--agent` and listed from the agents Claude
  reports.

Models and their options normally come from Claude's session initialization.
When Claude reports no models there, BiBCode offers its built-in catalog, which
lists only the options above. A turn whose options the session refuses, for
example one saved while an older catalog offered another option, is not retried:
its delivery fails once and the thread shows **Delivery failed** with the
reason, such as "Fast Mode is not supported by the selected model." Choose
another model or option, then send the message again.

## Steering a running turn

Steer sends the head queued message to Claude while it is working. Claude can
consume it at the next tool boundary. If no tool boundary is available, Claude
finishes the current reply first, then immediately consumes the message as its
next turn.
Steer therefore does not guarantee that a reply without tool calls will change
before it finishes.

BiBCode marks the message delivered when the running Claude process replays the
same input text, including when that replay arrives after the current reply's
`result`. Image blocks alongside the text do not prevent acknowledgement;
image-only messages are acknowledged too. A replay containing any tool-result
block, or different text, is not an acknowledgement.
A local command such as `/compact` is not replayed; Claude reports it with a
`result` naming the command (`local_command`), which marks it delivered and ends
its turn. While Claude compacts the conversation, before reading a message or
for `/compact`, the 60-second confirmation wait is paused and then restarts.
The acknowledgement remains pending for the lifetime of that process, even
across turn completion. The message is attributed to the BiBCode runtime turn
current when the echo arrives, without allocating a new turn for the steer.
The card can remain **Steering…** while Claude finishes the earlier reply. You can
keep composing and queueing messages while it waits. If the process or session
is replaced before a matching replay arrives, delivery is uncertain; BiBCode
does not automatically resend it. A steer rejected before writing, or whose turn
is unavailable, returns to the queue with its reason on the card. Interrupts and
errors keep queued messages on hold until you use **Send now**. See
[Queued messages](../user/workspace-ui.md#queued-messages) for queue controls.

## BiBCode tools

Each Claude chat session gets the BiBCode MCP server (`bibcode`) with a
credential scoped to its thread. BiBCode writes that server's configuration,
bearer token included, to a file only your user can read (mode `0600` on Linux
and macOS, an owner-only access list on Windows) in the system temporary directory and passes the file's path to
`--mcp-config`, so the token never appears on Claude's command line, where any
local process could read it. The file is removed when the session ends. The
server's MCP URL uses loopback (`127.0.0.1`) even when BiBCode listens on every
interface.

The credential is a random token that exists only in the running server's
memory, as a hash. It expires after eight hours, is replaced when a message
starts the thread's session again after it was suspended or stopped, and is
discarded when BiBCode quits. To retire a token that may have been exposed, quit
and restart BiBCode, or stop the thread's session and send it a new message.

## Background work and idle suspension

BiBCode suspends an idle chat session 60 seconds after its turn ends and
resumes the same conversation on the next message. A Claude session is not idle
while Claude still runs work outside a turn: `run_in_background` Bash commands,
Monitor watches, background subagents, or MCP calls Claude moved to the
background. Claude reports these on its stream-JSON output, with or without hook
events and whatever the Chat Activity setting, in `system` messages
(`task_started`, `task_progress`, `task_updated`, `task_notification`, and
`background_tasks_changed`, which carries the complete live set in Claude Code
2.1.203 and later; see the
[Agent SDK reference](https://code.claude.com/docs/en/agent-sdk/typescript)).
BiBCode counts tasks that are neither foreground nor `ambient` and keeps the
session while the count is non-zero and Claude has written something in the
last 30 minutes.

When a background task finishes, Claude answers it in a turn it starts itself:
a `user` message and a `result` whose `origin.kind` is `task-notification`.
BiBCode gives that answer a turn of its own, so the thread reads running and
idle suspension waits for its result. Between the finished task and that turn
the session stays busy.

If a session was stopped while background work ran (for example by an earlier
version of BiBCode), resuming it makes Claude deliver the stopped tasks'
notification first. Claude can report that turn's `result` before it reads the
new message; BiBCode ignores that `result` and logs `ignored a Claude result
that arrived before the turn's input was read`, then the new message runs as
its own turn.

## Activity observation

BiBCode detects Claude activity features from the configured executable instead
of promising support for an indefinite version range.

For structured chat, it probes `--version` and `--help`. Hook activity is enabled
only when help advertises both exact switches:

- `--include-hook-events`
- `--forward-subagent-text`

If either switch is unavailable or the bounded probe fails, Claude still runs in
its normal stream-JSON mode without structured hook activity.

A launch waits up to two seconds for the probe. The probe itself may run for up
to five seconds, so a slow first `--help` still records its answer for the next
launch. BiBCode caches only a probe in which both commands answered, keyed by the
executable's identity and version. A failed or timed-out probe is not cached,
so the next launch probes again instead of running without the switches for the
life of the server. Each probe run writes one line to `server.log`: `Claude
activity probe finished` with `outcome` (`supported` or `unsupported`), the
version, and the elapsed time, or a `Claude activity probe failed` warning with
`outcome` (`failed` or `timed out`) and the reason. A launch that stops waiting
logs `launching Claude without activity flags`.

Structured chat control correlation is stricter than display observation.
BiBCode exposes a private task target only after one session and Activity
generation proves the complete identity chain: an Agent/legacy Task tool
invocation, its authenticated asynchronous PostToolUse result carrying the
same `tool_use_id` and `agentId`, a `task_started` record carrying that
`tool_use_id` and `task_id`, and the matching `SubagentStart` `agent_id`.
`task_type` may be absent or exactly `local_agent` or `remote_agent`; every
other value fails closed.
Root invocations require both the stream invocation and authenticated hook to
identify the root as their source and `SubagentStart` to report no parent. A
nested invocation additionally requires its stream `parent_tool_use_id` to
resolve to an active exact parent correlation whose launched `agentId` equals
the nested authenticated PostToolUse hook's top-level source `agent_id`; the
exact PostToolUse `agentId` names the child. Documented `SubagentStart` hook
payloads do not include `parent_agent_id`; after the exact source proof, BiBCode
privately reparents that named child to the source parent. If a future payload
does include explicit lineage, it must agree. Root/nested mismatch, sibling
cross-wiring, conflicting lineage, or mismatched source ownership never enables
control. A bounded dependency fixpoint settles already-present parent, child,
and deeper chains in one observation regardless of lexical identity order.

The complete PostToolUse chain is the authoritative path. When a nested Claude
version provides no usable PostToolUse launch result, an authenticated
`PreToolUse` from an already exact parent instead opens a parent-owned pending
interval. The nested-only parent-local fallback admits control only when the
stream parent tool, the accepted nested `task_started` candidate, and the
unmatched verified child are globally unique, with no unresolved root launch
that could own the child. The correlator then privately assigns that child to
the exact source parent. A later exact PostToolUse result promotes that pending
state to an exact target or contradicts it, retires it, and tombstones its
identity chain so replay cannot recreate the target. Multiple nested candidates,
multiple unmatched children, or any unresolved root launch remain observable
but unsupported and cause zero provider I/O; BiBCode does not infer identity
from text, timing, order, proximity, or transcript contents.

Arrival order is irrelevant. Every accepted fact that installs, retires, or
terminalizes a target carries a deterministic domain-separated SHA-256 event
key through the production event pump. The key is duplicate-stable, bounded,
status-separated, and contains no provider-native identity. Effect-producing
conflicts use bounded canonical classes such as asynchronous/other and
local-agent/other, so arbitrary status or type labels cannot suppress a target
retirement. Optional source and parent identities are framed as explicit
`none` or `some` plus the bounded value; the valid literal native identity
`<root>` is therefore never confused with absence. Present invalid hook source
or parent fields are rejected at the authenticated boundary rather than being
coerced to the absent/root form. Names, roles, descriptions, prompts,
timestamps, output paths, and event proximity are never correlation keys;
incomplete, conflicting, stale, malformed, or saturated chains remain
observable but uncontrollable. Live identity maps and unmatched terminal
statuses are each bounded at the Activity page limit, including the
generation-owned parent-local pending set (200 correlations). The fallback uses
no timers, polling, or transcript reads. Terminal retirement, Activity
disablement, and runtime replacement clear or retire its pending and installed
state. Terminal retirement
atomically removes the live tool/agent/task join and records generation-scoped identities in three
fixed 256-word tombstone filters (2 KiB each; 6 KiB total). Tombstones are never
evicted within a generation: a filter false positive can only disable a private
target, never create one or invent lifecycle. At exact terminal-status-map
saturation new ambiguous task facts fail closed rather than displacing earlier
terminal authority. Only bounded identities and terminal status are retained,
and provider-native IDs do not cross persistence, contracts, or logs.

A `task_notification` with status `stopped`/`cancelled`, `failed`, or
`interrupted` is authoritative Cancelled, Failed, or Interrupted lifecycle for
an exactly mapped actor and retires its private target. The runtime retains a
bounded exact terminal task-to-agent link after an ordinary SubagentStop so a
reordered authoritative task notification can still replace Completed. A later
`SubagentStop` cannot rewrite cancelled, interrupted, or failed lifecycle to
completed.

When the compatibility probe proves both hook switches and the authenticated
hook sink starts, targeted cancellation is provisionally enabled for that
structured-chat runtime generation. Clicking an actor with a complete exact
mapping or admitted nested-only parent-local fallback sends one stream-JSON
control request with subtype `stop_task` and that private `task_id`. Any
non-Claude target, root/foreground actor, ambiguous nested actor, or actor
without a current admitted mapping is rejected before provider I/O; BiBCode
never falls back to the conversation-wide interrupt request.

The control response must carry the matching request ID. Only Claude's exact
unsupported-control protocol error authoritatively disables targeted
cancellation for the current generation. Before the caller receives that
result, BiBCode removes every current Claude task handle and operation,
publishes targeted cancellation as unsupported, and cancels and reaps queued or
in-flight targeted work. Future clicks then fail without another provider
write. Generic provider errors, timeouts, malformed responses, and connection
closure remain safe operation failures and do not disable the capability. A
restarted or re-enabled runtime starts a fresh generation and may prove support
and exact mappings again. Private task IDs and raw control errors are redacted
from Activity errors, logs, and Debug output.

The Activity panel shows `Stopping` from the server's ephemeral control overlay;
the `stop_task` response itself does not terminalize the actor. Authenticated
hook and task lifecycle events remain authoritative. If only part of a selected
subtree is delivered, **Retry remaining** sends only the current exact residual
task mappings and late descendants already captured by the original
generation-scoped fence. It never performs a new semantic lookup or falls back
to Claude's conversation-wide interrupt.

Provider-terminal observation has a separate capability and safety gate. The
executable must support the required settings switches and BiBCode's additive
hook preparation. If preparation cannot be established before spawn, the
original Claude terminal command runs without structured terminal activity.

See [Activity observation](../architecture/activity-observation.md) for the
shared protocol and provider matrix.
