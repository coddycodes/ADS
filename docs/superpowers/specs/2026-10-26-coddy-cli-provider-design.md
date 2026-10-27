# Coddy CLI as the single provider driver — design

**Status:** proposed
**Date:** 2026-10-26
**Owners:** Coddy Studio server
**Related:** [Provider architecture](../architecture/providers.md), [RPC and orchestration](../architecture/rpc-and-orchestration.md)

## 1. Problem

Coddy Studio drives five coding-agent protocols, one per provider driver:

| Driver | Native integration | Where it lives |
| --- | --- | --- |
| `codex` | Codex App Server JSON-RPC | `apps/server/src/provider/codex` |
| `claudeAgent` | Claude stream-JSON CLI + authenticated hooks | `apps/server/src/provider/claude` |
| `cursor` | Agent Client Protocol | `apps/server/src/provider/cursor` |
| `grok` | Agent Client Protocol | `apps/server/src/provider/grok` |
| `opencode` | OpenCode 1/2 HTTP server + event streams | `apps/server/src/provider/opencode` |

Each driver re-implements the same product behavior against a different wire: streaming
assistant text, tool lifecycle, approvals, user questions, turn completion, resume,
token usage, MCP status, capability discovery and version gating. Five copies of that
logic means five definitions of "a turn finished", five streaming quirk lists and five
install/auth stories for users.

The Coddy agent CLI already owns that behavior for its own runtime. It exposes a
multi-client HTTP + SSE server (`coddy cli server`) with workspaces, durable sessions,
streaming messages, permissions, questions, MCP and skills. Coddy Studio should speak
that one protocol instead of five.

## 2. Decision

1. Add a single built-in provider driver kind, `coddy`.
2. `coddy` drives the Coddy CLI server over loopback HTTP + SSE.
3. Make `coddy` the default provider and the only supported driver.
4. Retire `codex`, `claudeAgent`, `cursor`, `grok` and `opencode` from Settings, chat,
   inventory and the provider terminal **after** `coddy` reaches parity.
5. Close the small machine-surface gaps in the CLI rather than teaching Studio to read
   CLI internals: a models catalog endpoint, a bound-port handshake, and SSE keepalive.

Legacy threads stay decodable (driver kinds are open slugs) but become read-only
history; the composer offers a Coddy continuation with the existing context-handoff
block instead of silently re-binding a Codex conversation to a different runtime.

## 3. How Coddy Studio providers work today

The provider layer is already a clean seam, which is why this replacement is feasible:

- **Launch request.** `ProviderLaunchRequest`
  (`apps/server/src/production/provider_runtime.rs:142`) carries the thread id, provider
  kind, instance id, label, binary path, cwd, runtime/interaction mode, model, options,
  resume cursor, environment, endpoint password and MCP bridge config.
- **Driver contract.** `ProviderDriver`
  (`provider_runtime.rs:348`) is `start`, `send`/`deliver`, `steer`, `interrupt`,
  `approve`, `answer`, `set_mode`, `set_model`, `set_options`, `rollback`, `next_event`,
  `shutdown`, plus capability hooks (`holds_unsettled_work`, `owns_native_work`).
- **Factory.** `ProviderDriverFactory` (`:474`) is implemented by
  `NativeProviderDriverFactory::create` (`:10369`), which matches the driver kind string
  and spawns the concrete runtime.
- **Canonical stream.** `ProviderEvent` (`:236`) is a thin envelope
  (`event_type`, `thread_id`, `turn_id`, ids, payload, activity mutations). The typed
  vocabulary is `ProviderRuntimeEventV2`
  (`packages/contracts/src/providerRuntime.ts:1006`): `session.*`, `thread.*`,
  `turn.started|completed|aborted`, `item.*`, `content.delta`, `request.*`,
  `user-input.*`, `task.*`, `mcp.status.updated`, `runtime.error`, and so on.
- **Supervisor.** `ProviderRuntimeSupervisor` owns delivery ordering, accepted/ambiguous
  outcomes, idle suspension, stop deadlines, restart recovery and checkpoint reverts.
  Drivers only translate native I/O.
