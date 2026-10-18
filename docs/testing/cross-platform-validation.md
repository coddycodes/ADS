# Cross-Platform Validation

This page defines the procedure common to every native desktop validation.
Pair it with the [Windows](./windows-desktop.md),
[Linux](./linux-desktop.md), or [macOS](./macos-desktop.md) runbook.

## Inputs

Before starting, record the requested:

- repository and remote;
- branch or exact revision;
- required ancestor commits, if any;
- expected product version, if any;
- native operating system and architecture;
- features, regressions, and user scenarios in scope; and
- permission boundaries for commits, pushes, merges, installations, system
  settings, credentials, and external services.

Inputs are execution data. Do not edit the living runbooks to insert them.

## Required pre-work

1. Read every applicable `AGENTS.md` from the repository root to the affected
   files.
2. Run `git status --short` and preserve unrelated changes.
3. State the requested outcome, constraints, affected packages, and completion
   evidence.
4. Follow the CodeGraph setup and fallback rules in `AGENTS.md`.
5. Read `docs/README.md`, the architecture overview, workspace layout, scripts
   reference, relevant living documents, package manifests, tests, and CI.
6. Inspect recent history for affected paths when intent is unclear.

Do not install, repair, or re-index repository tools outside the authority
granted by `AGENTS.md` and the current request.

## Native process sampling

When native process sampling or its boot initialization changes, run the focused
sampling, attribution, and ownership checks:

```sh
cargo test -p bibcode-server --lib diagnostics::native::tests:: -j 2
cargo test -p bibcode-server --lib diagnostics::resource_sampler::tests:: -j 2
cargo test -p bibcode-server --lib diagnostics::monitor::tests:: -j 2
cargo test -p bibcode-server --lib production::server_terminal::tests::embedded_host_cleanup_preserves_unmanaged_host_descendants -j 2 -- --exact
```

Construction must leave the native process table empty. The first diagnostics
read starts the background sampler; its first sample must discover the current
process and previously unknown descendants with their names, creation
identities, and parents; subsequent samples must refresh resource observations.
Reads must never take a sample or wait for one in progress after the first, the
sampler must stop once reads stop, and shutdown must stop it for good.
Attribution must retain its core/UI/external scopes, and cleanup must preserve
unrelated host processes. Direct PID identity lookup and
empty ownership cleanup must not require a host-wide sample.

For a boot-sampling performance change, compare eager and lazy construction on
the same host using the pinned `sysinfo` version, recording process count,
sample count, percentile method, construction latency, and the first requested
refresh latency. Keep measurements and machine details in the execution report.
Moving discovery out of construction must preserve full on-demand descendant
discovery; constructor timing alone does not prove whole-server startup latency.

## Provider skill discovery

For changes to provider visibility or the chat command menu, run the focused
capability and protocol checks from the repository root:

```sh
cargo test -p bibcode-server --lib skill -j 2
cargo test -p bibcode-server --lib provider::cursor::capabilities -j 2
cargo test -p bibcode-server --lib capability_shutdown -j 2
cargo test -p bibcode-server --lib production_runtime_covers_core_routes_assets_diagnostics_and_shutdown -j 2
cargo test -p bibcode-server --test server_settings_domain --test rpc_wire -j 2
node scripts/run-local-vp.mjs test run packages/client-runtime/src/state/server.test.ts packages/client-runtime/src/state/queryTransport.test.ts apps/web/src/components/chat/ChatComposer.test.tsx apps/web/src/components/chat/ChatComposer.rerender.test.tsx apps/web/src/components/chat/ComposerCommandMenu.test.tsx apps/web/src/components/chat/composerCapabilities.test.ts packages/contracts/src/rpcRustParity.test.ts packages/contracts/scripts/export-rust-rpc-fixtures.test.ts
```

Automated native-provider fixtures must use temporary homes, explicit fake
executables, and local HTTP fixtures, never the host's provider CLIs or user
configuration. Cursor filesystem tests must also supply an explicit temporary
home. Keep cancellation/reaping (including abandoned requests during shutdown),
configured homes (including Codex shadow homes), local/remote OpenCode directory
encoding, Claude visibility/aliases, and partial-catalog coverage alongside discovery.

For packaged visual validation on each available supported provider:

1. Open an empty repository with a known user-level skill outside that repository.
   Verify it appears under `$` for Codex or `/` for Claude, Cursor, and OpenCode.
2. Add a repository skill, then switch between two worktrees with distinct skills.
   Both retain user skills; each shows only its applicable repository skills.
   Include two concurrent chat panels and an environment switch.
3. Check configured user homes, nested/symlinked Cursor skills, and remote
   OpenCode directory selection where those setups are available. Confirm native
   disabled/non-invocable skills stay hidden and name collisions have one effective
   entry.
4. Refresh providers, change instance settings, and reconnect. Reopen a stale
   menu and confirm the catalog refreshes without losing draft text or inline
   skills. Typing in its search must not launch another discovery per keypress.
5. Make a fixture fail or return a partial catalog. Verify loading/Retry feedback,
   retained same-context results, and successful recovery; an unavailable catalog
   must not be presented as an authoritative empty list.

Record unavailable provider/platform scenarios and visual evidence in the
execution report. Hermetic tests do not establish a packaged native visual pass.

Run `opencode --version` with the desktop's `PATH` first and record it: OpenCode
1 prints `1.x.y`, OpenCode 2 prints `opencode v2.x.y`. With OpenCode 2 the
chat scenarios (messages, permissions, questions, Stop), subagent activity, the
model, agent, command and skill catalogs, checkpoint revert with its
conversation rewind, and observed provider terminals all apply; an observed
OpenCode 2 terminal runs `opencode --server … --session …` instead of
`opencode attach`, and an unobserved one (Chat Activity off) runs
`opencode --standalone` with the selected model. The focused checks are
`cargo test -p bibcode-server --lib -j 2 -- opencode`, which replays responses
captured from a real OpenCode 2.0.29 server, and
`cargo test -p bibcode-server --test provider_terminal_supervisor -- opencode`.
The opt-in `live_opencode_2` tests (in the library and in
`provider_terminal_supervisor`) drive a real OpenCode 2 binary or server. The
doc comments of `live_opencode_2_round_trip`, `live_opencode_2_local_inventory`
and `live_opencode_2_terminal_observation` list the environment they need; the
server's default model must be a scripted local endpoint so no provider credits
are spent.

## Revision and worktree preflight

Use GitHub CLI for GitHub metadata and Git for worktree/revision operations.
A typical read-only preflight is:

```sh
git status --short
git branch --show-current
git rev-parse HEAD
gh repo view --json nameWithOwner
gh api "repos/OWNER/REPOSITORY/git/ref/heads/BRANCH"
git fetch origin BRANCH
git rev-list --left-right --count "origin/BRANCH...HEAD"
```

Replace the uppercase input tokens for the current execution. Quote branch
names when the shell requires it. Fast-forward only a clean worktree and only
when the request authorizes updating it. Never force-checkout, reset, rewrite,
or overwrite unrelated changes to reach a requested revision.

For every required commit:

```sh
git merge-base --is-ancestor REQUIRED_COMMIT HEAD
```

Stop when a required revision or expected version is absent. Do not test an
older substitute or recreate a missing change from memory. Record local HEAD,
remote HEAD, merge base, ahead/behind counts, required ancestry, and the version
sources used by the affected packages.

## Source-of-truth audit

Trace the feature from public behavior to its owner before selecting tests.
Inspect:

- package scripts and workspace manifests;
- relevant source, schemas, persisted formats, and public contracts;
- focused unit and integration tests;
- `.github/workflows/ci.yml`, native desktop smoke, and release workflows;
- [repository scripts](../reference/scripts.md);
- [CI quality gates](../operations/ci.md); and
- [release process](../operations/release.md) for native artifact support.

Check platform boundaries explicitly: filesystem identity, path spelling,
process ownership, environment presentation, desktop bridge operations,
provider availability, network trust, cancellation, restart, duplicate
delivery, partial streams, and cleanup.

## Focused tests

### Ordered terminal input

When changing terminal input scheduling, request admission, lease ownership, or
reattachment, run the contracts, shared-runtime, renderer, and Rust owner seams:

```sh
vp test run packages/contracts/src/terminal.test.ts packages/contracts/src/environment.test.ts packages/contracts/src/rpc.test.ts packages/contracts/src/rpcRustParity.test.ts
vp test run packages/client-runtime/src/state/terminalInput.test.ts packages/client-runtime/src/state/orderedTerminalInput.test.ts packages/client-runtime/src/state/terminalSession.test.ts packages/client-runtime/src/state/terminalCommands.test.ts packages/client-runtime/src/rpc
vp test run apps/web/src/components/ThreadTerminalPanel apps/web/src/state/terminalSessions apps/web/src/components/status-bar
cargo test -p bibcode-server --lib terminal:: -j 2
cargo test -p bibcode-server --lib rpc::session::tests -j 2
cargo test -p bibcode-server --test production_server_terminal_rpc -j 2
```

