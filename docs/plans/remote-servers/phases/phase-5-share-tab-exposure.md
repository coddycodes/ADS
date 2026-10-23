# Phase 5 — "Share this host" Tab & Grant-Driven Exposure Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps
> use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The "Share this host" tab of the Remote Servers settings section: pairing-offer
generation (intent radio, address picker, browser URL + `bibcode://pair` deep link + QR),
paired-client management with revocation, and the grant-driven exposure state machine
(spec §4.2 generation side, §4.6, §4.8; §3 threat-model copy).

**Architecture:** The server records a `reach` plus a mint-time `off_host` flag per
pairing grant (link and the session it becomes), derives desired exposure from the
unrevoked off-host flags, and actively terminates a revoked client's live WebSocket
sessions; the pairing-offer mint endpoint landed by Phase 3
(`POST /api/auth/pairing-offer`, which composes the full pairing-code payload
server-side) is extended so the grants it mints persist both fields. The desktop host
gains an apply-with-rollback exposure
command (restart-based rebind through the existing exposure machinery) plus Windows
firewall rule management. The web renderer orchestrates the ceremony: widen → wait ready
→ mint → compose artifacts, and runs a narrow-only reconciler so revoking the last
off-host grant reverts the bind.

**Tech Stack:** Rust (Axum/Tokio, rusqlite migrations, Tauri 2 bridge commands,
`netsh advfirewall` on Windows), TypeScript (React, Effect Schema/Atom, TanStack Router
settings routes), existing QR component (`apps/web/src/components/ui/qr-code.tsx`).

**Spec:** `docs/plans/remote-servers/remote-servers-spec.md` — §4.2 (pairing-code
generation side), §4.6 (exposure state machine), §4.8 (Share tab UI contract), §3
(threat-model copy). Master plan: `docs/plans/remote-servers/remote-servers-plan.md`
(this file is Phase 5).

## Global Constraints

Copied from the master plan; every task's requirements implicitly include these.

- Zero reference-product strings in code, identifiers, UI copy, or comments; product
  strings are "BiBCode"/"bibcode" by context (spec D16).
- `packages/contracts` stays schema-only; every new WS method gets a Rust mirror and an
  entry in the TS↔Rust parity manifests; every RPC method declares exactly one scope in
  `apps/server/src/auth/scope.rs`. (This phase adds HTTP endpoints, not WS methods — the
  HTTP parity manifest in `packages/contracts/src/authRustParity.test.ts` +
  `packages/contracts/fixtures/auth-http/` is the equivalent obligation.)
- All new descriptor/contract fields are additive and decode-defaulted so older servers
  keep working (no breaking wire changes).
- No production Node runtime, no Electron, no sidecars; desktop-privileged operations
  cross `DesktopBridge`; normal traffic uses typed HTTP/WS RPC.
- Preserve unrelated worktree changes — in particular the user's pending deletions under
  `docs/plans/2026-08-24-environment-project-management/` must never be restored or
  committed by this work.
- Every phase: focused tests for changed behavior, `vp check`, `vp run typecheck`; Rust
  phases additionally `cargo fmt --all --check`, relevant Rust tests, and Clippy for
  affected targets with warnings denied; final `git diff`/`git status --short` review.
- Living docs (`docs/architecture/remote.md`, `connection-runtime.md`, `overview.md`) and
  `docs/testing/` runbooks update in the same patch as the behavior they describe; phases
  that change no runbook-relevant behavior state "reviewed and remain accurate".

## Consumed interfaces (from earlier phases — do not redefine)

| Interface                                                                                                                                                                                                                                                                                                                              | Owner         | This plan's usage                                                                                                                                                                                                                                                                                                                            |
| -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `packages/contracts/src/remotePairing.ts` — pairing-code payload schema, fields exactly `v`, `endpoint`, `name`, `token`, `hostKey`, `reach`, `storageInstanceId` (spec §4.2)                                                                                                                                                          | Phase 3       | Reach literals + payload shape. **Assumption:** the file exports `RemotePairingReach = Schema.Literals(["another-device", "this-computer", "custom"])`. If Phase 3 exported the literal under another name, use that name; if it exists only inline, add the named export to `remotePairing.ts` in Task 2 (schema-only, no behavior change). |
| Rust mirror of the payload in `apps/server/src/auth/` (spec §4.2)                                                                                                                                                                                                                                                                      | Phase 3       | Used inside the Phase 3-landed offer handler (payload composition + encoding); this phase never calls it directly. Verify with `rg "RemotePairingCodePayload                                                                                                                                                                                 | encode_pairing_code" apps/server/src` if Task 4's handler edit touches nearby code. |
| Host identity public key accessor (spec §4.1)                                                                                                                                                                                                                                                                                          | Phase 3       | Used inside the Phase 3-landed offer handler; this phase never calls it directly. Verify with `rg "host_identity" apps/server/src/auth` if Task 4's handler edit touches nearby code.                                                                                                                                                        |
| `POST /api/auth/pairing-offer` mint surface: handler `create_pairing_offer`, TS `AuthCreatePairingOfferInput` / `AuthPairingOfferResult` (input carries `reach`, payload embeds it), fixtures `requests/pairing-offer.json` / `responses/pairing-offer.json`, `ROUTE_INVENTORY` entry, scope `access:write`, endpoint/reach validation | Phase 3       | Task 4 modifies **only the handler's issuance call** so the minted grant persists `reach`; everything else is consumed as-is (no new fixtures, routes, or schemas here).                                                                                                                                                                     |
| Remote Servers settings section + tab shell (`/settings/remote-servers`, Connect tab)                                                                                                                                                                                                                                                  | Phase 4       | Task 10 mounts `ShareThisHostTab` as the "Share this host" tab. Locate the shell at execution time via `rg "Share this host\|remote-servers" apps/web/src/routes apps/web/src/components/settings`; the route file is `apps/web/src/routes/settings.remote-servers.tsx`.                                                                     |
| `classifyPairingEndpoint` in `packages/shared/src/advertisedEndpoint.ts`, signature `(endpoint: string) => "loopback" \| "private-network" \| "public" \| "unconnectable"`                                                                                                                                                             | Phase 3       | Task 5 wraps it in a Phase 5-local mapping (`shareClassForPairingEndpoint`); this phase never redefines or extends the shared export.                                                                                                                                                                                                        |
| `activeEnvironmentIdAtom`, `authEnvironment.accessChanges`, `PairingClientsList`, `QRCodeSvg`, `desktopNetworkAccessStateAtom`                                                                                                                                                                                                         | existing code | Reused as-is (verified against current source in this plan).                                                                                                                                                                                                                                                                                 |