- **Inventory.** `production/provider_inventory.rs` probes `--version`, per-driver
  auth/model/capability probes (`:719`) and publishes models, slash commands, skills and
  agents for the composer.
- **Settings.** Per-driver legacy settings
  (`server_settings/mod.rs:14` `built_in_provider_defaults`) plus the generic
  `provider_instances` map (`:103`), whose driver-specific payload is opaque to contracts.

Nothing in the supervisor, orchestration engine, persistence or React state requires
five drivers. They require **one driver that emits the canonical stream correctly**.

Closed sets that still name the five drivers and must open for `coddy`:

| Seam | Where |
| --- | --- |
| Accepted provider kinds | `canonical_provider_kind` (`provider_runtime.rs:5289`) |
| Driver → settings/binary resolution | `provider_binary_settings` (`:5302`) |
| Driver factory | `NativeProviderDriverFactory::create` (`:10369`) |
| Activity support | `provider_supports_agent_activity` (`:6905`) |
| Steer / rewind capability | `provider/mod.rs:13`, `:36` |
| Exact delivery reconciliation | `turn_delivery.rs:896` (currently `codex`/`opencode` only) |
| Inventory + capability discovery | `provider_inventory.rs:263`, `provider_inventory/capabilities.rs:4` |
| Provider terminal inventory | `provider_terminal/supervisor.rs:130` |
| Usage RPC | `provider_usage/mod.rs:54` (Claude/Codex only) |
| CLI session import | `agent_sessions/mod.rs:45` (Claude/Codex only) |
| Update capability | `provider_maintenance/source.rs:586` |
| Hermetic test allowlist | `apps/server/tests/support/hermetic_providers.rs:28` |

The Rust layer has no typed `ProviderEvent` enum: drivers emit
`ProviderEvent { event_type: String, payload: Value, … }` and the supervisor projects the
strings (`project_provider_event`, `provider_runtime.rs:8926`). The typed vocabulary lives
only in `packages/contracts/src/providerRuntime.ts`. A new driver is correct when its
strings and payloads satisfy that contract and the projection, not when it matches a Rust
enum.

## 4. The Coddy CLI contract Studio will consume

`coddy cli server --host tcp://127.0.0.1:<port>` serves a versioned v1 API
(`agent/src/modules/cli/internal/runtime/server/endpoints.go`). The parts that map onto
Studio:

| Need | Endpoint / stream |
| --- | --- |
| Liveness and compatibility | `GET /v1/health`, `GET /v1/version` |
| Workspace per cwd | `GET/POST /v1/workspaces`, `DELETE /v1/workspaces/{id}` |
| Durable conversation | `POST /v1/workspaces/{id}/sessions`, `GET …/sessions/{sid}`, `PUT`, `DELETE` |
| Transcript | `GET …/sessions/{sid}/messages`, `…/history` |
| Send a turn | `POST …/agent` (`proto.AgentMessage`, `202`, detached from the request) |
| Stream | `GET …/events?client_id=` (SSE, `data: {type,payload}`) |
| Cancel | `POST …/agent/sessions/{sid}/cancel` |
| Prompt queue | `GET …/prompts/queued`, `…/prompts/list`, `POST …/prompts/clear` |
| Permission decision | `POST …/permissions/grant` (`allow`, `allow_session`, `deny`) |
| Question answer | `POST …/questions/answer`, `…/questions/cancel` |
| Model switch | `POST …/config/model`, `POST …/config/set` |
| Skills | `GET …/skills`, `POST …/skills/read` |
| MCP | `…/mcp/states`, `…/mcp/refresh-*`, `…/mcp/auth` |
| Usage | `proto.Session.prompt_tokens` / `completion_tokens` / `cost` |

Stream payloads (`internal/runtime/server/events.go`):

- `message` — full `proto.Message` snapshots with parts `TextContent`,
  `ReasoningContent`, `ToolCall`, `ToolResult`, `Finish`, `ShellCommand`;
  message updates are debounced and published as cumulative snapshots.
- `run_complete` — authoritative end of a turn, echoes the caller's `RunID`, carries
  final `Text`, `Error`, `Cancelled` (`proto/proto.go:78`).
