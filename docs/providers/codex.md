# Codex

Install the Codex CLI, make sure `codex` is on the BiBCode server's `PATH`, and
sign in:

```bash
codex login
```

For one account, the default provider instance needs no custom home:

```text
Display name: Codex
Binary path: codex
CODEX_HOME path: empty
Shadow home path: empty
```

An empty `CODEX_HOME path` lets Codex use its normal default. BiBCode resolves
that default as `~/.codex` when grouping Codex state internally.

## Updates and version advisories

BiBCode identifies the installation from the resolved `codex` executable and
its canonical target. It does not use a separate package-manager inventory,
because another inventory can describe a different Codex installation.

- Official standalone installations use the resolved executable's `codex update`
  action.
- Homebrew cask installations use `brew upgrade --cask codex`.
- Recognized npm, pnpm, Bun, and Vite+ package-manager paths use their matching
  global-install command.

Each recognized Codex row obtains its latest-version advisory from npm's
`@openai/codex` latest document; the action is still selected from the
installation source above.

An arbitrary custom executable or wrapper is manual-only: BiBCode withholds
both its latest-version advisory and update action rather than applying a
package-manager command to an unproven installation. After an update command
exits successfully, BiBCode reprobes the same instance; it reports success only
when a fresh advisory is current or the installed version provably advanced.

## Shared state with separate authentication

Codex can use a shared home for sessions and configuration while keeping a
second account's `auth.json` in a shadow home.

First, authenticate the normal account:

```bash
codex login
```

Then authenticate the second account in a separate home:

```bash
mkdir -p ~/.codex-personal
CODEX_HOME=~/.codex-personal codex login
```

Configure two provider instances:

```text
Display name: Codex Work
Binary path: codex
CODEX_HOME path: ~/.codex
Shadow home path: empty

Display name: Codex Personal
Binary path: codex
CODEX_HOME path: ~/.codex
Shadow home path: ~/.codex-personal
```

Before starting the second instance, BiBCode prepares its shadow home. The
shadow keeps its own `auth.json` and links shared, non-private Codex state back
to the configured `CODEX_HOME`.

Do not copy or delete a whole Codex home as a troubleshooting step. Back up both
directories before changing them, and verify that the shadow directory contains
its own regular `auth.json` file.

### Current account-status limitation

The provider status probe does not currently apply the configured Codex home or
shadow home. The account email shown in Settings may therefore describe the
server's default Codex login rather than the selected shadow account. Use clear
display names, and verify the account from the Codex session when identity
matters.

BiBCode also does not currently publish the shared-home continuation group in
provider status. Existing-thread model pickers are locked to the Codex driver,
but must not be treated as proof that every displayed Codex instance can safely
resume the same provider session. Prefer the instance that created the thread.

## Custom endpoints and secrets

Add account-specific variables in the provider instance's **Environment
variables** section. Mark API keys and tokens as **Sensitive**. BiBCode stores
sensitive values as server secrets and does not send the value back to the app
after saving.

## Context-window usage

Codex publishes token usage only for the root thread of the current provider
session; child-thread notifications are ignored. In a normalized usage update,
`tokenUsage.last.totalTokens` is active context-window usage and
`tokenUsage.total.totalTokens` is lifetime processed usage. The optional
`tokenUsage.modelContextWindow` is the maximum context-window size. Lifetime
processed usage is displayed as context metadata and never substitutes for the
active usage meter.

Codex reports automatic context compaction with each valid usage update. The
workspace meter can therefore explain that Codex compacts context automatically
when needed, while still showing the current active usage and maximum
independently.

## Steering a running turn

While Codex is working, Enter adds a queued message. **Steer** on the first
card, or `Mod+Shift+Enter` in the composer, sends it through Codex `turn/steer`
with the active turn ID. Codex injects it at that turn's next boundary. The
message joins the running turn, and Stop continues to target that same turn.

