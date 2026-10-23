# SSH Pairing Bootstrap Repair Implementation Plan (Remote Servers — Phase 1)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restore the desktop SSH bootstrap by adding a native CLI subcommand `bibcode pairing issue` that creates a one-time pairing link against a given data root and prints the credential JSON the desktop SSH launcher already parses, and by pointing the launcher at the new command.

**Architecture:** The desktop SSH launcher (`apps/desktop/src-tauri/src/ssh.rs`) launches a remote `bibcode serve --base-dir "$HOME/.bibcode"`, then runs a pairing CLI command over SSH and parses its stdout with `parse_remote_pairing_credential`. That command (`bibcode auth pairing create`) no longer exists; the native CLI exposes only `serve`, `start`, and `storage`. This phase adds a `pairing issue` subcommand that writes a one-time pairing link directly into the data root's `auth_pairing_links` SQLite table. This works while the server is running because (a) the server consumes pairing credentials **from the database**, not from its in-memory cache, whenever repositories are configured (`consume_grant`, `apps/server/src/auth/service.rs:897-918`), (b) the store runtime lock is a **shared** lock (`StoreRuntimeGuard::acquire` uses `try_lock_shared`, `apps/server/src/persistence/backup.rs:306-343`), and (c) the database runs in WAL mode with a 5-second busy timeout (`apps/server/src/persistence/database.rs:25,780-793`), so a second process can insert safely.

**Tech Stack:** Rust only — clap (derive) CLI in `apps/server`, rusqlite/tokio persistence, Tauri 2 desktop crate for the SSH launcher. No TypeScript changes.

**Spec:** `docs/plans/remote-servers/remote-servers-spec.md` §4.7 (decision D15). Master plan: `docs/plans/remote-servers/remote-servers-plan.md` (this file is Phase 1). Current-state survey: `docs/plans/remote-servers/bibcode-current-state.md` §6 (verified against source; line references below are from the current source, re-locate by symbol name if drifted).

## Global Constraints

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

(Phase note: this phase adds no WS methods, no contract fields, and no TS code, so the
contracts/parity constraints are trivially satisfied; `vp check` and `vp run typecheck`
still run in the final gate.)

---

## Pinned facts (verified in source — the contract this phase must honor)

1. **Credential JSON shape the desktop expects.** `parse_remote_pairing_credential`
   (`apps/desktop/src-tauri/src/ssh.rs:1287-1302`) takes the **last non-empty line** of
   the remote command's stdout, parses it as JSON, and requires a non-empty string field
   named `credential` (it trims the value; every other field is ignored):

   ```json
   { "credential": "WXYZ23456789" }
   ```

   The new CLI therefore prints exactly one JSON line to stdout whose `credential` key
   holds the pairing credential. Extra keys (`id`, `label`, `expiresAt`) are allowed and
   ignored by the parser. Nothing else may be printed to stdout on the success path (the
   pairing code path must not initialize logging or print banners; errors go to stderr).

2. **Broken invocation being replaced.** `issue_remote_pairing_token`
   (`apps/desktop/src-tauri/src/ssh.rs:1366-1402`) currently runs, over SSH via
   `sh -lc`:

   ```
   bibcode auth pairing create --base-dir "$HOME/.bibcode" --json
   ```

   The native CLI (`apps/server/src/config.rs:241-249`) exposes only `Serve`, `Start`,
   and `Storage(StorageArgs)` — the command fails on every fresh SSH setup.

3. **Remote data root.** The remote launch script starts the server with
   `serve --host 127.0.0.1 --port "$REMOTE_PORT" --base-dir "$SERVER_HOME"` where
   `SERVER_HOME="$HOME/.bibcode"` (`apps/desktop/src-tauri/src/ssh.rs:57-148`). The
   pairing command must target the same `--base-dir` so both processes share
   `<base_dir>/userdata/state.sqlite` (`ServerConfig::state_dir`/`database_path`,
   `apps/server/src/config.rs:139-151`).

4. **How the credential is consumed.** The desktop exchanges the credential at
   `POST /oauth/token` as a one-time bootstrap token
   (`desktop_bridge_bootstrap_ssh_bearer_session`,
   `apps/desktop/src-tauri/src/bridge.rs:1126-1149`). Server side,
   `exchange_bootstrap` → `consume_grant` reads `auth_pairing_links` **directly from the
   database** when repositories are present (always true for a real server), so a row
   inserted by a separate CLI process is honored without a restart.

5. **Issuance policy to mirror.** The startup pairing (`issue_startup_pairing`,
   `apps/server/src/auth/service.rs:531-540`) issues `ADMINISTRATIVE_SCOPES` with
   subject `"administrative-bootstrap"`, method `"one-time-token"`, and
   `PAIRING_TTL_MS = 5 minutes`. The removed command's help text described "a
   five-minute environment administrator pairing" — the SSH bootstrap is the
   administrative owner of that environment, so this phase mirrors the startup pairing
   exactly (scopes, subject, TTL, 12-character credential from `PAIRING_ALPHABET`).