- `permission_request` / `permission_notification`.
- `question_batch_request` / `question_batch_notification`.
- `agent_event` — `error`, `summarize`, mode suggestions, AWS SSO progress.
- `session` — id, title, parent, todos, `prompt_tokens`, `completion_tokens`, `cost`.
- `mcp_event`, `skills_event`, `lsp_event`, `config_changed`, `file`.

Two properties matter for the mapping:

- `proto.Session.prompt_tokens` is the **current context size** of the last step (the
  agent overwrites it from each step's usage, `internal/agent/agent.go:2094`), while
  `completion_tokens` is that step's output.
- `RunID` is the only safe correlation key when a prompt queues behind a busy session,
  because another turn's `RunComplete` for the same session may arrive first.

## 5. Design

### 5.1 Process topology

One supervised Coddy server process per **provider session**, the same model Codex and
Claude use: each thread owns its agent process, so a crash or version mismatch stays
contained to one conversation.

```
Coddy Studio server
└── CoddyServerRegistry (keyed by instance id + environment fingerprint)
    └── CoddyServerHost (owns one child process)
        ├── HTTP client          (typed requests)
        ├── SSE reader           (one connection, fan-out by workspace)
        └── workspace per cwd    (created on demand, refcounted)
```

A shared per-instance daemon is the intended follow-up once workspace refcounting is
proven; the transport does not change. Why not embed the CLI: it is Go, it owns its
SQLite store and process lifecycle, and process isolation is the boundary Studio already
relies on for every driver.

Configuration (`provider_instances.<id>.config`):

```jsonc
{
  "binaryPath": "coddy",       // default; resolved per environment
  "serverUrl": "",              // optional externally managed server
  "dataDir": ""                // optional isolated ~/.coddy/cli data root
}
```

An explicit `serverUrl` skips spawning; Studio probes `/v1/version` and never kills it
(`owns_native_work = false`). `dataDir` reaches the spawned server as its `-D`
root through the vetted instance environment. Attachments are accepted and
dropped rather than forwarded; that mapping remains a follow-up.

### 5.2 Startup handshake

1. Resolve the executable with the instance environment (same policy as other drivers).
2. Ask for an ephemeral port with `--host tcp://127.0.0.1:0 --port-file <tmp>`; the CLI
   writes the actual bound address after `net.Listen` succeeds.
3. Poll `GET /v1/health` and `GET /v1/version` with a bounded deadline.
4. Reject a major version Studio does not drive with
   `ProviderRuntimeError::IncompatibleProvider` (fails the delivery once, actionable).
5. `POST /v1/workspaces` for the launch cwd, or reuse the existing workspace for that
   path. Hold a refcount; delete when the last session detaches.
6. Create or resume the session (`POST /v1/workspaces/{id}/sessions`, or `GET` the
   cursor's session id; a 404 means the conversation is gone → new session plus context
   handoff).
7. Open `GET /v1/workspaces/{id}/events?client_id=<uuid>`.

The registry kills only processes it spawned, reaps them through the shared
`process::supervised` owner, registers attribution, and shuts down before the profiler
teardown like every other provider root.

### 5.3 Turn identity and delivery

- Studio allocates the canonical turn id and sends it as `AgentMessage.RunID`.
- `POST /v1/workspaces/{id}/agent` returns `202` when the prompt is admitted, including
  when it is queued behind a busy session.
- The driver resolves the delivery as `Accepted { turn_id }` on `202`; `RunComplete`
  with the matching `RunID` settles the turn. A `RunComplete` for another `RunID` is
  ignored for delivery purposes but still projects activity.
- `interrupt` calls the cancel endpoint and leaves settlement to the resulting
  `RunComplete` (`Cancelled: true`), which the supervisor's 10-second stop deadline
  bounds from above.
- `steer` is unsupported at the driver seam: the CLI queues a second prompt as its own
  turn rather than injecting into the running one. Studio keeps its existing durable
  queue and exposes `supportsTurnSteer = false`.
- `rollback` is unsupported (`supportsConversationRewind = false`): checkpoint revert
  restores files only and says so, exactly as Cursor does today.

### 5.4 Event mapping

| CLI event | Canonical events |
| --- | --- |
| `session` created/updated, `agent_event` summarize | `session.state.changed` (`running`/`ready`), `thread.metadata.updated` (title, todos) |
| `message` assistant snapshot, first sight | `item.started { itemType: assistant_message }` + `content.delta { streamKind: assistant_text }` with the whole text |
| `message` assistant snapshot, later | `content.delta` for the appended suffix (see 5.5) |
| `ReasoningContent` | `item.started/updated { reasoning }`, `content.delta { reasoning_text }` |
| `ToolCall` with `finished: false` | `item.started { command_execution / file_change / mcp_tool_call / dynamic_tool_call / web_search / image_view }` |
| `ToolCall` with `finished: true` or a matching `ToolResult` | `item.completed` with input/result data, `status` from `is_error` |
| `Finish { reason }` | `turn.completed { state }` — `end_turn`/`tool_use` → completed, `canceled` → cancelled, `max_tokens`/`content_filter`/`error` → failed with message |
| `run_complete` | authoritative `turn.completed` (idempotent with `Finish`), usage from the latest `session` |
| `permission_request` | `request.opened { requestType: command_execution_approval / file_read_approval / file_change_approval / dynamic_tool_call }` + args |
| `permission_notification` | `request.resolved { decision }` |
| `question_batch_request` | `user-input.requested { questions }` (choice/confirm/fill-in mapped to options) |
| question answered/cancelled | `user-input.resolved { answers }` |
| `mcp_event` state changes | `mcp.status.updated` (aggregate snapshot of every server) |
| `session` with tokens | `thread.token-usage.updated { usedTokens: prompt_tokens, outputTokens, inputTokens, maxTokens }` |
| `agent_event { type: error }` | `runtime.error` and a failed `turn.completed` if the turn is still open |
| SSE EOF / process exit | `session.exited { exitKind }`; the supervisor settles an abandoned turn |
| child `session` (parent_session_id set) | `task.started` / `task.progress` / `task.completed` for subagent activity |

### 5.5 Streaming fidelity: snapshot diffing

The CLI publishes cumulative message snapshots, not deltas. The driver keeps one
`MessageStream` per message id with the last emitted text and reasoning:

- appended text → emit exactly the suffix;
- an unchanged snapshot → emit nothing (the CLI debounces to the same content);
- a rewritten snapshot (provider retry, compaction, edit) → emit the new text as a
  replacement by closing the current assistant item and starting a new one, because
  `content.delta` cannot retract text;
- `run_complete.text` is the reconciliation source: if the accumulated assistant text
  differs, close and restart the item with the final text before completing the turn.

This is the one place where a CLI-side delta event would be cheaper and more precise.
The alternative — adding `content_delta` events to the CLI message service — is recorded
in §8 as a follow-up; snapshot diffing avoids a Go change in the critical path and has a
single, testable invariant.

### 5.6 Reconnect

The CLI SSE has no event ids and no replay (`internal/runtime/server/proto.go:177`).
On a dropped stream the driver:

1. reconnects with backoff while the process is alive;
2. re-reads `GET …/sessions/{sid}/messages` and reconciles message snapshots (any text
   already emitted is kept; missing text is emitted; rewrites restart the item);
3. resumes live events;
4. if the process died, emits `session.exited` and lets the supervisor retire the
   session; the next turn relaunches and resumes the same CLI session.

SSE keepalive comments (every 15 s) and `retry:` are a CLI-side addition so idle streams
are not dropped by intermediaries and the client backoff is server-informed.

### 5.7 Inventory and capabilities

`provider_inventory.rs` gains a `"coddy"` arm:

- `--version` for install state (the CLI prints `coddy version …`);
- `GET /v1/workspaces/{id}/models` for the catalog (new endpoint) — model id, display
  name, context window, option descriptors;
- `GET …/skills` for skills (`invocation: "slash"`), `GET …/providers` for the configured
  upstream provider and readiness;
- `slashCommands`: the CLI's built-ins that Studio can send as text (`/compact`,
  `/summarize`); a full `/commands` catalog is deferred;
