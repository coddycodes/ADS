# OpenCode V2 client — design

Status: approved by the user on 2026-10-15 as option A ("a native Rust V2
HTTP+SSE client in the OpenCode adapter, selected by `probe_server_api`, with V1
kept for 1.x"). This record lists that decision, the rejected alternatives, and
the design and staging that follow from it. The evidence is from OpenCode
2.0.29 and 1.18.32 servers started locally with a scratch `HOME` and XDG
directories, from the 2.0.29 `/openapi.json`, and from the
`anomalyco/opencode@v2.0.29` source.

## Problem

OpenCode 2 replaced the V1 server API that BiBCode's OpenCode adapter speaks
with `/api/*` routes. Every unprefixed V1 `GET` now returns the web UI as HTTP
200, and unprefixed `POST`s return 405. Commit `b7a3a8a8` made readiness strict
and refused OpenCode 2 with an actionable error. OpenCode 1.x is still published
(`opencode-ai` 1.19.0 on 2026-10-11), and OpenCode 2 ships as `@opencode/cli`
(2.0.29). BiBCode has to speak both.

## Decision and rejected alternatives

| Option | Outcome |
| --- | --- |
| **A. Native Rust V2 HTTP+SSE client** inside `provider::opencode`, chosen per server by `probe_server_api` | **Approved.** Keeps every OpenCode feature BiBCode has (structured activity, rollback, exact delivery reconciliation, inventory) and the existing process supervision and loopback model. |
| B. Drive OpenCode through `opencode acp` with the existing ACP driver | Rejected. It is cheaper and protocol-stable, but ACP has no revert and no message lookup, so rollback and exact delivery recovery would be lost for OpenCode; activity would depend on OpenCode's private child-session extension. |
| C. Use the official `@opencode/client` SDK | Rejected. It needs a production Node runtime, which `AGENTS.md` forbids. Generating a Rust client from `/openapi.json` was also rejected: the spec types events as an opaque JSON string, so the event layer would still be hand-written, and generated code would churn with every release. |
| D. Keep refusing OpenCode 2 | Superseded. It stays as the behaviour for versions BiBCode cannot drive (major 3 or later) and for parts not yet built during staging. |

## Evidence summary

- `GET /api/info` answers `{"version":"2.0.29","pid":…,"urls":[…],…}` behind
  Basic auth. `GET /global/health` answers `{"healthy":true,"version":"1.18.32"}`
  on 1.x and HTML on 2.x. Both servers honour `OPENCODE_SERVER_PASSWORD` and the
  `opencode` Basic user.
- The 2.0.29 OpenAPI document calls itself "Experimental HttpApi surface for
  selected instance routes". 2.0.26 to 2.0.29 shipped in one week. MCP
  registration exists only as `PUT /api/experimental/mcp/{server}`.
- A real 2.0.29 server was driven through complete turns against a local,
  scripted OpenAI-compatible endpoint (`@opencode/ai/providers/openai-compatible`
  pointed at `127.0.0.1`), so no provider credentials or credits were involved.
  Captured scenarios: text reply, shell permission ask, question form,
  foreground subagent, background subagent with a provider-started follow-up
  turn, provider failure, user interrupt, revert. Catalog, MCP and error
  responses were captured separately.
- Message IDs must match `^msg_` (2.x) or `^msg` (1.x). A UUID is rejected
  with HTTP 400 on both (observed).

## Design

### Module layout

```
provider/opencode/
  mod.rs        probe_server_api, OpenCodeServerApi, messages, fixtures
  runtime.rs    OpenCodeSessionRuntime: shared shell + V1 wire (unchanged paths)
  v2.rs         V2 wire: request paths/bodies, ruleset, forms, errors,
                event decoding and the root-turn translator
  v2_activity.rs  V2 → V1-shaped event/DTO normaliser for the activity tracker
  activity.rs   OpenCodeActivityTracker (protocol-independent output, V1 input)
  sse.rs        SSE decoder (shared)
```

`OpenCodeSessionRuntime` stays the single runtime type. It gains an
`api: OpenCodeApi` (`V1` or `V2`) fixed at construction. The event channel,
turn bookkeeping, lifecycle (`start`/`resume`/`stop`), activity flush and
reconciliation plumbing are shared. Each public operation branches once on
`api` at the wire: V1 keeps today's code verbatim; V2 calls into `v2.rs`. The
driver in `provider_runtime.rs` is unchanged apart from passing the detected API
to the runtime.

### Version selection and fallback

`probe_server_api` returns:

| Answer | Meaning | Use |
| --- | --- | --- |
| `V1` | `/global/health` JSON `healthy: true` | V1 wire |
| `V2 { version }` | `/api/info` JSON, major version 2 | V2 wire |
| `Unsupported { version }` | `/api/info` JSON, any other major | refuse with the actionable message |
| `Unavailable` | neither answered | managed: keep waiting until the deadline; configured Server URL: V1, as before |

Inventory reads the CLI `--version`: `1.x` → V1 inventory, `2.x` → V2
inventory, other majors → the unsupported message.

### Session, prompt, command and model

| Operation | V1 | V2 |
| --- | --- | --- |
| create | `POST /session {title, permission}` | `POST /api/session {title, location:{directory}, permissions, agent?, model?}` → `data.id` |
| resume | `GET /session/{id}` (404 → new) | `GET /api/session/{id}` (404 → new) |
| prompt | `POST /session/{id}/prompt_async {parts, messageID, model, variant, agent}` | `POST /api/session/{id}/model` when the model or variant changed, then `POST /api/session/{id}/prompt {id, text, files, delivery:"steer"}` |
| command | `POST /session/{id}/command {command, arguments, …}` | `POST /api/session/{id}/command {name, text, delivery:"steer"}` |
| interrupt | `POST /session/{id}/abort` | `POST /api/session/{id}/interrupt` |
| exact lookup | `GET /session/{id}/message/{mid}` | `GET /api/session/{id}/message/{mid}` (`data.type == "user"`) |
| variants | `GET /provider` `all[].models[m].variants` keys | `GET /api/model` entry `variants[].id` |
| MCP | `POST /mcp {name, config}` | `PUT /api/experimental/mcp/{name} {config}` |

The V2 message id for a delivery is `msg_` followed by the delivery key with
characters outside `[A-Za-z0-9]` removed, so the same key always maps to the
same id and reconciliation can look it up. V2 orders messages by creation time,
so an id that does not sort by time is safe there. File attachments become
`files: [{uri, name}]` with the same `file://` URIs V1 uses. The model
reference is `{providerID, id, variant?}`.

### Ruleset renames

V2 rules are `{action, resource, effect}` and the last match wins. BiBCode's
modes map to:

- full access: `[{*, *, allow}]`;
- otherwise: `{*, *, ask}`, then `ask` for `shell`, `edit`, `webfetch`,
  `websearch`, `external_directory`, then `allow` for `question`.

`bash` is `shell`, `task` is `subagent`, and `write`/`patch` are `edit` in V2.
`codesearch` and `doom_loop` are not V2 actions and are dropped.

### Event envelope and vocabulary → BiBCode turns

V2 frames are `data: {"id","type","created"?,"location"?,"data":{…},"durable"?}`.
The stream is global, so every event is filtered by `data.sessionID` (forms by
`data.form.sessionID`). The root-turn translator is a small state machine in
`v2.rs`:

| V2 event (root session) | BiBCode event |
| --- | --- |
| `session.execution.started` without a BiBCode turn | `turn.started` for a provider-started turn (background results) |
| `session.text.delta` | `content.delta` (`assistant_text`, item = `assistantMessageID`) |
| `session.text.ended` | completes the item's text if deltas were missed |
| `session.step.ended` with `finish:"stop"` | `message.assistant.completed` |
| `session.step.failed` | remembered as the turn's error |
| `session.execution.succeeded` | `turn.completed {state:"completed"}` |
| `session.execution.interrupted` | `turn.completed {state:"cancelled"}` |
| `session.execution.failed` | `turn.completed {state:"failed", errorMessage, errorClass}` |
| `permission.asked` (root or a known descendant) | `request.opened` (`exec_command_approval`, detail `action: resources`) |
| `form.created` (root or a known descendant) | `user-input.requested` |
| `permission.replied`, `form.replied`, `form.cancelled` | clear the pending request |
| `session.created` with a known `parentID` | records the child for routing |

V2 error objects are `{type, message, status?}`. `aborted` maps to `cancelled`,
`provider.auth` to `permission_error`, `provider.content-filter` to `refusal`,
step finish `length` to `max_tokens`, anything else to `provider_error` with the
message and HTTP status. A server-sent event the translator does not know is
ignored with a `debug` trace, as V1 does.

Permission replies go to the session that asked:
`POST /api/session/{sessionID}/permission/{id}/reply {decision}` with
`accept → once`, `acceptForSession → always`, otherwise `reject`. Surfacing
descendant asks matters on V2: a foreground subagent blocks the parent until
its own asks are answered.

### Forms

The question tool becomes a form whose fields are `q0…qN`
(`title` = header, `description` = question, `options[].label`). A form is shown
as `user-input.requested` with one question per field (`id` = field key,
`header` = title or key, `question` = description or the form title,
`options` = `{label, description}`). Answers are posted to
`POST /api/session/{sessionID}/form/{id}/reply {answer}`, with each value
coerced to the field type: string fields take the first answer, multiselect
fields the list, booleans `true/yes`, numbers a parsed number. A form with an
`external` field (OAuth-style) cannot be answered in BiBCode and is cancelled
with `DELETE /api/session/{sessionID}/form/{id}`, which tells the agent nobody
answered.

### Children and background sessions (activity)

Children are sessions whose `parentID` is the root or another known child.
2.x runs delegated work in the background and starts a new root execution to
deliver the result, so activity tracking matters more than on 1.x.

The activity tracker keeps its V1 input contract. `v2_activity.rs` normalises
V2 input into V1-shaped values:

- events: `session.created` → `session.created {info:{id, parentID, title,
  time.created}}`; `session.execution.started|succeeded|failed|interrupted` →
  `session.status {busy|idle}`; `session.status` (V2 `retry`, `busy`, `idle`)
  passes through; `session.step.started|ended` → `message.updated` (assistant
  info); `session.text.delta/ended` → `message.part.updated` text parts
  (accumulated, since V1 parts carry the full text); `session.tool.called`,
  `success`, `failed` → `message.part.updated` tool parts with `state.status`
  `running|completed|error`.
- reconciliation: children from `GET /api/session?parentID={id}`; statuses from
  `GET /api/session/active` (present → `busy`, absent → `idle`); history from
  `GET /api/session/{id}/message` (newest first; reversed and mapped to
  `{info, parts}`).

### Two-phase revert (rollback)

`rollback(n)` lists messages, takes the n-th most recent `user` message, and
calls `POST /api/session/{id}/revert/stage {messageID}` then
`POST /api/session/{id}/revert/commit`. Staging at a user message removes it and
everything after it, so exactly n turns go. `n = 0` is a no-op; `n` larger than
the number of user turns reverts to the first one. A failed commit clears the
stage with `DELETE /api/session/{id}/revert`.

### MCP registration

`PUT /api/experimental/mcp/bibcode?location[directory]=…` with
`{config:{type:"remote", url, headers:{Authorization}, oauth:false}}` → 204.
The route is experimental; a non-2xx answer fails the launch with the HTTP
status, as on V1.

### Inventory and skills

V2 inventory reads `/api/model` (models, variants), `/api/provider`
(connected providers), `/api/agent` (`id`, `mode`, `description`),
`/api/command` and `/api/skill`, each scoped with `location[directory]`. The
existing `build_inventory_snapshot` consumes V1 shapes, so the V2 responses are
mapped to them in one place.

### Terminal observer

2.x has no `attach` subcommand. The TUI connects with the root flags
`opencode --server <url> --session <id> <directory>`. The observer's
`OpenCodeRemoteClient` trait is the seam: a V2 client implements
`create_root`, `abort`, `statuses`, `children`, `messages` and the event stream
on `/api/*`, through the same normaliser. The capability probe accepts either
`attach` (1.x) or the 2.x root help (`--server`, `--session`).

### Fixtures

Fixtures are built from captured 2.0.29 responses with directories, IDs of
real users, and keys replaced (`<REDACTED>`, `/work/project`). The capture
harness (a scripted OpenAI-compatible endpoint on `127.0.0.1`, a scratch
`HOME`, and a script that drives `/api/session` and records `/api/event`) is
described in the report for this change and can be rerun against a new
OpenCode release to refresh the fixtures.

## Staging

Each stage leaves 2.x either working for what it covers or refused with an
actionable message.

1. **Chat core**: session create/resume, prompt and command, events → turn
   lifecycle, permissions, forms, interrupt, MCP, exact delivery lookup. Lifts
   the launch refusal. Inventory keeps an OpenCode 2 notice until stage 3;
   rollback is refused with "Rewinding isn't supported with OpenCode 2 yet."
   until stage 5.
2. **Children/activity**: the normaliser, reconciliation snapshots, Activity
   capabilities on V2.
3. **Inventory/skills**: V2 catalogs; the inventory notice goes away.
4. **Terminal observer**: V2 remote client and launch arguments.
5. **Rollback**: two-phase revert.

## Risks

- **An experimental API that churns.** Mitigations: strict version selection
  (major 2 only), tolerant decoding (unknown event types and fields are
  ignored, required fields are checked), fixtures from real captures that can
  be refreshed, and the actionable refusal for unknown majors.
- **Provider-started turns.** 2.x starts root executions on its own to deliver
  background results. The adapter reports them as turns; whether the
  supervisor keeps such a session alive is decided outside the adapter (see the
  provider background-work research note).
- **Delivery IDs on 1.x.** 1.x requires `^msg` and sorts messages by ID, so the
  durable delivery key (a UUID) is rejected there today. Fixing it on 1.x needs
  a time-ordered ID that reconciliation can recompute; it is out of scope here
  and reported separately.