Interfaces this phase **produces** (consumed by Phase 3's add-flow and Phase 6/7 UI):

- Pairing grants persist `reach` and a mint-time `off_host` flag (server DB;
  `AuthPairingLink.reach` / `AuthClientSession.reach` optional contract fields —
  `off_host` stays server-internal).
- `GET /api/auth/share-state` → `AuthShareStateResult` (desired exposure derivation
  over the persisted off-host flags).
- Grants minted by Phase 3's `POST /api/auth/pairing-offer` persist their `reach` and
  computed `off_host` (feeding the amended §4.6 derivation).
- Revoking a client actively terminates its live WebSocket sessions (amended spec §3).
- `DesktopBridge.applyServerExposure(desired)` (replaces `setServerExposureMode`).
- `shareClassForPairingEndpoint` in
  `apps/web/src/components/settings/remote-servers/endpointClass.ts` (a mapping over
  Phase 3's shared `classifyPairingEndpoint`).

## Design decisions pinned by this plan

1. **Widening/reverting is a backend restart — be honest about it.**
   `desktop_bridge_set_server_exposure_mode` today persists the setting and calls
   `BackendSupervisor::restart_default_if_active` (`apps/desktop/src-tauri/src/bridge.rs:1189`),
   which **stops every backend slot (native primary and WSL secondaries) and starts them
   again** (`apps/desktop/src-tauri/src/backend.rs:1153`). Live WebSocket connections
   drop, in-flight provider turns on local backends terminate, terminals close. Durable
   orchestration state persists and the renderer reconnects via the `backend-ready`
   event. The new `applyServerExposure` command keeps this mechanism (it _is_ the
   existing exposure machinery the spec requires) and adds verification + rollback. UI
   copy must warn before widening: "Enabling remote access restarts the local server.
   Running turns on this machine will stop."
2. **Ceremony sequencing: widen → wait-for-ready → mint → compose.** Never mint first: a
   minted off-host grant with a failed widen would pin desired exposure wide with no
   working bind. With widen-first, a mint failure after a successful widen leaves a wide
   bind with zero off-host grants — exactly the state the narrow-only reconciler
   (decision 6) self-heals.
3. **The persisted desktop setting is the launch-time cache of derived exposure.** The
   bind host must be chosen before the server (and its grant DB) is running, so the
   desktop cannot derive exposure pre-launch. After this phase the manual toggle is gone:
   `server_exposure_mode` in desktop settings is written **only** by the widen ceremony
   and the revert path, so later launches honor grant state by construction. A
   `this-computer` grant never touches the setting, so it never widens a later launch
   (spec §4.6).
4. **Legacy-grant and custom-grant semantics (now pinned in amended spec §4.6).**
   Grants that predate `reach` decode as `reach = NULL` / `off_host = NULL`. Rules:
   (a) widening is only ever triggered by the explicit generate ceremony — no grant
   state ever auto-widens; (b) auto-revert to loopback requires zero unrevoked
   off-host grants **and** zero legacy (`NULL`) one-time-token bearer/DPoP grants.
   "Off-host" is the **mint-time persisted flag**: `another-device` ⇒ true,
   `this-computer` ⇒ false, `custom` ⇒ whether the offered endpoint classified
   off-host when the offer was minted (a loopback custom offer — SSH tunnel, reverse
   proxy — never widens). Derivation reads only the flag, never re-derives from the
   reach literal, so generator and server always agree. This keeps users who manually
   enabled network access (and whose paired clients predate reach) working after
   upgrade; the UI explains legacy grants block auto-revert until re-paired or
   revoked. Cloud-relay grants (`subject != "one-time-token"`) and browser-cookie /
   desktop-bootstrap sessions never count — they do not depend on a wide bind.
5. **Windows firewall: one program-scoped inbound allow rule.** The backend port is
   dynamically picked (`portpicker` fallback in `backend.rs`), so a port-scoped rule
   would silently go stale. The rule allows inbound TCP to the desktop executable
   (`netsh advfirewall firewall add rule name="BiBCode Remote Access" dir=in
   action=allow program=<exe> protocol=TCP profile=domain,private enable=yes`), added on
   widen, deleted on revert. Command construction is unit-tested on all platforms;
   execution is `#[cfg(windows)]` and validated per `docs/testing/windows-desktop.md`.
6. **Narrow-only reconciler at app level.** A headless desktop-mode hook mounted in
   `AppRoot` watches auth-access changes and reverts to loopback when the server-derived
   desired exposure is loopback (with a toast). It never widens. This covers revocation
   of the last off-host grant from any client, not just the Share tab.
7. **Scope boundary.** Grant-driven widening governs the **native primary** backend
   only. WSL backends keep their existing WSL-owned wildcard transport behavior
   untouched; headless `bibcode serve` exposure stays operator-owned (read-only UI with
   CLI guidance).
8. **Known degradation while wide (pre-existing, now mainstream):** a native primary
   bound non-loopback disables the desktop maintenance routes
   (`maintenance_routes_enabled`, `apps/server/src/maintenance.rs:630`), so desktop
   update protection is unavailable while sharing. Documented in Task 12, not "fixed"
   here.

## File map

- Modify: `apps/server/src/persistence/migrations.rs`, `apps/server/src/persistence/repositories.rs`
- Modify: `apps/server/src/auth/service.rs`, `apps/server/src/auth/model.rs`, `apps/server/src/auth/http.rs`, `apps/server/src/http.rs` (WebSocket handler — Task 4b)
- Modify: `packages/contracts/src/auth.ts`, `packages/contracts/src/environmentHttp.ts`, `packages/contracts/src/authRustParity.test.ts`, `packages/contracts/scripts/export-rust-auth-fixtures.ts`, `packages/contracts/fixtures/auth-http/*`
- Create: `apps/desktop/src-tauri/src/firewall.rs`; Modify: `apps/desktop/src-tauri/src/bridge.rs`, `apps/desktop/src-tauri/src/lib.rs`
- Modify: `packages/contracts/src/ipc.ts`, `apps/web/src/tauriDesktopBridge.ts` (+ test)
- Modify: `apps/web/src/environments/primary/auth.ts`, `apps/web/src/environments/primary/index.ts`
- Create: `apps/web/src/components/settings/remote-servers/endpointClass.ts` (+ test), `apps/web/src/components/settings/remote-servers/shareOffer.ts` (+ test), `apps/web/src/components/settings/remote-servers/ShareThisHostTab.tsx` (+ test)
- Create: `apps/web/src/state/shareExposureReconciler.ts` (+ logic test); Modify: `apps/web/src/AppRoot.tsx`
- Modify: `apps/web/src/components/settings/ConnectionsSettings.tsx` (remove superseded manual-toggle surface; exact residue depends on Phase 4's restructuring)
- Modify: `docs/architecture/remote.md`, `docs/architecture/overview.md`, `docs/testing/windows-desktop.md` (+ review `linux-desktop.md`, `macos-desktop.md`, `cross-platform-validation.md`)

---

### Task 1: Persist `reach` + `off_host` on pairing links and sessions (migration + repositories)

**Files:**

- Modify: `apps/server/src/persistence/migrations.rs` (registry ~line 617, add `migration_046`)
- Modify: `apps/server/src/persistence/repositories.rs` (structs ~lines 1442–1494, SQL consts ~lines 1492–1494, `create_auth_pairing_link` ~line 1044, `create_auth_session` ~line 1111, decoders)
- Test: repository round-trip tests inside `apps/server/src/persistence/repositories.rs` test module (follow the file's existing test placement; if auth repo tests live in `apps/server/tests/auth_http.rs`'s restart tests instead, put the round-trip test beside the existing auth persistence tests found via `rg "create_auth_pairing_link" apps/server --glob '*test*'`)

**Interfaces:**

- Consumes: existing `AuthPairingLink`, `NewAuthSession`, `AuthSession` row structs.
- Produces: `reach: Option<String>` and `off_host: Option<bool>` fields on all three
  structs; migration id 46 (`AuthPairingReach`). Later tasks rely on the exact field
  names `reach` and `off_host`.

Note: migration ids up to 45 exist today (`migration_045`). If an earlier remote-servers
phase claimed 46 by the time this executes, take the next free id and keep the name
`AuthPairingReach`.

- [x] **Step 1: Write the failing test** — round-trip a pairing link and a session with
      `reach` through the repositories (place beside the existing repository tests; mirror
      their store-construction helper):

```rust
#[tokio::test]
async fn auth_pairing_reach_round_trips_through_persistence() {
    let (repositories, _temp) = test_repositories().await; // reuse the module's existing helper
    let link = AuthPairingLink {
        id: "link-1".into(),
        credential: "credential-1".into(),
        method: "one-time-token".into(),
        scopes: serde_json::json!(["orchestration:read"]),
        subject: "one-time-token".into(),
        label: None,
        proof_key_thumbprint: None,
        created_at: "2026-08-27T00:00:00.000Z".into(),
        expires_at: "2027-08-27T00:00:00.000Z".into(),
        consumed_at: None,
        revoked_at: None,
        reach: Some("another-device".into()),
        off_host: Some(true),
    };
    repositories.create_auth_pairing_link(link).await.unwrap();
    let listed = repositories
        .list_active_auth_pairing_links("2026-08-27T00:00:01.000Z".into())
        .await
        .unwrap();
    assert_eq!(listed[0].reach.as_deref(), Some("another-device"));
    assert_eq!(listed[0].off_host, Some(true));

    let session = NewAuthSession {
        session_id: "session-1".into(),
        subject: "one-time-token".into(),
        scopes: serde_json::json!(["orchestration:read"]),
        method: "bearer-access-token".into(),
        client: AuthSessionClient {
            label: None, ip_address: None, user_agent: None,
            device_type: "unknown".into(), os: None, browser: None,
        },
        issued_at: "2026-08-27T00:00:00.000Z".into(),
        expires_at: "2027-08-27T00:00:00.000Z".into(),
        reach: Some("this-computer".into()),
        off_host: Some(false),
    };
    repositories.create_auth_session(session).await.unwrap();
    let sessions = repositories
        .list_active_auth_sessions("2026-08-27T00:00:01.000Z".into())
        .await
        .unwrap();
    assert_eq!(sessions[0].reach.as_deref(), Some("this-computer"));
    assert_eq!(sessions[0].off_host, Some(false));
}
```

- [x] **Step 2: Run to verify it fails**

Run: `cargo test -p bibcode-server auth_pairing_reach_round_trips`
Expected: FAIL — `reach` field does not exist on `AuthPairingLink` / `NewAuthSession`.

- [x] **Step 3: Implement the migration and plumbing**

In `migrations.rs`, after `migration_045`:

```rust
fn migration_046(transaction: &Transaction<'_>) -> Result<()> {
    for table in ["auth_pairing_links", "auth_sessions"] {
        for (column, definition) in [("reach", "reach TEXT"), ("off_host", "off_host INTEGER")] {
            if !table_has_column(transaction, table, column)? {
                transaction
                    .execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {definition}"))?;
            }
        }
    }
    Ok(())
}
```

Register: `Migration::new(46, "AuthPairingReach", migration_046),` at the end of
`MIGRATIONS`.

The two columns travel together (amended spec §4.6): `reach` is the user-facing intent
literal; `off_host` is the **mint-time computed** flag the exposure derivation reads
(`custom` grants classify off-host or loopback at mint, never at read time). Both are
`NULL` on legacy rows.

In `repositories.rs`:

- Add `pub reach: Option<String>` and `pub off_host: Option<bool>` to
  `AuthPairingLink`, `NewAuthSession`, `AuthSession` (last fields of each).
- Append `reach, off_host` as the **last** columns of `PAIRING_SELECT`, the `RETURNING`
  list of `PAIRING_RETURNING_SQL`, and `AUTH_SESSION_SELECT`; extend both `INSERT`
  statements (`create_auth_pairing_link`, `create_auth_session`) with both columns and
  parameters (rusqlite maps `Option<bool>` to a nullable INTEGER).
- In `decode_pairing_link`, read the new trailing indexes (`row.get(11)?` /
  `row.get(12)?` — the current select has 11 columns, indices 0–10); in
  `decode_auth_session`, `row.get(14)?` / `row.get(15)?` (current 14 columns, indices
  0–13). Verify the decoders are positional before editing
  (`rg "fn decode_pairing_link" -A 20 apps/server/src/persistence/repositories.rs`).

- [x] **Step 4: Run to verify it passes**

Run: `cargo test -p bibcode-server auth_pairing_reach_round_trips`
Expected: PASS. Also run `cargo test -p bibcode-server --test persistence_compat` — the
fixture stores must still migrate cleanly through migration 46 (fixtures are historical
snapshots; adding an additive migration must not require fixture changes — if the
manifest's ledger assertions fail, follow the failure message, do not regenerate
fixtures by hand).

- [x] **Step 5: Commit**

```bash
git add apps/server/src/persistence/migrations.rs apps/server/src/persistence/repositories.rs
git commit -m "feat(server): persist pairing-grant reach and off-host flag on links and sessions"
```

---

### Task 2: Thread `reach` through AuthService and the auth contracts

**Files:**

- Modify: `apps/server/src/auth/service.rs` (`PairingRecord` ~line 98, `SessionRecord`
  ~line 83, `Grant` ~line 111, `issue_pairing_for_subject` ~line 542,
  `exchange_bootstrap` ~line 299, `persisted_pairing_link` ~line 991,
  `pairing_record_from_persisted` / `session_record_from_persisted`, `PairingRecord::view`
  ~line 978, `SessionRecord::view` ~line 961, plus every `issue_session` caller)
- Modify: `apps/server/src/auth/model.rs` (`PairingLinkView` ~line 115, `ClientSessionView` ~line 168)
- Modify: `packages/contracts/src/auth.ts` (`AuthPairingLink`, `AuthClientSession`)
- Modify: `packages/contracts/fixtures/auth-http/responses/pairing-list.json` (and the
  clients-list response fixture) via `packages/contracts/scripts/export-rust-auth-fixtures.ts`
- Test: service tests in `apps/server/src/auth/service.rs`; parity via
  `packages/contracts/src/authRustParity.test.ts`

**Interfaces:**

- Consumes: Task 1's `reach` + `off_host` persistence fields; Phase 3's
  `RemotePairingReach` TS literal (assumption noted above — verify/align, adding the
  named export to `packages/contracts/src/remotePairing.ts` only if missing).
- Produces:
  - `AuthService::issue_share_pairing(&self, scopes: Vec<String>, label: Option<String>, reach: String, off_host: bool) -> Result<PairingCredentialResult, AuthError>`
    — `off_host` is the mint-time classification of the offered endpoint (amended spec
    §4.6); the service stores it, it never recomputes it.
  - `reach: Option<String>` on `PairingLinkView` and `ClientSessionView` (serde
    `skip_serializing_if = "Option::is_none"`); `off_host` stays internal to the
    service/persistence (derivation input, not a view field)
  - TS: `reach: Schema.optionalKey(RemotePairingReach)` on `AuthPairingLink` and
    `AuthClientSession` (additive, decode-optional — older servers keep working)
  - Sessions minted from a reach-carrying pairing link inherit its `reach` **and**
    `off_host`.

- [x] **Step 1: Write the failing service test** (in the `service.rs` test module,
      mirroring `owned_scopes(STANDARD_SCOPES)` usage from existing tests):

```rust
#[tokio::test]
async fn share_pairing_reach_is_recorded_and_inherited_by_sessions() {
    let service = service(); // the module's existing constructor helper (service.rs ~line 1216)
    let issued = service
        .issue_share_pairing(
            owned_scopes(STANDARD_SCOPES),
            Some("Tablet".to_owned()),
            "another-device".to_owned(),
            true,
        )
        .await
        .expect("share pairing");
    let listed = service.list_pairings().await;
    assert_eq!(listed[0].reach.as_deref(), Some("another-device"));

    let session = service
        .exchange_bootstrap(&issued.credential, None, ClientMetadata::default(), None)
        .await
        .expect("session");
    let clients = service.list_clients(&session.principal.session_id).await;
    let paired = clients
        .iter()
        .find(|client| client.session_id == session.principal.session_id)
        .expect("paired client");
    assert_eq!(paired.reach.as_deref(), Some("another-device"));
}

#[tokio::test]
async fn share_pairing_rejects_unknown_reach() {
    let service = service();
    let error = service
        .issue_share_pairing(owned_scopes(STANDARD_SCOPES), None, "everywhere".to_owned(), true)
        .await
        .expect_err("invalid reach");
    assert!(matches!(error, AuthError::InvalidCredential));
}
```

(If a local binding named `service` shadowing the helper `service()` trips the borrow
of the helper name, bind as `let auth = service();` and adjust the calls — keep the
helper name exactly as the module defines it.)

- [x] **Step 2: Run to verify it fails**

Run: `cargo test -p bibcode-server share_pairing_reach`
Expected: FAIL — `issue_share_pairing` not defined.

- [x] **Step 3: Implement**

In `service.rs`:

- Add `reach: Option<String>` and `off_host: Option<bool>` to `PairingRecord`,
  `SessionRecord`, and `Grant`.
- Add a validation helper + constant near the top of the file:

```rust
pub(crate) const PAIRING_REACH_VALUES: [&str; 3] = ["another-device", "this-computer", "custom"];

fn is_valid_pairing_reach(value: &str) -> bool {
    PAIRING_REACH_VALUES.contains(&value)
}
```

- `issue_pairing_for_subject` gains two trailing parameters `reach: Option<String>,
  off_host: Option<bool>`; the four existing internal callers (`issue_pairing`,
  `issue_pairing_with_proof`, `issue_cloud_pairing`, `issue_startup_pairing`) pass
  `None, None`. Add:

```rust
pub async fn issue_share_pairing(
    &self,
    scopes: Vec<String>,
    label: Option<String>,
    reach: String,
    off_host: bool,
) -> Result<PairingCredentialResult, AuthError> {
    if !is_valid_pairing_reach(&reach) {
        return Err(AuthError::InvalidCredential);
    }
    self.issue_pairing_for_subject(
        scopes,
        label,
        "one-time-token",
        None,
        PAIRING_TTL_MS,
        Some(reach),
        Some(off_host),
    )
    .await
}
```

- `consume_grant` copies the consumed record's `reach` and `off_host` into `Grant`;
  `exchange_bootstrap` passes both into `issue_session`. `issue_session` gains trailing
  `reach: Option<String>, off_host: Option<bool>` parameters stored on `SessionRecord`
  and on `NewAuthSession` when persisting; all other `issue_session` callers (browser
  session, desktop bootstrap — find them with `rg "issue_session\(" apps/server/src`)
  pass `None, None`.
- `persisted_pairing_link` / `pairing_record_from_persisted` and the session equivalents
  copy `reach` and `off_host` both directions (hydration keeps both across restarts).
- `PairingRecord::view` / `SessionRecord::view` copy `reach` into the views (`off_host`
  is not a view field — it feeds only Task 3's derivation).

In `model.rs`: add to both views:

```rust
#[serde(skip_serializing_if = "Option::is_none")]
pub reach: Option<String>,
```

In `packages/contracts/src/auth.ts` (import `RemotePairingReach` from
`./remotePairing.ts`):

```ts
// AuthPairingLink gains:
reach: Schema.optionalKey(RemotePairingReach),
// AuthClientSession gains:
reach: Schema.optionalKey(RemotePairingReach),
```

Update the TS records derived from these in
`apps/web/src/environments/primary/auth.ts` (`listServerPairingLinks`,
`listServerClientSessions` field-by-field copies) to carry `reach` through, and the
`toDesktopPairingLinkRecord` / `toDesktopClientSessionRecord` mappers in
`apps/web/src/components/settings/ConnectionsSettings.tsx` if their record types are
explicit (`rg "ServerPairingLinkRecord" apps/web/src` to find the type definition and
add `reach?: RemotePairingReach`).

- [x] **Step 4: Regenerate auth fixtures and run parity + tests**

Run:

```bash
cargo test -p bibcode-server share_pairing_reach
node packages/contracts/scripts/export-rust-auth-fixtures.ts
vp test run packages/contracts/src/authRustParity.test.ts
cargo test -p bibcode-server --test auth_http
```

Expected: all PASS. If the export script embeds sample response bodies, extend the
pairing-list and clients-list samples with `"reach": "another-device"` so the Rust side
exercises the new field.

- [x] **Step 5: Commit**

```bash
git add apps/server/src/auth packages/contracts/src/auth.ts packages/contracts/src/remotePairing.ts \
  packages/contracts/fixtures/auth-http packages/contracts/scripts/export-rust-auth-fixtures.ts \
  apps/web/src/environments/primary/auth.ts apps/web/src/components/settings/ConnectionsSettings.tsx
git commit -m "feat(server,contracts): thread pairing reach through auth service and views"
```

---

### Task 3: Share-state derivation + `GET /api/auth/share-state`

**Files:**

- Modify: `apps/server/src/auth/service.rs` (new `share_exposure_state`), `apps/server/src/auth/model.rs` (new `ShareExposureState`), `apps/server/src/auth/http.rs` (route + handler)
- Modify: `packages/contracts/src/auth.ts`, `packages/contracts/src/environmentHttp.ts`, `packages/contracts/src/authRustParity.test.ts`, `packages/contracts/scripts/export-rust-auth-fixtures.ts`
- Create: `packages/contracts/fixtures/auth-http/responses/share-state.json`
- Modify: `packages/contracts/fixtures/auth-http/manifest.json`, `packages/contracts/fixtures/auth-http/scopes.json` (mirror the exact shape of existing entries)
- Modify: the `ROUTE_INVENTORY` definition (`rg "ROUTE_INVENTORY" apps/server/src`) — the fixture test asserts route parity
- Test: `apps/server/src/auth/service.rs` unit tests; `apps/server/tests/auth_http.rs` integration test

**Interfaces:**

- Consumes: Task 2's reach/off-host-carrying records. The derivation reads the
  **persisted mint-time `off_host` flag**, never the `reach` literal (amended spec
  §4.6: a `custom` grant pointing at a loopback endpoint must not widen).
- Produces:
  - Rust `AuthService::share_exposure_state(&self) -> ShareExposureState` with
    `ShareExposureState { desired_exposure: String /* "wide" | "loopback" */, off_host_grant_count: usize, legacy_grant_count: usize }`
  - TS `AuthShareStateResult = Schema.Struct({ desiredExposure: Schema.Literals(["wide", "loopback"]), offHostGrantCount: Schema.Number, legacyGrantCount: Schema.Number })`
  - `GET /api/auth/share-state`, scope `access:read`.

- [x] **Step 1: Write the failing derivation tests** (service.rs test module):

```rust
#[tokio::test]
async fn share_exposure_derives_wide_only_from_off_host_flags() {
    let auth = service();
    // No grants → loopback.
    let state = auth.share_exposure_state().await;
    assert_eq!(state.desired_exposure, "loopback");

    // A this-computer grant never widens (spec §4.6).
    auth.issue_share_pairing(
        owned_scopes(STANDARD_SCOPES),
        None,
        "this-computer".to_owned(),
        false,
    )
    .await
    .unwrap();
    assert_eq!(auth.share_exposure_state().await.desired_exposure, "loopback");

    // A custom grant whose endpoint classified LOOPBACK at mint (an SSH tunnel)
    // never widens either (amended spec §4.6).
    auth.issue_share_pairing(owned_scopes(STANDARD_SCOPES), None, "custom".to_owned(), false)
        .await
        .unwrap();
    assert_eq!(auth.share_exposure_state().await.desired_exposure, "loopback");

    // One grant whose off-host flag was computed true at mint → wide.
    let off_host = auth
        .issue_share_pairing(
            owned_scopes(STANDARD_SCOPES),
            None,
            "another-device".to_owned(),
            true,
        )
        .await
        .unwrap();
    let state = auth.share_exposure_state().await;
    assert_eq!(state.desired_exposure, "wide");
    assert_eq!(state.off_host_grant_count, 1);

    // Consuming keeps it wide (grant lives on as the session)…
    let session = auth
        .exchange_bootstrap(&off_host.credential, None, ClientMetadata::default(), None)
        .await
        .unwrap();
    assert_eq!(auth.share_exposure_state().await.desired_exposure, "wide");

    // …and revoking the session reverts to loopback (spec §4.6).
    auth.revoke_client("administrator", &session.principal.session_id)
        .await
        .unwrap();
    assert_eq!(auth.share_exposure_state().await.desired_exposure, "loopback");
}

#[tokio::test]
async fn legacy_null_reach_grants_count_separately_and_never_widen() {
    let auth = service();
    // Legacy pairing (pre-reach): issue through the reach-less path.
    let legacy = auth
        .issue_pairing(owned_scopes(STANDARD_SCOPES), None)
        .await
        .unwrap();
    auth.exchange_bootstrap(&legacy.credential, None, ClientMetadata::default(), None)
        .await
        .unwrap();
    let state = auth.share_exposure_state().await;
    assert_eq!(state.desired_exposure, "loopback"); // never auto-widens
    assert_eq!(state.legacy_grant_count, 1);        // but blocks auto-revert (decision 4)
}
```

- [x] **Step 2: Run to verify failure**

Run: `cargo test -p bibcode-server share_exposure`
Expected: FAIL — `share_exposure_state` not defined.

- [x] **Step 3: Implement**

`model.rs`:

```rust
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareExposureState {
    pub desired_exposure: String,
    pub off_host_grant_count: usize,
    pub legacy_grant_count: usize,
}
```

`service.rs`:

```rust
pub async fn share_exposure_state(&self) -> ShareExposureState {
    let now = now_ms();
    let state = self.state.lock().await;
    let link_grants = state
        .pairings
        .values()
        .filter(|pairing| {
            pairing.subject == "one-time-token"
                && pairing.consumed_at_ms.is_none()
                && pairing.revoked_at_ms.is_none()
                && pairing.expires_at_ms > now
        })
        .map(|pairing| pairing.off_host);
    let session_grants = state
        .sessions
        .values()
        .filter(|session| {
            session.subject == "one-time-token"
                && session.revoked_at_ms.is_none()
                && session.expires_at_ms > now
                && matches!(
                    session.method.as_str(),
                    "bearer-access-token" | "dpop-access-token"
                )
        })
        .map(|session| session.off_host);
    let mut off_host_grant_count = 0usize;
    let mut legacy_grant_count = 0usize;
    for off_host in link_grants.chain(session_grants) {
        match off_host {
            // The flag was computed at mint time from the offered endpoint
            // (amended spec §4.6); derivation trusts it and never reclassifies.
            Some(true) => off_host_grant_count += 1,
            Some(false) => {}
            None => legacy_grant_count += 1,
        }
    }
    ShareExposureState {
        desired_exposure: if off_host_grant_count > 0 { "wide" } else { "loopback" }.to_owned(),
        off_host_grant_count,
        legacy_grant_count,
    }
}
```

`http.rs` — route (in `add_routes`, beside the pairing-links route) and handler
(mirroring `pairing_links`):

```rust
.route("/api/auth/share-state", get(share_state))
```

```rust
async fn share_state(State(state): State<AppState>, headers: HeaderMap, uri: Uri) -> Response {
    match authenticated_with_scope(&state.auth, &headers, &uri, SCOPE_ACCESS_READ).await {
        Ok(_) => Json(state.auth.share_exposure_state().await).into_response(),
        Err(error) => auth_error_for_request(error, &headers, "share_state_load_failed"),
    }
}
```

Add `"share_state_load_failed"` wherever the internal-reason literals are inventoried
(check `rg "pairing_links_load_failed" apps/server/src packages/contracts/src` — the
reason strings appear in `packages/contracts/src/environmentHttp.ts` ~line 77; add the
new literal there too). Add the route to `ROUTE_INVENTORY`.

TS `environmentHttp.ts` (auth group):

```ts
.add(
  HttpApiEndpoint.get("shareState", "/api/auth/share-state", {
    headers: OptionalBearerHeaders,
    success: AuthShareStateResult,
    error: EnvironmentScopedOperationErrors,
  }).middleware(EnvironmentAuthenticatedAuth),
)
```

TS `auth.ts`:

```ts
export const AuthShareStateResult = Schema.Struct({
  desiredExposure: Schema.Literals(["wide", "loopback"]),
  offHostGrantCount: Schema.Number,
  legacyGrantCount: Schema.Number,
});
export type AuthShareStateResult = typeof AuthShareStateResult.Type;
```

`authRustParity.test.ts`: add to `authRouteContract`:

```ts
{
  name: "shareState",
  method: "GET",
  path: "/api/auth/share-state",
  requestContentTypes: [],
  successStatuses: [200],
  errorStatuses: [403, 500],
},
```

(match the exact `errorStatuses` the reflection derives — run the test and align).
Fixture: `responses/share-state.json`:

```json
{ "desiredExposure": "loopback", "offHostGrantCount": 0, "legacyGrantCount": 1 }
```

Register it in the fixture manifest lists in `authRustParity.test.ts`, the export
script, `fixtures/auth-http/manifest.json` routes+fixtures arrays, and map the scope in
`fixtures/auth-http/scopes.json` following the file's existing structure.

Integration test in `apps/server/tests/auth_http.rs` (uses the file's existing helpers):

```rust
#[tokio::test]
async fn share_state_reports_grant_derived_exposure() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_desktop_server(&temp).await;
    let client = Client::new();
    let token_response = exchange_token(&client, &handle, DESKTOP_BOOTSTRAP, None).await;
    let token = access_token(&token_response);

    let initial = get_json(
        client
            .get(http_url(&handle, "/api/auth/share-state"))
            .bearer_auth(token)
            .send()
            .await
            .expect("share state"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(initial["desiredExposure"], "loopback");

    let offer = get_json(
        client
            .post(http_url(&handle, "/api/auth/pairing-token"))
            .bearer_auth(token)
            .json(&json!({ "label": "Tablet" }))
            .send()
            .await
            .expect("pairing token"),
        StatusCode::OK,
    )
    .await;
    // Reach-less legacy link → still loopback, one legacy grant.
    let after_legacy = get_json(
        client
            .get(http_url(&handle, "/api/auth/share-state"))
            .bearer_auth(token)
            .send()
            .await
            .expect("share state"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(after_legacy["desiredExposure"], "loopback");
    assert_eq!(after_legacy["legacyGrantCount"], 1);
    let _ = offer;
}
```

(Off-host wide assertion lands in Task 4's integration test once the Phase 3-landed
offer endpoint persists `reach`. Note: until Task 4, offers minted through that
endpoint decode as `NULL`-reach and therefore count as **legacy** grants here — the
derivation stays loopback, which is the safe direction.)

- [x] **Step 4: Run to verify pass**

```bash
cargo test -p bibcode-server share_exposure
cargo test -p bibcode-server --test auth_http
vp test run packages/contracts/src/authRustParity.test.ts
```

Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add apps/server/src/auth packages/contracts/src packages/contracts/fixtures/auth-http \
  packages/contracts/scripts/export-rust-auth-fixtures.ts
git commit -m "feat(server,contracts): derive desired exposure from unrevoked off-host grants"
```

---

### Task 4: Persist `reach` + mint-time `off_host` on grants minted by the pairing-offer endpoint

**Files:**

- Modify: `apps/server/src/auth/http.rs` (`create_pairing_offer` handler — **landed by
  Phase 3**; this task changes only its issuance call)
- Test: `apps/server/tests/auth_http.rs`

**Interfaces:**

- Consumes:
  - Phase 3's landed pairing-offer surface — verify presence before starting
    (`rg "pairing-offer|pairingOffer" apps/server/src packages/contracts/src`):
    `POST /api/auth/pairing-offer` (scope `access:write`), handler
    `create_pairing_offer`, TS `AuthCreatePairingOfferInput` /
    `AuthPairingOfferResult` (input carries `reach`; the encoded §4.2 payload embeds
    it), fixtures `requests/pairing-offer.json` / `responses/pairing-offer.json`, the
    `ROUTE_INVENTORY` entry, and Phase 3's endpoint/reach validation rules. **None of
    these are created, renamed, or re-fixtured here.**
  - Task 2's `AuthService::issue_share_pairing(scopes, label, reach, off_host)` and
    `PAIRING_REACH_VALUES`; `is_loopback_host` from the auth module (raise visibility
    to `pub(crate)` if needed).
- Produces: pairing links minted by the offer endpoint (and the sessions they become
  via Task 2's inheritance) persist the offer's `reach` **and** its mint-time computed
  `off_host` flag (amended spec §4.6), so Task 3's `share_exposure_state` counts them.
  This is the seam the §4.6 exposure state machine hangs on; Tasks 9–11 rely on it end
  to end.

Context: Phase 3's handler embeds `reach` in the encoded payload but mints through the
reach-less issuance path, so until this task its grant rows decode as `reach = NULL`
(legacy under decision 4 — they never widen and they block auto-revert). The delta
here is (a) computing the grant's off-host flag from the validated intent + endpoint
(`another-device` ⇒ true; `this-computer` ⇒ false; `custom` ⇒ whether the offered
endpoint's host is non-loopback) and (b) switching the handler's issuance call to
`issue_share_pairing` so the grant row records both.

- [x] **Step 1: Write the failing integration test** (`apps/server/tests/auth_http.rs`,
      using the file's existing helpers):

```rust
#[tokio::test]
async fn pairing_offer_persists_reach_onto_the_minted_grant() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_desktop_server(&temp).await;
    let client = Client::new();
    let token_response = exchange_token(&client, &handle, DESKTOP_BOOTSTRAP, None).await;
    let token = access_token(&token_response);

    // Mint an off-host offer through the Phase 3-landed endpoint.
    let offer = get_json(
        client
            .post(http_url(&handle, "/api/auth/pairing-offer"))
            .bearer_auth(token)
            .json(&json!({
                "name": "AI-SERVER",
                "endpoint": "http://192.168.1.20:3773",
                "reach": "another-device",
            }))
            .send()
            .await
            .expect("pairing offer"),
        StatusCode::OK,
    )
    .await;
    let offer_id = offer["id"].as_str().expect("offer id").to_owned();

    // The grant row itself records the reach…
    let links = get_json(
        client
            .get(http_url(&handle, "/api/auth/pairing-links"))
            .bearer_auth(token)
            .send()
            .await
            .expect("pairing links"),
        StatusCode::OK,
    )
    .await;
    let minted = links
        .as_array()
        .expect("links array")
        .iter()
        .find(|link| link["id"] == offer_id.as_str())
        .expect("minted link");
    assert_eq!(minted["reach"], "another-device");

    // …so exposure derivation counts it as an off-host grant (spec §4.6).
    let state = get_json(
        client
            .get(http_url(&handle, "/api/auth/share-state"))
            .bearer_auth(token)
            .send()
            .await
            .expect("share state"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(state["desiredExposure"], "wide");
    assert_eq!(state["offHostGrantCount"], 1);
    assert_eq!(state["legacyGrantCount"], 0);
}

#[tokio::test]
async fn loopback_custom_offers_never_flip_desired_exposure_wide() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_desktop_server(&temp).await;
    let client = Client::new();
    let token_response = exchange_token(&client, &handle, DESKTOP_BOOTSTRAP, None).await;
    let token = access_token(&token_response);

    // A custom offer at a loopback endpoint (an SSH tunnel) classifies off-host=false
    // at mint (amended spec §4.6)…
    let offer = get_json(
        client
            .post(http_url(&handle, "/api/auth/pairing-offer"))
            .bearer_auth(token)
            .json(&json!({
                "name": "AI-SERVER",
                "endpoint": "http://127.0.0.1:9022",
                "reach": "custom",
            }))
            .send()
            .await
            .expect("custom loopback offer"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(offer["reach"], "custom");

    // …so it must not widen desired exposure, and it is not legacy either.
    let state = get_json(
        client
            .get(http_url(&handle, "/api/auth/share-state"))
            .bearer_auth(token)
            .send()
            .await
            .expect("share state"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(state["desiredExposure"], "loopback");
    assert_eq!(state["offHostGrantCount"], 0);
    assert_eq!(state["legacyGrantCount"], 0);
}
```

- [x] **Step 2: Run to verify failure**

Run: `cargo test -p bibcode-server --test auth_http pairing_offer_persists_reach`
Expected: FAIL — the minted link has no `reach` field (Phase 3 mints reach-less), so
`minted["reach"]` is `Value::Null` and share-state reports `"loopback"` with
`legacyGrantCount == 1`.

- [x] **Step 3: Minimal implementation**

In `create_pairing_offer` (`apps/server/src/auth/http.rs`), after Phase 3's existing
endpoint/reach validation (which already parses the endpoint URL — reuse its parsed
value rather than parsing twice), compute the mint-time flag and replace the
reach-less issuance call Phase 3 used (`issue_pairing(...)` or equivalent — read the
landed handler first):

```rust
let endpoint_is_loopback = endpoint
    .host_str()
    .is_some_and(|host| is_loopback_host(host));
let off_host = match payload.reach.as_str() {
    "another-device" => true,
    "this-computer" => false,
    // "custom": classified from the offered endpoint AT MINT TIME (amended spec
    // §4.6) — a loopback custom offer (SSH tunnel / reverse proxy) must not widen.
    _ => !endpoint_is_loopback,
};
let issued = match state
    .auth
    .issue_share_pairing(scopes, non_empty(payload.label), payload.reach.clone(), off_host)
    .await
{
    Ok(issued) => issued,
    Err(error) => {
        return auth_error_for_request(error, &headers, "pairing_offer_issuance_failed")
    }
};
```

(keep the handler's existing error-arm shape and internal-reason string — adopt
whatever literal Phase 3 registered rather than introducing a second one). Leave every
other part of the handler — validation, payload composition, encode, response,
post-issuance failure cleanup — untouched. If Phase 3's handler validates `reach`
against its own literal list, unify it on Task 2's `PAIRING_REACH_VALUES` constant
(single source of truth) without changing the accepted values.

- [x] **Step 4: Run to verify pass**

```bash
cargo test -p bibcode-server --test auth_http
vp test run packages/contracts/src/authRustParity.test.ts
```

Expected: PASS. No fixture changes: the response shape already carries `reach`
(Phase 3), and the pairing-links response fixture already gained `reach` in Task 2.

- [x] **Step 5: Commit**

```bash
git add apps/server/src/auth/http.rs apps/server/tests/auth_http.rs
git commit -m "feat(server): persist reach and mint-time off-host flag on pairing offers"
```

---

### Task 4b: Actively terminate live WebSocket sessions on revocation

Amended spec §3 (Revocation): today `revoke_client` / `revoke_other_clients` only
remove the session — per-request reauthorization rejects the _next_ call
(`apps/server/src/rpc/session.rs`), but already-open WebSocket streams keep flowing.
This task adds active termination so a revoked device loses its streams immediately.

**Files:**

- Modify: `apps/server/src/auth/service.rs` (connection registry on the state struct
  behind `self.state`; `mark_connected` ~line 752 / `mark_disconnected` ~line 784;
  `revoke_client` ~line 679; `revoke_other_clients` ~line 716)
- Modify: `apps/server/src/http.rs` (`websocket` handler ~lines 199–254 — the
  per-connection `session_shutdown` CancellationToken already exists and already
  terminates `run_session` when cancelled; register it with the auth service)
- Test: `apps/server/tests/auth_http.rs`

**Interfaces:**

- Consumes: the existing per-connection `session_shutdown` token
  (`state.shutdown.child_token()`, `http.rs:205`) and the existing
  `mark_connected`/`mark_disconnected` bookkeeping.
- Produces:
  - `AuthService::mark_connected(&self, session_id: &str, shutdown: CancellationToken) -> u64`
    (returns a connection id) and
    `AuthService::mark_disconnected(&self, session_id: &str, connection_id: u64)` —
    signature change; `http.rs` is the only caller
    (`rg "mark_connected|mark_disconnected" apps/server/src`).
  - Revoking a session (single or revoke-others) cancels every registered token for
    the revoked session ids, closing its live WebSockets server-side.

- [x] **Step 1: Write the failing integration test** (`apps/server/tests/auth_http.rs`,
      same helpers as the existing WebSocket tests):

```rust
#[tokio::test]
async fn revoking_a_client_terminates_its_live_websocket() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_desktop_server(&temp).await;
    let client = Client::new();
    let administrator = exchange_token(&client, &handle, DESKTOP_BOOTSTRAP, None).await;
    let administrator_token = access_token(&administrator);

    // Pair a second client and open a WebSocket as it.
    let pairing = get_json(
        client
            .post(http_url(&handle, "/api/auth/pairing-token"))
            .bearer_auth(administrator_token)
            .json(&json!({ "label": "Tablet" }))
            .send()
            .await
            .expect("pairing"),
        StatusCode::OK,
    )
    .await;
    let paired = exchange_token(
        &client,
        &handle,
        pairing["credential"].as_str().expect("credential"),
        None,
    )
    .await;
    let paired_token = access_token(&paired);
    let paired_ticket = websocket_ticket(&client, &handle, paired_token).await;
    let (mut paired_socket, _) = connect_async(format!(
        "ws://{}/ws?wsTicket={paired_ticket}",
        handle.local_addr()
    ))
    .await
    .expect("paired WebSocket");

    // Identify the paired session from the administrator's clients list.
    let clients_list = get_json(
        client
            .get(http_url(&handle, "/api/auth/clients"))
            .bearer_auth(administrator_token)
            .send()
            .await
            .expect("clients"),
        StatusCode::OK,
    )
    .await;
    let target = clients_list
        .as_array()
        .expect("clients array")
        .iter()
        .find(|session| session["current"] == false && session["connected"] == true)
        .expect("paired connected session")["sessionId"]
        .as_str()
        .expect("session id")
        .to_owned();

    // Revoke it; the open socket must drop WITHOUT the revoked client sending
    // another request (amended spec §3 — streams stop immediately).
    let revoke = client
        .post(http_url(&handle, "/api/auth/clients/revoke"))
        .bearer_auth(administrator_token)
        .json(&json!({ "sessionId": target }))
        .send()
        .await
        .expect("revoke");
    assert_eq!(revoke.status(), StatusCode::OK);
    let closed = timeout(Duration::from_secs(5), async {
        loop {
            match paired_socket.next().await {
                None => break true,
                Some(Ok(tungstenite::Message::Close(_))) => break true,
                Some(Ok(_)) => continue, // drain any in-flight frames
                Some(Err(_)) => break true,
            }
        }
    })
    .await
    .expect("revoked socket must close within 5s");
    assert!(closed);
    shutdown(handle).await;
}
```

(Adapt the pairing-credential exchange to the harness's exact idiom — the existing
`one_time_pairing_credentials_are_atomic_and_scope_constrained` test at ~line 187
shows how a pairing credential goes through `/oauth/token`.)

- [x] **Step 2: Run to verify failure**

Run: `cargo test -p bibcode-server --test auth_http revoking_a_client_terminates`
Expected: FAIL — the socket stays open and the 5s timeout elapses (today nothing
cancels the connection's shutdown token on revocation).

- [x] **Step 3: Implement**

`service.rs` — on the state struct behind `self.state` (the one holding `pairings` /
`sessions`), add:

```rust
live_connections: HashMap<String, HashMap<u64, CancellationToken>>,
next_connection_id: u64,
```

(import `tokio_util::sync::CancellationToken` — the same type `http.rs` uses; add the
`tokio-util` dependency reference only if `apps/server/Cargo.toml` does not already
carry it, which it does for the shutdown token). Change the bookkeeping methods,
keeping every existing `connected_count` / `last_connected_at` behavior:

```rust
pub async fn mark_connected(&self, session_id: &str, shutdown: CancellationToken) -> u64 {
    let mut state = self.state.lock().await;
    state.next_connection_id = state.next_connection_id.wrapping_add(1);
    let connection_id = state.next_connection_id;
    state
        .live_connections
        .entry(session_id.to_owned())
        .or_default()
        .insert(connection_id, shutdown);
    // …existing connected_count / last_connected_at logic unchanged…
    connection_id
}

pub async fn mark_disconnected(&self, session_id: &str, connection_id: u64) {
    let mut state = self.state.lock().await;
    if let Some(connections) = state.live_connections.get_mut(session_id) {
        connections.remove(&connection_id);
        if connections.is_empty() {
            state.live_connections.remove(session_id);
        }
    }
    // …existing connected_count logic unchanged…
}
```

Add a private helper and call it from **both** the repository-backed and in-memory
branches of `revoke_client` (for the target id, after a successful revoke) and
`revoke_other_clients` (for every removed id):

```rust
fn cancel_live_connections(state: &mut AuthState, session_id: &str) {
    if let Some(connections) = state.live_connections.remove(session_id) {
        for (_, shutdown) in connections {
            shutdown.cancel();
        }
    }
}
```

(`AuthState` = the actual name of the struct behind `self.state`; read it first.)

`http.rs` `websocket` handler: register the existing token and thread the id through:

```rust
let connection_id = auth.mark_connected(&session_id, session_shutdown.clone()).await;
// …unchanged… and in the on_upgrade cleanup tail:
auth.mark_disconnected(&session_id, connection_id).await;
```

Cancelling `session_shutdown` is exactly what the existing expiration guard does
(`http.rs:236–241`), so `run_session` already treats it as a terminate signal — no RPC
session changes needed.

- [x] **Step 4: Run to verify pass**

```bash
cargo test -p bibcode-server --test auth_http
cargo test -p bibcode-server auth
```

Expected: PASS, including the existing connected-count and revocation tests.

- [x] **Step 5: Commit**

```bash
git add apps/server/src/auth/service.rs apps/server/src/http.rs apps/server/tests/auth_http.rs
git commit -m "feat(server): close live WebSocket sessions when a client is revoked"
```

---

### Task 5: Share-class mapping over Phase 3's `classifyPairingEndpoint`

**Files:**

- Create: `apps/web/src/components/settings/remote-servers/endpointClass.ts`
- Test: `apps/web/src/components/settings/remote-servers/endpointClass.test.ts`

**Interfaces:**

- Consumes: **Phase 3 owns** `classifyPairingEndpoint` in
  `packages/shared/src/advertisedEndpoint.ts` with signature
  `(endpoint: string) => "loopback" | "private-network" | "public" | "unconnectable"`.
  Do NOT redefine or extend that export — verify it first
  (`rg "classifyPairingEndpoint" packages/shared`) and treat a signature mismatch as a
  cross-phase blocker to raise, not to patch around.
- Produces (Task 9 relies on these exact names):

```ts
export type ShareEndpointClass = "loopback" | "off-host" | "unconnectable";
export function shareClassForPairingEndpoint(endpoint: string): ShareEndpointClass;
```

Mapping (pinned): `"loopback"` → `"loopback"`; `"private-network"` and `"public"` →
`"off-host"`; `"unconnectable"` → `"unconnectable"` (Task 9 turns it into an
`invalid-address` failure). The share flow never needs the private/public
distinction — only whether a grant reaches beyond this host.

- [x] **Step 1: Failing test**

```ts
import { describe, expect, it } from "@effect/vitest";

import { shareClassForPairingEndpoint } from "./endpointClass.ts";

describe("shareClassForPairingEndpoint", () => {
  it("maps loopback endpoints to loopback", () => {
    expect(shareClassForPairingEndpoint("http://127.0.0.1:3773")).toBe("loopback");
    expect(shareClassForPairingEndpoint("http://localhost:3773")).toBe("loopback");
  });

  it("maps private-network and public endpoints to off-host", () => {
    expect(shareClassForPairingEndpoint("http://192.168.1.20:3773")).toBe("off-host");
    expect(shareClassForPairingEndpoint("https://machine.tailnet.ts.net")).toBe("off-host");
    expect(shareClassForPairingEndpoint("https://example.com")).toBe("off-host");
  });

  it("passes unconnectable through for the invalid-address path", () => {
    expect(shareClassForPairingEndpoint("http://")).toBe("unconnectable");
  });
});
```

(If Phase 3's classifier judges any of these inputs differently — e.g. it may
classify a bare hostname as `"unconnectable"` until probed — align the test inputs
with its documented behavior rather than changing the mapping.)

- [x] **Step 2: Run to verify failure**

Run: `vp test run apps/web/src/components/settings/remote-servers/endpointClass.test.ts`
Expected: FAIL — module missing.

- [x] **Step 3: Implement** (`endpointClass.ts`; check the exact subpath export for
      the shared module in `packages/shared/package.json`):

```ts
import { classifyPairingEndpoint } from "@bibcode/shared/advertisedEndpoint";

export type ShareEndpointClass = "loopback" | "off-host" | "unconnectable";

export function shareClassForPairingEndpoint(endpoint: string): ShareEndpointClass {
  switch (classifyPairingEndpoint(endpoint)) {
    case "loopback":
      return "loopback";
    case "private-network":
    case "public":
      return "off-host";
    case "unconnectable":
      return "unconnectable";
  }
}
```

- [x] **Step 4: Run to verify pass**

Run: `vp test run apps/web/src/components/settings/remote-servers/endpointClass.test.ts` — PASS.

- [x] **Step 5: Commit**

```bash
git add apps/web/src/components/settings/remote-servers/endpointClass.ts \
  apps/web/src/components/settings/remote-servers/endpointClass.test.ts
git commit -m "feat(web): map pairing-endpoint classification to share classes"
```

---

### Task 6: Windows firewall rule management (desktop)

**Files:**

- Create: `apps/desktop/src-tauri/src/firewall.rs`
- Modify: `apps/desktop/src-tauri/src/lib.rs` (add `mod firewall;` beside the existing module declarations)

**Interfaces:**

- Produces:
  - `firewall::remote_access_rule_add_args(program: &str) -> Vec<String>`
  - `firewall::remote_access_rule_delete_args() -> Vec<String>`
  - `pub async fn sync_remote_access_rule(enabled: bool) -> Result<(), String>` — no-op
    `Ok(())` on non-Windows; on Windows runs `netsh` delete (always, ignore
    not-found) then add when `enabled`.
- Rule identity: `BiBCode Remote Access` — program-scoped (decision 5), so the
  dynamically picked backend port never strands the rule.

- [x] **Step 1: Failing construction tests** (in `firewall.rs`, run on all platforms):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_rule_arguments_are_program_scoped() {
        let args = remote_access_rule_add_args(r"C:\Apps\BiBCode\bibcode-desktop.exe");
        assert_eq!(
            args,
            vec![
                "advfirewall".to_string(),
                "firewall".to_string(),
                "add".to_string(),
                "rule".to_string(),
                "name=BiBCode Remote Access".to_string(),
                "dir=in".to_string(),
                "action=allow".to_string(),
                r"program=C:\Apps\BiBCode\bibcode-desktop.exe".to_string(),
                "protocol=TCP".to_string(),
                "profile=domain,private".to_string(),
                "enable=yes".to_string(),
            ]
        );
    }

    #[test]
    fn delete_rule_arguments_target_the_rule_by_name() {
        assert_eq!(
            remote_access_rule_delete_args(),
            vec![
                "advfirewall".to_string(),
                "firewall".to_string(),
                "delete".to_string(),
                "rule".to_string(),
                "name=BiBCode Remote Access".to_string(),
            ]
        );
    }
}
```

- [x] **Step 2: Run to verify failure**

Run: `cargo test -p bibcode-desktop --lib firewall`
Expected: FAIL — module missing.

- [x] **Step 3: Implement**

```rust
//! Windows Defender Firewall integration for grant-driven server exposure.
//!
//! The desktop backend port is picked dynamically, so the inbound allow rule is
//! program-scoped rather than port-scoped: it follows the executable across
//! launches and never goes stale on a port change. Non-Windows platforms have
//! no managed firewall here and every call is a successful no-op.

const REMOTE_ACCESS_RULE_NAME: &str = "BiBCode Remote Access";

#[must_use]
pub(crate) fn remote_access_rule_add_args(program: &str) -> Vec<String> {
    vec![
        "advfirewall".to_owned(),
        "firewall".to_owned(),
        "add".to_owned(),
        "rule".to_owned(),
        format!("name={REMOTE_ACCESS_RULE_NAME}"),
        "dir=in".to_owned(),
        "action=allow".to_owned(),
        format!("program={program}"),
        "protocol=TCP".to_owned(),
        "profile=domain,private".to_owned(),
        "enable=yes".to_owned(),
    ]
}

#[must_use]
pub(crate) fn remote_access_rule_delete_args() -> Vec<String> {
    vec![
        "advfirewall".to_owned(),
        "firewall".to_owned(),
        "delete".to_owned(),
        "rule".to_owned(),
        format!("name={REMOTE_ACCESS_RULE_NAME}"),
    ]
}

#[cfg(windows)]
pub(crate) async fn sync_remote_access_rule(enabled: bool) -> Result<(), String> {
    // Delete is idempotent cleanup: netsh exits non-zero when no rule matches,
    // which is not an error for us.
    let _ = run_netsh(remote_access_rule_delete_args()).await;
    if !enabled {
        return Ok(());
    }
    let program = std::env::current_exe()
        .map_err(|error| format!("failed to resolve desktop executable: {error}"))?
        .to_string_lossy()
        .into_owned();
    let output = run_netsh(remote_access_rule_add_args(&program)).await?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "netsh failed to add the remote access firewall rule: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

#[cfg(windows)]
async fn run_netsh(args: Vec<String>) -> Result<std::process::Output, String> {
    let mut command = tokio::process::Command::new("netsh");
    command.args(args);
    bibcode_server::process::configure_background_command(&mut command);
    command
        .output()
        .await
        .map_err(|error| format!("failed to run netsh: {error}"))
}

#[cfg(not(windows))]
pub(crate) async fn sync_remote_access_rule(_enabled: bool) -> Result<(), String> {
    Ok(())
}
```

(Verify `configure_background_command` accepts `tokio::process::Command` —
`backend.rs` line 2 imports both variants; use the matching one.)

- [x] **Step 4: Run to verify pass**

Run: `cargo test -p bibcode-desktop --lib firewall` — PASS.
Note: `sync_remote_access_rule`'s Windows execution path cannot be exercised on this
Linux workspace; it is validated on native Windows per Task 12's runbook update
(evidence: rule present in `netsh advfirewall firewall show rule name="BiBCode Remote
Access"` after widening, absent after revert).

- [x] **Step 5: Commit**

```bash
git add apps/desktop/src-tauri/src/firewall.rs apps/desktop/src-tauri/src/lib.rs
git commit -m "feat(desktop): manage a program-scoped Windows firewall rule for remote access"
```

---

### Task 7: `applyServerExposure` bridge command with verification + rollback

**Files:**

- Modify: `apps/desktop/src-tauri/src/bridge.rs` (replace
  `desktop_bridge_set_server_exposure_mode` ~line 1189 with
  `desktop_bridge_apply_server_exposure`; update the command registration test ~line 2837
  and the invoke test ~line 2969)
- Modify: `apps/desktop/src-tauri/src/lib.rs` (command registration, both lists ~lines 31/150)
- Modify: `packages/contracts/src/ipc.ts` (`DesktopBridge` ~line 1235)
- Modify: `apps/web/src/tauriDesktopBridge.ts` (+ `apps/web/src/tauriDesktopBridge.test.ts`)

**Interfaces:**

- Consumes: `BackendSupervisor::restart_default_if_active`, `read_desktop_settings`,
  `update_desktop_settings`, `server_exposure_state`, Task 6
  `firewall::sync_remote_access_rule`.
- Produces:
  - Rust command `desktop_bridge_apply_server_exposure(desired: String) -> Result<Value, String>`
    returning the same `DesktopServerExposureState` JSON as
    `desktop_bridge_get_server_exposure_state`.
  - TS `DesktopBridge.applyServerExposure(desired: DesktopServerExposureMode): Promise<DesktopServerExposureState>`.
  - `setServerExposureMode` is **removed** from the bridge contract (its only consumer
    is the manual toggle this phase deletes; no compatibility alias).

Rollback semantics (pinned, spec §4.6 "rollback to loopback on bind failure"):

1. Snapshot the prior persisted mode.
2. Persist the desired mode; restart the default backend set.
3. Verify: for `network-accessible`, the achieved run config must report
   `server_exposure_mode == "network-accessible"` (this catches both bind failures and
   `resolve_backend_exposure` falling back closed when no LAN address is resolvable).
4. Sync the firewall rule to the achieved mode.
5. On restart error, verification failure, **or firewall-sync failure while widening**:
   restore the prior mode, restart again, remove the firewall rule, and return `Err`
   so the offer generation fails visibly. A wide listener whose firewall rule could
   not be installed must never be left standing behind an error return.
6. Firewall-sync failure while **narrowing** commits with a logged warning — the
   narrow succeeded and a stale program-scoped allow rule is inert against a loopback
   bind.

- [x] **Step 1: Failing bridge test** — extend the existing invoke-based test module in
      `bridge.rs` (mirror the harness used by the current
      `desktop_bridge_set_server_exposure_mode` test at ~line 2969):

```rust
#[tokio::test]
async fn apply_server_exposure_rejects_unknown_modes_and_reports_state() {
    let (app, _backend) = test_bridge_app(); // reuse the module's existing harness helper
    let error = invoke(
        "desktop_bridge_apply_server_exposure",
        json!({ "desired": "public-internet" }),
    )
    .await
    .expect_err("unsupported mode");
    assert!(error.contains("Unsupported server exposure mode"));

    // local-only → local-only is a no-op that still returns current state.
    let state = invoke(
        "desktop_bridge_apply_server_exposure",
        json!({ "desired": "local-only" }),
    )
    .await
    .expect("state");
    assert_eq!(state["mode"], "local-only");
}
```

Also add a unit test for the pure outcome helper introduced below — it is the
authority on when rollback happens, including the firewall-failure-after-widen case:

```rust
#[test]
fn exposure_outcomes_cover_bind_and_firewall_failures() {
    use ExposureApplyOutcome::{Commit, CommitWithFirewallWarning, Rollback};
    assert_eq!(exposure_apply_outcome("local-only", true, None, true), Commit);
    assert_eq!(
        exposure_apply_outcome("network-accessible", true, Some("network-accessible"), true),
        Commit
    );
    assert_eq!(
        exposure_apply_outcome("network-accessible", true, Some("local-only"), true),
        Rollback
    );
    assert_eq!(exposure_apply_outcome("network-accessible", true, None, true), Rollback);
    assert_eq!(exposure_apply_outcome("network-accessible", false, None, true), Rollback);
    // A wide listener without its firewall rule must not be left standing behind an
    // error return — firewall failure after a successful widen rolls back too.
    assert_eq!(
        exposure_apply_outcome("network-accessible", true, Some("network-accessible"), false),
        Rollback
    );
    // Narrowing succeeded; a stale allow rule is inert against a loopback bind.
    assert_eq!(
        exposure_apply_outcome("local-only", true, None, false),
        CommitWithFirewallWarning
    );
}
```

- [x] **Step 2: Run to verify failure**

Run: `cargo test -p bibcode-desktop --lib apply_server_exposure`
Expected: FAIL — command not defined.

- [x] **Step 3: Implement**

In `bridge.rs`, replacing `desktop_bridge_set_server_exposure_mode`:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExposureApplyOutcome {
    Commit,
    CommitWithFirewallWarning,
    Rollback,
}

fn exposure_apply_outcome(
    desired: &str,
    restart_ok: bool,
    achieved_mode: Option<&str>,
    firewall_ok: bool,
) -> ExposureApplyOutcome {
    if !restart_ok {
        return ExposureApplyOutcome::Rollback;
    }
    if desired == "network-accessible" {
        if achieved_mode != Some("network-accessible") || !firewall_ok {
            return ExposureApplyOutcome::Rollback;
        }
        return ExposureApplyOutcome::Commit;
    }
    if firewall_ok {
        ExposureApplyOutcome::Commit
    } else {
        ExposureApplyOutcome::CommitWithFirewallWarning
    }
}

#[tauri::command]
pub async fn desktop_bridge_apply_server_exposure(
    app: AppHandle<DesktopRuntime>,
    backend: State<'_, BackendSupervisor>,
    desired: String,
) -> Result<Value, String> {
    if !matches!(desired.as_str(), "local-only" | "network-accessible") {
        return Err(format!("Unsupported server exposure mode: {desired}"));
    }
    let previous = read_desktop_settings(&app)?.server_exposure_mode;
    if previous == desired {
        // No transition: reconcile the firewall rule; on failure report the error
        // without touching the (unchanged) exposure state.
        let settings = read_desktop_settings(&app)?;
        crate::firewall::sync_remote_access_rule(desired == "network-accessible").await?;
        return Ok(server_exposure_state(
            &settings,
            backend.current_run_config().as_ref(),
        ));
    }

    let settings = update_desktop_settings(&app, |settings| {
        settings.server_exposure_mode = desired.clone();
    })?;
    let restart = backend.restart_default_if_active(app.clone()).await;
    let current_config = match &restart {
        Ok(config) => config.clone().or_else(|| backend.current_run_config()),
        Err(_) => None,
    };
    let achieved_mode = current_config
        .as_ref()
        .map(|config| config.server_exposure_mode.as_str());
    let verified =
        restart.is_ok() && (desired == "local-only" || achieved_mode == Some("network-accessible"));
    let firewall_result = if verified {
        crate::firewall::sync_remote_access_rule(desired == "network-accessible").await
    } else {
        Ok(())
    };

    match exposure_apply_outcome(
        &desired,
        restart.is_ok(),
        achieved_mode,
        firewall_result.is_ok(),
    ) {
        ExposureApplyOutcome::Commit => {
            Ok(server_exposure_state(&settings, current_config.as_ref()))
        }
        ExposureApplyOutcome::CommitWithFirewallWarning => {
            tracing::warn!(
                target: "bibcode_desktop_tauri::bridge",
                "exposure narrowed but firewall cleanup failed: {}",
                firewall_result.err().unwrap_or_default()
            );
            Ok(server_exposure_state(&settings, current_config.as_ref()))
        }
        ExposureApplyOutcome::Rollback => {
            // Restore the previous mode, restart again, drop the firewall rule.
            let detail = match (&restart, &firewall_result) {
                (Err(error), _) => error.clone(),
                (Ok(_), Err(error)) => {
                    format!("firewall rule synchronization failed: {error}")
                }
                (Ok(_), Ok(())) => {
                    "the backend could not bind a network-accessible address".to_owned()
                }
            };
            let _ = update_desktop_settings(&app, |settings| {
                settings.server_exposure_mode = previous.clone();
            })?;
            let recovery = backend.restart_default_if_active(app.clone()).await;
            let _ = crate::firewall::sync_remote_access_rule(false).await;
            let recovery_note = match recovery {
                Ok(_) => String::new(),
                Err(error) => format!(" Recovery restart also failed: {error}"),
            };
            Err(format!(
                "Could not change server exposure; reverted to {previous}: {detail}.{recovery_note}"
            ))
        }
    }
}
```

Register in `lib.rs` (both invoke-handler lists), update the registration-parity test in
`bridge.rs` ~line 2837, and delete the old command + its tests.

`packages/contracts/src/ipc.ts` — replace:

```ts
applyServerExposure: (desired: DesktopServerExposureMode) => Promise<DesktopServerExposureState>;
```

`apps/web/src/tauriDesktopBridge.ts` — replace the `setServerExposureMode`
implementation with:

```ts
applyServerExposure: (desired) =>
  invokeDesktop("desktop_bridge_apply_server_exposure", { desired }),
```

(match the file's existing invoke-wrapper naming — `rg "setServerExposureMode" -B3 -A3
apps/web/src/tauriDesktopBridge.ts` — and update `tauriDesktopBridge.test.ts`
expectations for the renamed command.) `ConnectionsSettings.tsx` still references
`setServerExposureMode` after this step; leave a compile break here only if Task 10 is
executed in the same session — otherwise update the call site mechanically to
`applyServerExposure` so the tree stays green (Task 10 deletes it), **and** update the
fake bridge object plus any exposure-toggle expectations in
`apps/web/src/components/settings/ConnectionsSettings.test.tsx` to the new method
name.

- [x] **Step 4: Run to verify pass**

```bash
cargo test -p bibcode-desktop --lib
vp test run apps/web/src/tauriDesktopBridge.test.ts
vp test run apps/web/src/components/settings/ConnectionsSettings.test.tsx
vp run typecheck
```

Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add apps/desktop/src-tauri/src apps/web/src/tauriDesktopBridge.ts \
  apps/web/src/tauriDesktopBridge.test.ts packages/contracts/src/ipc.ts \
  apps/web/src/components/settings/ConnectionsSettings.tsx
git commit -m "feat(desktop): apply server exposure with verification, rollback, and firewall sync"
```

---

### Task 8: Web API functions for share-state and pairing offers

**Files:**

- Modify: `apps/web/src/environments/primary/auth.ts` (beside
  `createServerPairingCredential` ~line 370), `apps/web/src/environments/primary/index.ts` (exports)

**Interfaces:**

- Consumes: Task 3's share-state contract and Phase 3's pairing-offer contract
  (`client.auth.pairingOffer`) via the existing `PrimaryEnvironmentHttpClient` /
  `runPrimaryHttp` pattern. **Assumption:** Phase 3's endpoint accepts an
  `Idempotency-Key` request header and dedupes on it server-side (same key + same
  input returns the original offer instead of minting a second grant) — verify with
  `rg -i "idempotency" apps/server/src packages/contracts/src`; if the typed contract
  declares the header schema, pass it through the typed field instead of a raw header.
- Produces:
  - `createServerPairingOffer(input: AuthCreatePairingOfferInput, idempotencyKey: string): Promise<AuthPairingOfferResult>`
    — the key makes Task 9's transport-level retries safe: a lost response cannot
    double-mint a grant.
  - `getServerShareState(): Promise<AuthShareStateResult>`

- [x] **Step 1: Implement (mirroring the file's existing wrappers)**

```ts
export async function createServerPairingOffer(
  input: AuthCreatePairingOfferInput,
  idempotencyKey: string,
): Promise<AuthPairingOfferResult> {
  try {
    return await runPrimaryHttp(
      PrimaryEnvironmentHttpClient.pipe(
        Effect.flatMap((client) =>
          client.auth.pairingOffer({
            headers: { "idempotency-key": idempotencyKey },
            payload: input,
          }),
        ),
      ),
    );
  } catch (error) {
    throw PrimaryEnvironmentRequestError.fromCause({
      operation: "create-pairing-offer",
      cause: error,
    });
  }
}

export async function getServerShareState(): Promise<AuthShareStateResult> {
  try {
    return await runPrimaryHttp(
      PrimaryEnvironmentHttpClient.pipe(
        Effect.flatMap((client) => client.auth.shareState({ headers: {} })),
      ),
    );
  } catch (error) {
    throw PrimaryEnvironmentRequestError.fromCause({
      operation: "get-share-state",
      cause: error,
    });
  }
}
```

Export both from `apps/web/src/environments/primary/index.ts` beside
`revokeServerPairingLink`. These thin wrappers get their behavioral coverage through
Task 9/10 tests (which mock this module exactly as `ConnectionsSettings.test.tsx` mocks
its siblings) — no dedicated unit test file.

- [x] **Step 2: Typecheck**

Run: `vp run typecheck` — PASS.

- [x] **Step 3: Commit**

```bash
git add apps/web/src/environments/primary/auth.ts apps/web/src/environments/primary/index.ts
git commit -m "feat(web): client wrappers for pairing offers and share state"
```

---

### Task 9: Share-offer logic module (links, address options, generate orchestration)

**Files:**

- Create: `apps/web/src/components/settings/remote-servers/shareOffer.ts`
- Test: `apps/web/src/components/settings/remote-servers/shareOffer.test.ts`

**Interfaces:**

- Consumes: `AdvertisedEndpoint` (`@bibcode/contracts`), `DesktopServerExposureState`,
  Task 5's `shareClassForPairingEndpoint` / `ShareEndpointClass`, Task 8 function
  signatures (injected), `applyServerExposure` (injected).
- Produces (Task 10 relies on these exact names):

```ts
export type ShareIntent = "another-device" | "this-computer" | "custom";
export interface ShareAddressOption {
  readonly id: string;
  readonly label: string;
  readonly httpBaseUrl: string | null; // null = resolved after widening ("Automatic (LAN)")
  readonly description?: string;
}
export function buildPairDeepLink(code: string): string;
export function buildBrowserPairUrl(endpoint: string, code: string): string;
export function resolveShareAddressOptions(input: {
  readonly intent: ShareIntent;
  readonly advertisedEndpoints: ReadonlyArray<AdvertisedEndpoint>;
  readonly exposureState: DesktopServerExposureState | null;
  readonly primaryHttpBaseUrl: string | null;
}): ReadonlyArray<ShareAddressOption>;
export type GenerateShareOfferFailure =
  | { readonly kind: "invalid-address"; readonly message: string }
  | { readonly kind: "widen-failed"; readonly message: string }
  | { readonly kind: "mint-failed"; readonly message: string; readonly widened: boolean };
export interface GeneratedShareOffer {
  readonly code: string;
  readonly deepLink: string;
  readonly browserUrl: string;
  readonly endpoint: string;
  readonly name: string;
  readonly expiresAt: string;
  readonly reach: ShareIntent;
  readonly endpointClass: "loopback" | "off-host";
}
export interface GenerateShareOfferDeps {
  readonly intent: ShareIntent;
  readonly name: string;
  readonly customAddress: string | null;
  readonly selectedOption: ShareAddressOption;
  readonly hasDesktopBridge: boolean;
  readonly exposureState: DesktopServerExposureState | null;
  readonly applyServerExposure:
    | ((desired: "local-only" | "network-accessible") => Promise<DesktopServerExposureState>)
    | null;
  readonly mintOffer: (input: {
    name: string;
    endpoint: string;
    reach: ShareIntent;
    idempotencyKey: string;
  }) => Promise<{ code: string; endpoint: string; name: string; expiresAt: string }>;
  readonly newIdempotencyKey: () => string; // one key per generate invocation
  readonly classifyMintError: (error: unknown) => "retryable" | "fatal";
  readonly sleep: (ms: number) => Promise<void>;
}
export async function generateShareOffer(
  deps: GenerateShareOfferDeps,
): Promise<
  { ok: true; offer: GeneratedShareOffer } | { ok: false; failure: GenerateShareOfferFailure }
>;
```

Pinned behavior of `generateShareOffer` (decision 2):

1. Resolve the target endpoint: custom → validated `customAddress`
   (`normalizeHttpBaseUrl`; parse failure ⇒ `invalid-address`); this-computer →
   `primary` loopback option's URL; another-device → the selected option's URL, or, for
   the "Automatic (LAN)" option (`httpBaseUrl: null`), the post-widen
   `exposureState.endpointUrl`.
2. If intent is off-host (`another-device`, or `custom` whose address classifies
   `off-host`) **and** the desktop bridge is present **and** current exposure mode is
   not `network-accessible`: call `applyServerExposure("network-accessible")` first. Any
   rejection ⇒ `{ kind: "widen-failed" }` — nothing was minted, no grant dangles.
   Re-read the returned state for the automatic endpoint URL; a null `endpointUrl`
   after widening ⇒ `widen-failed` ("no reachable LAN address").
3. Mint with a bounded, classified retry: the widen restarted the backend, so the
   primary connection may still be re-establishing. Generate one idempotency key via
   `newIdempotencyKey()` for the whole invocation and pass it on **every** attempt —
   the server dedupes, so a lost response cannot double-mint a grant. On rejection,
   consult `classifyMintError`: `"fatal"` (any typed contract error — validation,
   scope, auth: a 4xx will not succeed by repetition) fails immediately;
   `"retryable"` (transport/network failures) retries up to **4** more attempts with
   `sleep(2000)` between (~8 s budget, above the supervisor's early 1/2/4 s reconnect
   steps). Final failure ⇒ `{ kind: "mint-failed", widened: <whether step 2 ran> }` —
   the UI explains that the widened bind will auto-revert because no off-host grant
   exists (Task 11).
4. Compose `deepLink = buildPairDeepLink(code)` (`bibcode://pair?code=<code>`),
   `browserUrl = buildBrowserPairUrl(endpoint, code)` (`<endpoint>/pair?code=<code>`),
   `endpointClass` from Task 5's `shareClassForPairingEndpoint(endpoint)` (an
   `"unconnectable"` classification of a custom address fails as `invalid-address`
   before any widen or mint).
5. `this-computer` never calls `applyServerExposure` (spec §4.6). Browser mode
   (`hasDesktopBridge: false`) never calls it either — mint only.

- [x] **Step 1: Failing tests** (representative set — write all of these):

```ts
import { describe, expect, it, vi } from "@effect/vitest";

import {
  buildBrowserPairUrl,
  buildPairDeepLink,
  generateShareOffer,
  resolveShareAddressOptions,
} from "./shareOffer.ts";

const wideState = {
  mode: "network-accessible" as const,
  endpointUrl: "http://192.168.1.20:3773",
  advertisedHost: "192.168.1.20",
  tailscaleServeEnabled: false,
  tailscaleServePort: 443,
};
const loopbackState = {
  ...wideState,
  mode: "local-only" as const,
  endpointUrl: null,
  advertisedHost: null,
};
const defaultDeps = {
  newIdempotencyKey: () => "key-1",
  classifyMintError: (): "retryable" | "fatal" => "retryable",
  sleep: async () => {},
};

describe("share offer links", () => {
  it("builds the deep link and browser URL from one code", () => {
    expect(buildPairDeepLink("abc123")).toBe("bibcode://pair?code=abc123");
    expect(buildBrowserPairUrl("http://192.168.1.20:3773", "abc123")).toBe(
      "http://192.168.1.20:3773/pair?code=abc123",
    );
  });
});

describe("generateShareOffer", () => {
  it("widens before minting for another-device on a loopback desktop", async () => {
    const calls: string[] = [];
    const applyServerExposure = vi.fn(async () => {
      calls.push("widen");
      return wideState;
    });
    const mintOffer = vi.fn(async (input: { endpoint: string }) => {
      calls.push("mint");
      return {
        code: "c0de",
        endpoint: input.endpoint,
        name: "AI-SERVER",
        expiresAt: "2026-08-27T01:00:00.000Z",
      };
    });
    const result = await generateShareOffer({
      intent: "another-device",
      name: "AI-SERVER",
      customAddress: null,
      selectedOption: { id: "auto-lan", label: "Automatic (LAN)", httpBaseUrl: null },
      hasDesktopBridge: true,
      exposureState: loopbackState,
      applyServerExposure,
      mintOffer,
      ...defaultDeps,
    });
    expect(calls).toEqual(["widen", "mint"]);
    expect(result).toMatchObject({
      ok: true,
      offer: { endpoint: "http://192.168.1.20:3773", deepLink: "bibcode://pair?code=c0de" },
    });
  });

  it("fails visibly without minting when widening fails", async () => {
    const mintOffer = vi.fn();
    const result = await generateShareOffer({
      intent: "another-device",
      name: "AI-SERVER",
      customAddress: null,
      selectedOption: { id: "auto-lan", label: "Automatic (LAN)", httpBaseUrl: null },
      hasDesktopBridge: true,
      exposureState: loopbackState,
      applyServerExposure: async () => {
        throw new Error("Could not change server exposure; reverted to local-only: bind failed.");
      },
      mintOffer,
      ...defaultDeps,
    });
    expect(mintOffer).not.toHaveBeenCalled();
    expect(result).toMatchObject({ ok: false, failure: { kind: "widen-failed" } });
  });

  it("never widens for this-computer offers", async () => {
    const applyServerExposure = vi.fn();
    const result = await generateShareOffer({
      intent: "this-computer",
      name: "AI-SERVER",
      customAddress: null,
      selectedOption: {
        id: "primary",
        label: "This computer",
        httpBaseUrl: "http://127.0.0.1:3773",
      },
      hasDesktopBridge: true,
      exposureState: loopbackState,
      applyServerExposure,
      mintOffer: async (input) => ({
        code: "c0de",
        endpoint: input.endpoint,
        name: input.name,
        expiresAt: "2026-08-27T01:00:00.000Z",
      }),
      ...defaultDeps,
    });
    expect(applyServerExposure).not.toHaveBeenCalled();
    expect(result).toMatchObject({ ok: true, offer: { endpointClass: "loopback" } });
  });

  it("retries retryable mint failures with one stable idempotency key", async () => {
    let attempts = 0;
    const seenKeys = new Set<string>();
    const result = await generateShareOffer({
      intent: "another-device",
      name: "AI-SERVER",
      customAddress: null,
      selectedOption: { id: "auto-lan", label: "Automatic (LAN)", httpBaseUrl: null },
      hasDesktopBridge: true,
      exposureState: wideState,
      applyServerExposure: async () => wideState,
      mintOffer: async (input) => {
        attempts += 1;
        seenKeys.add(input.idempotencyKey);
        if (attempts < 3) throw new Error("connection re-establishing");
        return {
          code: "c0de",
          endpoint: input.endpoint,
          name: input.name,
          expiresAt: "2026-08-27T01:00:00.000Z",
        };
      },
      ...defaultDeps,
    });
    expect(attempts).toBe(3);
    expect(seenKeys.size).toBe(1); // same key every attempt — server-side dedupe holds
    expect(result).toMatchObject({ ok: true });
  });

  it("never retries fatal (4xx contract) mint failures", async () => {
    const mintOffer = vi.fn(async () => {
      throw new Error("invalid_pairing_offer");
    });
    const result = await generateShareOffer({
      intent: "another-device",
      name: "AI-SERVER",
      customAddress: null,
      selectedOption: { id: "auto-lan", label: "Automatic (LAN)", httpBaseUrl: null },
      hasDesktopBridge: true,
      exposureState: wideState,
      applyServerExposure: async () => wideState,
      mintOffer,
      ...defaultDeps,
      classifyMintError: () => "fatal",
    });
    expect(mintOffer).toHaveBeenCalledTimes(1);
    expect(result).toMatchObject({ ok: false, failure: { kind: "mint-failed" } });
  });

  it("caps retryable mint attempts at five", async () => {
    const mintOffer = vi.fn(async () => {
      throw new Error("still unreachable");
    });
    const result = await generateShareOffer({
      intent: "another-device",
      name: "AI-SERVER",
      customAddress: null,
      selectedOption: { id: "auto-lan", label: "Automatic (LAN)", httpBaseUrl: null },
      hasDesktopBridge: true,
      exposureState: wideState,
      applyServerExposure: async () => wideState,
      mintOffer,
      ...defaultDeps,
    });
    expect(mintOffer).toHaveBeenCalledTimes(5); // 1 initial + 4 retries
    expect(result).toMatchObject({ ok: false, failure: { kind: "mint-failed" } });
  });

  it("classifies a loopback custom address for the tunnel acknowledgement copy", async () => {
    const applyServerExposure = vi.fn(async () => wideState);
    const result = await generateShareOffer({
      intent: "custom",
      name: "AI-SERVER",
      customAddress: "http://127.0.0.1:9022",
      selectedOption: { id: "custom", label: "Custom address", httpBaseUrl: null },
      hasDesktopBridge: true,
      exposureState: loopbackState,
      applyServerExposure,
      mintOffer: async (input) => ({
        code: "c0de",
        endpoint: input.endpoint,
        name: input.name,
        expiresAt: "2026-08-27T01:00:00.000Z",
      }),
      ...defaultDeps,
    });
    // Loopback custom address (an SSH tunnel) must not widen the bind (amended §4.6).
    expect(applyServerExposure).not.toHaveBeenCalled();
    expect(result).toMatchObject({
      ok: true,
      offer: { endpointClass: "loopback", reach: "custom" },
    });
  });
});

describe("resolveShareAddressOptions", () => {
  it("offers automatic LAN plus non-loopback advertised endpoints for another-device", () => {
    const options = resolveShareAddressOptions({
      intent: "another-device",
      advertisedEndpoints: [
        {
          id: "tailscale-https",
          label: "Tailscale HTTPS",
          provider: { id: "tailscale", label: "Tailscale", kind: "private-network", isAddon: true },
          httpBaseUrl: "https://machine.tailnet.ts.net/",
          wsBaseUrl: "wss://machine.tailnet.ts.net/",
          reachability: "private-network",
          compatibility: { hostedHttpsApp: "compatible", desktopApp: "compatible" },
          source: "desktop-addon",
          status: "available",
        },
      ],
      exposureState: loopbackState,
      primaryHttpBaseUrl: "http://127.0.0.1:3773",
    });
    expect(options[0]).toMatchObject({ id: "auto-lan", httpBaseUrl: null });
    expect(options.some((option) => option.httpBaseUrl === "https://machine.tailnet.ts.net/")).toBe(
      true,
    );
  });

  it("offers only the loopback primary endpoint for this-computer", () => {
    const options = resolveShareAddressOptions({
      intent: "this-computer",
      advertisedEndpoints: [],
      exposureState: loopbackState,
      primaryHttpBaseUrl: "http://127.0.0.1:3773",
    });
    expect(options).toEqual([
      {
        id: "primary",
        label: "This computer",
        httpBaseUrl: "http://127.0.0.1:3773",
        description: "Only clients on this machine (or a tunnel into it) can use this offer.",
      },
    ]);
  });
});
```

- [x] **Step 2: Run to verify failure**

Run: `vp test run apps/web/src/components/settings/remote-servers/shareOffer.test.ts`
Expected: FAIL — module missing.

- [x] **Step 3: Implement `shareOffer.ts`** exactly to the pinned behavior above.
      Reference implementation of the core orchestration:

```ts
export async function generateShareOffer(
  deps: GenerateShareOfferDeps,
): Promise<
  { ok: true; offer: GeneratedShareOffer } | { ok: false; failure: GenerateShareOfferFailure }
> {
  let endpoint: string | null;
  try {
    endpoint = resolveOfferEndpoint(deps);
  } catch (error) {
    return {
      ok: false,
      failure: {
        kind: "invalid-address",
        message: error instanceof Error ? error.message : "Enter a valid http(s) address.",
      },
    };
  }

  const endpointClassForWiden =
    deps.intent === "another-device"
      ? "off-host"
      : deps.intent === "custom" && endpoint !== null
        ? shareClassForPairingEndpoint(endpoint)
        : "loopback";
  if (endpointClassForWiden === "unconnectable") {
    return {
      ok: false,
      failure: {
        kind: "invalid-address",
        message: "This address is not reachable as entered. Check the host and port.",
      },
    };
  }
  let widened = false;
  if (
    endpointClassForWiden === "off-host" &&
    deps.hasDesktopBridge &&
    deps.applyServerExposure !== null &&
    deps.exposureState?.mode !== "network-accessible"
  ) {
    try {
      const state = await deps.applyServerExposure("network-accessible");
      widened = true;
      if (endpoint === null) endpoint = state.endpointUrl;
    } catch (error) {
      return {
        ok: false,
        failure: {
          kind: "widen-failed",
          message: error instanceof Error ? error.message : "Could not enable remote access.",
        },
      };
    }
  }
  if (endpoint === null) endpoint = deps.exposureState?.endpointUrl ?? null;
  if (endpoint === null) {
    return {
      ok: false,
      failure: { kind: "widen-failed", message: "No reachable network address is available." },
    };
  }

  const reach = deps.intent;
  const idempotencyKey = deps.newIdempotencyKey(); // one key for every attempt below
  const MAX_MINT_ATTEMPTS = 5; // 1 initial + 4 retries, retryable failures only
  let lastError: unknown = null;
  for (let attempt = 0; attempt < MAX_MINT_ATTEMPTS; attempt += 1) {
    if (attempt > 0) await deps.sleep(2000);
    try {
      const minted = await deps.mintOffer({ name: deps.name, endpoint, reach, idempotencyKey });
      const endpointClass = shareClassForPairingEndpoint(minted.endpoint);
      return {
        ok: true,
        offer: {
          code: minted.code,
          deepLink: buildPairDeepLink(minted.code),
          browserUrl: buildBrowserPairUrl(minted.endpoint, minted.code),
          endpoint: minted.endpoint,
          name: minted.name,
          expiresAt: minted.expiresAt,
          reach,
          endpointClass: endpointClass === "unconnectable" ? "off-host" : endpointClass,
        },
      };
    } catch (error) {
      lastError = error;
      if (deps.classifyMintError(error) === "fatal") {
        break; // a contract/validation/auth rejection will not succeed by repetition
      }
    }
  }
  return {
    ok: false,
    failure: {
      kind: "mint-failed",
      widened,
      message:
        lastError instanceof Error ? lastError.message : "Could not create the pairing offer.",
    },
  };
}
```

`resolveOfferEndpoint` handles the three intents (custom validates via
`normalizeHttpBaseUrl` and throws with a user-readable message; this-computer takes the
selected option's URL; another-device takes the selected option's URL or `null` for
automatic). `buildBrowserPairUrl` uses `new URL(endpoint)`, sets `pathname = "/pair"`
and `searchParams.set("code", code)` — matching the existing
`resolveDesktopPairingUrl` idiom in `apps/web/src/components/settings/pairingUrls.ts`
but with the `code` parameter of spec §4.2 instead of the legacy token parameter.

- [x] **Step 4: Run to verify pass**

Run: `vp test run apps/web/src/components/settings/remote-servers/shareOffer.test.ts` — PASS.

- [x] **Step 5: Commit**

```bash
git add apps/web/src/components/settings/remote-servers
git commit -m "feat(web): share-offer generation logic with widen-first ceremony"
```

---

### Task 10: `ShareThisHostTab` component and settings integration

**Files:**

- Create: `apps/web/src/components/settings/remote-servers/ShareThisHostTab.tsx`
- Test: `apps/web/src/components/settings/remote-servers/ShareThisHostTab.test.tsx`
- Modify: the Phase 4 Remote Servers tab shell (locate per "Consumed interfaces") to
  mount the tab; `apps/web/src/components/settings/ConnectionsSettings.tsx` to delete
  the superseded manual-exposure surface

**Interfaces:**

- Consumes: Task 9 module; Task 8 API functions; `desktopNetworkAccessStateAtom` +
  `refreshDesktopNetworkAccessState` (`apps/web/src/state/desktopNetworkAccess.ts`);
  `authEnvironment.accessChanges` snapshot (pairing links + client sessions, as in
  `ConnectionsSettings.tsx` ~line 2082); `PairingClientsList`,
  `AuthorizedClientsHeaderAction` and the revoke handlers (reuse by extracting them
  unchanged from `ConnectionsSettings.tsx` into the new directory if Phase 4 has not
  already moved them — move, do not duplicate); `QRCodeSvg`; `SettingsSection` /
  `SettingsRow` layout primitives; `readCurrentEnvironmentPresentationPolicy` for
  browser-mode degradation.
- Produces: `export function ShareThisHostTab(): ReactElement` — the complete §4.8
  "Share this host" tab.

UI contract (all copy pinned here; spec §3/§4.8):

- **Offer generator** section:
  - Name input (defaults to the machine/host name from the primary server config when
    available, else "BiBCode Server").
  - Intent radio: `Another device` (default, description "Recommended. Uses a network
    address other devices can reach."), `This computer only` (description "Creates a
    loopback offer. Other devices need a tunnel — for example SSH port forwarding — to
    use it."), `Custom address` (description "For SSH tunnels, reverse proxies, or a
    hostname you manage." with a text input validated on generate).
  - Address picker (select) fed by `resolveShareAddressOptions`; hidden for
    `this-computer`; refresh button calls `refreshDesktopNetworkAccessState()`.
  - Threat-model banner, always visible above the Generate button:
    "Pairing grants your user account on this machine. A paired client can read and
    write files, run terminals, and use git as you."
  - Widen warning shown when the pending generate would widen (off-host intent, desktop
    bridge present, mode currently `local-only`): "Enabling remote access restarts the
    local server. Running turns on this machine will stop."
  - Generate button → `generateShareOffer` with deps wired to
    `window.desktopBridge?.applyServerExposure`, `createServerPairingOffer`, and
    `getServerShareState`-independent sleep. Failures render inline by kind
    (`invalid-address` under the custom input; `widen-failed` / `mint-failed` as a
    section-level error, `mint-failed` with `widened: true` adding "Remote access will
    switch off again automatically because no offer was created.").
  - Result panel: the pairing code (copyable), the deep link (copy button,
    `bibcode://pair?code=…`), the browser URL labeled **"Open in browser — for networks
    you trust"** with the sub-copy "This link and the page it loads travel over plain
    HTTP. Prefer the pairing code for BiBCode clients." (spec §3 case 3), and a
    `QRCodeSvg value={offer.deepLink} size={128} level="M" marginSize={2}
    title="Pairing code — scan with a BiBCode client"`. For `this-computer` and
    loopback-classified custom offers, add "Loopback offer: reachable only through a
    tunnel into this machine."
- **Exposure** section:
  - Desktop bridge present: read-only `SettingsRow` "Remote access" showing the
    effective state from `desktopNetworkAccessStateAtom` ("Limited to this machine." /
    "Reachable at <endpointUrl>") plus the derivation sentence "Managed automatically:
    switches on while at least one off-host pairing exists and back off when the last
    one is revoked." When `getServerShareState()` reports `legacyGrantCount > 0` and the
    mode is wide, add "Paired clients from an earlier version keep remote access on.
    Revoke or re-pair them to allow automatic switch-off."
  - No desktop bridge (browser mode / headless server): read-only row with the existing
    policy-derived text (auth policy `remote-reachable` ⇒ "This server is configured
    for remote access.") and CLI guidance: "Exposure is controlled where the server is
    launched — restart `bibcode serve` with `--host` to change it." Generation stays
    fully available (server-side mint).
  - Tailscale HTTPS row and the advertised-endpoint rows move here unchanged from
    `ConnectionsSettings.tsx` (`renderTailscaleRow`, `renderEndpointRows`).
- **Paired clients** section: `PairingClientsList` + `AuthorizedClientsHeaderAction`
  (create-link dialog retitled "Create pairing offer" only if Phase 4 hasn't already
  restructured it — otherwise leave as moved), wired to the existing revoke handlers
  (`revokeServerPairingLink`, `revokeServerClientSession`,
  `revokeOtherServerClientSessions`). Revocation success triggers a share-state
  re-check (Task 11's reconciler picks up the narrow).

Deletions in the same patch: the manual "Network access" toggle
(`renderNetworkAccessToggle`, `renderNetworkAccessRow`,
`renderDisabledNetworkAccessRow`, `handleDesktopServerExposureChange`,
`handleConfirmDesktopServerExposureChange`, the exposure confirm dialog state) and any
remaining `applyServerExposure` direct call left by Task 7's mechanical rename. The
Share tab is now the only exposure surface.

- [x] **Step 1: Failing component tests** — mock exactly like
      `ConnectionsSettings.test.tsx` mocks `~/environments/primary` and the bridge (reuse
      its harness helpers where exported). Cover at minimum:

```ts
// ShareThisHostTab.test.tsx — assertions to implement with the harness:
// 1. renders the three intent options and the threat-model copy
//    ("Pairing grants your user account on this machine").
// 2. desktop + local-only + Another device: shows the restart warning; clicking
//    Generate calls bridge.applyServerExposure("network-accessible") BEFORE
//    createServerPairingOffer (spy call order), then renders deep link, browser URL
//    labeled "for networks you trust", and the QR svg.
// 3. widen rejection renders the widen-failed error and does not call
//    createServerPairingOffer.
// 4. This computer only: Generate never touches the bridge and the result shows the
//    loopback/tunnel note.
// 5. browser mode (no bridge): exposure row is read-only with the CLI guidance copy;
//    Generate still mints via createServerPairingOffer.
// 6. revoking a client session calls revokeServerClientSession and refreshes share
//    state.
```

Write these as real `@testing-library/react` tests following the render/query patterns
of `ConnectionsSettings.test.tsx` (same providers, same `vi.mock` module paths, fake
`window.desktopBridge` object carrying `applyServerExposure`,
`getServerExposureState`, `getAdvertisedEndpoints`).

- [x] **Step 2: Run to verify failure**

Run: `vp test run apps/web/src/components/settings/remote-servers/ShareThisHostTab.test.tsx`
Expected: FAIL — component missing.

- [x] **Step 3: Implement the component** per the UI contract above. Skeleton of the
      generate wiring (state and layout follow the section list; use the imports and
      patterns already present in `ConnectionsSettings.tsx`):

```tsx
const [intent, setIntent] = useState<ShareIntent>("another-device");
const [offerName, setOfferName] = useState("");
const [customAddress, setCustomAddress] = useState("");
const [selectedOptionId, setSelectedOptionId] = useState<string | null>(null);
const [offer, setOffer] = useState<GeneratedShareOffer | null>(null);
const [failure, setFailure] = useState<GenerateShareOfferFailure | null>(null);
const [isGenerating, setIsGenerating] = useState(false);

const options = useMemo(
  () =>
    resolveShareAddressOptions({
      intent,
      advertisedEndpoints,
      exposureState: desktopServerExposureState,
      primaryHttpBaseUrl,
    }),
  [intent, advertisedEndpoints, desktopServerExposureState, primaryHttpBaseUrl],
);
const selectedOption =
  options.find((option) => option.id === selectedOptionId) ?? options[0] ?? null;

const handleGenerate = useCallback(async () => {
  if (selectedOption === null) return;
  setIsGenerating(true);
  setFailure(null);
  const result = await generateShareOffer({
    intent,
    name: offerName.trim() === "" ? "BiBCode Server" : offerName.trim(),
    customAddress: intent === "custom" ? customAddress : null,
    selectedOption,
    hasDesktopBridge: desktopBridge !== undefined,
    exposureState: desktopServerExposureState,
    applyServerExposure: desktopBridge
      ? (desired) => desktopBridge.applyServerExposure(desired)
      : null,
    mintOffer: ({ idempotencyKey, ...input }) => createServerPairingOffer(input, idempotencyKey),
    newIdempotencyKey: () => crypto.randomUUID(),
    // Fatal = the request itself was rejected by the contract (a 4xx that repetition
    // cannot fix). Transport failures are ALSO tagged in Effect (RequestError /
    // ResponseError), so match the explicit 4xx contract tags — never "any _tag".
    classifyMintError: (error) => {
      const FATAL_CAUSE_TAGS = new Set([
        "EnvironmentRequestInvalidError",
        "EnvironmentScopeRequiredError",
        "EnvironmentAuthInvalidError",
        "EnvironmentOperationForbiddenError",
      ]);
      const cause =
        typeof error === "object" && error !== null && "cause" in error
          ? (error as { cause?: { _tag?: string } }).cause
          : undefined;
      return cause?._tag !== undefined && FATAL_CAUSE_TAGS.has(cause._tag) ? "fatal" : "retryable"; // incl. RequestError/ResponseError and 500-class internal errors
    },
    sleep: (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
  });
  if (result.ok) {
    setOffer(result.offer);
    refreshDesktopNetworkAccessState();
  } else {
    setFailure(result.failure);
    refreshDesktopNetworkAccessState();
  }
  setIsGenerating(false);
}, [customAddress, desktopBridge, desktopServerExposureState, intent, offerName, selectedOption]);
```

Mount the tab in the Phase 4 shell; move (not copy) `PairingClientsList` and its row
components if extracting them from `ConnectionsSettings.tsx` keeps that file compiling
(update its imports). Delete the superseded manual-toggle code listed above and update
`ConnectionsSettings.test.tsx` accordingly (its exposure-toggle tests are replaced by
this file's tests).

- [x] **Step 4: Run to verify pass**

```bash
vp test run apps/web/src/components/settings/remote-servers/ShareThisHostTab.test.tsx
vp test run apps/web/src/components/settings/ConnectionsSettings.test.tsx
vp run typecheck
```

Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add apps/web/src/components/settings
git commit -m "feat(web): Share this host tab with grant-driven exposure ceremony"
```

---

### Task 11: Narrow-only exposure reconciler (app level)

**Files:**

- Create: `apps/web/src/state/shareExposureReconciler.ts`
- Test: `apps/web/src/state/shareExposureReconciler.test.ts`
- Modify: `apps/web/src/AppRoot.tsx` (mount the hook — the file already hosts
  desktop-only effects and reads `window.desktopBridge`)

**Interfaces:**

- Consumes: `getServerShareState` (Task 8), `DesktopBridge.applyServerExposure`
  (Task 7), `authEnvironment.accessChanges` via `useEnvironmentQuery`
  (`apps/web/src/state/query`), `primaryEnvironmentIdAtom`
  (`apps/web/src/state/primaryEnvironment.ts`), toast (`toastManager` — same import as
  `ConnectionsSettings.tsx`).
- Produces:
  - Pure: `shouldRevertExposure(input: { shareState: AuthShareStateResult; exposureMode: DesktopServerExposureMode }): boolean`
  - Hook: `useShareExposureReconciler(): void`

Pinned behavior:

- Revert iff `exposureMode === "network-accessible"` and
  `shareState.desiredExposure === "loopback"` and `shareState.legacyGrantCount === 0`
  (decision 4). The reconciler **never** widens.
- Triggers: once after the desktop bridge and primary environment are available
  (startup check, covers a wide bind left by a mint failure or a crash mid-ceremony),
  and on every auth-access snapshot revision change (covers revocation from any
  client). Concurrency-guard with a ref so overlapping checks collapse.
- On revert: `applyServerExposure("local-only")`, then a toast:
  `{ type: "info", title: "Remote access switched off", description: "No active off-host pairings remain, so the local server is loopback-only again." }`
  (use the toast variant the toast manager actually supports — check
  `rg "toastManager.add" apps/web/src | head`). Failures log and retry on the next
  trigger only (no tight loop).
- No-op entirely when `window.desktopBridge?.applyServerExposure` is absent (browser
  mode).

- [x] **Step 1: Failing tests** — test the pure function exhaustively and the hook's
      trigger wiring with mocks:

```ts
import { describe, expect, it } from "@effect/vitest";

import { shouldRevertExposure } from "./shareExposureReconciler.ts";

describe("shouldRevertExposure", () => {
  const loopbackDesired = {
    desiredExposure: "loopback",
    offHostGrantCount: 0,
    legacyGrantCount: 0,
  } as const;

  it("reverts a wide bind with no off-host and no legacy grants", () => {
    expect(
      shouldRevertExposure({ shareState: loopbackDesired, exposureMode: "network-accessible" }),
    ).toBe(true);
  });

  it("never reverts while legacy grants exist", () => {
    expect(
      shouldRevertExposure({
        shareState: { ...loopbackDesired, legacyGrantCount: 1 },
        exposureMode: "network-accessible",
      }),
    ).toBe(false);
  });

  it("never acts on a loopback bind or a wide-desired state", () => {
    expect(shouldRevertExposure({ shareState: loopbackDesired, exposureMode: "local-only" })).toBe(
      false,
    );
    expect(
      shouldRevertExposure({
        shareState: { desiredExposure: "wide", offHostGrantCount: 1, legacyGrantCount: 0 },
        exposureMode: "network-accessible",
      }),
    ).toBe(false);
  });
});
```

Add a hook test (React Testing Library `renderHook` with mocked
`~/environments/primary` and a fake bridge) asserting: revision change →
`getServerShareState` called → `applyServerExposure("local-only")` called exactly once
when `shouldRevertExposure` is true, never called when the bridge is absent.

- [x] **Step 2: Run to verify failure**

Run: `vp test run apps/web/src/state/shareExposureReconciler.test.ts`
Expected: FAIL — module missing.

- [x] **Step 3: Implement** the module (pure function + hook per the pinned behavior)
      and mount `useShareExposureReconciler()` inside the component in `AppRoot.tsx` that
      already runs per-app desktop effects (top of the component that reads
      `window.desktopBridge`, ~line 28).

- [x] **Step 4: Run to verify pass**

```bash
vp test run apps/web/src/state/shareExposureReconciler.test.ts
vp test run apps/web/src/AppRoot.test.tsx apps/web/src/AppRoot.lifecycle.test.tsx
```

Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add apps/web/src/state/shareExposureReconciler.ts apps/web/src/state/shareExposureReconciler.test.ts apps/web/src/AppRoot.tsx
git commit -m "feat(web): revert server exposure when the last off-host grant is revoked"
```

---

### Task 12: Living documentation and testing runbooks

**Files:**

- Modify: `docs/architecture/remote.md`, `docs/architecture/overview.md`
- Modify: `docs/testing/windows-desktop.md`
- Review (state outcome explicitly): `docs/testing/linux-desktop.md`,
  `docs/testing/macos-desktop.md`, `docs/testing/cross-platform-validation.md`,
  `docs/testing/README.md`

- [x] **Step 1: `docs/architecture/remote.md`** — add/extend a "Share ceremony and
      exposure" section covering, in prose consistent with the file's voice:
  - pairing offers: extend whatever Phase 3 documented for
    `POST /api/auth/pairing-offer` (it mints the §4.2 payload server-side) with this
    phase's addition — grants minted there record `reach` plus the mint-time
    `off_host` flag, and sessions inherit both on consumption;
  - grant-driven exposure: desired exposure is derived server-side
    (`GET /api/auth/share-state`) from the persisted off-host flags — wide iff ≥1
    unrevoked off-host grant, with `custom` classified at mint time (amended §4.6);
    the desktop widens only during off-host offer generation through
    `desktop_bridge_apply_server_exposure` (restart-based rebind, verified, rolled back
    on bind **or firewall** failure, Windows firewall rule synced), and the persisted
    desktop setting acts as the launch-time cache so a `this-computer` grant never
    widens a later launch;
  - the legacy-grant rule from decision 4, verbatim in substance (never auto-widen;
    auto-revert blocked while `NULL`-reach one-time-token grants remain);
  - revocation: revoking a paired client now also terminates its live WebSocket
    sessions (amended spec §3; Task 4b) — align the file's revocation wording;
  - the honest restart consequence (live connections drop; durable state persists) and
    the maintenance-API degradation while wide (decision 8).
- [x] **Step 2: `docs/architecture/overview.md`** — in the "Desktop update protection"
      /exposure vicinity, one paragraph: exposure changes now flow exclusively through the
      grant-driven ceremony; the manual toggle is gone; wildcard native binds still occur
      only via this machinery.
- [x] **Step 3: `docs/testing/windows-desktop.md`** — add a validation procedure to the
      packaged-UI flow section: generate an "Another device" offer from Settings → Remote
      Servers → Share this host; verify (a) the server restarts and the offer renders
      URL + deep link + QR, (b) `netsh advfirewall firewall show rule name="BiBCode Remote
  Access"` lists the program-scoped rule, (c) revoking the offer/paired client reverts
      exposure and (d) the rule is removed. Keep execution-specific evidence out of the
      runbook (it belongs in reports from the template).
- [x] **Step 4: Review the remaining runbooks.** Linux/macOS packaged flows gain the
      Share tab generation + revert check **without** the firewall step — add that flow if
      the runbooks enumerate settings surfaces; otherwise record in the final report:
      "reviewed and remain accurate". `cross-platform-validation.md` likewise.
- [x] **Step 5: Commit**

```bash
git add docs/architecture/remote.md docs/architecture/overview.md docs/testing
git commit -m "docs: share ceremony, grant-driven exposure, and Windows validation runbook"
```

---

### Task 13: Full validation gate and self-review

- [x] **Step 1: Run the complete gate** (master-plan validation gate):

```bash
vp check
vp run typecheck
cargo fmt --all --check
cargo clippy -p bibcode-server -p bibcode-desktop --all-targets -- -D warnings
cargo test -p bibcode-server
cargo test -p bibcode-desktop --lib
vp test run packages/contracts/src/authRustParity.test.ts packages/contracts/src/persistenceRustParity.test.ts
vp run --filter @bibcode/web test
vp run --filter @bibcode/shared test
```

(If a workspace filter name differs, take it from the package's `package.json` `name`.)
Record every command and its result for the final report; anything that cannot run on
this machine (Windows firewall execution, packaged desktop flows) must be listed as
Windows-only evidence owed per `docs/testing/windows-desktop.md`.

- [x] **Step 2: Diff review**

```bash
git status --short
git diff origin/HEAD --stat
```

Check: no `.codegraph/` or generated files staged, no dependency drift, the pending
deletions under `docs/plans/2026-08-24-environment-project-management/` untouched, no
debug output.

- [x] **Step 3: Self-review checklist**
  - Spec coverage: §4.2 generation side (intent radio ✓ Task 10, address picker ✓
    Tasks 9–10, browser URL + deep link + QR ✓ Tasks 9–10, server-side mint ✓ Phase 3
    endpoint + Task 4 reach/off-host persistence);
    §4.6 amended (reach + off-host persisted ✓ Tasks 1–2, flag-based derivation incl.
    custom-at-mint ✓ Tasks 3–4, widen/rollback/firewall incl.
    firewall-failure-rolls-back ✓ Tasks 6–7, this-computer and loopback-custom never
    widen ✓ Tasks 3/4/9 tests, revert on last revocation ✓ Task 11, headless/browser
    read-only exposure with mint+revoke available ✓ Task 10); §4.8 Share tab (clients
    list + revocation ✓ Task 10); §3 amended (threat copy ✓ Task 10, active WS
    termination on revoke ✓ Task 4b).
  - Reference-string ban: `grep -ric <the banned reference-product name> apps packages docs/plans/remote-servers/phases/phase-5-share-tab-exposure.md`
    over every file this phase touched must return 0 matches. (This plan deliberately
    never spells the name; it is the first path segment of the research companion
    document's filename listed in the spec's "Companion documents" section.)
  - Type consistency: `reach` / `off_host` field names, `issue_share_pairing`
    four-argument signature, `applyServerExposure` signature,
    `shareClassForPairingEndpoint` mapping, `AuthShareStateResult` field names, and the
    `mark_connected`/`mark_disconnected` connection-id signatures identical across Rust
    serde renames, TS schemas, fixtures, and web call sites.

- [x] **Step 4: Final commit** (only if the gate produced fixes)

```bash
git add -A -- ':!docs/plans/2026-08-24-environment-project-management'
git commit -m "chore: phase 5 validation fixes"
```

## Residual risks (report these at completion)

1. **Restart blast radius.** Every widen/revert stops and restarts all local backends;
   long-running local turns are terminated. Mitigated by explicit warning copy and by
   only restarting on actual mode transitions — not eliminable with the current
   restart-based rebind.
2. **Port drift vs. advertised offers.** A restart can pick a different port; an
   already-generated (unconsumed) offer then points at a dead endpoint. Offers are
   short-lived (existing pairing TTL) and regeneration is cheap; documented, not
   solved.
3. **Firewall execution is Windows-only evidence.** netsh behavior (elevation
   requirements, store-signed executable paths) can only be proven on native Windows
   per the runbook.
4. **Legacy grants block auto-revert by design** — a user with pre-upgrade paired
   clients keeps a wide bind until they revoke/re-pair; surfaced in UI copy.
5. **Update protection while shared.** Wide-bound native primaries have no maintenance
   API (`maintenance_routes_enabled`), so desktop update protection degrades while
   sharing — pre-existing behavior that this feature makes more common.