Root runs leave these web tests uncompiled: confirm
[compiler-sensitive](./README.md#web-unit-tests-and-the-react-compiler)
happy-dom files from `apps/web` with `vp test run --project unit` and the same
paths without the `apps/web/` prefix.

Use delayed acknowledgements to prove that multiple ordered frames are sent
before the first reply while the legacy path remains serialized. Include two
input callers, Unicode paste boundaries, connection-wide saturation with
persistent subscriptions, control admission, missing/duplicate frames, a late
old begin issuing after a newer attachment, dropped responses, and disconnect
or process replacement while input is queued. Uncertain input must never replay.
Also verify open-after-close and restart followed by immediate programmatic
writes in both ordered and legacy modes, before a renderer has attached.
`production::server_terminal::input_admission_tests` (inside the `terminal::`
filter) proves that frames are acknowledged while the SQLite worker is busy,
that workspace loss and removal seal the input session, and that a changed
workspace re-admits it without interrupting input.

On a disposable native terminal, verify **Reconnect input** after a controlled
delivery failure preserves the running process and resumes only newly entered
text. Hiding or moving a terminal panel must preserve its binding. Exercise a
server without `terminalOrderedInput` to prove the negotiated legacy path.
Record network round-trip, client queue, and paint measurements separately;
an improved simulated queue time is not proof of an improved live network.

### Terminal size and reply ownership

When changing terminal attachment, sizing, or query replies, also run:

```sh
vp test run packages/contracts/src/terminal.test.ts packages/contracts/src/environment.test.ts packages/contracts/src/rpcRustParity.test.ts packages/contracts/scripts/export-rust-rpc-fixtures.test.ts
vp test run packages/client-runtime/src/state/terminalTranscriptRuntime.test.ts packages/client-runtime/src/state/terminalAttachAdapter.test.ts
vp test run apps/web/src/components/ThreadTerminalPanel apps/web/src/components/terminalSizePolicy.test.ts apps/web/src/components/terminalSizing.test.ts apps/web/src/components/terminalReplyGuard.test.ts apps/web/src/components/terminalOutputSink.test.ts apps/web/src/components/ChatView.hooks.test.tsx apps/web/src/centerTerminalActions.test.ts
cargo test -p bibcode-server --lib terminal -j 2
cargo test -p bibcode-server --test production_server_terminal_rpc -j 2
```

Root runs leave these web tests uncompiled: confirm
[compiler-sensitive](./README.md#web-unit-tests-and-the-react-compiler)
happy-dom files from `apps/web` with `vp test run --project unit` and the same
paths without the `apps/web/` prefix.

Open the same disposable terminal in two windows with different sizes, including
a desktop host and remote browser when available. Use tmux and a supported
provider TUI (Claude Code for the original attach regression). The first attach
owns an unclaimed terminal even without document focus. On Windows, verify that
live startup cursor-position/device queries (for example from PowerShell) are
answered immediately, before any resize round trip. Include DSR/DA in startup
history: a new PTY with no previously attached windows gets one history reply
pass. Later/concurrent windows and cached remounts must not answer it again.
Restart with windows still attached: startup queries are answered live, and an
additional unfocused window must neither resend the replies nor take ownership.
Restart without attached windows: one new history reply pass is allowed.
Reopen an exited terminal (for example by rerunning a script) while a window is
still attached: the next newly attached window answers the startup queries
once, and later windows do not answer them again.

A lone window and an equal-size mirror must show no size notice. A
different-size mirror explains that it is **Sized for another window** and
offers **Fit to this window**. Dismiss the notice with its labeled button using
pointer and keyboard. It must remain hidden through layout changes, unchanged
size events, hide/show, tab visibility changes, and panel remounts while
size/owner remain the same. A changed applied size/owner makes it eligible to
reappear. Closing the terminal and creating a new one must not inherit its
dismissal.

Resize the mirror's container: it must not resize the program. Focus, click, or
type in a mirror, including an equal-size mirror, to transfer ownership. After
a click or keypress, the notice must stay hidden without flashing back while
the terminal catches up. Try **Fit to this window** on an exited terminal,
then reopen or restart it: the window must remain usable, with the notice and
fit action available when another window owns a different size.
Close or disconnect the owner: other windows receive a null claim, immediately
attempt a claim, and may answer queries while unowned. Repeat through reconnect
and process restart; a departed stream cannot clear the replacement's claim. An
older attach without `sizeClaim` and the general `subscribeTerminalEvents`
stream must never receive `resized`.

Check separators, right-aligned notices, and input borders in both windows:
clipping or extra space is allowed, but rendering must use the PTY width. New
opens and attaches include available fitted dimensions; unavailable geometry
must still allow opening at server defaults, followed by the owner's first fit.
Create a right-panel terminal, then exercise a controlled launch failure: the
usual error must remain visible without a stuck retry indicator. Type while
waiting for the first terminal snapshot and verify that the text reaches the
shell. Reconnect the visible terminal and verify its output stays intact with no
extra blank or flickering redraw.

In the center terminal, right-panel terminal, and a provider's terminal view,
select one line of output and copy with **Ctrl+C**, then paste with **Ctrl+V**
(**Cmd+C/V** on macOS). Verify the exact selected text reaches the destination
once. Also check **Ctrl+Shift+C/V**, and multiline paste with an application
that enables bracketed paste. Selecting text must leave keyboard focus in the
terminal and must not open the **Add to chat** menu or overwrite the clipboard.
Right-click the selected text and choose **Add to chat**; the selection must
still attach to the composer. Right-click it again and choose **Copy**; the
exact text must reach the clipboard and stay selected. With no selection,
**Ctrl+C** must interrupt the running terminal program. Start a CLI that enables
any-motion mouse tracking (for example `codex` or `opencode`), drag across its
output, release, and move the pointer over the terminal for a few seconds; the
selection must stay. Then copy with **Ctrl+C** (**Cmd+C** on macOS) and with
right-click **Copy**; the selected text must reach the clipboard without
interrupting the CLI. A plain click must clear the selection and hand pointer
input back to the CLI. **Shift+drag** (**Option+drag** on macOS) must reach the
CLI instead of selecting. Copy a screenshot to the client's clipboard and paste it
into `codex` or `claude` running in a provider terminal, once against a local
environment and once against a remote one: the terminal must paste a
`terminal-pastes/` path and the CLI must attach the image. A text paste must
still arrive as text. Repeat copy/paste where asynchronous Clipboard API
access is unavailable; native clipboard events must still work. These packaged
checks supplement the focused terminal and keybinding component tests.

Repeat attach, hide/show, reconnect, and reload at a bash prompt; queries from
guarded replays must not enter the command line. Test snapshots with the OSC
responder flag on and off: the owner answers color queries only when the server
does not. With a server lacking `terminalSizeOwnership`, local fit-and-resize
and live replies still work, but every history replay is guarded and
startup-history queries receive no client reply on that transitional path.
Capture native screenshots and record program/server versions in the execution
report, not this runbook.

### Durable message queue

The durable queue is shared by browser, desktop, and remote clients. Validate
server ownership, renderer behavior, packaged controls, and real provider timing
separately. When admission, promotion, steering, hold, cancellation, claiming, or
recovery changes, run the server owner and integration seams before broad gates:

```sh
cargo test -p bibcode-server --lib production::turn_delivery:: -j 2
cargo test -p bibcode-server --lib production::provider_runtime -j 2
cargo test -p bibcode-server --lib production::orchestration_rpc::tests:: -j 2
cargo test -p bibcode-server --lib queued_turn_start_records_message_without_turn_start_requested -j 2
cargo test -p bibcode-server --test orchestration --test turn_delivery_queue --test turn_delivery_recovery --test production_orchestration_rpc -j 2
cargo test -p bibcode-server --lib steer_ -j 2
cargo test -p bibcode-server --test production_provider_runtime steer -j 2
cargo test -p bibcode-server --test production_provider_runtime provider_stream_end_ -j 2
cargo test -p bibcode-server --test production_provider_runtime provider_session_exit_ -j 2
cargo test -p bibcode-server codex::runtime -j 2
cargo test -p bibcode-server --test repositories -j 2
cargo test -p bibcode-server migrations -j 2
```

Deliver a queued message at completion and let its turn run past the idle timeout;
the session must stay live until one idle timeout after that turn completes.
Likewise, end a turn while a provider subagent keeps running: the session must stay
live until about one idle timeout after the subagent finishes. After a clean server
restart, the next message must resume the same native conversation. Then make the
saved conversation unresumable (for an isolated Claude instance, delete its
transcript `<config dir>/projects/<project>/<session id>.jsonl`; for OpenCode,
delete the session on its server) and send a message: the provider must start a
new conversation instead of failing repeatedly, the thread must show "Couldn't
resume the previous <provider> conversation. Started a new one with a summary of
this thread." once, and only that message's provider input begins with the
`<bibcode_thread_context>` block.

With a fake provider withholding acknowledgement, stop the session through
workspace loss so its frozen delivery becomes uncertain, then restore the
workspace and explicitly confirm **Retry**. A deleted or cursorless matching
runtime must launch a fresh native conversation, preserve the user message and
FIFO order, and show "Sent in a new conversation with a summary of earlier
messages." under the delivered message in muted text. Verify the native
invocation starts a new session without resume, that its input begins with a
`<bibcode_thread_context>` block of the thread's earlier messages while the
timeline shows only the typed text, and capture light/dark
screenshots. A still-resumable runtime must retain its conversation and show no
new notice; conflicting runtime identities, stale attempt/state guards, steers,
and already delivered rows must not trigger a fresh launch. Nonaccepted outcomes
and unknown-reason decoding retain their existing behavior.

With OpenCode, stop the server while a message is still sending, delete that
session on the OpenCode server, and restart: the message must become uncertain
with "The provider no longer has the conversation this message was sent to, so
BiBCode can't tell whether it arrived. Retry sends it to a new conversation.",
nothing may be resent automatically, and a further restart must leave it
unchanged. An explicit **Retry** must deliver it once in a new conversation with
the muted new-conversation notice.

Send a message in a chat panel whose provider takes several seconds to start,
then close the panel's tab before the client shows it running: no turn may start
or keep running in the workspace, and **Reopen closed chat** must deliver the
message once.

Verify enqueue without a turn-start event or working projection, oldest-first
promotion once per settle, explicit Send now clearing only its row's hold,
interrupt/error holds, approval and user-input gates, and withdrawal without
message resurrection after replay. Use tied timestamps and skewed client clocks
to verify admission FIFO and matching snapshot order. Promote a message enqueued
before the prior turn's final reply: its projected and client-cached timestamps
must become the promotion time and place it after that reply, including after
projection replay. Transaction rollback must preserve the original timestamps;
steering and still-queued messages retain enqueue times. Dismissing a rejected head
must unblock its tail; dismissing sending or uncertain work must preserve the
barrier against automatic delivery before settlement. For a head refused for its
model or options, verify that it offers only **Dismiss**, later pending messages
show **Waiting for an earlier message**, and the queued head card shows
**Waiting for an earlier message** with **Send now** and **Steer** disabled and
the reason explained; dismissing the refused head releases them.
Exercise an older-client pending start
while running/starting and prove it is claimed only after ready, including
the SQLite claim boundary. Migration coverage must preserve rows and indexes;
recovery must preserve queued state, payload, mode, and existing holds without
launching a provider. A starting/connecting/running projection without a live
runtime, including after graceful shutdown, must become an error with no active
turn, settle its partial assistant messages, and hold every queued message.
For accepted-start publication, use deterministic supervisor and native-driver
gates to cover both workspace-loss orderings: settle after native acceptance
before publication, then hold an actor-owned publication and queue settlement
behind it. Repeat ready/running projection, shutdown failure, retained partial
text and queue holds, old-driver identity and wrong generation. Accepted and
one native send must survive suppression, caller/actor acknowledgement closure
and public shutdown; steers must retain their no-running-publication policy.

Also cover normal terminal completion before queued Accepted publication with
known and unknown native turn IDs and failed completion. In the opposite order,
hold Accepted between its durable running writes and require the entire terminal
batch to follow. Gate a terminal after status projection but before partial text
completion; next native admission and later deltas/completions must wait until
partial settlement and terminal activity finish. A late previous-turn terminal
conservatively suppresses running publication; a fresh admission can publish.
Exercise revision exhaustion without wraparound or reopening publication.

Use the existing SQLite/engine persistence gate to pause a real submitted core
command, then cancel/stop or restart. Drain must retain the writer through the
complete core batch before pump abort, runtime deletion or replacement. Verify
closed-fence stale writers make no runtime/session/message writes, successful
restart uses a fresh fence, and failed native shutdown leaves the old session
installed. Preserve EOF/fatal-exit partial settlement and idle-rearm controls.
A bounded native output fixture must keep draining while the actor awaits a
native control. Keep fake drivers, queued writes and tasks scoped and joined;
no actual provider, account or host credential is needed. Run the closest owner
group and adjacent provider supervisor tests before integrations:

```sh
cargo test -p bibcode-server --lib production::provider_runtime::workspace_loss_tests:: -j 2
cargo test -p bibcode-server --test production_provider_runtime --test turn_delivery_recovery -j 2
```

In light and dark themes, both restart reconciliation and workspace-loss
settlement must show "BiBCode stopped this session" above their existing
actionable explanation. A genuine provider disconnect must retain the
lost-connection title. A newer unknown error classification must decode to the
generic banner without dropping the session or live event.
Repeat startup to verify no duplicate events, then promote the head and prove
the pending start is claimable. Keep original request digests stable when
admission resolves the queued flag.

Verify steer head/running/capability admission, atomic pending/steer projection,
native steer routing with the active turn ID and durable delivery key, and
accepted-message turn attribution. Settle and provider rejection must return
unaccepted steers to queued/start with the reason in `delivery.detail`; ambiguous writes remain uncertain. Codex
recovery reconciles steer keys as for starts; Claude sending steers become
uncertain. Confirm Stop retains Codex's original active turn ID after a follow-up.
Claude content arrays containing any tool-result block, including mixed
text/image/tool-result arrays, and unrelated first text blocks must not
acknowledge steering or start a runtime turn. Test exact string, text/image,
and image-only replays for starts and steers; image-only input uses an empty
text key on registration and replay. Also test replays after the original
result/system-init continuation without allocating a new runtime turn, attributing
the message to the runtime turn current at replay. A normal delivery must be
accepted while a steer echo is still pending; correlate each echo to its own input. Turn completion must not
retire a written acknowledgement, while process exit, stream failure, and
session replacement still make an unacknowledged write ambiguous.
A message starting with a known slash command or skill must be acknowledged by
Claude's expanded replay, including multi-line arguments; an unknown command's
raw replay must also acknowledge it, and another command's replay must not. A
write with no recognisable replay must become uncertain at the acknowledgement
deadline, while a steer's deadline starts only after its turn stops taking
input. After Stop, a provider process BiBCode owns that never answers the
interrupt, or answers and never settles the turn, must be retired at the
interrupt deadline: the session is projected interrupted with its resume cursor
retained, and the next message starts a new provider process. An OpenCode
session on a configured external server is not retired: Stop returns within the
interrupt deadline and the turn stays running until that server settles it.
Interrupt/error during an in-flight steer must hold any subsequent requeue; a
late acceptance must not restore a session that has already settled. Test local
Claude settlement/interrupt during writer contention, late user echoes, and a
failed target lookup after a durable claim. Unwritten input returns to the
queue or pending; input that may have reached the provider stays uncertain.

The native Codex and Claude steer fixtures use subprocess pipes, not real
providers or sockets. They currently run on Unix; record Windows native-protocol
coverage separately. The fake-router and queue projection tests need no listener.
The full production provider-runtime suite also includes Claude hook-sink and
OpenCode HTTP fixtures that require loopback listeners. Production RPC and
subprocess crash-recovery cases need loopback sockets as well; a denied bind
is unavailable evidence and must be rerun on a capable host. A crash fixture
must reach its intentional abort boundary, rather than treating a setup panic
as a successful crash probe. Run the recovery suite in the foreground with
Cargo jobs bounded by `-j 2`; retain child diagnostics and report pipe/resource
failures without weakening delivery assertions or production deadlines.

On Unix, recovery-test children have a private re-executed monitor group and a
parent-owned lifetime lease. Follow [the watchdog procedure](./flaky-tests.md#unix-recovery-test-watchdog)
for `turn_delivery_recovery` on native Linux and native macOS: parent SIGKILL
and SIGINT must remove handshake-proven child/grandchild/group identities while
an owned peer survives. Verify raw child status, streamed binary diagnostics,
normal root exit with descendant-held pipes, startup/protocol failure, original
absolute deadlines, and monitor reaping. This is test-fixture ownership;
production `ProcessRunner` behavior and Windows's direct helper are separate.
Descendants that leave the group are excluded and retain an explicit fixture
cleanup owner. Record paired base/change latency and reliability evidence;
existing macOS desktop CI alone does not run this server integration target.

For web queue behavior, run the focused renderer seams and then the web gates:

```sh
vp run --filter @bibcode/web test ChatView ChatComposer MessagesTimeline QueuedMessage restoreQueuedMessage queuedMessageCache Sidebar.logic keybindings
vp run --filter @bibcode/web test
vp check
vp run typecheck
```

For the desktop fixture support and the packaged queue scenario, use the current
package scripts:

```sh
node scripts/run-local-vp.mjs test run apps/desktop/e2e/support/test-project.test.ts apps/desktop/e2e/support/provider-input-log.test.ts
vp run test:ui:desktop:build
vp run test:ui:desktop
```

Follow the native runbook to select the worktree-built `BIBCODE_E2E_APP_PATH`
and, on Linux, for the headless `xvfb-run` invocation and its Wayland-socket
guard.
The default suite includes `composer-message-queue.e2e.ts`. To isolate it, set
`BIBCODE_E2E_SPEC=./specs/composer-message-queue.e2e.ts` for the test invocation
using the native shell's environment syntax, then clear that selection before
the full suite. The Codex fixture keeps a `[[slow]]` prompt running until a
turn-specific file marker releases it, with a bounded timeout for failed runs.
The scenario releases it only after reload assertions. Support tests cover
steer/start log discrimination, turn identity, release, timeout, and interrupt
cleanup without requiring provider accounts or listeners.

The native composer fixture must advertise each user-invocable Claude skill in
both the `initialize.commands` catalog and `reload_skills.skills`. The server
filters out reload-only model skills, so adding a fixture skill only to the
reload response cannot make it appear in the native slash menu. Run
`node scripts/run-local-vp.mjs test run apps/desktop/e2e/support/provider-shims.test.ts`
to check the generated provider protocols before the six-platform packaged UI
matrix. Keep the menu-group and exact native-input assertions in that matrix.
Cursor's fixture profile must include the generated skills in its native
compatibility roots, including `.claude/skills` as well as `.cursor/skills`.
The fixture-files regression checks that union; keep the exact native menu
inventory assertion so omitted or unexpected skills still fail qualification.

The packaged scenario must show two Queued cards in FIFO order below the working
row while the provider log still contains only the initial start. Steer the head
and verify exactly one `kind: "steer"` log entry for its text and the original
turn ID. Cancel the remaining card and verify its text returns to the composer.
Queue another message, refresh the document with the fixture's native page-load
helper (`browser.refresh()`), and verify the same card ID returns before any
new provider input. Release the slow turn, then require exactly one
`kind: "start"` entry for the promoted text with a new turn ID and no queued
cards. Retain `provider-input.jsonl`, the named `message-queue-*.png` screenshots,
and any failure screenshot/page source from the artifact directory. This stub
scenario proves the packaged controls; record real-provider behavior separately.

Observe queued-card FIFO by the displayed cards' vertical screen positions and
durable IDs. The virtualizer positions its pooled slots before it reorders the
DOM, so a WebDriver collection's DOM index is not the visible queue order.
The existing bounded wait must require the expected count, visible distinct
positions, and matching prompt order before retaining IDs; a visually reversed
queue must still fail. Keep the working-row placement, head-only Steer, Cancel,
reload identity, and exact provider-input checks.

On a host that can start the development server, use `vp run dev` and perform
the following live acceptance on both Codex and Claude, capturing and viewing
screenshots:

1. Queue two messages while a turn runs. Verify FIFO cards below the working
   row, an empty composer after each Enter, and no new Working transition.
2. Let the turn settle normally. Only the first queued message starts; the
   second waits for that new turn to settle. The promoted timestamp places the
   message after the preceding reply.
3. Steer the head. Observe Codex `turn/steer` with the active turn ID. Claude
   consumes a user line at the next tool boundary, or immediately after a
   tool-free reply as the next Claude turn. Its matching text echo may include
   attachments and arrive after completion; tool-result lines cannot acknowledge
   it. Verify attribution to the runtime turn current at acknowledgement.
4. On Cursor, Grok, and OpenCode where available, verify the explanation that
   steering is unsupported and confirm automatic sending still works. Record
   unavailable providers explicitly.
5. Cancel a queued card with another client connected: it disappears in both,
   while only the cancelling client's draft receives its text and locally cached
   attachments. Preserve an existing draft and report unavailable or excess files.
6. Stop with multiple queued messages: return their text in order before
   interrupting, preserve existing draft text, and verify no automatic send.
   Failed cancellations remain visible and held after the interrupt.
7. Reload mid-queue, then separately restart the server mid-queue. Both preserve
   queued state and FIFO order. After restart, the abandoned active session
   becomes an error and the held head shows **Waiting for you**; **Send now**
   must start it. Report unavailable attachment restoration after reload or
   from another client without losing text.
8. Hold a pending approval or question and verify no automatic sending. After
   interrupt and error settles, verify **Waiting for you**, persistent holds
   through ready, and explicit **Send now** starting only the head.
   End the provider child mid-turn and verify the next composer send or
   **Send now** launches a replacement driver with its saved native resume
   cursor and delivers. Automated fixtures must cover both EOF and the native
   `turn.completed failed` followed by `session.exited` while the event sender
   remains open; the latter must not overwrite the existing error or project
   another failed turn. Also cover an exit with an active turn still to settle,
   and exclude OpenCode's explicit-stop notice from fatal-exit recovery.
   A send during terminal-event projection must wait for
   terminal projection to settle. In automated coverage, hold that projection
   and verify the five-second deadline releases other threads' supervisor
   controls. Starts on the affected thread must remain definitely-not-sent and
   retryable, steers must be rejected, and all partial assistant messages must
   settle once persistence resumes. A dead driver's shutdown error must not
   block start recovery.
   A steer against the dead session must be
   rejected/requeued without a replacement launch.

Also press Enter while the first delivery is pending/sending: the next prompt
must queue and clear the composer. Repeated Enter during the same client request
preserves the unsent draft without duplication. Check pending/sending
**Steering…**, visible rejection detail, and `Mod+Shift+Enter` without a newline.
Review changed components/hooks against the React guidance and `UI.md`.

If the environment cannot launch the server, packaged app, listener, or provider
subprocess fixture, record that evidence as unavailable with its exact blocker
and assign the missing run to a capable host. Never infer a packaged or native
provider pass from static checks or unit tests.

### Selecting other focused coverage

For the Pull Requests web shell/detail/review, settings, and auth-host display,
and the Pull Requests client runtime and contracts, run:

```sh
vp test run apps/web/src/pullRequestsStore.test.ts apps/web/src/components/pullRequests apps/web/src/components/Sidebar.test.tsx apps/web/src/components/settings apps/web/src/components/gitManager/provider apps/web/src/components/ui/permission-button.test.tsx packages/shared/src/sourceControl.test.ts packages/shared/src/git.test.ts packages/client-runtime/src/state/pullRequests.test.ts packages/contracts/src/pullRequests.test.ts packages/contracts/src/sourceControl.test.ts
vp run check:contracts
vp run typecheck
vp check
cargo test -p bibcode-server discovery -j 2
cargo test -p bibcode-server --test git_rpc source_control_discovery_uses_structured_bounded_probes -j 2
```

Root runs leave these web tests uncompiled: confirm
[compiler-sensitive](./README.md#web-unit-tests-and-the-react-compiler)
happy-dom files from `apps/web` with `vp test run --project unit` and the same
paths without the `apps/web/` prefix.

Auth schema changes also require regenerated RPC fixtures and contract parity
checks. A sandbox denial of TCP binding is a blocked validation result; record
the exact error and rerun the affected target on a capable host. Do not report
blocked targets as passed or skipped.

For Pull Requests server context, vocabulary, list, detail/tab reads, permissions,
review/edit/merge/state mutations, checkout, or host-runner changes,
run the owner and RPC boundaries from the repository root:

```sh
cargo test -p bibcode-server pull_requests -j 2
cargo test -p bibcode-server --test pull_requests_context_list -j 2
cargo test -p bibcode-server --test pull_requests_detail_reads -j 2
cargo test -p bibcode-server --test pull_requests_review_actions -j 2
cargo test -p bibcode-server --test pull_requests_edit_merge_actions -j 2
cargo test -p bibcode-server --test pull_requests_checkout -j 2
cargo test -p bibcode-server --test rpc_wire -j 2
cargo test -p bibcode-server registers_every -j 2
```

For provider detection of self-hosted hosts (the recorded host observation,
remote parsing, status classification, and the create action's provider check
before anything is published), also run:

```sh
cargo test -p bibcode-server source_control:: -j 2
cargo test -p bibcode-server --lib only_an_explicit_source_control_discovery_records_hosts -j 2
cargo test -p bibcode-server status_provider -j 2
cargo test -p bibcode-server create_pr -j 2
cargo test -p bibcode-server commit_push_pr -j 2
```

The inline CLI-script integration fixtures currently run on Unix. Shared
parser, model, error, capability, deadline, and registration tests cover
platform-independent behavior; native Windows CLI and private-file ACL
execution still require native evidence. Verify custom-host discovery, that a
recorded host skips discovery until Rescan, `unknown_host` or
`not_authenticated` forgets it, that only an explicit Settings scan
(`recordHosts`) replaces recorded hosts, that full GitLab logout reports
Unauthenticated and clears its recorded hosts while unrecognized output keeps
them with Unknown status, that a request created through
`git.runStackedAction` makes the next list read its totals again,
host/repository pinning, no-origin and auth failures, pagination, optional
totals and their 30 s reuse, bounded cancellation, and private body-file cleanup.
For both context and list, verify a failed login cancels and awaits the provider child instead
of waiting for the page-read deadline, retains login-error precedence, and
rechecks a checkout removed while reads were in flight. For detail reads,
verify the permission/readiness tables and server-authored reasons, GitLab
version gates, reviewer-state mappings, opaque timeline ids and line positions,
native GitLab `appliable` suggestions and observed note/body reactions,
GitHub inventories beyond 100 files, nullable check timings and check grouping,
diff refs, binary rows, 1 MiB
per-file omission, 8 MiB whole-patch truncation, and page caps. Verify unknown
merge/check/reviewer states remain conservative and no live host calls occur
in fixture tests. GitLab patch-bearing JSON uses the patch budget; ordinary
metadata JSON retains the smaller cap. Exercise both adapters through the
unary RPC service and verify context is read once for detail. The RPC transport
cases require TCP listeners; a denied bind is blocked evidence with the exact
error, never a pass or skip. The module has no idle provider polling or logging
of host/repository/user content. Dedicated packaged UI validation belongs with
the later UI implementation.

For edit/merge/state mutations, exercise every supported action on each host with
recording CLI fixtures. Verify numeric GitHub milestone IDs resolve to titles,
GitLab people edits preserve the full current-plus-add-minus-remove list with one
lookup per retained login, and an empty edit does not open an interactive CLI.
Concurrent edits of one request from different checkouts must wait before reading
fresh metadata; cancellation must remove queued work without a host action while
another request remains independent. Verify immediate GitLab merges explicitly
disable the CLI's default auto-merge mode.
Merge must reject missing/blank or stale heads, denied permissions, and disallowed
methods without any mutation call after the fresh pre-check reads. Exercise normal,
bypass, and auto-merge permission paths, exact head-pinning flags, GitLab's GraphQL
override before merging, and private-file REST merge messages. Verify the returned
`autoMergeEnabled` and nullable `mergedSha`, and GitHub deletion's unavailable reason.
GitLab revert must create the target-based branch, revert the known merge/squash
commit, then create the MR through the existing creation service, with all three
calls using the pinned runner and private body transport. Fail each step and verify
later calls do not run, body files are removed, and errors preserve their code and
describe any completed steps. An override followed by a failed merge must likewise
report the applied override. Validate new GraphQL documents against a disposable,
authorized live host separately; fixture acceptance is not live API evidence.

For review mutations, record exact CLI argv, pinned host/repository, stdin JSON
and body-file bytes, 0600 mode at invocation, and removal after success/failure.
Check all supported action routes and unavailable reasons; reject malformed
opaque ids without mutation. A fresh pre-check denial or stale head must start
no mutation process. Exercise reaction and resolution no-ops, reaction removal
for the authenticated viewer only, GitLab user-id lookup before re-request,
GitHub atomic comment-related 422 rejection, pinned GitLab diff-version paths
and both coordinates for context lines (including renamed files), missing-line
draft retention, and GitLab per-comment failure
continuation plus later summary/event failures with posted counts. Review
success must not start a server reread. Validate new GraphQL documents against
the intended hosts separately from recording-stub tests and report blocked
network/listener execution with the exact error. GitLab multi-line review
comments currently use the documented end-line fallback.

For supervised process runner changes (spawn, deadline, cancellation, drop,
or process-tree cleanup), run the owner and the inline Git RPC boundaries:

```sh
cargo test -p bibcode-server --lib process:: -j 2
cargo test -p bibcode-server --test process_runner -j 2
cargo test -p bibcode-server --test production_git_manager_rpc -j 2
cargo test -p bibcode-server --test production_git_vcs_rpc -j 2
```

Keep separate evidence that a dropped, aborted, or runtime-shutdown run leaves
no descendant alive; that interrupting an inline Git read, or closing its
socket, closes the transport helper's connection to a stalled remote; and that
a cancelled run still stops a descendant that outlives its root. On Linux the
subreaper unit test re-executes itself to prove the killed helper ended by
`SIGKILL` and the dropped run's root was reaped. On Windows the same drop
terminates the run's Job, with kill-on-close as the backstop; record native
Windows results for these cases rather than inferring them from Unix.

Run the closest behavioral coverage before broad suites. Discover exact Rust
targets and filters from manifests and `cargo test -- --list`; do not invent
test names. When concurrency matters, run the affected owner at its default
harness width and at the repository's relevant explicit parallel widths.

Use `vp test` for the built-in Vite+ test command. Use `vp run test` only when
the workspace package-script graph is required. When a globally installed `vp`
could shadow the checkout's `node_modules/vite-plus` (native Windows hosts and
Parallels guest execution), run `node scripts/run-local-vp.mjs test ...` with
the same arguments; it executes the workspace copy so exactly one Vitest runtime
loads, and it fails with an install instruction when dependencies are missing.
Exact subprocess tests may select a single thread only when the subprocess
intentionally owns isolated process-global state, as documented in the
repository scripts reference.

Root `vp test` runs web tests without the React Compiler; see
[Web unit tests and the React Compiler](./README.md#web-unit-tests-and-the-react-compiler).

A focused suite must cover the changed success behavior and its material
failure, cancellation, retry, restart, and cleanup seams. For cross-platform
logic, include host-independent fixtures for every affected platform.

### Native release distribution evidence

For every desktop target, build and install the native artifact, inspect the installed
executable architecture, run the packaged UI suite, retain bounded screenshots/logs, and
exercise a seeded update. The required matrix is macOS ARM64/x64, Linux ARM64/x64, and
Windows ARM64/x64. A cross-compiled or emulated executable is compatibility evidence, not
a native pass.

For every standalone server target, inspect the native executable, run `bibcode
--version`, start the staged distribution without `--static-dir`, fetch `/`, read the
environment descriptor, issue and exchange pairing material, then terminate and prove
cleanup. Linux additionally installs and removes both package formats in native
containers:

| Format | Required systems                      |
| ------ | ------------------------------------- |
| `.deb` | Ubuntu 22.04, Ubuntu 24.04, Debian 12 |
| `.rpm` | Rocky Linux 9, Fedora 44              |

Package removal must delete `/usr/bin/bibcode` and package-owned documentation/web
assets while preserving a test sentinel in the isolated BiBCode data root. Record the
archive/package name, reported architecture, version, startup result, pairing result,
shutdown result, and evidence artifact location in the execution report.

### Grant-driven remote sharing

When pairing-offer generation, grant reach metadata, desktop exposure, or
client revocation changes, validate the complete ceremony on every supported
native desktop: **Another device** widens before minting and shows one pairing
code as a deep link, browser URL, and QR code; **This computer only** and every
**Custom address** remain externally managed and never widen; a bind or firewall
failure mints nothing; and revoking the final native-managed **Another device**
offer or client returns the server to loopback. Confirm an off-host custom grant
remains visible in the off-host count but leaves authoritative desired exposure
loopback before and after session exchange.
Also verify browser/headless presentation remains read-only for exposure while
server-side mint and revocation stay available. Windows additionally owns the
program-scoped firewall evidence in its platform runbook; Linux and macOS do
not substitute a firewall assertion.

Capture visual evidence for all exposure-compensation outcomes, using a
test-owned server/profile and controlled failure injection where required:

- a mint failure after widening plus successful cleanup shows **The offer was
  not created. Remote access is confirmed local-only.**, and native state is
  local-only;
- a concurrent live grant shows that remote access remains enabled for another
  live access reason;
- a failed or blackholed cancellation still runs authoritative cleanup when
  this attempt widened: loopback authority narrows, while a possibly live offer
  or another live reason keeps the host wide;
- cancellation and authoritative cleanup that are both unconfirmed do not
  claim local-only restoration; and
- failed cleanup says topology could not be verified;
- consuming an off-host offer through the browser keeps exposure wide, and
  revoking that last browser session causes one local-only restart; and
- when a new off-host grant appears during narrowing, the one post-narrow read
  causes one compensating widen and the resulting offer remains reachable.
- a blackholed pairing-offer create response reaches the five-second attempt
  deadline, completes the bounded retry/cancel path, and reports either verified
  local-only cleanup or explicit cancellation/cleanup failure instead of
  remaining indefinitely in the generating state.
- a prior network-accessible native configuration with only legacy grants starts
  local-only and widens only after **Resume legacy remote access** is selected;
- unmounting the reconciler or switching topology before its first privileged
  apply cancels that work; after a local-only apply commits, unmount the view and
  prove the mandatory authoritative refetch and one needed compensating widen
  still complete.
- a failed background reconciliation retries at five-second intervals for at
  most three passes, then surfaces the warning toast instead of silently waiting
  for another authority revision.

Record the exposure mode, grant/session row, restart boundary, visible message,
and screenshot for each outcome. Do not use a later app restart as substitute
evidence for direct failed-mint cleanup.

Run the living-document contract plus the public WebSocket lifecycle and
transport-size tests from the repository root whenever this boundary changes:

```sh
vp test scripts/remote-architecture-contract.test.ts
vp test packages/contracts/src/rpc.test.ts packages/client-runtime/src/rpc/session.test.ts packages/client-runtime/src/connection/pairingAdd.test.ts packages/client-runtime/src/connection/registry.test.ts packages/client-runtime/src/state/remoteUpdates.test.ts apps/web/src/hostedPairing.test.ts apps/web/src/routes/pair.test.tsx apps/web/src/routes/settings.remote-servers.test.tsx apps/web/src/connection/databaseHealth.test.ts apps/web/src/components/ConnectionDatabaseRecoveryDialog.test.tsx apps/web/src/state/shareExposureReconciler.test.tsx apps/web/src/components/settings/remote-servers/shareOffer.test.ts
cargo test -p bibcode-server --test auth_http plain_websocket_connected_state_tracks_the_completed_upgrade_lifecycle -- --exact
cargo test -p bibcode-server --test auth_http plain_websocket_rejects_a_single_frame_larger_than_16_mib -- --exact
cargo test -p bibcode-server --test auth_http auth_routes_include_browser_cors_and_preflight_headers -- --exact
cargo test -p bibcode-server --test e2ee_ws oversized_pre_auth_websocket_message_is_rejected -- --exact
cargo test -p bibcode-server --test e2ee_ws preauth_peer_connection_cap_rejects_the_fifth_connection -- --exact
cargo test -p bibcode-server --test e2ee_ws preauth_loopback_forwarder_bypasses_public_peer_cap_but_keeps_burst_limit -- --exact
cargo test -p bibcode-server --test e2ee_ws established_capacity_is_partitioned_by_principal_and_released_on_close -- --exact
cargo test -p bibcode-server --test e2ee_ws inbound_plaintext_capacity_backpressures_by_principal_and_releases_on_close -- --exact
cargo test -p bibcode-server --test e2ee_ws incomplete_authenticated_message_closes_after_ten_seconds_without_progress -- --exact
cargo test -p bibcode-server --test e2ee_ws idle_authenticated_connection_has_no_reassembly_deadline -- --exact
cargo test -p bibcode-server --test e2ee_ws pairing_bootstrap_inside_the_channel_serves_get_config -- --exact
cargo test -p bibcode-server --test e2ee_ws off_host_pairing_requires_confirmation_even_without_a_client_flag -- --exact
cargo test -p bibcode-server --test e2ee_ws delivered_pairing_session_stays_pending_until_confirm_rpc -- --exact
cargo test -p bibcode-server --test e2ee_ws closing_before_confirm_revokes_the_pending_session -- --exact
cargo test -p bibcode-server --test e2ee_ws confirmed_pairing_session_survives_disconnect_and_restart_cleanup -- --exact
cargo test -p bibcode-server rpc::e2ee::tests::preauth_admission_partitions_slots_and_refills_peer_tokens --lib -- --exact
cargo test -p bibcode-server rpc::e2ee::tests::preauth_network_keys_canonicalize_ipv4_24_and_ipv6_64 --lib -- --exact
cargo test -p bibcode-server rpc::e2ee::tests::one_public_subnet_cannot_consume_more_than_half_the_global_pool --lib -- --exact
cargo test -p bibcode-server rpc::e2ee::tests::loopback_forwarder_can_use_global_capacity_without_the_public_peer_cap --lib -- --exact
cargo test -p bibcode-server rpc::e2ee::tests::missing_connect_info_uses_the_strict_unspecified_bucket --lib -- --exact
cargo test -p bibcode-server rpc::e2ee::tests::unrelated_public_networks_still_stop_at_the_global_cap --lib -- --exact
cargo test -p bibcode-server rpc::e2ee::tests::idle_preauth_peer_and_network_entries_are_pruned_without_exceeding_the_map_cap --lib -- --exact
cargo test -p bibcode-server rpc::e2ee::tests::inbound_empty_continuations_and_excessive_fragmentation_are_rejected --lib -- --exact
cargo test -p bibcode-server rpc::e2ee::tests::inbound_global_pressure_waits_for_capacity_instead_of_closing_the_victim --lib -- --exact
cargo test -p bibcode-server rpc::e2ee::tests::inbound_global_pressure_waits_past_five_seconds_and_resumes --lib -- --exact
cargo test -p bibcode-server rpc::e2ee::tests::incomplete_minted_session_delivery_is_compensated --lib -- --exact
cargo test -p bibcode-server rpc::e2ee::tests::outbound_logical_message_accepts_progress_across_record_deadlines --lib -- --exact
cargo test -p bibcode-server rpc::e2ee::tests::outbound_logical_message_rejects_a_stalled_record --lib -- --exact
cargo test -p bibcode-server rpc::e2ee::tests::outbound_logical_message_enforces_the_size_derived_total_deadline --lib -- --exact
cargo test -p bibcode-server rpc::e2ee::tests::completed_messages_retain_their_global_buffer_budget --lib -- --exact
cargo test -p bibcode-server rpc::session::tests::fit_first_budget_does_not_block_small_waiters_behind_an_aged_large_waiter --lib -- --exact
cargo test -p bibcode-server rpc::session::tests::cancelled_weighted_waiter_is_removed_and_capacity_is_refunded_once --lib -- --exact
cargo test -p bibcode-server rpc::session::tests::outbound_connection_wait_uses_the_same_absolute_deadline_as_process_wait --lib -- --exact
cargo test -p bibcode-server rpc::session::tests::two_tier_admission_does_not_hold_connection_bytes_while_process_bytes_are_blocked --lib -- --exact
cargo test -p bibcode-server rpc::session::tests::two_tier_admission_observes_release_between_probe_and_notify_poll --lib -- --exact
cargo test -p bibcode-server rpc::session::tests::inbound_guard_is_released_after_dispatch_not_handler_completion --lib -- --exact
cargo test -p bibcode-server rpc::session::tests::slow_socket_cannot_hide_more_than_one_large_response_in_the_session_queue --lib -- --exact
cargo test -p bibcode-server rpc::session::tests::slow_sockets_share_one_process_outbound_plaintext_budget --lib -- --exact
cargo test -p bibcode-server rpc::session::tests::response_larger_than_the_connection_budget_fails_the_session_closed --lib -- --exact
cargo test -p bibcode-server rpc::session::tests::byte_and_queue_admission_share_one_five_second_deadline --lib -- --exact
cargo test -p bibcode-server auth::service::tests::completed_pairing_offer_replays_and_cancels_after_restart --lib -- --exact
cargo test -p bibcode-server auth::service::tests::pending_pairing_offer_can_be_cancelled_after_restart --lib -- --exact
cargo test -p bibcode-server auth::service::tests::pending_pairing_offer_recovers_for_retry_after_restart --lib -- --exact
cargo test -p bibcode-server auth::service::tests::live_service_does_not_recover_another_services_young_pending_offer --lib -- --exact
cargo test -p bibcode-server auth::service::tests::remote_offer_cancellation_converges_dormant_share_state_and_access_events --lib -- --exact
cargo test -p bibcode-server auth::service::tests::cancelled_guard_registration_releases_bookkeeping_while_persistence_is_queued --lib -- --exact
cargo test -p bibcode-server auth::service::tests::cancelled_pending_session_issuance_revokes_durable_commit_before_state_publication --lib -- --exact
cargo test -p bibcode-server auth::service::tests::pending_pairing_sweep_only_revokes_sessions_past_the_grace_window --lib -- --exact
cargo test -p bibcode-server --lib keeps_one_service_watcher -- --nocapture
cargo test -p bibcode-server auth::service::tests::cross_service_authentication_starts_watcher_for_the_cached_session --lib -- --exact
cargo test -p bibcode-server --test repositories pairing_offer_reservations_enforce_the_shared_ -- --nocapture
cargo test -p bibcode-server --test auth_http pairing_offer_authority_is_shared_across_simultaneously_live_servers -- --exact
cargo test -p bibcode-server --test auth_http concurrent_pairing_offer_retries_across_live_servers_return_one_result -- --exact
cargo test -p bibcode-server --test auth_http remote_revocation_closes_an_acked_live_stream_before_later_events -- --exact
cargo test -p bibcode-server --test repositories pending_auth_sessions_confirm_by_id_and_startup_cleanup_is_selective -- --exact
cargo test -p bibcode-desktop firewall::tests --lib -- --nocapture
cargo test -p bibcode-desktop server_exposure::tests --lib -- --nocapture
cargo test -p bibcode-desktop bridge::tests::entering_wsl_only_recovers_native_exposure_before_switching_topology --lib -- --exact
cargo test -p bibcode-desktop bridge::tests::leaving_wsl_only_restarts_the_native_topology_explicitly_local_only --lib -- --exact
cargo test -p bibcode-desktop network_interfaces::tests::advertised_endpoint_classification_fixtures --lib -- --exact
cargo test -p bibcode-desktop network_interfaces::tests::public_default_route_is_advertised_but_never_default --lib -- --exact
cargo test -p bibcode-desktop backend::tests::lan_advertised_host_accepts_only_private_usable_ipv4_defaults --lib -- --exact
cargo test -p bibcode-desktop bridge::tests::local_only_discovery_surfaces_public_only_topology_as_unavailable --lib -- --exact
cargo test -p bibcode-desktop bridge::tests::local_only_discovery_marks_private_default_candidate_unavailable --lib -- --exact
cargo test -p bibcode-server auth::service::tests::custom_off_host_grants_remain_externally_managed_after_exchange --lib -- --exact
```

The pairing-add client suite must prove that the registered supervisor is the
only bearer proof—no second pinned socket—and that an ambiguous confirmation
observes `registry.stateChanges` for at most 30 interruptible seconds. Connected
state proves activation; blocked authentication, host identity, or storage
change rolls back local writes with `pairing-rejected`; an inconclusive window
leaves recovery with the supervisor. Local persistence and confirmation remain
the only uninterruptible segment.

### Direct E2EE interop gate

When direct pairing, host identity, `/ws-e2ee`, Noise framing, or client E2EE
session preparation changes, build the current Rust server and run the opt-in
TypeScript-to-Rust interop suite:

```sh
cargo build -p bibcode-server
cd packages/client-runtime
BIBCODE_E2EE_SERVER_BIN="$(git rev-parse --show-toplevel)/target/debug/bibcode" vp test run src/e2ee/serverInterop.test.ts
cd ../..
```

The suite must mint through the real pairing endpoint, pin the persisted host
key, authenticate the pending bootstrap channel, call `server.getConfig`, prove
the delivered bearer cannot reconnect before confirmation, confirm through
`auth.confirmPairing`, then reconnect with the active in-channel credential. It
must also reassemble a fragmented request and reject a bad pairing token.
Without `BIBCODE_E2EE_SERVER_BIN`, the same file intentionally reports skipped
so ordinary `vp test` does not depend on a prebuilt binary.

### Cross-container remote-server gate

When remote authentication, pairing, E2EE, share-state derivation, revocation,
or remote updates change, run the opt-in smoke test with the server and client
in distinct Linux containers. The recipe bind-mounts the host-built
`target/debug/bibcode` into a Linux container, so it requires a Linux host with
Docker networking: a macOS or Windows host builds a native executable the
container cannot run, and Apple Container or a Docker Desktop proxy that
cannot route between containers or publish ports yields no valid client/server
pair. Detect the host first and record the gate as **unavailable evidence**
with the exact blocker when it is not a Linux host; do not change application
networking or fall back to a loopback host server to compensate. Build the
current server, verify Docker is available, and use test-owned names so
cleanup can be proven:

```sh
case "$(uname -s)" in
  Linux) ;;
  *) echo "cross-container gate unavailable: requires a Linux host binary and Docker networking ($(uname -s))" >&2; exit 2 ;;
esac
cargo build -p bibcode-server
docker version

cleanup_bibcode_remote_docker() {
  docker rm -f bibcode-remote-client 2>/dev/null || true
  docker rm -f bibcode-remote-server 2>/dev/null || true
  docker network rm bibcode-remote-stabilization 2>/dev/null || true
  docker volume rm bibcode-remote-stabilization-data 2>/dev/null || true
}
trap cleanup_bibcode_remote_docker EXIT INT TERM
docker network create bibcode-remote-stabilization
docker volume create bibcode-remote-stabilization-data

docker run -d --name bibcode-remote-server \
  --network bibcode-remote-stabilization \
  --security-opt label=disable \
  -e DEBIAN_FRONTEND=noninteractive \
  -v "$PWD/target/debug/bibcode:/usr/local/bin/bibcode:ro" \
  -v bibcode-remote-stabilization-data:/data \
  debian:trixie-slim \
  sh -c 'apt-get update >/dev/null && \
    apt-get install -y --no-install-recommends ca-certificates >/dev/null && \
    rm -rf /var/lib/apt/lists/* && \
    exec /usr/local/bin/bibcode --base-dir /data --host 0.0.0.0 --port 3773 serve'

for attempt in $(seq 1 120); do
  BIBCODE_DOCKER_PAIRING_JSON=$(docker exec bibcode-remote-server \
    /usr/local/bin/bibcode --base-dir /data pairing issue --json 2>/dev/null) && break
  sleep 0.25
done
test -n "${BIBCODE_DOCKER_PAIRING_JSON:-}"
BIBCODE_DOCKER_ADMIN_CREDENTIAL=$(node -e \
  'process.stdout.write(JSON.parse(process.argv[1]).credential)' \
  "$BIBCODE_DOCKER_PAIRING_JSON")

docker run --rm --name bibcode-remote-client \
  --network bibcode-remote-stabilization \
  --security-opt label=disable \
  -v "$PWD:/workspace:ro" -w /workspace \
  --tmpfs /workspace/node_modules/.vite-temp:rw,mode=1777 \
  -e BIBCODE_DOCKER_SERVER_URL=http://bibcode-remote-server:3773 \
  -e BIBCODE_DOCKER_ADMIN_CREDENTIAL="$BIBCODE_DOCKER_ADMIN_CREDENTIAL" \
  node:26-bookworm \
  ./node_modules/.bin/vp test packages/client-runtime/src/e2ee/dockerRemoteSmoke.test.ts

unset BIBCODE_DOCKER_PAIRING_JSON BIBCODE_DOCKER_ADMIN_CREDENTIAL
cleanup_bibcode_remote_docker
trap - EXIT INT TERM
```

The smoke test must cover descriptor negotiation, administrative token exchange,
an off-host pairing offer, pinned-host Noise NK authentication, pending
share-state retention, bootstrap-channel RPC, rejected bearer reconnect before
confirmation, idempotent `auth.confirmPairing`, and authenticated bearer
reconnect afterward. It sends a maximum-size Noise record and accepts continued
progress after more than five seconds, verifies updater status/check, a typed
manual-install failure, and a later healthy status result, and proves plain
`/ws` closes a 16 MiB-plus-one-byte frame. It also rejects a fifth silent
pre-auth socket from the real container peer, closes an authenticated connection
that exceeds the 2,048-record limit, retains exposure through a browser session,
actively closes a revoked E2EE session, and reaches final loopback share state.
Finally, it cancels an ambiguously delivered offer and proves a delayed retry
cannot recreate its grant.

One fixed-IP Node client cannot independently exercise the IPv4 `/24` fan-out
classifier, so subnet aggregation remains required focused Rust fixture/unit
evidence. The production headless container likewise has no injectable hung
desktop updater delegate or supervisor-acquisition seam; whole-update timeout and
fan-out-slot release remain required client-runtime tests, while Docker proves
result isolation. The containers disable SELinux process labeling because both
host bind mounts are read-only; this keeps the command portable on enforcing
hosts without relabeling the worktree. The client receives a narrow tmpfs for
Vite's generated config cache, so the repository itself stays read-only. It must
not spawn a host server, fall back to loopback, or print a pairing/session
credential.

After the run, all three commands below must print nothing:

```sh
docker ps -a --filter name=bibcode-remote- --format '{{.Names}}'
docker network ls --filter name=bibcode-remote-stabilization --format '{{.Name}}'
docker volume ls --filter name=bibcode-remote-stabilization-data --format '{{.Name}}'
```

Record image IDs, architecture, exact command results, and cleanup evidence in
an execution report. Never record or retain the temporary pairing credential.

### VCS coordination gates

When VCS status observation, mutation ownership, automatic fetch, or client
refresh scheduling changes, run the current focused owners before broad gates:

```sh
vp run check:contracts
vp test run apps/web/src/components/SourceControlPanel.test.tsx apps/web/src/components/files/FileBrowserPanel.test.tsx apps/web/src/components/GitActionsControl.test.tsx apps/web/src/components/Sidebar.test.tsx apps/web/src/components/ThreadStatusIndicators.test.tsx
vp test run packages/contracts/src/gitManager.test.ts packages/contracts/src/environment.test.ts apps/web/src/gitManagerStore.test.ts apps/web/src/components/gitManager scripts/privacy-contract.test.ts
node scripts/run-msvc.mjs cargo test -p bibcode-server git:: -- --nocapture
node scripts/run-msvc.mjs cargo test -p bibcode-server --lib git::manager:: -- --nocapture
node scripts/run-msvc.mjs cargo test -p bibcode-server --lib git::broadcaster::tests::ref_poll_is_replaced_by_watcher_and_safety_status_reads -- --exact --nocapture
node scripts/run-msvc.mjs cargo test -p bibcode-server --lib terminal::manager::tests::retained_process_exit_callback_does_not_hold_terminal_publication -- --exact --nocapture
node scripts/run-msvc.mjs cargo test -p bibcode-server --lib production::runtime::tests::structured_terminal_process_exit_immediately_invalidates_status_under_watcher_fallback -- --exact --nocapture
node scripts/run-msvc.mjs cargo test -p bibcode-server --lib production::runtime::tests::provider_lifecycle_and_delivery_events_do_not_trigger_git_status_reads -- --exact --nocapture
node scripts/run-msvc.mjs cargo test -p bibcode-server --test production_git_vcs_rpc -- --nocapture
node scripts/run-msvc.mjs cargo test -p bibcode-server --test git_manager_reads -- --nocapture
node scripts/run-msvc.mjs cargo test -p bibcode-server --test git_manager_commit -- --nocapture
node scripts/run-msvc.mjs cargo test -p bibcode-server --test production_git_manager_rpc -- --nocapture
node scripts/run-msvc.mjs cargo test -p bibcode-server --test git_rpc -- --nocapture
vp test run packages/client-runtime/src/state/vcs.test.ts packages/client-runtime/src/state/gitManager.test.ts packages/client-runtime/src/state/gitManagerRefresh.test.ts apps/web/src/connection/platform.test.ts apps/web/src/components/GitActionsControl.test.tsx
```

Root runs leave these web tests uncompiled: confirm
[compiler-sensitive](./README.md#web-unit-tests-and-the-react-compiler)
happy-dom files from `apps/web` with `vp test run --project unit` and the same
paths without the `apps/web/` prefix.

`vp run check:contracts` regenerates the RPC wire fixtures and ends by failing
when the regenerated tree differs from the committed one, so a contract change
must land together with its regenerated fixtures; a report records the exact
fixture files that changed rather than restoring them by hand.

On non-Windows hosts, replace the MSVC launcher with the equivalent direct
Cargo invocation. Run the production RPC file once with its default harness and
record the complete pass/fail matrix. If it exposes a causal product failure,
isolate only that case. Do not repeatedly run the file, serialize it, or change
production deadlines to conceal load/order timeouts.

For the event-driven VCS boundary, retain separate evidence for:

- the paused-time idle regression: after the initial snapshot, 59 seconds starts
  no status or `symbolic-ref` Git process, and 60 seconds starts exactly one
  local safety read without `symbolic-ref`;
- native worktree content, index, `HEAD`, packed-ref, and nested-ref events;
- watcher setup failure, root loss, overflow, sticky fallback, final release,
  reattachment, and shutdown;
- one 125 ms trailing watcher read and one immediate structured-terminal read;
- reconnect plus hidden, reveal, focus, and Git-menu explicit catch-up; and
- execution-host routing for native, WSL-direct, and SSH/server workspaces.

For the Git Manager boundary, retain separate evidence for tip-pinned paging
and generation splicing; a commit made from Changes appearing at the History
tip after switching tabs without pressing **Refresh** (the panel's refs
generation moves past the loaded page and History refetches and splices, while
a stale first page that resolves behind the loaded generation is ignored); the
shared project-then-repository lock rejecting a competing Git Manager or
catalog mutation with `operation-in-flight`;
server-authored blocked copy rendered unchanged; stream cancellation reaching
the Git child; an interrupted or disconnected inline read (such as
`gitManager.getRemoteTags` against a stalled remote) stopping Git's transport
helper; and one explicit provider refresh after an idle interval that
produced no provider process or browser network request.

In a disposable repository, damage `.git/config` before opening the first status
subscription, then repair it while that subscription remains open. Local status
must recover and the same lifecycle must attach automatic fetch, without a
restart or resubscription; repeat with a malformed `HEAD`. Confirm a subsequent
automatic-fetch interval observes the remote change. Use the broadcaster's gated
repair tests to cover rapid break/repair observations, overlapping local/full
refreshes, a subscriber departing during common-directory resolution, a new
subscriber joining, mutation epoch retirement, and final-release cancellation.
Each current subscriber must attach to one shared physical-repository owner;
departed subscribers and stale resolution results must not remain attached.

For external Git Manager refresh, keep the automatic fetch interval at its
180-second default. Default fixtures use ordinary `git init` without
`--ref-format`, so supported older Git installations can run them. Reftable
tests probe `git init --ref-format=reftable` and print a skip when unsupported;
record that skip as unavailable coverage and run on a Git installation with
reftable support before claiming reftable validation. In disposable file-ref
and reftable fixtures, run commit,
stash push and non-top `stash@{1}` pop/drop, tag,
and fetch or update-ref commands from both a companion terminal and BiBCode's
terminal while the manager stays focused. Refs, History, and the open stash pane
should converge within about one second without pressing **Refresh**. Index-only
`git add` must update Changes without an extra Git Manager signal read. Retain
coverage for unchanged signatures, retirement fencing, sticky watcher health,
and one degraded-only refetch per focus/visibility return. Also retain evidence
for sibling-worktree HEAD switches with both file and reftable storage, metadata directories created after
attachment, continuous file/index writes while a ref changes (the ref debounce
has a one-second cap), and a ref observation retried after an in-app mutation
fence settles. After external and in-app commits, exactly one loaded row should
carry the current HEAD decoration; moved/deleted refs must update older rows
without losing selection, pinned cursors, or scroll context. Compare decoration
arrays and ordering to `git log --decorate=short --format=%D` using HEAD, a tag,
`origin/main`, `origin/HEAD`, and a second branch, including after an external
ref change. Healthy watchers must not refetch these queries merely because the
window regains focus, and alt-tab alone must not probe environment supervisors.
Retain watch-plan evidence for native recursive worktree coverage, including
its `.git` tree: notify/inotify manages Linux recursion, FSEvents manages macOS
recursion, and Windows uses native subtree watching. Events from top-level
`objects/`/`lfs/` and submodule `modules/<name>/objects/` stores are filtered,
while a commit or checkout inside a submodule still refreshes the superproject.
Only external Git metadata uses non-recursive roots plus recursive
refs/logs/reftable/worktrees stores; its own object stores are not watched, and
events from linked worktrees' submodule object stores under the recursive
`worktrees/` watch are filtered. Confirm sibling index/log churn does not invalidate the
current worktree and Windows uppercase metadata paths register/classify
correctly. Exercise native overflow (every full rescan must publish a ref
invalidation after reinstallation, even on failure) and confirm that creating
hundreds of nested ref directories, as a large fetch does, starts no
registration work and no rescan.
In a main checkout, make stash push the first external ref change after startup
settles, with both an empty stash list and an existing stash reflog, then pop,
push, and drop (including a non-top entry) while Stashes stays open; a prior commit signal
must not satisfy the stash assertion. Verify one retained History batch per
external change, no duplicate same-tip offset-zero read and no retained read
when the first page covers all loaded rows, preserved selection and scroll,
older-response fencing, and actionable background errors with Retry, including
a cause without final punctuation. With the Stashes panel closed, an external
stash push must start no stash-list read and the button shows no count; opening
the panel then lists the new entry. Opening History reads the first page once; an in-app History operation
while the watcher is degraded still refreshes History, and one change never
causes two first-page reads. A stalled focus consumer must not delay supervisor
visibility wakeups, and an `application-active` wakeup must survive a following
focus return.
Exercise signature-input failures without losing remote-status publication.
For remote/local reconciliation ordering changes, run
`git::broadcaster::tests::local_error_keeps_one_pending_remote_reconcile_until_later_success`
in at least 300 separate test-process runs with `--exact --test-threads 1` and
record the pass/fail count.

Host-independent event-shape and routing tests are compatibility evidence, not
native evidence for another operating system or remote host. Record unavailable
Linux, macOS, WSL, or SSH execution separately instead of simulating it as run.

For an automatic-fetch default decision, measure a current-source server or
desktop runtime, never an installed application. Use a disposable scenario
with a recorded number of physical repositories, worktrees, and active
`subscribeVcsStatus` streams. After bootstrap work settles, verify the recorder
with a short probe, clear it, then leave the scenario idle for a real interval
of at least ten minutes. Count top-level Git launches attributed by exact root
PID and process-start identity, retain command lines when the platform exposes
them, and normalize launches per elapsed minute per physical repository. State
whether discovery, status/diff, and fetch could be distinguished; do not infer
an internal operation label that the evidence does not contain.

On Windows the maintained controller performs that complete scenario and the
production-Atom queue benchmark. The default command records a 600-second
window; the short command exercises the same build, fixture, identity, probe,
cleanup, parser, and queue paths without serving as threshold evidence:

```powershell
node scripts/measure-vcs-runtime.ts
node scripts/measure-vcs-runtime.ts --duration-ms 3000 --queue-warmups 2 --queue-samples 10
```

The controller prints its unique evidence directory and retains ready, raw Git
launch, parsed Git summary, queue summary, server log, and aggregate summary
files there. Its example build overrides inherited Cargo target configuration
with an isolated target inside that directory and consumes Cargo
`compiler-artifact` JSON to launch those exact executable paths, including a
configured target-triple directory. Pass `--output-dir` only with a new path;
the controller refuses to
overwrite an existing evidence directory. Every success or failure requests a
graceful stop after one atomic Windows snapshot binds PID, parent PID, decimal
FILETIME, and executable. Both graceful and timeout cleanup capture/revalidate
the exact child tree, terminate verified descendants leaf-first and the owned
server handle last when required, await the server, and reject survivors.

Measure foreground queue delay separately through the actual production Atom
commands. Hold a real `vcs.refreshStatus` command active, schedule a same-key
mutation command, and record from scheduling immediately before the command run
to the mutation RPC execution start. Warm the harness, collect at least 100
measured samples, report the sample count and percentile method, and compute
p95 from the sorted measured values. A synthetic scheduler without production
command wiring is not acceptance evidence.

The automatic-fetch default is 180 seconds. A future default change requires an
approved measurement gate; the current decision thresholds are more than 20
top-level Git processes per minute per physical repository or more than 250 ms
foreground mutation queue delay at p95. Update the contract/default codec, Rust
settings defaults and fallbacks, RPC fixtures, settings tests, and user-facing
reset/presentation together. Preserve live updates, bounded failure backoff,
and `0 = disabled`. Record machine-specific process counts, timings, paths, and
the decision only in the execution report.

### Worktree catalog native fleet evidence

When catalog fingerprint inputs, reuse timing, or inventory invalidation change,
run the ignored native fleet test explicitly:

```powershell
node scripts/run-msvc.mjs cargo test -p bibcode-server --lib worktree_catalog::tests::fingerprint_focus_fleet_reconciles_every_five_minutes_for_thirty_minutes -- --ignored --exact --nocapture
```

The test uses ten disposable real Git repositories, the production filesystem
fingerprint reader, and production Git inventory. It executes 18,000 Focus
refreshes over 30 minutes of paused policy time, checks every five-minute real
inventory boundary, and then proves real changed and Unknown fingerprint inputs
scan immediately. Record the native runtime and inventory counts in the
execution report; the test is ignored so this several-minute evidence workload
does not inflate ordinary server suites.

### File Manager index benchmark gate

When File Manager index phases or eager ignored-directory traversal change,
run the current-source, test-owned native Windows benchmark from the repository
root. Clear any smoke-sample override so the acceptance run collects exactly 30
cold builds and 30 immediate warm hits:

```powershell
Remove-Item Env:BIBCODE_FILE_INDEX_BENCHMARK_SAMPLES -ErrorAction SilentlyContinue
node scripts/run-msvc.mjs cargo test -p bibcode-server --lib workspace::rpc::tests::benchmark_file_manager_index_phases -- --ignored --exact --nocapture
```

The ignored test creates one unique disposable real-Git repository and completes
fixture creation and Git setup before sampling. Before every cold request it
invalidates the cached root outside the measured build; each warm request follows
the completed cold request without setup or invalidation. Every cold sample must
assert exactly
`cache_wait(miss) -> git_snapshot(build) -> ignored_walk(build) ->
directory_walk(build) -> cache_build(built)`. Every warm sample must assert only
`cache_hit(hit)`, with the physical scan count unchanged; this is the acceptance
assertion that a warm hit started zero Git work.

The output must retain the raw millisecond arrays for `cache_build`,
`git_snapshot`, `ignored_walk`, `directory_walk`, and `cache_hit`, plus
`filesystem_walk` when a fallback fixture makes it applicable. Compute p50 and
p95 with sorted nearest rank at zero-based index
`ceil(sample_count * percentile / 100) - 1`; for 30 samples the p50 index is 14
and the p95 index is 28.

Record enough fixture metadata to reproduce and reconcile the returned entry
count: tracked workload files, tracked control files, ordinary untracked files,
ordinary directory rows, ignored files, ignored directory rows, empty directory
rows, total entries, and ignored entries. Record the host model, OS/build and
architecture, CPU core/logical-processor counts, physical memory, Rust/Cargo
versions, and Git version.

Apply the lazy-loading gate literally: a separately reviewed follow-up is
required when `ignored_walk` p95 is greater than 50% of `cache_build` p95 **OR**
greater than 500 ms. Otherwise record that lazy loading remains deferred; do not
change eager tree behavior as part of the measurement task. Machine-specific
fixture paths, raw arrays, phase timings, host details, ratios, and the gate
decision belong only in the execution report, never in this living runbook.

The two `git ls-files` reads use a workspace-index-specific ten-second
post-spawn execution bound. A bound change must preserve external cancellation,
output limits, sibling settlement, and bounded filesystem fallback. Verify a
slow successful pair inside the bound and timeout fallback beyond it with:

```powershell
node scripts/run-msvc.mjs cargo test -p bibcode-server --lib workspace::search::tests::git_snapshot_accepts_slow_success_inside_bound_and_falls_back_beyond_it -- --exact --nocapture
```

Then run `workspace_rpc` twice at its default harness width. Isolate the known
watcher burst-coalescing assertion if it fails, but do not serialize the file,
weaken exact Git classification, or widen the shared Git runner.

## Workspace and static gates

Run broad owners sequentially so one Cargo process owns the shared build
directory at a time:

```sh
vp run -r --concurrency-limit 1 test
cargo test --workspace -j 2 -- --test-threads=2
vp check
vp run typecheck
cargo fmt --all --check
cargo clean -p bibcode-server -p bibcode-desktop -p bibcode-updater-verifier
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
```

The direct recursive `-r` invocation keeps package test tasks sequential;
putting the concurrency limit on the root `test` wrapper does not constrain
its nested graph. Rust tests still use their package's default harness width.

Use the repository's Windows/MSVC launcher when required by the native Windows
page. The `-j 2` option bounds Cargo compilation jobs. The
`--test-threads=2` harness option bounds concurrent tests within each Rust test
binary; it does not serialize distinct test binaries. Do not add
`--test-threads=1`, broad locks, sleeps, yields, or larger production deadlines
to make a loaded suite pass.

For each command record the exact invocation, exit code, duration, test totals,
warnings, cleanup diagnostics, and commands skipped after a failure. A broad
suite does not replace focused evidence.

## Native package contract

Build through current repository scripts. Discover the artifact and executable
from current package output and metadata rather than assuming a filename.
Confirm:

- package version and architecture;
- executable existence and permission;
- native package metadata and bundle identity;
- the artifact came from the tested worktree/revision; and
- signing, notarization, or Authenticode state without overstating unavailable
  credentials.

Build packaged desktop E2E through:

```sh
vp run test:ui:desktop:build
vp run test:ui:desktop
```

Set only the native platform value documented by the selected platform page.
After the build, set `BIBCODE_E2E_APP_PATH` to the exact worktree-built
application as documented by the selected platform page. Never use an installed
BiBCode application as evidence for the worktree build.

## Disposable external-worktree scenario

Create a unique temporary Git repository outside BiBCode-managed worktree and
user project locations. Configure identity locally, create an initial commit,
then create two or more worktrees with native `git worktree add`.

Include platform-relevant path aliases and at least one path with spaces. Record
Git's worktree paths and the host's physical/canonical identities. In the
packaged application:

1. add the repository as a project;
2. observe manually created worktrees in **Discovered worktrees**;
3. verify the parent is grouped once and full paths remain accessible;
4. adopt one candidate and exercise **Add all** only on disposable candidates;
5. verify **Keep hidden** does not delete the Git worktree;
6. present the same physical worktree through its platform alias and confirm no
   duplicate owner/catalog entry appears;
7. restart the exact package and confirm identity/adoption persists; and
8. prove every external worktree still exists on disk.

Do not run destructive worktree scenarios against a user repository.

## Remote-server update scenario

Exercise the controlled second host from both Settings and the sidebar in both
themes: named confirmation with fresh counts and Cancel; progress replacing the
action; view-close persistence; failure with Retry reconfirmation and row
Dismiss; manual platform/install/SSH instructions with Copy. For browser mode,
restart its primary host on another version and verify an explicit Reload
prompt with composer input retained. Host notice links to sharing management.
Record fake-host UI/session evidence separately from real installation evidence.

The seeded `remote-install` lane runs only on disposable native CI runners. It
checks candidate boot/version, retained storage/project/backup, observed
percent/stage, requester log count, and Linux mount/runtime ownership. Record
whether a live native grant actually widened; `widened: false` is not a wide
live pass. When the private credential receipt is not published, raw remote
WebDriver logs stay withheld. The retained `remote-credential-trace.log`
records whether the share control was found, clicked, and widened, plus the
redacted credential error. A pairing-link failure includes the HTTP status,
attempt count, and whether the bootstrap endpoint changed during the wait. It
does not include the endpoint or the response body. The wait re-reads the
desktop bootstrap on each attempt because that cache updates when the restarted
server is ready. Host notice can remain tests-only under the approved
validation contract. Never execute the seeded harness locally, even with an
isolated data root; legacy cleanup can terminate another desktop app.

For seeded CI qualification, distinguish a packaging-child timeout from a
runtime-upgrade failure. The candidate, previous stable, and protected baseline
are three sequential builds with separate cold Cargo outputs outside the
workflow's cached repository `target`; `remote-install` reuses the protected
package. Only macOS x64 gives each packaging child 90 minutes and the complete
job 360 minutes. Other targets keep 45-minute packaging children and 240-minute
jobs; the separate WSL job also keeps 240 minutes. Frozen installs remain
10 minutes per checkout. Intel's job reserves 60 additional minutes after the
three packaging and install bounds for setup, all lanes, evidence, and cleanup.
Product, WebDriver, and restart deadlines remain unchanged. Record a command
timeout and the last completed phase as unavailable native evidence; passing
tooling tests cannot substitute for completed upgrade observations.

## Clone from URL network scenario

Before the network-transfer checks, select a remote server and enter a Git URL
and parent folder. Press **Browse…** beside **Parent folder** and confirm the
browser names that server, starts at the entered folder, and lists its directories
through that environment's connection. Choose a folder, clone, and verify the
checkout is beneath that folder on the same server. Reopen the form and cancel
parent browsing with both **Cancel** and **Back**; the URL and parent path must
remain intact. Exercise loading, empty folders, permission denial, and a dropped
server connection; confirm **Refresh** can retry and returning to the form keeps
its input. Capture the flow in light and dark. Local/WSL native picking must still
work, and manual parent entry must remain available.

For manual **Add Project → Type a path instead** during host initialization,
verify that the path stays editable, **Open project** is disabled with a waiting
status, and pointer, Enter and form submission do not create a project while host
platform information is unknown. When it arrives, the same dialog must enable
submission without clearing the path or changing the selected host; nothing is
submitted automatically. A selected host that disconnects retains the existing
fallback behavior. Run `AddProjectReadiness.test.tsx` through the web package's
compiled happy-dom lane alongside the Add Project component/workflow tests.

Create a disposable bare repository large enough that a clone at about
600 KB/s takes well over 30 seconds (for example 20 MB or more of
incompressible files). Serve it over smart HTTP (`git http-backend` behind a
local HTTP server) in two modes: throttled to about 600 KB/s, and one that stops
sending after a few megabytes while keeping the connection open. Clone through
**Add Project → Clone from URL** into disposable parent folders:

1. Throttled clone: the form stays visible with **Cloning…** and **Cancel
   clone**; the clone completes after more than 30 seconds; the dialog closes
   only once the project appears; the new checkout's `HEAD` is a real branch.
2. Cancel during a throttled clone: the form returns to an editable state with
   "Clone cancelled.", the destination folder the clone created disappears,
   and pressing **Clone** again at once starts a new clone instead of reporting
   an incomplete clone.
3. Stalling server: the clone fails after about 60 seconds with "The
   transfer stalled…", shown in the form, and its destination folder is
   removed.
4. Pre-existing half clone (a folder with `git init`, the same `origin`, and
   `HEAD` set to `ref: refs/heads/.invalid`): the clone is refused with "An
   incomplete clone exists at <path>. Remove it or choose another folder." and
   the folder is kept. Opening that repository in the Git Manager shows **No
   commits yet** and offers Fetch.

Steps 5-8 need the clone to still be running when the outage, the Cancel, or
the restart reaches the host. Either use a fixture whose clone lasts at least
three times the longest outage (for example 120 MB or more of incompressible
data at about 600 KB/s, over 200 seconds), or switch the throttled server to a
slow rate during the event. Keep that rate above 1 KB/s, or Git's stall guard
ends the clone. Record which method was used.

5. Connection drop during a throttled clone: after the clone has run for about
   ten seconds, drop the client's connection for at least 40 seconds (take the
   network offline, or stop a TCP proxy between client and server). The form
   keeps **Cloning…** and shows "Lost the connection to <host>. The clone
   continues on the server; reconnecting…". The destination folder keeps
   growing on the server. After the connection returns, the line clears, the
   clone completes, and the dialog closes once the project appears. If the
   clone finished during the outage, the re-attach finds the finished clone
   and adds it the same way.
6. Cancel across a reconnect: start a throttled clone and drop the connection.
   Press **Cancel clone** while disconnected. The form shows **Cancelling…**
   and "The clone stops when <host> reconnects.". Restore the connection. The
   line clears while the cancel reaches the host, and the result depends on
   where the clone stood:
   - partial clone (Git still running when the Cancel reached the host): the
     form shows "Clone cancelled." and the folder is removed;
   - completed clone (Git finished first): the form shows "The clone finished
     before it could be cancelled. It is in <path> and was not added as a
     project. Press Clone to add it."; the folder keeps its full checkout, is
     not added as a project, and the next **Clone** into it adds it.

   Repeat with the connection restored before pressing Cancel, with the same
   two outcomes. Then press **Cancel clone** while connected and drop the
   connection at once: the form stays **Cancelling…** and shows "The clone
   stops when <host> reconnects." until the connection returns. Close the
   dialog while it shows the reconnecting line: it closes; after the
   connection returns the clone is cancelled and its folder removed, and a new
   Clone of that URL into that folder waits for the cancel, then starts. State
   which outcome occurred in each variant.

7. Server restart mid-clone: stop the server gracefully during a throttled
   clone and start it again. For a partial clone, the destination folder is
   removed during shutdown, and after reconnecting the form shows "No clone is
   in progress for <path>. Press Clone to start again.". A clone that
   completed before the stop is found again after reconnecting, and the
   project is added.
8. Window closed mid-clone (orphan policy): close the client window during a
   throttled clone against a remote host. The cancel is best effort, so the
   clone may keep running on the host. Reopen the client, and clone the same
   URL into the same folder: it joins the running clone or adds the finished
   repository. It must never report an incomplete clone while the orphan runs.

Record each duration and the exact messages. SSH remotes have no stall guard;
record SSH coverage separately if tested.

## Staged chat attachment uploads

For changes to upload pacing, resume, turn binding or progress/Cancel, run the
closest client/UI seams as well as the real Rust upload liveness binary:

```sh
cd packages/client-runtime
node ../../scripts/run-local-vp.mjs test run src/operations/uploadStager.test.ts src/operations/attachmentStaging.test.ts src/connection/nextSession.test.ts src/state/vcsClone.test.ts src/state/vcsClone.registry.test.ts
cd ../../apps/web
vp test run --project unit src/lib/formatTransferBytes.test.ts src/components/ChatView.logic.test.ts src/components/chat/AttachmentUploadNotice.test.tsx src/components/chat/MessagesTimeline.test.tsx src/test/uiTypography.source.test.ts
cd ../..
cargo test -p bibcode-server --test rpc_liveness -j 2
```

Use the isolated server/web/proxy topology in the slow-link scenario below,
with test-owned protocol provider fixtures and disposable data. Repeat for
plain RPC and a pinned E2EE profile. Verify that the carrying session advertises
`attachmentStaging`; never add the flag only to a descriptor fixture.

1. Send a valid 10 MiB image with `up=65536`, then `up=16384` and unlimited
   downstream. Confirm acknowledged raw-byte progress, at most two append
   requests in flight, no 4408 during the upload, and identical received bytes
   and digest in the provider fixture's input log. Keep another thread streaming
   and verify its **Stop** still works while upload requests are outstanding.
2. Freeze a link mid-upload. Record the client 4408 within 33 seconds, visible
   **Reconnecting…**, the acknowledged checkpoint, and resume after thaw.
   Repeat a multi-file send lasting more than ten minutes and verify early
   completed stages remain bindable without another byte upload. Restart the
   disposable server after an earlier file completes: its lost stage restarts
   once under the shared per-file budget and a
   second loss reports failure with the outgoing work restored.
3. Cancel separate uploads with pointer, Tab/Enter and Tab/Space. Require one
   abort per activation, no error banner, and usable restored image previews.
   Type fresh composer content while staging; cancellation/failure keeps both
   that work and the outgoing prompt, attachments, terminal/element contexts,
   preview annotations and review comments. Check the route/unmount fence so
   late progress cannot affect another thread.
4. Queue an attachment message while a provider turn runs. It has a local pending
   upload row, not a claimed server queue entry, and leaves **Stop** available.
   On admission, remove that local row once. Exercise failed turn admission and
   unchanged retry without another upload; changed files or expired stages must
   stage again. Cancel/failure releases unbound uploads or leaves only expiring
   entries when disconnected.
   Hold turn admission pending before binding and change route/unmount: no
   uploads.cancel may retire its IDs. Verify success, and failure restoring the
   old draft without modifying the new thread. Drop an accepted admission reply,
   then retry: command/message IDs, timestamp and raw upload IDs must replay
   unchanged, without get/re-upload of consumed IDs. A typed non-accepted
   not_found permits fresh IDs on a subsequent explicit send. Preserve edited
   composer work during reconciliation.
   Unmount/remount the actual ChatView both before and after the ambiguous
   result settles; require the original command and zero restaging. Mount two
   views of the same scoped thread and prove a pending admission/replay fences
   the other. Repeat typed not_found followed by explicit fresh Send after
   remount. Verify blocked/removed environments and host/storage rotation leave
   unresolved intent recoverable and do not send to the replacement authority.
   Cookie/credential rotation on the same store may replay unchanged under the
   current operate permission, with foreign unaccepted stages refused.
5. Against an old server without the capability, confirm the existing inline
   data-URL path and zero `uploads.*` probes. Capture light/dark progress and
   reconnect/Cancel screenshots. Record the browser's Ping/Pong and queued-data
   behavior after `close()` separately from upload success. Report unavailable
   native WebKitGTK measurements explicitly as **not measured**.

Record these results in the report template's staged upload subsection. This
scenario does not validate Phase B file-panel uploads/downloads or asset routing.

## Slow-link liveness scenario

For transport or heartbeat changes, first run the paused-clock unit coverage
and the real-socket liveness matrix:

```sh
cargo test -p bibcode-server --lib rpc::transport::tests -j 2
cargo test -p bibcode-server --test rpc_liveness -j 2
```

The unit coverage checks Ping cadence under timer jitter and after a stalled
runtime. The integration matrix keeps the production silence and transfer
deadlines, including an idle client kept alive only by its WebSocket Pongs.

Run this against an isolated development server (its own `BIBCODE_HOME`) or a
standalone server reached from a browser, never against user data. Create a
disposable repository whose newest commit adds a text file of about 4 MB, below
the 4.375 MB patch bound, so Git Manager returns the whole patch:

```sh
node -e "require('fs').writeFileSync('big.txt', ('0123456789abcdef'.repeat(4)+'\n').repeat(61500))"
git add big.txt && git commit -q -m "big diff"
```

Put `scripts/throttle-proxy.ts` between the browser and the server:

1. Start the web client and note `webPort` from its `[dev-runner] mode=dev:web …`
   line: `BIBCODE_PORT_OFFSET=81 vp run dev:web`. It targets server port
   13854 (13773 + 81), where the proxy will listen.
2. Start the server on another offset and allow the web origin:
   `BIBCODE_PORT_OFFSET=80 BIBCODE_HOME=<disposable> vp run dev:server -- --dev-url http://localhost:<webPort>`.
3. Start the proxy:
   `node scripts/throttle-proxy.ts --listen 127.0.0.1:13854 --target 127.0.0.1:13853 --control 127.0.0.1:13855`.
4. Pair through the server's startup token (`/pair#token=…` on the web port) and
   add the repository.

Before the establishment checks, open a thread in the environment so the
composer shows its connection banner. Reconnect by reloading the page: the web
client is served directly, so only the environment's HTTP and RPC traffic
crosses the proxy.

First record the size of the first config snapshot: the data the RPC WebSocket
receives before the banner disappears, shown in the browser's network-panel
frames. The checks exercise the old 15-second limit only when size ÷ rate
exceeds 15 seconds (about 250 KB on a host with many Claude skills).

Check establishment before the large-diff transfers:

1. `curl "http://127.0.0.1:13855/set?down=16384&up=16384"`, then reload. The
   environment must connect on its first attempt: one RPC WebSocket, no retry.
   The banner reads "<environment>: Connecting…"; about 5 seconds after the
   socket connects, its second line reads "Receiving settings from
   <environment> over a slow connection…". The banner disappears when the
   environment connects, after about 16 seconds plus preparation for a 255 KB
   snapshot. "did not respond during connection setup." must not appear. A
   remote environment reached through a proxy shows the same line on its
   context card.
2. Repeat at `down=8192&up=8192`: first-attempt connection after about 31 seconds
   plus preparation.
3. Repeat at `down=4096&up=4096` and record which bound decides the attempt:
   connection after about 62 seconds when the path buffers the snapshot, or
   the server ending the session at its write deadline when writes block.
4. At 8 KiB/s, reload and freeze the link with
   `curl "http://127.0.0.1:13855/set?freeze=1"` once the "Receiving settings…"
   line shows. Within 33 seconds of the freeze the banner reads
   "<environment>: Reconnecting…" with "No data from <environment> for 30
   seconds. The connection is too slow or was lost." Record it within 15
   seconds (the next attempt can replace it), then thaw with `freeze=0`.

Then check liveness during large-diff transfers:

1. `curl "http://127.0.0.1:13855/set?down=65536&up=65536"`, then open the big
   commit's diff in Git Manager History. The transfer takes about a minute and
   must finish without a disconnect. The socket must show
   `Sec-WebSocket-Protocol: bibcode.rpc.chunked.v1`, and the large response must
   arrive as binary frames. About 15 seconds in, the status bar shows a warning
   such as **1 slow request**; clicking it lists `gitManager.getDiff` with its
   start time. No toast covers the panel, and the warning clears when the
   transfer finishes.
2. Repeat at `down=262144&up=262144`.
3. With the page idle, `curl "http://127.0.0.1:13855/set?freeze=1"`. Within 33
   seconds the socket closes with code 4408, and Git Manager shows "Reconnecting
   to <environment>. Git Manager loads when the connection is back." A remote
   environment's context card shows "No data from <environment> for 30 seconds.
   The connection is too slow or was lost. Reconnecting…". Record that text
   within 15 seconds of the close. After that, keep the link frozen: the next
   attempt must fail within 15 seconds of starting because the setup deadline
   still applies before the socket connects. The descriptor request's own
   10-second limit usually reports first as "Remote environment endpoint <url>
   timed out after 10000ms." Record its time and text. Thaw with
   `freeze=0` and confirm the connection returns.
4. Freeze an idle connection and record the freeze-start timestamp. While it
   remains frozen, verify the server ends that session within 50 seconds of
   freeze start ("RPC peer silent past the heartbeat limit; ending the session",
   with its `silent_for` and `limit` fields). Record the
   timestamp and duration before thawing. A later end is a failed check;
   accepted writes never move this measurement's origin.
5. Thaw after the idle assertion. Start another large transfer, freeze it after the first record, and keep it
   frozen. Verify server-side teardown within 33 seconds of freeze start,
   before thawing. Record subscription cleanup and the writer-failure log.
   A post-thaw close is not evidence of teardown within the bound.

Record the rates, transfer durations, close codes, the exact status text, and
the server log lines.

## Git Manager validation scenario

Run this scenario only against the exact packaged application and disposable
repositories. Destructive Git Manager scenarios must never run against a user
repository. Record the resulting paths, revisions, screenshots, command output,
and timings in the execution report, not in this runbook.

### Disposable Git Manager fixture

Run the following in a POSIX-compatible shell with Git and Node.js available.
On Windows, use Git Bash so the same fixture recipe applies. It creates one
fixture family in the operating system's temporary area: a primary repository,
two independent project clones for cache and isolation checks, one linked
worktree whose path contains a space, and a bare `origin`. The configured
`origin` looks like a supported provider remote so the explicit provider pane
is reachable, while repository Git traffic is rewritten locally to the bare
repository and never needs a network or real forge.

```sh
set -eu

GIT_MANAGER_FIXTURE_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/bibcode-git-manager.XXXXXX")"
GIT_MANAGER_FIXTURE_REMOTE="https://github.com/bibcode-validation/git-manager-fixture.git"
GIT_MANAGER_FIXTURE_ORIGIN_URL="$(node -e \
  'process.stdout.write(require("node:url").pathToFileURL(process.argv[1]).href)' \
  "$GIT_MANAGER_FIXTURE_ROOT/origin.git")"
export GIT_MANAGER_FIXTURE_ROOT GIT_MANAGER_FIXTURE_REMOTE

git init --bare --quiet "$GIT_MANAGER_FIXTURE_ROOT/origin.git"
git init --quiet -b main "$GIT_MANAGER_FIXTURE_ROOT/main"
git -C "$GIT_MANAGER_FIXTURE_ROOT/main" config user.name \
  "BiBCode Git Manager Fixture"
git -C "$GIT_MANAGER_FIXTURE_ROOT/main" config user.email \
  "git-manager-fixture@example.test"
git -C "$GIT_MANAGER_FIXTURE_ROOT/main" config \
  "url.$GIT_MANAGER_FIXTURE_ORIGIN_URL.insteadOf" "$GIT_MANAGER_FIXTURE_REMOTE"
git -C "$GIT_MANAGER_FIXTURE_ROOT/main" remote add origin \
  "$GIT_MANAGER_FIXTURE_REMOTE"

cd "$GIT_MANAGER_FIXTURE_ROOT/main"

write_fixture_png() {
  node - "$1" "$2" "$3" "$4" <<'NODE'
const fs = require("node:fs");
const path = require("node:path");
const zlib = require("node:zlib");

const [output, redText, greenText, blueText] = process.argv.slice(2);
const red = Number(redText);
const green = Number(greenText);
const blue = Number(blueText);
const crcTable = Array.from({ length: 256 }, (_, index) => {
  let value = index;
  for (let bit = 0; bit < 8; bit += 1) {
    value = (value & 1) === 1 ? 0xedb88320 ^ (value >>> 1) : value >>> 1;
  }
  return value >>> 0;
});

function crc32(buffer) {
  let value = 0xffffffff;
  for (const byte of buffer) value = crcTable[(value ^ byte) & 0xff] ^ (value >>> 8);
  return (value ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const typeBytes = Buffer.from(type, "ascii");
  const length = Buffer.alloc(4);
  const checksum = Buffer.alloc(4);
  length.writeUInt32BE(data.length);
  checksum.writeUInt32BE(crc32(Buffer.concat([typeBytes, data])));
  return Buffer.concat([length, typeBytes, data, checksum]);
}

const header = Buffer.alloc(13);
header.writeUInt32BE(2, 0);
header.writeUInt32BE(2, 4);
header[8] = 8;
header[9] = 6;
const row = [0, red, green, blue, 255, red, green, blue, 255];
const pixels = Buffer.from([...row, ...row]);
const png = Buffer.concat([
  Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
  chunk("IHDR", header),
  chunk("IDAT", zlib.deflateSync(pixels)),
  chunk("IEND", Buffer.alloc(0)),
]);
fs.mkdirSync(path.dirname(output), { recursive: true });
fs.writeFileSync(output, png);
NODE
}

{
  printf '%s\n' "section one target"
  for line in 1 2 3 4 5 6 7 8 9 10; do
    printf 'section one stable %s\n' "$line"
  done
  printf '%s\n' "section two target"
  for line in 1 2 3 4 5 6 7 8 9 10; do
    printf 'section two stable %s\n' "$line"
  done
  printf '%s\n' "section three target"
  for line in 1 2 3 4 5 6 7 8 9 10; do
    printf 'section three stable %s\n' "$line"
  done
} > partial.txt
printf '%s\n' "main base" > conflict.txt
printf '%s\n' "tracked discard baseline" > discard.txt
printf '%s\n' "tracked stash baseline" > stash.txt
write_fixture_png images/modified.png 220 40 40
write_fixture_png images/deleted.png 40 180 80
git add .
git commit --quiet -m "fixture baseline"

fixture_commit=1
while [ "$fixture_commit" -le 105 ]; do
  printf 'history fixture %s\n' "$fixture_commit" >> history.txt
  git add history.txt
  git commit --quiet -m "history fixture $fixture_commit"
  fixture_commit=$((fixture_commit + 1))
done

write_fixture_png images/modified.png 40 90 220
write_fixture_png images/added.png 220 180 40
rm -- images/deleted.png
git add -A images
git commit --quiet -m "image diff fixtures"
git tag -a fixture-annotated -m "annotated fixture tag"
git tag fixture-lightweight

git switch --quiet -c conflict-continue
printf '%s\n' "topic resolution" > conflict.txt
git add conflict.txt
git commit --quiet -m "conflicting topic for continue"
git switch --quiet main
git switch --quiet -c conflict-abort
printf '%s\n' "topic resolution" > conflict.txt
git add conflict.txt
git commit --quiet -m "conflicting topic for abort"
git switch --quiet main
printf '%s\n' "main resolution" > conflict.txt
git add conflict.txt
git commit --quiet -m "conflicting main change"

git switch --quiet -c cherry-source
printf '%s\n' "cherry-pick fixture" > cherry-source.txt
git add cherry-source.txt
git commit --quiet -m "cherry-pick source"
git switch --quiet main
git switch --quiet -c rewrite-sandbox
printf '%s\n' "rewrite first" > rewrite.txt
git add rewrite.txt
git commit --quiet -m "rewrite first"
printf '%s\n' "rewrite second" >> rewrite.txt
git add rewrite.txt
git commit --quiet -m "rewrite second"
git switch --quiet main

git push --quiet -u origin main
git -C "$GIT_MANAGER_FIXTURE_ROOT/origin.git" symbolic-ref HEAD refs/heads/main
git push --quiet origin fixture-annotated

git switch --quiet -c force-lease
git push --quiet -u origin force-lease
printf '%s\n' "local side" > force-local.txt
git add force-local.txt
git commit --quiet -m "force-with-lease local side"
git switch --quiet main
git switch --quiet -c push-ready
git push --quiet -u origin push-ready
printf '%s\n' "push-ready local change" > push-ready.txt
git add push-ready.txt
git commit --quiet -m "push-ready local commit"
git switch --quiet main
git switch --quiet -c publish-ready
printf '%s\n' "publish-ready local change" > publish-ready.txt
git add publish-ready.txt
git commit --quiet -m "publish-ready local commit"
git switch --quiet main

git clone --quiet "$GIT_MANAGER_FIXTURE_ORIGIN_URL" "$GIT_MANAGER_FIXTURE_ROOT/project-two"
git -C "$GIT_MANAGER_FIXTURE_ROOT/project-two" config user.name \
  "BiBCode Git Manager Fixture"
git -C "$GIT_MANAGER_FIXTURE_ROOT/project-two" config user.email \
  "git-manager-fixture@example.test"
git -C "$GIT_MANAGER_FIXTURE_ROOT/project-two" config \
  "url.$GIT_MANAGER_FIXTURE_ORIGIN_URL.insteadOf" "$GIT_MANAGER_FIXTURE_REMOTE"
git -C "$GIT_MANAGER_FIXTURE_ROOT/project-two" remote set-url origin \
  "$GIT_MANAGER_FIXTURE_REMOTE"
printf '%s\n' "remote main update" > "$GIT_MANAGER_FIXTURE_ROOT/project-two/remote-update.txt"
git -C "$GIT_MANAGER_FIXTURE_ROOT/project-two" add remote-update.txt
git -C "$GIT_MANAGER_FIXTURE_ROOT/project-two" commit --quiet -m "remote main update"
git -C "$GIT_MANAGER_FIXTURE_ROOT/project-two" push --quiet origin main
git -C "$GIT_MANAGER_FIXTURE_ROOT/project-two" switch --quiet force-lease
printf '%s\n' "remote side" > "$GIT_MANAGER_FIXTURE_ROOT/project-two/force-remote.txt"
git -C "$GIT_MANAGER_FIXTURE_ROOT/project-two" add force-remote.txt
git -C "$GIT_MANAGER_FIXTURE_ROOT/project-two" commit --quiet -m \
  "force-with-lease remote side"
git -C "$GIT_MANAGER_FIXTURE_ROOT/project-two" push --quiet origin force-lease
printf '%s\n' "project two only" > \
  "$GIT_MANAGER_FIXTURE_ROOT/project-two/project-two-only.txt"

git clone --quiet "$GIT_MANAGER_FIXTURE_ORIGIN_URL" "$GIT_MANAGER_FIXTURE_ROOT/project-three"
git -C "$GIT_MANAGER_FIXTURE_ROOT/project-three" config user.name \
  "BiBCode Git Manager Fixture"
git -C "$GIT_MANAGER_FIXTURE_ROOT/project-three" config user.email \
  "git-manager-fixture@example.test"
git -C "$GIT_MANAGER_FIXTURE_ROOT/project-three" config \
  "url.$GIT_MANAGER_FIXTURE_ORIGIN_URL.insteadOf" "$GIT_MANAGER_FIXTURE_REMOTE"
git -C "$GIT_MANAGER_FIXTURE_ROOT/project-three" remote set-url origin \
  "$GIT_MANAGER_FIXTURE_REMOTE"
printf '%s\n' "project three only" > \
  "$GIT_MANAGER_FIXTURE_ROOT/project-three/project-three-only.txt"

printf '%s\n' "stash one" >> stash.txt
git add stash.txt
git stash push --quiet -m "fixture stash one"
printf '%s\n' "stash two" >> stash.txt
git add stash.txt
git stash push --quiet -m "fixture stash two"

awk '
  $0 == "section one target" { print "section one changed"; next }
  $0 == "section two target" { print "section two changed"; next }
  $0 == "section three target" { print "section three changed"; next }
  { print }
' partial.txt > partial.txt.next
mv -- partial.txt.next partial.txt
printf '%s\n' "tracked discard changed" > discard.txt
printf '%s\n' "untracked discard fixture" > untracked-discard.txt

git worktree add --quiet -b occupied "$GIT_MANAGER_FIXTURE_ROOT/occupied worktree" main
git worktree list --porcelain
git rev-list --count --all
git stash list
git status --short
git for-each-ref --format='%(refname:short) %(objecttype)' refs/tags
git ls-remote --heads origin
```

The final checks must show more than 100 reachable commits, two stash entries,
the `occupied` linked worktree, both tag kinds, and the three intended working
tree changes. Keep the exported root available to the companion shell for the
procedure below. Add `main`, `project-two`, and `project-three` as three distinct
BiBCode projects; do not add `origin.git` or the linked worktree as separate
projects. Remove only this exact temporary root during the runbook's
[Cleanup](#cleanup).

### Local packaged-application procedure

The following are packaged-surface acceptance steps. Do not substitute a raw
RPC or command-line Git operation when a named Git Manager control is missing;
record that step as **FAIL**. The companion-shell commands below are exceptions
used only to create repository states that cannot be held while the application
starts.

1. Hover the primary fixture project's header and choose its **Git Manager**
   branch-icon button. Confirm the centre route is the project-scoped `/git`
   route, choosing the button again focuses the same manager, and reload returns
   to that route. Confirm every fresh open starts on **Main Checkout**, including
   after a prior linked-worktree selection and after reload.
2. Use the **Worktree** selector to choose `occupied worktree` and verify Changes,
   History, branch, and sync data retarget together. Return to **Main Checkout**,
   open the branch dropdown, and choose `occupied`. The row must name the owning
   worktree and say **Switch to worktree**; choosing it visibly redirects the
   selector instead of issuing checkout. Attempt rename and delete from that
   row and confirm the server-authored blocked presentation names the owning
   worktree path.
3. On **Main Checkout**, inspect the Changes rows for `partial.txt`,
   `discard.txt`, and `untracked-discard.txt`, then select each file and inspect
   its diff. While these changes are dirty, request a branch switch and confirm
   the dialog explains **Leave my changes** as an ordinary visible stash and
   **Bring my changes** as carrying the working tree; choose **Bring my changes**
   and then return to `main` the same way. In `partial.txt`, use the
   partial-staging gutter to toggle one line, one complete **Changed lines** run,
   and a dragged range; apply the selection and confirm only those lines move
   between **Unstaged** and **Staged**. Switch the diff between both areas,
   unstage a selected line, and exercise **Discard selected lines…** behind its
   confirmation. Then commit one included whole-file change, add another
   included whole-file change through **Amend Last Commit**, and confirm the
   commit is replaced rather than duplicated. Choose **Undo**, accept its
   explicit confirmation, and confirm the message and changes return without
   deleting files. Exercise whole-file discard and **Discard All**; tracked
   content must restore, an untracked path must use OS trash when available, and
   a trash failure must require the separate permanent-discard confirmation.
   Finish with a clean checkout.
4. Open **History**, scroll until the loaded list crosses the 100-commit page
   boundary, and select commits on both sides of that boundary. Confirm the
   selected commit's metadata, changed-file list, and per-file diff agree with
   Git. While the older page remains loaded, run this in the companion shell:

   ```sh
   printf '%s\n' "generation splice fixture" >> \
     "$GIT_MANAGER_FIXTURE_ROOT/occupied worktree/generation.txt"
   git -C "$GIT_MANAGER_FIXTURE_ROOT/occupied worktree" add generation.txt
   git -C "$GIT_MANAGER_FIXTURE_ROOT/occupied worktree" commit -m \
     "generation splice fixture"
   ```

   Confirm the new commit is spliced above the pinned history generation while
   the loaded older page, selection, and scroll context remain coherent, with no
   duplicate or missing rows.

5. From **Main Checkout**, create and check out a disposable branch, rename it,
   return to `main`, and delete the disposable branch through its confirmation.
   Confirm the dropdown's Default, Recent, and Other groups, current marker, and
   filtering stay current after each operation. Confirm a symbolic remote
   default such as `origin/HEAD` never appears as a branch row, including when a
   real local branch is named `origin`. Repeat the occupied-branch redirect from
   step 2 after these mutations to prove its owner was not lost.

   Prepare a remote-only branch in the disposable fixture:

   ```sh
   git -C "$GIT_MANAGER_FIXTURE_ROOT/main" push --quiet origin main:refs/heads/remote-checkout
   ```

   Choose **Fetch origin**, open the branch picker, and search `REMOTE-CHECKOUT`.
   Confirm **Remote branches** contains `origin/remote-checkout`, with no Rename
   or Delete action. Select it and verify the local branch and tracking ref:

   ```sh
   git -C "$GIT_MANAGER_FIXTURE_ROOT/main" branch --show-current
   git -C "$GIT_MANAGER_FIXTURE_ROOT/main" rev-parse --symbolic-full-name '@{upstream}'
   ```

   Expect `remote-checkout` and `refs/remotes/origin/remote-checkout`. Return to
   `main` through the picker and select that remote row again. The failure must
   explain that the local branch exists and suggest selecting or renaming it;
   `main`, the local branch tip, and its upstream must remain unchanged. Confirm
   the local `remote-checkout` row still checks out normally, then return to
   `main`. Repeat a remote-only checkout with pending changes: Cancel leaves
   them untouched, **Bring my changes** carries non-conflicting edits, and
   **Leave my changes** saves an ordinary stash before switching. A conflicting
   edit must be preserved when Git refuses the switch.

6. On `main`, choose **Fetch origin** and confirm the remote-only main commit is
   discovered; choose **Pull origin** and confirm it arrives locally. Check out
   `push-ready` and choose **Push origin**; check out `publish-ready` and choose
   **Publish branch to origin**, confirming an upstream is established. Check
   that an upstream configured before its remote-tracking ref exists does not
   make refs or unrelated local checkouts unavailable; Fetch must hydrate the
   missing tracking ref and restore ahead/behind reporting. Check
   out `force-lease` after the fetch and confirm its divergent state offers
   **Force push origin**. The confirmation must state `--force-with-lease` and
   the operation must stop if the lease is stale; no bare force-push path is
   acceptable. Return to `main`.
7. Open **Stashes** and confirm both native entries appear. Select each entry and
   each changed file to inspect its per-entry diff. Apply one entry and verify it
   remains listed, clean the applied changes, then pop that entry and verify it
   is removed; clean again and drop the other entry through its destructive
   confirmation. From the companion shell, create `merge-ready` from the
   current `main` tip so that a plain merge could fast-forward:

   ```sh
   git -C "$GIT_MANAGER_FIXTURE_ROOT/main" switch --quiet -c merge-ready
   printf '%s\n' "merge preview fixture" > "$GIT_MANAGER_FIXTURE_ROOT/main/merge-ready.txt"
   git -C "$GIT_MANAGER_FIXTURE_ROOT/main" add merge-ready.txt
   git -C "$GIT_MANAGER_FIXTURE_ROOT/main" commit --quiet -m "merge-ready change"
   git -C "$GIT_MANAGER_FIXTURE_ROOT/main" switch --quiet main
   ```

   Open **Merge…**, select `merge-ready`, confirm the server-computed preview
   reports one commit ahead and none behind, then merge with **Merge commit**.
   Confirm the operation's started-to-finished presentation and that History
   shows a new merge commit on `main` with two parents rather than a
   fast-forward. Next, merge into a branch that is not checked out: from the
   companion shell run
   `git -C "$GIT_MANAGER_FIXTURE_ROOT/main" branch merge-into-target main~1`,
   open **Merge…**, choose `merge-into-target` under **Into**, select `main` as
   the source, confirm the dialog says the branch is updated without checking it
   out and offers no Squash, and merge. Confirm `main` stays checked out with an
   unchanged working tree and that
   `git -C "$GIT_MANAGER_FIXTURE_ROOT/main" log -1 --format=%P merge-into-target`
   prints two parents. In the chat's **Source Control** panel, confirm
   **Merge into current branch…** is enabled on the clean tree and opens the
   same dialog without an **Into** picker; **Fetch** completes and the dialog
   stays open. Open the **Tags** tab: collapse and expand the **Local** and
   **Remote origin** sections and confirm the state survives leaving and
   reopening the manager; create a tag from a History commit with **Push to
   origin after creating** on and confirm it appears under Remote origin
   without a "not fetched" badge; then choose **Push origin** on `main`, tick
   **Also push tags**, and confirm the dialog names the branch and the push
   completes.

8. Check out `conflict-continue` in the panel and create the deliberate rebase
   conflict from the companion shell:

   ```sh
   if git -C "$GIT_MANAGER_FIXTURE_ROOT/main" rebase main; then
     printf '%s\n' "expected rebase conflict did not occur" >&2
     exit 1
   fi
   git -C "$GIT_MANAGER_FIXTURE_ROOT/main" status --short
   ```

   Confirm the panel reports **Rebase underway**, presents `conflict.txt` as
   conflicted, and blocks unrelated mutations with the server-authored pending
   operation reason. Resolve the file in an editor, stage it, and choose
   **Continue**; the rebase must complete and the strip must clear. Then check out
   `conflict-abort`, create the same deliberate conflict, choose **Abort**, accept
   the confirmation, and confirm the pre-rebase branch and clean operation state
   are restored.

9. Check out `rewrite-sandbox`. In History, use the commit actions to
   cherry-pick the `cherry-pick source` commit, revert the resulting commit, and
   reset to the earlier `rewrite first` commit. Confirm cherry-pick and revert
   publish their operation state, and reset is unavailable until its explicit
   mode/effect confirmation is accepted. If the History action menu or any of
   these confirmations is absent, record **FAIL**; server primitives or an
   unmounted component are not packaged-application evidence.
10. Create an annotated tag from the toolbar, delete a disposable local tag
    through its confirmation, and push `fixture-lightweight` to `origin`; confirm
    the remote tag appears only after the explicit push. In History, select the
    `image diff fixtures` commit and inspect `images/modified.png`,
    `images/added.png`, and `images/deleted.png`. Exercise **2-up**, **Swipe**,
    **Onion-skin**, and **Difference** for the two-sided modification, then
    confirm the added file has no Before image and the deleted file has no After
    image. If delete or push is absent from the routed tag surface, record
    **FAIL** rather than substituting command-line Git.
11. Open **Show pull requests** (**Show merge requests** on GitLab), confirm the
    pane says provider data loads only on demand, and choose **Refresh** exactly once. Confirm one
    `gitManager.listPullRequests` refresh is sent through the environment's
    existing RPC connection. A configured provider may run separate pull-request
    and checks subcommands on the server; a missing CLI, credentials, or the
    fixture's intentionally nonexistent forge repository must produce the
    provider pane's explicit error or unavailable presentation without retrying
    in the background. For GitLab, confirm the pane uses merge-request wording
    throughout its toggle, heading and empty/unavailable states, `!N` for a loaded
    request and **Create merge request**; GitHub uses pull-request wording,
    `#N` and **Create pull request**.
    Choose that Create action: the review dialog must open with the detected
    provider, a source-branch selector defaulting to the checkout, an empty
    required target-branch selector,
    whether the branch will be published first, and a title and description
    seeded from the latest commit. Creation must remain disabled until an
    option is selected; typing alone must not enable it. Choose a non-default
    target (for example `release/next`) and confirm a status refresh preserves
    the choice, clearing it disables creation again, and reopening starts
    unselected. Repeat from the chat Git action; combined commit/push/request
    actions must also require target review before any mutation. No push,
    provider mutation, or pull request may run just by opening the dialog from
    these entry points; on servers with `pullRequestCreateOptions`, opening it
    may run only the read-only defaults and picker reads. Cancel it and
    confirm the branch, its upstream, and the forge are unchanged. From Source Control, **Push & create PR/MR** and its
    Create menu action must push an unpublished branch first, then open the
    dialog with that branch selected as the source and listed among the origin
    branches; a rejected push must show an error and leave the dialog closed, and
    cancelling the dialog after the push must not create a request. After the
    request is created or found, **Done** in the dialog opened from Source
    Control must open it as a right-panel tab beside the thread (one tab per
    request; Done again does not duplicate it), unless the host is Bitbucket or
    Azure DevOps or **Pull requests** is off in Settings, where Done only closes
    the dialog; **Done** from the chat Git action, the Pull Requests page or the
    Git Manager only closes the dialog. A branch
    created with `git checkout -b feature origin/main` (upstream `origin/main`)
    must open the dialog without a push and publish `origin/feature` only when
    the request is created.
    With a disposable configured forge, create once and verify the exact selected
    non-default target; plain **Push** must never create a request. Only the
    dialog's explicit primary action may publish or create.
    Both selectors must contain only origin branches, including those with a
    same-named local branch. Verify local-only branches, other remotes, and `HEAD`
    are absent. A local-only checkout must still start with its branch as the
    source, state that it will be published first, and publish it only when the
    primary action runs.
    Change the source to another published branch and then an origin-only branch.
    Each change must clear the target, retain edited title/description, and seed
    untouched fields from that source's tip rather than another branch's newer
    commit. Select the target again and create: verify both exact branches at
    the forge, with the current checkout and its working files unchanged.
    Combined commit actions must explain why their source remains the current
    or newly generated branch. Search and paging must reach branches beyond the
    first page in both selectors.
    Connect to an older server without `gitPullRequestBranchSelection`, or
    reconnect to one after opening the dialog: creation must be disabled with
    update guidance, and attempting the action must send no mutation.
    Create options: on GitLab, create a merge request with **Mark as draft**,
    **Assign to me**, a reviewer, a label, a milestone, **Delete source
    branch** and **Squash commits**, and verify each on the GitLab page and
    that the title has exactly one `Draft:` prefix. On a project that always
    squashes, the Squash box must be locked with its note. On GitHub, create
    a draft pull request with an assignee, a reviewer, a label and a milestone
    and verify them on GitHub. On github.com, a reviewer who lacks access must
    leave the pull request created once with the warning shown; GitHub
    Enterprise Server may refuse such a request before creating it, which must
    show the failure and create nothing. Against a server without
    `pullRequestCreateOptions`, the new fields must be absent.
    Record whether a failed attempt published the branch; native stacked actions
    currently report start/end outcomes without intermediate phase events.
    Retry a failed combined commit/push/request action: it must reread Git, reuse
    a created feature branch, skip committing a clean tree, and require target
    reselection if the source branch changed. An unrelated external checkout
    detected during Retry must not be committed or published automatically.
12. Validate the two-project view isolation before the cache limit. Give `main`
    and `project-two` different Changes filters, selected files, and active tabs,
    then alternate between their project-header buttons. Each project must
    restore only its own selection, filter, and tab and must never render paths,
    refs, stashes, history, or operation events from the other project.
13. Validate the two-entry least-recently-used view-state bound. Set distinctive
    state in `main`, then `project-two`, revisit `main` to make `project-two` least
    recent, and set different state in `project-three`. Revisit `main` and
    `project-three` and confirm both states survive. Finally revisit
    `project-two` and confirm its evicted selection, filter, and tab return to
    defaults. The panels must not remain mounted or subscribed while hidden.
14. Complete the [zero-telemetry observation](#zero-telemetry-observation) below
    and record it separately from the automated evidence under
    [VCS coordination gates](#vcs-coordination-gates).

### Zero-telemetry observation

This packaged-application check is mandatory; automated source and component
tests do not replace it.

1. Open browser developer tools, select **Network**, preserve the log, filter to
   third-party hosts, and clear the existing entries. Leave the Git Manager and
   its pull-request pane open and completely idle for at least ten minutes.
   Confirm the filtered log remains empty: no provider poll, avatar lookup,
   analytics, error report, feature flag, asset CDN, or other third-party
   request is permitted.
2. Keep the log and WebSocket-frame view open, then choose **Refresh** in the
   pull-request pane exactly once. Confirm in decoded frames or the server
   operation log that exactly one `gitManager.listPullRequests` provider-refresh
   invocation crosses the existing BiBCode connection and that no direct browser
   request targets the provider or any other third-party host. Server-side
   provider subcommands are permitted only as work owned by that one explicit
   invocation.
3. Render History author identities and every image-diff case, then run this in
   the developer-tools Console:

   ```js
   document
     .querySelector('section[aria-label="Repository history"]')
     ?.querySelectorAll('img[src^="http://"], img[src^="https://"]');
   ```

   Confirm the result is empty. Author identity must be local initials or a
   deterministic local identicon, and repository images must use local `data:`
   sources rather than an HTTP or HTTPS source.

The automated counterparts live in `scripts/privacy-contract.test.ts`,
`apps/web/src/components/gitManager/gitManagerTelemetry.test.tsx`, the inline
`mod telemetry` in `apps/server/src/git/manager/mod.rs`, and the source-text
tripwire in `apps/server/tests/git_rpc.rs`. Run them through
[VCS coordination gates](#vcs-coordination-gates); the manual pass above is the
evidence for the packaged application.

### Pull Requests web shell validation

Use a disposable project or an existing project with authorized read access to
a configured GitHub or GitLab repository. Keep provider authentication on the
server host. Exercise the same flows in the packaged UI on each native target
and in browser mode; do not substitute direct RPC calls for missing controls.

Start with this read-only packaged smoke on each native target; retain the
screenshots and route evidence in the execution report:

- Open the project's **Pull Requests** sidebar button (accessible label
  `Pull Requests for <project>`), assert the URL contains `/pull-requests`, and
  capture the list with the project header still highlighted.
- With a configured, authenticated GitHub/GitLab host and a readable request,
  open a row, capture the detail title/state, then select **Files changed** /
  **Changes**. Verify `?tab=files`, a real text patch rendered by FileDiff and
  retained selection after reload. An empty or unavailable repository is
  availability evidence only; it does not pass the list/detail/files smoke.
- With the disposable no-remote packaged fixture, record the actionable
  unavailable state instead. The Pull Requests sibling beside the Git Manager
  navigation in `apps/desktop/e2e/specs/pierre-diffs.e2e.ts` checks only route
  entry and saves `pierre-pull-requests-route.png`; it does not substitute for
  the authenticated list/detail/files pass or authorize host writes.

Run the regression tripwires without a browser:

```sh
vp test run apps/web/src/components/pullRequests/pullRequestsTelemetry.test.tsx
cargo test -p bibcode-server --lib pull_requests::tripwires -j 2
```

The root run leaves this web test uncompiled: if a change is
[compiler-sensitive](./README.md#web-unit-tests-and-the-react-compiler), confirm
it from `apps/web` with `vp test run --project unit` and the same path without
the `apps/web/` prefix.

The web test forbids direct fetch/Image/WebSocket/XHR/beacon use and rendered
images, checks all actor initials, advances an idle hour, and counts explicit
refresh/action dispatches. Server tripwires inspect every module source,
default `gh`/`glab`/`git` specs and direct HTTP/background workers. One-shot
request deadlines and the protected 24-hour checkout write ceiling are
allowed. Keep TCP-listener failures blocked with their exact bind error;
never count a sandbox-denied listener test as passed or skipped.

1. Hover the project header and verify **Pull Requests** is the fourth action,
   after Git Manager. Open it, reload the list URL, and open a row. Both list and
   detail routes must keep that physical/grouped project header highlighted.
   The detail route must render title, state, branches, count tabs and metadata;
   reload `?tab=files` and verify that Files changed/Changes remains selected.
2. In **Settings → Source Control**, toggle **Pull requests** off. Confirm the
   button disappears and an existing route links to the setting with an
   explanation. Re-enable it. Verify the GitHub/GitLab rows list each configured
   host, redact authenticated accounts, and distinguish unauthenticated hosts.
3. Inspect provider/repository/account/custom-host header data. Switch checkouts
   and verify reads target the selected opaque checkout on its environment.
   A saved unavailable checkout must still offer the worktree selector for
   recovery. Disconnect the environment and verify opening the route does not
   reconnect it or issue module reads.
4. Verify host-specific tabs/counts, 300 ms text search and Enter, each filter,
   lazy label/milestone/branch vocabularies, Clear filters, and fixed-height
   virtualized rows. Load more must retain earlier rows, then disappear when
   nextCursor is null. Refresh must return to the first page. Leaving and
   returning to the list must restore its saved scroll position, including
   previously loaded later pages. Reload while scrolled and repeat the check.
   Rescan after changing origin/account in a disposable checkout: the new
   header must never retain old repository rows or picker results.
5. Verify empty, no-remote, unsupported-host, missing-tool, and authentication
   states. Copy any offered login command and use Rescan after recovery. Use
   an isolated CLI config when simulating authentication loss; preserve the
   real account configuration. List failures must show the server message,
   optional host detail, Retry, and Clear filters when filters are active.
   GitLab review-status filters currently return this explicit unavailable
   state. GitHub search has no cursor/total and must not show a zero comment
   badge or Load more.
6. Open **New pull request** (using host vocabulary) and cancel the existing
   Git Manager review dialog; opening it must not publish anything. Follow
   **Open in Pull Requests** from a loaded Git Manager provider row and verify
   its environment/project/number URL.
7. Inspect the module DOM and network log: no avatar images, remote images,
   client HTTP fetches, provider polling, or window-focus refresh. Module data
   uses the existing typed WebSocket RPC. Record React best-practice and UI.md
   reviews separately from visual/native evidence.
8. In Conversation verify description markdown, comments, reviews, threads,
   resolved/outdated markers, suggestions and system events. Remote images
   must be labelled browser links. A minimized comment opens with Show comment;
   a thread's file link must select Files and scroll to its encoded path.
   Confirm metadata moves above the merge box when the detail is narrower than
   its container breakpoint (48rem): in a right-panel tab and in a narrow window.
   On GitLab, post an authorized inline suggestion, then refresh Conversation and
   Files: a `DiffNote`/other Note-subtype GlobalID must retain its thread, numeric
   note id, suggestion and actions. Inspect label/milestone add/remove events.
   Measure provider-process counts separately for cold and warm writes/read refreshes.
   Confirm the 30-second context bound, Rescan bypass, and fresh permission/head
   refusal after access is revoked or the request changes within that window.
9. Right-panel requests: with a thread on a branch that has an open request,
   choose **Open MR in panel** (**Open PR in panel** on GitHub) from the
   Source Control menu; it opens one right-panel tab labelled `!N`/`#N` beside
   the thread, against the thread's checkout. For a Bitbucket or Azure DevOps
   repository, or with **Pull requests** off in Settings, the item is absent
   and Done opens no tab. In the tab, switch tabs, follow a conversation file
   link (it must select Changes and scroll to the file), reload and confirm the
   tab and its selected section persist, then use **Open full page** and
   confirm the `?tab=` URL. On a disposable request, a revert must open the
   revert request as a second tab and a delete must close only its tab. From
   the full page, **Open beside a thread…** must list the project's threads
   with those on the request's branch first ("On this branch"), open the
   request in the chosen thread and navigate to it; with no thread it is
   disabled with its reason. Remove the thread's worktree and confirm the tab
   shows the unavailable reason instead of an error.
10. Verify Commits copy the full SHA and open the host commit. Checks/Pipelines
    group rows with state, duration and links and distinguish the host's empty
    state. Refresh reloads detail plus the active tab (including timeline in
    Files); reopening a cached tab refreshes it before displaying its previous result. Exercise an expired
    authentication response and verify context recovery and Retry.
11. Verify real GitHub split patches and GitLab synthetic-header patches render
    in Files, including renamed files. Scroll far enough to exercise list and
    diff virtualization. Viewed must survive tab changes and reload. Test the
    whitespace toggle, binary/unavailable/too-large rows, long-line plain text,
    and the explicit Show diff anyway gate using controlled fixtures.
12. Exercise the merge readiness states with host data or declared fixtures,
    including merged author/time, closed, reviews required and conflicts.
    Every denied action
    must have a hover explanation and screen-reader description. A read-only
    viewer sees the server merge reason without a method select. Allowed method,
    delete-branch and auto-merge values match the server.
    Record fixture-based unit rendering separately from browser/native evidence.
13. On an explicitly authorized throwaway request, post a comment, preview it,
    edit it, add/remove a reaction, and delete it through confirmation. Reply
    to and resolve/unresolve a thread. Confirm each action refreshes the header
    and timeline without reloading the page. Copy/Open on host currently use
    the request URL; the comment model has no host permalink.
14. Add single-line and reverse-drag multi-line inline comments; insert a
    suggestion with the selected new source lines. Verify amber pending cards,
    Edit/Remove, the sticky counter in both tabs, and review submission. On
    GitLab, Overview shows **Approve** under the description, with **Approval
    is optional** when the host requires none, otherwise the approved/required
    count. It stays inactive with **Loading…** until the live detail succeeds,
    and choosing it approves the current head without opening **Review** and
    without posting pending inline comments. After approval the same control
    is **Revoke approval**. A denied account still sees **Approve** with the
    server reason. Reload while comment, reply, edit, inline, and review-summary
    drafts exist and verify they survive. Unrenderable lines must retain draft cards below the diff.
15. Exercise full-success, zero-landed, and partial-landed review receipts with
    controlled fixtures or an authorized test host. Only failed comments stay
    pending by path, line, and body; identical duplicate drafts retain their
    candidates for inspection. A posted GitLab summary must not be reposted on
    retry; a GitHub atomic rejection must keep its unsent summary. Move
    the head before submission: retain every draft and show the actionable
    stale-head Refresh toast. Verify own-approval/request-changes denials and
    all unsupported host actions stay visible with server reasons.
16. Where supported, apply one suggestion with an optional commit message and
    a batch selected from Files. GitHub offers the Apply denial reason and Copy.
    Dismiss a review only after a required message and confirmation; re-request
    an eligible reviewer. Confirm the environment mutation capability also
    disables review writes while Refresh and Retry remain available. Minimize a
    GitHub thread comment, refresh, then verify Unminimize and collapsed content.
    Refresh after removing a commented file from the diff and verify saved
    drafts remain editable/removable. Repeat a failed action and verify draft retention.
    Capture gate output, React/UI reviews, and browser/native evidence separately.
17. On an authorized disposable request, edit title and description, preview,
    save, cancel with Escape, and reload drafts. Add/remove reviewers, assignees,
    labels and milestones; verify author exclusion, milestone Clear, five-second
    Undo, no write on expiry, lazy vocabulary reads and 300 ms truncated search.
    Delay the detail refresh after Undo: a still-checked label must remain disabled
    until the refreshed unchecked value arrives, and the next click must re-add it.
    If that read fails, show Retry rather than allowing edits against stale metadata.
    Change the base with pending comments: Cancel and host failure preserve them,
    and only success clears them after the warning.
18. On that disposable request, verify repository merge defaults, single-method
    hiding and GitLab's project-method text. Confirm Merge names the method,
    target, branch deletion and auto-merge choice. On GitLab, verify the title is
    **Merge merge request** and recovery/toast, list-state and checkout labels use
    **merge request**; the **Pull Requests** module name stays unchanged.
    Auto-merge reports enabled
    rather than merged and supports Disable. GitHub disables bypass during
    auto-merge with its reason; GitLab permits the combination only when both
    server permissions allow it. Verify bypass confirmations name the consequence.
    Move the head before confirming: verify stale-head Refresh and retained drafts,
    with no automatic retry. Simulate a partial-progress failure and preserve the
    exact server message. Verify Update branch merge/rebase and GitLab Skip CI.
19. Verify Mark ready/Convert to draft, Close/Reopen without unnecessary confirmation,
    GitHub lock-reason selection and GitLab reasonless lock, Unlock, Copy URL and
    Open in browser. Revert a merged disposable request after its creation warning
    and verify navigation to the returned number. Delete a GitLab disposable
    request only after the host/number/permanence confirmation and verify return
    to the list. Check denied actions and the environment mutation capability,
    keyboard operation, and absence of Base UI native-button console errors.

20. On a disposable request, use **Checkout** in the selected clean checkout and
    verify its branch and head with Git. Dirty the target, then repeat: the
    server reason must appear and existing changes must remain. Repeat with an
    unfinished merge/rebase. Use another project worktree as the target and
    verify the source checkout stays unchanged. For an occupied branch, choose
    **Switch to that worktree**, then confirm the successful result path.
21. Choose **New worktree…** and verify its branch/head, persisted workspace owner,
    and sidebar row. With a conflicting local head branch, verify the new branch
    uses the request suffix and the original tip is unchanged. Repeat with the
    matching head already checked out in the main or another worktree: the name
    must be `<head>-pr-<number>`, then `-2` on further collision. A matching,
    unoccupied local branch must be reused. Once catalog data arrives, neither
    the Pull Requests toolbar nor checkout menu may keep showing **Loading worktrees…**;
    an empty snapshot also counts as loaded, and catalog errors must remain visible. **Open Git Manager
    there** must select the result worktree in the correct project/environment.
    Repeat on GitLab and a remote environment. During a deliberately slow checkout,
    close/disconnect the client: the write must finish with the expected branch,
    clean working tree and no stale `index.lock`. Reconnect and refresh to observe
    it. Repeat during branch fetch and managed worktree creation; verify the catalog
    owner appears and competing mutations/removal stay fenced until completion.
    Cancellation or deadline expiry before writing may stop preparation; after
    writing starts it only ends the wait with a neutral background message. Verify
    graceful server shutdown drains the write within the documented command/host
    bounds. Simulate Git failure, unavailable worktrees and read-only connections;
    `git status` in the reported path is the recovery starting point. No automatic
    retry or late navigation is permitted. An existing redirect toast must honor a
    subsequent mutation denial. Record browser/native checks separately from CLI-stub tests.

### Remote-hosted Git Manager run

Create the disposable fixture on the remote host, attach that environment by
following [Remote access](../user/remote-access.md), add the three fixture
projects to that environment, and repeat the complete local packaged-application
procedure and zero-telemetry observation against the remote-owned project. This
second run is required. Verify these remote deltas and no substitutes:

- the panel forwards the server-owned `workspaceRoot` as an opaque value, never
  resolves it through the client filesystem, and never substitutes a local
  path;
- deliberately disconnecting the environment renders **Git Manager
  Unavailable**, names the disconnection reason, and does not re-dial the
  environment;
- reconnecting the environment transparently re-attaches status and operation
  subscriptions, after which external repository changes and streamed operation
  events resume without reloading or reopening the panel; and
- an attached compatibility server whose descriptor omits one optional
  capability degrades only that surface. Prefer omitting
  `gitManagerPartialStaging`: the diff must remain readable, its line/hunk
  mutation controls must explain that partial staging is unsupported, and the
  rest of the panel must remain usable. Use an actual advertised descriptor,
  not a client-side edit; record the server build and descriptor in the report.

## Packaged visual validation

Use Codex Computer Use to operate the exact packaged executable. Before launch,
prove no conflicting BiBCode instance is running. Use disposable application
data and platform-specific renderer isolation without overwriting a user
profile.

Capture original-resolution screenshots at normal and minimum supported window
sizes. Cover relevant:

- Add Project and environment presentation, including the left-panel environment
  rail (Local entry with its WSL sub-picker where applicable, saved-server
  entries with status dots, the selected entry framed by a primary-colored
  ring so the active host is unambiguous, the add/manage affordances) and the
  environment
  context card with its ⋯ menu when a remote environment is selected—verifying
  that switching rail selection filters the projects panel without interrupting
  running sessions on other environments—and the cross-environment **Agents**
  nav row below Search, whose unread badge aggregates across environments and
  is hidden when nothing is unread;
  verify that it opens the full Agents view, selecting a row shows its live
  session in the right pane, the back arrow returns to the normal view, and the
  per-row jump-to-workspace action returns to the normal view and re-points the
  rail to that row's environment;
- the **Environments | Repositories** switch: open one repository as a project
  on Local and on a remote environment. Switch the left panel to
  **Repositories**: the environment rail disappears, the BiBCode brand stays where it was
  beside the sidebar toggle (not under it), and one card named after the
  repository lists both environments with their state and path; an offline
  environment's entry is dimmed. Its `…` menu shows the normal project menu for
  that environment. Change the remote checkout's `origin` with
  `git remote set-url` and, after the next catalog refresh of that project
  (window focus, a Git action, or reopening the project), confirm the entry
  moves to its new card. Collapse one environment's entry and confirm the
  other environment's entry stays open. Switch back to **Environments** and
  confirm the rail returns with the same environment selected;
- provider settings and provider/terminal action menus;
- status-bar Claude/Codex usage with different accounts or usage values on local
  and remote servers: switching the environment rail changes the displayed
  usage, manual refresh targets that server, and an unavailable or still-loading
  remote never displays the local account's values; keep a refresh in flight
  while switching to verify that its completion does not replace the new
  selection's usage;
- discovered and adopted external worktrees;
- workspace cards and sidebar menus at the 422 px default width, in light and
  dark: each project (Environments) or repository (Repositories) reads as one
  lifted card on the list background, environment bands are distinct inside a
  repository card, and chat rows and secondary text stay legible in dark; every status glyph (needs approval, waiting for your answer, working,
  failed, plan ready, finished not opened, idle), the branch line with an open
  and a merged request, a dirty dot and a running terminal, the session line,
  **N more chats**, the hidden-worktree line, and the focus ring; **Shift+F10**
  on a focused card opens exactly one menu at the card; separators between menu
  groups in the native menus (macOS, Linux) and the in-app menu (Windows,
  browser), never two in a row and never at an edge; **Pull** and **Copy Branch
  Name** on worktree and primary cards; **Show Hidden Worktrees (N)** on an
  expanded project with discovery; while a session runs or starts in a worktree, its
  card's **Delete Worktree…** is disabled with "Stop the running session before
  deleting this worktree." (the native menus append it to the label), and the
  removal dialog, opened from **Settings → Archive → Delete**, shows its delete
  action disabled with the same visible reason until the session stops;
  on an isolated server with a fake provider holding a turn open, a scripted
  `worktree.getRemovalPlan` followed by `worktree.remove` returns
  `WorktreeRemovalError` with reason `session-running` and leaves the checkout
  intact; a second client starting the turn after the dialog loads its plan
  produces the same visible refusal sentence when the server rejects deletion;
  after the session stops, deletion succeeds; capture the refusal in light and
  dark; and
  keyboard operation of the in-app menu;
- the left panel after the typography sweep: an expanded project without
  worktrees shows only its primary card; a saved server's project headers show
  no cloud icon, while a WSL project under **Local** keeps its container icon;
  the **Projects** label is sentence case; **Settings** matches the nav rows;
  and no left-panel text is smaller than the cards' 12 px lines or letter-spaced
  (the stage badge stays uppercase);
- Create Worktree exact local and remote ref selection: the exact value appears
  once, the derived name remains correct, and a remote-to-local race succeeds
  without duplicate branch creation;
- Git Manager opened from the project header, its worktree selector, a Changes
  list with the partial-staging gutter, History with a selected commit diff, the
  branch dropdown, fetch/pull/push/force-with-lease sync states, the native
  stash list, and an in-progress/conflicted repository state;
- Pull Requests sidebar entry, settings switch/host list, list tabs and filters,
  pagination, unavailable/auth recovery, detail tabs, editors, metadata Undo,
  merge/state confirmations, and draft recovery; include the
  [read-only list/detail/files smoke](#pull-requests-web-shell-validation) with
  route, selected tab and real patch evidence;
- thread creation, switching, persistence, and streaming;
- the [durable message queue](#durable-message-queue): FIFO cards, native steer,
  Cancel restoration, reload and restart persistence, promotion, Stop drain,
  holds, and unsupported-provider explanations;
- terminal input/output and panel switching, including reopening the global right panel after a
  sibling chat suppresses a previously active Activity surface;
- Files tree nesting, mutations, and moves: one row per directory with its own
  expand arrow rather than merged directory chains, **New File…** creating the
  entry in the clicked nested folder while expanded folders stay expanded, a
  drag-move onto a folder row and onto the tree root, and a refused move (a name
  the target already holds) reported as an error with the tree resynced;
- Files Download and Upload: right-click a folder → **Download** → pick a
  destination folder → verify the resulting `.zip`; right-click the tree
  background → **Upload Files…** → pick a file → verify it appears in the
  workspace root; upload a file whose name collides with an existing entry and
  verify the Replace prompt, then confirm it and verify the replacement lands;
  download a file whose name is not ASCII (for example `résumé.txt`) and verify
  the saved file keeps that name rather than a mangled or numbered one; with a
  workspace on a Linux or macOS host, create a file named `report:v2.txt` there
  and verify it uploads and downloads unchanged, and that the Windows desktop
  app saves that download as `report_v2.txt` rather than refusing it; against a
  Windows-hosted workspace, attempt to upload a file named `report:v2.txt` and
  verify the refusal names the rule instead of failing silently;
- Remote folder download recovery: leave the native destination picker open
  longer than five minutes, then confirm and verify a valid ZIP. Cancelling the
  picker must not mint a download URL. With a controlled disconnected server,
  expired token, missing folder, or archive read failure, verify an actionable
  error and a corresponding local frontend/server log entry, with no signed
  transfer URL in either. Restore the fixture and retry; no partial ZIP may be
  reported as success or overwrite an existing download;
- Files picking up a file created in the workspace by another tool while the
  packaged application stays open, both on its own within seconds and
  immediately via **Refresh**; while a controlled rescan is pending, verify the
  visible **Refreshing…** state and repeated-request coalescing; on Windows,
  cover a WSL-hosted workspace as well as a native one, because
  directory-timestamp fidelity differs across that boundary;
- Files path-kind changes: replace a file with a directory containing nested
  entries while the tree is open, then select a descendant and replace its
  directory with a file. Exercise automatic refresh and **Refresh**; neither
  may show an application error or require reload. Surviving expanded folders
  and selections stay usable, and sibling rows still open normally;
- Activity subagents and background tasks, including elapsed time and keyboard
  navigation;
- with **Settings → General → Theme** on **System**, switch the operating system
  between light and dark while the app shows the home screen and again while it
  shows a thread: the window follows each switch without a reload; with
  **Light** or **Dark** selected, the app ignores the switch;
- **Settings → About → View diagnostics → Profiler** in light and dark: the environment
  selector with a remote environment connected; the switch off (**Off. Turn on
  to start recording. Crash checks for the previous run still happen.**),
  **Starting...**, and recording (**Recording since … · … MB · … events**, plus
  **… dropped** when any were); **Not recording.** with "Another BiBCode app or
  server is already recording to this profiler folder. Close it, then restart
  this one." when a headless `bibcode --base-dir <root> serve` on the same
  disposable data root started first and holds the folder; **Open folder** enabled only for the primary
  environment on desktop and **Export bundle** only for the primary
  environment, each otherwise disabled with its stated reason; **Include crash
  dumps** and its warning, disabled with Export bundle's reason when it is;
  where an environment runs a BiBCode older than the profiler, only
  the disabled switch with "This server's BiBCode version has no profiler.
  Update the server to record."; and the findings list, empty (**No findings yet.**)
  and after the [Profiler crash smoke](#profiler-crash-smoke), with each
  finding's severity icon, title, count, and last seen. While an environment records,
  the status bar shows the **Profiler** segment; its tooltip lists the
  recording environments, clicking it opens Diagnostics on the first of them,
  and it disappears once every profiler is off;
- responsive menus, overlays, narrow panels, and focus states; and
- loaded interaction without stale ownership, duplicate events, or runaway
  process growth.

The default packaged suite runs all of its spec files in one embedded-driver
session and resets client connection state before every test. After refresh,
it waits for a new main-webview page-load completion from the test-only Tauri
logging plugin in `userdata/logs/server.log`. Navigation generations distinguish
old completions and loading pages without sending WebDriver JavaScript into a
document being replaced. Once that native signal establishes the replacement
document, the harness waits for its DOM load event as well; WebKitGTK can still
report an interactive DOM at native completion. These markers carry no page URLs and are absent from
production builds. The disposable fixture enables only this lifecycle target
even when its parent process uses a warning-only filter. The maintained `document-navigation.e2e.ts` scenario reloads
three times and checks a complete, distinct document after each native finish.
The suite restores its motion guard after scenario reloads. Runtime motion
assertions explicitly select native transitions and restore the guard afterward.
The suite disables WebDriver command retries. Treat reporter hook errors, retries, and timeouts as
test failures even when the individual scenarios are reported as passing.
On macOS, copy the signed DMG payload to an isolated installation directory,
compare its executable bytes, and verify its signature before running the suite.
The Tauri service captures backend stdout/stderr in the artifact directory.
macOS CI also retains `native-crashes.log`, a bounded summary of application
crash types, termination reasons, and faulting symbols; it excludes raw memory
and the rest of the diagnostic report. Inspect these logs when the app or
embedded driver exits unexpectedly. A missing provider-terminal activity dock
also records structural pane, surface, and activity-host geometry to distinguish
layout visibility from provider-observation failures.
Window resizing polls the actual viewport and native window until the requested
size is reached, including when GTK startup dimensions settle independently.
It does not depend on animation frames, which can stop in occluded macOS
webviews. The host display may cap
large requested sizes; keep the scenario layout assertions and screenshots.
Terminal setup waits for the workspace menu trigger before opening it. Pierre
fixture replacement sends select-all to the shadow editor itself and requires
the editor to handle both selection and input before verifying edits and undo.

The maintained Pierre acceptance scenario
(`apps/desktop/e2e/specs/pierre-diffs.e2e.ts`) uses that same isolated launcher
and disposable application state. It creates its own committed editor fixture
and three independent working-tree hunks, then verifies the packaged app's
unified and split renderers, line hover utility, line selection, conversation
diff card, partial stage and unstage controls, editable-file input, and undo
history after switching to another right-panel tab and back. It is part of the
default suite. To rerun only this scenario against the exact package selected
by the native platform page, set
`BIBCODE_E2E_SPEC=./specs/pierre-diffs.e2e.ts` for the
`vp run test:ui:desktop` invocation. Retain its named `pierre-*.png` screenshots
with the other bounded UI artifacts; do not put fixture state or screenshots in
the repository.

At final packaged shutdown, inspect the raw worker and server logs. Provider and
terminal owners, operational logs, orchestration, and the SQLite worker must all
close without a retry, timeout, or dependency on stale cloned handles.

For every screenshot record the absolute evidence path, UI state, and review
finding. Inspect the full image and focused crops for clipping, overflow,
spacing, truncation, contrast, icon/text alignment, focus rings, tooltip
placement, disabled states, stale labels, and unintended movement. Keep
diagnostic frames separate from acceptance evidence.

Authentication-dependent scenarios must be reported as unavailable when the
native host has no suitable credentials. Never copy secrets into evidence.

### Integrated browser link routing

Run on the packaged desktop app (macOS 14+, Windows, Linux) with a disposable
thread whose workspace contains `index.html`, a `.pdf`, and a `.txt`. Repeat
each row with **Settings → General → Open links in** set to **BiBCode browser**
and **System browser**; the modifier must always select the other target.
Record, per row, where the link landed (BiBCode browser tab, system browser,
editor, or notice). In a browser tab (web mode), **Open links in** is not shown.

- Chat `http(s)` link: plain click, Ctrl/Cmd-click, Shift-click, Alt-click, and
  middle-click. Right-click offers **Open in BiBCode browser**, **Open in
  system browser**, and **Copy link**; other link types show no custom menu. An `.html` or `.pdf` file chip opens in the BiBCode browser, and
  any modifier-click opens the editor. A file outside the workspace shows the
  notice with **Open in editor**.
- Terminal URL: plain click does nothing; Ctrl/Cmd-click opens the setting's
  target; Ctrl/Cmd+Shift-click opens the other.
- OSC 8: `printf '\e]8;;https://example.com\e\\example.com\e]8;;\e\\\n'` opens
  on Ctrl/Cmd-click with no menu (text matches destination). With
  `printf '\e]8;;https://example.com\e\\docs\e]8;;\e\\\n'` a menu shows
  **Open …** and **Copy link**; plain click does nothing.
- Terminal paths: `index.html`, an absolute `.html` path, a `file:///…/x.pdf`
  URL, and a `.txt` path. HTML and PDF open in the BiBCode browser and with
  Ctrl/Cmd+Shift in the editor; `.txt` opens in the editor. On any platform,
  `\\server\share\x.html`, `//server/share/x.html`, and `\\.\C:\x.html` print
  "Network paths can't be opened from the terminal." and open nothing;
  `file://server/share/x.html` prints "Unable to open this file link." A
  relative `x.html` under a UNC working directory (WSL `\\wsl.localhost\…`)
  still opens. With the thread's environment disconnected, an HTML path shows
  "Couldn't preview this file" with **Open in editor**.
- Preview tab `target=_blank` link and `window.open` popup open a new tab in the
  same thread; `window.open('about:blank')` opens nothing. Git Manager pull
  request title opens in the system browser, not nowhere.
- Agent `preview_open` opens a tab, shows it (also with `show: false`), and
  reports success; `preview_navigate` then loads a new URL in it. Snapshot,
  click, and type are still unsupported and report so.
- Loopback links (`http://localhost:3000`, `127.0.0.2`, `0.0.0.0`, `[::]`,
  `app.localhost`): from a local thread they open locally. From a LAN,
  tailnet, WSL, host-name (`devbox`), or desktop SSH thread they open through
  the preview gateway (see [Preview gateway](#preview-gateway)), never this
  computer's localhost. From a BiBCode Connect or public-IP thread, or an SSH
  thread in a browser tab, they show "Can't open this address here" with "This
  address is on <environment>, not this computer. Opening its ports from here
  isn't supported yet.", the address, and **Copy link** for both targets. A
  disconnected thread's notice asks to reconnect the environment.
- A failed system-browser open (no handler registered) shows the link with
  **Copy link**.
- Agent-written asset response: `curl -sI '<signed /api/assets URL>'` for an
  HTML file shows a `content-security-policy` header of
  `sandbox allow-scripts allow-forms allow-popups`; for SVG, XML, and any other `+xml` type, of
  `default-src 'none'; style-src 'unsafe-inline'; sandbox`; a `.txt` or raster
  image asset has none. In the BiBCode browser, a script
  in the HTML reads `localStorage` and `document.cookie` as unavailable.
- Port attribution: start `python3 -m http.server 8123` in a thread's terminal;
  the port appears in that thread's and terminal's discovered ports (port
  card), and not under another thread's terminal.

Pending native checks (not covered by automated tests; mark each unavailable if
the host cannot run it): Ctrl+Shift-click while a mouse-tracking TUI (Codex or
opencode) runs must invert the target, not the TUI's selection; an OSC 8 link
printed by a real CLI (a CLI that emits `https` hyperlinks) behaves as above;
Linux/Wayland preview popups open as tabs; Windows UNC refusal as above.

### Preview gateway

The gateway is described in
[Remote access architecture](../architecture/remote.md#preview-gateway) and the
[user guide](../user/workspace-ui.md#previewing-the-servers-dev-servers). The
SSH preview and brainstorming-companion procedures live in
[Desktop-managed SSH environments](./ssh-environments.md#preview-gateway-over-ssh).

Use a server on another machine that the client reaches on a LAN, tailnet, or
WSL address (a bearer environment; see
[Remote access](../user/remote-access.md)), and a disposable thread on it. In
the thread's terminal, run `python3 -m http.server 8123 --bind 127.0.0.1` and
`python3 -m http.server 8124 --bind 127.0.0.1`. Record the client (desktop or
browser tab), the environment's address, and each gateway port.

- **LAN preview.** On desktop, clicking `http://localhost:8123/` in the chat
  opens a BiBCode browser tab that shows the listing at
  `http://localhost:8123/`. With **System browser**, the system browser lands on
  `http://<server address>:<gateway port>/`. `https://localhost:8123/` shows
  the HTTPS notice, and a port with nothing listening shows "Nothing is
  listening on port <port> on <environment>."
- **Bind and reach.** With the server started on `--host 0.0.0.0`, the gateway
  port listens only on the address the client used (`ss -ltn` on Linux,
  `netstat -an` elsewhere), not on `0.0.0.0`. Reach the same server through a
  reverse proxy on its host, or through Tailscale Serve, and click the link
  again: the preview is refused as a proxied address and no gateway port opens.
- **Browser-mode cross-site bootstrap.** Open the BiBCode UI in a browser from
  a different host than the environment's address, so the UI and the gateway
  are different sites (for example, the UI served by this computer and the
  environment on a LAN server). Click the 8123 link: a blank tab opens at once,
  passes through `/__bibcode/bootstrap`, and ends on
  `http://<server address>:<gateway port>/` with the listing. The browser's
  developer tools list a `bibcode-gw-<gateway port>` cookie for that origin
  that is `HttpOnly`, `SameSite=Lax`, and `Path=/`. A plain reload of the
  tab still shows the listing, which proves the cookie survived the
  `location.replace` hop.
- **Browser-mode panel preview.** Open the BiBCode UI over plain HTTP on the
  environment's own address (`http://<server address>:<server port>`). Click
  the 8123 link: it opens in the Browser surface, not a new tab, and the address
  bar shows `http://localhost:8123/`. Open a directory in the listing: the
  address bar follows it. In a browser with the Navigation API (record the
  browser and version), Back and Forward move inside the frame and leave the
  BiBCode tab where it is; without it they stay disabled. The frame's document
  response in the developer tools carries
  `Content-Security-Policy: frame-ancestors 'self' http://<server address>:<server port>`
  and no `X-Frame-Options`. Serve a Vite app on the environment and preview it:
  an edit hot-reloads inside the frame. Serve a page that sends
  `X-Frame-Options: DENY` (for example a small Python handler): it still shows.
  Typing `https://example.com/` in the address bar shows "Can't show this page
  here" with **Open in new tab**, which opens it in a new tab. The capture
  button is gone, and the more menu holds only the device toolbar; the
  toolbar's Reload reloads the frame. Modifier-click the 8123 link: it opens in a new tab.
  On a framed page, a plain click on a link to another site (add one to the
  served directory, for example an `index.html` linking `https://example.com/`)
  opens that site in a new tab and leaves the frame on the page; a link to
  `http://localhost:8124/` opens as a new BiBCode browser tab showing the 8124
  listing. With the address bar focused, Ctrl/Cmd `+` zooms the browser tab.
  Narrow the window to 980 px or less: the preview shows inside the right-panel
  sheet, and the more menu opens above it.
- **Browser mode on the server's machine.** Open the UI at
  `http://localhost:<server port>` on the server's own machine and click the
  8123 link: it opens in the Browser surface through a gateway port on
  `localhost`, not directly on 8123. Modifier-click it, or click it in a
  terminal that belongs to no thread: it opens `http://localhost:8123/`
  directly in a new tab.
- **Browser mode over HTTPS.** Open the UI over HTTPS (Tailscale Serve or a TLS
  reverse proxy). The `+` menu has no Browser, and the preview shortcut shows
  "Previews open in a new tab". A gateway link is refused as a proxied address
  (above).
- **Cross-port 403.** Open the 8124 link too, so two gateway origins share the
  host. In the 8124 tab's developer console run
  `fetch('http://<server address>:<8123 gateway port>/', {method: 'POST', mode: 'no-cors', credentials: 'include'})`
  and `new WebSocket('ws://<server address>:<8123 gateway port>/')`. The
  network panel shows `403` for both, and the 8123 server's terminal logs no
  new request. Over `curl`, with the 8123 cookie copied from the developer
  tools, a `POST` whose `Origin` is the 8124 gateway origin, or that has no
  `Origin`, returns `403` with "This request didn't come from the preview
  itself, so the gateway blocked it." The same `POST` with
  `Origin: http://<server address>:<8123 gateway port>` reaches the server
  (Python answers `501`), and a `GET` with the 8124 `Origin` returns the
  listing. Do not record the cookie value.
- **Revocation.** In a browser-mode tab that holds a WebSocket (a Vite dev
  server, or the brainstorming companion run on this environment), revoke the
  client's session or device on the host's **Settings → Remote Servers →
  Share** tab. Within 30 s the WebSocket closes; reloading the tab shows "This
  preview link expired. Go back to BiBCode and open it again." After the
  client pairs again, opening the link again from BiBCode works.
- **Commands that open a browser.** In the thread's terminal, `BROWSER` and
  `BRAINSTORM_OPEN_CMD` are `bibcode-open-url`. Looking it up
  (`command -v bibcode-open-url`, or `where.exe bibcode-open-url` on Windows)
  names `runtime/open-url/` under the server's state directory; on Windows
  that directory holds `bibcode-open-url.exe` and no `.cmd` or extensionless
  shim. `"$BROWSER" http://localhost:8123/` prints nothing and exits 0; on
  desktop the address opens like a clicked link, and in a browser tab the "A
  command wants to open <address>" bar appears. With two clients showing the thread, only one opens it; a client
  in a hidden window loses to a visible one. With every client on another
  thread, after 2 seconds one visible client shows "A command in “<thread title>”
  wants to open <address>", and **Show thread and open** switches to that thread and opens it. With the token unset
  (`env -u BIBCODE_OPEN_URL_AUTH "$BROWSER" http://localhost:8123/`), it prints
  the address and exits 0. With no BiBCode client connected to the server, it
  also prints the address and exits 0. `"$BROWSER" file:///etc/hosts` writes
  `bibcode open-url: expected an http(s) URL` and exits 2.
- **Agent open in a browser tab.** With only a browser-mode client on the
  thread, ask the agent to call `preview_open` for `http://localhost:8123/`:
  the "Agent wants to open <address>" bar appears and the agent's result is
  `pending-user`. On a UI served over plain HTTP on the environment's own host,
  **Open** shows the page in the Browser surface.

- **Relay preview (BiBCode Connect).** Needs a deployed relay stage with this
  change and an environment linked to it (re-link an existing one, or open
  BiBCode on it signed in so the background re-link runs; its **Share** tab's
  link state then reports preview hostnames). From a relay-connected desktop
  client, click `http://localhost:8123/` in the thread's chat: the BiBCode
  browser shows the listing, its address bar shows `http://localhost:8123/`,
  and the developer tools (or a `curl -sI` of the bootstrap URL) show the page
  on `https://<environment hostname>-p1.<zone>` with a `bibcode-gw-<port>`
  cookie that is `Secure`, `HttpOnly`, and `SameSite=Lax`. Serve a Vite app and
  preview it: an edit hot-reloads (the WebSocket passes through the relay).
  Reload re-bootstraps. Close the tab and reopen the link: it loads again. From
  a browser-mode client connected through the relay, clicking the link opens a
  new tab on the preview hostname; typing the address into the Browser panel
  shows "<environment> is reached through BiBCode Connect, so its previews
  can't show inside this page." Keep four previews open and open a fifth
  port: "Too many previews of <environment> are open over
  BiBCode Connect. Close one, then try again; one closed in a web browser takes up to 10 minutes to free up." With the Profiler on, end the `bibcode-connect` process with `kill -SEGV` (a plain `kill` is a deliberate stop and never counts): within 5 s a `subprocess.crash` finding labelled by the connector's executable appears, and a relay preview open fails with the "aren't set up" notice. Restart the environment's server
  without re-linking: the relay connection comes back on its own. Record the
  relay stage, environment, and preview hostnames, never cookie values.

Pending native checks: Windows `bibcode-open-url.exe` started from `cmd` and
PowerShell (the desktop build attaches to the parent console; record whether
the prompt waits and the exit code is visible); whether a Codex agent's
commands receive `BIBCODE_OPEN_URL_AUTH` (from a Codex tool call,
`[ -n "$BIBCODE_OPEN_URL_AUTH" ] && echo present || echo missing`; never print
the value); whether Codex's `workspace-write` sandbox makes `"$BROWSER"` print
the address instead of opening it; a session older than 8 hours falls back to
printing.

## Profiler crash smoke

Run this on every native platform when a change touches the
[Profiler](../architecture/overview.md#profiler), crash capture, symbols, or
desktop packaging, and for release qualification. The CI `crash-smoke` job
proves the monitor and symbolication with a small test binary; this procedure
proves them in the packaged desktop app. The platform page names the build
command, the executable, the crash trigger, and its manual checks.

Run the focused profiler coverage first:

```sh
cargo test -p bibcode-server --lib profiler:: -j 2
cargo test -p bibcode-server --test profiler_runtime --test profiler_sentinel --test profiler_exits --test profiler_worktree_audit --test profiler_cli --test crash_smoke -j 2
cargo test -p bibcode-desktop -j 2
vp test run packages/contracts/src/profiler.test.ts apps/web/src/profiler apps/web/src/components/settings/ProfilerSection.test.tsx apps/web/src/components/status-bar/ProfilerRecordingSegment.test.tsx
```

Root runs leave the component tests uncompiled, so also run them from `apps/web`
with `vp test run --project unit` and their paths without the `apps/web/`
prefix, as for the other
[compiler-sensitive](./README.md#web-unit-tests-and-the-react-compiler) files.

Use a fresh data root for every process in the procedure, by setting
`BIBCODE_HOME` to an absolute test-owned directory, together with the
platform's renderer isolation. The profiler directory is then
`$BIBCODE_HOME/userdata/profiler`. Run `profiler` commands with the same
executable and the same `BIBCODE_HOME`: `<executable> profiler status --json`.

1. **Enable.** Launch the app, open **Settings → About → View diagnostics →
   Profiler**, keep
   the local environment selected, and turn on **Record profiling data**. The
   status line moves from **Starting...** to **Recording since …**.
2. **Capabilities.** Within 10 s, `capabilities.json` exists. Record it and the
   `capabilities` of `profiler status --json`. Expect
   `nativeDumps: "available"`, `osCrashRecords` as the platform page states,
   and `storage: "ok"`. Confirm that exactly one `--crash-monitor` process runs
   with the app process as its parent. A `nativeDumps` of `unavailable` or
   `disabled` means no monitor is armed, so the crash-test variable does
   nothing and no dump is written: record it as unavailable evidence with its
   cause. Where the platform page names another crash trigger, use it to cover
   the OS crash record in step 5.
3. **Crash.** Quit the app normally, then start it again with
   `BIBCODE_PROFILER_CRASH_TEST=1` set for that launch only. The setting is
   already on, so the monitor arms during startup and the process faults about
   5 s later. Child processes inherit the variable: a BiBCode executable
   started from the app, such as `bibcode` in an integrated terminal, with its
   own profiler on would fault too. Never set it in a shell profile, and start
   every later launch without it.
4. **Dump.** `crashes/` holds one new `<id>.dmp` and its `<id>.json` sidecar,
   both readable only by the current user. Record the dump size in bytes. The
   sidecar's `processKind` is `desktop`, its `parentPid` is the crashed
   process, its `bootId` is the current boot, and its `version`, `os`, and
   `arch` match the build;
   `profiler crashes --json` lists it.
5. **Restart.** Start the app without the variable. The Diagnostics Profiler
   section lists a `crash.native` finding (error) for the previous run, and
   `profiler findings --kind 'crash.*' --json` shows `evidence.crashId` equal to
   the sidecar `id`, listed in `evidence.crashIds` too, and `evidence.faultModule`
   naming the faulting module. The same run has no second crash finding, such as a
   `crash.unclean-exit`. Where the platform kept an OS crash record, its frames
   may be attached to the evidence.
6. **Symbolicate.** Run
   `profiler symbolicate <id> --symbols <store> --json` with the symbol store
   that the platform page's `--symbols` build wrote. The crash reason is the
   write to an invalid address (`SIGSEGV`, `EXC_BAD_ACCESS`, or an access
   violation on Windows), and the stack names
   `bibcode_server::diagnostics::profiler::crash::crash_test_fault`. For a
   published release, run it with `--download` instead and record the download
   time. A build that was never released answers "the release publishes no
   symbols at <url>"; record that and use the local store.
7. **Clean up.** Turn the profiler off and confirm the monitor process exits;
   quit the app and confirm nothing from the run survives. Remove only the
   test-owned data root.

Record each value in the execution report's profiler section. A dump without a
`crashId` finding, a duplicate crash finding, or a monitor that outlives the
app fails the smoke.

## Non-native compatibility audit

Review shared and platform-gated code for every supported non-native host. Run
host-independent source-inclusion, contract, fixture, and cross-target checks
where they are supported. Confirm a native fix does not introduce:

- foreign path normalization or separators in shared code;
- an unsupported platform API in an unguarded module;
- platform-global environment or CWD mutation;
- deleted remote functionality when presentation alone is hidden;
- changed provider visibility outside the current product contract;
- lost process admission, cancellation, kill, wait, reap, or peer isolation;
- test serialization or timing workarounds; or
- dependency, lockfile, generated, or vendored-subtree drift.

If an SDK, linker, signing identity, or system service is unavailable, report
the exact limitation. Do not claim the non-native host passed.

## Failure classification and repair

On the first distinct failure:

1. stop the broad run and preserve its exact output;
2. reproduce once with the smallest relevant command;
3. classify it as product, test fixture, package/build, or environment;
4. trace the owning state and lifecycle boundary;
5. form a falsifiable hypothesis;
6. add a deterministic behavioral RED before a real repair;
7. implement the smallest coherent fix while preserving every platform;
8. rerun focused tests at relevant concurrency widths;
9. rerun affected package, static, native package, and visual gates in
   proportion to the boundary; and
10. update living architecture and these runbooks when their contract changes.

Do not repair a tested latency contract with sleeps, yields, broad
serialization, timeout widening, global locks, global process mutation, retry
loops that hide the failure, or weakened assertions. Distinguish honest
load-sensitive contract failures from environment starvation with positive
owner/readiness/cleanup evidence.

An integration test whose contract is ownership, output, or cleanup rather than
product latency may use one fixed, absolute, test-only observation deadline.
That deadline must bound the complete test owner, retain positive
readiness/output/reap assertions, and leave both the production deadline and a
dedicated production-deadline regression unchanged. Do not extend a deadline
that is itself the behavior under test.

## Cleanup

Resolve exact ownership before any destructive action. Stop only processes
launched by the run, using PID plus executable, creation/start identity, and
fixture/worktree association where available. Unmount only test-owned package
mounts. Remove only exact disposable repositories, worktrees, profiles,
artifact directories, and temporary roots created by the run.

Capture before/after process and temporary-root snapshots. Report pre-existing
survivors without killing or deleting them. Prove no scoped desktop, server,
provider, terminal, WebDriver, package, or fixture process remains.

## Final Git audit

Run:

```sh
git diff --check
git status --short
git log --oneline -10
```

Review the complete diff for unrelated edits, debug output, generated files,
dependency/lockfile drift, vendored changes, platform leaks, and missing living
documentation. Re-sync CodeGraph when source changed and it is usable under
`AGENTS.md`.

Do not push, merge, open a pull request, or publish an artifact unless the
current request explicitly authorizes it.

## Reporting

Copy [the execution report template](./execution-report-template.md). Lead with
one result classification and keep native, compatibility, and unavailable
evidence separate. Do not claim completion from partial output or prior runs.
