# Coddy CLI provider migration — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace Coddy Studio's five provider drivers (Codex, Claude, Cursor, Grok, OpenCode) with one `coddy` driver that speaks the Coddy CLI server's HTTP + SSE API.

**Architecture:** A supervised `coddy cli server` process per provider instance/environment, driven by a `CoddyDriver` that translates the CLI's message/run/permission/question streams into the canonical `ProviderRuntimeEventV2` vocabulary. The CLI gains three small machine-surface additions (bound-port handshake, models catalog, SSE keepalive). Legacy drivers are retired after parity, with a read-only fallback and context handoff for their threads.

**Tech Stack:** Rust/Axum/Tokio server (`coddy-studio-server`), Effect/Schema TypeScript contracts, React 19 web, Go Coddy CLI (`github.com/coddycodes/coddyagent/src/modules/cli`), Tauri 2 desktop host.

**Spec:** `docs/superpowers/specs/2026-10-26-coddy-cli-provider-design.md`

## Global Constraints

- Tasks 1–3 run in the Coddy CLI checkout (`agent/src/modules/cli`, Go 1.27). Tasks 4–18 run in `ads`.
- The managed CLI server binds `127.0.0.1` only, on an ephemeral port. Never advertise it via relay, remote access or previews.
- No new production runtime dependency (no Node sidecar, no second Go binary inside the desktop bundle).
- Never report a failed turn as success. Terminal states map exactly as in the spec §5.4.
- Do not log, profile or persist prompt/message/terminal content; keep existing redaction.
- `ads` has unrelated uncommitted work. Stage only the files named by each task; never `git add -A`; never revert unrelated changes.
- Rust tests: `cargo test -p coddy-studio-server <filter>`. TS: `vp test <path>`. Repo gates: `vp check`, `vp run typecheck`.
- Go tests from `agent/src/modules/cli`: `.\..\..\..\scripts\windows\build\windows-go-safe.ps1 -Action test -Packages ./internal/runtime/server/... -Workers 2`.
- Every task ends with its focused test green before the commit step.

## Review Focus

1. **Reconnect mid-stream after a rewritten message.** A dropped SSE re-reads messages; text already shown must not duplicate, and rewritten text must replace the item rather than append.
2. **A prompt queued behind a busy session.** Another turn's `RunComplete` for the same session arrives first; delivery must wait for the matching `RunID`.
3. **Process death after `202` and before `RunComplete`.** The delivery is ambiguous, never blindly resent; the supervisor settles the turn via `session.exited`.
4. **Two workspaces (cwd) under one server.** Sessions, models, skills and events must not leak across workspaces; SSE is per workspace with its own `client_id`.
5. **SSE frame chunk boundaries and very large snapshots.** Frames split across TCP reads and multi-megabyte message snapshots must not truncate or double-emit.

---

### Task 1: Bound-port handshake for an ephemeral CLI server

**Files:**
- Modify: `internal/runtime/server/server.go`
- Modify: `internal/cmd/server.go`
- Test: `internal/runtime/server/listen_test.go` (create)

**Interfaces:**
- Produces: `func (s *Server) ListenAndServeWithBound(onBound func(net.Addr) error) error`; flag `--port-file <path>` on `coddy cli server`, written atomically after `net.Listen` succeeds and removed on shutdown.
- Consumes: existing `listen(network, address)` helper.

- [ ] **Step 1: Write the failing test**

```go
func TestListenAndServeWithBoundReportsActualTCPAddress(t *testing.T) {
    srv := NewServer(configStore(t), "tcp", "127.0.0.1:0")
    bound := make(chan net.Addr, 1)
    go func() { _ = srv.ListenAndServeWithBound(func(a net.Addr) error { bound <- a; return nil }) }()
    addr := <-bound
    require.NotEqual(t, 0, addr.(*net.TCPAddr).Port)
    require.NoError(t, srv.Close())
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `go test ./internal/runtime/server -run TestListenAndServeWithBoundReportsActualTCPAddress -v`
Expected: FAIL — `ListenAndServeWithBound` undefined.

- [ ] **Step 3: Implement `ListenAndServeWithBound` and the `--port-file` flag**

`ListenAndServe` delegates to `ListenAndServeWithBound(nil)`. After `listen` succeeds and before `Serve`, call `onBound(s.ln.Addr())`. In `cmd/server.go`, when `--port-file` is set, write `http://<addr>` with `os.WriteFile` to `<path>.tmp` then `os.Rename`; remove the file in the shutdown path. A write failure aborts the server with the error.

- [ ] **Step 4: Run the test to verify it passes**

Run: the command above, then the whole package.
Expected: PASS.

- [ ] **Step 5: Commit (CLI repo)**

```bash
git add internal/runtime/server/server.go internal/runtime/server/listen_test.go internal/cmd/server.go
git commit -m "feat(server): report the bound address for ephemeral TCP listeners"
```

---

### Task 2: Models catalog endpoint

**Files:**
- Modify: `internal/runtime/server/endpoints.go`
- Create: `internal/runtime/server/models.go`
- Modify: `internal/runtime/server/proto.go`
- Modify: `internal/runtime/proto/proto.go` (add `ModelInfo`)
- Test: `internal/runtime/server/models_test.go`