- `agents`: empty in v1 (the CLI has named agents but no list endpoint yet).

Capabilities: `supportsContextWindowUsage = true` once models carry a context window,
`supportsMcpStatus = true`, `supportsTurnSteer = false`,
`supportsConversationRewind = false`, structured activity = true (chat + subagent
sessions), provider terminal observation = false (the TUI is not attachable).

### 5.8 Provider terminal

Studio's provider terminal opens `coddy cli --cwd <dir>` (the TUI) and is **unobserved**,
like Cursor's terminal today. The server cannot attach a PTY to a CLI server session, and
pretending otherwise would double-spend tokens. `provider_terminal` returns no observer
for `coddy`; the terminal still launches, and the chat composer points users at the Chat
instead of promising activity mirrored from the terminal.

### 5.9 Security

- The managed server binds `127.0.0.1` on an ephemeral port only; it is never advertised
  through the relay or remote environments.
- `serverUrl` is an operator-supplied endpoint; Studio sends no credentials it does not
  already own, and refuses non-loopback URLs unless the user opts in explicitly.
- Provider environment values may carry `CODDY_API_KEY`; they stay in the instance
  environment and never enter logs, profiler records or transcripts (existing redaction).
- The CLI API has no authentication; the loopback-only bind and ephemeral port are the
  boundary. Documented in `docs/providers/coddy.md`.