If steering is rejected or the turn is no longer available, the message returns
to the queue with the reason on its card. It can start normally when the session
is ready; interruptions and errors keep queued messages on hold until you use
**Send now**. See [Queued messages](../user/workspace-ui.md#queued-messages) for
cancellation, attachments, and reload behavior.

## Reverting to a checkpoint

**Revert to this message** first asks Codex to drop the newer turns from its
conversation history. Only after that succeeds does BiBCode restore the
workspace files from its own Git checkpoint refs, so a refused rewind changes
nothing (see
[Checkpoint revert](../architecture/rpc-and-orchestration.md#checkpoint-revert)).
Codex never restores files.

Codex reports a `historyMode` for each thread in its `thread/start` and
`thread/resume` responses, and that mode decides how BiBCode rewinds it:

- `paginated` threads, which Codex creates by default from 0.151: BiBCode
  pages newest-first through `thread/turns/list`, then sends `thread/revert`
  with the ID of the oldest turn to drop as `beforeTurnId`. When Codex holds
  fewer turns than BiBCode is dropping, every remaining turn is dropped.
- `legacy` threads, and responses without `historyMode`: BiBCode sends
  `thread/rollback` with the number of turns to drop.

Codex 0.156 removed `thread/rollback`, so it cannot rewind a legacy thread that
an older Codex created. That revert fails with an error saying so and changes
nothing; start a new thread to continue from that point.

## Activity observation

Structured Codex chats run through `codex app-server`. BiBCode uses App Server
events for live activity and feature-detects optional reconciliation methods such
as descendant-thread, history, and background-terminal discovery. Unsupported
methods degrade the affected activity view instead of inventing data.

BiBCode accepts validated `subAgentActivity` items as bounded child-discovery
hints and direct-reads those child thread IDs. This remains effective when an
App Server version, including Codex 0.146, omits spawned children from
`thread/list`; listing remains enabled as a complementary discovery source for
versions that expose it. Malformed or out-of-scope hints are ignored. When read
and list methods are incompatible, recovery capabilities downgrade truthfully
without disabling ordinary chat. Valid mutations accepted before a later
transient failure or same-root reconnect remain pending until a current
reconciliation publication succeeds; replacing the root or disabling activity
discards that root-owned pending state. Pending recovery coalesces superseded
record states while preserving retained per-actor chronology, and orders each
parent state before the child or reparent state that depends on it. Unsortable
or cyclic topology stays pending behind a runtime warning. Valid publication is
split into bounded events; the first carries scope and health, and only
successfully queued event prefixes are acknowledged.

Provider-terminal activity is also capability-gated. BiBCode probes the
configured executable before adding its private remote-observation arguments. If
the required switches are unavailable or preparation fails safely, the original
Codex terminal command runs without structured terminal activity.
Provider-terminal reconciliation remains list-based: `subAgentActivity` can
trigger a bounded list pass but cannot create a provisional terminal-scope actor
or opt that observer into structured-chat direct-read recovery.

In structured chats, BiBCode exposes targeted Stop capability only for a
canonical verified child thread with one currently active turn. The private
child `threadId` and `turnId` can be recovered from validated live or
reconciliation lifecycle data and are removed on matching completion. Root,
provisional, stale, conflicting, terminal, malformed, and oversized targets are
rejected without falling back to the root composer interrupt, terminal cleanup,
or operating-system process termination. Provider-terminal Activity remains
read-only.

After revalidating the private handle against the current session and Activity
generation, BiBCode sends App Server `turn/interrupt` with that exact child
`threadId` and active child `turnId`. It never substitutes the root thread or
routes the request through the composer Stop path. Interrupt acknowledgement
records delivery only: subsequent App Server lifecycle events decide whether
the actor is interrupted, cancelled, completed, or failed. Subtree retries stay
inside the original server-owned fence and send only active residual or late
descendant handles that are still exact.

See [Activity observation](../architecture/activity-observation.md) for the
shared protocol and provider matrix.