**Interfaces:**
- Produces: `GET /v1/workspaces/{id}/models` → `[]proto.ModelInfo` with `id`, `name`, `context_window`, `max_output_tokens`, `provider_id`, `options []ModelOption{id,label,kind,values}`.
- Consumes: the workspace's resolved `config.Config` (`Providers`, `KnownProviders`), same source as `coddy cli models`.

- [ ] **Step 1: Write the failing test**

```go
func TestWorkspaceModelsReturnsCatalog(t *testing.T) {
    h := newTestHarness(t) // creates a workspace in t.TempDir() with one catalogued model
    rec := h.get("/v1/workspaces/" + h.workspaceID + "/models")
    require.Equal(t, http.StatusOK, rec.Code)
    var models []proto.ModelInfo
    require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &models))
    require.NotEmpty(t, models)
    require.NotEmpty(t, models[0].ID)
    require.Equal(t, int64(200000), models[0].ContextWindow) // the harness model's window
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `go test ./internal/runtime/server -run TestWorkspaceModelsReturnsCatalog -v`
Expected: FAIL — 404, endpoint not registered.

- [ ] **Step 3: Implement the endpoint**

Add the entry in `endpoints()`, a handler in `proto.go` that resolves the workspace config and flattens configured providers' models (deduplicated by provider/id), and a `handleGetWorkspaceModels` in `models.go`. Unknown context windows stay `0` (the client then shows no meter for that model).

- [ ] **Step 4: Run the test to verify it passes**

Run: the command above.
Expected: PASS.

- [ ] **Step 5: Commit (CLI repo)**

```bash
git add internal/runtime/server/endpoints.go internal/runtime/server/models.go internal/runtime/server/proto.go internal/runtime/proto/proto.go internal/runtime/server/models_test.go
git commit -m "feat(server): expose the workspace model catalog"
```

---

### Task 3: SSE keepalive, retry hint, and stream contract

**Files:**
- Modify: `internal/runtime/server/proto.go` (`handleGetWorkspaceEvents`)
- Test: `internal/runtime/server/events_test.go`
- Docs: `agent/docs/features/cli/server-api.md` (create)

**Interfaces:**
- Produces: `retry: 3000\n` before the first event; a `: keepalive\n\n` comment every 15 s while idle. Frame payload stays `data: {type,payload}`.
- Consumes: nothing new.

- [ ] **Step 1: Write the failing test**

```go
func TestWorkspaceEventsSendsRetryAndKeepalive(t *testing.T) {
    h := newTestHarness(t)
    resp := h.stream("/v1/workspaces/" + h.workspaceID + "/events?client_id=" + h.clientID)
    require.Equal(t, "text/event-stream", resp.Header.Get("Content-Type"))
    line := readLine(t, resp.Body)
    require.Equal(t, "retry: 3000", line)
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `go test ./internal/runtime/server -run TestWorkspaceEventsSendsRetryAndKeepalive -v`
Expected: FAIL — first line is empty/`data:`.

- [ ] **Step 3: Implement keepalive and retry**

Write `retry: 3000` after headers flush, then use a `time.NewTicker(15 * time.Second)` in the select loop that writes `: keepalive\n\n` and flushes. Document in `server-api.md`: no event ids, no replay; clients reconcile by re-reading messages.

- [ ] **Step 4: Run the test to verify it passes**

Run: the command above.
Expected: PASS.

- [ ] **Step 5: Commit (CLI repo)**

```bash
git add internal/runtime/server/proto.go internal/runtime/server/events_test.go docs/features/cli/server-api.md
git commit -m "feat(server): add SSE keepalive and retry hint"
```

---

### Task 4: Contract vocabulary for the Coddy driver

**Files:**
- Modify: `packages/contracts/src/model.ts`
- Modify: `packages/contracts/src/providerRuntime.ts`
- Modify: `packages/contracts/src/providerInstance.ts` (docs only, if needed)
- Test: `packages/contracts/src/model.test.ts`, `packages/contracts/src/providerRuntime.test.ts`

**Interfaces:**
- Produces: `CODDY_DRIVER_KIND = ProviderDriverKind.make("coddy")` (follow the existing constant style), `PROVIDER_DISPLAY_NAMES["coddy"] = "Coddy"`, `RuntimeEventRawSource` includes `"coddy.cli.sse"`.
- Consumes: existing branded slug schema; no enum closure. No `DEFAULT_MODEL_BY_PROVIDER` entry: the default model is whatever the CLI catalog lists, so a hardcoded id would go stale.

- [ ] **Step 1: Write the failing test**

```ts
test("coddy is a first-class driver kind", () => {
  expect(PROVIDER_DISPLAY_NAMES[ProviderDriverKind.make("coddy")]).toBe("Coddy");
  expect(Schema.decodeUnknownSync(RuntimeEventRaw)({ source: "coddy.cli.sse" }).source)
    .toBe("coddy.cli.sse");
});
```

- [ ] **Step 2: Run it to verify it fails**

Run: `vp test packages/contracts/src/model.test.ts packages/contracts/src/providerRuntime.test.ts`
Expected: FAIL — `undefined` display name, decode error.

- [ ] **Step 3: Add the constants and raw source**

Add the constants next to the existing driver constants using the same `ProviderDriverKind.make` pattern.

- [ ] **Step 4: Run the test to verify it passes**

Run: the command above.
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add packages/contracts/src/model.ts packages/contracts/src/providerRuntime.ts packages/contracts/src/model.test.ts packages/contracts/src/providerRuntime.test.ts
git commit -m "feat(contracts): add the coddy provider driver kind"
```

---

### Task 5: `coddy` settings, instance config, and defaults

**Files:**
- Modify: `apps/server/src/server_settings/mod.rs`
- Modify: `apps/server/src/production/control.rs`
- Modify: `packages/contracts/src/settings.ts`
- Modify: `apps/server/tests/support/hermetic_providers.rs`
- Modify: `apps/server/src/test_support/mod.rs`
- Create: `apps/server/src/provider/coddy/config.rs`
- Modify: `apps/web/src/components/settings/providerDriverMeta.ts`
- Test: `apps/server/src/server_settings/mod.rs` (inline tests), `apps/server/src/production/control.rs` (inline tests), `packages/contracts/src/settings.test.ts`, `apps/web/src/components/settings/providerDriverMeta.test.ts` (create)

**Interfaces:**
- Produces: `built_in_provider_defaults()["coddy"] = { "enabled": true, "binaryPath": "coddy", "serverUrl": "", "dataDir": "" }`; `ProvidersState.coddy: CoddyProviderSettingsState { enabled, binary_path, server_url, data_dir }`; `ProvidersPatch.coddy` plus `ProviderSettingsPatch.data_dir`; `CoddySettings` schema in contracts with the same four fields, wired into `ServerSettings.providers` and the provider-settings patch schema; `CoddyInstanceConfig { binary_path: String, server_url: String, data_dir: String }` decoded from `provider_instances.<id>.config` with defaults for every missing field; `validate_settings_document` accepts `providers.coddy`; `apply_settings_defaults` fills it; `BUILTIN_PROVIDER_DRIVERS` includes `"coddy"` so the hermetic guard stays honest.
- Consumes: Task 4's driver kind string.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn coddy_defaults_fill_partial_documents() {
    let state = decode_settings(r#"{"providers":{"coddy":{"binaryPath":"/opt/coddy"}}}"#);
    assert_eq!(state.providers.coddy.binary_path, "/opt/coddy");
    assert_eq!(state.providers.coddy.data_dir, "");
    assert!(state.providers.coddy.enabled);
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p coddy-studio-server coddy_defaults_fill_partial_documents`
Expected: FAIL — unknown field / missing struct field.

- [ ] **Step 3: Implement defaults, state field, patch field, and decoder**

Add `CoddyProviderSettingsState` (enabled, binary path, server URL, data dir) and the `coddy` field on `ProvidersState`; add the `coddy` key in `built_in_provider_defaults`, `ProvidersPatch.coddy`, `ProviderSettingsPatch.data_dir`, and the settings apply path. Extend `packages/contracts/src/settings.ts` with `CoddySettings`, add it to `ServerSettings.providers` and the provider patch, and accept it in `control.rs`'s `validate_settings_document`. Add `CoddyInstanceConfig::from_value(Value) -> CoddyInstanceConfig` in `apps/server/src/provider/coddy/config.rs` (the driver's runtime lands in Task 8). Update the hermetic provider list and its drift guard in the same change.