6. **CLI conventions to follow.** The `storage` subcommand family
   (`apps/server/src/config.rs:251-302,380-408`) is the template: a `Subcommand` enum, a
   `CliAction` variant carrying a `ResolvedDataRoot` (resolved via
   `select_data_root_request(args.base_dir, BIBCODE_HOME, None, home_dir)` +
   `resolve_data_root`), a `--json` flag per leaf command, and a runner in
   `apps/server/src/lib.rs` (`run_storage_command`). `--base-dir` is already a global
   arg (`ServerArgs`, `apps/server/src/config.rs:315-316`), so
   `bibcode pairing issue --base-dir X` parses without new flag plumbing.

## File structure

- `apps/server/src/auth/service.rs` — new free function
  `issue_administrative_pairing_link` (issuance policy stays in the auth package) + unit
  test. `apps/server/src/auth/mod.rs` re-exports it `pub(crate)`.
- `apps/server/src/config.rs` — `Pairing` subcommand parsing, `PairingCommand` action,
  `ConfigError::PairingCommandIsNotServer` + parse test.
- `apps/server/src/lib.rs` — `run_pairing_command` runner, `RunError` variants, export
  of `PairingCommand`.
- `apps/server/tests/cli_smoke.rs` — end-to-end tests (real binary, real running
  server, real token exchange).
- `apps/desktop/src-tauri/src/ssh.rs` — `REMOTE_PAIRING_ISSUE_COMMAND` constant used by
  `issue_remote_pairing_token`.
- `apps/desktop/src-tauri/tests/ssh_public_contract.rs` — pins the invoked command and
  the parser's tolerance of the real CLI output.
- `docs/architecture/remote.md` — limitation entry removed, bootstrap documented.

---

### Task 1: Administrative pairing issuance seam in the auth package

**Files:**

- Modify: `apps/server/src/auth/service.rs` (new free function after
  `generate_pairing_credential`, ~line 1184; new test in the existing `mod tests`)
- Modify: `apps/server/src/auth/mod.rs:19` (extend the `service` re-export)

**Interfaces:**

- Consumes: existing private items in `service.rs` — `PairingRecord`,
  `persisted_pairing_link`, `generate_pairing_credential`, `PAIRING_TTL_MS`,
  `MAX_ACTIVE_PAIRINGS`, `owned_scopes`, `ADMINISTRATIVE_SCOPES`, `format_iso`,
  `now_ms`; `crate::persistence::Repositories`
  (`create_auth_pairing_link`, `list_active_auth_pairing_links`).
- Produces (Task 2 relies on this exact signature):

  ```rust
  pub(crate) async fn issue_administrative_pairing_link(
      repositories: &Repositories,
      label: Option<String>,
  ) -> Result<PairingCredentialResult, AuthError>
  ```

  `PairingCredentialResult` is the existing struct in `apps/server/src/auth/model.rs`
  (fields `id: String`, `credential: String`, `label: Option<String>`,
  `expires_at: String`).

- [x] **Step 1: Write the failing test**

Append inside the existing `#[cfg(test)] mod tests` in
`apps/server/src/auth/service.rs` (it already has `use super::*;` and imports
`ClientMetadata`, `owned_scopes`, `ADMINISTRATIVE_SCOPES` via the file header):

```rust
    #[tokio::test]
    async fn issues_a_persisted_administrative_pairing_a_running_service_exchanges_once() {
        let database = crate::persistence::Database::open_in_memory()
            .await
            .expect("in-memory database opens");
        database
            .call(|connection| {
                crate::persistence::run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("all migrations apply");
        let repositories = Repositories::new(database);

        let issued =
            issue_administrative_pairing_link(&repositories, Some("SSH bootstrap".to_owned()))
                .await
                .expect("pairing link issues");
        assert_eq!(issued.credential.len(), PAIRING_LENGTH);
        assert!(
            issued
                .credential
                .bytes()
                .all(|byte| PAIRING_ALPHABET.contains(&byte))
        );
        assert_eq!(issued.label.as_deref(), Some("SSH bootstrap"));

        let secrets = tempfile::tempdir().expect("secret store directory");
        let secret_store = SecretStore::new(secrets.path())
            .await
            .expect("secret store opens");
        let config = ServerConfig::new(".").with_bind("127.0.0.1", 3773);
        let service = AuthService::new_with_persistence(
            &config,
            vec![7_u8; 32],
            secret_store,
            repositories.clone(),
        )
        .await
        .expect("service hydrates over the same repositories");

        let session = service
            .exchange_bootstrap(&issued.credential, None, ClientMetadata::default(), None)
            .await
            .expect("credential exchanges once");
        assert_eq!(session.principal.scopes, owned_scopes(ADMINISTRATIVE_SCOPES));
        assert_eq!(session.principal.subject, "administrative-bootstrap");
        assert!(matches!(
            service
                .exchange_bootstrap(&issued.credential, None, ClientMetadata::default(), None)
                .await,
            Err(AuthError::InvalidCredential)
        ));
    }
```

