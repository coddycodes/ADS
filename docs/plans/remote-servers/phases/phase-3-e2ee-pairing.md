# Phase 3: E2EE Channel + Pairing Code Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give every direct (Bearer) connection an application-layer E2EE channel with a pinned host identity key, a `bibcode://pair` pairing-code format that distributes that key out of band, and a verify-then-add client flow with classified failures.

**Architecture:** The server owns a static X25519 keypair (`host_identity`) in the existing secret store and exposes a new `/ws-e2ee` WebSocket route that runs a `Noise_NK_25519_ChaChaPoly_SHA256` responder handshake, performs the entire credential exchange **inside** the encrypted channel (`e2ee_auth` with a one-time pairing token on first connect — the bootstrap exchange and device-session mint happen in-channel and the reply returns the bearer credential — or with the stored bearer on subsequent connects; no plaintext `/oauth/token` or ticket round-trips for hostKey targets), records the minted sessions as `transport: "e2ee"` so they are rejected on the plain `/ws` route and plain-HTTP bearer surfaces (no-downgrade), then runs the unchanged RPC session over encrypted frames. The pairing code (`bibcode://pair?code=<base64url(JSON)>`) carries endpoint, one-time pairing token, host public key, reach, and storage identity; a new authenticated HTTP endpoint `POST /api/auth/pairing-offer` mints it (joining the existing HTTP pairing surface: `/api/auth/pairing-token`, `/api/auth/pairing-links`). The client grows a pure-TypeScript Noise NK initiator (noble stack), an E2EE `Socket` wrapper, `hostKey` on saved Bearer profiles (channel-selection rule: hostKey → `/ws-e2ee`, legacy → `/ws` + "unencrypted" badge state), and a verify-then-add flow with five classified failures.

**Tech Stack:** Rust (Axum/Tokio, `snow` for Noise NK), TypeScript (`@noble/curves` x25519 + `@noble/ciphers` chacha20poly1305 + `@noble/hashes` sha256/hmac, effect `Socket`), effect/Schema contracts with TS↔Rust parity fixtures.

**Spec:** `docs/plans/remote-servers/remote-servers-spec.md` §4.1–§4.3 (normative names used verbatim). Master plan: `docs/plans/remote-servers/remote-servers-plan.md` (this file is Phase 3). Current-state survey: `docs/plans/remote-servers/bibcode-current-state.md` §§2–3. The reference-implementation research document lives in the same directory (its §§1–3, 9–10 ground the E2EE/pairing design).

## Global Constraints

(Copied verbatim from the master plan. Every task's requirements implicitly include this section.)

- Zero reference-product strings in code, identifiers, UI copy, or comments; product
  strings are "BiBCode"/"bibcode" by context (spec D16).
- `packages/contracts` stays schema-only; every new WS method gets a Rust mirror and an
  entry in the TS↔Rust parity manifests; every RPC method declares exactly one scope in
  `apps/server/src/auth/scope.rs`.
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

## Phase context and cross-phase interfaces

**Consumes from Phase 2** (`phases/phase-2-protocol-compat.md`): `REMOTE_PROTOCOL_VERSION`, `MIN_COMPATIBLE_REMOTE_PROTOCOL`, descriptor fields `remoteProtocolVersion` / `minCompatibleRemoteProtocol`, and the verdict from `packages/client-runtime/src/connection/compat.ts`. This plan does **not** redefine them. The verdict type is pinned by spec §4.4 as:

```ts
export type CompatVerdict =
  | { kind: "compatible" }
  | { kind: "legacy" } // server predates the window (both fields 0)
  | { kind: "server-too-old"; serverVersion: number; minSupported: number }
  | { kind: "client-too-old"; serverMinCompatible: number; clientVersion: number };
```

This plan calls the verdict-computing export `computeCompatVerdict(descriptor): CompatVerdict`. At implementation time, open `packages/client-runtime/src/connection/compat.ts` and use whatever export name Phase 2 actually landed for that computation; only the `CompatVerdict` shape above is normative.

**Produces for Phases 4/5:**

- `packages/contracts/src/remotePairing.ts` — `RemotePairingCodePayload`, `RemotePairingReach`, `E2eeAuthMessage` (union of the pairing and bearer forms), `E2eeAuthenticatedMessage` (optionally carrying `credential`/`environmentId`/`storageInstanceId` on the pairing form), `E2eeErrorMessage`; `packages/contracts/src/auth.ts` — `AuthCreatePairingOfferInput`, `AuthPairingOfferResult`.
- `packages/shared/src/pairingCode.ts` — `encodePairingCode`, `parsePairingCode`, `buildPairingDeepLink`, `buildBrowserPairUrl`.
- `packages/shared/src/advertisedEndpoint.ts` — `classifyPairingEndpoint`.
- HTTP endpoint `POST /api/auth/pairing-offer` (scope `access:write`, idempotent via the `Idempotency-Key` request header) — declared as `HttpApiEndpoint.post("pairingOffer", "/api/auth/pairing-offer", ...)` in `packages/contracts/src/environmentHttp.ts`, handled by `create_pairing_offer` in `apps/server/src/auth/http.rs`. Phase 5 **modifies** this endpoint (reach persistence via `issue_share_pairing`, exposure derivation); Phase 3 creates it.
- `hostKey` on `BearerConnectionProfile` (decode-default `null`); `PreparedConnection.e2ee` (`{ hostKey, auth: pairing | bearer }`); `RpcSession.e2eeAuthenticated` for reading the in-channel bootstrap result.
- Server-side no-downgrade enforcement: auth sessions carry `transport` (`"plain" | "e2ee"`); e2ee-minted sessions are rejected on `/ws` and plain-HTTP bearer surfaces.
- `ConnectionOnboarding.verifyAndAddPairingCode` with failure classification `unreachable | host-identity-mismatch | pairing-rejected | incompatible | duplicate-storage-identity` plus the distinct `PairingLoopbackAcknowledgementRequiredError` for the tunnel-acknowledgement UI.
- `connectionTransportSecurity(entry)` presentation helper (`"local" | "e2ee" | "channel-secured" | "unencrypted"`) for the "unencrypted" badge.

**Known spec deviation (resolved in Task 15).** Spec §4.3 says each WS binary message's plaintext is "exactly the bytes the plain `/ws` socket would carry". Noise caps one message at 65,535 bytes (ciphertext, including the 16-byte AEAD tag), while RPC messages (e.g. `projects.readFile` responses) routinely exceed that. This plan therefore uses a 1-byte record header inside every Noise transport plaintext (`0x00` final / `0x01` continuation) and preserves the invariant in concatenated form: _the concatenation of a logical message's record chunks is exactly the bytes the plain `/ws` socket would carry_. One WS binary message is still exactly one Noise message. Spec §4.3 was already amended to this record framing during plan review (2026-08-27); Task 15 verifies the spec text matches rather than amending it. Both handshake messages and the empty-prologue choice are unaffected.

**Wire summary (both sides must match; constants are asserted in tests):**

| item                      | value                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| ------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Noise protocol            | `Noise_NK_25519_ChaChaPoly_SHA256`, empty prologue, pre-message `<- s` = host identity public key                                                                                                                                                                                                                                                                                                                                             |
| Message 1 (client→server) | NK `-> e, es`, **empty handshake payload enforced** (non-empty → protocol violation, close), one WS binary frame                                                                                                                                                                                                                                                                                                                              |
| Message 2 (server→client) | NK `<- e, ee`, **empty handshake payload enforced** (non-empty → protocol violation, close), one WS binary frame                                                                                                                                                                                                                                                                                                                              |
| Wrong pinned key          | responder cannot decrypt Message 1 → closes with **WS close code 4403**, never sends Message 2; initiator maps close-4403 or an AEAD failure on Message 2 to `host-identity-mismatch`                                                                                                                                                                                                                                                         |
| Transport frame           | one WS binary frame = one Noise transport ciphertext, ≤ 65535 bytes                                                                                                                                                                                                                                                                                                                                                                           |
| Record plaintext          | `flag byte (0x00 final / 0x01 continuation)` ++ chunk (chunk ≤ 65518 bytes)                                                                                                                                                                                                                                                                                                                                                                   |
| Logical message cap       | pre-auth (`e2ee_auth`): **64 KiB** reassembled; post-auth: 64 MiB reassembled; violation → close                                                                                                                                                                                                                                                                                                                                              |
| First transport message   | client→server, one of `{"type":"e2ee_auth","pairing":"<one-time pairing token>"}` (first connect; server performs the bootstrap exchange **inside the channel**) or `{"type":"e2ee_auth","bearer":"<stored access credential>"}` (subsequent connects). **No `/oauth/token` or WebSocket-ticket HTTP round-trips for hostKey targets**; the only pre-auth HTTP is the unauthenticated descriptor fetch (routing hint, re-verified in-channel) |
| Server replies            | pairing form: `{"type":"e2ee_authenticated","credential":"<bearer>","environmentId":…,"storageInstanceId":…}`; bearer form: `{"type":"e2ee_authenticated"}`; failure: `{"type":"e2ee_error","code":"unauthorized"                                                                                                                                                                                                                             | "protocol"}` then close. The one-time token is consumed only by a successful in-channel exchange (pre-auth failures leave it retryable) |
| No-downgrade rule         | sessions minted through `/ws-e2ee` are recorded `transport: "e2ee"` and rejected by the plain `/ws` route and plain-HTTP bearer surfaces                                                                                                                                                                                                                                                                                                      |
| Handshake+auth deadline   | **one combined 10 s deadline** from upgrade to `e2ee_authenticated`; exceeded → close                                                                                                                                                                                                                                                                                                                                                         |
| Pre-auth connection cap   | unauthenticated in-flight `/ws-e2ee` connections capped (32); over cap → immediate close 1013                                                                                                                                                                                                                                                                                                                                                 |
| Outbound writer policy    | mirrors the plain RPC session: 5 s per-write timeout, 1 s pump-join timeout then abort                                                                                                                                                                                                                                                                                                                                                        |
| Nonce policy              | no rekey in v1; connection-lifetime bound is the Noise 2^64−1 counter (unreachable in practice: >584,000 years at 1M msgs/s); counter overflow or any AEAD failure closes the connection                                                                                                                                                                                                                                                      |
| hostKey encoding          | base64url, unpadded, of the raw 32 public-key bytes (spec §4.1)                                                                                                                                                                                                                                                                                                                                                                               |

---

### Task 1: Verify and pin the crypto dependencies

The spec mandates `snow` (Rust) and the noble stack (TS) but requires re-verifying current library status before locking versions. Do that first so every later task compiles against known-good APIs.

**Files:**

- Modify: `Cargo.toml` (workspace `[workspace.dependencies]`)
- Modify: `apps/server/Cargo.toml` (`[dependencies]`)
- Modify: `pnpm-workspace.yaml` (catalog)
- Modify: `packages/client-runtime/package.json` (`dependencies`)

**Interfaces:**

- Consumes: nothing.
- Produces: `snow` available to `bibcode-server`; `@noble/curves`, `@noble/ciphers`, `@noble/hashes` available to `@bibcode/client-runtime`. Later tasks import `snow::Builder`, `x25519` from `@noble/curves/ed25519.js`, `chacha20poly1305` from `@noble/ciphers/chacha.js`, `sha256` from `@noble/hashes/sha2.js`, `hmac` from `@noble/hashes/hmac.js`.

- [x] **Step 1: Verify current library versions and APIs (do not trust memory)**

Run and record the outputs:

```bash
cargo search snow --limit 3
pnpm view @noble/ciphers version
pnpm view @noble/curves version
pnpm view @noble/hashes version
```

The catalog already pins `@noble/curves: 2.2.0` and `@noble/hashes: 2.2.0` (`pnpm-workspace.yaml` lines 28–29); keep those pins. Pick the current stable 2.x of `@noble/ciphers` (align its major with curves/hashes 2.x). For `snow`, pick the newest non-yanked release (expected `0.10.x`; if only `0.9.x` is current, use that and adjust the builder calls in Tasks 2/4/5 — in `0.9.x` `Builder::local_private_key` returns `Builder`, in `0.10.x` it returns `Result`).

Then verify the exact import paths and call signatures against the installed packages (open the files under `node_modules` after Step 2, and `cargo doc -p snow --no-deps` or docs.rs for snow):

- `@noble/curves/ed25519.js` must export `x25519` with `x25519.getPublicKey(priv)`, `x25519.getSharedSecret(priv, pub)`, and `x25519.utils.randomSecretKey()` (older releases call it `randomPrivateKey`; use whichever the installed version exports).
- `@noble/ciphers/chacha.js` must export `chacha20poly1305(key, nonce12, aad?)` returning `{ encrypt(bytes), decrypt(bytes) }` where `decrypt` throws on tag failure.
- `@noble/hashes/sha2.js` exports `sha256`; `@noble/hashes/hmac.js` exports `hmac(sha256, key, data)`.
- `snow`: `Builder::new("Noise_NK_25519_ChaChaPoly_SHA256".parse()?)`, `generate_keypair()`, `local_private_key(..)`, `build_responder()`/`build_initiator()`, `HandshakeState::{read_message, write_message, into_transport_mode}`, `TransportState::{read_message, write_message}`, and the fixed-ephemeral builder hook for tests (`fixed_ephemeral_key_for_testing_only`, behind snow's `default-resolver` — confirm the feature/name for the pinned version).

If any import path or signature differs, adjust the code in Tasks 2, 4, 5, 9 accordingly when you reach them — the shapes in this plan reflect the noble 2.x / snow 0.10 line.

- [x] **Step 2: Add the dependencies**

`Cargo.toml` (workspace deps, alphabetical position near `sha2`/`subtle`):

```toml
snow = "0.10"
```

`apps/server/Cargo.toml` (alphabetical in `[dependencies]`):

```toml
snow.workspace = true
```

`pnpm-workspace.yaml` catalog (next to the existing noble pins, using the version verified in Step 1):

```yaml
"@noble/ciphers": <verified 2.x version>
```

`packages/client-runtime/package.json` dependencies:

```json
    "@noble/ciphers": "catalog:",
    "@noble/curves": "catalog:",
    "@noble/hashes": "catalog:",
```

- [x] **Step 3: Install and compile**

Run: `pnpm install` then `cargo check -p bibcode-server`
Expected: both succeed; lockfiles update; no unused-dependency warnings yet (snow is unused until Task 2 — if the workspace lint denies unused deps, note it and fold this step's Cargo edits into Task 2's commit instead).

- [x] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock apps/server/Cargo.toml pnpm-workspace.yaml pnpm-lock.yaml packages/client-runtime/package.json
git commit -m "build(e2ee): pin snow and noble crypto dependencies"
```

---

### Task 2: Host identity key (server, spec §4.1)

The server owns a static X25519 keypair, `host_identity`, generated on first use and stored via the existing secret store. Public key encoding everywhere: base64url, unpadded, of the raw 32 bytes. Distributed only inside pairing codes.

**Files:**

- Create: `apps/server/src/auth/host_identity.rs`
- Modify: `apps/server/src/auth/mod.rs` (declare + re-export module)
- Modify: `apps/server/src/auth/service.rs` (field + accessor + load in `new_with_persistence`)

**Interfaces:**

- Consumes: `SecretStore` (`apps/server/src/auth/secret_store.rs`) — `get`, `create` (create-new semantics with `is_already_exists()` race classification, exactly as `get_or_create_random` uses them).
- Produces: `pub(crate) const NOISE_NK_PARAMS: &str = "Noise_NK_25519_ChaChaPoly_SHA256";`, `pub struct HostIdentity` with `load_or_generate(&SecretStore) -> Result<Self, HostIdentityError>`, `generate_ephemeral() -> Self`, `public_key_base64url(&self) -> String`, `private_key_bytes(&self) -> &[u8; 32]`, `public_key_bytes(&self) -> &[u8; 32]`; `AuthService::host_identity(&self) -> &HostIdentity`. Tasks 5 and 8 consume these.

- [x] **Step 1: Write the failing tests**

In `apps/server/src/auth/host_identity.rs` (module skeleton with `#[cfg(test)] mod tests` at the bottom; the non-test code comes in Step 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::secret_store::SecretStore;

    async fn test_store() -> (tempfile::TempDir, SecretStore) {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = SecretStore::new(dir.path().join("secrets"))
            .await
            .expect("secret store");
        (dir, store)
    }

    #[tokio::test]
    async fn generates_once_and_reloads_the_same_keypair() {
        let (_dir, store) = test_store().await;
        let first = HostIdentity::load_or_generate(&store).await.unwrap();
        let second = HostIdentity::load_or_generate(&store).await.unwrap();
        assert_eq!(first.public_key_bytes(), second.public_key_bytes());
        assert_eq!(first.private_key_bytes(), second.private_key_bytes());
    }

    #[tokio::test]
    async fn public_key_encoding_is_unpadded_base64url_of_32_bytes() {
        let identity = HostIdentity::generate_ephemeral();
        let encoded = identity.public_key_base64url();
        assert_eq!(encoded.len(), 43); // 32 bytes -> 43 base64url chars, no padding
        assert!(!encoded.contains('='));
        assert!(!encoded.contains('+') && !encoded.contains('/'));
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&encoded)
            .expect("decodes");
        assert_eq!(decoded.as_slice(), identity.public_key_bytes());
    }

    #[tokio::test]
    async fn persisted_record_is_private_then_public_64_bytes() {
        let (_dir, store) = test_store().await;
        let identity = HostIdentity::load_or_generate(&store).await.unwrap();
        let raw = store
            .get(HOST_IDENTITY_SECRET_NAME)
            .await
            .unwrap()
            .expect("secret exists");
        assert_eq!(raw.len(), 64);
        assert_eq!(&raw[..32], identity.private_key_bytes());
        assert_eq!(&raw[32..], identity.public_key_bytes());
    }

    #[tokio::test]
    async fn corrupt_record_is_reported_not_silently_regenerated() {
        let (_dir, store) = test_store().await;
        store
            .create(HOST_IDENTITY_SECRET_NAME, b"short")
            .await
            .unwrap();
        assert!(matches!(
            HostIdentity::load_or_generate(&store).await,
            Err(HostIdentityError::Corrupt { .. })
        ));
    }
}
```

Add to `apps/server/src/auth/mod.rs`: `mod host_identity;` and `pub(crate) use host_identity::{HostIdentity, HostIdentityError, HOST_IDENTITY_SECRET_NAME, NOISE_NK_PARAMS};` (match the file's existing `pub(crate) use` style).

- [x] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p bibcode-server host_identity`
Expected: compile error (types not defined) — that counts as the red state; proceed.

- [x] **Step 3: Write the implementation**

```rust
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use thiserror::Error;

use super::secret_store::{SecretStore, SecretStoreError};

pub(crate) const NOISE_NK_PARAMS: &str = "Noise_NK_25519_ChaChaPoly_SHA256";
pub(crate) const HOST_IDENTITY_SECRET_NAME: &str = "host-identity-x25519";

#[derive(Debug, Error)]
pub enum HostIdentityError {
    #[error("failed to access the host identity secret")]
    Store(#[from] SecretStoreError),
    #[error("failed to generate the host identity keypair: {0}")]
    Generate(String),
    #[error("host identity record at {name:?} has {len} bytes; expected 64")]
    Corrupt { name: &'static str, len: usize },
    #[error("host identity secret disappeared after a concurrent creator won the race")]
    ConcurrentRead,
}

/// The server's static Noise NK responder keypair (spec section 4.1).
/// The public key is distributed only inside pairing codes.
#[derive(Clone)]
pub struct HostIdentity {
    private: [u8; 32],
    public: [u8; 32],
}

impl std::fmt::Debug for HostIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HostIdentity")
            .field("public", &self.public_key_base64url())
            .finish_non_exhaustive()
    }
}

impl HostIdentity {
    pub(crate) async fn load_or_generate(store: &SecretStore) -> Result<Self, HostIdentityError> {
        if let Some(existing) = store.get(HOST_IDENTITY_SECRET_NAME).await? {
            return Self::from_record(&existing);
        }
        let generated = Self::generate()?;
        let mut record = [0_u8; 64];
        record[..32].copy_from_slice(&generated.private);
        record[32..].copy_from_slice(&generated.public);
        match store.create(HOST_IDENTITY_SECRET_NAME, &record).await {
            Ok(()) => Ok(generated),
            Err(error) if error.is_already_exists() => store
                .get(HOST_IDENTITY_SECRET_NAME)
                .await?
                .ok_or(HostIdentityError::ConcurrentRead)
                .and_then(|winner| Self::from_record(&winner)),
            Err(error) => Err(error.into()),
        }
    }

    /// Test-mode and `AuthService::new`-without-persistence identity.
    pub(crate) fn generate_ephemeral() -> Self {
        Self::generate().expect("X25519 keypair generation cannot fail")
    }

    fn generate() -> Result<Self, HostIdentityError> {
        let keypair = snow::Builder::new(
            NOISE_NK_PARAMS
                .parse()
                .map_err(|error| HostIdentityError::Generate(format!("{error:?}")))?,
        )
        .generate_keypair()
        .map_err(|error| HostIdentityError::Generate(format!("{error:?}")))?;
        let mut private = [0_u8; 32];
        let mut public = [0_u8; 32];
        private.copy_from_slice(&keypair.private);
        public.copy_from_slice(&keypair.public);
        Ok(Self { private, public })
    }

    fn from_record(record: &[u8]) -> Result<Self, HostIdentityError> {
        if record.len() != 64 {
            return Err(HostIdentityError::Corrupt {
                name: HOST_IDENTITY_SECRET_NAME,
                len: record.len(),
            });
        }
        let mut private = [0_u8; 32];
        let mut public = [0_u8; 32];
        private.copy_from_slice(&record[..32]);
        public.copy_from_slice(&record[32..]);
        Ok(Self { private, public })
    }

    #[must_use]
    pub fn public_key_base64url(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.public)
    }

    #[must_use]
    pub(crate) fn private_key_bytes(&self) -> &[u8; 32] {
        &self.private
    }

    #[must_use]
    pub(crate) fn public_key_bytes(&self) -> &[u8; 32] {
        &self.public
    }
}
```

Wire into `AuthService` (`apps/server/src/auth/service.rs`):

1. Add field `host_identity: HostIdentity` to `struct AuthService`.
2. `new_with_persistence`: before calling `Self::build(...)`, run `let host_identity = HostIdentity::load_or_generate(&secret_store).await.map_err(|error| AuthError::Internal(error.to_string()))?;` and pass it through (change `build`'s signature to take `host_identity: HostIdentity`).
3. The `#[cfg(test)] new` constructor passes `HostIdentity::generate_ephemeral()`.
4. Add accessor:

```rust
#[must_use]
pub fn host_identity(&self) -> &HostIdentity {
    &self.host_identity
}
```

- [x] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p bibcode-server host_identity && cargo test -p bibcode-server auth`
Expected: PASS (including all existing auth tests — the field addition must not disturb them).

- [x] **Step 5: Commit**

```bash
git add apps/server/src/auth/host_identity.rs apps/server/src/auth/mod.rs apps/server/src/auth/service.rs
git commit -m "feat(server): persist a static X25519 host identity keypair"
```

---

### Task 3: Split-transport refactor of `run_session` (server, pure refactor)

`run_session` (`apps/server/src/rpc/session.rs:314`) takes a concrete `axum::extract::ws::WebSocket` and immediately `.split()`s it. The E2EE route needs to feed the same session loop through an encrypt/decrypt pump. Refactor first, behind existing tests, so Task 5 builds on a proven seam.

**Files:**

- Modify: `apps/server/src/rpc/session.rs` (`run_session` → thin wrapper over new `run_session_split`)
- Modify: `apps/server/src/rpc/mod.rs` (export `run_session_split`)

**Interfaces:**

- Consumes: existing `run_session(socket, registry, context, session_shutdown)` call sites in `apps/server/src/http.rs:209,243` (unchanged).
- Produces:

```rust
pub(crate) async fn run_session_split<W, R>(
    socket_writer: W,
    socket_reader: R,
    registry: RpcRegistry,
    context: RpcSessionContext,
    session_shutdown: CancellationToken,
) where
    W: Sink<Message> + Unpin + Send + 'static,
    W::Error: Send,
    R: Stream<Item = Result<Message, axum::Error>> + Send,
```

Task 5 consumes `run_session_split` with channel-backed halves.

- [x] **Step 1: Establish the green baseline**

Run: `cargo test -p bibcode-server --test activity_rpc && cargo test -p bibcode-server rpc`
Expected: PASS. Record the passing test names — they are the safety net for this refactor (no new tests are added here; a pure refactor is proven by existing coverage).

- [x] **Step 2: Refactor**

In `apps/server/src/rpc/session.rs`:

```rust
pub(crate) async fn run_session(
    socket: WebSocket,
    registry: RpcRegistry,
    context: RpcSessionContext,
    session_shutdown: CancellationToken,
) {
    let (socket_writer, socket_reader) = socket.split();
    run_session_split(socket_writer, socket_reader, registry, context, session_shutdown).await;
}

pub(crate) async fn run_session_split<W, R>(
    socket_writer: W,
    socket_reader: R,
    registry: RpcRegistry,
    context: RpcSessionContext,
    session_shutdown: CancellationToken,
) where
    W: Sink<Message> + Unpin + Send + 'static,
    W::Error: Send,
    R: Stream<Item = Result<Message, axum::Error>> + Send,
{
    let mut socket_writer = socket_writer;
    let socket_reader = socket_reader;
    let mut socket_reader = std::pin::pin!(socket_reader);
    // ... the existing body of run_session from the (mut socket_writer, mut socket_reader)
    // bindings onward, textually unchanged except:
    //   - `socket_reader.next()` becomes `socket_reader.as_mut().next()` if the pinned
    //     stream needs it (SplitStream is Unpin; `pin!` keeps generality),
    //   - the writer task's send/close already only relies on SinkExt::{send, close},
    //     which the generic bound provides.
}
```

Move the entire existing body (writer task, in-flight map, dispatch loop, drain) into `run_session_split` unchanged. Do not alter behavior: same `OUTBOUND_CAPACITY`, same `SOCKET_WRITE_TIMEOUT`, same `matches!(timeout(...), Ok(Ok(())))` write checks (these compile unchanged under the generic `W`).

- [x] **Step 3: Run the baseline tests again**

Run: `cargo test -p bibcode-server --test activity_rpc && cargo test -p bibcode-server rpc && cargo clippy -p bibcode-server --all-targets -- -D warnings`
Expected: PASS with zero behavior diffs.

- [x] **Step 4: Commit**

```bash
git add apps/server/src/rpc/session.rs apps/server/src/rpc/mod.rs
git commit -m "refactor(server): run the RPC session over generic transport halves"
```

---

### Task 4: E2EE record layer and Noise responder session (server)

The encrypted-channel engine: record framing (flag byte + chunk), the snow responder handshake, transport encrypt/decrypt with limits, and the control-message types. Pure module, unit-tested against a snow _initiator_ in the same tests (snow↔snow), so Task 5 only wires it to Axum.

**Files:**

- Create: `apps/server/src/rpc/e2ee.rs`
- Modify: `apps/server/src/rpc/mod.rs` (`mod e2ee;` + re-exports used by http.rs)
- Modify: `Cargo.toml` / `apps/server/Cargo.toml` only if `tokio-util` needs the `sync` feature (Task 5 uses `PollSender`; add `"sync"` to the workspace `tokio-util` features list now: `tokio-util = { version = "0.7.18", features = ["io", "rt", "sync"] }`)

**Interfaces:**

- Consumes: `NOISE_NK_PARAMS`, `HostIdentity` (Task 2).
- Produces (consumed by Task 5):

```rust
pub(crate) const MAX_E2EE_CIPHERTEXT_BYTES: usize = 65_535;
pub(crate) const E2EE_RECORD_FLAG_FINAL: u8 = 0x00;
pub(crate) const E2EE_RECORD_FLAG_CONTINUATION: u8 = 0x01;
pub(crate) const MAX_E2EE_CHUNK_BYTES: usize = 65_518; // 65535 - 16 (tag) - 1 (flag)
pub(crate) const MAX_E2EE_LOGICAL_MESSAGE_BYTES: usize = 64 * 1024 * 1024; // post-auth cap
pub(crate) const MAX_E2EE_PREAUTH_MESSAGE_BYTES: usize = 64 * 1024;        // e2ee_auth cap
pub(crate) const E2EE_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10); // upgrade -> e2ee_authenticated
pub(crate) const E2EE_HOST_IDENTITY_CLOSE_CODE: u16 = 4403; // wrong pinned key
pub(crate) const E2EE_MAX_PREAUTH_CONNECTIONS: usize = 32;

pub(crate) enum E2eeSessionError { /* Handshake, Protocol(String), Crypto(String), Closed, Timeout */ }

pub(crate) struct E2eeChannel { /* snow::TransportState + reassembly state */ }
impl E2eeChannel {
    /// Runs NK messages 1+2 over the provided frame IO. Returns the transport
    /// channel. Message 1 must carry an EMPTY handshake payload (non-empty ->
    /// Protocol); an undecryptable Message 1 (wrong pinned key) -> Handshake,
    /// which the route closes with code 4403. Message 2 is sent with an empty
    /// payload.
    pub(crate) async fn respond<Rx, Tx, RxFut, TxFut>(
        host_identity: &HostIdentity, recv_binary_frame: Rx, send_binary_frame: Tx,
    ) -> Result<Self, E2eeSessionError>
    where Rx: FnMut() -> RxFut, RxFut: Future<Output = Option<Vec<u8>>>,
          Tx: FnMut(Vec<u8>) -> TxFut, TxFut: Future<Output = Result<(), E2eeSessionError>>;
    /// Encrypts one logical plaintext into >= 1 wire frames.
    pub(crate) fn encrypt_message(&mut self, plaintext: &[u8]) -> Result<Vec<Vec<u8>>, E2eeSessionError>;
    /// Decrypts one wire frame against a caller-supplied reassembly cap
    /// (MAX_E2EE_PREAUTH_MESSAGE_BYTES before auth, MAX_E2EE_LOGICAL_MESSAGE_BYTES
    /// after); Some(message) when the final record completes a logical message.
    pub(crate) fn decrypt_frame(
        &mut self, frame: &[u8], max_message_bytes: usize,
    ) -> Result<Option<Vec<u8>>, E2eeSessionError>;
}