- [ ] **Step 4: Run the test to verify it passes**

Run: the command above, plus `vp test packages/contracts/src/settings.test.ts`, plus `cargo test -p coddy-studio-server server_settings`, plus `cargo test -p coddy-studio-server test_support`.
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add apps/server/src/server_settings/mod.rs apps/server/src/production/control.rs apps/server/src/provider/coddy/config.rs apps/server/tests/support/hermetic_providers.rs apps/server/src/test_support/mod.rs packages/contracts/src/settings.ts packages/contracts/src/settings.test.ts apps/web/src/components/settings/providerDriverMeta.ts apps/web/src/components/settings/providerDriverMeta.test.ts
git commit -m "feat(settings): add coddy provider defaults and instance config"
```

---

### Task 6: Coddy server host and registry

**Files:**
- Create: `apps/server/src/provider/coddy/host.rs`
- Create: `apps/server/src/provider/coddy/host_registry.rs` (or fold into `host.rs` if under 400 lines)
- Modify: `apps/server/src/provider/mod.rs`
- Test: `apps/server/src/provider/coddy/host.rs` (inline `#[cfg(test)]`)

**Interfaces:**
- Produces:
  - `struct CoddyServerHost { base_url: Url, child: Option<SupervisedChild>, client_id: Uuid }`
  - `async fn CoddyServerHost::connect(binary: &Path, data_dir: Option<&Path>, env: &BTreeMap<String,String>, attribution: &ProcessAttributionRegistry, external_url: Option<&str>) -> Result<Self, ProviderRuntimeError>`
  - `async fn CoddyServerHost::ensure_workspace(&self, cwd: &Path) -> Result<String, ProviderRuntimeError>` (refcounted by canonical path)
  - `async fn CoddyServerHost::release_workspace(&self, id: &str)`
  - `struct CoddyServerRegistry` keyed by `(instance_id, binary, data_dir, env fingerprint)`, returning `Arc<CoddyServerHost>`.