- [x] **Step 2: Run the test to verify it fails**

Run (repository root):
`cargo test -p bibcode-server --lib auth::service::tests::issues_a_persisted_administrative_pairing_a_running_service_exchanges_once -- --exact`
Expected: compile error — `issue_administrative_pairing_link` not found.

- [x] **Step 3: Write the minimal implementation**

In `apps/server/src/auth/service.rs`, after `generate_pairing_credential` (~line 1184):

```rust
/// Issues a one-time administrative pairing link directly against a data
/// root's repositories, without a full [`AuthService`]. Used by the native CLI
/// (`bibcode pairing issue`) beside a running server: credential consumption
/// reads `auth_pairing_links` from the database, so the running server honors
/// links inserted here. Mirrors `issue_startup_pairing` (administrative
/// scopes, `administrative-bootstrap` subject, five-minute TTL).
pub(crate) async fn issue_administrative_pairing_link(
    repositories: &Repositories,
    label: Option<String>,
) -> Result<PairingCredentialResult, AuthError> {
    let now = now_ms();
    let active = repositories
        .list_active_auth_pairing_links(format_iso(now))
        .await
        .map_err(|error| AuthError::Internal(error.to_string()))?;
    if active.len() >= MAX_ACTIVE_PAIRINGS {
        return Err(AuthError::Internal(
            "active pairing capacity exceeded".to_owned(),
        ));
    }
    let record = PairingRecord {
        id: Uuid::new_v4().to_string(),
        credential: generate_pairing_credential()?,
        scopes: owned_scopes(ADMINISTRATIVE_SCOPES),
        subject: "administrative-bootstrap".to_owned(),
        label: label.clone(),
        proof_key_thumbprint: None,
        created_at_ms: now,
        expires_at_ms: now.saturating_add(PAIRING_TTL_MS),
        consumed_at_ms: None,
        revoked_at_ms: None,
    };
    repositories
        .create_auth_pairing_link(persisted_pairing_link(&record))
        .await
        .map_err(|error| AuthError::Internal(error.to_string()))?;
    Ok(PairingCredentialResult {
        id: record.id,
        credential: record.credential,
        label,
        expires_at: format_iso(record.expires_at_ms),
    })
}
```

Notes for the implementer:

- The capacity check races benignly with a running server (no shared transaction); the
  cap is a soft guard and the database `UNIQUE` constraint on `credential` fails closed
  on the astronomically unlikely collision. Do not add a transaction for this.
- No access-change event is emitted — this process has no subscribers. The running
  server's in-memory pairing list will not show the link (see Residual risks).

In `apps/server/src/auth/mod.rs`, change line 19:

```rust
pub(crate) use service::{AuthError, AuthService, issue_administrative_pairing_link};
```

- [x] **Step 4: Run the test to verify it passes**