// Two auth forms (spec section 4.3): pairing (first connect, in-channel bootstrap)
// or bearer (subsequent connects). Exactly one of the two fields is set.
#[derive(Deserialize)]
pub(crate) struct E2eeAuthMessage {
    pub r#type: String,
    #[serde(default)] pub pairing: Option<String>,
    #[serde(default)] pub bearer: Option<String>,
}
pub(crate) fn e2ee_authenticated_json() -> Vec<u8>; // bearer form: {"type":"e2ee_authenticated"}
pub(crate) fn e2ee_authenticated_with_credential_json(
    credential: &str, environment_id: &str, storage_instance_id: &str,
) -> Vec<u8>; // pairing form reply
pub(crate) fn e2ee_error_json(code: &str) -> Vec<u8>; // {"type":"e2ee_error","code":code}
```

- [x] **Step 1: Write the failing tests**

At the bottom of `apps/server/src/rpc/e2ee.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::HostIdentity;

    struct SnowInitiator {
        transport: snow::TransportState,
    }

    /// Drives a raw snow initiator against `E2eeChannel::respond`, exchanging
    /// frames through in-memory queues.
    async fn establish() -> (SnowInitiator, E2eeChannel) {
        let identity = HostIdentity::generate_ephemeral();
        let mut initiator = snow::Builder::new(crate::auth::NOISE_NK_PARAMS.parse().unwrap())
            .remote_public_key(identity.public_key_bytes())
            .unwrap()
            .build_initiator()
            .unwrap();
        let mut message_a = vec![0_u8; MAX_E2EE_CIPHERTEXT_BYTES];
        let len_a = initiator.write_message(&[], &mut message_a).unwrap();
        message_a.truncate(len_a);

        let (to_responder, mut responder_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
        let (to_initiator, mut initiator_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
        to_responder.send(message_a).await.unwrap();
        let responder = E2eeChannel::respond(
            &identity,
            || async { responder_rx.recv().await },
            |frame| {
                let sender = to_initiator.clone();
                async move {
                    sender.send(frame).await.map_err(|_| E2eeSessionError::Closed)
                }
            },
        )
        .await
        .unwrap();
        let message_b = initiator_rx.recv().await.unwrap();
        let mut payload = vec![0_u8; MAX_E2EE_CIPHERTEXT_BYTES];
        let len = initiator.read_message(&message_b, &mut payload).unwrap();
        assert_eq!(len, 0, "message B carries an empty handshake payload");
        let transport = initiator.into_transport_mode().unwrap();
        (SnowInitiator { transport }, responder)
    }

    fn initiator_encrypt(initiator: &mut SnowInitiator, records: &[Vec<u8>]) -> Vec<Vec<u8>> {
        records
            .iter()
            .map(|record| {
                let mut frame = vec![0_u8; MAX_E2EE_CIPHERTEXT_BYTES];
                let len = initiator.transport.write_message(record, &mut frame).unwrap();
                frame.truncate(len);
                frame
            })
            .collect()
    }

    fn record(flag: u8, chunk: &[u8]) -> Vec<u8> {
        let mut record = Vec::with_capacity(1 + chunk.len());
        record.push(flag);
        record.extend_from_slice(chunk);
        record
    }

    #[tokio::test]
    async fn small_round_trip_uses_a_single_final_record() {
        let (mut initiator, mut responder) = establish().await;
        // client -> server
        let frames = initiator_encrypt(
            &mut initiator,
            &[record(E2EE_RECORD_FLAG_FINAL, b"{\"hello\":true}")],
        );
        assert_eq!(frames.len(), 1);
        let message = responder
            .decrypt_frame(&frames[0], MAX_E2EE_LOGICAL_MESSAGE_BYTES)
            .unwrap()
            .unwrap();
        assert_eq!(message, b"{\"hello\":true}");
        // server -> client
        let frames = responder.encrypt_message(b"{\"ok\":1}").unwrap();
        assert_eq!(frames.len(), 1);
        let mut plaintext = vec![0_u8; MAX_E2EE_CIPHERTEXT_BYTES];
        let len = initiator.transport.read_message(&frames[0], &mut plaintext).unwrap();
        assert_eq!(&plaintext[..len], record(E2EE_RECORD_FLAG_FINAL, b"{\"ok\":1}").as_slice());
    }

    #[tokio::test]
    async fn large_messages_fragment_and_reassemble() {
        let (mut initiator, mut responder) = establish().await;
        let big = vec![b'a'; MAX_E2EE_CHUNK_BYTES * 2 + 5];
        let frames = responder.encrypt_message(&big).unwrap();
        assert_eq!(frames.len(), 3);
        for frame in &frames {
            assert!(frame.len() <= MAX_E2EE_CIPHERTEXT_BYTES);
        }
        // Reassemble on the initiator side to prove flags are correct.
        let mut assembled = Vec::new();
        for (index, frame) in frames.iter().enumerate() {
            let mut plaintext = vec![0_u8; MAX_E2EE_CIPHERTEXT_BYTES];
            let len = initiator.transport.read_message(frame, &mut plaintext).unwrap();
            let expected_flag = if index == frames.len() - 1 {
                E2EE_RECORD_FLAG_FINAL
            } else {
                E2EE_RECORD_FLAG_CONTINUATION
            };
            assert_eq!(plaintext[0], expected_flag);
            assembled.extend_from_slice(&plaintext[1..len]);
        }
        assert_eq!(assembled, big);
    }

    #[tokio::test]
    async fn oversized_ciphertext_frames_are_rejected_before_decryption() {
        let (_initiator, mut responder) = establish().await;
        let oversized = vec![0_u8; MAX_E2EE_CIPHERTEXT_BYTES + 1];
        assert!(matches!(
            responder.decrypt_frame(&oversized, MAX_E2EE_LOGICAL_MESSAGE_BYTES),
            Err(E2eeSessionError::Protocol(_))
        ));
    }

    #[tokio::test]
    async fn reassembly_respects_the_caller_supplied_cap() {
        // The per-call cap is what lets the route enforce 64 KiB before auth and
        // 64 MiB after: exceed a small cap and the channel fails closed.
        let (mut initiator, mut responder) = establish().await;
        let continuation = record(E2EE_RECORD_FLAG_CONTINUATION, &vec![0_u8; MAX_E2EE_CHUNK_BYTES]);
        let frames = initiator_encrypt(&mut initiator, &[continuation.clone()]);
        assert!(matches!(
            responder.decrypt_frame(&frames[0], MAX_E2EE_PREAUTH_MESSAGE_BYTES),
            Err(E2eeSessionError::Protocol(_))
        ));
    }

    #[tokio::test]
    async fn logical_message_overflow_is_rejected() {
        let (mut initiator, mut responder) = establish().await;
        let continuation = record(E2EE_RECORD_FLAG_CONTINUATION, &vec![0_u8; MAX_E2EE_CHUNK_BYTES]);
        let needed = MAX_E2EE_LOGICAL_MESSAGE_BYTES / MAX_E2EE_CHUNK_BYTES + 1;
        let mut overflowed = false;
        for _ in 0..=needed {
            let frames = initiator_encrypt(&mut initiator, &[continuation.clone()]);
            match responder.decrypt_frame(&frames[0], MAX_E2EE_LOGICAL_MESSAGE_BYTES) {
                Ok(None) => {}
                Err(E2eeSessionError::Protocol(_)) => {
                    overflowed = true;
                    break;
                }
                other => panic!("unexpected: {other:?}"),
            }
        }
        assert!(overflowed);
    }

    #[tokio::test]
    async fn tampered_frames_fail_closed() {
        let (mut initiator, mut responder) = establish().await;
        let mut frames =
            initiator_encrypt(&mut initiator, &[record(E2EE_RECORD_FLAG_FINAL, b"x")]);
        let last = frames[0].len() - 1;
        frames[0][last] ^= 0x01;
        assert!(matches!(
            responder.decrypt_frame(&frames[0], MAX_E2EE_LOGICAL_MESSAGE_BYTES),
            Err(E2eeSessionError::Crypto(_))
        ));
    }

    #[tokio::test]
    async fn non_empty_message_one_payload_is_a_protocol_violation() {
        let identity = HostIdentity::generate_ephemeral();
        let mut initiator = snow::Builder::new(crate::auth::NOISE_NK_PARAMS.parse().unwrap())
            .remote_public_key(identity.public_key_bytes())
            .unwrap()
            .build_initiator()
            .unwrap();
        let mut message_a = vec![0_u8; MAX_E2EE_CIPHERTEXT_BYTES];
        let len_a = initiator.write_message(b"sneaky", &mut message_a).unwrap();
        message_a.truncate(len_a);
        let (to_responder, mut responder_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
        to_responder.send(message_a).await.unwrap();
        let result = E2eeChannel::respond(
            &identity,
            || async { responder_rx.recv().await },
            |_frame| async { Ok(()) },
        )
        .await;
        assert!(matches!(result, Err(E2eeSessionError::Protocol(_))));
    }

    #[tokio::test]
    async fn wrong_pinned_key_fails_the_handshake() {
        let identity = HostIdentity::generate_ephemeral();
        let wrong = HostIdentity::generate_ephemeral();
        let mut initiator = snow::Builder::new(crate::auth::NOISE_NK_PARAMS.parse().unwrap())
            .remote_public_key(wrong.public_key_bytes())
            .unwrap()
            .build_initiator()
            .unwrap();
        let mut message_a = vec![0_u8; MAX_E2EE_CIPHERTEXT_BYTES];
        let len_a = initiator.write_message(&[], &mut message_a).unwrap();
        message_a.truncate(len_a);
        let (to_responder, mut responder_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
        to_responder.send(message_a).await.unwrap();
        let result = E2eeChannel::respond(
            &identity,
            || async { responder_rx.recv().await },
            |_frame| async { Ok(()) },
        )
        .await;
        assert!(matches!(result, Err(E2eeSessionError::Handshake)));
    }
}
```

- [x] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p bibcode-server e2ee`
Expected: compile failure (module absent) — red.

- [x] **Step 3: Write the implementation**

```rust
use std::{future::Future, time::Duration};

use serde::Deserialize;
use serde_json::json;

use crate::auth::{HostIdentity, NOISE_NK_PARAMS};

pub(crate) const MAX_E2EE_CIPHERTEXT_BYTES: usize = 65_535;
const NOISE_TAG_BYTES: usize = 16;
pub(crate) const E2EE_RECORD_FLAG_FINAL: u8 = 0x00;
pub(crate) const E2EE_RECORD_FLAG_CONTINUATION: u8 = 0x01;
pub(crate) const MAX_E2EE_CHUNK_BYTES: usize =
    MAX_E2EE_CIPHERTEXT_BYTES - NOISE_TAG_BYTES - 1;
pub(crate) const MAX_E2EE_LOGICAL_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const MAX_E2EE_PREAUTH_MESSAGE_BYTES: usize = 64 * 1024;
/// One deadline covering upgrade -> handshake -> e2ee_authenticated.
pub(crate) const E2EE_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Wrong pinned key: the responder cannot decrypt Message 1 and closes with this code.
pub(crate) const E2EE_HOST_IDENTITY_CLOSE_CODE: u16 = 4403;
pub(crate) const E2EE_MAX_PREAUTH_CONNECTIONS: usize = 32;

#[derive(Debug)]
pub(crate) enum E2eeSessionError {
    /// Message A could not be processed with the local static key: either the
    /// peer pinned a different host key or the frame is not a Noise NK message.
    Handshake,
    /// Framing violation (oversized frame, empty record, reassembly overflow, ...).
    Protocol(String),
    /// AEAD failure or nonce exhaustion after the handshake.
    Crypto(String),
    /// Peer closed / frame source drained.
    Closed,
    /// Handshake or auth deadline exceeded (used by the route in Task 5).
    Timeout,
}

pub(crate) struct E2eeChannel {
    transport: snow::TransportState,
    assembling: Vec<u8>,
}

impl E2eeChannel {
    pub(crate) async fn respond<Rx, Tx, RxFut, TxFut>(
        host_identity: &HostIdentity,
        mut recv_binary_frame: Rx,
        mut send_binary_frame: Tx,
    ) -> Result<Self, E2eeSessionError>
    where
        Rx: FnMut() -> RxFut,
        RxFut: Future<Output = Option<Vec<u8>>>,
        Tx: FnMut(Vec<u8>) -> TxFut,
        TxFut: Future<Output = Result<(), E2eeSessionError>>,
    {
        let params = NOISE_NK_PARAMS
            .parse()
            .map_err(|error| E2eeSessionError::Protocol(format!("noise params: {error:?}")))?;
        let mut responder = snow::Builder::new(params)
            .local_private_key(host_identity.private_key_bytes())
            .map_err(|error| E2eeSessionError::Protocol(format!("local key: {error:?}")))?
            .build_responder()
            .map_err(|error| E2eeSessionError::Protocol(format!("responder: {error:?}")))?;

        let message_a = recv_binary_frame().await.ok_or(E2eeSessionError::Closed)?;
        if message_a.len() > MAX_E2EE_CIPHERTEXT_BYTES {
            return Err(E2eeSessionError::Protocol("oversized handshake frame".into()));
        }
        let mut payload = vec![0_u8; MAX_E2EE_CIPHERTEXT_BYTES];
        let payload_len = responder
            .read_message(&message_a, &mut payload)
            .map_err(|_| E2eeSessionError::Handshake)?;
        if payload_len != 0 {
            // Spec section 4.3: handshake payloads must be empty.
            return Err(E2eeSessionError::Protocol(
                "message 1 carried a non-empty handshake payload".into(),
            ));
        }

        let mut message_b = vec![0_u8; MAX_E2EE_CIPHERTEXT_BYTES];
        let len = responder
            .write_message(&[], &mut message_b)
            .map_err(|error| E2eeSessionError::Crypto(format!("message B: {error:?}")))?;
        message_b.truncate(len);
        send_binary_frame(message_b).await?;

        let transport = responder
            .into_transport_mode()
            .map_err(|error| E2eeSessionError::Crypto(format!("transport: {error:?}")))?;
        Ok(Self {
            transport,
            assembling: Vec::new(),
        })
    }

    pub(crate) fn encrypt_message(
        &mut self,
        plaintext: &[u8],
    ) -> Result<Vec<Vec<u8>>, E2eeSessionError> {
        if plaintext.len() > MAX_E2EE_LOGICAL_MESSAGE_BYTES {
            return Err(E2eeSessionError::Protocol("outbound message too large".into()));
        }
        let mut frames = Vec::new();
        let mut chunks = plaintext.chunks(MAX_E2EE_CHUNK_BYTES).peekable();
        // An empty logical message still produces one final record.
        if chunks.peek().is_none() {
            frames.push(self.encrypt_record(E2EE_RECORD_FLAG_FINAL, &[])?);
            return Ok(frames);
        }
        while let Some(chunk) = chunks.next() {
            let flag = if chunks.peek().is_none() {
                E2EE_RECORD_FLAG_FINAL
            } else {
                E2EE_RECORD_FLAG_CONTINUATION
            };
            frames.push(self.encrypt_record(flag, chunk)?);
        }
        Ok(frames)
    }

    fn encrypt_record(&mut self, flag: u8, chunk: &[u8]) -> Result<Vec<u8>, E2eeSessionError> {
        let mut record = Vec::with_capacity(1 + chunk.len());
        record.push(flag);
        record.extend_from_slice(chunk);
        let mut frame = vec![0_u8; record.len() + NOISE_TAG_BYTES];
        let len = self
            .transport
            .write_message(&record, &mut frame)
            .map_err(|error| E2eeSessionError::Crypto(format!("encrypt: {error:?}")))?;
        frame.truncate(len);
        Ok(frame)
    }

    pub(crate) fn decrypt_frame(
        &mut self,
        frame: &[u8],
        max_message_bytes: usize,
    ) -> Result<Option<Vec<u8>>, E2eeSessionError> {
        if frame.len() > MAX_E2EE_CIPHERTEXT_BYTES {
            return Err(E2eeSessionError::Protocol("oversized frame".into()));
        }
        let mut record = vec![0_u8; MAX_E2EE_CIPHERTEXT_BYTES];
        let len = self
            .transport
            .read_message(frame, &mut record)
            .map_err(|error| E2eeSessionError::Crypto(format!("decrypt: {error:?}")))?;
        if len == 0 {
            return Err(E2eeSessionError::Protocol("empty record".into()));
        }
        let flag = record[0];
        let chunk = &record[1..len];
        if self
            .assembling
            .len()
            .saturating_add(chunk.len())
            > max_message_bytes
        {
            return Err(E2eeSessionError::Protocol("reassembly overflow".into()));
        }
        match flag {
            E2EE_RECORD_FLAG_CONTINUATION => {
                self.assembling.extend_from_slice(chunk);
                Ok(None)
            }
            E2EE_RECORD_FLAG_FINAL => {
                let mut message = std::mem::take(&mut self.assembling);
                message.extend_from_slice(chunk);
                Ok(Some(message))
            }
            other => Err(E2eeSessionError::Protocol(format!("unknown record flag {other}"))),
        }
    }
}

/// Two auth forms (spec section 4.3). Exactly one of `pairing` (first connect,
/// in-channel bootstrap) or `bearer` (subsequent connects) must be present.
#[derive(Debug, Deserialize)]
pub(crate) struct E2eeAuthMessage {
    pub r#type: String,
    #[serde(default)]
    pub pairing: Option<String>,
    #[serde(default)]
    pub bearer: Option<String>,
}

pub(crate) fn e2ee_authenticated_json() -> Vec<u8> {
    serde_json::to_vec(&json!({ "type": "e2ee_authenticated" })).expect("static JSON")
}

pub(crate) fn e2ee_authenticated_with_credential_json(
    credential: &str,
    environment_id: &str,
    storage_instance_id: &str,
) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "type": "e2ee_authenticated",
        "credential": credential,
        "environmentId": environment_id,
        "storageInstanceId": storage_instance_id,
    }))
    .expect("static JSON")
}

pub(crate) fn e2ee_error_json(code: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({ "type": "e2ee_error", "code": code })).expect("static JSON")
}
```

Add `mod e2ee;` to `apps/server/src/rpc/mod.rs` and re-export what Task 5 needs (`pub(crate) use e2ee::{...};` following the file's existing style).

Note on nonce exhaustion: snow's `TransportState` fails a `write_message`/`read_message` once its u64 nonce is exhausted; that surfaces here as `E2eeSessionError::Crypto`, which Task 5 turns into a connection close. No rekey in v1 (documented in Task 15); the counter bound is unreachable in practice.

- [x] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p bibcode-server e2ee && cargo clippy -p bibcode-server --all-targets -- -D warnings`
Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add apps/server/src/rpc/e2ee.rs apps/server/src/rpc/mod.rs Cargo.toml Cargo.lock
git commit -m "feat(server): Noise NK responder channel with record framing"
```

---

### Task 5: `/ws-e2ee` route — in-channel credential bootstrap, no-downgrade, hardening (server, spec §4.3)

Mount the route; run handshake → in-channel `e2ee_auth` (pairing form performs the bootstrap exchange and mints the device session **inside** the channel; bearer form authenticates a stored credential) → `run_session_split` with encrypt/decrypt pumps, mirroring every side effect of the plain `/ws` handler (`mark_connected`/`mark_disconnected`, expiration guard, unsafe-no-auth behavior). Two red-green cycles: **Cycle A** adds the session `transport` marker and the no-downgrade rejections to `AuthService`; **Cycle B** builds the route with pre-auth hardening (4403 wrong-key close, 64 KiB pre-auth cap, single combined deadline, pre-auth connection cap, plain-session write/join pump timeouts).

**Files:**

- Modify: `apps/server/src/auth/token.rs` (`SessionClaims` gains a `tr` transport claim, decode-defaulted `"plain"`)
- Modify: `apps/server/src/auth/service.rs` (`SessionTransport`, transport-aware issuance + authentication checks)
- Modify: `apps/server/src/auth/http.rs` + `apps/server/src/rpc/mod.rs`/callers (plain surfaces pass `SessionTransport::Plain`)
- Modify: `apps/server/src/http.rs` (route, `ROUTE_INVENTORY`, handler, shared expiration-guard helper)
- Modify: `apps/server/src/rpc/e2ee.rs` (add `run_e2ee_session`)
- Test: `apps/server/tests/e2ee_ws.rs` (new integration test; snow + tokio-tungstenite client against an in-process `ServerRuntime`, following the `activity_rpc.rs` connect-helper pattern)
- Modify: `apps/server/tests/server_runtime.rs` (`expected_routes()` gains `("GET", "/ws-e2ee")`)

**Interfaces:**

- Consumes: `E2eeChannel`, control-message helpers, constants (Task 4); `run_session_split` (Task 3); `AuthService::{exchange_bootstrap, authenticate_token, mark_connected, mark_disconnected, host_identity}`; `RpcSessionContext::{authenticated, unauthenticated}`; `AppState.config.{environment_id, storage_instance_id}`.
- Produces: `pub(crate) const WS_E2EE_PATH: &str = "/ws-e2ee";` in `http.rs`; the running route; `pub(crate) enum SessionTransport { Plain, E2ee }` in `auth/service.rs` with transport-aware `exchange_bootstrap` / `authenticate_token` (existing call sites pass `Plain`); the no-downgrade guarantee (e2ee-minted credentials rejected on `/ws` and plain-HTTP bearer surfaces). `GET /ws-e2ee` classifies as `RpcMutability::Read` automatically (`http_mutability` in `apps/server/src/maintenance.rs:49` treats all GETs as reads — no change there).

**Design note — where `transport` is recorded.** The bearer credential is a server-signed `SessionClaims` token (`auth/token.rs`); the transport marker lives as a signed claim (`tr: "plain" | "e2ee"`, decode-defaulted `"plain"` for pre-existing tokens) plus a mirrored field on the in-memory `SessionRecord` at issuance. Because the claim is signature-protected, every surface that sees the credential can enforce no-downgrade without a schema migration (the `auth_sessions` table is untouched — avoiding a migration-corpus regeneration whose golden databases have no in-repo generator). Enforcement is therefore claims-based and restart-safe: the claim re-supplies the transport on every authentication. If the spec owner later requires a persisted column, that is migration 34 + persistence-parity corpus regeneration — out of scope here and noted in the final report.

#### Cycle A — session transport marker and no-downgrade checks

- [x] **Step A1: Write the failing unit tests** (in `apps/server/src/auth/service.rs` `mod tests`, following its existing `AuthService::new` fixtures):

```rust
#[tokio::test]
async fn e2ee_minted_tokens_are_rejected_on_plain_surfaces() {
    let auth = service(); // the module's existing fixture helper (auth/service.rs ~line 1216)
    let pairing = auth
        .issue_pairing(owned_scopes(STANDARD_SCOPES), Some("device".to_owned()))
        .await
        .unwrap();
    let issued = auth
        .exchange_bootstrap(
            &pairing.credential,
            None,
            test_client_metadata(),
            None,
            SessionTransport::E2ee,
        )
        .await
        .unwrap();

    // Accepted on the e2ee surface...
    assert!(auth
        .authenticate_token(&issued.token, SessionTransport::E2ee)
        .await
        .is_ok());
    // ...rejected on every plain surface (no-downgrade).
    assert!(matches!(
        auth
            .authenticate_token(&issued.token, SessionTransport::Plain)
            .await,
        Err(AuthError::InvalidCredential)
    ));
}

#[tokio::test]
async fn plain_minted_tokens_still_work_on_both_surfaces() {
    // Upgrade is allowed; only downgrade is forbidden.
    let auth = service();
    let pairing = auth
        .issue_pairing(owned_scopes(STANDARD_SCOPES), Some("device".to_owned()))
        .await
        .unwrap();
    let issued = auth
        .exchange_bootstrap(
            &pairing.credential,
            None,
            test_client_metadata(),
            None,
            SessionTransport::Plain,
        )
        .await
        .unwrap();
    assert!(auth
        .authenticate_token(&issued.token, SessionTransport::Plain)
        .await
        .is_ok());
    assert!(auth
        .authenticate_token(&issued.token, SessionTransport::E2ee)
        .await
        .is_ok());
}

#[tokio::test]
async fn legacy_tokens_without_a_transport_claim_decode_as_plain() {
    // Sign claims without `tr` (serde default) and assert they authenticate on
    // Plain — additive decode-default, no invalidation of existing sessions.
}
```

(Write the third test for real by constructing `SessionClaims` without the new field — e.g. via a serde-json round trip that strips `tr` — and signing with the service's signer through a test seam; if no such seam exists, sign a claims JSON built by hand with the same `TokenSigner`.)

- [x] **Step A2: Run to verify failure**

Run: `cargo test -p bibcode-server auth::service`
Expected: compile failure (no `SessionTransport`, wrong arities) — red.

- [x] **Step A3: Implement**

`auth/token.rs`: add to `SessionClaims`:

```rust
#[serde(default = "default_transport")]
pub tr: String, // "plain" | "e2ee"; default keeps pre-existing tokens valid
```

with `fn default_transport() -> String { "plain".to_owned() }`.

`auth/service.rs`:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionTransport {
    Plain,
    E2ee,
}

impl SessionTransport {
    const fn claim(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::E2ee => "e2ee",
        }
    }
}
```

- `issue_session` (internal) gains `transport: SessionTransport`, writes `tr: transport.claim()` into the claims and mirrors it on the in-memory `SessionRecord`.
- `exchange_bootstrap` and `create_browser_session` gain the `transport` parameter; `create_browser_session` callers pass `Plain` (cookies never ride e2ee).
- `authenticate_token(token, surface: SessionTransport)`: after the existing claim checks, add: `if claims.tr == "e2ee" && surface == SessionTransport::Plain { return Err(AuthError::InvalidCredential); }` (reject unknown `tr` values the same way).
- `verify_websocket_ticket` is used only by the plain `/ws` route; tickets are minted from an authenticated principal, and principals from e2ee sessions can only try to fetch tickets over plain HTTP where their bearer is already rejected — no ticket change needed. State this in a comment on the function.
- Update every existing caller (`auth/http.rs` handlers, `authenticate_request_for_method`, rpc context re-auth if any) to pass `SessionTransport::Plain`. `rg -n "authenticate_token|exchange_bootstrap|create_browser_session" apps/server/src` and fix all sites; the compiler enforces completeness.

- [x] **Step A4: Run to verify pass**

Run: `cargo test -p bibcode-server auth && cargo test -p bibcode-server --test auth_http`
Expected: PASS (existing HTTP behavior unchanged — all plain surfaces still authenticate plain-minted tokens).

- [x] **Step A5: Commit**

```bash
git add apps/server/src/auth/token.rs apps/server/src/auth/service.rs apps/server/src/auth/http.rs
git commit -m "feat(server): transport-scoped auth sessions with no-downgrade enforcement"
```

#### Cycle B — the route

- [x] **Step 1: Write the failing integration tests**

`apps/server/tests/e2ee_ws.rs`. Boot pattern: `ServerRuntime::start(ServerConfig::new(root.path()).with_bind("127.0.0.1", 0))` exactly as `cli_smoke.rs` does. The startup pairing credential from `handle.startup_access()` is a one-time pairing token — the e2ee tests authenticate with it **inside the channel** (`{"type":"e2ee_auth","pairing":"<startup credential>"}`); no `/oauth/token` or ticket HTTP round-trips on the e2ee path. (Plain-HTTP `admin_bearer_token` from `/oauth/token` remains a helper for the no-downgrade assertions and Task 8's mint calls only.) Test helper `noise_connect(addr, host_key, ) -> (WebSocketStream, snow::TransportState)`:

```rust
async fn noise_connect(
    addr: std::net::SocketAddr,
    host_key: &[u8],
) -> (
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    snow::TransportState,
) {
    let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws-e2ee"))
        .await
        .expect("open /ws-e2ee");
    let mut initiator = snow::Builder::new("Noise_NK_25519_ChaChaPoly_SHA256".parse().unwrap())
        .remote_public_key(host_key)
        .unwrap()
        .build_initiator()
        .unwrap();
    let mut message_a = vec![0_u8; 65_535];
    let len = initiator.write_message(&[], &mut message_a).unwrap();
    message_a.truncate(len);
    socket.send(Message::Binary(message_a.into())).await.unwrap();
    let frame = socket.next().await.unwrap().unwrap();
    let Message::Binary(message_b) = frame else { panic!("expected binary message B") };
    let mut payload = vec![0_u8; 65_535];
    initiator.read_message(&message_b, &mut payload).unwrap();
    (socket, initiator.into_transport_mode().unwrap())
}
```

The host key for tests: read the persisted secret directly from the data root (`<root>/userdata/secrets/host-identity-x25519.bin`, bytes 32..64 are the public key — Task 2 pinned that layout with a test). Helper `read_host_public_key(root: &Path) -> Vec<u8>`.

Record helpers `encrypt_records` / `decrypt_frames` mirroring Task 4's test helpers (flag byte, ≤ 65518-byte chunks).

Tests (each with its own `TempDir` + runtime):

1. `pairing_bootstrap_inside_the_channel_serves_get_config` — handshake; send encrypted `{"type":"e2ee_auth","pairing":"<startup credential>"}`; expect encrypted `{"type":"e2ee_authenticated","credential":"<non-empty>","environmentId":"<config env id>","storageInstanceId":"<config storage id>"}` (assert all three fields against the running config); then an encrypted `{"_tag":"Request","id":"1","tag":"server.getConfig","payload":{},"headers":[]}` round-trips with `"id":"1"` and no `ClientProtocolError` — decrypt with the reassembler since responses may fragment.
2. `bearer_form_reconnect_works_with_the_in_channel_credential` — perform test 1's bootstrap, close, open a second e2ee connection, authenticate with `{"type":"e2ee_auth","bearer":"<credential from test 1's reply>"}`, expect plain `{"type":"e2ee_authenticated"}` (no credential fields) and a working `server.getConfig`.
3. `no_downgrade_e2ee_credential_is_rejected_on_plain_surfaces` — bootstrap as in test 1, then: (a) `GET /api/auth/session` with `Authorization: Bearer <e2ee credential>` reports `authenticated: false` (or 401 per that endpoint's contract); (b) `POST /api/auth/websocket-ticket` with the e2ee bearer → 401; (c) connect plain `/ws` with `Authorization: Bearer <e2ee credential>` header → rejected (HTTP 401 on upgrade). A plain-HTTP-minted bearer (via `admin_bearer_token`) still works on all three — the rule is downgrade-only.
4. `bad_pairing_token_gets_e2ee_error_unauthorized_and_stays_unconsumed` — handshake; auth with `{"type":"e2ee_auth","pairing":"nope"}`; expect encrypted `{"type":"e2ee_error","code":"unauthorized"}` then close. Separately: complete a handshake, then drop the connection WITHOUT sending `e2ee_auth`, reconnect, and authenticate successfully with the same (real) startup credential — pre-auth failures leave the one-time token retryable.
5. `bad_bearer_gets_e2ee_error_unauthorized` — handshake; `{"type":"e2ee_auth","bearer":"garbage"}` → encrypted `unauthorized` error, close.
6. `wrong_host_key_closes_with_4403` — `noise_connect` against a random 32-byte key: the server cannot read message A and must close with WS close code **4403** and no message B (assert the close frame's code; `read_message` on our side never happens).
7. `non_empty_handshake_payload_is_rejected` — send a message A built with payload `b"x"`; expect close (protocol violation; no message B).
8. `oversized_binary_frame_closes_the_connection` — after authenticating, send a 65 536-byte binary frame; expect the connection to close (encrypted `e2ee_error` `"protocol"` first is acceptable but the close is the contract).
9. `preauth_message_cap_is_64kib` — handshake, then send a fragmented `e2ee_auth` whose reassembled size exceeds 64 KiB (two 40 KiB continuation records + final); expect close before any auth processing.
10. `handshake_timeout_closes_the_socket` — open `/ws-e2ee`, send nothing, assert the server closes within ~11 s (use `tokio::time::timeout(Duration::from_secs(12), socket.next())` and expect Close/None; real-time test — keep it, it guards the single combined deadline).
11. `preauth_connection_cap_rejects_the_overflow_connection` — open `E2EE_MAX_PREAUTH_CONNECTIONS` sockets that stall before `e2ee_auth`, then open one more and expect an immediate close (code 1013); complete or drop one stalled socket and verify a new connection is admitted again.
12. `text_frames_before_handshake_are_rejected` — send a Text frame first; expect close.

- [x] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p bibcode-server --test e2ee_ws`
Expected: FAIL — connecting to `/ws-e2ee` gets an HTTP 404/handshake failure (route absent).

- [x] **Step 3: Implement the route**

In `apps/server/src/rpc/session.rs`, make `SOCKET_WRITE_TIMEOUT` `pub(crate)` (the pumps adopt the plain session's 5 s write policy) and add next to it `pub(crate) const PUMP_JOIN_TIMEOUT: Duration = Duration::from_secs(1);` — verify the actual constant name/value in the file first and reuse whatever the plain writer task uses; do not invent a second write-timeout value.

In `apps/server/src/rpc/e2ee.rs` add the session orchestrator (uses `axum::extract::ws::{CloseFrame, Message, WebSocket}`, `futures_util::{SinkExt, StreamExt}`, `tokio_util::sync::PollSender`, `futures_util::stream`, `tokio::sync::Semaphore`, `tokio::time::timeout`):

```rust
/// Caps unauthenticated in-flight E2EE connections process-wide (pre-auth
/// hardening, spec section 4.3). Permits are held only until e2ee_auth resolves.
static E2EE_PREAUTH_PERMITS: Semaphore =
    Semaphore::const_new(E2EE_MAX_PREAUTH_CONNECTIONS);

pub(crate) enum E2eeAccept {
    Authenticated {
        principal: crate::auth::Principal,
        /// Set on the pairing form: the in-channel bootstrap reply payload.
        minted: Option<MintedE2eeSession>,
    },
    Unauthenticated,
}

pub(crate) struct MintedE2eeSession {
    pub credential: String,
}

/// Runs the full /ws-e2ee session: NK handshake, in-channel e2ee_auth (pairing
/// bootstrap or stored bearer), then the normal RPC session over encrypted
/// frames. All failure paths close the socket; a wrong pinned key closes with
/// code 4403 so initiators can classify host-identity-mismatch.
pub(crate) async fn run_e2ee_session(
    socket: WebSocket,
    auth: crate::auth::AuthService,
    registry: crate::rpc::RpcRegistry,
    config: std::sync::Arc<crate::config::ServerConfig>,
    session_shutdown: tokio_util::sync::CancellationToken,
) {
    let Ok(preauth_permit) = E2EE_PREAUTH_PERMITS.try_acquire() else {
        // Over the pre-auth cap: close immediately (1013 = try again later).
        let mut socket = socket;
        let _ = socket
            .send(Message::Close(Some(CloseFrame { code: 1013, reason: "busy".into() })))
            .await;
        return;
    };

    let (mut ws_writer, mut ws_reader) = socket.split();

    // ---- one combined deadline: upgrade -> handshake -> e2ee_authenticated ----
    let established = timeout(E2EE_HANDSHAKE_TIMEOUT, async {
        // recv_binary: next Binary frame; Ping/Pong skipped; Text/Close/error -> None.
        // (Implemented as a small local helper struct if the closure borrow of
        //  ws_reader across the phases requires it.)
        let mut channel =
            E2eeChannel::respond(auth.host_identity(), /* recv */, /* send via ws_writer */)
                .await?;

        // e2ee_auth: first logical transport message, pre-auth 64 KiB cap.
        let auth_bytes = loop {
            let Some(frame) = /* recv binary */ else {
                return Err(E2eeSessionError::Closed);
            };
            if let Some(message) =
                channel.decrypt_frame(&frame, MAX_E2EE_PREAUTH_MESSAGE_BYTES)?
            {
                break message;
            }
        };
        Ok((channel, auth_bytes))
    })
    .await;

    let (mut channel, auth_bytes) = match established {
        Ok(Ok(established)) => established,
        Ok(Err(E2eeSessionError::Handshake)) => {
            // Wrong pinned key: close 4403, never send message B.
            let _ = ws_writer
                .send(Message::Close(Some(CloseFrame {
                    code: E2EE_HOST_IDENTITY_CLOSE_CODE,
                    reason: "host-identity".into(),
                })))
                .await;
            return;
        }
        _ => {
            // Protocol violation, transport loss, or deadline exceeded.
            let _ = ws_writer.close().await;
            return;
        }
    };

    let unsafe_no_auth = config.unsafe_no_auth;
    let accept: Result<E2eeAccept, &'static str> =
        match serde_json::from_slice::<E2eeAuthMessage>(&auth_bytes) {
            Ok(message) if message.r#type == "e2ee_auth" => {
                if unsafe_no_auth {
                    Ok(E2eeAccept::Unauthenticated)
                } else {
                    match (message.pairing, message.bearer) {
                        // First connect: bootstrap INSIDE the channel. The one-time
                        // token is consumed only by this successful exchange.
                        (Some(pairing), None) => match auth
                            .exchange_bootstrap(
                                &pairing,
                                None,
                                e2ee_client_metadata(),
                                None,
                                crate::auth::SessionTransport::E2ee,
                            )
                            .await
                        {
                            Ok(issued) => Ok(E2eeAccept::Authenticated {
                                principal: issued.principal,
                                minted: Some(MintedE2eeSession { credential: issued.token }),
                            }),
                            Err(_) => Err("unauthorized"),
                        },
                        // Subsequent connects: stored bearer, e2ee surface.
                        (None, Some(bearer)) => match auth
                            .authenticate_token(&bearer, crate::auth::SessionTransport::E2ee)
                            .await
                        {
                            Ok(principal) => Ok(E2eeAccept::Authenticated {
                                principal,
                                minted: None,
                            }),
                            Err(_) => Err("unauthorized"),
                        },
                        _ => Err("protocol"), // neither or both fields
                    }
                }
            }
            _ => Err("protocol"),
        };
    drop(preauth_permit); // auth resolved either way; free the pre-auth slot

    let accept = match accept {
        Ok(accept) => accept,
        Err(code) => {
            if let Ok(frames) = channel.encrypt_message(&e2ee_error_json(code)) {
                for frame in frames {
                    let _ = timeout(
                        crate::rpc::SOCKET_WRITE_TIMEOUT,
                        ws_writer.send(Message::Binary(frame.into())),
                    )
                    .await;
                }
            }
            let _ = ws_writer.close().await;
            return;
        }
    };
    let reply = match &accept {
        E2eeAccept::Authenticated { minted: Some(minted), .. } => {
            e2ee_authenticated_with_credential_json(
                &minted.credential,
                &config.environment_id,
                &config
                    .storage_instance_id
                    .map(|id| id.to_string())
                    .unwrap_or_default(),
            )
        }
        _ => e2ee_authenticated_json(),
    };
    if let Ok(frames) = channel.encrypt_message(&reply) {
        for frame in frames {
            if !matches!(
                timeout(
                    crate::rpc::SOCKET_WRITE_TIMEOUT,
                    ws_writer.send(Message::Binary(frame.into())),
                )
                .await,
                Ok(Ok(()))
            ) {
                return;
            }
        }
    }

    // ---- pumps + RPC session ----
    // channel is split logically: the outbound pump owns encrypt_message, the
    // inbound pump owns decrypt_frame. snow's TransportState is a single object;
    // wrap it in Arc<std::sync::Mutex<E2eeChannel>> — both pumps take the lock
    // per frame, never across await points.
    let channel = std::sync::Arc::new(std::sync::Mutex::new(channel));

    let (outbound_tx, mut outbound_rx) = tokio::sync::mpsc::channel::<Message>(64);
    let (inbound_tx, inbound_rx) = tokio::sync::mpsc::channel::<Result<Message, axum::Error>>(64);

    let pump_shutdown = session_shutdown.clone();
    let outbound_channel = channel.clone();
    let outbound_pump = tokio::spawn(async move {
        while let Some(message) = outbound_rx.recv().await {
            let frames = match &message {
                Message::Text(text) => outbound_channel
                    .lock()
                    .expect("e2ee channel lock")
                    .encrypt_message(text.as_bytes()),
                Message::Binary(bytes) => outbound_channel
                    .lock()
                    .expect("e2ee channel lock")
                    .encrypt_message(bytes),
                Message::Close(_) => break,
                Message::Ping(_) | Message::Pong(_) => continue,
            };
            let Ok(frames) = frames else { break };
            let mut failed = false;
            for frame in frames {
                // Same write policy as the plain RPC session's writer task:
                // a bounded write, and any timeout/error ends the session.
                if !matches!(
                    timeout(
                        crate::rpc::SOCKET_WRITE_TIMEOUT,
                        ws_writer.send(Message::Binary(frame.into())),
                    )
                    .await,
                    Ok(Ok(()))
                ) {
                    failed = true;
                    break;
                }
            }
            if failed {
                break;
            }
        }
        let _ = timeout(crate::rpc::SOCKET_WRITE_TIMEOUT, ws_writer.close()).await;
        pump_shutdown.cancel();
    });

    let inbound_shutdown = session_shutdown.clone();
    let inbound_channel = channel.clone();
    let inbound_pump = tokio::spawn(async move {
        while let Some(frame) = ws_reader.next().await {
            let message = match frame {
                Ok(Message::Binary(bytes)) => {
                    match inbound_channel
                        .lock()
                        .expect("e2ee channel lock")
                        .decrypt_frame(&bytes, MAX_E2EE_LOGICAL_MESSAGE_BYTES)
                    {
                        Ok(Some(plaintext)) => Message::Binary(plaintext.into()),
                        Ok(None) => continue,
                        Err(_) => break, // crypto/protocol violation: fail closed
                    }
                }
                Ok(Message::Ping(_) | Message::Pong(_)) => continue,
                Ok(Message::Close(_)) | Err(_) => break,
                Ok(Message::Text(_)) => break, // plaintext frames are a violation post-handshake
            };
            if inbound_tx.send(Ok(message)).await.is_err() {
                break;
            }
        }
        inbound_shutdown.cancel();
    });

    let context = match &accept {
        E2eeAccept::Authenticated { principal, .. } => {
            auth.mark_connected(&principal.session_id).await;
            crate::rpc::RpcSessionContext::authenticated(principal.clone(), auth.clone())
        }
        E2eeAccept::Unauthenticated => crate::rpc::RpcSessionContext::unauthenticated(),
    };

    let writer_sink = PollSender::new(outbound_tx);
    let reader_stream = futures_util::stream::unfold(inbound_rx, |mut receiver| async {
        receiver.recv().await.map(|item| (item, receiver))
    });
    crate::rpc::run_session_split(writer_sink, reader_stream, registry, context, session_shutdown.clone()).await;

    session_shutdown.cancel();
    // Bounded pump reaping (the plain session's join policy): give each pump the
    // join timeout, then abort — the wrapper must not weaken shutdown guarantees.
    let mut outbound_pump = outbound_pump;
    if timeout(crate::rpc::PUMP_JOIN_TIMEOUT, &mut outbound_pump).await.is_err() {
        outbound_pump.abort();
    }
    let mut inbound_pump = inbound_pump;
    if timeout(crate::rpc::PUMP_JOIN_TIMEOUT, &mut inbound_pump).await.is_err() {
        inbound_pump.abort();
    }
    if let E2eeAccept::Authenticated { principal, .. } = accept {
        auth.mark_disconnected(&principal.session_id).await;
    }
}

fn e2ee_client_metadata() -> crate::auth::ClientMetadata {
    // Match ClientMetadata's real field set (auth/model.rs); the e2ee bootstrap
    // has no HTTP headers to harvest, so everything defaults.
    crate::auth::ClientMetadata {
        label: None,
        ip_address: None,
        user_agent: None,
        device_type: "unknown".to_owned(),
        os: None,
        browser: None,
    }
}
```

The plaintext delivered to `run_session_split` is `Message::Binary` — `decode_client_messages` (`apps/server/src/rpc/session.rs:387-388`) already treats Text and Binary identically, so the RPC layer stays unaware of the wrapper, and outbound Text frames are encrypted byte-for-byte (spec §4.3's plaintext rule in concatenated-record form).

In `apps/server/src/http.rs`:

1. `pub(crate) const WS_E2EE_PATH: &str = "/ws-e2ee";`
2. `ROUTE_INVENTORY`: add `route(RouteMethod::Get, WS_E2EE_PATH)` directly after the `"/ws"` entry.
3. Extract the session-expiration watchdog from the existing `websocket` handler into a shared helper and reuse it (the E2EE principal has the same `expires_at_ms` semantics):

```rust
fn spawn_session_expiration_guard(
    expires_at_ms: i64,
    session_shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let remaining_ms = expires_at_ms.saturating_sub(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .and_then(|duration| i64::try_from(duration.as_millis()).ok())
                .unwrap_or(i64::MAX),
        );
        tokio::select! {
            () = session_shutdown.cancelled() => {}
            () = tokio::time::sleep(std::time::Duration::from_millis(
                u64::try_from(remaining_ms.max(0)).unwrap_or_default(),
            )) => session_shutdown.cancel(),
        }
    })
}
```

Refactor the plain `websocket` handler to use it (behavior-preserving), then add:

```rust
async fn websocket_e2ee(State(state): State<AppState>, upgrade: WebSocketUpgrade) -> Response {
    let session_shutdown = state.shutdown.child_token();
    upgrade
        .on_upgrade(move |socket| {
            run_e2ee_session(
                socket,
                state.auth,
                state.rpc_registry,
                state.config,
                session_shutdown,
            )
        })
        .into_response()
}
```

and `.route(WS_E2EE_PATH, get(websocket_e2ee))` next to the `/ws` route in `build_router`. Inside `run_e2ee_session`, after a successful `e2ee_auth`, call `spawn_session_expiration_guard(principal.expires_at_ms, session_shutdown.clone())` and await the guard next to the pumps during cleanup (mirror the plain handler's ordering: run session → cancel → await guard → `mark_disconnected`). Pass the helper in as needed (`run_e2ee_session` can take a `spawn_expiration_guard: impl FnOnce(i64, CancellationToken) -> JoinHandle<()>` parameter, or simply live with the guard spawned from `http.rs` by returning the principal — choose the former for locality).

4. `apps/server/tests/server_runtime.rs` `expected_routes()`: insert `("GET", "/ws-e2ee")` after `("GET", "/ws")`.

- [x] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p bibcode-server --test e2ee_ws && cargo test -p bibcode-server --test auth_http && cargo test -p bibcode-server --test server_runtime route_inventory && cargo test -p bibcode-server --test production_maintenance && cargo clippy -p bibcode-server --all-targets -- -D warnings`
Expected: PASS (including the maintenance-gap test — GET routes classify as reads automatically — and the no-downgrade cases from test 3).

- [x] **Step 5: Commit**

```bash
git add apps/server/src/http.rs apps/server/src/rpc/e2ee.rs apps/server/src/rpc/mod.rs apps/server/src/rpc/session.rs apps/server/tests/e2ee_ws.rs apps/server/tests/server_runtime.rs
git commit -m "feat(server): serve the RPC session over an in-channel-authenticated /ws-e2ee Noise channel"
```

---

### Task 6: Pairing-code contract schema, Rust mirror, parity fixtures (spec §4.2)

**Files:**

- Create: `packages/contracts/src/remotePairing.ts`
- Create: `packages/contracts/src/remotePairing.test.ts`
- Create: `packages/contracts/fixtures/remote-pairing/manifest.json`
- Create: `packages/contracts/fixtures/remote-pairing/payload.json`
- Create: `packages/contracts/fixtures/remote-pairing/code.txt`
- Create: `packages/contracts/fixtures/remote-pairing/unsupported-version.json`
- Modify: `packages/contracts/src/index.ts` (add `export * from "./remotePairing.ts";`)
- Create: `apps/server/src/auth/pairing_code.rs`
- Modify: `apps/server/src/auth/mod.rs` (`mod pairing_code;` + re-exports)
- Test: `apps/server/tests/remote_pairing.rs`

**Interfaces:**

- Consumes: `TrimmedNonEmptyString` from `packages/contracts/src/baseSchemas.ts`.
- Produces (TS): `RemotePairingReach`, `REMOTE_PAIRING_CODE_VERSION`, `RemotePairingCodePayload`, `E2eeAuthPairingMessage`, `E2eeAuthBearerMessage`, `E2eeAuthMessage` (their union), `E2eeAuthenticatedMessage` (optional `credential`/`environmentId`/`storageInstanceId`), `E2eeErrorCode`, `E2eeErrorMessage`. (The mint endpoint's request/response schemas are Task 8's, in `auth.ts`.)
- Produces (Rust): `RemotePairingReach`, `RemotePairingCodePayload`, `REMOTE_PAIRING_CODE_VERSION`, `PairingCodeError`, `encode_pairing_code`, `decode_pairing_code`, `pairing_deep_link`, `browser_pair_url`. Tasks 7, 8, 13, 14 consume these.

- [x] **Step 1: Write the failing TS test**

`packages/contracts/src/remotePairing.test.ts` (parity style follows `persistenceRustParity.test.ts`: a checked-in fixture corpus decoded by both languages; the Rust half comes in Step 5):

```ts
// @effect-diagnostics nodeBuiltinImport:off
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";

import { describe, expect, it } from "@effect/vitest";
import * as Schema from "effect/Schema";

import {
  E2eeAuthMessage,
  E2eeAuthenticatedMessage,
  E2eeErrorMessage,
  REMOTE_PAIRING_CODE_VERSION,
  RemotePairingCodePayload,
  RemotePairingReach,
} from "./remotePairing.ts";

const fixtureDirectory = NodePath.resolve(import.meta.dirname, "../fixtures/remote-pairing");
const readFixture = (name: string): string =>
  NodeFS.readFileSync(NodePath.join(fixtureDirectory, name), "utf8");

const decodePayload = Schema.decodeUnknownSync(Schema.fromJsonString(RemotePairingCodePayload));

describe("remote pairing contract", () => {
  it("decodes the canonical payload fixture", () => {
    const payload = decodePayload(readFixture("payload.json").trim());
    expect(payload.v).toBe(REMOTE_PAIRING_CODE_VERSION);
    expect(payload.endpoint).toBe("http://192.168.1.20:3773");
    expect(payload.name).toBe("AI-SERVER");
    expect(payload.reach).toBe("another-device");
    expect(payload.hostKey).toHaveLength(43);
  });

  it("the encoded code fixture is base64url of the payload fixture", () => {
    const code = readFixture("code.txt").trim();
    const decoded = Buffer.from(code, "base64url").toString("utf8");
    expect(JSON.parse(decoded)).toEqual(JSON.parse(readFixture("payload.json")));
    // base64url alphabet, unpadded
    expect(code).toMatch(/^[A-Za-z0-9_-]+$/);
  });

  it("rejects an unsupported payload version at the schema level", () => {
    expect(() => decodePayload(readFixture("unsupported-version.json").trim())).toThrow();
  });

  it("covers all reach values", () => {
    expect([...RemotePairingReach.literals]).toEqual(["another-device", "this-computer", "custom"]);
  });

  it("round-trips the channel control messages", () => {
    const decodeAuth = Schema.decodeUnknownSync(E2eeAuthMessage);
    const pairingForm = decodeAuth({ type: "e2ee_auth", pairing: "one-time" });
    expect("pairing" in pairingForm && pairingForm.pairing).toBe("one-time");
    const bearerForm = decodeAuth({ type: "e2ee_auth", bearer: "stored" });
    expect("bearer" in bearerForm && bearerForm.bearer).toBe("stored");

    const decodeReady = Schema.decodeUnknownSync(E2eeAuthenticatedMessage);
    expect(decodeReady({ type: "e2ee_authenticated" }).type).toBe("e2ee_authenticated");
    const minted = decodeReady({
      type: "e2ee_authenticated",
      credential: "bearer-token",
      environmentId: "env-1",
      storageInstanceId: "3f2f6a52-6f5f-4f4e-9d38-0a1e2ac21d11",
    });
    expect(minted.credential).toBe("bearer-token");
    expect(minted.environmentId).toBe("env-1");

    const decodeError = Schema.decodeUnknownSync(E2eeErrorMessage);
    expect(decodeError({ type: "e2ee_error", code: "unauthorized" }).code).toBe("unauthorized");
  });
});
```

Fixtures (write them now; the `token` value is a syntactically plausible sample, not a real credential):

`payload.json` — **single canonical JSON encoding, key order exactly as below** (the Rust test re-serializes and compares strings, so the field order must match the Rust struct declaration order):

```json
{
  "v": 1,
  "endpoint": "http://192.168.1.20:3773",
  "name": "AI-SERVER",
  "token": "BCDFGHJKMNPQ",
  "hostKey": "HcMLXPPBHFNvcbHrCVMH-DMh49rd5AGCzSCqAVJ49hM",
  "reach": "another-device",
  "storageInstanceId": "3f2f6a52-6f5f-4f4e-9d38-0a1e2ac21d11"
}
```

`code.txt` — the base64url (unpadded) of the exact bytes of `payload.json` (without trailing newline). Generate it during implementation with `node -e 'console.log(Buffer.from(require("fs").readFileSync("packages/contracts/fixtures/remote-pairing/payload.json","utf8").trim()).toString("base64url"))'` and commit the output.

`unsupported-version.json`:

```json
{
  "v": 2,
  "endpoint": "http://192.168.1.20:3773",
  "name": "AI-SERVER",
  "token": "BCDFGHJKMNPQ",
  "hostKey": "HcMLXPPBHFNvcbHrCVMH-DMh49rd5AGCzSCqAVJ49hM",
  "reach": "another-device",
  "storageInstanceId": "3f2f6a52-6f5f-4f4e-9d38-0a1e2ac21d11"
}
```

`manifest.json`:

```json
{
  "formatVersion": 1,
  "fixtures": ["code.txt", "payload.json", "unsupported-version.json"]
}
```

- [x] **Step 2: Run to verify it fails**

Run (from `packages/contracts`): `vp test run src/remotePairing.test.ts`
Expected: FAIL — module `./remotePairing.ts` does not exist.

- [x] **Step 3: Write the TS schema**

`packages/contracts/src/remotePairing.ts` (schema-only — no runtime logic; the codec lives in `packages/shared`, Task 7):

```ts
import * as Schema from "effect/Schema";

import { EnvironmentId, TrimmedNonEmptyString } from "./baseSchemas.ts";

export const REMOTE_PAIRING_CODE_VERSION = 1;

/** Spec section 4.2: pairing intent recorded per grant. */
export const RemotePairingReach = Schema.Literals(["another-device", "this-computer", "custom"]);
export type RemotePairingReach = typeof RemotePairingReach.Type;

/**
 * JSON payload carried by `bibcode://pair?code=<base64url(JSON)>`.
 * `hostKey` is the base64url (unpadded) X25519 host identity public key.
 * An unknown `v` must surface as "unsupported pairing code" (parse helper in
 * `@bibcode/shared/pairingCode`).
 */
export const RemotePairingCodePayload = Schema.Struct({
  v: Schema.Literal(REMOTE_PAIRING_CODE_VERSION),
  endpoint: TrimmedNonEmptyString,
  name: TrimmedNonEmptyString,
  token: TrimmedNonEmptyString,
  hostKey: TrimmedNonEmptyString,
  reach: RemotePairingReach,
  storageInstanceId: TrimmedNonEmptyString,
});
export type RemotePairingCodePayload = typeof RemotePairingCodePayload.Type;

/**
 * First transport message inside the E2EE channel (spec section 4.3), in one of
 * two forms: `pairing` (first connect — the server performs the bootstrap
 * exchange inside the channel) or `bearer` (subsequent connects).
 */
export const E2eeAuthPairingMessage = Schema.Struct({
  type: Schema.Literal("e2ee_auth"),
  pairing: TrimmedNonEmptyString,
});
export type E2eeAuthPairingMessage = typeof E2eeAuthPairingMessage.Type;

export const E2eeAuthBearerMessage = Schema.Struct({
  type: Schema.Literal("e2ee_auth"),
  bearer: TrimmedNonEmptyString,
});
export type E2eeAuthBearerMessage = typeof E2eeAuthBearerMessage.Type;

export const E2eeAuthMessage = Schema.Union([E2eeAuthPairingMessage, E2eeAuthBearerMessage]);
export type E2eeAuthMessage = typeof E2eeAuthMessage.Type;

/**
 * Success reply. The pairing form carries the in-channel-minted bearer
 * credential plus the authenticated identity (`environmentId`,
 * `storageInstanceId`) that verify-then-add compares against the pairing
 * payload; the bearer form carries none of the optional fields.
 */
export const E2eeAuthenticatedMessage = Schema.Struct({
  type: Schema.Literal("e2ee_authenticated"),
  credential: Schema.optionalKey(TrimmedNonEmptyString),
  environmentId: Schema.optionalKey(EnvironmentId),
  storageInstanceId: Schema.optionalKey(TrimmedNonEmptyString),
});
export type E2eeAuthenticatedMessage = typeof E2eeAuthenticatedMessage.Type;

export const E2eeErrorCode = Schema.Literals(["unauthorized", "protocol"]);
export type E2eeErrorCode = typeof E2eeErrorCode.Type;

export const E2eeErrorMessage = Schema.Struct({
  type: Schema.Literal("e2ee_error"),
  code: E2eeErrorCode,
});
export type E2eeErrorMessage = typeof E2eeErrorMessage.Type;
```

Add `export * from "./remotePairing.ts";` to `packages/contracts/src/index.ts` (after the `remoteAccess.ts` line).

- [x] **Step 4: Run TS test to verify it passes**

Run (from `packages/contracts`): `vp test run src/remotePairing.test.ts`
Expected: PASS.

- [x] **Step 5: Write the failing Rust parity test, then the Rust mirror**

`apps/server/tests/remote_pairing.rs`:

```rust
use std::path::PathBuf;

use bibcode_server::auth_pairing_code::{
    PairingCodeError, REMOTE_PAIRING_CODE_VERSION, RemotePairingCodePayload, RemotePairingReach,
    browser_pair_url, decode_pairing_code, encode_pairing_code, pairing_deep_link,
};

fn fixture_directory() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/contracts/fixtures/remote-pairing")
}

fn read_fixture(name: &str) -> String {
    std::fs::read_to_string(fixture_directory().join(name)).expect("read fixture")
}

#[test]
fn canonical_payload_fixture_round_trips_through_the_rust_mirror() {
    let payload: RemotePairingCodePayload =
        serde_json::from_str(read_fixture("payload.json").trim()).expect("decode payload");
    assert_eq!(payload.v, REMOTE_PAIRING_CODE_VERSION);
    assert_eq!(payload.endpoint, "http://192.168.1.20:3773");
    assert_eq!(payload.reach, RemotePairingReach::AnotherDevice);
    // Field-order-stable re-serialization must reproduce the fixture bytes.
    assert_eq!(
        serde_json::to_string(&payload).expect("encode payload"),
        read_fixture("payload.json").trim()
    );
}

#[test]
fn code_fixture_matches_the_rust_encoder() {
    let payload: RemotePairingCodePayload =
        serde_json::from_str(read_fixture("payload.json").trim()).expect("decode payload");
    assert_eq!(
        encode_pairing_code(&payload).expect("encode"),
        read_fixture("code.txt").trim()
    );
    let decoded = decode_pairing_code(read_fixture("code.txt").trim()).expect("decode");
    assert_eq!(decoded, payload);
}

#[test]
fn unsupported_version_is_classified_distinctly() {
    let code = {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        URL_SAFE_NO_PAD.encode(read_fixture("unsupported-version.json").trim())
    };
    assert!(matches!(
        decode_pairing_code(&code),
        Err(PairingCodeError::UnsupportedVersion { v: 2 })
    ));
}

#[test]
fn deep_link_and_browser_url_shapes_are_stable() {
    assert_eq!(pairing_deep_link("abc"), "bibcode://pair?code=abc");
    assert_eq!(
        browser_pair_url("http://192.168.1.20:3773", "abc").expect("browser url"),
        "http://192.168.1.20:3773/pair?code=abc"
    );
}
```

Run `cargo test -p bibcode-server --test remote_pairing` — expect compile failure (red). Then implement `apps/server/src/auth/pairing_code.rs`:

```rust
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const REMOTE_PAIRING_CODE_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RemotePairingReach {
    AnotherDevice,
    ThisComputer,
    Custom,
}

/// Rust mirror of `packages/contracts/src/remotePairing.ts` (spec section 4.2).
/// Field order is the canonical serialization order pinned by the parity fixture.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePairingCodePayload {
    pub v: u32,
    pub endpoint: String,
    pub name: String,
    pub token: String,
    pub host_key: String,
    pub reach: RemotePairingReach,
    pub storage_instance_id: String,
}

#[derive(Debug, Error)]
pub enum PairingCodeError {
    #[error("pairing code is not valid base64url")]
    Encoding(#[from] base64::DecodeError),
    #[error("pairing code payload is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported pairing code version {v}")]
    UnsupportedVersion { v: u64 },
    #[error("pairing endpoint is not a valid HTTP URL: {0}")]
    Endpoint(#[from] url::ParseError),
}

pub fn encode_pairing_code(
    payload: &RemotePairingCodePayload,
) -> Result<String, PairingCodeError> {
    Ok(URL_SAFE_NO_PAD.encode(serde_json::to_vec(payload)?))
}

pub fn decode_pairing_code(code: &str) -> Result<RemotePairingCodePayload, PairingCodeError> {
    let bytes = URL_SAFE_NO_PAD.decode(code.trim())?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    let version = value.get("v").and_then(serde_json::Value::as_u64);
    if version != Some(u64::from(REMOTE_PAIRING_CODE_VERSION)) {
        return Err(PairingCodeError::UnsupportedVersion {
            v: version.unwrap_or(0),
        });
    }
    Ok(serde_json::from_value(value)?)
}

#[must_use]
pub fn pairing_deep_link(code: &str) -> String {
    format!("bibcode://pair?code={code}")
}

pub fn browser_pair_url(endpoint: &str, code: &str) -> Result<String, PairingCodeError> {
    let mut url = url::Url::parse(endpoint)?;
    url.set_path("/pair");
    url.set_query(Some(&format!("code={code}")));
    Ok(url.to_string())
}
```

In `apps/server/src/auth/mod.rs`: `pub mod pairing_code;`. In `apps/server/src/lib.rs`, re-export it for the integration test the way other test-visible modules are exported (add `pub use auth::pairing_code as auth_pairing_code;` — or match the crate's existing re-export idiom; check how `ROUTE_INVENTORY` etc. are surfaced at `lib.rs:52` and follow it).

- [x] **Step 6: Run all tests to verify they pass**

Run: `cargo test -p bibcode-server --test remote_pairing` and (from `packages/contracts`) `vp test run src/remotePairing.test.ts`
Expected: PASS both. If the string-equality assertion fails on the re-serialization, fix the _fixture_ to the Rust serializer's output and re-run the TS test — both sides must agree on one canonical byte sequence.

- [x] **Step 7: Commit**

```bash
git add packages/contracts/src/remotePairing.ts packages/contracts/src/remotePairing.test.ts packages/contracts/src/index.ts packages/contracts/fixtures/remote-pairing apps/server/src/auth/pairing_code.rs apps/server/src/auth/mod.rs apps/server/src/lib.rs apps/server/tests/remote_pairing.rs
git commit -m "feat(contracts): bibcode://pair payload schema with Rust parity"
```

---

### Task 7: Pairing-code codec and endpoint classification (shared)

Runtime helpers live in `packages/shared` (contracts stays schema-only): encode/parse the code and its two URL forms, and classify pairing endpoints for the loopback/tunnel rule.

**Files:**

- Create: `packages/shared/src/pairingCode.ts`
- Create: `packages/shared/src/pairingCode.test.ts`
- Modify: `packages/shared/src/advertisedEndpoint.ts` (add `classifyPairingEndpoint`)
- Modify: `packages/shared/src/advertisedEndpoint.test.ts` (new cases)
- Modify: `packages/shared/package.json` (add `"./pairingCode"` export entry, matching the existing per-file export style)

**Interfaces:**

- Consumes: `RemotePairingCodePayload`, `REMOTE_PAIRING_CODE_VERSION` from `@bibcode/contracts` (Task 6).
- Produces:

```ts
// pairingCode.ts
export class PairingCodeParseError extends Schema.TaggedErrorClass<PairingCodeParseError>()(
  "PairingCodeParseError",
  { detail: Schema.String },
) {}
export class PairingCodeUnsupportedVersionError extends Schema.TaggedErrorClass<PairingCodeUnsupportedVersionError>()(
  "PairingCodeUnsupportedVersionError",
  { version: Schema.Number },
) {}
export function encodePairingCode(payload: RemotePairingCodePayload): string;
/** Accepts the bare base64url code, `bibcode://pair?code=...`, or `http(s)://.../pair?code=...`. */
export function parsePairingCode(raw: string): RemotePairingCodePayload; // throws the two errors above
export function buildPairingDeepLink(code: string): string; // bibcode://pair?code=<code>
export function buildBrowserPairUrl(endpoint: string, code: string): string; // <endpoint>/pair?code=<code>

// advertisedEndpoint.ts
export type PairingEndpointClassification =
  | "loopback"
  | "private-network"
  | "public"
  | "unconnectable";
export function classifyPairingEndpoint(endpoint: string): PairingEndpointClassification;
```

Tasks 8 (Rust equivalent validation), 13, 14 and Phase 5 consume these.

- [x] **Step 1: Write the failing tests**

`packages/shared/src/pairingCode.test.ts`:

```ts
import { describe, expect, it } from "@effect/vitest";
import { REMOTE_PAIRING_CODE_VERSION, type RemotePairingCodePayload } from "@bibcode/contracts";

import {
  PairingCodeParseError,
  PairingCodeUnsupportedVersionError,
  buildBrowserPairUrl,
  buildPairingDeepLink,
  encodePairingCode,
  parsePairingCode,
} from "./pairingCode.ts";

const payload: RemotePairingCodePayload = {
  v: REMOTE_PAIRING_CODE_VERSION,
  endpoint: "http://192.168.1.20:3773",
  name: "AI-SERVER",
  token: "BCDFGHJKMNPQ",
  hostKey: "HcMLXPPBHFNvcbHrCVMH-DMh49rd5AGCzSCqAVJ49hM",
  reach: "another-device",
  storageInstanceId: "3f2f6a52-6f5f-4f4e-9d38-0a1e2ac21d11",
};

describe("pairingCode", () => {
  it("round-trips encode/parse", () => {
    expect(parsePairingCode(encodePairingCode(payload))).toEqual(payload);
  });

  it("parses the deep-link and browser-url forms", () => {
    const code = encodePairingCode(payload);
    expect(parsePairingCode(buildPairingDeepLink(code))).toEqual(payload);
    expect(parsePairingCode(buildBrowserPairUrl(payload.endpoint, code))).toEqual(payload);
    expect(buildPairingDeepLink(code)).toBe(`bibcode://pair?code=${code}`);
    expect(buildBrowserPairUrl(payload.endpoint, code)).toBe(
      `http://192.168.1.20:3773/pair?code=${code}`,
    );
  });

  it("classifies an unknown version as unsupported", () => {
    const future = Buffer.from(JSON.stringify({ ...payload, v: 99 })).toString("base64url");
    expect(() => parsePairingCode(future)).toThrow(PairingCodeUnsupportedVersionError);
  });

  it("classifies garbage as a parse error", () => {
    expect(() => parsePairingCode("not-base64url-json!!")).toThrow(PairingCodeParseError);
    expect(() => parsePairingCode("bibcode://pair?nope=1")).toThrow(PairingCodeParseError);
    const missingField = Buffer.from(JSON.stringify({ v: 1, endpoint: "http://x" })).toString(
      "base64url",
    );
    expect(() => parsePairingCode(missingField)).toThrow(PairingCodeParseError);
  });
});
```

`packages/shared/src/advertisedEndpoint.test.ts` additions:

```ts
describe("classifyPairingEndpoint", () => {
  it.each([
    ["http://127.0.0.1:3773", "loopback"],
    ["http://[::1]:3773", "loopback"],
    ["http://localhost:3773", "loopback"],
    ["http://192.168.1.20:3773", "private-network"],
    ["http://10.0.0.5:3773", "private-network"],
    ["http://172.16.0.9:3773", "private-network"],
    ["http://100.64.12.1:3773", "private-network"], // CGNAT range used by mesh VPNs
    ["http://[fd00::1]:3773", "private-network"],
    ["http://203.0.113.7:3773", "public"],
    ["https://server.example.com", "public"],
    ["http://0.0.0.0:3773", "unconnectable"],
    ["http://[::]:3773", "unconnectable"],
    ["http://192.168.1.20:0", "unconnectable"],
    ["not a url", "unconnectable"],
  ])("classifies %s as %s", (endpoint, expected) => {
    expect(classifyPairingEndpoint(endpoint)).toBe(expected);
  });
});
```

- [x] **Step 2: Run to verify failure**

Run (from `packages/shared`): `vp test run src/pairingCode.test.ts src/advertisedEndpoint.test.ts`
Expected: FAIL (missing module / missing export).

- [x] **Step 3: Implement**

`packages/shared/src/pairingCode.ts`:

```ts
import { REMOTE_PAIRING_CODE_VERSION, RemotePairingCodePayload } from "@bibcode/contracts";
import * as Schema from "effect/Schema";

export class PairingCodeParseError extends Schema.TaggedErrorClass<PairingCodeParseError>()(
  "PairingCodeParseError",
  { detail: Schema.String },
) {
  override get message(): string {
    return `The pairing code is invalid: ${this.detail}`;
  }
}

export class PairingCodeUnsupportedVersionError extends Schema.TaggedErrorClass<PairingCodeUnsupportedVersionError>()(
  "PairingCodeUnsupportedVersionError",
  { version: Schema.Number },
) {
  override get message(): string {
    return "This pairing code was created by a newer BiBCode. Update this app, then try again.";
  }
}

const decodePayload = Schema.decodeUnknownSync(RemotePairingCodePayload);

function base64UrlDecode(code: string): string {
  // Buffer exists in Node; browsers/webviews use atob with alphabet mapping.
  if (typeof Buffer !== "undefined") {
    return Buffer.from(code, "base64url").toString("utf8");
  }
  const base64 = code.replaceAll("-", "+").replaceAll("_", "/");
  const padded = base64 + "=".repeat((4 - (base64.length % 4)) % 4);
  const binary = atob(padded);
  const bytes = Uint8Array.from(binary, (char) => char.charCodeAt(0));
  return new TextDecoder().decode(bytes);
}

function base64UrlEncode(text: string): string {
  if (typeof Buffer !== "undefined") {
    return Buffer.from(text, "utf8").toString("base64url");
  }
  const bytes = new TextEncoder().encode(text);
  let binary = "";
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

export function encodePairingCode(payload: RemotePairingCodePayload): string {
  return base64UrlEncode(JSON.stringify(payload));
}

function extractCode(raw: string): string {
  const trimmed = raw.trim();
  if (
    trimmed.startsWith("bibcode://") ||
    trimmed.startsWith("http://") ||
    trimmed.startsWith("https://")
  ) {
    let url: URL;
    try {
      url = new URL(trimmed);
    } catch (cause) {
      throw new PairingCodeParseError({ detail: `unparsable URL (${String(cause)})` });
    }
    const code = url.searchParams.get("code");
    if (code === null || code === "") {
      throw new PairingCodeParseError({ detail: "the URL carries no code parameter" });
    }
    return code;
  }
  return trimmed;
}

export function parsePairingCode(raw: string): RemotePairingCodePayload {
  const code = extractCode(raw);
  if (!/^[A-Za-z0-9_-]+$/.test(code)) {
    throw new PairingCodeParseError({ detail: "the code is not base64url" });
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(base64UrlDecode(code));
  } catch (cause) {
    throw new PairingCodeParseError({ detail: `not base64url JSON (${String(cause)})` });
  }
  const version =
    typeof parsed === "object" && parsed !== null && "v" in parsed
      ? (parsed as { v: unknown }).v
      : null;
  if (typeof version !== "number" || !Number.isInteger(version)) {
    throw new PairingCodeParseError({ detail: "the payload has no integer version" });
  }
  if (version !== REMOTE_PAIRING_CODE_VERSION) {
    throw new PairingCodeUnsupportedVersionError({ version });
  }
  try {
    return decodePayload(parsed);
  } catch (cause) {
    throw new PairingCodeParseError({ detail: `payload shape mismatch (${String(cause)})` });
  }
}

export function buildPairingDeepLink(code: string): string {
  return `bibcode://pair?code=${code}`;
}

export function buildBrowserPairUrl(endpoint: string, code: string): string {
  const url = new URL(endpoint);
  url.pathname = "/pair";
  url.search = `code=${code}`;
  url.hash = "";
  return url.toString();
}
```

`classifyPairingEndpoint` in `packages/shared/src/advertisedEndpoint.ts`:

```ts
export type PairingEndpointClassification =
  | "loopback"
  | "private-network"
  | "public"
  | "unconnectable";

function isPrivateIpv4(host: string): boolean {
  const octets = host.split(".").map(Number);
  if (octets.length !== 4 || octets.some((octet) => Number.isNaN(octet))) {
    return false;
  }
  const [a, b] = octets as [number, number, number, number];
  return (
    a === 10 ||
    (a === 172 && b >= 16 && b <= 31) ||
    (a === 192 && b === 168) ||
    (a === 100 && b >= 64 && b <= 127) || // CGNAT, used by mesh VPN tailnets
    (a === 169 && b === 254)
  );
}

export function classifyPairingEndpoint(endpoint: string): PairingEndpointClassification {
  let url: URL;
  try {
    url = new URL(normalizeHttpBaseUrl(endpoint));
  } catch {
    return "unconnectable";
  }
  // `new URL("http://h:0")` normalizes port 0 away in some engines; check the raw string too.
  if (/:0(?:[/?#]|$)/.test(endpoint.trim())) {
    return "unconnectable";
  }
  const host = url.hostname.replace(/^\[|\]$/g, "").toLowerCase();
  if (host === "0.0.0.0" || host === "::" || host === "") {
    return "unconnectable";
  }
  if (host === "localhost" || host === "::1" || host.startsWith("127.")) {
    return "loopback";
  }
  if (isPrivateIpv4(host)) {
    return "private-network";
  }
  if (host.startsWith("fd") || host.startsWith("fc") || host.startsWith("fe80")) {
    return "private-network"; // IPv6 ULA / link-local
  }
  return "public";
}
```

Add the `"./pairingCode"` entry to `packages/shared/package.json` `exports` (same shape as `"./advertisedEndpoint"`). Verify `@bibcode/shared` declares `@bibcode/contracts` in its dependencies (it already imports it; if the manifest lacks it, add `"@bibcode/contracts": "workspace:*"`).

- [x] **Step 4: Run to verify pass**

Run (from `packages/shared`): `vp test run src/pairingCode.test.ts src/advertisedEndpoint.test.ts`
Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add packages/shared/src/pairingCode.ts packages/shared/src/pairingCode.test.ts packages/shared/src/advertisedEndpoint.ts packages/shared/src/advertisedEndpoint.test.ts packages/shared/package.json pnpm-lock.yaml
git commit -m "feat(shared): pairing-code codec and pairing endpoint classification"
```

---

### Task 8: `POST /api/auth/pairing-offer` mint endpoint (server + contracts parity chain)

Authenticated, `access:write`-scoped minting of complete pairing codes over HTTP — the pairing surface is already HTTP (`/api/auth/pairing-token`, `/api/auth/pairing-links`, `/api/auth/pairing-links/revoke` in `apps/server/src/auth/http.rs`; there are zero pairing WS methods), so the mint endpoint joins it. The server contributes the one-time token (existing `issue_pairing`), `hostKey`, and `storageInstanceId`; the caller contributes `endpoint`, `name`, `reach` (plus optional `label`, `scopes`). This is the only surface that distributes `hostKey`, and it is never reachable without authentication (spec §4.1).

Phase 5 later **modifies** this endpoint — its Tasks 1–3 add reach persistence (`issue_share_pairing` + migration) and exposure derivation, then its Task 4 swaps the issuance call here. All names below (`AuthPairingOfferResult`, `pairingOffer`, `create_pairing_offer`, fixture paths, `invalid_pairing_offer`) are pinned jointly with Phase 5's Task 4 so that phase edits, never re-creates, this surface.

**Files:**

- Modify: `packages/contracts/src/auth.ts` (`AuthCreatePairingOfferInput`, `AuthPairingOfferResult`)
- Modify: `packages/contracts/src/environmentHttp.ts` (`pairingOffer` endpoint; `"invalid_pairing_offer"` reason literal)
- Modify: `packages/contracts/src/authRustParity.test.ts` (route contract, samples, fixtures, decoders, route count)
- Modify: `packages/contracts/scripts/export-rust-auth-fixtures.ts` (route + fixture registration), then regenerate `packages/contracts/fixtures/auth-http/`
- Modify: `apps/server/src/auth/model.rs` (`CreatePairingOfferRequest`, `PairingOfferResult`)
- Modify: `apps/server/src/auth/http.rs` (route `.route("/api/auth/pairing-offer", post(create_pairing_offer))` + handler)
- Modify: `apps/server/src/http.rs` (`ROUTE_INVENTORY` entry `route(RouteMethod::Post, "/api/auth/pairing-offer")` after the pairing-token entry)
- Modify: `apps/server/tests/server_runtime.rs` (`expected_routes()` gains `("POST", "/api/auth/pairing-offer")`)
- Test: `apps/server/tests/auth_http.rs` (mint + validation + scope cases)
- Test: `apps/server/tests/e2ee_ws.rs` (full mint → pair → encrypted-session round trip)

**Interfaces:**

- Consumes: `RemotePairingReach` (Task 6 TS), `RemotePairingCodePayload` + `encode_pairing_code` (Task 6 Rust), `AuthService::{issue_pairing, host_identity}`, `is_loopback_host` (`apps/server/src/auth/service.rs:1143` — raise visibility to `pub(crate)`), `owned_scopes(STANDARD_SCOPES)` idiom from `service.rs`, `AppState.config.storage_instance_id`.
- Produces: `POST /api/auth/pairing-offer`, scope `access:write`. Request `{ name, endpoint, reach, label?, scopes? }` (`AuthCreatePairingOfferInput`); response `{ id, code, reach, endpoint, name, expiresAt }` (`AuthPairingOfferResult`) where `code` is the base64url-unpadded §4.2 JSON payload. New additive `EnvironmentRequestInvalidReason` literal `"invalid_pairing_offer"`. Phase 5's Share tab and Task 14's interop test consume the endpoint.
- **Boundary (explicit):** this endpoint accepts `reach`, validates it against the endpoint (rules below), and embeds it in the §4.2 payload — but it does **not** persist `reach` on the pairing link. Reach persistence (schema migration, `issue_share_pairing`, exposure desired-state derivation, spec §4.6) is Phase 5's Tasks 1–3; until then the grant is issued through the existing `issue_pairing`.

Validation rules (pinned jointly with Phase 5):

- `endpoint` must parse as an `http:`/`https:` URL; anything else → `invalid_pairing_offer`.
- Wildcard hosts (`0.0.0.0`, `::`) and port 0 are unconnectable → `invalid_pairing_offer`.
- `reach: "this-computer"` requires a loopback endpoint host; `reach: "another-device"` requires a non-loopback host; `reach: "custom"` accepts either.
- `name` must be non-empty after trimming.
- `scopes` defaults to the standard client scopes and is subset-checked against the caller's scopes exactly like `create_pairing_credential` does.
- **Idempotency:** the endpoint accepts an optional `Idempotency-Key` request header. A repeated request with the same key **and byte-identical input** returns the original offer (no second grant is minted); the same key with a different input is refused with `invalid_pairing_offer`. Records expire with the offer's `expiresAt` (the 5-minute pairing TTL). Phase 5's client retry policy sends this header on every mint.

- [x] **Step 1: Write the failing Rust integration tests**

Two files. **(a)** `apps/server/tests/auth_http.rs` — the HTTP contract cases, using that file's existing helpers (`start_desktop_server`, `exchange_token(&client, &handle, DESKTOP_BOOTSTRAP, <requested scopes>)`, `access_token`, `http_url`, `get_json` — read them before writing; they already exist at the top of the file). **(b)** `apps/server/tests/e2ee_ws.rs` — the full mint → pair → encrypted-session round trip, reusing Task 5's helpers (`boot()`, `admin_bearer_token`, `ws_ticket`, `read_host_public_key`, `noise_connect`) plus a small reqwest call for the mint.

`apps/server/tests/auth_http.rs`:

```rust
#[tokio::test]
async fn pairing_offer_mints_the_spec_payload_with_pinned_host_identity() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_desktop_server(&temp).await;
    let client = Client::new();
    let token_response = exchange_token(&client, &handle, DESKTOP_BOOTSTRAP, None).await;
    let token = access_token(&token_response);

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
    assert_eq!(offer["name"], "AI-SERVER");
    assert_eq!(offer["reach"], "another-device");
    assert_eq!(offer["endpoint"], "http://192.168.1.20:3773");
    assert!(offer["id"].as_str().is_some_and(|id| !id.is_empty()));
    assert!(offer["expiresAt"].as_str().is_some_and(|at| !at.is_empty()));

    let code = offer["code"].as_str().expect("offer code");
    let payload = bibcode_server::auth_pairing_code::decode_pairing_code(code).expect("decodes");
    assert_eq!(payload.v, 1);
    assert_eq!(payload.name, "AI-SERVER");
    assert_eq!(payload.endpoint, "http://192.168.1.20:3773");
    assert_eq!(
        payload.reach,
        bibcode_server::auth_pairing_code::RemotePairingReach::AnotherDevice
    );
    assert!(!payload.token.is_empty());
    assert_eq!(payload.host_key.len(), 43, "unpadded base64url of 32 bytes");
    uuid::Uuid::parse_str(&payload.storage_instance_id).expect("storage id is a UUID");

    // The embedded one-time token is a real pairing link: it must appear in the
    // pairing-links listing until consumed.
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
    assert!(
        links
            .as_array()
            .expect("link list")
            .iter()
            .any(|link| link["id"] == offer["id"]),
        "minted grant must be listed: {links}"
    );
}

#[tokio::test]
async fn pairing_offer_rejects_invalid_endpoint_and_reach_combinations() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_desktop_server(&temp).await;
    let client = Client::new();
    let token_response = exchange_token(&client, &handle, DESKTOP_BOOTSTRAP, None).await;
    let token = access_token(&token_response);

    for (endpoint, reach) in [
        ("ftp://192.168.1.20:3773", "custom"),      // non-http scheme
        ("http://0.0.0.0:3773", "custom"),          // wildcard host
        ("http://127.0.0.1:0", "custom"),           // port 0
        ("http://127.0.0.1:3773", "another-device"), // loopback offered off-host
        ("http://192.168.1.20:3773", "this-computer"), // non-loopback offered as local
    ] {
        let response = client
            .post(http_url(&handle, "/api/auth/pairing-offer"))
            .bearer_auth(token)
            .json(&json!({ "name": "X", "endpoint": endpoint, "reach": reach }))
            .send()
            .await
            .expect("invalid offer");
        let body = get_json(response, StatusCode::BAD_REQUEST).await;
        assert_eq!(body["code"], "invalid_request", "{endpoint} {reach}");
        assert_eq!(body["reason"], "invalid_pairing_offer", "{endpoint} {reach}");
    }
}

#[tokio::test]
async fn pairing_offer_replays_are_idempotent_per_key() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_desktop_server(&temp).await;
    let client = Client::new();
    let token_response = exchange_token(&client, &handle, DESKTOP_BOOTSTRAP, None).await;
    let token = access_token(&token_response);
    let body = json!({
        "name": "AI-SERVER",
        "endpoint": "http://192.168.1.20:3773",
        "reach": "another-device",
    });

    let mint = |body: Value| {
        client
            .post(http_url(&handle, "/api/auth/pairing-offer"))
            .bearer_auth(token)
            .header("idempotency-key", "retry-key-1")
            .json(&body)
            .send()
    };
    let first = get_json(mint(body.clone()).await.expect("first mint"), StatusCode::OK).await;
    let second = get_json(mint(body.clone()).await.expect("replay"), StatusCode::OK).await;
    assert_eq!(first, second, "same key + same input must replay the original offer");

    // Only one grant exists.
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
    let matching = links
        .as_array()
        .expect("link list")
        .iter()
        .filter(|link| link["id"] == first["id"])
        .count();
    assert_eq!(matching, 1);
    assert_eq!(links.as_array().unwrap().len(), 1, "no duplicate grant was minted");

    // Same key, different input: refused.
    let conflicting = mint(json!({
        "name": "OTHER",
        "endpoint": "http://192.168.1.20:3773",
        "reach": "another-device",
    }))
    .await
    .expect("conflicting mint");
    let conflict_body = get_json(conflicting, StatusCode::BAD_REQUEST).await;
    assert_eq!(conflict_body["reason"], "invalid_pairing_offer");
}

#[tokio::test]
async fn pairing_offer_requires_the_access_write_scope() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_desktop_server(&temp).await;
    let client = Client::new();
    // Exchange a session limited to the standard client scopes (no access:write).
    let limited_response = exchange_token(
        &client,
        &handle,
        DESKTOP_BOOTSTRAP,
        Some("orchestration:read orchestration:operate terminal:operate review:write relay:read"),
    )
    .await;
    let limited = access_token(&limited_response);

    let response = client
        .post(http_url(&handle, "/api/auth/pairing-offer"))
        .bearer_auth(limited)
        .json(&json!({
            "name": "X",
            "endpoint": "http://192.168.1.20:3773",
            "reach": "another-device",
        }))
        .send()
        .await
        .expect("scope-limited offer");
    let body = get_json(response, StatusCode::FORBIDDEN).await;
    assert_eq!(body["reason"], "access:write");
}
```

(Adapt the `exchange_token` scope argument to that helper's real signature — the file already exchanges scope-limited tokens in its scope tests; copy the exact idiom, and match the 403 body assertion to the `EnvironmentScopeRequiredError` shape the existing scope tests assert.)

`apps/server/tests/e2ee_ws.rs` — the round trip, minting over HTTP:

```rust
async fn mint_pairing_offer(
    handle: &bibcode_server::ServerHandle,
    bearer: &str,
    body: Value,
) -> Value {
    let response = reqwest::Client::new()
        .post(format!("http://{}/api/auth/pairing-offer", handle.local_addr()))
        .bearer_auth(bearer)
        .json(&body)
        .send()
        .await
        .expect("pairing offer request");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    response.json().await.expect("pairing offer json")
}

#[tokio::test]
async fn minted_pairing_offer_pins_the_host_key_and_opens_the_e2ee_channel() {
    let (root, handle) = boot().await;
    let admin_token = admin_bearer_token(&handle).await;

    let offer = mint_pairing_offer(
        &handle,
        &admin_token,
        json!({ "endpoint": "http://127.0.0.1:3773", "name": "Test Host", "reach": "custom" }),
    )
    .await;
    let code = offer["code"].as_str().expect("code");
    let payload = bibcode_server::auth_pairing_code::decode_pairing_code(code).expect("decodes");
    assert_eq!(payload.v, 1);
    assert_eq!(payload.name, "Test Host");
    assert_eq!(payload.reach, bibcode_server::auth_pairing_code::RemotePairingReach::Custom);
    let expected_public = read_host_public_key(root.path());
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    assert_eq!(URL_SAFE_NO_PAD.decode(&payload.host_key).unwrap(), expected_public);

    // Complete the loop entirely inside the channel: the one-time token rides
    // e2ee_auth's pairing form — no /oauth/token or ticket HTTP round-trips.
    let host_key = URL_SAFE_NO_PAD.decode(&payload.host_key).unwrap();
    let (mut socket, mut transport) = noise_connect(handle.local_addr(), &host_key).await;
    send_encrypted(
        &mut socket,
        &mut transport,
        format!("{{\"type\":\"e2ee_auth\",\"pairing\":\"{}\"}}", payload.token).as_bytes(),
    )
    .await;
    let authenticated = recv_encrypted_json(&mut socket, &mut transport).await;
    assert_eq!(authenticated["type"], "e2ee_authenticated");
    assert!(
        authenticated["credential"]
            .as_str()
            .is_some_and(|credential| !credential.is_empty()),
        "pairing form must return the in-channel-minted bearer: {authenticated}"
    );
    assert_eq!(
        authenticated["storageInstanceId"].as_str().unwrap(),
        payload.storage_instance_id,
        "authenticated identity must match the pairing payload"
    );
    send_encrypted(
        &mut socket,
        &mut transport,
        json!({ "_tag": "Request", "id": "2", "tag": "server.getConfig", "payload": {}, "headers": [] })
            .to_string()
            .as_bytes(),
    )
    .await;
    let reply = recv_encrypted_json(&mut socket, &mut transport).await;
    assert_eq!(reply["requestId"].as_str(), Some("2"));
    handle.shutdown();
}
```

`send_encrypted` (record-split + `transport.write_message` + Binary frame) and `recv_encrypted_json` (Binary frames → `transport.read_message` → record reassembly → `serde_json::Value`) are small helpers alongside Task 5's; write them once in the file and reuse. The encrypted reply envelope is the `ServerMessage` JSON from `apps/server/src/rpc/message.rs` (`{"_tag":"Exit","requestId":...}`) — match whatever the plain-`/ws` tests in `activity_rpc.rs` destructure.

- [x] **Step 2: Run to verify failure**

Run: `cargo test -p bibcode-server --test auth_http pairing_offer && cargo test -p bibcode-server --test e2ee_ws minted_pairing_offer`
Expected: FAIL — 404 responses (route missing).

- [x] **Step 3: Implement server side**

`apps/server/src/auth/service.rs`: change `fn is_loopback_host` (line 1143) to `pub(crate) fn is_loopback_host` so the handler can reuse it.

`apps/server/src/auth/model.rs` (next to the other request/response structs):

```rust
#[derive(Clone, Debug, Deserialize)]
pub struct CreatePairingOfferRequest {
    pub name: String,
    pub endpoint: String,
    pub reach: String,
    pub label: Option<String>,
    pub scopes: Option<Vec<String>>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingOfferResult {
    pub id: String,
    pub code: String,
    pub reach: String,
    pub endpoint: String,
    pub name: String,
    pub expires_at: String,
}
```

`apps/server/src/auth/http.rs` — add `.route("/api/auth/pairing-offer", post(create_pairing_offer))` to `add_routes` (after the `pairing-token` route) and the handler, mirroring the auth/scope validation of the existing `pairing_token` handler:

```rust
async fn create_pairing_offer(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
    Json(payload): Json<CreatePairingOfferRequest>,
) -> Response {
    let principal =
        match authenticated_with_scope(&state.auth, &headers, &uri, SCOPE_ACCESS_WRITE).await {
            Ok(principal) => principal,
            Err(error) => {
                return auth_error_for_request(error, &headers, "pairing_offer_issuance_failed");
            }
        };
    let endpoint_raw = payload.endpoint.trim();
    let endpoint = match url::Url::parse(endpoint_raw) {
        Ok(url) if matches!(url.scheme(), "http" | "https") => url,
        _ => return invalid_pairing_offer_response("endpoint must be an http(s) URL"),
    };
    let host = endpoint.host_str().unwrap_or_default();
    if matches!(host, "" | "0.0.0.0" | "[::]" | "::") || endpoint.port() == Some(0) {
        return invalid_pairing_offer_response(
            "endpoint must be a connectable address (no wildcard host, no port 0)",
        );
    }
    let endpoint_is_loopback = is_loopback_host(host);
    let reach_ok = match payload.reach.as_str() {
        "this-computer" => endpoint_is_loopback,
        "another-device" => !endpoint_is_loopback,
        "custom" => true,
        _ => false,
    };
    let name = payload.name.trim().to_owned();
    if !reach_ok || name.is_empty() {
        return invalid_pairing_offer_response("reach does not match the offered endpoint");
    }
    let scopes = payload
        .scopes
        .unwrap_or_else(|| owned_scopes(STANDARD_SCOPES));
    if !scopes
        .iter()
        .all(|scope| principal.scopes.iter().any(|granted| granted == scope))
    {
        return invalid_pairing_offer_response("requested scope exceeds the caller's grant");
    }
    let Some(storage_instance_id) = state.config.storage_instance_id else {
        return auth_error_for_request(
            AuthError::Internal("storage identity is unavailable".to_owned()),
            &headers,
            "pairing_offer_issuance_failed",
        );
    };
    // Phase 3 boundary: reach is validated and embedded in the code but NOT
    // persisted on the grant. Phase 5 replaces this call with issue_share_pairing
    // (reach persistence + exposure derivation, spec section 4.6).
    let label = payload
        .label
        .as_deref()
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| name.clone());
    let issued = match state.auth.issue_pairing(scopes, Some(label)).await {
        Ok(issued) => issued,
        Err(error) => {
            return auth_error_for_request(error, &headers, "pairing_offer_issuance_failed");
        }
    };
    let code_payload = RemotePairingCodePayload {
        v: REMOTE_PAIRING_CODE_VERSION,
        endpoint: endpoint_raw.trim_end_matches('/').to_owned(),
        name: name.clone(),
        token: issued.credential.clone(),
        host_key: state.auth.host_identity().public_key_base64url(),
        reach: match payload.reach.as_str() {
            "this-computer" => RemotePairingReach::ThisComputer,
            "another-device" => RemotePairingReach::AnotherDevice,
            _ => RemotePairingReach::Custom,
        },
        storage_instance_id: storage_instance_id.to_string(),
    };
    let code = match encode_pairing_code(&code_payload) {
        Ok(code) => code,
        Err(error) => {
            // Do not leave a live grant behind a failed offer.
            let _ = state.auth.revoke_pairing(&issued.id).await;
            return auth_error_for_request(
                AuthError::Internal(format!("pairing code encoding failed: {error}")),
                &headers,
                "pairing_offer_issuance_failed",
            );
        }
    };
    Json(PairingOfferResult {
        id: issued.id,
        code,
        reach: payload.reach,
        endpoint: code_payload.endpoint,
        name,
        expires_at: issued.expires_at,
    })
    .into_response()
}

fn invalid_pairing_offer_response(detail: &str) -> Response {
    let trace_id = Uuid::new_v4().to_string();
    tracing::debug!(target: "bibcode_server::auth", %trace_id, "invalid pairing offer: {detail}");
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "code": "invalid_request",
            "reason": "invalid_pairing_offer",
            "traceId": trace_id,
        })),
    )
        .into_response()
}
```

**Idempotency wiring** (insert at the top of the handler, right after the scope check, and at the bottom, right before the success return):

```rust
    // -- after the scope check --
    let idempotency_key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .filter(|key| !key.trim().is_empty())
        .map(str::to_owned);
    // Fingerprint the *raw* validated input so any difference conflicts.
    let input_fingerprint = format!(
        "{}\n{}\n{}\n{:?}\n{:?}",
        payload.name.trim(),
        payload.endpoint.trim(),
        payload.reach,
        payload.label,
        payload.scopes,
    );
    if let Some(key) = &idempotency_key {
        match state.auth.replay_pairing_offer(key, &input_fingerprint).await {
            PairingOfferReplay::Original(original) => return Json(original).into_response(),
            PairingOfferReplay::Conflict => {
                return invalid_pairing_offer_response(
                    "idempotency key was already used with a different input",
                );
            }
            PairingOfferReplay::Fresh => {}
        }
    }

    // -- right before `Json(PairingOfferResult { ... }).into_response()` --
    let result = PairingOfferResult { /* fields as above */ };
    if let Some(key) = idempotency_key {
        state
            .auth
            .record_pairing_offer(key, input_fingerprint, result.clone())
            .await;
    }
    Json(result).into_response()
```

with the small store on `AuthService` (`auth/service.rs`; entries pruned lazily against `expires_at` on every access, keyed by idempotency key):

```rust
pub(crate) enum PairingOfferReplay {
    Original(PairingOfferResult),
    Conflict,
    Fresh,
}
// AuthState gains: pairing_offer_idempotency: HashMap<String, StoredPairingOffer>
// where StoredPairingOffer { input_fingerprint: String, result: PairingOfferResult,
// expires_at_ms: i64 }. replay_pairing_offer(key, fingerprint) prunes expired
// entries, then: hit + same fingerprint -> Original(result.clone()); hit + different
// fingerprint -> Conflict; miss -> Fresh. record_pairing_offer inserts with
// expires_at_ms parsed from result.expires_at.
```

(Write the two methods out in full following the surrounding `AuthService` state-mutex idioms; `PairingOfferResult` needs `Clone`, already derived above.)

Match this file's existing imports and idioms exactly (`owned_scopes`, `STANDARD_SCOPES`, `SCOPE_ACCESS_WRITE`, `authenticated_with_scope`, `auth_error_for_request`, trace-id construction — copy the surrounding handlers' patterns; adjust visibility of `owned_scopes`/`STANDARD_SCOPES` if they are private to `service.rs`/`model.rs`). `issued.id`/`issued.credential`/`issued.expires_at` come from `PairingCredentialResult` (`model.rs:105`). The field shape of `PairingOfferResult` and the `invalid_pairing_offer` body are pinned jointly with Phase 5 — do not rename.

`apps/server/src/http.rs`: add `route(RouteMethod::Post, "/api/auth/pairing-offer"),` to `ROUTE_INVENTORY` directly after the `"/api/auth/pairing-token"` entry, and add `("POST", "/api/auth/pairing-offer")` at the same position in `expected_routes()` in `apps/server/tests/server_runtime.rs`. (POST classifies as `RpcMutability::Mutation` automatically in `http_mutability` — correct for a grant-creating endpoint; no `maintenance.rs` change.)

Note: the 5-minute pairing TTL (`PAIRING_TTL_MS`, `service.rs:35`) applies unchanged — `expiresAt` in the result is what Phase 5 uses for its regenerate UX.

- [x] **Step 4: Implement the contracts side and regenerate parity fixtures**

`packages/contracts/src/auth.ts` (import `RemotePairingReach` from `./remotePairing.ts`; place next to `AuthCreatePairingCredentialInput` / `AuthPairingCredentialResult`):

```ts
export const AuthCreatePairingOfferInput = Schema.Struct({
  name: TrimmedNonEmptyString,
  endpoint: TrimmedNonEmptyString,
  reach: RemotePairingReach,
  label: Schema.optionalKey(TrimmedNonEmptyString),
  scopes: Schema.optionalKey(AuthEnvironmentScopes),
});
export type AuthCreatePairingOfferInput = typeof AuthCreatePairingOfferInput.Type;

export const AuthPairingOfferResult = Schema.Struct({
  id: TrimmedNonEmptyString,
  code: TrimmedNonEmptyString,
  reach: RemotePairingReach,
  endpoint: TrimmedNonEmptyString,
  name: TrimmedNonEmptyString,
  expiresAt: Schema.DateTimeUtc,
});
export type AuthPairingOfferResult = typeof AuthPairingOfferResult.Type;
```

`packages/contracts/src/environmentHttp.ts`:

1. Add `"invalid_pairing_offer"` to the `EnvironmentRequestInvalidReason` literals (line 53 — additive), and `"pairing_offer_issuance_failed"` to the `EnvironmentInternalErrorReason` literals (the handler's `auth_error_for_request` reason string; both changes alter those error schemas' fingerprints, which the regenerated manifest absorbs).
2. Add the endpoint to the auth group, directly after the `pairingCredential` endpoint (identifier, path, payload/success/error schemas pinned jointly with Phase 5; the headers struct additionally carries the idempotency key, which Phase 5's retry policy sends):

```ts
const PairingOfferHeaders = Schema.Struct({
  authorization: Schema.optionalKey(Schema.String),
  dpop: Schema.optionalKey(Schema.String),
  "idempotency-key": Schema.optionalKey(Schema.String),
});

.add(
  HttpApiEndpoint.post("pairingOffer", "/api/auth/pairing-offer", {
    headers: PairingOfferHeaders,
    payload: AuthCreatePairingOfferInput,
    success: AuthPairingOfferResult,
    error: EnvironmentPairingCredentialErrors,
  }).middleware(EnvironmentAuthenticatedAuth),
)
```

`packages/contracts/src/authRustParity.test.ts` — extend the pinned contract:

1. `authRouteContract` gains (after the `pairingCredential` entry):

```ts
  {
    name: "pairingOffer",
    method: "POST",
    path: "/api/auth/pairing-offer",
    requestContentTypes: ["application/json"],
    successStatuses: [200],
    errorStatuses: [400, 401, 403, 500],
  },
```

2. `expect(manifest.routes).toHaveLength(10)` becomes `11`.
3. `namedSchemas` gains `AuthCreatePairingOfferInput` and `AuthPairingOfferResult` (imported from `./auth.ts`).
4. `expectedSamples` gains `pairingOffer: { request: "requests/pairing-offer.json", success: "responses/pairing-offer.json" }`; `expectedFixtures` gains both paths (keep the list sorted as-is).
5. `fixtureDecoders` gains both fixtures decoding through `Schema.toCodecJson(AuthCreatePairingOfferInput)` / `Schema.toCodecJson(AuthPairingOfferResult)`.

`packages/contracts/scripts/export-rust-auth-fixtures.ts` — register the route name `"pairingOffer"` in its route list (line ~59 area) and add the two fixtures with `addJsonFixture`, mirroring the `pairing-create` entries (line ~313):

```ts
addJsonFixture("requests/pairing-offer.json", AuthCreatePairingOfferInput, {
  name: "AI-SERVER",
  endpoint: "http://192.168.1.20:3773",
  reach: "another-device",
  label: "Tablet",
});
addJsonFixture("responses/pairing-offer.json", AuthPairingOfferResult, {
  id: "4f8f2f2e-0000-4000-8000-000000000000",
  code: "eyJ2IjoxfQ",
  reach: "another-device",
  endpoint: "http://192.168.1.20:3773",
  name: "AI-SERVER",
  expiresAt: "2026-08-27T01:00:00.000Z",
});
```

(Fixture values are pinned jointly with Phase 5; adapt the `addJsonFixture`/samples registration calls to the exporter's exact helper signatures, and update its `expectedSamples` mirror if it keeps one.) Then regenerate:

```bash
pnpm --filter @bibcode/contracts run generate:rust-auth-fixtures
```

and read the fixture diff: only `pairing-offer` entries, the route addition, and the changed `EnvironmentRequestInvalidError` fingerprint (the new reason literal) may appear.

- [x] **Step 5: Run everything to verify green**

Run:

```bash
cargo test -p bibcode-server --test auth_http
cargo test -p bibcode-server --test e2ee_ws
cargo test -p bibcode-server --test server_runtime
cargo test -p bibcode-server --test production_maintenance
cd packages/contracts && vp test run src/authRustParity.test.ts src/auth.test.ts src/environmentHttp.test.ts
```

Expected: PASS. `auth_http.rs`'s own fixture-inventory test (`language_neutral_auth_fixtures_match_the_rust_http_inventory`) and the TS parity manifest both cover the new route — if either fails, the endpoint declaration, exporter, and fixtures are out of sync.

- [x] **Step 6: Commit**

```bash
git add packages/contracts/src/auth.ts packages/contracts/src/environmentHttp.ts packages/contracts/src/authRustParity.test.ts packages/contracts/scripts/export-rust-auth-fixtures.ts packages/contracts/fixtures/auth-http apps/server/src/auth/model.rs apps/server/src/auth/http.rs apps/server/src/auth/service.rs apps/server/src/http.rs apps/server/tests/auth_http.rs apps/server/tests/server_runtime.rs apps/server/tests/e2ee_ws.rs
git commit -m "feat(server,contracts): mint bibcode://pair offers over POST /api/auth/pairing-offer"
```

---

### Task 9: TypeScript Noise NK module with official test vectors

Pure crypto module: NK initiator (production) and responder (used by unit tests and available to future work), validated against official Noise test vectors so the primitives are proven independently of our own responder.

**Files:**

- Create: `packages/client-runtime/src/e2ee/noise.ts`
- Create: `packages/client-runtime/src/e2ee/noise.test.ts`
- Create: `packages/client-runtime/src/e2ee/index.ts` (re-export the module surface for in-package use)

**Interfaces:**

- Consumes: noble packages pinned in Task 1.
- Produces:

```ts
export const NOISE_NK_PROTOCOL_NAME = "Noise_NK_25519_ChaChaPoly_SHA256"; // exactly 32 bytes
export const MAX_NOISE_MESSAGE_BYTES = 65535;
export const NOISE_TAG_BYTES = 16;
export class NoiseAuthenticationError extends Error {} // AEAD failure
export class NonceExhaustedError extends Error {}
export class NoiseProtocolError extends Error {} // wrong-length keys/messages, wrong phase
export interface NoiseCipherState {
  encryptWithAd(ad: Uint8Array, plaintext: Uint8Array): Uint8Array; // throws NonceExhaustedError
  decryptWithAd(ad: Uint8Array, ciphertext: Uint8Array): Uint8Array; // throws NoiseAuthenticationError
}
export interface NkTransport {
  readonly send: NoiseCipherState; // this side -> peer
  readonly receive: NoiseCipherState; // peer -> this side
  readonly handshakeHash: Uint8Array;
}
export interface NkInitiator {
  writeMessageA(payload: Uint8Array): Uint8Array;
  readMessageB(message: Uint8Array): Uint8Array; // returns handshake payload
  split(): NkTransport;
}
export interface NkResponder {
  readMessageA(message: Uint8Array): Uint8Array;
  writeMessageB(payload: Uint8Array): Uint8Array;
  split(): NkTransport; // send = responder->initiator, receive = initiator->responder
}
export function createNkInitiator(options: {
  responderStaticPublicKey: Uint8Array; // 32 bytes
  prologue?: Uint8Array; // default empty
  ephemeralPrivateKey?: Uint8Array; // test injection only
}): NkInitiator;
export function createNkResponder(options: {
  staticPrivateKey: Uint8Array;
  prologue?: Uint8Array;
  ephemeralPrivateKey?: Uint8Array;
}): NkResponder;
export function decodeBase64UrlKey(encoded: string): Uint8Array; // throws NoiseProtocolError unless 32 bytes
```

Tasks 10, 11, 14 consume these.

- [x] **Step 1: Obtain the official NK vector (fetch-select-embed; do not fabricate bytes)**

The snow crate vendors the cacophony vector corpus. Fetch it and extract the NK entry (URL current as of writing — verify at implementation time; the corpus also exists in the cacophony project itself):

```bash
curl -fsSL https://raw.githubusercontent.com/mcginty/snow/master/tests/vectors/cacophony.json \
  -o /tmp/cacophony.json
node -e '
  const { vectors } = JSON.parse(require("fs").readFileSync("/tmp/cacophony.json", "utf8"));
  const vector = vectors.find((entry) =>
    (entry.protocol_name ?? entry.name) === "Noise_NK_25519_ChaChaPoly_SHA256");
  console.log(JSON.stringify(vector, null, 2));
'
```

Embed the printed vector verbatim as a `const OFFICIAL_NK_VECTOR = {...} as const;` in `noise.test.ts` with a comment naming the source URL and retrieval date. The vector supplies hex fields: `init_prologue`, `init_ephemeral`, `init_remote_static`, `resp_static`, `resp_ephemeral`, `handshake_hash`, and `messages: [{ payload, ciphertext }, ...]` (message 0 = A, message 1 = B, 2+ = transport, alternating initiator/responder). If the file layout differs from this description, adapt the test's field access to the actual layout — the assertions below are what matters.

- [x] **Step 2: Write the failing tests**

`packages/client-runtime/src/e2ee/noise.test.ts`:

```ts
import { describe, expect, it } from "@effect/vitest";

import {
  MAX_NOISE_MESSAGE_BYTES,
  NoiseAuthenticationError,
  NonceExhaustedError,
  createNkInitiator,
  createNkResponder,
  decodeBase64UrlKey,
} from "./noise.ts";

const hex = (value: string): Uint8Array =>
  value === "" ? new Uint8Array(0) : Uint8Array.from(Buffer.from(value, "hex"));
const toHex = (bytes: Uint8Array): string => Buffer.from(bytes).toString("hex");

const OFFICIAL_NK_VECTOR = /* embedded in Step 1 */;

describe("Noise NK against the official vector", () => {
  it("initiator reproduces message A and decrypts message B", () => {
    const initiator = createNkInitiator({
      responderStaticPublicKey: hex(OFFICIAL_NK_VECTOR.init_remote_static),
      prologue: hex(OFFICIAL_NK_VECTOR.init_prologue),
      ephemeralPrivateKey: hex(OFFICIAL_NK_VECTOR.init_ephemeral),
    });
    const messageA = initiator.writeMessageA(hex(OFFICIAL_NK_VECTOR.messages[0].payload));
    expect(toHex(messageA)).toBe(OFFICIAL_NK_VECTOR.messages[0].ciphertext);
    const payloadB = initiator.readMessageB(hex(OFFICIAL_NK_VECTOR.messages[1].ciphertext));
    expect(toHex(payloadB)).toBe(OFFICIAL_NK_VECTOR.messages[1].payload);
    const transport = initiator.split();
    expect(toHex(transport.handshakeHash)).toBe(OFFICIAL_NK_VECTOR.handshake_hash);
    // Transport messages: even offsets from index 2 are initiator->responder.
    for (let index = 2; index < OFFICIAL_NK_VECTOR.messages.length; index += 1) {
      const message = OFFICIAL_NK_VECTOR.messages[index];
      const fromInitiator = index % 2 === 0;
      const cipher = fromInitiator ? transport.send : transport.receive;
      const produced = fromInitiator
        ? cipher.encryptWithAd(new Uint8Array(0), hex(message.payload))
        : cipher.decryptWithAd(new Uint8Array(0), hex(message.ciphertext));
      expect(toHex(produced)).toBe(fromInitiator ? message.ciphertext : message.payload);
    }
  });

  it("responder reproduces the vector from the other side", () => {
    const responder = createNkResponder({
      staticPrivateKey: hex(OFFICIAL_NK_VECTOR.resp_static),
      prologue: hex(OFFICIAL_NK_VECTOR.init_prologue),
      ephemeralPrivateKey: hex(OFFICIAL_NK_VECTOR.resp_ephemeral),
    });
    const payloadA = responder.readMessageA(hex(OFFICIAL_NK_VECTOR.messages[0].ciphertext));
    expect(toHex(payloadA)).toBe(OFFICIAL_NK_VECTOR.messages[0].payload);
    const messageB = responder.writeMessageB(hex(OFFICIAL_NK_VECTOR.messages[1].payload));
    expect(toHex(messageB)).toBe(OFFICIAL_NK_VECTOR.messages[1].ciphertext);
  });
});

describe("Noise NK self round-trip", () => {
  const establish = () => {
    const responderStatic = /* x25519 secret */ crypto.getRandomValues(new Uint8Array(32));
    // use the module's own key derivation to get the matching public key:
    const responder = createNkResponder({ staticPrivateKey: responderStatic });
    const initiator = createNkInitiator({
      responderStaticPublicKey: publicKeyOf(responderStatic), // export a helper or use x25519 directly in the test
    });
    responder.readMessageA(initiator.writeMessageA(new Uint8Array(0)));
    initiator.readMessageB(responder.writeMessageB(new Uint8Array(0)));
    return { client: initiator.split(), server: responder.split() };
  };

  it("round-trips transport messages both directions", () => {
    const { client, server } = establish();
    const empty = new Uint8Array(0);
    const outbound = client.send.encryptWithAd(empty, Uint8Array.from([1, 2, 3]));
    expect(server.receive.decryptWithAd(empty, outbound)).toEqual(Uint8Array.from([1, 2, 3]));
    const inbound = server.send.encryptWithAd(empty, Uint8Array.from([4, 5]));
    expect(client.receive.decryptWithAd(empty, inbound)).toEqual(Uint8Array.from([4, 5]));
  });

  it("wrong pinned key makes message B fail authentication", () => {
    const responder = createNkResponder({
      staticPrivateKey: crypto.getRandomValues(new Uint8Array(32)),
    });
    const initiator = createNkInitiator({
      responderStaticPublicKey: crypto.getRandomValues(new Uint8Array(32)),
    });
    const messageA = initiator.writeMessageA(new Uint8Array(0));
    expect(() => responder.readMessageA(messageA)).toThrow(NoiseAuthenticationError);
  });

  it("tampered transport frames fail authentication", () => {
    const { client, server } = establish();
    const empty = new Uint8Array(0);
    const frame = client.send.encryptWithAd(empty, Uint8Array.from([9]));
    frame[frame.length - 1] ^= 1;
    expect(() => server.receive.decryptWithAd(empty, frame)).toThrow(NoiseAuthenticationError);
  });

  it("exhausted nonce counters refuse to encrypt", () => {
    const { client } = establish();
    (client.send as { nonce: bigint }).nonce = (1n << 64n) - 1n; // test seam: expose the counter
    expect(() => client.send.encryptWithAd(new Uint8Array(0), new Uint8Array(1))).toThrow(
      NonceExhaustedError,
    );
  });

  it("decodes base64url host keys and rejects wrong lengths", () => {
    expect(decodeBase64UrlKey("HcMLXPPBHFNvcbHrCVMH-DMh49rd5AGCzSCqAVJ49hM")).toHaveLength(32);
    expect(() => decodeBase64UrlKey("dG9vLXNob3J0")).toThrow();
  });
});
```

(Resolve `publicKeyOf` by exporting a small `derivePublicKey(privateKey: Uint8Array): Uint8Array` from `noise.ts` — it is also useful for diagnostics.)

- [x] **Step 3: Run to verify failure**

Run (from `packages/client-runtime`): `vp test run src/e2ee/noise.test.ts`
Expected: FAIL — module missing.

- [x] **Step 4: Implement `noise.ts`**

Full implementation (Noise revision 34 semantics; HKDF is the Noise two-output construction):

```ts
import { chacha20poly1305 } from "@noble/ciphers/chacha.js";
import { x25519 } from "@noble/curves/ed25519.js";
import { hmac } from "@noble/hashes/hmac.js";
import { sha256 } from "@noble/hashes/sha2.js";

export const NOISE_NK_PROTOCOL_NAME = "Noise_NK_25519_ChaChaPoly_SHA256";
export const MAX_NOISE_MESSAGE_BYTES = 65535;
export const NOISE_TAG_BYTES = 16;
const DH_BYTES = 32;
const EMPTY = new Uint8Array(0);
const MAX_NONCE = (1n << 64n) - 1n; // reserved by the Noise spec

export class NoiseProtocolError extends Error {}
export class NoiseAuthenticationError extends Error {}
export class NonceExhaustedError extends Error {
  constructor() {
    super("Noise nonce counter exhausted; the connection must be re-established.");
  }
}

const encoder = new TextEncoder();

function concat(...parts: ReadonlyArray<Uint8Array>): Uint8Array {
  const total = parts.reduce((sum, part) => sum + part.length, 0);
  const out = new Uint8Array(total);
  let offset = 0;
  for (const part of parts) {
    out.set(part, offset);
    offset += part.length;
  }
  return out;
}

function hkdf2(chainingKey: Uint8Array, ikm: Uint8Array): [Uint8Array, Uint8Array] {
  const tempKey = hmac(sha256, chainingKey, ikm);
  const output1 = hmac(sha256, tempKey, Uint8Array.of(0x01));
  const output2 = hmac(sha256, tempKey, concat(output1, Uint8Array.of(0x02)));
  return [output1, output2];
}

export interface NoiseCipherState {
  encryptWithAd(ad: Uint8Array, plaintext: Uint8Array): Uint8Array;
  decryptWithAd(ad: Uint8Array, ciphertext: Uint8Array): Uint8Array;
}

class CipherState implements NoiseCipherState {
  private key: Uint8Array | null = null;
  nonce = 0n; // exposed for the nonce-exhaustion test seam

  initializeKey(key: Uint8Array): void {
    this.key = key;
    this.nonce = 0n;
  }

  get hasKey(): boolean {
    return this.key !== null;
  }

  private nonceBytes(): Uint8Array {
    // ChaChaPoly Noise nonce: 4 zero bytes ++ 8-byte little-endian counter.
    const bytes = new Uint8Array(12);
    new DataView(bytes.buffer).setBigUint64(4, this.nonce, true);
    return bytes;
  }

  encryptWithAd(ad: Uint8Array, plaintext: Uint8Array): Uint8Array {
    if (this.key === null) {
      return plaintext;
    }
    if (this.nonce >= MAX_NONCE) {
      throw new NonceExhaustedError();
    }
    const ciphertext = chacha20poly1305(this.key, this.nonceBytes(), ad).encrypt(plaintext);
    this.nonce += 1n;
    return ciphertext;
  }

  decryptWithAd(ad: Uint8Array, ciphertext: Uint8Array): Uint8Array {
    if (this.key === null) {
      return ciphertext;
    }
    if (this.nonce >= MAX_NONCE) {
      throw new NonceExhaustedError();
    }
    let plaintext: Uint8Array;
    try {
      plaintext = chacha20poly1305(this.key, this.nonceBytes(), ad).decrypt(ciphertext);
    } catch (cause) {
      throw new NoiseAuthenticationError(`AEAD authentication failed: ${String(cause)}`);
    }
    this.nonce += 1n;
    return plaintext;
  }
}

class SymmetricState {
  chainingKey: Uint8Array;
  hash: Uint8Array;
  readonly cipher = new CipherState();

  constructor() {
    const name = encoder.encode(NOISE_NK_PROTOCOL_NAME);
    // The protocol name is exactly 32 bytes, so h = name (no hashing/padding needed).
    this.hash = name;
    this.chainingKey = name.slice();
  }

  mixHash(data: Uint8Array): void {
    this.hash = sha256(concat(this.hash, data));
  }

  mixKey(ikm: Uint8Array): void {
    const [chainingKey, tempKey] = hkdf2(this.chainingKey, ikm);
    this.chainingKey = chainingKey;
    this.cipher.initializeKey(tempKey);
  }

  encryptAndHash(plaintext: Uint8Array): Uint8Array {
    const ciphertext = this.cipher.encryptWithAd(this.hash, plaintext);
    this.mixHash(ciphertext);
    return ciphertext;
  }

  decryptAndHash(ciphertext: Uint8Array): Uint8Array {
    const plaintext = this.cipher.decryptWithAd(this.hash, ciphertext);
    this.mixHash(ciphertext);
    return plaintext;
  }

  split(): [CipherState, CipherState] {
    const [key1, key2] = hkdf2(this.chainingKey, EMPTY);
    const first = new CipherState();
    first.initializeKey(key1);
    const second = new CipherState();
    second.initializeKey(key2);
    return [first, second];
  }
}

function requireKey(bytes: Uint8Array, label: string): Uint8Array {
  if (bytes.length !== DH_BYTES) {
    throw new NoiseProtocolError(`${label} must be ${DH_BYTES} bytes, got ${bytes.length}`);
  }
  return bytes;
}

export function derivePublicKey(privateKey: Uint8Array): Uint8Array {
  return x25519.getPublicKey(requireKey(privateKey, "private key"));
}

export interface NkTransport {
  readonly send: NoiseCipherState;
  readonly receive: NoiseCipherState;
  readonly handshakeHash: Uint8Array;
}

export interface NkInitiator {
  writeMessageA(payload: Uint8Array): Uint8Array;
  readMessageB(message: Uint8Array): Uint8Array;
  split(): NkTransport;
}

export interface NkResponder {
  readMessageA(message: Uint8Array): Uint8Array;
  writeMessageB(payload: Uint8Array): Uint8Array;
  split(): NkTransport;
}

// NK: pre-message "<- s"; message A "-> e, es"; message B "<- e, ee".
export function createNkInitiator(options: {
  responderStaticPublicKey: Uint8Array;
  prologue?: Uint8Array;
  ephemeralPrivateKey?: Uint8Array;
}): NkInitiator {
  const rs = requireKey(options.responderStaticPublicKey, "responder static public key");
  const state = new SymmetricState();
  state.mixHash(options.prologue ?? EMPTY);
  state.mixHash(rs); // pre-message: <- s
  const ephemeralPrivate = options.ephemeralPrivateKey
    ? requireKey(options.ephemeralPrivateKey, "ephemeral private key").slice()
    : x25519.utils.randomSecretKey();
  const ephemeralPublic = x25519.getPublicKey(ephemeralPrivate);
  let phase: "a" | "b" | "done" = "a";

  return {
    writeMessageA(payload) {
      if (phase !== "a") throw new NoiseProtocolError("message A already written");
      state.mixHash(ephemeralPublic); // e
      state.mixKey(x25519.getSharedSecret(ephemeralPrivate, rs)); // es
      const ciphertext = state.encryptAndHash(payload);
      phase = "b";
      return concat(ephemeralPublic, ciphertext);
    },
    readMessageB(message) {
      if (phase !== "b") throw new NoiseProtocolError("message B out of order");
      if (message.length < DH_BYTES + NOISE_TAG_BYTES) {
        throw new NoiseProtocolError("message B is too short");
      }
      const re = message.slice(0, DH_BYTES);
      state.mixHash(re); // e
      state.mixKey(x25519.getSharedSecret(ephemeralPrivate, re)); // ee
      const payload = state.decryptAndHash(message.slice(DH_BYTES));
      phase = "done";
      return payload;
    },
    split() {
      if (phase !== "done") throw new NoiseProtocolError("handshake incomplete");
      const [send, receive] = state.split(); // initiator: first key sends
      return { send, receive, handshakeHash: state.hash };
    },
  };
}

export function createNkResponder(options: {
  staticPrivateKey: Uint8Array;
  prologue?: Uint8Array;
  ephemeralPrivateKey?: Uint8Array;
}): NkResponder {
  const staticPrivate = requireKey(options.staticPrivateKey, "static private key").slice();
  const staticPublic = x25519.getPublicKey(staticPrivate);
  const state = new SymmetricState();
  state.mixHash(options.prologue ?? EMPTY);
  state.mixHash(staticPublic); // pre-message: <- s
  const ephemeralPrivate = options.ephemeralPrivateKey
    ? requireKey(options.ephemeralPrivateKey, "ephemeral private key").slice()
    : x25519.utils.randomSecretKey();
  const ephemeralPublic = x25519.getPublicKey(ephemeralPrivate);
  let remoteEphemeral: Uint8Array | null = null;
  let phase: "a" | "b" | "done" = "a";

  return {
    readMessageA(message) {
      if (phase !== "a") throw new NoiseProtocolError("message A out of order");
      if (message.length < DH_BYTES + NOISE_TAG_BYTES) {
        throw new NoiseProtocolError("message A is too short");
      }
      remoteEphemeral = message.slice(0, DH_BYTES);
      state.mixHash(remoteEphemeral); // e
      state.mixKey(x25519.getSharedSecret(staticPrivate, remoteEphemeral)); // es (responder side)
      const payload = state.decryptAndHash(message.slice(DH_BYTES));
      phase = "b";
      return payload;
    },
    writeMessageB(payload) {
      if (phase !== "b" || remoteEphemeral === null) {
        throw new NoiseProtocolError("message B out of order");
      }
      state.mixHash(ephemeralPublic); // e
      state.mixKey(x25519.getSharedSecret(ephemeralPrivate, remoteEphemeral)); // ee
      const ciphertext = state.encryptAndHash(payload);
      phase = "done";
      return concat(ephemeralPublic, ciphertext);
    },
    split() {
      if (phase !== "done") throw new NoiseProtocolError("handshake incomplete");
      const [initiatorToResponder, responderToInitiator] = state.split();
      return {
        send: responderToInitiator,
        receive: initiatorToResponder,
        handshakeHash: state.hash,
      };
    },
  };
}

export function decodeBase64UrlKey(encoded: string): Uint8Array {
  const base64 = encoded.replaceAll("-", "+").replaceAll("_", "/");
  const padded = base64 + "=".repeat((4 - (base64.length % 4)) % 4);
  let binary: string;
  try {
    binary =
      typeof Buffer !== "undefined"
        ? Buffer.from(padded, "base64").toString("binary")
        : atob(padded);
  } catch (cause) {
    throw new NoiseProtocolError(`host key is not base64url: ${String(cause)}`);
  }
  const bytes = Uint8Array.from(binary, (char) => char.charCodeAt(0));
  return requireKey(bytes, "host key");
}
```

Note for the implementer: if the noble AEAD's `decrypt` error type or the x25519 helper names differ in the pinned versions (Task 1's verification), adapt the three call sites (`chacha20poly1305(...).decrypt`, `x25519.utils.randomSecretKey`, `x25519.getSharedSecret`) — the test vectors will catch any semantic slip.

- [x] **Step 5: Run to verify pass**

Run (from `packages/client-runtime`): `vp test run src/e2ee/noise.test.ts`
Expected: PASS, including the official-vector suite. If the vector assertions fail while self-round-trip passes, the bug is real (padding, nonce layout, hash order) — do not weaken the vector test.

- [x] **Step 6: Commit**

```bash
git add packages/client-runtime/src/e2ee
git commit -m "feat(client-runtime): Noise NK initiator and responder over the noble stack"
```

---

### Task 10: TypeScript record layer (fragmentation)

**Files:**

- Create: `packages/client-runtime/src/e2ee/frame.ts`
- Create: `packages/client-runtime/src/e2ee/frame.test.ts`

**Interfaces:**

- Consumes: `NOISE_TAG_BYTES`, `MAX_NOISE_MESSAGE_BYTES` (Task 9).
- Produces (constants mirror `apps/server/src/rpc/e2ee.rs` exactly; the interop test in Task 14 exercises both sides):

```ts
export const E2EE_RECORD_FLAG_FINAL = 0x00;
export const E2EE_RECORD_FLAG_CONTINUATION = 0x01;
export const MAX_E2EE_CHUNK_BYTES = 65518; // 65535 - 16 (tag) - 1 (flag)
export const MAX_E2EE_LOGICAL_MESSAGE_BYTES = 64 * 1024 * 1024;
export class E2eeFrameError extends Error {}
export function splitIntoRecords(plaintext: Uint8Array): Array<Uint8Array>; // >= 1 records, each = flag byte ++ chunk
export class RecordAssembler {
  push(recordPlaintext: Uint8Array): Uint8Array | null; // full message on final record; throws E2eeFrameError
}
```

- [x] **Step 1: Write the failing tests**

```ts
import { describe, expect, it } from "@effect/vitest";

import {
  E2EE_RECORD_FLAG_CONTINUATION,
  E2EE_RECORD_FLAG_FINAL,
  E2eeFrameError,
  MAX_E2EE_CHUNK_BYTES,
  MAX_E2EE_LOGICAL_MESSAGE_BYTES,
  RecordAssembler,
  splitIntoRecords,
} from "./frame.ts";

describe("e2ee record layer", () => {
  it("splits small payloads into one final record", () => {
    const records = splitIntoRecords(Uint8Array.from([1, 2, 3]));
    expect(records).toHaveLength(1);
    expect(records[0][0]).toBe(E2EE_RECORD_FLAG_FINAL);
    expect([...records[0].slice(1)]).toEqual([1, 2, 3]);
  });

  it("represents the empty message as one final empty record", () => {
    const records = splitIntoRecords(new Uint8Array(0));
    expect(records).toHaveLength(1);
    expect(records[0]).toEqual(Uint8Array.of(E2EE_RECORD_FLAG_FINAL));
  });

  it("splits large payloads with continuation flags and reassembles them", () => {
    const payload = new Uint8Array(MAX_E2EE_CHUNK_BYTES * 2 + 7).fill(0xab);
    const records = splitIntoRecords(payload);
    expect(records).toHaveLength(3);
    expect(records[0][0]).toBe(E2EE_RECORD_FLAG_CONTINUATION);
    expect(records[1][0]).toBe(E2EE_RECORD_FLAG_CONTINUATION);
    expect(records[2][0]).toBe(E2EE_RECORD_FLAG_FINAL);
    expect(records[0]).toHaveLength(1 + MAX_E2EE_CHUNK_BYTES);
    const assembler = new RecordAssembler();
    expect(assembler.push(records[0])).toBeNull();
    expect(assembler.push(records[1])).toBeNull();
    expect(assembler.push(records[2])).toEqual(payload);
  });

  it("the assembler resets between messages", () => {
    const assembler = new RecordAssembler();
    expect(assembler.push(Uint8Array.of(E2EE_RECORD_FLAG_FINAL, 1))).toEqual(Uint8Array.of(1));
    expect(assembler.push(Uint8Array.of(E2EE_RECORD_FLAG_FINAL, 2))).toEqual(Uint8Array.of(2));
  });

  it("rejects empty records, unknown flags, and overflow", () => {
    const assembler = new RecordAssembler();
    expect(() => assembler.push(new Uint8Array(0))).toThrow(E2eeFrameError);
    expect(() => assembler.push(Uint8Array.of(0x02, 1))).toThrow(E2eeFrameError);
    const chunk = new Uint8Array(1 + MAX_E2EE_CHUNK_BYTES);
    chunk[0] = E2EE_RECORD_FLAG_CONTINUATION;
    const overflowing = new RecordAssembler();
    const rounds = Math.ceil(MAX_E2EE_LOGICAL_MESSAGE_BYTES / MAX_E2EE_CHUNK_BYTES) + 1;
    expect(() => {
      for (let index = 0; index < rounds; index += 1) {
        overflowing.push(chunk);
      }
    }).toThrow(E2eeFrameError);
  });

  it("refuses to split payloads beyond the logical cap", () => {
    // One 64 MiB + 1 allocation; acceptable in CI.
    expect(() => splitIntoRecords(new Uint8Array(MAX_E2EE_LOGICAL_MESSAGE_BYTES + 1))).toThrow(
      E2eeFrameError,
    );
  });
});
```

- [x] **Step 2: Run to verify failure**

Run (from `packages/client-runtime`): `vp test run src/e2ee/frame.test.ts`
Expected: FAIL (module missing).

- [x] **Step 3: Implement `frame.ts`**

```ts
import { MAX_NOISE_MESSAGE_BYTES, NOISE_TAG_BYTES } from "./noise.ts";

export const E2EE_RECORD_FLAG_FINAL = 0x00;
export const E2EE_RECORD_FLAG_CONTINUATION = 0x01;
export const MAX_E2EE_CHUNK_BYTES = MAX_NOISE_MESSAGE_BYTES - NOISE_TAG_BYTES - 1;
export const MAX_E2EE_LOGICAL_MESSAGE_BYTES = 64 * 1024 * 1024;

export class E2eeFrameError extends Error {}

export function splitIntoRecords(plaintext: Uint8Array): Array<Uint8Array> {
  if (plaintext.length > MAX_E2EE_LOGICAL_MESSAGE_BYTES) {
    throw new E2eeFrameError(`outbound message of ${plaintext.length} bytes exceeds the E2EE cap`);
  }
  const records: Array<Uint8Array> = [];
  if (plaintext.length === 0) {
    return [Uint8Array.of(E2EE_RECORD_FLAG_FINAL)];
  }
  for (let offset = 0; offset < plaintext.length; offset += MAX_E2EE_CHUNK_BYTES) {
    const chunk = plaintext.subarray(offset, offset + MAX_E2EE_CHUNK_BYTES);
    const final = offset + MAX_E2EE_CHUNK_BYTES >= plaintext.length;
    const record = new Uint8Array(1 + chunk.length);
    record[0] = final ? E2EE_RECORD_FLAG_FINAL : E2EE_RECORD_FLAG_CONTINUATION;
    record.set(chunk, 1);
    records.push(record);
  }
  return records;
}

export class RecordAssembler {
  private parts: Array<Uint8Array> = [];
  private assembledBytes = 0;

  push(recordPlaintext: Uint8Array): Uint8Array | null {
    if (recordPlaintext.length === 0) {
      throw new E2eeFrameError("empty E2EE record");
    }
    const flag = recordPlaintext[0];
    const chunk = recordPlaintext.subarray(1);
    if (this.assembledBytes + chunk.length > MAX_E2EE_LOGICAL_MESSAGE_BYTES) {
      throw new E2eeFrameError("E2EE reassembly overflow");
    }
    if (flag === E2EE_RECORD_FLAG_CONTINUATION) {
      this.parts.push(chunk.slice());
      this.assembledBytes += chunk.length;
      return null;
    }
    if (flag !== E2EE_RECORD_FLAG_FINAL) {
      throw new E2eeFrameError(`unknown E2EE record flag ${flag}`);
    }
    const message = new Uint8Array(this.assembledBytes + chunk.length);
    let offset = 0;
    for (const part of this.parts) {
      message.set(part, offset);
      offset += part.length;
    }
    message.set(chunk, offset);
    this.parts = [];
    this.assembledBytes = 0;
    return message;
  }
}
```

- [x] **Step 4: Run to verify pass**

Run (from `packages/client-runtime`): `vp test run src/e2ee/frame.test.ts`
Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add packages/client-runtime/src/e2ee/frame.ts packages/client-runtime/src/e2ee/frame.test.ts
git commit -m "feat(client-runtime): E2EE record fragmentation layer"
```

---

### Task 11: E2EE `Socket` wrapper (client)

Wrap an inner effect `Socket.Socket` (the raw WebSocket) so the RPC protocol layer sees a plain socket: handshake + `e2ee_auth` run before the outer `onOpen` fires; writes latch until authenticated; every failure carries a typed `E2eeProtocolError` cause for Task 13's classification.

**Files:**

- Create: `packages/client-runtime/src/e2ee/socket.ts`
- Create: `packages/client-runtime/src/e2ee/socket.test.ts`
- Modify: `packages/client-runtime/src/e2ee/index.ts`

**Interfaces:**

- Consumes: Task 9 (`createNkInitiator`, `decodeBase64UrlKey`, errors), Task 10 (`splitIntoRecords`, `RecordAssembler`), `Socket` from `effect/unstable/socket/Socket` (interface: `run/runString/runRaw/writer`, constructor `Socket.make({ runRaw, writer })`, error types `Socket.SocketError` + `Socket.SocketReadError` / `SocketCloseError` — verify exact exported names in `node_modules/effect/src/unstable/socket/Socket.ts` before coding; the vendored reference copy is `.repos/effect-smol/packages/effect/src/unstable/socket/Socket.ts`).
- Produces:

```ts
export type E2eeFailureReason = "host-identity-mismatch" | "unauthorized" | "protocol" | "timeout";
export class E2eeProtocolError extends Error {
  readonly reason: E2eeFailureReason;
}
export const E2EE_HANDSHAKE_TIMEOUT_MS = 10_000;
export const E2EE_HOST_IDENTITY_CLOSE_CODE = 4403; // mirrors the server constant
export type E2eeAuthRequest =
  | { readonly kind: "pairing"; readonly token: string } // first connect: in-channel bootstrap
  | { readonly kind: "bearer"; readonly credential: string }; // subsequent connects
export interface E2eeSocketOptions {
  readonly hostKey: string; // base64url host identity public key
  readonly auth: E2eeAuthRequest; // sent inside the channel as e2ee_auth
  readonly handshakeTimeoutMs?: number;
  /** Fires exactly once, on e2ee_authenticated, before the outer onOpen runs. */
  readonly onAuthenticated?: (message: E2eeAuthenticatedMessage) => void;
}
export function makeE2eeSocket(inner: Socket.Socket, options: E2eeSocketOptions): Socket.Socket;
```

(`E2eeAuthenticatedMessage` is the contracts type from Task 6.) Failure mapping is part of the contract: an inner-socket close with code 4403 **or** an AEAD failure on message B maps to `E2eeProtocolError` reason `"host-identity-mismatch"`; a non-empty message-B handshake payload is a `"protocol"` violation (spec §4.3's empty-payload rule, enforced here because the vector-driven `noise.ts` must keep accepting payload-bearing messages). Task 12 wires it into `RpcSessionFactory`; Task 13 classifies its failures.

- [x] **Step 1: Write the failing tests**

`packages/client-runtime/src/e2ee/socket.test.ts` builds an in-memory inner `Socket` pair and a scripted responder using `createNkResponder` + the record layer (this is exactly what the server does, in TS, so the socket state machine is tested hermetically; cross-language equivalence is Task 14's job):

```ts
import { describe, expect, it } from "@effect/vitest";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Socket from "effect/unstable/socket/Socket";

import { RecordAssembler, splitIntoRecords } from "./frame.ts";
import { createNkResponder, derivePublicKey } from "./noise.ts";
import type { E2eeAuthenticatedMessage } from "@bibcode/contracts";

import { E2eeProtocolError, makeE2eeSocket } from "./socket.ts";

/**
 * Test double: an inner Socket whose peer is a scripted responder function
 * (frames in -> frames out). Synchronous mailbox + microtask pump — no real
 * network. `emit` queues a frame for the outer handler; `close` ends the run —
 * with an error-worthy close code (e.g. 4403) it fails the run with a
 * SocketCloseError, exactly as fromWebSocket's close classifier does.
 */
const makeScriptedInnerSocket = (
  onFrame: (
    frame: Uint8Array,
    emit: (frame: Uint8Array) => void,
    close: (code?: number) => void,
  ) => void,
): Socket.Socket => {
  let deliver: ((frame: Uint8Array) => void) | null = null;
  let finish: ((code?: number) => void) | null = null;
  const pending: Array<Uint8Array> = [];
  let closed: { code?: number } | null = null;
  const emit = (frame: Uint8Array): void => {
    if (deliver === null) {
      pending.push(frame);
    } else {
      deliver(frame);
    }
  };
  const close = (code?: number): void => {
    closed = { code };
    finish?.(code);
  };
  return Socket.make({
    runRaw: (handler, options) =>
      Effect.callback<void, Socket.SocketError>((resume) => {
        deliver = (frame) => {
          const result = handler(frame);
          if (Effect.isEffect(result)) {
            // Handler effects run eagerly; a failure fails the run like the
            // real fromWebSocket fiber-set does.
            Effect.runPromise(result as Effect.Effect<unknown, unknown>).catch((cause) => {
              resume(
                Effect.fail(
                  cause instanceof Socket.SocketError
                    ? cause
                    : new Socket.SocketError({
                        reason: new Socket.SocketReadError({ cause }),
                      }),
                ),
              );
            });
          }
        };
        finish = (code) =>
          resume(
            code === undefined || code === 1000
              ? Effect.void
              : Effect.fail(
                  new Socket.SocketError({
                    reason: new Socket.SocketCloseError({ code, closeReason: "" }),
                  }),
                ),
          );
        for (const frame of pending.splice(0)) {
          deliver(frame);
        }
        if (closed !== null) {
          finish(closed.code);
        } else if (options?.onOpen) {
          Effect.runPromise(options.onOpen).catch(() => {});
        }
      }),
    writer: Effect.succeed((chunk: Uint8Array | string | Socket.CloseEvent) =>
      Effect.sync(() => {
        if (typeof chunk === "string" || chunk instanceof Uint8Array) {
          const bytes = typeof chunk === "string" ? new TextEncoder().encode(chunk) : chunk;
          onFrame(bytes, emit, close);
        } else {
          close();
        }
      }),
    ),
  });
};

/** Digs the typed E2EE failure out of a failed exit's cause chain. */
const findE2eeCause = (exit: Exit.Exit<unknown, unknown>): E2eeProtocolError | null => {
  if (exit._tag !== "Failure") return null;
  for (const failure of Cause.failures(exit.cause)) {
    if (failure instanceof E2eeProtocolError) return failure;
    if (failure instanceof Socket.SocketError) {
      const cause = (failure.reason as { cause?: unknown }).cause;
      if (cause instanceof E2eeProtocolError) return cause;
    }
  }
  return null;
};

const responderScript = (options?: { failAuth?: boolean; messageBPayload?: Uint8Array }) => {
  const staticPrivate = crypto.getRandomValues(new Uint8Array(32));
  const hostKey = derivePublicKey(staticPrivate);
  const responder = createNkResponder({ staticPrivateKey: staticPrivate });
  const assembler = new RecordAssembler();
  let transport: ReturnType<typeof responder.split> | null = null;
  const received: Array<string> = [];
  const script = (
    frame: Uint8Array,
    emit: (frame: Uint8Array) => void,
    close: (code?: number) => void,
  ): void => {
    if (transport === null) {
      responder.readMessageA(frame);
      emit(responder.writeMessageB(options?.messageBPayload ?? new Uint8Array(0)));
      transport = responder.split();
      return;
    }
    const record = transport.receive.decryptWithAd(new Uint8Array(0), frame);
    const message = assembler.push(record);
    if (message === null) return;
    const text = new TextDecoder().decode(message);
    received.push(text);
    const reply = (body: object) => {
      for (const outRecord of splitIntoRecords(new TextEncoder().encode(JSON.stringify(body)))) {
        emit(transport!.send.encryptWithAd(new Uint8Array(0), outRecord));
      }
    };
    const parsed = JSON.parse(text) as { type?: string; pairing?: string; bearer?: string };
    if (parsed.type === "e2ee_auth") {
      if (options?.failAuth) {
        reply({ type: "e2ee_error", code: "unauthorized" });
        close();
      } else if (parsed.pairing !== undefined) {
        // In-channel bootstrap reply (spec section 4.3, pairing form).
        reply({
          type: "e2ee_authenticated",
          credential: `minted-for-${parsed.pairing}`,
          environmentId: "env-1",
          storageInstanceId: "3f2f6a52-6f5f-4f4e-9d38-0a1e2ac21d11",
        });
      } else {
        reply({ type: "e2ee_authenticated" });
      }
      return;
    }
    reply({ echoed: text.length });
  };
  return { hostKey, script, received };
};

describe("makeE2eeSocket", () => {
  it.effect("handshakes, bootstraps in-channel, then delivers decrypted strings", () =>
    Effect.gen(function* () {
      const { hostKey, script, received } = responderScript();
      const authenticated: Array<E2eeAuthenticatedMessage> = [];
      const socket = makeE2eeSocket(makeScriptedInnerSocket(script), {
        hostKey: Buffer.from(hostKey).toString("base64url"),
        auth: { kind: "pairing", token: "one-time-1" },
        onAuthenticated: (message) => {
          authenticated.push(message);
        },
      });
      const delivered: Array<string> = [];
      const opened = yield* Deferred.make<void>();
      const fiber = yield* Effect.fork(
        Effect.scoped(
          Effect.gen(function* () {
            const write = yield* socket.writer;
            yield* Effect.fork(
              socket.runString(
                (text) => {
                  delivered.push(text);
                },
                { onOpen: Deferred.succeed(opened, undefined).pipe(Effect.asVoid) },
              ),
            );
            yield* Deferred.await(opened); // onOpen must NOT fire before e2ee_authenticated
            expect(authenticated).toHaveLength(1); // onAuthenticated fired first
            yield* write(JSON.stringify({ hello: true }));
            yield* Effect.sleep("50 millis");
          }),
        ),
      );
      yield* Effect.sleep("200 millis");
      yield* Fiber.interrupt(fiber);
      expect(received[0]).toBe(JSON.stringify({ type: "e2ee_auth", pairing: "one-time-1" }));
      expect(received[1]).toBe(JSON.stringify({ hello: true }));
      expect(delivered).toEqual([JSON.stringify({ echoed: received[1].length })]);
      expect(authenticated[0].credential).toBe("minted-for-one-time-1");
      expect(authenticated[0].environmentId).toBe("env-1");
    }),
  );

  it.effect("bearer form sends the stored credential", () =>
    Effect.gen(function* () {
      const { hostKey, script, received } = responderScript();
      const socket = makeE2eeSocket(makeScriptedInnerSocket(script), {
        hostKey: Buffer.from(hostKey).toString("base64url"),
        auth: { kind: "bearer", credential: "stored-1" },
      });
      const opened = yield* Deferred.make<void>();
      const fiber = yield* Effect.fork(
        socket.runString(() => {}, {
          onOpen: Deferred.succeed(opened, undefined).pipe(Effect.asVoid),
        }),
      );
      yield* Deferred.await(opened);
      yield* Fiber.interrupt(fiber);
      expect(received[0]).toBe(JSON.stringify({ type: "e2ee_auth", bearer: "stored-1" }));
    }),
  );

  it.effect("maps a 4403 close to host-identity-mismatch (the real wrong-key path)", () =>
    Effect.gen(function* () {
      // A responder without the pinned static key cannot read message A; per spec
      // it closes with code 4403 and never sends message B.
      const script = (
        _frame: Uint8Array,
        _emit: (frame: Uint8Array) => void,
        close: (code?: number) => void,
      ): void => {
        close(4403);
      };
      const socket = makeE2eeSocket(makeScriptedInnerSocket(script), {
        hostKey: Buffer.from(derivePublicKey(crypto.getRandomValues(new Uint8Array(32)))).toString(
          "base64url",
        ),
        auth: { kind: "pairing", token: "t" },
      });
      const exit = yield* socket.runString(() => {}).pipe(Effect.exit);
      expect(findE2eeCause(exit)?.reason).toBe("host-identity-mismatch");
    }),
  );

  it.effect("maps an AEAD failure on message B to host-identity-mismatch (active MITM)", () =>
    Effect.gen(function* () {
      // A MITM that answers with garbage of message-B shape instead of closing:
      // the ee/es-keyed AEAD check fails.
      const pinnedKey = derivePublicKey(crypto.getRandomValues(new Uint8Array(32)));
      const script = (_frame: Uint8Array, emit: (frame: Uint8Array) => void): void => {
        const forged = new Uint8Array(48); // e (32) + tag (16) of garbage
        crypto.getRandomValues(forged);
        emit(forged);
      };
      const socket = makeE2eeSocket(makeScriptedInnerSocket(script), {
        hostKey: Buffer.from(pinnedKey).toString("base64url"),
        auth: { kind: "pairing", token: "t" },
      });
      const exit = yield* socket.runString(() => {}).pipe(Effect.exit);
      expect(findE2eeCause(exit)?.reason).toBe("host-identity-mismatch");
    }),
  );

  it.effect("rejects a non-empty message-B handshake payload as a protocol violation", () =>
    Effect.gen(function* () {
      const { hostKey, script } = responderScript({ messageBPayload: Uint8Array.of(1) });
      const socket = makeE2eeSocket(makeScriptedInnerSocket(script), {
        hostKey: Buffer.from(hostKey).toString("base64url"),
        auth: { kind: "pairing", token: "t" },
      });
      const exit = yield* socket.runString(() => {}).pipe(Effect.exit);
      expect(findE2eeCause(exit)?.reason).toBe("protocol");
    }),
  );

  it.effect("fails with unauthorized when the server rejects the credential", () =>
    Effect.gen(function* () {
      const { hostKey, script } = responderScript({ failAuth: true });
      const socket = makeE2eeSocket(makeScriptedInnerSocket(script), {
        hostKey: Buffer.from(hostKey).toString("base64url"),
        auth: { kind: "pairing", token: "expired" },
      });
      const exit = yield* socket.runString(() => {}).pipe(Effect.exit);
      expect(findE2eeCause(exit)?.reason).toBe("unauthorized");
    }),
  );

  it.effect("times out a stalled handshake", () =>
    Effect.gen(function* () {
      const socket = makeE2eeSocket(
        makeScriptedInnerSocket(() => {}), // black-holes message A
        {
          hostKey: Buffer.from(
            derivePublicKey(crypto.getRandomValues(new Uint8Array(32))),
          ).toString("base64url"),
          auth: { kind: "bearer", credential: "t" },
          handshakeTimeoutMs: 100,
        },
      );
      const exit = yield* socket.runString(() => {}).pipe(Effect.exit);
      expect(findE2eeCause(exit)?.reason).toBe("timeout");
    }),
  );

  it.effect("fragments large writes and reassembles large replies", () =>
    Effect.gen(function* () {
      const { hostKey, script, received } = responderScript(); // its echo replies { echoed: length }
      const socket = makeE2eeSocket(makeScriptedInnerSocket(script), {
        hostKey: Buffer.from(hostKey).toString("base64url"),
        auth: { kind: "bearer", credential: "stored-1" },
      });
      const large = JSON.stringify({ blob: "x".repeat(200_000) }); // > 2 chunks
      const delivered: Array<string> = [];
      const opened = yield* Deferred.make<void>();
      const fiber = yield* Effect.fork(
        Effect.scoped(
          Effect.gen(function* () {
            const write = yield* socket.writer;
            yield* Effect.fork(
              socket.runString(
                (text) => {
                  delivered.push(text);
                },
                { onOpen: Deferred.succeed(opened, undefined).pipe(Effect.asVoid) },
              ),
            );
            yield* Deferred.await(opened);
            yield* write(large);
            yield* Effect.sleep("100 millis");
          }),
        ),
      );
      yield* Effect.sleep("300 millis");
      yield* Fiber.interrupt(fiber);
      expect(received[1]).toBe(large); // responder reassembled all fragments intact
      expect(delivered).toEqual([JSON.stringify({ echoed: large.length })]);
    }),
  );
});
```

Additional imports for the test file: `import * as Cause from "effect/Cause";` and `import * as Exit from "effect/Exit";`. Timing-based `sleep` synchronization is acceptable here because the scripted socket is fully synchronous — if flakiness appears, replace the sleeps with a Deferred completed by the delivery handler.

- [x] **Step 2: Run to verify failure**

Run (from `packages/client-runtime`): `vp test run src/e2ee/socket.test.ts`
Expected: FAIL (module missing).

- [x] **Step 3: Implement `socket.ts`**

```ts
import { E2eeAuthenticatedMessage } from "@bibcode/contracts";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Schema from "effect/Schema";
import * as Scope from "effect/Scope";
import * as Socket from "effect/unstable/socket/Socket";

import { RecordAssembler, splitIntoRecords } from "./frame.ts";
import {
  NoiseAuthenticationError,
  type NkTransport,
  createNkInitiator,
  decodeBase64UrlKey,
} from "./noise.ts";

const decodeAuthenticated = Schema.decodeUnknownSync(E2eeAuthenticatedMessage);

export const E2EE_HANDSHAKE_TIMEOUT_MS = 10_000;
const EMPTY = new Uint8Array(0);
const encoder = new TextEncoder();
const decoder = new TextDecoder();

export type E2eeFailureReason = "host-identity-mismatch" | "unauthorized" | "protocol" | "timeout";

export class E2eeProtocolError extends Error {
  readonly reason: E2eeFailureReason;
  constructor(reason: E2eeFailureReason, detail: string) {
    super(detail);
    this.reason = reason;
  }
}

export const E2EE_HOST_IDENTITY_CLOSE_CODE = 4403;

export type E2eeAuthRequest =
  | { readonly kind: "pairing"; readonly token: string }
  | { readonly kind: "bearer"; readonly credential: string };

export interface E2eeSocketOptions {
  readonly hostKey: string;
  readonly auth: E2eeAuthRequest;
  readonly handshakeTimeoutMs?: number;
  readonly onAuthenticated?: (message: E2eeAuthenticatedMessage) => void;
}

const socketFailure = (error: E2eeProtocolError): Socket.SocketError =>
  new Socket.SocketError({ reason: new Socket.SocketReadError({ cause: error }) });

/** Walks a SocketError for the wrapped E2EE failure; used by connection-error mapping. */
export const e2eeFailureOf = (error: unknown): E2eeProtocolError | null => {
  if (error instanceof E2eeProtocolError) return error;
  if (Socket.isSocketError?.(error) ?? error instanceof Socket.SocketError) {
    const cause = (error as Socket.SocketError).reason?.cause;
    if (cause instanceof E2eeProtocolError) return cause;
  }
  return null;
};

export const makeE2eeSocket = (inner: Socket.Socket, options: E2eeSocketOptions): Socket.Socket => {
  const responderKey = decodeBase64UrlKey(options.hostKey);
  const timeoutMs = options.handshakeTimeoutMs ?? E2EE_HANDSHAKE_TIMEOUT_MS;

  // Session state shared between runRaw and writer; reset per run. The RPC
  // stack builds one socket per connection attempt (protocol-level reconnects
  // are disabled), so single-run semantics are sufficient — a second run
  // recreates the state below via the closure in runRaw.
  let transportDeferred = Deferred.makeUnsafe<NkTransport, Socket.SocketError>();

  const encryptAndSend = (
    transport: NkTransport,
    write: (chunk: Uint8Array | string) => Effect.Effect<void, Socket.SocketError>,
    plaintext: Uint8Array,
  ): Effect.Effect<void, Socket.SocketError> =>
    Effect.suspend(() => {
      try {
        const frames = splitIntoRecords(plaintext).map((record) =>
          transport.send.encryptWithAd(EMPTY, record),
        );
        return Effect.forEach(frames, (frame) => write(frame), { discard: true });
      } catch (cause) {
        return Effect.fail(
          socketFailure(new E2eeProtocolError("protocol", `encrypt failed: ${String(cause)}`)),
        );
      }
    });

  const runRaw: Socket.Socket["runRaw"] = (handler, runOptions) =>
    Effect.suspend(() => {
      transportDeferred = Deferred.makeUnsafe<NkTransport, Socket.SocketError>();
      const initiator = createNkInitiator({ responderStaticPublicKey: responderKey });
      const assembler = new RecordAssembler();
      let phase: "handshake" | "auth" | "open" = "handshake";
      let transport: NkTransport | null = null;
      const authenticated = Deferred.makeUnsafe<void, Socket.SocketError>();

      const fail = (reason: E2eeFailureReason, detail: string) => {
        const error = socketFailure(new E2eeProtocolError(reason, detail));
        Deferred.doneUnsafe(transportDeferred, Effect.fail(error));
        Deferred.doneUnsafe(authenticated, Effect.fail(error));
        return Effect.fail(error);
      };

      return Effect.scopedWith((scope) =>
        Effect.gen(function* () {
          // inner.writer is scoped; bind it to this run's scope — the same idiom
          // effect's own Socket combinators use (see fromWebSocket in Socket.ts).
          const innerWrite = yield* Scope.provide(inner.writer, scope);

          const innerHandler = (data: string | Uint8Array) => {
            if (typeof data === "string") {
              return fail("protocol", "peer sent a plaintext text frame on the E2EE channel");
            }
            switch (phase) {
              case "handshake": {
                let messageBPayload: Uint8Array;
                try {
                  messageBPayload = initiator.readMessageB(data);
                } catch (cause) {
                  return fail(
                    cause instanceof NoiseAuthenticationError
                      ? "host-identity-mismatch"
                      : "protocol",
                    `Noise handshake failed: ${String(cause)}`,
                  );
                }
                if (messageBPayload.length !== 0) {
                  // Spec section 4.3: handshake payloads must be empty. Enforced
                  // here (not in noise.ts, which must keep accepting the official
                  // vectors' payload-bearing messages).
                  return fail("protocol", "message B carried a non-empty handshake payload");
                }
                transport = initiator.split();
                phase = "auth";
                const auth = encoder.encode(
                  JSON.stringify(
                    options.auth.kind === "pairing"
                      ? { type: "e2ee_auth", pairing: options.auth.token }
                      : { type: "e2ee_auth", bearer: options.auth.credential },
                  ),
                );
                return encryptAndSend(transport, innerWrite, auth);
              }
              case "auth":
              case "open": {
                let message: Uint8Array | null;
                try {
                  const record = transport!.receive.decryptWithAd(EMPTY, data);
                  message = assembler.push(record);
                } catch (cause) {
                  return fail("protocol", `E2EE frame rejected: ${String(cause)}`);
                }
                if (message === null) {
                  return undefined;
                }
                if (phase === "open") {
                  return handler(decoder.decode(message));
                }
                // auth phase: expect e2ee_authenticated | e2ee_error
                let parsed: { type?: string; code?: string };
                try {
                  parsed = JSON.parse(decoder.decode(message)) as typeof parsed;
                } catch (cause) {
                  return fail("protocol", `unparsable control message: ${String(cause)}`);
                }
                if (parsed.type === "e2ee_authenticated") {
                  let ready: E2eeAuthenticatedMessage;
                  try {
                    ready = decodeAuthenticated(parsed);
                  } catch (cause) {
                    return fail("protocol", `malformed e2ee_authenticated: ${String(cause)}`);
                  }
                  if (options.auth.kind === "pairing" && ready.credential === undefined) {
                    return fail(
                      "protocol",
                      "the pairing bootstrap reply carried no minted credential",
                    );
                  }
                  phase = "open";
                  options.onAuthenticated?.(ready);
                  Deferred.doneUnsafe(transportDeferred, Effect.succeed(transport!));
                  Deferred.doneUnsafe(authenticated, Effect.void);
                  return undefined;
                }
                if (parsed.type === "e2ee_error") {
                  return fail(
                    parsed.code === "unauthorized" ? "unauthorized" : "protocol",
                    `server rejected the E2EE session (${parsed.code ?? "unknown"})`,
                  );
                }
                return fail("protocol", `unexpected control message ${parsed.type ?? "?"}`);
              }
            }
          };

          const sendMessageA = Effect.suspend(() => innerWrite(initiator.writeMessageA(EMPTY)));

          // Spec section 4.3: a responder that cannot decrypt message A closes
          // with 4403 and never sends message B — map that close (pre-open) to
          // host-identity-mismatch so verify-then-add classifies the real path.
          const runInner = inner
            .runRaw(innerHandler, { onOpen: sendMessageA })
            .pipe(
              Effect.mapError((error) =>
                phase !== "open" &&
                error instanceof Socket.SocketError &&
                error.reason instanceof Socket.SocketCloseError &&
                error.reason.code === E2EE_HOST_IDENTITY_CLOSE_CODE
                  ? socketFailure(
                      new E2eeProtocolError(
                        "host-identity-mismatch",
                        "the host closed the handshake with code 4403 (pinned key mismatch)",
                      ),
                    )
                  : error,
              ),
            );
          const deadline = Deferred.await(authenticated).pipe(
            Effect.timeoutOrElse({
              duration: `${timeoutMs} millis`,
              orElse: () =>
                Effect.suspend(() => fail("timeout", "E2EE handshake did not complete in time")),
            }),
            // After authentication, hold the race open forever so runInner decides.
            Effect.andThen(
              runOptions?.onOpen === undefined
                ? Effect.never
                : Effect.andThen(runOptions.onOpen, Effect.never),
            ),
          );
          return yield* Effect.raceFirst(runInner, deadline);
        }),
      );
    });

  return Socket.make({
    runRaw,
    writer: Effect.gen(function* () {
      const innerWrite = yield* inner.writer;
      return (chunk: Uint8Array | string | Socket.CloseEvent) =>
        typeof chunk === "object" && !(chunk instanceof Uint8Array)
          ? innerWrite(chunk) // CloseEvent passes through
          : Deferred.await(transportDeferred).pipe(
              Effect.flatMap((transport) =>
                encryptAndSend(
                  transport,
                  innerWrite,
                  typeof chunk === "string" ? encoder.encode(chunk) : chunk,
                ),
              ),
            );
    }),
  });
};
```

Implementer notes (resolve while coding, guided by the failing tests):

- If `Scope.provide(inner.writer, scope)` does not typecheck against the installed effect version, use the exact combinator `fromWebSocket` itself uses in `node_modules/effect/src/unstable/socket/Socket.ts` to bind a scoped effect to an explicit scope.
- On any run failure or completion, settle `transportDeferred` and `authenticated` with the failure (an `Effect.onExit` around the raced run) so a writer blocked on `Deferred.await(transportDeferred)` cannot hang after the socket dies.
- Verify `Socket.SocketCloseError`'s constructor/field names (`code`, `closeReason`) against the installed effect version when writing the 4403 mapping and the test double.
- The outer `onOpen` (which drives `RpcClient.ConnectionHooks.onConnect` and therefore `RpcSession.ready`) must run only after `e2ee_authenticated` — that is what the `deadline` chain does.
- `CloseEvent` detection: use the `Socket.isCloseEvent` guard if exported; otherwise a structural `"code" in chunk` check.
- Delivered plaintext is decoded as UTF-8 text (the plain `/ws` server sends Text frames), preserving byte-for-byte parity for the RPC layer.

- [x] **Step 4: Run to verify pass**

Run (from `packages/client-runtime`): `vp test run src/e2ee/socket.test.ts src/e2ee/noise.test.ts src/e2ee/frame.test.ts`
Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add packages/client-runtime/src/e2ee
git commit -m "feat(client-runtime): E2EE socket wrapper with in-channel auth"
```

---

### Task 12: Channel selection — hostKey on profiles, prepared E2EE, session wiring, badge state

The spec §4.3 rule: a saved direct (Bearer) connection **with** a stored `hostKey` must use `/ws-e2ee`; one **without** (legacy) uses `/ws` and surfaces an `unencrypted` transport badge state. Relay/SSH/Primary unchanged.

**Files:**

- Modify: `packages/client-runtime/src/connection/model.ts` (`PreparedE2eeChannel`, `PreparedConnection.e2ee`, `ConnectionBlockedReason` + `"host-identity"`)
- Modify: `packages/client-runtime/src/connection/catalog.ts` (`BearerConnectionProfile.hostKey`)
- Modify: `packages/client-runtime/src/authorization/remote.ts` (`e2eeSocketUrl` pure helper — no HTTP)
- Modify: `packages/client-runtime/src/authorization/service.ts` (`authorizeBearer` hostKey input + `e2ee` output; skips ticket issuance for hostKey targets)
- Modify: `packages/client-runtime/src/connection/resolver.ts` (pass `profile.hostKey`; put `e2ee` on every `PreparedConnection` literal)
- Modify: `packages/client-runtime/src/rpc/session.ts` (wrap the socket when `connection.e2ee !== null`; surface `e2eeAuthenticated`; map E2EE failures)
- Modify: `packages/client-runtime/src/connection/presentation.ts` (`connectionTransportSecurity`)
- Modify: `packages/client-runtime/src/connection/onboarding.ts` (both bearer registration builders set `hostKey` — `null` for the legacy manual path)
- Tests: `packages/client-runtime/src/connection/resolver.test.ts`, `packages/client-runtime/src/rpc/session.test.ts`, `packages/client-runtime/src/connection/presentation.test.ts`, plus the existing catalog/storage decode tests in `apps/web/src/connection/storage.test.ts` (decode-default case)

**Interfaces:**

- Consumes: Task 11 (`makeE2eeSocket`, `e2eeFailureOf`, `E2eeAuthRequest`), `E2eeAuthenticatedMessage` (Task 6).
- Produces:

```ts
// model.ts
export interface PreparedE2eeChannel {
  readonly hostKey: string;
  readonly auth: E2eeAuthRequest; // steady state: { kind: "bearer", credential }
}
export interface PreparedConnection {
  /* existing fields */ readonly e2ee: PreparedE2eeChannel | null;
}
// ConnectionBlockedReason gains the literal "host-identity"
// catalog.ts — BearerConnectionProfile gains:
//   hostKey: Schema.NullOr(Schema.String).pipe(Schema.withDecodingDefault(Effect.succeed(null)))
// presentation.ts
export type ConnectionTransportSecurity = "local" | "e2ee" | "channel-secured" | "unencrypted";
export function connectionTransportSecurity(
  entry: ConnectionCatalogEntry,
): ConnectionTransportSecurity;
// authorization/remote.ts
export function e2eeSocketUrl(wsBaseUrl: string): string; // pure: pathname -> /ws-e2ee, no params
// authorization/service.ts — authorizeBearer input gains `hostKey?: string | null`;
//   AuthorizedRemoteEnvironment gains `e2ee: PreparedE2eeChannel | null`
// rpc/session.ts — RpcSession gains:
//   readonly e2eeAuthenticated: Effect.Effect<E2eeAuthenticatedMessage | null>; // resolves after ready; null on plain channels
```

**No plaintext credential round-trips for hostKey targets (amended spec §4.3):** when `hostKey` is present, `authorizeBearer` performs **only** the unauthenticated descriptor fetch (routing hint); it must NOT call `/oauth/token` or `/api/auth/websocket-ticket`. The stored bearer credential rides inside the channel as `e2ee_auth`'s bearer form. Phase 4 renders `connectionTransportSecurity`; Task 13 consumes everything.

- [x] **Step 1: Write the failing tests**

1. **Profile decode-default** (find the existing decode assertions for `BearerConnectionProfile`/catalog documents — `packages/client-runtime/src/platform/storageDocument.test.ts` and/or `apps/web/src/connection/storage.test.ts` — and add):

```ts
it("decodes legacy bearer profiles without hostKey to null", () => {
  const decoded = Schema.decodeUnknownSync(BearerConnectionProfile)({
    _tag: "BearerConnectionProfile",
    connectionId: "bearer:env-1",
    environmentId: "env-1",
    label: "Legacy",
    httpBaseUrl: "http://192.168.1.20:3773/",
    wsBaseUrl: "ws://192.168.1.20:3773/",
  });
  expect(decoded.hostKey).toBeNull();
});
```

Also confirm the desktop protected-catalog path stays additive-safe: the desktop host round-trips the catalog document as an opaque protected string (`apps/desktop/src-tauri/src/security.rs` `protect_string`/`unprotect_string` never parse the JSON), so no Rust change is needed — assert nothing, but note it in the commit message.

2. **Resolver channel selection** (`resolver.test.ts`, following its existing bearer-broker test fixtures): with a profile whose `hostKey` is a 43-char base64url string, `prepare` must produce `socketUrl` ending in `/ws-e2ee` with **no** query parameters and `e2ee: { hostKey, auth: { kind: "bearer", credential: <stored token> } }` populated; with `hostKey: null`, `socketUrl` ends in `/ws?wsTicket=...` and `e2ee` is `null`. (Stub `RemoteEnvironmentAuthorization` the way the existing tests stub it; the assertion moves to the authorization service test if the resolver test doubles sit above this seam — put the test where the existing suite stubs least.)
3. **Authorization service** (`packages/client-runtime/src/authorization/remote.test.ts` / `layer.test.ts` style): `e2eeSocketUrl("ws://host:3773/")` returns `"ws://host:3773/ws-e2ee"`; and `authorizeBearer` with a `hostKey` performs **only** the descriptor fetch — assert via the HTTP stub that `/oauth/token` and `/api/auth/websocket-ticket` are never requested (amended spec §4.3: no plaintext credential round-trips), and that the returned `e2ee.auth` carries the stored bearer.
4. **Session wiring** (`session.test.ts`): with `connection.e2ee` set, `RpcSessionFactory.connect` builds the socket through `makeE2eeSocket` (assert via a WebSocketConstructor stub: the URL requested is the `/ws-e2ee` URL and the first frame written is binary with length 48 — 32-byte ephemeral + 16-byte tag, i.e. Noise message A); with `e2ee: null`, the first frame is the JSON ping/request text the existing tests already observe.
5. **Presentation** (`presentation.test.ts`):

```ts
describe("connectionTransportSecurity", () => {
  it("classifies each target kind", () => {
    expect(connectionTransportSecurity(primaryEntry)).toBe("local");
    expect(connectionTransportSecurity(bearerEntryWithHostKey)).toBe("e2ee");
    expect(connectionTransportSecurity(bearerEntryWithoutHostKey)).toBe("unencrypted");
    expect(connectionTransportSecurity(relayEntry)).toBe("channel-secured");
    expect(connectionTransportSecurity(sshEntry)).toBe("channel-secured");
  });
});
```

(Local desktop-managed bearer targets — connection ids with the `local:` prefix, `DESKTOP_LOCAL_CONNECTION_ID_PREFIX` — classify as `"local"`, not `"unencrypted"`: they are loopback by construction. Import the prefix constant or match on the id prefix string; check `apps/web/src/connection/desktopLocal.ts` for the exported name and, since that constant lives in the web app, hard-code the `"local:"` prefix in client-runtime with a comment pointing at the desktop-local module.)

- [x] **Step 2: Run to verify failures**

Run (from `packages/client-runtime`): `vp test run src/connection/resolver.test.ts src/rpc/session.test.ts src/connection/presentation.test.ts`
Expected: FAIL (missing fields/exports).

- [x] **Step 3: Implement**

`model.ts`:

```ts
import type { E2eeAuthRequest } from "../e2ee/socket.ts";

export interface PreparedE2eeChannel {
  readonly hostKey: string;
  readonly auth: E2eeAuthRequest;
}
```

Add `readonly e2ee: PreparedE2eeChannel | null;` to `PreparedConnection`, and `"host-identity"` to the `ConnectionBlockedReason` literals. Then chase the compiler: every `PreparedConnection` literal (`resolver.ts` ×4 broker returns, `authorization/service.ts`, test fixtures) gains `e2ee: null` except the bearer path below. Audit `ConnectionBlockedReason` consumers (`errors.ts`, `presentation.ts`, `supervisor.ts`, web-side rendering) — they treat reasons generically; extend any exhaustive switch with `"host-identity"` mapping to the blocked/error presentation.

`catalog.ts` (`BearerConnectionProfile` fields, plus `import * as Effect from "effect/Effect";`):

```ts
    hostKey: Schema.NullOr(Schema.String).pipe(Schema.withDecodingDefault(Effect.succeed(null))),
```

Constructor call sites (`onboarding.ts` both builders, `resolver.ts` SSH profile rewrite is `SshConnectionProfile` — untouched) now pass `hostKey`: the legacy manual flow passes `null` (or the previously stored value on update — `prepareBearerConnectionUpdate` must **preserve** `entry.profile.value.hostKey`, not clear it).

`authorization/remote.ts` — a pure URL helper only; the amended spec forbids any credential HTTP for hostKey targets:

```ts
/** /ws-e2ee URL for a hostKey target. Pure: no ticket, no query parameters. */
export function e2eeSocketUrl(wsBaseUrl: string): string {
  const url = new URL(wsBaseUrl);
  if (url.pathname === "" || url.pathname === "/") {
    url.pathname = "/ws-e2ee";
  }
  url.search = "";
  url.hash = "";
  return url.toString();
}
```

`authorization/service.ts` `authorizeBearer`: accept `hostKey?: string | null`. When a non-null `hostKey` is present, keep the descriptor fetch (routing hint) and the environment-id check, then **skip `resolveRemoteWebSocketConnectionUrl` entirely** (no `/api/auth/websocket-ticket` call) and return:

```ts
return {
  descriptor,
  environmentId: descriptor.environmentId,
  label: descriptor.label,
  httpBaseUrl: input.httpBaseUrl,
  socketUrl: e2eeSocketUrl(input.wsBaseUrl),
  httpAuthorization: { _tag: "Bearer" as const, token: input.bearerToken },
  e2ee: {
    hostKey: input.hostKey,
    auth: { kind: "bearer" as const, credential: input.bearerToken },
  },
};
```

Otherwise keep the existing ticket path with `e2ee: null`. Extend `AuthorizedRemoteEnvironment` with `readonly e2ee: PreparedE2eeChannel | null;` and set `e2ee: null` on the DPoP/relay path. (Note: `httpAuthorization` is retained for the legacy plain-HTTP call sites, but the server's no-downgrade rule rejects e2ee-minted bearers there — Task 15's audit documents which HTTP surfaces remain usable for hostKey targets.)

`resolver.ts` bearer broker: pass `hostKey: profile.hostKey` into `authorizeBearer`, and `e2ee: authorized.e2ee` into the returned `PreparedConnection`. All other brokers: `e2ee: null` (SSH keeps `/ws` — the tunnel is the channel security; primary/relay unchanged).

`rpc/session.ts`:

```ts
import type { E2eeAuthenticatedMessage } from "@bibcode/contracts";

import { makeE2eeSocket } from "../e2ee/index.ts";

// inside connect(), before the layers are built:
const e2eeAuthenticated = yield * Deferred.make<E2eeAuthenticatedMessage | null>();

const socketLayer = Layer.effect(
  Socket.Socket,
  Effect.map(
    Socket.makeWebSocket(connection.socketUrl, { openTimeout: SOCKET_OPEN_TIMEOUT }),
    (socket) =>
      connection.e2ee === null
        ? socket
        : makeE2eeSocket(socket, {
            hostKey: connection.e2ee.hostKey,
            auth: connection.e2ee.auth,
            onAuthenticated: (message) => {
              Deferred.doneUnsafe(e2eeAuthenticated, Effect.succeed(message));
            },
          }),
  ),
).pipe(Layer.provide(Layer.succeed(Socket.WebSocketConstructor, webSocketConstructor)));
```

and extend the returned `RpcSession`:

```ts
return {
  client,
  initialConfig,
  ready: /* unchanged */,
  probe,
  closed: Deferred.await(disconnected),
  e2eeAuthenticated:
    connection.e2ee === null
      ? Effect.succeed(null)
      : Deferred.await(e2eeAuthenticated),
} satisfies RpcSession;
```

(Add `readonly e2eeAuthenticated: Effect.Effect<E2eeAuthenticatedMessage | null>;` to the `RpcSession` interface; it resolves once `e2ee_authenticated` arrives — always before `ready`, since the wrapper's outer `onOpen` runs after authentication.)

(`Socket.layerWebSocket(url, opts)` is exactly `Layer.effect(Socket.Socket, Socket.makeWebSocket(url, opts))`, so this is a behavior-preserving generalization for the plain path.) In the `ConnectionHooks.onDisconnect` failure detail, when `connection.e2ee !== null` and the socket failure carries an `e2eeFailureOf(...)` cause, surface it: `host-identity-mismatch` → `ConnectionBlockedError({ reason: "host-identity", detail: ... })` — thread this through the same error channel the hooks currently use for the transient disconnect error (keep the transient error for plain transport failures). Where the hook API only allows one error shape, record the E2EE reason in the `detail` string AND have `RpcSessionFactory.connect` fail fast: after `Layer.build`, race `ready` against `closed` exactly as today — the classification consumer (Task 13) probes with its own scoped connect and inspects the failure it gets back, so ensure the `ConnectionTransientError`/`ConnectionBlockedError` produced here preserves the reason (blocked `"host-identity"` for mismatch, blocked `"authentication"` for unauthorized, transient `"timeout"`/`"transport"` otherwise).

`presentation.ts`:

```ts
export type ConnectionTransportSecurity = "local" | "e2ee" | "channel-secured" | "unencrypted";

const DESKTOP_LOCAL_CONNECTION_ID_PREFIX = "local:"; // mirrors the desktop-local module in apps/web

export function connectionTransportSecurity(
  entry: ConnectionCatalogEntry,
): ConnectionTransportSecurity {
  switch (entry.target._tag) {
    case "PrimaryConnectionTarget":
    case "UnavailableConnectionTarget":
      return "local";
    case "RelayConnectionTarget":
    case "SshConnectionTarget":
      return "channel-secured";
    case "BearerConnectionTarget": {
      if (entry.target.connectionId.startsWith(DESKTOP_LOCAL_CONNECTION_ID_PREFIX)) {
        return "local";
      }
      const profile =
        Option.isSome(entry.profile) && entry.profile.value._tag === "BearerConnectionProfile"
          ? entry.profile.value
          : null;
      return profile?.hostKey != null ? "e2ee" : "unencrypted";
    }
  }
}
```

- [x] **Step 4: Run to verify pass**

Run (from `packages/client-runtime`): `vp test run src/connection src/rpc src/authorization` and (from `apps/web`) `vp test run src/connection/storage.test.ts`
Expected: PASS, including all pre-existing suites (the `e2ee: null` additions must not disturb them).

- [x] **Step 5: Commit**

```bash
git add packages/client-runtime/src apps/web/src/connection/storage.test.ts
git commit -m "feat(client-runtime): route hostKey-bearing connections over /ws-e2ee"
```

---

### Task 13: Verify-then-add flow with classified failures (spec §4.2)

Generalizes the manual bearer add flow (`onboarding.ts` `preparePairingRegistration`, ~line 87) to the amended spec §4.3: parse the code, classify the endpoint, then live-probe with **no plaintext credential exchange** — the unauthenticated descriptor fetch is a routing hint only; the one-time pairing token rides `e2ee_auth`'s pairing form and the bearer credential is minted **inside** the channel. Before anything is persisted, the authenticated identity (`environmentId`/`storageInstanceId` from the `e2ee_authenticated` reply and the authenticated `server.getConfig`) is compared against the pairing payload and the descriptor hint, mirroring the two checks the normal driver performs (`driver.ts:46` region: `verifyPreparedStorageIdentity` + the post-`initialConfig` re-check). Failures classify as `unreachable | host-identity-mismatch | pairing-rejected | incompatible | duplicate-storage-identity`; persistence stores `hostKey` plus the in-channel-minted credential.

**Files:**

- Create: `packages/client-runtime/src/connection/pairingAdd.ts`
- Create: `packages/client-runtime/src/connection/pairingAdd.test.ts`
- Modify: `packages/client-runtime/src/connection/onboarding.ts` (expose `verifyAndAddPairingCode` on `ConnectionOnboarding`)
- Modify: `packages/client-runtime/src/connection/layer.ts` (provide the new dependencies to the onboarding layer)

**Interfaces:**

- Consumes: `parsePairingCode` + errors (Task 7), `classifyPairingEndpoint` (Task 7), `fetchRemoteEnvironmentDescriptor` (routing hint only), `computeCompatVerdict` (Phase 2 — see the consumption note in the header), `RpcSessionFactory` + `RpcSession.e2eeAuthenticated` + `e2eeSocketUrl` (Task 12), `EnvironmentRegistry`, `AcceptedStorageIdentityStore.{get, accept}` + `storageIdentityTargetKey`, `deriveWsBaseUrl`/`normalizeHttpBaseUrl`. **Not consumed:** `bootstrapRemoteBearerSession` — the amended spec forbids the plaintext `/oauth/token` exchange for hostKey targets.
- **Identity-mismatch classification (pinned here):** an authenticated `storageInstanceId` that differs from `payload.storageInstanceId`, or an authenticated `environmentId` that differs from the descriptor hint, classifies as `host-identity-mismatch` (detail: "the server behind this endpoint does not match the pairing code") — the Noise key proved _a_ host, but not the one that minted the code.
- Produces:

```ts
export type PairingAddFailureReason =
  | "unreachable"
  | "host-identity-mismatch"
  | "pairing-rejected"
  | "incompatible"
  | "duplicate-storage-identity";

export class PairingAddError extends Schema.TaggedErrorClass<PairingAddError>()("PairingAddError", {
  reason: Schema.Literals([
    "unreachable",
    "host-identity-mismatch",
    "pairing-rejected",
    "incompatible",
    "duplicate-storage-identity",
  ]),
  detail: Schema.String,
}) {}

export class PairingLoopbackAcknowledgementRequiredError extends Schema.TaggedErrorClass<PairingLoopbackAcknowledgementRequiredError>()(
  "PairingLoopbackAcknowledgementRequiredError",
  { endpoint: Schema.String },
) {}

export interface VerifyPairingCodeInput {
  readonly code: string; // bare code, deep link, or browser URL
  readonly allowLoopbackTunnel?: boolean; // the explicit tunnel acknowledgement
}

export const verifyAndAddPairingCode: (input: VerifyPairingCodeInput) => Effect.Effect<
  EnvironmentId,
  | PairingAddError
  | PairingLoopbackAcknowledgementRequiredError
  | PairingCodeParseError
  | PairingCodeUnsupportedVersionError
  | ConnectionAttemptError
  | Persistence.ConnectionPersistenceError
  /* services: registry, presentation, http, sessions, credentials, identities */
>;
```

Phase 4's Add Server dialog consumes `ConnectionOnboarding.verifyAndAddPairingCode` and switches copy on the tagged errors.

- [x] **Step 1: Write the failing tests**

`pairingAdd.test.ts`, following `onboarding.test.ts`'s service-stub idiom (stub `HttpClient` for the descriptor fetch, `RpcSessionFactory` — whose session stub exposes `e2eeAuthenticated`/`initialConfig`/`ready`/`probe` — `EnvironmentRegistry`, and `AcceptedStorageIdentityStore`). One helper builds a valid code via `encodePairingCode`. Cases (each asserts the exact tagged error / reason):

```ts
const validPayload = (overrides?: Partial<RemotePairingCodePayload>): RemotePairingCodePayload => ({
  v: 1,
  endpoint: "http://192.168.1.20:3773",
  name: "AI-SERVER",
  token: "BCDFGHJKMNPQ",
  hostKey: "HcMLXPPBHFNvcbHrCVMH-DMh49rd5AGCzSCqAVJ49hM",
  reach: "another-device",
  storageInstanceId: "3f2f6a52-6f5f-4f4e-9d38-0a1e2ac21d11",
  ...overrides,
});
```

1. `loopback endpoint without acknowledgement fails with PairingLoopbackAcknowledgementRequiredError` — payload endpoint `http://127.0.0.1:3773` (regardless of `reach`; a `"custom"` code with a loopback endpoint must also trigger it — classification keys off `classifyPairingEndpoint(payload.endpoint)`, not `payload.reach`), no `allowLoopbackTunnel`. Nothing is fetched (assert the HTTP stub was never called).
2. `loopback endpoint with acknowledgement proceeds` — same code, `allowLoopbackTunnel: true`, happy-path stubs → resolves with the environment id and registers a `BearerConnectionRegistration` whose profile `hostKey` equals the payload's and whose label is the payload `name`.
3. `unconnectable endpoints fail as unreachable` — endpoint `http://0.0.0.0:3773` → `PairingAddError` reason `"unreachable"` (wildcard hosts cannot be dialed; fail before any network call).
4. `descriptor fetch failure classifies as unreachable` — HTTP stub fails with a network error.
5. `incompatible verdicts block the add` — descriptor stub reports a compat verdict of `server-too-old` (drive whatever descriptor fields Phase 2's `computeCompatVerdict` reads) → reason `"incompatible"`; also assert `legacy` does **not** block (spec §4.4: legacy renders as limited, capability downgrade governs).
6. `no plaintext credential exchange happens` — happy path; assert via the HTTP stub that only the descriptor path (`/.well-known/bibcode/environment`) was requested: never `/oauth/token`, never `/api/auth/websocket-ticket` (amended spec §4.3).
7. `in-channel pairing rejection classifies as pairing-rejected` — `RpcSessionFactory.connect` stub fails with `ConnectionBlockedError({ reason: "authentication", ... })` (the mapped `e2ee_error` `unauthorized` from the pairing form).
8. `E2EE probe mismatch classifies as host-identity-mismatch` — connect stub fails with `ConnectionBlockedError({ reason: "host-identity", ... })` (the mapped 4403 close / message-B AEAD failure).
9. `E2EE probe transport failure classifies as unreachable` — connect stub fails with `ConnectionTransientError`; assert the flow surfaces `unreachable` AND that nothing was persisted — the unconsumed one-time token stays retryable (spec §4.3), so a second call with working stubs succeeds.
10. `already-registered environment classifies as duplicate-storage-identity` — registry stub already holds an entry with the descriptor's environmentId.
11. `known storage identity classifies as duplicate-storage-identity` — `AcceptedStorageIdentityStore.get` for an existing bearer target key returns the payload's `storageInstanceId`.
12. `authenticated storage identity must match the pairing payload` — the session stub's `e2eeAuthenticated` reports a `storageInstanceId` different from `payload.storageInstanceId` → `host-identity-mismatch`, and nothing is registered.
13. `authenticated environment id must match the descriptor hint` — the session stub's `initialConfig` reports an environmentId different from the descriptor's → `host-identity-mismatch`, and nothing is registered.
14. `success persists hostKey and returns the environment id` — assert the registration passed to `registry.register`:
    - `target`: `BearerConnectionTarget` with `connectionId: "bearer:<environmentId>"`,
    - `profile.hostKey === payload.hostKey`, `profile.httpBaseUrl` normalized from `payload.endpoint`, `profile.wsBaseUrl === deriveWsBaseUrl(...)`,
    - `credential.token` is the **in-channel-minted credential from the `e2ee_authenticated` reply** (never the one-time pairing token),
    - the accepted storage identity for the new target key was recorded (`AcceptedStorageIdentityStore.accept`) with the authenticated `storageInstanceId`,
    - the probe's `PreparedConnection.e2ee.auth` was `{ kind: "pairing", token: payload.token }`.

- [x] **Step 2: Run to verify failure**

Run (from `packages/client-runtime`): `vp test run src/connection/pairingAdd.test.ts`
Expected: FAIL (module missing).

- [x] **Step 3: Implement `pairingAdd.ts`**

```ts
import { type EnvironmentId, type RemotePairingCodePayload } from "@bibcode/contracts";
import { classifyPairingEndpoint } from "@bibcode/shared/advertisedEndpoint";
import {
  PairingCodeParseError,
  PairingCodeUnsupportedVersionError,
  parsePairingCode,
} from "@bibcode/shared/pairingCode";
import * as Effect from "effect/Effect";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";
import * as SubscriptionRef from "effect/SubscriptionRef";
import * as HttpClient from "effect/unstable/http/HttpClient";

import { e2eeSocketUrl } from "../authorization/remote.ts";
import { fetchRemoteEnvironmentDescriptor } from "../environment/descriptor.ts";
import { deriveWsBaseUrl, normalizeHttpBaseUrl } from "../environment/endpoint.ts";
import * as Persistence from "../platform/persistence.ts";
import * as RpcSession from "../rpc/session.ts";
import {
  BearerConnectionCredential,
  BearerConnectionProfile,
  BearerConnectionRegistration,
} from "./catalog.ts";
import { computeCompatVerdict } from "./compat.ts"; // Phase 2 export — bind to its actual name
import { mapRemoteEnvironmentError } from "./errors.ts";
import {
  BearerConnectionTarget,
  type ConnectionAttemptError,
  type PreparedConnection,
} from "./model.ts";
import * as EnvironmentRegistry from "./registry.ts";
import { storageIdentityTargetKey } from "./storageIdentity.ts";

export type PairingAddFailureReason =
  | "unreachable"
  | "host-identity-mismatch"
  | "pairing-rejected"
  | "incompatible"
  | "duplicate-storage-identity";

export class PairingAddError extends Schema.TaggedErrorClass<PairingAddError>()("PairingAddError", {
  reason: Schema.Literals([
    "unreachable",
    "host-identity-mismatch",
    "pairing-rejected",
    "incompatible",
    "duplicate-storage-identity",
  ]),
  detail: Schema.String,
}) {
  override get message(): string {
    return this.detail;
  }
}

export class PairingLoopbackAcknowledgementRequiredError extends Schema.TaggedErrorClass<PairingLoopbackAcknowledgementRequiredError>()(
  "PairingLoopbackAcknowledgementRequiredError",
  {
    endpoint: Schema.String,
  },
) {
  override get message(): string {
    return "This pairing code points at the host itself. Confirm you reach it through a tunnel (e.g. SSH port forwarding), then try again.";
  }
}

export interface VerifyPairingCodeInput {
  readonly code: string;
  readonly allowLoopbackTunnel?: boolean;
}

const classifyAttemptError = (error: ConnectionAttemptError): PairingAddError =>
  new PairingAddError(
    error._tag === "ConnectionBlockedError" && error.reason === "host-identity"
      ? { reason: "host-identity-mismatch", detail: error.detail }
      : error._tag === "ConnectionBlockedError" &&
          (error.reason === "authentication" || error.reason === "permission")
        ? { reason: "pairing-rejected", detail: error.detail }
        : { reason: "unreachable", detail: error.detail ?? String(error) },
  );

export const verifyAndAddPairingCode = Effect.fn(
  "clientRuntime.connection.pairingAdd.verifyAndAddPairingCode",
)(function* (input: VerifyPairingCodeInput) {
  const payload: RemotePairingCodePayload = yield* Effect.try({
    try: () => parsePairingCode(input.code),
    catch: (cause) => cause as PairingCodeParseError | PairingCodeUnsupportedVersionError,
  });

  // Endpoint classification (spec 4.2): loopback requires the tunnel acknowledgement;
  // unconnectable endpoints can never be dialed. Keys off the endpoint, not `reach`.
  switch (classifyPairingEndpoint(payload.endpoint)) {
    case "unconnectable":
      return yield* new PairingAddError({
        reason: "unreachable",
        detail: `The pairing endpoint ${payload.endpoint} is not a connectable address.`,
      });
    case "loopback":
      if (input.allowLoopbackTunnel !== true) {
        return yield* new PairingLoopbackAcknowledgementRequiredError({
          endpoint: payload.endpoint,
        });
      }
      break;
    case "private-network":
    case "public":
      break;
  }

  const registry = yield* EnvironmentRegistry.EnvironmentRegistry;
  const identities = yield* Persistence.AcceptedStorageIdentityStore;

  // Early duplicate detection against saved environments' accepted identities.
  const entries = yield* SubscriptionRef.get(registry.entries);
  for (const entry of entries.values()) {
    const accepted = yield* identities
      .get(storageIdentityTargetKey(entry.target))
      .pipe(Effect.orElseSucceed(() => Option.none<string>()));
    if (Option.isSome(accepted) && accepted.value === payload.storageInstanceId) {
      return yield* new PairingAddError({
        reason: "duplicate-storage-identity",
        detail: `${entry.target.label} already uses this server's storage identity.`,
      });
    }
  }

  const httpBaseUrl = yield* Effect.try({
    try: () => normalizeHttpBaseUrl(payload.endpoint),
    catch: () =>
      new PairingAddError({
        reason: "unreachable",
        detail: `The pairing endpoint ${payload.endpoint} is not a valid HTTP URL.`,
      }),
  });

  // Live probe 1: descriptor fetch.
  const descriptor = yield* fetchRemoteEnvironmentDescriptor({ httpBaseUrl }).pipe(
    Effect.mapError(
      (error) =>
        new PairingAddError({
          reason: "unreachable",
          detail: mapRemoteEnvironmentError(error).message,
        }),
    ),
  );

  if (entries.has(descriptor.environmentId)) {
    return yield* new PairingAddError({
      reason: "duplicate-storage-identity",
      detail: `${descriptor.label} is already saved.`,
    });
  }

  // Compatibility window (Phase 2).
  const verdict = computeCompatVerdict(descriptor);
  if (verdict.kind === "server-too-old" || verdict.kind === "client-too-old") {
    return yield* new PairingAddError({
      reason: "incompatible",
      detail:
        verdict.kind === "server-too-old"
          ? `${descriptor.label} runs a protocol older than this app supports. Update the server.`
          : `${descriptor.label} requires a newer app. Update this app.`,
    });
  }

  // Live probe: E2EE handshake with the pinned hostKey; the one-time pairing
  // token rides e2ee_auth's pairing form and the device credential is minted
  // INSIDE the channel (amended spec section 4.3 — no /oauth/token, no ticket).
  // The descriptor above was a routing hint; the channel's authenticated
  // identity is what gets trusted and persisted.
  const sessions = yield* RpcSession.RpcSessionFactory;
  const connectionId = `bearer:${descriptor.environmentId}`;
  const target = new BearerConnectionTarget({
    environmentId: descriptor.environmentId,
    label: payload.name,
    connectionId,
  });
  const prepared: PreparedConnection = {
    environmentId: descriptor.environmentId,
    label: payload.name,
    descriptor,
    httpBaseUrl,
    socketUrl: e2eeSocketUrl(deriveWsBaseUrl(httpBaseUrl)),
    httpAuthorization: null,
    e2ee: {
      hostKey: payload.hostKey,
      auth: { kind: "pairing", token: payload.token },
    },
    target,
  };
  const verified = yield* Effect.scoped(
    Effect.gen(function* () {
      const session = yield* sessions.connect(prepared);
      yield* session.ready;
      const authenticated = yield* session.e2eeAuthenticated;
      if (authenticated === null || authenticated.credential === undefined) {
        return yield* new PairingAddError({
          reason: "pairing-rejected",
          detail: "The host did not return a device credential.",
        });
      }
      // Identity re-verification (mirrors driver.ts's two checks): the
      // authenticated identity must match the pairing payload and the hint.
      if (authenticated.storageInstanceId !== payload.storageInstanceId) {
        return yield* new PairingAddError({
          reason: "host-identity-mismatch",
          detail: "The server behind this endpoint does not match the pairing code.",
        });
      }
      const config = yield* session.initialConfig; // authenticated server.getConfig
      const authenticatedEnvironmentId =
        config.environment?.environmentId ?? authenticated.environmentId;
      if (authenticatedEnvironmentId !== descriptor.environmentId) {
        return yield* new PairingAddError({
          reason: "host-identity-mismatch",
          detail: "The server behind this endpoint does not match the pairing code.",
        });
      }
      return {
        credential: authenticated.credential,
        environmentId: descriptor.environmentId,
        storageInstanceId: authenticated.storageInstanceId,
      };
    }),
  ).pipe(
    Effect.mapError((error) =>
      error instanceof PairingAddError ? error : classifyAttemptError(error),
    ),
  );
  // (Verify the exact ServerConfig field path for the environment id against
  // `packages/contracts/src/server.ts` — driver.ts reads `initialConfig.environment`;
  // reuse the same accessor it passes to verifyPreparedStorageIdentity.)

  // Persist: profile carries hostKey; credential is the in-channel-minted bearer.
  const registration = new BearerConnectionRegistration({
    target,
    profile: new BearerConnectionProfile({
      connectionId,
      environmentId: verified.environmentId,
      label: payload.name,
      httpBaseUrl,
      wsBaseUrl: deriveWsBaseUrl(httpBaseUrl),
      hostKey: payload.hostKey,
    }),
    credential: new BearerConnectionCredential({ token: verified.credential }),
  });
  yield* registry.register(registration);
  yield* identities.accept({
    targetKey: storageIdentityTargetKey(target),
    storageInstanceId: verified.storageInstanceId,
  });
  // (Match AcceptedStorageIdentity's real field names in platform/persistence.ts.)
  return registration.target.environmentId as EnvironmentId;
});
```

Wire into `ConnectionOnboarding` (`onboarding.ts`): add `verifyAndAddPairingCode` to the service interface and `make` (providing `EnvironmentRegistry`, `HttpClient`, `RpcSessionFactory`, `AcceptedStorageIdentityStore` — mirror how `registerPairing` provides its services). Update `layer.ts` so the onboarding layer receives the two new services (`RpcSessionFactory` is already built there for the driver; reuse that layer value, and provide `AcceptedStorageIdentityStore` from the persistence context).

- [x] **Step 4: Run to verify pass**

Run (from `packages/client-runtime`): `vp test run src/connection/pairingAdd.test.ts src/connection/onboarding.test.ts`
Expected: PASS (existing onboarding tests untouched).

- [x] **Step 5: Commit**

```bash
git add packages/client-runtime/src/connection/pairingAdd.ts packages/client-runtime/src/connection/pairingAdd.test.ts packages/client-runtime/src/connection/onboarding.ts packages/client-runtime/src/connection/layer.ts
git commit -m "feat(client-runtime): verify-then-add pairing flow with classified failures"
```

---

### Task 14: Cross-language interop test (TS initiator ↔ Rust responder)

The repo has no existing TS test that spawns the server binary (verified: no `bibcode serve`/`CARGO_BIN` usage under `packages/`), so this task introduces an opt-in vitest suite driving the real `bibcode serve` with the TS Noise modules directly (Node's global `WebSocket` + `fetch`; no effect Socket machinery — the crypto interop is the subject). The snow↔snow route tests (Task 5) and the noble↔noble socket tests (Task 11) already cover each side alone; this test is the cross-language proof, plus the official vectors (Task 9) anchor both to the standard.

**Files:**

- Create: `packages/client-runtime/src/e2ee/serverInterop.test.ts`

**Interfaces:**

- Consumes: `createNkInitiator`, `decodeBase64UrlKey` (Task 9); `splitIntoRecords`, `RecordAssembler` (Task 10); `parsePairingCode` (Task 7); the running server's `/oauth/token`, `/api/auth/websocket-ticket`, `POST /api/auth/pairing-offer` (Task 8), `/ws-e2ee` (Task 5), and the secret-file layout (Task 2, cross-check only).
- Produces: the executable interop proof, gated on `BIBCODE_E2EE_SERVER_BIN`. The pairing code is obtained through the real mint endpoint, so the suite also proves the full mint → parse → pin → pair loop across languages.

- [x] **Step 1: Write the test**

```ts
// Cross-language E2EE interop: TS Noise NK initiator against the real Rust
// responder. Opt-in: requires a built server binary.
//
//   cargo build -p bibcode-server
//   BIBCODE_E2EE_SERVER_BIN=$PWD/target/debug/bibcode vp test run src/e2ee/serverInterop.test.ts
//
// @effect-diagnostics nodeBuiltinImport:off
import { spawn, type ChildProcess } from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import { createInterface } from "node:readline";

import { afterAll, beforeAll, describe, expect, it } from "@effect/vitest";
import type { RemotePairingCodePayload } from "@bibcode/contracts";
import { parsePairingCode } from "@bibcode/shared/pairingCode";

import { RecordAssembler, splitIntoRecords } from "./frame.ts";
import { createNkInitiator, decodeBase64UrlKey, type NkTransport } from "./noise.ts";

const serverBinary = process.env["BIBCODE_E2EE_SERVER_BIN"];

interface RunningServer {
  process: ChildProcess;
  httpBaseUrl: string;
  token: string;
  dataRoot: string;
}

async function startServer(): Promise<RunningServer> {
  const dataRoot = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "bibcode-e2ee-"));
  const child = spawn(serverBinary!, [
    "serve",
    "--host",
    "127.0.0.1",
    "--port",
    "0",
    "--base-dir",
    dataRoot,
    "--no-browser",
  ]);
  // `serve` prints one JSON readiness line:
  //   {"address":"127.0.0.1:PORT","httpBaseUrl":"http://...","token":"...","pairingUrl":"..."}
  const startup = await new Promise<{ httpBaseUrl: string; token: string }>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("server did not report readiness")), 30_000);
    createInterface({ input: child.stdout! }).on("line", (line) => {
      try {
        const parsed = JSON.parse(line) as { httpBaseUrl?: string; token?: string };
        if (parsed.httpBaseUrl && parsed.token) {
          clearTimeout(timer);
          resolve({ httpBaseUrl: parsed.httpBaseUrl, token: parsed.token });
        }
      } catch {
        /* non-JSON log line */
      }
    });
    child.on("exit", (code) => reject(new Error(`server exited early (${code})`)));
  });
  return { process: child, dataRoot, ...startup };
}

function readHostPublicKey(dataRoot: string): Uint8Array {
  // Task 2 pins the record layout: 32 private bytes then 32 public bytes.
  const record = NodeFS.readFileSync(
    NodePath.join(dataRoot, "userdata", "secrets", "host-identity-x25519.bin"),
  );
  expect(record).toHaveLength(64);
  return Uint8Array.from(record.subarray(32));
}

async function exchangeAccessToken(server: RunningServer, subjectToken: string): Promise<string> {
  const tokenResponse = await fetch(`${server.httpBaseUrl}/oauth/token`, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      grant_type: "urn:ietf:params:oauth:grant-type:token-exchange",
      subject_token: subjectToken,
      subject_token_type: "urn:bibcode:params:oauth:token-type:environment-bootstrap",
      requested_token_type: "urn:ietf:params:oauth:token-type:access_token",
    }),
  });
  expect(tokenResponse.ok).toBe(true);
  const { access_token } = (await tokenResponse.json()) as { access_token: string };
  return access_token;
}