- Consumes: `process::supervised`, `ProcessAttributionRegistry`, `ProviderRuntimeError::{Spawn, MissingExecutable, IncompatibleProvider}`, Task 1's `--port-file`.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn connect_spawns_loopback_server_and_handshakes() {
    let host = CoddyServerHost::connect(&fake_cli_binary(), None, &BTreeMap::new(), &ProcessAttributionRegistry::new(), None).await.unwrap();
    assert!(host.base_url.host_str() == Some("127.0.0.1"));
    assert!(host.child.is_some());
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn connect_refuses_a_missing_binary() { /* expect MissingExecutable */ }

#[tokio::test]
async fn connect_reuses_the_same_workspace_for_the_same_cwd() { /* ids equal, refcount 2 */ }
```

Use a small test binary fixture (a shell script/node-free Rust test bin) that serves `/v1/health`, `/v1/version` and `/v1/workspaces`; do not require the real CLI in CI.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p coddy-studio-server provider::coddy::host`
Expected: FAIL — module/type missing.

- [ ] **Step 3: Implement spawn, handshake, workspace refcount, shutdown**

Spawn with `--host tcp://127.0.0.1:0 --port-file <tmp> -D <data_dir>`; read the port file with a 10 s deadline; `GET /v1/health` and `/v1/version`; reject an unparseable or `<1` major version. Register the child with attribution; `shutdown` kills and reaps it (external URL → no kill). The registry drops a host when its refcount reaches zero and reaps on drop.

- [ ] **Step 4: Run the tests to verify they pass**

Run: the command above.
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add apps/server/src/provider/coddy/host.rs apps/server/src/provider/coddy/host_registry.rs apps/server/src/provider/mod.rs
git commit -m "feat(provider): supervise a coddy CLI server per instance"
```

---

### Task 7: Typed HTTP client and SSE reader

**Files:**
- Create: `apps/server/src/provider/coddy/api.rs`
- Create: `apps/server/src/provider/coddy/sse.rs`
- Test: inline tests in both files

**Interfaces:**
- Produces:
  - `CoddyApiClient::{health, version, create_workspace, create_session, get_session, messages, send, cancel, grant, answer_question, set_model, skills, models}`
  - `struct CoddyEvent { payload_type: String, raw: Value }` for `message`, `run_complete`, `permission_request`, `permission_notification`, `question_batch_request`, `question_batch_notification`, `agent_event`, `session`, `mcp_event`, `skills_event`, `config_changed`
  - `SseReader::next_frame(&mut self) -> Result<Option<Vec<u8>>, CoddyTransportError>` that buffers partial frames and ignores `:` comments.
- Consumes: `reqwest` (already a dependency), `serde_json`.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn sse_reader_reassembles_a_frame_split_across_reads() {
    let body = b"data: {\"type\":\"session\",\ndata: \"payload\":{}}\n\n";
    let mut reader = SseReader::new(chunked_body(vec![&body[..20], &body[20..]]));
    assert_eq!(reader.next_frame().await.unwrap().unwrap(), br#"{"type":"session","payload":{}}"#);
}

#[test]
fn api_maps_409_to_delivery_rejection() { /* typed error, not ambiguity */ }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p coddy-studio-server provider::coddy::api provider::coddy::sse`
Expected: FAIL — types missing.

- [ ] **Step 3: Implement the client and reader**

`send` posts `{session_id, run_id, prompt, attachments}` and returns `Accepted` on `202`, `Rejected` on `4xx` (except `408`/`429` → retryable), `Ambiguous` on a transport failure after write. `sse` reads with `bytes_stream()`, splits on `\n\n`, strips an optional `data: ` prefix, and enforces a 32 MiB frame cap.

- [ ] **Step 4: Run the tests to verify they pass**

Run: the command above.
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add apps/server/src/provider/coddy/api.rs apps/server/src/provider/coddy/sse.rs
git commit -m "feat(provider): add coddy HTTP client and SSE reader"
```

---

### Task 8: Driver skeleton, launch, delivery, interrupt

**Files:**
- Create: `apps/server/src/provider/coddy/mod.rs`
- Create: `apps/server/src/provider/coddy/runtime.rs`
- Modify: `apps/server/src/provider/mod.rs`
- Test: `apps/server/src/provider/coddy/runtime.rs` (inline), plus a wire-level test in `apps/server/tests/coddy_provider.rs`

**Interfaces:**
- Produces: `CoddyDriver::spawn(request: ProviderLaunchRequest, host: Arc<CoddyServerHost>, attachments: AttachmentMaterializer) -> Result<Self, ProviderRuntimeError>`; implements `ProviderDriver` (`start`, `send`, `deliver`, `interrupt`, `next_event`, `shutdown`, `owns_native_work`).
- Consumes: Tasks 6–7; `StartedSession { resume_cursor: json!({"coddySessionId": sid, "workspaceId": wid}), .. }`.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn start_resumes_the_cursor_session_or_creates_one() { /* GET 200 keeps id; GET 404 creates */ }

#[tokio::test]
async fn deliver_resolves_accepted_on_202_and_settles_on_matching_run_complete() {
    /* send -> Accepted{turn_id}; push RunComplete{turn_id}; next_event -> turn.completed */
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p coddy-studio-server provider::coddy::runtime`
Expected: FAIL — `CoddyDriver` missing.

- [ ] **Step 3: Implement launch, send, deliver, interrupt, shutdown**