Run: `cargo test -p bibcode-server --lib auth::service::tests::issues_a_persisted_administrative_pairing_a_running_service_exchanges_once -- --exact`
Expected: PASS. Also run the whole auth module to catch regressions:
`cargo test -p bibcode-server --lib auth::`
Expected: all PASS. (An "unused" warning for the new re-export is expected to surface
only until Task 2 wires the CLI; if `cargo build` warns, silence is NOT the fix — Task 2
consumes it. If Clippy in CI blocks on the temporarily unused re-export, land Task 1 and
Task 2 in sequence before running the full gate, which is the plan's order anyway.)

- [x] **Step 5: Commit**

```bash
git add apps/server/src/auth/service.rs apps/server/src/auth/mod.rs
git commit -m "feat(server): issue administrative pairing links against a data root"
```

---

### Task 2: `bibcode pairing issue` CLI subcommand

**Files:**

- Modify: `apps/server/src/config.rs` (subcommand enums ~line 241, `CliAction` ~line
  282, `ConfigError` ~line 331, `into_action`/`into_server_config` ~lines 372-466, test
  in the existing `mod tests`)
- Modify: `apps/server/src/lib.rs` (exports ~line 46, `RunError` ~line 66, `run_cli`
  ~line 84, new `run_pairing_command`)
- Test: `apps/server/tests/cli_smoke.rs`

**Interfaces:**

- Consumes: `issue_administrative_pairing_link` from Task 1;
  `persistence::{StoreRuntimeGuard, StatePaths, Database, Repositories}`;
  `select_data_root_request` + `resolve_data_root` (existing, `apps/server/src/config.rs`
  / `data_root.rs`).
- Produces:
  - CLI surface (Task 3's desktop invocation relies on it):
    `bibcode pairing issue [--base-dir <path>] [--label <text>] [--json]`, honoring the
    `BIBCODE_HOME` environment variable exactly like `storage` commands.
  - `--json` stdout contract (exactly one line, nothing else on stdout):

    ```json
    {
      "id": "<uuid>",
      "credential": "<12 chars>",
      "label": "<label if given>",
      "expiresAt": "<RFC3339>"
    }
    ```

  - `pub enum PairingCommand { Issue { root: ResolvedDataRoot, label: Option<String>, json: bool } }`
    exported from the crate root alongside `StorageCommand`.

- [x] **Step 1: Write the failing parse test**

Append inside the existing `#[cfg(test)] mod tests` in `apps/server/src/config.rs`:

```rust
    #[test]
    fn pairing_issue_resolves_the_cli_data_root_and_is_not_a_server_command() {
        let temp = tempfile::tempdir().expect("temporary base directory");
        let base_dir = temp.path().to_string_lossy().into_owned();

        let action = Cli::try_parse_from([
            "bibcode",
            "pairing",
            "issue",
            "--base-dir",
            base_dir.as_str(),
            "--label",
            "SSH bootstrap",
            "--json",
        ])
        .expect("parse pairing issue CLI")
        .into_action()
        .expect("build pairing action");
        let CliAction::Pairing(PairingCommand::Issue { root, label, json }) = action else {
            panic!("pairing issue must produce a pairing action");
        };
        assert_eq!(root.requested, PathBuf::from(base_dir.as_str()));
        assert_eq!(label.as_deref(), Some("SSH bootstrap"));
        assert!(json);

        let error = Cli::try_parse_from([
            "bibcode",
            "pairing",
            "issue",
            "--base-dir",
            base_dir.as_str(),
        ])
        .expect("parse pairing issue CLI")
        .into_server_config()
        .expect_err("pairing issue is not a server command");
        assert!(matches!(error, ConfigError::PairingCommandIsNotServer));
    }
```

(`tempfile` is already a dev-dependency of `bibcode-server`; the `storage` tests use it
from `tests/`. Add `use tempfile;` only if the compiler asks — the `mod tests` block
resolves dev-dependencies directly.)

- [x] **Step 2: Write the failing end-to-end tests**

Append to `apps/server/tests/cli_smoke.rs` (imports at the top of the file already
include `Command`, `TempDir`, `Value`, `ServerConfig`, `ServerRuntime`, and `reqwest` is
a dependency used elsewhere in the crate):

```rust
#[tokio::test]
async fn pairing_issue_prints_a_credential_the_running_server_exchanges() {
    let root = TempDir::new().expect("temporary storage root");
    let handle = ServerRuntime::start(ServerConfig::new(root.path()).with_bind("127.0.0.1", 0))
        .await
        .expect("start pairing storage owner");
    let http_base_url = format!("http://{}", handle.local_addr());

    let output = Command::new(env!("CARGO_BIN_EXE_bibcode"))
        .args(["pairing", "issue", "--base-dir"])
        .arg(root.path())
        .args(["--label", "SSH bootstrap", "--json"])
        .output()
        .expect("run pairing issue");
    assert!(
        output.status.success(),
        "pairing issue failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 pairing output");
    let line = stdout
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .expect("pairing JSON line");
    let value: Value = serde_json::from_str(line).expect("pairing JSON document");
    let credential = value["credential"].as_str().expect("credential string");
    assert!(!credential.trim().is_empty());
    assert_eq!(value["label"], "SSH bootstrap");
    assert!(value["expiresAt"].as_str().is_some());

    let exchange = reqwest::Client::new()
        .post(format!("{http_base_url}/oauth/token"))
        .form(&[
            (
                "grant_type",
                "urn:ietf:params:oauth:grant-type:token-exchange",
            ),
            ("subject_token", credential),
            (
                "subject_token_type",
                "urn:bibcode:params:oauth:token-type:environment-bootstrap",
            ),
            (
                "requested_token_type",
                "urn:ietf:params:oauth:token-type:access_token",
            ),
            ("client_label", "CLI pairing smoke"),
            ("client_device_type", "desktop"),
        ])
        .send()
        .await
        .expect("token exchange request");
    assert_eq!(exchange.status(), reqwest::StatusCode::OK);
    let token: Value = exchange.json().await.expect("token exchange JSON");
    assert!(
        token["access_token"]
            .as_str()
            .is_some_and(|token| !token.is_empty())
    );
    assert_eq!(token["token_type"], "Bearer");
    assert!(
        token["scope"]
            .as_str()
            .is_some_and(|scope| scope.contains("access:write"))
    );

    handle.shutdown();
    handle.join().await.expect("stop pairing storage owner");
}

#[test]
fn pairing_issue_fails_closed_without_a_data_store() {
    let root = TempDir::new().expect("temporary empty root");

    let output = Command::new(env!("CARGO_BIN_EXE_bibcode"))
        .args(["pairing", "issue", "--base-dir"])
        .arg(root.path())
        .arg("--json")
        .output()
        .expect("run pairing issue without a store");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no BiBCode data store"), "{stderr}");
    assert!(
        output.stdout.is_empty(),
        "no credential may be printed on failure"
    );
}
```

The end-to-end test is the load-bearing proof for this phase: the CLI process inserts
the link while a live server owns the store, and that server accepts the credential over
`POST /oauth/token`. The wire strings are hardcoded deliberately (black-box contract;
the `urn:` constants live in the private `auth::model` module).

- [x] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p bibcode-server --lib pairing_issue_resolves_the_cli_data_root_and_is_not_a_server_command`
Expected: compile error — `PairingCommand` / `CliAction::Pairing` /
`ConfigError::PairingCommandIsNotServer` not found.

Run: `cargo test -p bibcode-server --test cli_smoke pairing_issue`
Expected: both new tests FAIL — the binary exits with a clap "unrecognized subcommand
'pairing'" error, so `output.status.success()` is false in the first test (assertion
message shows the clap stderr) and the second test fails on the stderr-content
assertion.

- [x] **Step 4: Implement the CLI parsing in `apps/server/src/config.rs`**

Extend `CliCommand` (~line 241):

```rust
#[derive(Debug, Subcommand)]
enum CliCommand {
    #[command(about = "Run the BiBCode server without opening a browser.")]
    Serve,
    #[command(about = "Run the BiBCode server.")]
    Start,
    #[command(about = "Inspect or explicitly recover offline project data.")]
    Storage(StorageArgs),
    #[command(about = "Manage one-time pairing credentials for a data root.")]
    Pairing(PairingArgs),
}
```

After `StorageSubcommand` (~line 279) add:

```rust
#[derive(Debug, Args)]
struct PairingArgs {
    #[command(subcommand)]
    command: PairingSubcommand,
}

#[derive(Debug, Subcommand)]
enum PairingSubcommand {
    #[command(
        about = "Create a five-minute administrative pairing credential for this data root."
    )]
    Issue {
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        json: bool,
    },
}
```

Extend `CliAction` (~line 282) and add the public command enum next to
`StorageCommand`:

```rust
#[derive(Clone, Debug)]
pub enum CliAction {
    Run(Box<ServerConfig>),
    Storage(StorageCommand),
    Pairing(PairingCommand),
}