// NOTE: verify the three token-exchange constants above against
// packages/contracts/src/auth.ts (AuthTokenExchangeGrantType,
// AuthEnvironmentBootstrapTokenType, AuthAccessTokenType) and import them from
// "@bibcode/contracts" instead of string literals.

/**
 * Mints a pairing code through the real endpoint (Task 8) and parses it with the
 * shared codec — the same path a sharing host and an adding client use.
 */
async function mintedPairing(server: RunningServer): Promise<{
  payload: RemotePairingCodePayload;
  hostKey: Uint8Array;
}> {
  const adminToken = await exchangeAccessToken(server, server.token);
  const response = await fetch(`${server.httpBaseUrl}/api/auth/pairing-offer`, {
    method: "POST",
    headers: {
      authorization: `Bearer ${adminToken}`,
      "content-type": "application/json",
    },
    body: JSON.stringify({
      name: "Interop",
      endpoint: server.httpBaseUrl,
      reach: "custom",
    }),
  });
  expect(response.ok).toBe(true);
  const offer = (await response.json()) as { code: string };
  const payload = parsePairingCode(offer.code);
  const hostKey = decodeBase64UrlKey(payload.hostKey);
  // Cross-check the pinned key against the persisted record (Task 2 layout).
  expect(hostKey).toEqual(readHostPublicKey(server.dataRoot));
  return { payload, hostKey };
}