## 6. Migration

Driver kinds are open slugs and persisted threads keep their binding, so nothing crashes
when a build ships only `coddy`. The user-visible path:

1. `coddy` appears as a provider instance, enabled by default, alongside the legacy rows
   while they still exist.
2. New threads default to `coddy`.
3. A thread bound to a retired driver opens read-only: the transcript still renders, the
   composer is disabled, and the banner offers **Continue with Coddy**. That action
   creates a new `coddy` thread and sends the existing context-handoff block, then links
   the old thread as history.
4. Once all five drivers are removed, Settings shows one provider family; the legacy
   `providers.codex` … `providers.opencode` settings keys are migrated into disabled
   instances (or dropped) by a one-shot settings migration, never silently deleted.

## 7. Alternatives considered

| Alternative | Why rejected |
| --- | --- |
| Keep five drivers, add `coddy` as a sixth | Does not meet the requirement; keeps five streaming/approval implementations and their install stories. |
| stdio JSONL protocol from Studio to `coddy run` | Loses multi-client event fan-out, durable sessions, queue, permission round-trips; would need a new CLI mode anyway. |
| Link the Go agent as a library | Cross-language embedding, no process isolation, SQLite/version coupling; contradicts the existing supervised-process boundary. |
| Implement ACP in the CLI and reuse Studio's ACP driver | Larger protocol surface in Go (session config, terminal methods), and Studio would still need a Coddy-specific inventory/approval mapping. HTTP already exists and is tested. |
| Read `~/.coddy/cli/cache/models.json` for the catalog | Crosses the process boundary, breaks remote environments, duplicates cache invalidation. A versioned endpoint is smaller. |

## 8. Follow-ups (explicitly out of scope for v1)

- CLI-native `content_delta` events to replace snapshot diffing.
- CLI SSE event ids + `Last-Event-ID` replay; Studio currently reconciles by re-reading
  messages.
- `GET …/commands` and `GET …/agents` catalogs for slash commands and named agents.
- Terminal observation through a CLI attach API, if one is added.
- Moving the MCP bridge from Studio's `ProviderMcpConfig` to the CLI's own MCP client
  (the CLI already has MCP; v1 keeps Studio's bridge out of the Coddy path).

## 9. Acceptance

- `coddy` is the only driver in `NativeProviderDriverFactory::create`, the only kind in
  `PROVIDER_DISPLAY_NAMES`, inventory, Settings and docs.
- A thread started with `coddy` streams assistant text, reasoning, tool calls, approvals,
  questions, MCP status, usage and completion through the canonical events, with a live
  scenario proving reconnect and Stop.
- No production code path references `codex`, `claudeAgent`, `cursor`, `grok` or
  `opencode`.
- `vp check`, `vp run typecheck`, focused Rust tests, `cargo fmt --all --check` and Clippy
  for affected targets pass; the provider runbooks are reviewed and updated.