#[derive(Clone, Debug)]
pub enum PairingCommand {
    Issue {
        root: ResolvedDataRoot,
        label: Option<String>,
        json: bool,
    },
}
```

Extend `ConfigError` (~line 343, next to `StorageCommandIsNotServer`):

```rust
    #[error("pairing commands cannot be converted into a server configuration")]
    PairingCommandIsNotServer,
```

In `Cli::into_server_config` (~line 373) handle the new action:

```rust
    pub fn into_server_config(self) -> Result<ServerConfig, ConfigError> {
        match self.into_action()? {
            CliAction::Run(config) => Ok(*config),
            CliAction::Storage(_) => Err(ConfigError::StorageCommandIsNotServer),
            CliAction::Pairing(_) => Err(ConfigError::PairingCommandIsNotServer),
        }
    }
```

In `Cli::into_action` (~line 385), add a `Pairing` arm directly after the
`Some(CliCommand::Storage(storage))` arm, using the same data-root selection:

```rust
            Some(CliCommand::Pairing(pairing)) => {
                let home_dir = dirs::home_dir().ok_or(DataRootError::HomeDirectoryUnavailable)?;
                let request = select_data_root_request(
                    args.base_dir,
                    bibcode_env_var("BIBCODE_HOME"),
                    None,
                    home_dir,
                );
                let root = crate::data_root::resolve_data_root(request)?;
                let PairingSubcommand::Issue { label, json } = pairing.command;
                return Ok(CliAction::Pairing(PairingCommand::Issue { root, label, json }));
            }