interface EncryptedSocket {
  socket: WebSocket;
  transport: NkTransport;
  nextMessage: () => Promise<string>;
  sendMessage: (text: string) => void;
  close: () => void;
}

async function openEncrypted(server: RunningServer, hostKey: Uint8Array): Promise<EncryptedSocket> {
  const wsUrl = server.httpBaseUrl.replace(/^http/, "ws") + "/ws-e2ee";
  const socket = new WebSocket(wsUrl);
  socket.binaryType = "arraybuffer";
  const frames: Array<Uint8Array> = [];
  let notify: (() => void) | null = null;
  socket.addEventListener("message", (event) => {
    frames.push(new Uint8Array(event.data as ArrayBuffer));
    notify?.();
  });
  const nextFrame = async (): Promise<Uint8Array> => {
    while (frames.length === 0) {
      await new Promise<void>((resolve) => {
        notify = resolve;
        setTimeout(resolve, 50);
      });
    }
    return frames.shift()!;
  };
  await new Promise<void>((resolve, reject) => {
    socket.addEventListener("open", () => resolve(), { once: true });
    socket.addEventListener("error", () => reject(new Error("ws open failed")), { once: true });
  });
  const initiator = createNkInitiator({ responderStaticPublicKey: hostKey });
  socket.send(initiator.writeMessageA(new Uint8Array(0)));
  initiator.readMessageB(await nextFrame());
  const transport = initiator.split();
  const assembler = new RecordAssembler();
  const decoder = new TextDecoder();
  const nextMessage = async (): Promise<string> => {
    for (;;) {
      const record = transport.receive.decryptWithAd(new Uint8Array(0), await nextFrame());
      const message = assembler.push(record);
      if (message !== null) {
        return decoder.decode(message);
      }
    }
  };
  const sendMessage = (text: string): void => {
    for (const record of splitIntoRecords(new TextEncoder().encode(text))) {
      socket.send(transport.send.encryptWithAd(new Uint8Array(0), record));
    }
  };
  return { socket, transport, nextMessage, sendMessage, close: () => socket.close() };
}

