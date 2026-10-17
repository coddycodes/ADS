# Provider background work, idle suspension, and upstream protocol drift

Research note, 2026-10-15, read-only. Source references are to commit
`17e46afd`. Provider CLIs inspected locally: Claude Code 2.1.300, Codex CLI
0.162.1, OpenCode 2.0.29 (mise `latest`, first on the desktop's `PATH`) and
1.18.32 (`~/.opencode/bin`), Cursor Agent 2026.10.14-941492e, Grok 1.0.44.
Another agent owns the Claude capability-probe fix described in the brief. This
note does not repeat that diagnosis. It asks whether the same failure class
reaches the other providers, and what the upstream documentation says BiBCode
must adopt.

## Findings summary

The Claude incident is one case of a general defect. The supervisor suspends
every provider session 60 seconds after a completed turn
(`apps/server/src/production/provider_runtime.rs:125`, `:8243-8252`). The only
thing that can hold the session open after the turn is the set of activity
actors and work items that the Activity projection accepted
(`:878-883`, `:5887-5897`). That set fills only when three conditions hold: the
provider is on a hard-coded list (`:6673-6675`), the user's Chat Activity gate is
on (`:8063-8066`), and the provider adapter emits activity for that kind of
work. Suspension calls `driver.shutdown()`, which kills the provider process tree
(`:10214-10221`, `:10291-10308`, `:10960-10964`, `:11165-11169`, `:11492-11506`).
It also marks the kill as a requested stop and writes no log line
(`:9184-9198`, `:10216-10218`).

The upstream CLIs now run work that outlives the turn. Most of them also **start
turns on their own** to deliver the results. Claude injects a synthetic
follow-up turn when a background task finishes. Codex goals keep working for
hours. A third-party report says Grok background subagents end the ACP turn
early, and their results arrive in a turn Grok starts later. Cursor background
shells, and (inferred) background subagents, outlive the prompt that started
them. BiBCode sees none of these provider-started turns: a provider `turn.started` neither projects `running`
nor invalidates the pending idle deadline. Only a BiBCode delivery
(`:6662`, `:7352`) or a completion (`:8211-8212`) does. So even after the Claude probe is fixed, the
turn that processes a finished subagent's result can still be killed mid-turn.

Separately, the installed OpenCode 2.0.29 removes the V1 HTTP API that BiBCode
calls. Codex 0.162.1 has also removed `thread/rollback`, which BiBCode still
sends.

## Ranked recommended changes

| #   | Change                                                                                                                                                                                                                                                                                                                                                 | Providers                              | Status                                                                          |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------- | ------------------------------------------------------------------------------- |
| 1   | Treat a provider-initiated turn as busy. On any provider `turn.started`, bump `idle_generation` and project `running` (or ask the driver whether it is busy). For Codex, also use the root `thread/status/changed` `active`/`idle` signal.                                                                                                             | all                                    | confirmed (source + docs)                                                       |
| 2   | Decouple the idle keepalive from the Activity UI gate and the per-provider allow-list. Add a provider-reported "live work" signal on `ProviderEvent` that the supervisor reads even when Chat Activity is off, or when the provider has no activity adapter.                                                                                           | all; Cursor and Grok have nothing else | confirmed                                                                       |
| 3   | For Claude, read `system/background_tasks_changed` (the full live set, v2.1.203+) as the keepalive. It needs no hook flags. Optionally also set `CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS=1` and read `session_state_changed`. This also covers `run_in_background` Bash, Monitor watches and auto-backgrounded MCP calls, which today are never tracked. | Claude                                 | confirmed (docs + 2.1.300 binary strings); stream behaviour not run here        |
| 4   | Gate OpenCode by major version. At 2.x, fail the launch with an actionable error until a V2 (`/api/*`) client exists. The V1 routes BiBCode calls are gone, and the readiness probe accepts any HTTP 200.                                                                                                                                              | OpenCode                               | confirmed by source diff; failure not yet observed in logs                      |
| 5   | Replace Codex `thread/rollback` (removed upstream) with `thread/revert {threadId, beforeTurnId}`, or report rollback as unsupported.                                                                                                                                                                                                                   | Codex                                  | confirmed                                                                       |
| 6   | Log every idle suspension at INFO with its reason and live-work counts. Today it is silent, and the kill is classified as a requested stop.                                                                                                                                                                                                            | all                                    | confirmed                                                                       |
| 7   | Move the Claude MCP bearer token off argv. Pass `--mcp-config` a 0600 file path (documented), or use `headersHelper` (documented), or use `${VAR}` header expansion with a non-reserved name (documented only for `.mcp.json`, so a test must prove it). Build the MCP URL with `local_http_base()` so a wildcard bind yields `127.0.0.1`.             | Claude (argv); all (URL)               | confirmed                                                                       |
| 8   | Cursor: advertise the ACP `subagents` client capability, so prompts wait for background subagents. Handle `cursor/task`. Cover the Cursor and Grok drivers with change 2.                                                                                                                                                                              | Cursor, Grok                           | capability name confirmed in the bundle; wait semantics inferred from changelog |
| 9   | Codex background terminals: also list child-thread terminals, add a bounded periodic reconcile while work is live, and log the latched `Unsupported` downgrade with `tracing::warn!`.                                                                                                                                                                  | Codex                                  | inferred risk                                                                   |
| 10  | Track the Claude `--bare`-default plan and the `system/init` `capabilities` and `mcp_server_errors` fields. Use them for feature detection and to catch a skipped `bibcode` MCP entry.                                                                                                                                                                 | Claude                                 | confirmed docs; impact inferred                                                 |

## A. The same failure class in the other providers

### The shared decision

- The deadline is armed only by a non-failed `turn.completed` projected by the
  event pump (`provider_runtime.rs:8209-8212`, `:8243-8252`). It is re-armed while
  busy (`:5922-5945`).
- "Busy" is evaluated in three places:
  - `session_is_confirmed_idle`: no active delivery, and a projected status that
    is neither `running` nor `starting`, with no `active_turn_id` (`:9163-9182`).
  - The live-activity check: `has_recent_live_activity()`, which needs a live
    actor or work item and a provider event within 30 minutes (`:849`,
    `:878-883`, `:5887-5897`).
  - Nothing else. In particular, the driver's own notion of a running turn is not
    consulted. For example, Codex sets `session.status = "running"` internally on
    `turn/started` (`codex/runtime.rs:2524-2537`), but the supervisor never reads
    it.
- Live actors and work items are recorded only from projected activity deltas,
  and only when `activity_capable && activity_state.enabled` (`:8063-8066`,
  `:8157-8162`). `activity_capable` is
  `matches!(provider, "codex" | "claude" | "claudeAgent" | "opencode")`
  (`:6673-6675`).
- Disabling Chat Activity clears the live set (`:6847-6854`). The test
  `idle_deadline_forgets_live_activity_when_activity_monitoring_is_disabled`
  (`:23456-23468`) codifies this, and `docs/architecture/providers.md:308-309`
  documents it. A display preference therefore decides whether background work
  survives.
- `waiting` and `unknown` do not count (`track_live_activity`, `provider_runtime.rs:967-976`;
  `docs/architecture/providers.md:306-307`).
- The living architecture already admits the gap: "The existing
  admission/projection check cannot see all unprojected provider work, including
  a provider-initiated turn" (`docs/architecture/providers.md:347-350`).

### Claude (for completeness; the fix is owned elsewhere)

1. **Work that outlives the turn.** Background work comes from four sources:
   `run_in_background` Bash commands, Monitor watches, background subagents, and
   MCP calls that ran past two minutes and were moved to the background.
   Completion arrives as `system/task_notification` and is followed by a
   synthetic turn whose `origin.kind` is `"task-notification"`
   ([SDK reference](https://code.claude.com/docs/en/agent-sdk/typescript):
   `SDKTaskNotificationMessage`, `SDKMessageOrigin`;
   [MCP](https://code.claude.com/docs/en/mcp), "Automatic backgrounding of long
   tool calls"). Self-paced `/loop` wakeups (`ScheduleWakeup`) are a further
   source ([CHANGELOG](https://raw.githubusercontent.com/anthropics/claude-code/main/CHANGELOG.md)
   2.1.299). Start signals: `task_started` with `task_type` set to `local_bash`,
   `local_agent` or `remote_agent`, plus `is_backgrounded`. Progress and state
   signals: `task_progress` and `task_updated`. End and live-set signals:
   `task_notification` and `background_tasks_changed`.
2. **Visibility.** BiBCode reads only `task_started` and `task_notification`
   (`claude/runtime.rs:1786-1810`), and only for `local_agent`,
   `remote_agent` or absent task types (`docs/architecture/activity-observation.md:371-377`).
   Claude's `background_work` capability is never set
   (`activity-observation.md:429`). Nothing reads `background_tasks_changed`,
   `task_updated` or `session_state_changed` (grep of the server crate: no hits).
   **Background Bash, Monitor and auto-backgrounded MCP tasks are therefore
   killed at 60 s even with full hook capability.** The synthetic follow-up turn
   is killed whenever it is still running when the already-armed deadline fires.
3. **Probe.** See the brief: the `--help` scan (`claude/activity.rs:381-388`)
   falls back to "unsupported" silently and is not cached on failure.
4. **Secrets and bind.** The MCP bearer token is on argv
   (`provider_runtime.rs:13351-13362`). It was observed with the token redacted
   in the live desktop's `/proc/<pid>/cmdline`, with
   `"url":"http://0.0.0.0:3773/mcp"`. The hook token is safe: it goes through
   `${BIBCODE_CLAUDE_HOOK_TOKEN}` plus `allowedEnvVars`
   (`claude/hook_sink.rs:100-109`), and the hook sink binds `127.0.0.1:0`
   (`hook_sink.rs:71`).

### Codex (`codex app-server`, JSON-RPC)

1. **Work that outlives the turn.**
   - Background terminals. The `thread/backgroundTerminals/list`, `/clean` and
     `/terminate` methods are **experimental-only** in 0.162.1: a diff of
     `codex app-server generate-json-schema` with and without `--experimental`
     shows they are absent from the stable schema. BiBCode opts in with
     `experimentalApi: true` (`codex/model.rs:557-559`).
   - Child agent threads.
   - Goals. "Codex can work independently for multiple hours without needing
     your input" ([Follow a goal](https://learn.chatgpt.com/use-cases/follow-goals)),
     and BiBCode sets goals through `thread/goal/set` (`codex/runtime.rs:1340-1346`).
     A goal continuation is a provider-initiated `turn/started`.
   - Codex reports root busy and idle through `thread/status/changed`, whose
     status is `active` (with `activeFlags`), `idle`, `systemError` or
     `notLoaded` (local schema `ThreadStatusChangedNotification.json`).
2. **Visibility.**
   - Child threads become actors. `active` maps to Running
     (`codex/activity.rs:506-517`), so they hold the session while Chat Activity
     is on.
   - Background terminals become work items, but only on two conditions.
     First, a root `commandExecution` item must wake reconciliation
     (`codex/activity.rs:630-634`). Second, the list call must name the **root**
     thread (`codex/runtime.rs:2191-2194`).
   - Reconciliation is hint-driven, with a 250 ms debounce and no periodic poll
     (`codex/runtime.rs:1643-1686`). A terminal that exits silently therefore
     holds the session until the 30-minute quiet cap. A terminal started by a
     child agent is never listed (inferred risk of a kill once the child is
     idle).
   - With Chat Activity off, `request_reconciliation` returns at once
     (`codex/runtime.rs:1697-1702`), so nothing is tracked.
   - Goal continuation turns are not seen at all (change 1). Under the shared
     decision above, **an active Codex goal is effectively capped at about
     60 s of continuation per completed turn**. This is inferred from the
     source; it was not observed in logs.
3. **Capability detection.** There is no CLI probe for chat. Method support is
   learned at run time:
   - A `-32601` response or a decode error latches `background_list_support =
Unsupported` for that runtime (`codex/runtime.rs:2255-2263`, initial value
     at `:784`).
   - The latch is reported once as a `runtime.warning` provider event and as a
     section-health error (`:2264-2276`, `:2296-2302`). It is not logged with
     `tracing`.
   - It is retried only by a new launch.
   - Transient errors mark the activity stale and retry on the next hint
     (`:2210-2213`).
4. **Secrets and bind.** The token goes in the environment as
   `BIBCODE_MCP_BEARER_TOKEN`, and `-c mcp_servers.bibcode.bearer_token_env_var`
   names it (`provider_runtime.rs:10327-10341`). The URL is the shared
   wildcard one (see change 7). There is no listener.

### OpenCode (`opencode serve`, HTTP + SSE)

1. **Work that outlives the turn.** In 1.x the `task` tool runs child sessions,
   which (inferred) the parent awaits. In 2.x, "delegated commands now run
   automatically in the background and report their results to the parent
   session" ([Migrate from V1](https://opencode.ai/v2/docs/migrate-v1/)). V2
   exposes `POST /api/session/{id}/background` and `GET /api/session/active`
   ([v2.0.29 client](https://github.com/anomalyco/opencode/blob/v2.0.29/packages/client/src/promise/generated/client.ts)).
2. **Visibility.** Child sessions become actors through `/session/{id}/children`
   and `/session/status`. Only `busy` counts as Running; `retry` and `idle` map
   to Waiting (`opencode/activity.rs:626-627`), so a child in provider back-off
   does not hold the session. For a managed server, suspension kills
   `opencode serve` and every child session with it
   (`provider_runtime.rs:11492-11506`). For a configured external server,
   `stop()` only detaches (`opencode/runtime.rs:1175-1205`), so the work
   survives.
3. **Capability detection.** A 404 on the root `/children` endpoint latches
   child support to Unsupported (`opencode/runtime.rs:2150-2163`). Other fetch
   errors mark the activity stale and retry. There is no version gate.
   Readiness accepts any HTTP response from the base URL
   (`provider_runtime.rs:15205-15209`).
4. **Secrets and bind.** The server binds `--hostname=127.0.0.1` on a free port.
   The port is picked by binding and dropping a listener, a small race.
   The password goes in the environment as `OPENCODE_SERVER_PASSWORD`
   (`provider_runtime.rs:11206-11225`). OpenCode 2.x still honours that name as a
   legacy alias of `OPENCODE_PASSWORD`
   ([v2.0.29 env.ts](https://github.com/anomalyco/opencode/blob/v2.0.29/packages/cli/src/env.ts)).
   The MCP token is sent over HTTP through `add_mcp_server`
   (`provider_runtime.rs:11255-11259`).

### Cursor (`cursor-agent acp`)

1. **Work that outlives the turn.**
   - Background shells. "Cancelling the prompt or sending a new one ends the
     wait without stopping the shell" (v2026.10.08).
   - Background subagents. "For those clients [that advertise subagents], a
     prompt waits for its background subagents"; "Clients without the
     capability keep receiving `cursor/task` notifications" (v2026.09.15). Both
     quotes are from the [Cursor CLI changelog](https://cursor.com/docs/cli/changelog).
   - The installed bundle checks `clientCapabilities.subagents` or
     `clientCapabilities._meta.subagents`
     (`~/.local/share/mise/installs/cursor-agent/2026.10.14-941492e/dist-package/6136.index.js`).
2. **Visibility.** There is none. Capabilities are hard-coded to `none()`
   (`provider_runtime.rs:10795`), and no activity is ever attached
   (`:10951`). Suspension is `kill_child` (`:10960-10964`). BiBCode advertises
   only `fs` false and `terminal` false (`cursor/runtime.rs:245-246`), so it gets
   the no-wait behaviour (inferred). Any background shell or subagent dies at
   60 s.
3. **Capability detection.** There is no probe. The `acp` subcommand still
   exists but is hidden from the top-level `--help` (local `cursor-agent acp
--help`).
4. **Secrets and bind.** The MCP header travels in ACP `mcpServers` over stdin
   (`provider_runtime.rs:15047`). Cursor's ACP page says ACP loads MCP
   servers from `.cursor/mcp.json` ([ACP](https://cursor.com/docs/cli/acp)), so
   whether the per-session `mcpServers` entry is honoured is **unverified**.

### Grok (`grok agent stdio`; legacy and hidden)

The Grok driver is "intentionally not exposed as a supported provider"
(`docs/providers/README.md:12-14`), which lowers its priority.

1. **Work that outlives the turn.**
   - `spawn_subagent` `run_in_background` "Defaults to `true`"
     ([grok-build subagents guide](https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-pager/docs/user-guide/16-subagents.md)).
   - Background commands and monitors
     ([Background Tasks](https://docs.x.ai/build/features/background-tasks)).
   - A third-party report on 1.0.46/1.0.50 says that `session/prompt` returns
     `end_turn` while the child still works, and that the result arrives "in a
     turn Grok starts by itself later"
     ([benchflow #76](https://github.com/benchflow-ai/agents/issues/76)).
     This is secondary evidence.
2. **Visibility.** There is none (`provider_runtime.rs:11046`, `:11156`), and
   suspension is `kill_child` (`:11165-11169`). The runtime drops
   `_x.ai/session/prompt_complete` (`grok/runtime.rs:533-535`). Updates that
   arrive outside a prompt are emitted with no turn (`:539-562`).
3. **Capability detection.** There is no probe. Unhandled session updates are
   logged at debug level (`grok/runtime.rs:577-584`).
4. **Secrets and bind.** `XAI_API_KEY` stays in the environment, and the MCP
   config goes over stdin. Grok's "leader" mode (`[cli] use_leader`) can route
   `grok agent stdio` to a shared `~/.grok/leader.sock` process (local
   `grok agent --help`). BiBCode does not pass `--no-leader`. Under that mode,
   suspension would kill the client but not the work, and process attribution
   would miss the leader (inferred).

### `acp_mode.rs`

`apps/server/src/provider/acp_mode.rs` only maps ACP mode IDs and permission
options. It owns no process and no lifecycle, so the failure class does not apply
to it.

### Wildcard MCP URL (all providers)

`lifecycle.rs:374` builds `mcp_endpoint` from the raw bound address. The
`open_url_endpoint` on the next lines goes through `local_http_base()`, which
maps `0.0.0.0` and `::` to loopback (`lifecycle.rs:375-378`, `:841-849`, tested at
`:941-954`). The `0.0.0.0:3773` listener itself is the desktop's LAN or WSL
exposure (`apps/desktop/src-tauri/src/backend.rs:45`, `:69`; live `ss -ltn`
shows `0.0.0.0:3773`). It is not a provider bind. Fixing the URL is a one-line
change.

### At-risk cases, ranked, with test ideas

| Rank | Case                                                                                                                         | Evidence                                                                               | Test idea                                                                                                                                                                                                                                                                                                                                                         |
| ---- | ---------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1    | Provider-initiated turn killed mid-turn: Claude task-notification follow-up, Codex goal continuation, Grok self-started turn | `provider_runtime.rs:6662`, `:8211-8212`, `:9163-9182`; Claude SDK and Codex goal docs | In the `IdleDeadlineFixture` style (`provider_runtime.rs:22815-23170`): complete a turn, emit a provider `turn.started` with no delivery, advance `2 × idle_timeout`, and expect `Busy`. Then emit `turn.completed` and expect `Suspended` after one timeout. Add a Codex variant with the fake app-server sending `thread/goal/updated` and then `turn/started`. |
| 2    | Claude background Bash, Monitor or auto-backgrounded MCP task killed, even with hooks                                        | `claude/runtime.rs:1786-1810`, `activity-observation.md:371-377`, `:429`               | `provider_claude.rs` fixture: `task_started{task_type:"local_bash",is_backgrounded:true}` then `result`. Expect no suspension while `background_tasks_changed.tasks` is non-empty, and suspension one timeout after an empty set.                                                                                                                                 |
| 3    | Any provider with Chat Activity off: background subagents and terminals are killed                                           | `:8063-8066`, `:6847-6854`, `codex/runtime.rs:1697-1702`                               | Invert `idle_deadline_forgets_live_activity_when_activity_monitoring_is_disabled`: with monitoring off and a provider live-work signal present, expect `Busy`.                                                                                                                                                                                                    |
| 4    | Cursor background shell or subagent killed at 60 s                                                                           | `:10795`, `:10960-10964`; Cursor changelog                                             | Fake ACP server: `session/prompt` returns `end_turn` and then keeps sending `cursor/task` or `session/update`. With change 2, expect the session held while the provider reports live work.                                                                                                                                                                       |
| 5    | Codex child-agent background terminal killed once the child goes idle                                                        | `codex/runtime.rs:2191-2194`, `codex/activity.rs:630-634`                              | Fake app-server: the child thread starts a terminal, then goes `idle`. `thread/backgroundTerminals/list` for that child returns it. Expect `Busy`.                                                                                                                                                                                                                |
| 6    | OpenCode `retry` child (provider back-off) treated as idle                                                                   | `opencode/activity.rs:626-627`                                                         | `provider_opencode.rs`: child status `retry` after the root completes. Expect the session held.                                                                                                                                                                                                                                                                   |

## B. Official documentation compared with BiBCode

### Claude Code (installed 2.1.300; the changelog's newest entry is 2.1.300)

- **Background task events.** `task_started` carries `task_type` (`local_bash`,
  `local_agent` or `remote_agent`), `is_backgrounded`, `run_id`, `spawn_depth`
  and `parent_task_id`. The other events are `task_progress`, `task_updated`
  (status `pending`, `running`, `completed`, `failed` or `killed`) and
  `task_notification` (`completed`, `failed` or `stopped`, with an optional
  `reason: "worker_restart"`). `background_tasks_changed` is "the full live set.
  Replace any cached set with each payload…". "Nothing is emitted at startup.
  Reset to an empty set whenever the session's CLI process starts or restarts".
  It requires v2.1.203+. Source:
  [SDK reference](https://code.claude.com/docs/en/agent-sdk/typescript). All of
  these strings are present in the installed 2.1.300 binary (`grep -c` on the
  binary). **BiBCode** reads `task_started` and `task_notification` only, and
  rejects `local_bash`. It should adopt `background_tasks_changed`, keyed by
  `task_id`, falling back to `task_id` when `run_id` is absent, as the docs
  advise.
- **Session state.** With `CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS=1`, Claude
  emits `session_state_changed` with `idle`, `running` or `requires_action`. By
  default "the session keeps reporting a running status past turn end while
  background work such as a background agent … is still live. … Background shell
  commands, such as a dev server, don't hold the running status" (v2.1.269+)
  ([env vars](https://code.claude.com/docs/en/env-vars)). That is an
  authoritative busy signal, but it does not cover shells, so it should be
  combined with `background_tasks_changed`.
- **Synthetic turns.** "When a background task finishes, the SDK injects a
  synthetic follow-up turn, and the resulting `SDKResultMessage` carries
  `origin: { kind: "task-notification" }`" (SDK reference). BiBCode does not
  read `origin`.
- **Flags.** `--include-hook-events` and `--forward-subagent-text` are both in
  the local `--help`. `--forward-subagent-text` and the
  `CLAUDE_CODE_FORWARD_SUBAGENT_TEXT` environment variable need v2.1.211+, and
  forwarding at every nesting depth needs v2.1.219+
  ([headless](https://code.claude.com/docs/en/headless), "Follow subagent
  messages"). The environment variable "is ignored" outside stream-json, so it
  needs no probe ([env vars](https://code.claude.com/docs/en/env-vars)).
  `system/init` carries a `capabilities` array intended for feature detection
  (headless, "Read session metadata").
- **Print mode at exit.** "After Claude finishes its turn and stdin has closed,
  a `claude -p` run can stay open to wait for background work". The default
  ceiling is 10 minutes (`CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS`), and background
  commands hold the run open (headless, "Background tasks at exit"). On SIGTERM
  Claude "terminates the process tree of any Bash command that is still
  running" (headless, "Stop a run with SIGTERM"). BiBCode keeps stdin open and
  kills on suspension, so these waits never help it.
- **`--mcp-config`.** The flag takes "JSON files or strings" (local `--help`).
  `${VAR}` expansion is documented for `url` and `headers` in `.mcp.json`. In
  remote `url` and `headers`, "credential variables" such as
  `ANTHROPIC_API_KEY`, `HTTPS_PROXY` or `NPM_TOKEN` read as empty, while a name
  outside that set "expands as written". `headersHelper` is not stripped for
  `--mcp-config` servers ([MCP](https://code.claude.com/docs/en/mcp)). The docs
  do **not** say that `${VAR}` expansion applies to `--mcp-config`. Prefer a
  0600 file path, or prove expansion with a test against an HTTP stub that
  echoes the `Authorization` header. `system/init.mcp_server_errors` reports
  skipped entries (headless).
- **Agent SDK.** It is a TypeScript or Python library that drives the CLI
  process (the SDK reference describes closing the process's stdin and a
  custom spawn function). Using it would add a production Node or Python
  runtime, which `AGENTS.md` forbids. Stay on the CLI and adopt the SDK's
  message types instead.
- **Deprecations and drift.**
  - "`--bare` is the recommended mode for scripted and SDK calls, and will
    become the default for `-p` in a future release". Bare mode runs no hooks,
    no auto-discovered MCP and "Background tasks: none run" (headless). BiBCode
    depends on all three, so it will need an explicit opt-out when one ships
    (inferred).
  - `--permission-mode` now also accepts `manual` (local `--help`). Nothing
    BiBCode passes is deprecated.
  - Relevant fixes in the changelog: 2.1.292, "one-shot `claude -p` … stopping a
    background command 5 seconds after the final result"; 2.1.299, "scheduled
    wakeups (ScheduleWakeup) not firing while a background task was running".

### Codex (installed 0.162.1; protocol taken from the local generated schema)

- **`thread/rollback` has been removed** ("`thread/rollback` has been removed
  from the API, including its request and response types",
  [app-server README](https://raw.githubusercontent.com/openai/codex/main/codex-rs/app-server/README.md)).
  It is also absent from the local `ClientRequest` schema. BiBCode still calls
  it (`codex/runtime.rs:1420-1438`). The replacement is `thread/revert
{threadId, beforeTurnId}`, which "only changes persisted conversation
  history. It does not revert local file changes" (local
  `v2/ThreadRevertParams.json`).
- **Personality.** "`friendly` and `pragmatic` no longer select a style" (README).
  BiBCode does not send `personality`, so nothing changes.
- **Goals.** `ThreadGoalSetParams` now has `origin` (`user` or `automatic`):
  "Missing provenance does not supply user authorization". `ThreadGoalStatus`
  adds `blocked`, `usageLimited` and `budgetLimited` (local schema). BiBCode
  sends neither `origin` nor `tokenBudget` (`codex/runtime.rs:1340-1346`); it
  should send `origin: "user"` (inferred impact). BiBCode does not consume
  `thread/goal/updated` or `thread/goal/cleared`.
- **Notifications BiBCode ignores.** `deprecationNotice`, `configWarning` and
  root `thread/status/changed` (it is used only for actor status). The
  `process/exited` and `process/outputDelta` notifications belong to the
  client's own `process/spawn`. They are not agent terminals (local
  `v2/ProcessExitedNotification.json`) and are not useful as a keepalive.
- **Experimental surface.** Background terminals, `thread/settings/update` and
  `thread/queue/*` exist only under `experimentalApi`. The README warns that "The
  general `experimentalApi` opt-in does not identify a compatible GUI version",
  so BiBCode's `-32601` downgrade path is the right defence.

### OpenCode (desktop `PATH` → 2.0.29; `~/.opencode/bin/opencode` is 1.18.32)

- "Integrations that call the V1 server API must migrate to the V2 API"
  ([Migrate from V1](https://opencode.ai/v2/docs/migrate-v1/)). The v2.0.29
  generated client lists only `/api/*` routes. Examples are
  `/api/session/{id}/prompt`, `/api/session/{id}/interrupt`, `/api/event` and
  `/api/session/{id}/permission/{requestID}/reply`. There is no
  `/session/{id}/children`, `/prompt_async`, `/abort`, `/question/…` or
  unprefixed `/event`
  ([client.ts](https://github.com/anomalyco/opencode/blob/v2.0.29/packages/client/src/promise/generated/client.ts)).
- BiBCode calls the V1 paths (`opencode/runtime.rs:636-1158`, `:2150`). The
  desktop's `PATH` puts `~/.local/share/mise/installs/opencode/latest` (2.0.29)
  ahead of `~/.opencode/bin` (`/proc/<desktop pid>/environ`, PATH only).
- Third-party reports say that unprefixed V1 paths fall through to the web UI
  with HTML and status 200 (search results summarised in the sources). This
  matches BiBCode's any-response readiness check. It is secondary evidence; the
  user's `server.log` has no OpenCode chat activity since 2.0.29 was installed
  on 2026-10-14.
- Permission names changed: "`bash` is now `shell`, `task` is now `subagent`"
  (migration guide). If BiBCode's OpenCode permission handling keys on action
  names, it must map the new ones (not checked).

### Cursor Agent (installed 2026.10.14-941492e)

- Background shells and subagents are covered in section A. In v2026.09.15,
  "loading a saved session shows its past subagents again" for clients that
  advertise the capability ([changelog](https://cursor.com/docs/cli/changelog)).
- `cursor/task` fields are `toolCallId`, `description`, `prompt`,
  `subagentType`, `model`, `agentId` and `durationMs`
  ([ACP](https://cursor.com/docs/cli/acp)). BiBCode ignores it.
- Authentication methods: `cursor_login`, `--api-key` and `--auth-token` (ACP
  page). BiBCode passes no key on argv; it passes only `-e <endpoint>`
  (`provider_runtime.rs:10725-10729`).

### Grok (installed 1.0.44)

- The relevant sources are those in section A. The official headless page shows
  `session/prompt` returning only completion metadata, and documents no
  background semantics
  ([headless](https://docs.x.ai/build/cli/headless-scripting)).
- The x.ai changelog returned HTTP 403 to the fetch, so no Grok changelog claims
  are made here.

## Not verified

- Live stream-json output of `background_tasks_changed` and
  `session_state_changed` from 2.1.300. That would need a model call; the
  evidence is the docs plus the binary strings only.
- Whether `${VAR}` expands in inline `--mcp-config` headers.
- Cursor prompt-wait semantics without the `subagents` capability (inferred
  from the changelog wording).
- Whether Cursor ACP honours `session/new` `mcpServers`.
- OpenCode 2.x runtime failure. This is source-diff evidence only: starting
  `opencode serve` would load the user's configuration and MCP servers, so it
  was not run.
- Grok `end_turn` behaviour on 1.0.44. The only report covers 1.0.46 and 1.0.50.

## Sources

Primary:

- [Claude Agent SDK TypeScript reference](https://code.claude.com/docs/en/agent-sdk/typescript)
- [Claude Code headless](https://code.claude.com/docs/en/headless)
- [Claude Code env vars](https://code.claude.com/docs/en/env-vars)
- [Claude Code MCP](https://code.claude.com/docs/en/mcp)
- [Claude Code CHANGELOG](https://raw.githubusercontent.com/anthropics/claude-code/main/CHANGELOG.md)
- [Codex app-server README](https://raw.githubusercontent.com/openai/codex/main/codex-rs/app-server/README.md)
- [Codex: Follow a goal](https://learn.chatgpt.com/use-cases/follow-goals)
- Local `codex app-server generate-json-schema --out <dir>` with and without
  `--experimental` (0.162.1)
- [OpenCode: Migrate from V1](https://opencode.ai/v2/docs/migrate-v1/)
- [OpenCode v2.0.29 client.ts](https://github.com/anomalyco/opencode/blob/v2.0.29/packages/client/src/promise/generated/client.ts)
- [OpenCode v2.0.29 env.ts](https://github.com/anomalyco/opencode/blob/v2.0.29/packages/cli/src/env.ts)
- [Cursor CLI changelog](https://cursor.com/docs/cli/changelog)
- [Cursor ACP](https://cursor.com/docs/cli/acp)
- [Grok Background Tasks](https://docs.x.ai/build/features/background-tasks)
- [Grok headless](https://docs.x.ai/build/cli/headless-scripting)
- [grok-build subagents guide](https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-pager/docs/user-guide/16-subagents.md)
- Local `--help` and `--version` of every CLI, plus a string scan of the
  installed Claude and Cursor bundles

Secondary (reports only):

- [benchflow-ai/agents#76](https://github.com/benchflow-ai/agents/issues/76)
- [anthropics/claude-code#94392](https://github.com/anthropics/claude-code/issues/94392)
- OpenCode 2.x incompatibility reports, such as
  [happier-dev/happier#414](https://github.com/happier-dev/happier/issues/414)