```

- [x] **Step 5: Implement the runner in `apps/server/src/lib.rs`**

Update the config re-export (~line 46):

```rust
pub use config::{
    Cli, CliAction, ConfigError, PairingCommand, ServerConfig, ServerMode, StorageCommand,
};
```

Add `use serde::Serialize;` next to the existing `use serde_json::json;` import.

Extend `RunError` (~line 66):

```rust
    #[error("could not issue a pairing credential: {0}")]
    PairingIssue(String),
    #[error("failed to encode pairing command output")]
    PairingOutput(#[source] serde_json::Error),
```

Dispatch in `run_cli` (~line 84):

```rust
pub async fn run_cli() -> Result<(), RunError> {
    match Cli::try_parse()?.into_action()? {
        CliAction::Run(config) => run_server(*config).await,
        CliAction::Storage(command) => run_storage_command(command).await,
        CliAction::Pairing(command) => run_pairing_command(command).await,
    }
}
```

Add the runner after `run_storage_command`:

```rust
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PairingIssueOutput {
    id: String,
    credential: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    expires_at: String,
}

/// Issues a one-time administrative pairing credential against a data root.
///
/// Coexists with a running server on the same root: it takes the shared store
/// runtime lock (blocking only offline recovery) and writes through the WAL
/// database, and the server consumes pairing links from the database. Prints
/// exactly one JSON line to stdout in `--json` mode — the desktop SSH
/// launcher parses the last non-empty stdout line — and never initializes
/// logging or other stdout writers.
async fn run_pairing_command(command: PairingCommand) -> Result<(), RunError> {
    let PairingCommand::Issue { root, label, json } = command;
    let _runtime_guard = persistence::StoreRuntimeGuard::acquire(&root.effective)
        .await
        .map_err(|error| RunError::PairingIssue(error.to_string()))?;
    let paths = persistence::StatePaths::from_config(&ServerConfig::new(&root.effective));
    if !paths.database.exists() {
        return Err(RunError::PairingIssue(format!(
            "no BiBCode data store at {}; start the server on this data root first",
            paths.database.display()
        )));
    }
    let database = persistence::Database::open_existing(&paths.database)
        .await
        .map_err(|error| RunError::PairingIssue(error.to_string()))?;
    let repositories = persistence::Repositories::new(database.clone());
    let issued = auth::issue_administrative_pairing_link(&repositories, label)
        .await
        .map_err(|error| RunError::PairingIssue(format!("{error:?}")));
    database.close().await;
    let issued = issued?;
    let output = PairingIssueOutput {
        id: issued.id,
        credential: issued.credential,
        label: issued.label,
        expires_at: issued.expires_at,
    };
    if json {
        println!(
            "{}",
            serde_json::to_string(&output).map_err(RunError::PairingOutput)?
        );
    } else {
        println!("Pairing credential: {}", output.credential);
        println!("Expires at: {}", output.expires_at);
    }
    Ok(())
}
```

Implementation notes:

- `database.close()` is `pub(crate)`; calling it from `lib.rs` (same crate) is fine and
  guarantees the CLI's WAL writes are flushed and its connection is gone before exit.
  The error from issuance is intentionally held (`let issued = ...;` then `close`, then
  `?`) so the database always closes.
- The store is **never created** here. A missing `state.sqlite` means the server has
  never run on that root — fail closed with the actionable message asserted by the
  smoke test. Do not call `prepare_store` (it creates/repairs stores and takes
  startup-grade locks).
- No migrations run here. `auth_pairing_links` has existed since migration 20 and the
  insert touches only stable columns.

- [x] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p bibcode-server --lib pairing_issue_resolves_the_cli_data_root_and_is_not_a_server_command`
Expected: PASS.
Run: `cargo test -p bibcode-server --test cli_smoke`
Expected: all PASS, including both new `pairing_issue_*` tests and every pre-existing
test (the `--help` and flag-position tests must not regress).

- [x] **Step 7: Commit**

```bash
git add apps/server/src/config.rs apps/server/src/lib.rs apps/server/tests/cli_smoke.rs
git commit -m "feat(cli): add bibcode pairing issue subcommand"
```

---

### Task 3: Point the desktop SSH launcher at the new command

**Files:**

- Modify: `apps/desktop/src-tauri/src/ssh.rs` (new `pub const` near the other constants
  ~line 38; use it in `issue_remote_pairing_token` ~line 1377)
- Test: `apps/desktop/src-tauri/tests/ssh_public_contract.rs`

**Interfaces:**

- Consumes: the CLI surface from Task 2 (`bibcode pairing issue --base-dir <path>
  --json`) and its stdout contract (Pinned fact 1).
- Produces: `pub const REMOTE_PAIRING_ISSUE_COMMAND: &str` on the public `ssh` module
  (`bibcode_desktop_lib::ssh`), asserted by the public contract test. No behavior change
  to `SshEnvironmentManager`'s public API.

- [x] **Step 1: Write the failing contract test**

In `apps/desktop/src-tauri/tests/ssh_public_contract.rs`, add
`REMOTE_PAIRING_ISSUE_COMMAND` to the existing `use bibcode_desktop_lib::ssh::{...}`
import list, and append this test:

```rust
#[test]
fn public_remote_pairing_command_targets_the_native_cli_and_parses_its_output() {
    assert_eq!(
        REMOTE_PAIRING_ISSUE_COMMAND,
        "bibcode pairing issue --base-dir \"$HOME/.bibcode\" --json"
    );

    assert_eq!(
        parse_remote_pairing_credential(
            "Warning: Permanently added 'devbox' to the list of known hosts.\n\
             {\"id\":\"3f3c0f6e-8a0e-4f61-9d55-1af26cf54e21\",\
             \"credential\":\"WXYZ23456789\",\"label\":\"SSH bootstrap\",\
             \"expiresAt\":\"2026-08-27T12:05:00Z\"}\n",
        ),
        Ok("WXYZ23456789".to_string())
    );
}
```

The first assertion pins the exact remote invocation to the subcommand the native CLI
now exposes (and, by construction, retires the removed `auth pairing create` form). The
second pins that the parser accepts the real `--json` output of Task 2 — same key
casing, extra fields tolerated, last non-empty line wins over SSH banners.

- [x] **Step 2: Run the test to verify it fails**

Run: `cargo test -p bibcode-desktop --test ssh_public_contract public_remote_pairing_command_targets_the_native_cli_and_parses_its_output`
Expected: compile error — `REMOTE_PAIRING_ISSUE_COMMAND` is not exported by
`bibcode_desktop_lib::ssh`.

- [x] **Step 3: Implement the constant and switch the invocation**

In `apps/desktop/src-tauri/src/ssh.rs`, next to the other remote constants (after
`REMOTE_REUSE_READY_TIMEOUT_MS`, ~line 38):

```rust
/// Remote command that mints the one-time SSH bootstrap pairing credential.
/// Must target the same `--base-dir` the launch script passes to `serve`
/// (`SERVER_HOME`), and must print a JSON line with a `credential` field —
/// see `parse_remote_pairing_credential`.
pub const REMOTE_PAIRING_ISSUE_COMMAND: &str =
    r#"bibcode pairing issue --base-dir "$HOME/.bibcode" --json"#;
```

In `issue_remote_pairing_token` (~line 1374), replace the removed command string:

```rust
    args.extend([
        "sh".to_string(),
        "-lc".to_string(),
        REMOTE_PAIRING_ISSUE_COMMAND.to_string(),
    ]);
```

(The rest of `issue_remote_pairing_token` — spawn, status check, stderr surfacing,
`parse_remote_pairing_credential` — is already correct and stays untouched.)

- [x] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p bibcode-desktop --test ssh_public_contract`
Expected: all PASS (the new test plus every pre-existing contract test).
Run: `cargo test -p bibcode-desktop --lib ssh::`
Expected: all PASS (inline `ssh.rs` tests, including the parser and launch-script
tests, are unaffected).

- [x] **Step 5: Commit**

```bash
git add apps/desktop/src-tauri/src/ssh.rs apps/desktop/src-tauri/tests/ssh_public_contract.rs
git commit -m "fix(desktop): mint SSH bootstrap pairing via bibcode pairing issue"
```

---

### Task 4: Living documentation and runbook review

**Files:**

- Modify: `docs/architecture/remote.md` (limitation entry ~lines 153-164; the
  "Desktop-managed SSH" section ~lines 86-104)
- Review only (no expected change): `docs/testing/README.md`,
  `docs/testing/cross-platform-validation.md`, `docs/testing/windows-desktop.md`,
  `docs/testing/linux-desktop.md`, `docs/testing/macos-desktop.md`,
  `docs/testing/execution-report-template.md`

**Interfaces:**

- Consumes: the shipped behavior of Tasks 1-3 (documentation must describe exactly what
  landed, per AGENTS.md same-patch rule).
- Produces: `docs/architecture/remote.md` with the SSH-pairing limitation removed and
  the repaired bootstrap described; an explicit runbook statement in the phase report.

- [x] **Step 1: Remove the resolved limitation**

In `docs/architecture/remote.md`, under `## Current limitations`, delete exactly this
bullet (the surrounding bullets stay):

```markdown
- The desktop SSH launcher and forwarding implementation exist, but fresh SSH
  setup is currently blocked: its pairing step invokes the removed
  `bibcode auth pairing create` command while the native CLI exposes only
  `start` and `serve`.
```

- [x] **Step 2: Document the repaired bootstrap**

In the `### Desktop-managed SSH` section of `docs/architecture/remote.md`, extend the
first paragraph. Replace:

```markdown
The Tauri host owns SSH, not the server or React app. It validates the SSH
profile, probes or launches `bibcode` remotely, establishes local forwarding,
and returns a local HTTP/WSS bootstrap plus bearer credential to the connection
runtime. The resulting `SshConnectionTarget` enters the same authorization and
RPC pipeline as other targets.
```

with:

```markdown
The Tauri host owns SSH, not the server or React app. It validates the SSH
profile, probes or launches `bibcode` remotely, establishes local forwarding,
and returns a local HTTP/WSS bootstrap plus bearer credential to the connection
runtime. The resulting `SshConnectionTarget` enters the same authorization and
RPC pipeline as other targets.

Fresh setup mints its bootstrap credential by running
`bibcode pairing issue --base-dir "$HOME/.bibcode" --json` on the remote host
(the same data root the launched `serve` uses). The command writes a one-time
administrative pairing link into that root's auth store and prints one JSON
line whose `credential` field the desktop exchanges at `/oauth/token`. Because
the server consumes pairing links from the database and the store runtime lock
is shared, the command works beside the already-running remote server without
a restart.
```

- [x] **Step 3: Review the testing runbooks**

Per AGENTS.md, this phase must review `docs/testing/` because it changes a desktop
bootstrap procedure and adds test targets. Verify there is nothing to update:

Run: `rg -n -i "pairing|cli_smoke|ssh_public_contract" docs/testing/`
Expected: only the existing packaged-UI lines stating that connections/SSH/pairing UI
is absent from ordinary desktop presentation (windows/linux/macos runbooks). Those
statements remain true — this phase changes an internal SSH bootstrap command, not
packaged UI flows, provider visibility, worktree/process lifecycle, or any documented
test command (the new tests live inside already-referenced suites: the server crate's
tests and the desktop contract tests). If that expectation holds, make no runbook edit
and record in the phase report: **"docs/testing/ runbooks reviewed and remain
accurate."** If the search surfaces a runbook line that names the removed
`auth pairing create` command or otherwise contradicts the new behavior, update that
line in this same commit instead.

- [x] **Step 4: Commit**

```bash
git add docs/architecture/remote.md
git commit -m "docs(remote): record repaired SSH pairing bootstrap"
```

(Include any runbook file in the `git add` only if Step 3 actually required an edit.)

---

### Task 5: Phase validation gate

**Files:** none (verification only).

**Interfaces:** none — this task executes the master plan's validation gate for the
phase and produces the evidence for the phase report.

- [x] **Step 1: Formatting and lints**

Run, in order, from the repository root:

```bash
cargo fmt --all --check
cargo clippy -p bibcode-server --all-targets -- -D warnings
cargo clippy -p bibcode-desktop --all-targets -- -D warnings
```

Expected: all clean. Fix any finding and amend the owning task's commit before
proceeding.

- [x] **Step 2: Focused and affected-crate tests**

```bash
cargo test -p bibcode-server --lib auth::
cargo test -p bibcode-server --lib config
cargo test -p bibcode-server --test cli_smoke
cargo test -p bibcode-desktop --lib ssh::
cargo test -p bibcode-desktop --test ssh_public_contract
```

Expected: all PASS. Then the broader crates (the change crosses the server/desktop
boundary):

```bash
cargo test -p bibcode-server
cargo test -p bibcode-desktop
```

Expected: all PASS.

- [x] **Step 3: Workspace checks**

```bash
vp check
vp run typecheck
```

Expected: clean. No TypeScript was touched in this phase, so `vp run typecheck` is a
regression guard only; `vp test` runs are not required beyond it for this phase.

- [x] **Step 4: Diff hygiene**

```bash
git status --short
git diff main...HEAD --stat
```

Expected: only the files named in Tasks 1-4 changed; no `.codegraph/` data, no
generated files, no dependency drift (`Cargo.toml`/`Cargo.lock` untouched — this phase
adds no dependencies), and the pre-existing pending deletions under
`docs/plans/2026-08-24-environment-project-management/` (if present in the worktree)
remain exactly as the user left them.

- [x] **Step 5: Report**

Write the phase report with: every command from Steps 1-4 and its outcome, any command
that could not run (and why), the runbook statement from Task 4 Step 3, and the
residual risks below.

---

## Residual risks (report these; do not "fix" them in this phase)

1. **Running server's in-memory pairing list.** A link minted by the CLI is honored for
   exchange (database read) but does not appear in the running server's in-memory
   pairing list (`list_pairings`) or its access-change broadcast until restart. The SSH
   flow consumes the link within seconds, so the stale listing is transient and benign.
2. **Capacity-check race.** The CLI's `MAX_ACTIVE_PAIRINGS` guard is read-then-insert
   without a shared transaction with the server. Two concurrent issuers could
   transiently exceed the soft cap by a handful of rows; the cap is a DoS guard, not an
   invariant.
3. **Binary/store schema skew.** If a newer `bibcode` CLI runs against a root owned by
   an older still-running server, the insert touches only columns that have existed
   since migration 20. Skew in the other direction (older CLI, newer store) is equally
   inert for this table. The SSH flow runs the same `bibcode` binary from `PATH` for
   both `serve` and `pairing issue`, so real skew requires an operator mixing binaries.
4. **Human-format output is not machine-parseable.** Only `--json` output is a
   contract; the desktop always passes `--json`. The human format exists for operators
   and may change freely.

## Self-review checklist (run after writing code, before the gate)

- Spec §4.7 coverage: CLI named `bibcode pairing issue` ✓ (Task 2); creates a one-time
  pairing link against a given data root ✓ (Tasks 1-2); prints the credential JSON in
  the exact shape `parse_remote_pairing_credential` expects ✓ (Pinned fact 1, Task 2
  Step 5, Task 3 test); `ssh.rs` updated to invoke it ✓ (Task 3); limitation entry
  removed from `docs/architecture/remote.md` in the same change ✓ (Task 4).
- Type consistency: `issue_administrative_pairing_link(&Repositories, Option<String>)
  -> Result<PairingCredentialResult, AuthError>` is defined in Task 1 and consumed with
  that exact signature in Task 2; `PairingCommand::Issue { root, label, json }` is
  defined and consumed identically in Task 2's config and lib code and tests;
  `REMOTE_PAIRING_ISSUE_COMMAND` is defined in Task 3 Step 3 and asserted verbatim in
  Task 3 Step 1.
- Naming: no reference-product strings anywhere in this phase's code, tests, or docs.