`start` ensures the workspace, resumes or creates the session, emits `session.started`, `thread.started`, opens the SSE stream, and spawns the pump. `interrupt` posts `cancel` and returns without settling. `shutdown` aborts the pump, releases the workspace, and never kills an external server.

- [ ] **Step 4: Run the tests to verify they pass**

Run: the command above.
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add apps/server/src/provider/coddy/mod.rs apps/server/src/provider/coddy/runtime.rs apps/server/src/provider/mod.rs apps/server/tests/coddy_provider.rs
git commit -m "feat(provider): add the coddy driver launch and delivery path"
```

---

### Task 9: Message, tool and turn mapping with snapshot diffing

**Files:**
- Create: `apps/server/src/provider/coddy/stream.rs`
- Create: `apps/server/src/provider/coddy/map.rs`
- Test: inline tests using `packages/contracts/fixtures/thread-stream/` style JSON fixtures under `apps/server/src/provider/coddy/fixtures/`

**Interfaces:**
- Produces:
  - `MessageStreams::observe(&mut self, message: &CliMessage) -> Vec<Delta>` where `Delta::{Start, Append(String), Replace, Complete}`
  - `map_cli_message(...) -> Vec<ProviderEvent>` emitting `item.started|updated|completed` and `content.delta`
  - `map_run_complete(...) -> ProviderEvent` (`turn.completed`, idempotent with `Finish`)
- Consumes: canonical field names from `providerRuntime.ts`; Task 8's driver.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn appended_text_emits_only_the_suffix() { /* "Hel" then "Hello" -> "Hel", "lo" */ }

#[test]
fn rewritten_text_closes_and_restarts_the_assistant_item() { /* "Hello" then "Hi" */ }

#[test]
fn run_complete_reconciles_a_missing_final_text() { /* snapshot "Hel", run text "Hello" */ }

#[test]
fn tool_call_lifecycle_maps_to_item_events() { /* finished:false -> started; result -> completed */ }

#[test]
fn finish_reason_maps_to_terminal_state() { /* end_turn, max_tokens, content_filter, canceled */ }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p coddy-studio-server provider::coddy::stream provider::coddy::map`
Expected: FAIL — types missing.

- [ ] **Step 3: Implement diffing and mapping**

Tool classification: `shell`/`bash` → `command_execution`, `edit`/`write`/`multiedit` → `file_change`, `mcp_*` → `mcp_tool_call`, `web_search`/`fetch` → `web_search`, `view`/`download` → `image_view`, everything else → `dynamic_tool_call`. `Finish.message`/`Details` goes into `errorMessage`/`data`, never into a user-visible success message.

- [ ] **Step 4: Run the tests to verify they pass**

Run: the command above.
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add apps/server/src/provider/coddy/stream.rs apps/server/src/provider/coddy/map.rs apps/server/src/provider/coddy/fixtures
git commit -m "feat(provider): map coddy messages and turns to canonical events"
```

---

### Task 10: Approvals and questions

**Files:**
- Modify: `apps/server/src/provider/coddy/map.rs`
- Modify: `apps/server/src/provider/coddy/runtime.rs`
- Test: inline tests

**Interfaces:**
- Produces: `approve(request_id, decision)` → `POST /permissions/grant` with `allow|allow_session|deny`; `answer(request_id, answers)` → `POST /questions/answer`. Emits `request.opened|resolved` and `user-input.requested|resolved`.
- Consumes: Task 7 client.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn permission_request_maps_tool_and_path_to_a_request_type() { /* shell -> command_execution_approval */ }

#[tokio::test]
async fn approve_sends_the_matching_permission_action() { /* allow_session on "Allow for session" */ }

#[test]
fn question_batch_maps_choices_and_fill_in() { /* options carry label+description */ }

#[tokio::test]
async fn resolved_question_emits_user_input_resolved() { /* answers round-trip */ }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p coddy-studio-server provider::coddy::map provider::coddy::runtime`
Expected: FAIL.

- [ ] **Step 3: Implement mapping and responses**

Map permission `Action`/`ToolName` to canonical `CanonicalRequestType`; unknown tools → `dynamic_tool_call`. Questions: `select`/`multi_select` → options, `confirm` → two options (`Yes`/`No`), `fill_in` → a single free-text option whose label is the question, because canonical options cannot be empty. Cancellation emits `user-input.resolved` with `{ "cancelled": true }`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: the command above.
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add apps/server/src/provider/coddy/map.rs apps/server/src/provider/coddy/runtime.rs
git commit -m "feat(provider): map coddy approvals and questions"
```

---

### Task 11: Usage, session state, MCP status and agent errors

**Files:**
- Modify: `apps/server/src/provider/coddy/map.rs`
- Modify: `apps/server/src/provider/coddy/runtime.rs`
- Test: inline tests

**Interfaces:**
- Produces: `thread.token-usage.updated { usedTokens: prompt_tokens, inputTokens: sent_tokens, outputTokens: completion_tokens, maxTokens: catalog[model].context_window }`, `session.state.changed`, `mcp.status.updated`, `runtime.error` + failed `turn.completed`.
- Consumes: Task 2's models catalog (cached by the host), Task 7 client.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn usage_uses_prompt_tokens_as_context_size() { /* prompt 12000, window 200000 -> used 12000, max 200000 */ }

#[test]
fn mcp_status_aggregates_every_server_state() { /* connected + error -> two entries */ }

#[test]
fn agent_error_closes_an_open_turn_as_failed() { /* errorClass provider_error */ }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p coddy-studio-server provider::coddy::map`
Expected: FAIL.