describe.skipIf(serverBinary === undefined)("TS initiator against the Rust responder", () => {
  let server: RunningServer;

  beforeAll(async () => {
    server = await startServer();
  }, 60_000);

  afterAll(() => {
    server?.process.kill();
    if (server) NodeFS.rmSync(server.dataRoot, { recursive: true, force: true });
  });

  it("mints a pairing offer, bootstraps in-channel, and round-trips server.getConfig encrypted", async () => {
    // The full amended-spec loop: HTTP mint (admin, plain-minted session) ->
    // parse the code -> pin its hostKey -> Noise NK -> e2ee_auth pairing form
    // (bootstrap INSIDE the channel; no /oauth/token or ticket for the target)
    // -> bearer-form reconnect with the minted credential -> RPC.
    const { payload, hostKey } = await mintedPairing(server);
    const channel = await openEncrypted(server, hostKey);
    channel.sendMessage(JSON.stringify({ type: "e2ee_auth", pairing: payload.token }));
    const authenticated = JSON.parse(await channel.nextMessage()) as {
      type: string;
      credential?: string;
      storageInstanceId?: string;
    };
    expect(authenticated.type).toBe("e2ee_authenticated");
    expect(authenticated.credential).toBeTruthy();
    expect(authenticated.storageInstanceId).toBe(payload.storageInstanceId);
    channel.sendMessage(
      JSON.stringify({
        _tag: "Request",
        id: "1",
        tag: "server.getConfig",
        payload: {},
        headers: [],
      }),
    );
    const response = JSON.parse(await channel.nextMessage()) as { requestId?: string; id?: string };
    // The response envelope carries the request id back; assert it and that no
    // ClientProtocolError arrived (which would mean framing corrupted the JSON).
    expect(JSON.stringify(response)).toContain('"1"');
    expect(JSON.stringify(response)).not.toContain("ClientProtocolError");
    channel.close();

    // Reconnect with the in-channel-minted credential (bearer form).
    const second = await openEncrypted(server, hostKey);
    second.sendMessage(JSON.stringify({ type: "e2ee_auth", bearer: authenticated.credential }));
    expect(JSON.parse(await second.nextMessage())).toEqual({ type: "e2ee_authenticated" });
    second.close();
  }, 30_000);

  it("reassembles a fragmented client message (large frames cross the language boundary)", async () => {
    const { payload, hostKey } = await mintedPairing(server);
    const channel = await openEncrypted(server, hostKey);
    channel.sendMessage(JSON.stringify({ type: "e2ee_auth", pairing: payload.token }));
    await channel.nextMessage(); // e2ee_authenticated (with credential fields)
    // A >64 KiB request must fragment into multiple Noise frames; the server can
    // only answer with the request id if it reassembled and parsed the JSON.
    const padding = "x".repeat(200_000);
    channel.sendMessage(
      JSON.stringify({
        _tag: "Request",
        id: "2",
        tag: "server.getConfig",
        payload: { ignored: padding },
        headers: [],
      }),
    );
    const response = await channel.nextMessage();
    expect(response).toContain('"2"');
    channel.close();
  }, 30_000);

  it("rejects a bad pairing token with e2ee_error unauthorized", async () => {
    const { hostKey } = await mintedPairing(server);
    const channel = await openEncrypted(server, hostKey);
    channel.sendMessage(JSON.stringify({ type: "e2ee_auth", pairing: "bogus" }));
    expect(JSON.parse(await channel.nextMessage())).toEqual({
      type: "e2ee_error",
      code: "unauthorized",
    });
  }, 30_000);
});
```

Adjust the response-envelope assertions to the actual `ServerMessage` JSON shapes in `apps/server/src/rpc/message.rs` (`_tag` variants) while implementing — the load-bearing assertions are: `e2ee_authenticated` arrives, the request id round-trips, no `ClientProtocolError`, and fragmentation survives the boundary. If `server.getConfig` responds with an `exit`/`chunk` pair, drain messages until the id appears.

- [x] **Step 2: Run against a freshly built server**

```bash
cargo build -p bibcode-server
cd packages/client-runtime
BIBCODE_E2EE_SERVER_BIN=$(git rev-parse --show-toplevel)/target/debug/bibcode vp test run src/e2ee/serverInterop.test.ts
```

Expected: 3 passing tests. Without the env var: the suite reports skipped (verify that too — plain `vp test run src/e2ee/serverInterop.test.ts` must not fail CI).

- [x] **Step 3: Commit**

```bash
git add packages/client-runtime/src/e2ee/serverInterop.test.ts
git commit -m "test(e2ee): cross-language Noise NK interop against bibcode serve"
```

---

### Task 15: Post-auth HTTP audit, living docs, spec alignment, runbooks, final gate

The amended spec §4.3 pins the in-channel bootstrap: for hostKey targets the **only** pre-auth HTTP is the unauthenticated descriptor fetch (`/.well-known/bibcode/environment`). Any other HTTP call against such a target must move to the RPC channel or be documented as an exception in `docs/architecture/remote.md`. Note additionally that the spec's trailing "HTTP calls for hostKey-bearing targets" bullet still lists `/oauth/token` + `/api/auth/websocket-ticket` as allowed bootstrap — leftover pre-amendment text that now contradicts the in-channel-bootstrap bullet above it; Step 2 aligns it and the final report flags the edit for the spec owner.

**Files:**

- Modify: `docs/architecture/remote.md`
- Modify: `docs/architecture/connection-runtime.md`
- Modify: `docs/plans/remote-servers/remote-servers-spec.md` (§4.3 record-layer amendment)
- Possibly modify: `packages/client-runtime/src/rpc/http.ts` call sites (audit outcome)
- Review: `docs/testing/linux-desktop.md`, `docs/testing/windows-desktop.md`, `docs/testing/macos-desktop.md`, `docs/testing/cross-platform-validation.md`, `docs/testing/README.md`

- [x] **Step 1: Audit post-auth HTTP usage against bearer targets**

```bash
rg -n "httpBaseUrl|environmentEndpointUrl|httpAuthorization" packages/client-runtime/src --type ts | rg -v "test"
rg -n "createUrl|logs.zip|/api/" packages/client-runtime/src apps/web/src --type ts | rg -v "test" | rg -v "auth"
```

For each hit that can execute against a Bearer target with a stored `hostKey` (candidates found in recon: the diagnostics log download `POST /api/diagnostics/logs.zip`, asset URLs from `assets.createUrl`, and any `rpc/http.ts` consumer outside the descriptor fetch): decide per call —

- already rides the WS RPC → nothing to do;
- authenticated plain HTTP → **now non-functional by design** for hostKey targets: the no-downgrade rule (Task 5, Cycle A) rejects e2ee-minted bearers on plain-HTTP surfaces. Either move the call onto the RPC channel or document the feature as unavailable for hostKey targets in `remote.md` (an "exception" can no longer mean "send the bearer over plain HTTP" — that hole is exactly what the amendment closed);
- unauthenticated by nature (descriptor) → fine.

Record the complete audit table (call site → verdict) in `remote.md`; do not silently skip any hit.

- [x] **Step 2: Update the living docs (same patch as behavior)**

`docs/architecture/remote.md` — new "Direct-connection E2EE" section covering: host identity key (storage name, encoding, distribution-only-via-pairing-codes), `/ws-e2ee` (handshake, record layer with the flag byte and both caps, in-channel `e2ee_auth` two-form bootstrap with the pairing-token-consumed-only-on-success rule, no-downgrade `transport` claim, pre-auth hardening: 64 KiB auth cap, combined deadline, connection cap, empty-payload rule, 4403 wrong-key close, pump write/join timeouts, `e2ee_error` close paths, nonce policy: no rekey, counter bound, fail-closed), pairing-code format + `POST /api/auth/pairing-offer` (scope `access:write`; idempotent via `Idempotency-Key`; reach validated and embedded but persisted only from Phase 5 on), verify-then-add classifications incl. the in-channel identity re-verification, the channel-selection rule and legacy-`/ws` badge, and the Step 1 HTTP audit table.

`docs/architecture/connection-runtime.md` — `hostKey` on `BearerConnectionProfile` (decode-default null; additive to the schema-v1 catalog document — no version bump; desktop protected storage treats the document opaquely), `PreparedConnection.e2ee` (auth forms), `RpcSession.e2eeAuthenticated`, the `/ws-e2ee` selection rule in the resolver/session factory (no ticket issuance for hostKey targets), and `connectionTransportSecurity`.

`docs/plans/remote-servers/remote-servers-spec.md` §4.3 — the framing and bootstrap bullets
were amended during review (2026-08-27). **Verify** they match what this phase implemented
(record framing constants; two-form `e2ee_auth`; no-downgrade; pre-auth hardening; 4403).
**Align** the leftover trailing bullet ("HTTP calls for hostKey-bearing targets") to the
amendment: for hostKey targets the pre-auth allowance is the descriptor fetch only — remove
`/oauth/token` and `/api/auth/websocket-ticket` from its list; flag the edit for the spec
owner in the final report. Amend nothing else.

- [x] **Step 3: Review the testing runbooks**

Read the five `docs/testing/` documents. This phase adds one opt-in test command (the Task 14 interop suite) and new always-on suites that run under the existing `vp test` umbrella. If a runbook enumerates test commands or validation evidence for the affected areas, add the interop command with its `BIBCODE_E2EE_SERVER_BIN` prerequisite; if none does, the final report must state the runbooks were **reviewed and remain accurate**.

- [x] **Step 4: Full validation gate (master plan)**

```bash
vp check
vp run typecheck
cargo fmt --all --check
cargo test -p bibcode-server
cargo clippy -p bibcode-server --all-targets -- -D warnings
cd packages/contracts && vp test run && cd ../..
cd packages/shared && vp test run && cd ../..
cd packages/client-runtime && vp test run && cd ../..
cargo build -p bibcode-server && cd packages/client-runtime && BIBCODE_E2EE_SERVER_BIN=$(git rev-parse --show-toplevel)/target/debug/bibcode vp test run src/e2ee/serverInterop.test.ts && cd ../..
git status --short && git diff --stat
```

Expected: all green; `git status` shows only intended files (and never resurrects the pending deletions under `docs/plans/2026-08-24-environment-project-management/`). Report the exact commands run, anything that could not run, and residual risk.

- [x] **Step 5: Commit**

```bash
git add docs/architecture/remote.md docs/architecture/connection-runtime.md docs/plans/remote-servers/remote-servers-spec.md packages/client-runtime/src docs/testing
git commit -m "docs(remote): document the E2EE channel, pairing codes, and record framing"
```

---

## Self-review checklist (run after writing code for every task, and once at the end)

1. **Spec coverage:** §4.1 (Task 2; distribution-only-in-codes enforced by Task 8 being the sole hostKey surface), §4.2 (Tasks 6, 7, 8, 13), §4.3 (Tasks 3–5, 9–12, 14, 15 alignment + HTTP audit) — including the amended in-channel bootstrap (Tasks 5B, 11, 13, 14), no-downgrade (Task 5A + test 3), pre-auth hardening (Tasks 4, 5B, 11), and identity re-verification (Task 13). §4.4 consumed, not redefined.
2. **Constant parity:** `65535 / 16 / 65518 / 64 KiB pre-auth / 64 MiB post-auth / 0x00 / 0x01 / 10 s combined deadline / 4403 / 32 pre-auth connections / 5 s write / 1 s join` appear in exactly two implementations (Rust `rpc/e2ee.rs` + `rpc/session.rs`, TS `e2ee/frame.ts` + `noise.ts` + `socket.ts`) and must be byte-identical; the Task 14 fragmentation test is the cross-check.
3. **Name consistency across tasks (and with Phase 5's Task 4):** `HostIdentity`, `NOISE_NK_PARAMS`, `RemotePairingCodePayload`, `/api/auth/pairing-offer` + `pairingOffer` + `create_pairing_offer` + `AuthPairingOfferResult` + `invalid_pairing_offer`, `makeE2eeSocket`, `PreparedE2eeChannel`, `hostKey`, `verifyAndAddPairingCode`, `connectionTransportSecurity` — grep the diff for drift.
4. **Reference-product strings:** `git diff | grep -i` for the banned reference-product name must return nothing; also grep this plan's own file before finishing.
5. **No placeholder text** (`TBD`, `TODO`, "implement later") survives into committed code.