- [ ] **Step 3: Implement the mappings**

`session.is_busy`/event origin decides `running` vs `ready`. A `session` event without counters emits no usage. MCP state strings map `connected|starting|needs-auth|disconnected|error` one-to-one; an unknown state becomes `error` with the raw state as detail. `agent_event.error` while a turn is open settles it as failed, then is not emitted twice.

- [ ] **Step 4: Run the tests to verify they pass**

Run: the command above.
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add apps/server/src/provider/coddy/map.rs apps/server/src/provider/coddy/runtime.rs
git commit -m "feat(provider): report coddy usage, mcp status and errors"
```

---

### Task 12: Reconnect, reconciliation and resume

**Files:**
- Modify: `apps/server/src/provider/coddy/runtime.rs`
- Modify: `apps/server/src/provider/coddy/stream.rs`
- Test: inline tests

**Interfaces:**
- Produces: pump reconnect loop (250 ms → 10 s backoff, bounded), snapshot re-read via `messages`, `session.exited` on process loss, resume cursor reuse, `AcceptedInNewConversation` + context-handoff mark when the CLI session is gone.
- Consumes: Tasks 7–9.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn reconnect_replays_missed_text_once() { /* drop stream, message grew, expect one suffix */ }

#[tokio::test]
async fn process_exit_emits_session_exited_and_holds_the_delivery_ambiguous() { }

#[tokio::test]
async fn missing_session_starts_a_new_conversation_and_marks_handoff() { }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p coddy-studio-server provider::coddy::runtime`
Expected: FAIL.

- [ ] **Step 3: Implement reconnect and resume**

Never resend a `202`-accepted turn after a reconnect; reconcile by `RunID` only. A `404` on `GET session` at launch creates a new session and returns `AcceptedInNewConversation` on the first delivery; the supervisor's existing handoff text carries the context. A dead child is not restarted mid-turn: `session.exited` settles, the next turn relaunches.

- [ ] **Step 4: Run the tests to verify they pass**

Run: the command above.
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add apps/server/src/provider/coddy/runtime.rs apps/server/src/provider/coddy/stream.rs
git commit -m "feat(provider): reconcile coddy streams across reconnects"
```

---

### Task 13: Inventory, factory wiring and capabilities

**Files:**
- Modify: `apps/server/src/production/provider_inventory.rs`
- Modify: `apps/server/src/production/provider_inventory/capabilities.rs`
- Modify: `apps/server/src/production/provider_runtime.rs` (`NativeProviderDriverFactory::create`, `canonical_provider_kind`, `provider_binary_settings`, `provider_supports_agent_activity`)
- Modify: `apps/server/src/provider/mod.rs` (`supports_turn_steer`, `supports_conversation_rewind`)
- Modify: `apps/server/src/production/turn_delivery.rs` (exact reconciliation arm)
- Create: `apps/server/src/provider/coddy/model.rs`
- Test: inline tests in `provider_inventory.rs`, `provider_runtime.rs`, `turn_delivery.rs`

**Interfaces:**
- Produces: `probe_coddy(...) -> Option<CoddyInventory { models, skills, provider_status, version }>`; `canonical_provider_kind("coddy") == Some("coddy")`; `provider_binary_settings` reads `config.binaryPath`, `config.serverUrl`, `config.dataDir`; `provider_supports_agent_activity("coddy") == true`; `create` arm `"coddy"`; capability flags (`supportsContextWindowUsage = true`, `supportsMcpStatus = true`, `supportsTurnSteer = false`, `supportsConversationRewind = false`); `turn_delivery` uses exact reconciliation for `"coddy"` (the CLI answers `GET session/messages` by `RunID`-derived message id).
- Consumes: Tasks 2, 6, 8.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn inventory_lists_coddy_models_and_skills() { /* fake server: 2 models, 1 skill */ }

#[test]
fn coddy_is_supported_by_the_native_factory() { /* create() with provider "coddy" is not UnsupportedProvider */ }

#[test]
fn coddy_route_settings_read_binary_server_url_and_data_dir() { /* instance config -> launch request */ }

#[test]
fn coddy_uses_exact_delivery_reconciliation() { /* turn_delivery arm, not Uncertain */ }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p coddy-studio-server coddy_`
Expected: FAIL.

- [ ] **Step 3: Implement the probe and every integration seam**

Add `"coddy"` to `definitions()` and `built_in_provider_defaults`-driven lists, `canonical_provider_kind`, `provider_binary_settings`, `provider_supports_agent_activity`, `supports_turn_steer`/`supports_conversation_rewind`, the delivery reconciliation arm, `discover_capabilities` (skills only), the factory arm, and the inventory branch. Cache the catalog for 60 s per instance/cwd like the other providers. Skills carry `invocation: "slash"`; slash commands are `compact` and `summarize` only; agents stay empty.

- [ ] **Step 4: Run the tests to verify they pass**

Run: the command above, plus `cargo test -p coddy-studio-server provider_inventory` and `cargo test -p coddy-studio-server turn_delivery`.
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add apps/server/src/production/provider_inventory.rs apps/server/src/production/provider_inventory/capabilities.rs apps/server/src/production/provider_runtime.rs apps/server/src/provider/mod.rs apps/server/src/production/turn_delivery.rs apps/server/src/provider/coddy/model.rs
git commit -m "feat(provider): wire coddy into inventory, routing and delivery recovery"
```

---

### Task 14: Settings UI and legacy-thread fallback

**Files:**
- Modify: `apps/web/src/components/settings/ProviderSettingsForm.tsx`
- Modify: `apps/web/src/components/settings/AddProviderInstanceDialog.tsx`
- Modify: `apps/web/src/components/settings/providerDriverMeta.ts`
- Modify: `apps/web/src/components/settings/ProviderInstanceCard.tsx`
- Modify: `apps/web/src/components/settings/ConnectionsSettings.logic.ts`
- Modify: `apps/web/src/composer-logic.ts` (read-only guard)
- Modify: `apps/web/src/components/chat/*` (banner + **Continue with Coddy**)
- Test: the matching `*.test.tsx`/`*.test.ts` files

**Interfaces:**
- Produces: Coddy settings fields (Binary path, Data directory, Server URL, Display name), a Coddy icon/accent, and a composer that is disabled with **Continue with Coddy** for a thread bound to a retired driver.
- Consumes: Task 5's settings shape; existing `threadProviderBinding.ts`, `providerInstances.ts`.

- [ ] **Step 1: Write the failing tests**

```tsx
test("coddy settings expose binary, data dir and server url", () => { /* render, query by label */ });

test("a thread bound to a retired driver shows Continue with Coddy", () => {
  /* composer disabled; button calls createThread with driver "coddy" */
});
```

- [ ] **Step 2: Run them to verify they fail**

Run: `vp test apps/web/src/components/settings/ProviderSettingsForm.test.tsx apps/web/src/composer-logic.test.ts`
Expected: FAIL.

- [ ] **Step 3: Implement the UI and the guard**

The form writes `providers.coddy.{binaryPath,dataDir,serverUrl}` through the existing settings RPC. The guard treats any driver not in the supported allowlist as read-only; **Continue with Coddy** creates a new thread with `driver: "coddy"` and copies the source thread id into the handoff metadata used by `launch_session`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: the command above.
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add apps/web/src/components/settings apps/web/src/composer-logic.ts apps/web/src/components/chat
git commit -m "feat(web): add coddy provider settings and legacy thread fallback"
```

---

### Task 15: Documentation and runbooks

**Files:**
- Create: `docs/providers/coddy.md`
- Modify: `docs/providers/README.md`
- Modify: `docs/architecture/providers.md`
- Modify: `docs/getting-started/provider-setup.md`
- Modify: `docs/reference/encyclopedia.md`
- Modify: `docs/testing/README.md` and `docs/testing/{windows,macos,linux}-desktop.md` (provider visibility)

**Interfaces:**
- Produces: the user-facing install/authenticate story (`coddy login`, `CODDY_API_KEY`), loopback security note, capability table (steer/rewind/activity/terminal), troubleshooting (binary missing, version mismatch, server URL unreachable).
- Consumes: Tasks 1–14.

- [ ] **Step 1: Write the doc assertions**

Add a `scripts/docs-provider-links.test.ts` check (or extend the existing docs link test) asserting every `docs/providers/*.md` is linked from `docs/providers/README.md` and that `coddy.md` exists.

- [ ] **Step 2: Run it to verify it fails**

Run: `vp test scripts/docs-provider-links.test.ts`
Expected: FAIL — `coddy.md` missing.

- [ ] **Step 3: Write the docs**

State plainly: Coddy Studio drives `coddy cli server`; the server is loopback-only; the provider terminal is unobserved; steering and conversation rewind are unsupported; Checkpoint revert restores files only; the CLI version floor and how to set a custom binary/data directory.

- [ ] **Step 4: Run it to verify it passes**

Run: the command above.
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add docs/providers docs/architecture/providers.md docs/getting-started/provider-setup.md docs/reference/encyclopedia.md docs/testing scripts/docs-provider-links.test.ts
git commit -m "docs: document the coddy provider and updated runbooks"
```

---

### Task 16: Disable legacy drivers and migrate settings

**Files:**
- Modify: `apps/server/src/server_settings/mod.rs`
- Modify: `apps/server/src/production/provider_inventory.rs`
- Modify: `apps/server/src/provider_terminal/supervisor.rs`
- Modify: `apps/server/src/production/provider_usage/mod.rs`
- Modify: `apps/server/src/agent_sessions/mod.rs`
- Modify: `apps/server/src/production/provider_maintenance/source.rs`
- Modify: `apps/web/src/components/settings/providerDriverMeta.ts`
- Test: inline Rust tests, `providerDriverMeta.test.ts`

**Interfaces:**
- Produces: `fn supported_driver_kinds() -> &'static [&'static str]` returning `["coddy"]`; inventory skips unsupported drivers; the provider-terminal inventory, usage RPC, CLI session import and update-capability probe return nothing for retired drivers; settings keep persisted legacy values untouched but never publish them; the UI hides them.
- Consumes: Task 5.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn legacy_drivers_are_hidden_but_preserved() {
    let state = decode_settings(r#"{"providers":{"codex":{"enabled":true,"binaryPath":"codex"}}}"#);
    assert_eq!(state.providers.codex.binary_path, "codex");
    assert!(published_driver_kinds(&state) == vec!["coddy"]);
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p coddy-studio-server legacy_drivers_are_hidden_but_preserved`
Expected: FAIL.

- [ ] **Step 3: Implement the allowlist**

Add `supported_driver_kinds`, use it in inventory publishing, the factory (unsupported → `UnsupportedProvider`), the Settings list, the add-instance dialog, `ProviderTerminalInventory::from_settings`, the usage RPC registration, `agentSessions.scan|import`, and `provider_maintenance` capability lookup. No persisted key is deleted.

- [ ] **Step 4: Run the test to verify it passes**

Run: the command above.
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add apps/server/src/server_settings/mod.rs apps/server/src/production/provider_inventory.rs apps/web/src/components/settings/providerDriverMeta.ts
git commit -m "feat(settings): publish only the coddy driver"
```

---

### Task 17: Delete the legacy drivers

**Files:**
- Delete: `apps/server/src/provider/{codex,claude,cursor,grok,opencode}`
- Delete: `apps/server/src/provider_terminal/{codex,claude,opencode}.rs` (keep the supervisor and its non-provider parts)
- Delete: `apps/server/tests/provider_{codex,claude,cursor,grok,opencode}.rs`
- Delete: `apps/server/tests/fixtures/{claude-provider,opencode-v2,provider-terminal}`, `packages/contracts/fixtures/{codex-provider,cursor-provider,grok-provider,opencode-provider}`
- Modify: `apps/server/src/provider/mod.rs`, `apps/server/src/provider_terminal/supervisor.rs`, `apps/server/src/production/provider_usage/mod.rs`, `apps/server/src/agent_sessions/mod.rs`, `apps/server/Cargo.toml` (remove now-unused driver crates), and any remaining `canonical_provider_kind`/`provider_binary_settings` aliases
- Modify: `apps/server/tests/activity_provider_conformance.rs` and `apps/server/tests/support/hermetic_providers.rs`
- Test: full `cargo test -p coddy-studio-server`

**Interfaces:**
- Produces: no production reference to `codex`, `claudeAgent`, `cursor`, `grok`, `opencode`.
- Consumes: Tasks 13–16.

- [ ] **Step 1: Write the guard test**

```rust
#[test]
fn no_legacy_driver_is_referenced_in_production_sources() {
    // Walk apps/server/src; fail on the banned identifiers outside cfg(test) and comments.
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p coddy-studio-server no_legacy_driver_is_referenced_in_production_sources`
Expected: FAIL — many matches.

- [ ] **Step 3: Delete the drivers and their orphans**

Remove modules, re-exports, terminal observers, usage fetchers, session importers, update rules, driver-only dependencies, fixtures and tests. Keep `provider_terminal`'s supervisor, adapters it still needs, and the canonical contracts. Fix compile errors by deleting, not by stubbing.

- [ ] **Step 4: Run focused and full tests**

Run: `cargo test -p coddy-studio-server`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -u apps/server
git add -u packages/contracts
git commit -m "refactor(provider): remove the legacy provider drivers"
```

---

### Task 18: End-to-end verification and acceptance report

**Files:**
- Create: `docs/plans/2026-10-26-coddy-cli-provider-acceptance.md`
- No production code.

**Interfaces:**
- Consumes: everything above.

- [ ] **Step 1: Run the full gates**

Run: `vp check`, `vp run typecheck`, `cargo fmt --all --check`, `cargo clippy -p coddy-studio-server --all-targets -- -D warnings`, `cargo test -p coddy-studio-server`, `vp test`.

- [ ] **Step 2: Run the live scenario on Windows**

Install `coddy`, set `CODDY_API_KEY`, start the desktop app, create a Coddy thread in a temp repo, send a prompt, approve a shell command, answer a question, stop a turn, disconnect the stream (network toggle), and confirm: text resumes once, no duplicate message, usage meter moves, MCP row shows the CLI's servers.

- [ ] **Step 3: Reconnect and resume scenario**

Kill the CLI process mid-turn; confirm the turn settles, the composer re-enables, and the next prompt resumes the same CLI session (transcript present).

- [ ] **Step 4: Write the acceptance report**

Record commands, outputs, screenshots/log paths (no prompt content), residual risk, and the profiler coverage line.

- [ ] **Step 5: Commit**

```bash
git add docs/plans/2026-10-26-coddy-cli-provider-acceptance.md
git commit -m "docs: record coddy provider migration acceptance"
```

---

## Self-review

- **Spec coverage:** process topology (Task 6), handshake (1, 6), delivery (8), mapping (9–11), reconnect (12), inventory (13), UI/migration (14, 16), removal (17), verification (18), docs (15).
- **Type consistency:** `CoddyServerHost::ensure_workspace`, `CoddyApiClient`, `MessageStreams::observe`, `CoddyDriver::spawn` are named identically in every task that consumes them.
- **Review Focus:** reconnect rewrite (Task 9/12), queued turn (8), death after 202 (12), workspace isolation (6/13), chunk boundaries (7).
- **Proportion:** steps name signatures and assertions, not implementations; the largest bodies (mapping tables) are decisions, not transcripts.
