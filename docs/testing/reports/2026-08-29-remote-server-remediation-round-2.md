# Platform Validation Execution Report

**Result:** PASS WITH RESIDUAL RISKS

## Tested revision

- Repository: Coddy Studio (`https://github.com/mubeda/BibCode.git`)
- Remote: `origin`; requested publication branch `develop`
- Branch or requested revision: local `mubeda/develop`; remediation range
  `0e4767b5e8678a756377bb7f0071f98049e88dc0..65af2c1084e3c8f87eb0269cdac18ba3f056de3c`
- Executable HEAD: `d0acdfb841112b350b438eaa2b1cd34a21b6ae97`
  supplied the server, desktop, web, contracts, and client-runtime source used
  for final validation. Test-only HEAD
  `9a211c90e084762c626f468bde375a3efecafe75` removes an ambiguous session
  selector exposed by the broad server run. Documentation HEAD
  `65af2c1084e3c8f87eb0269cdac18ba3f056de3c` records the final auditor-verified
  authority rule. The evidence-only report commit follows it.
- Remote HEAD before publication: `origin/develop` at
  `0e4767b5e8678a756377bb7f0071f98049e88dc0`
- Merge base and ahead/behind before the report commit: merge base `0e4767b5`;
  remote versus local `0 18`
- Dirty state before execution: two pre-existing staged environment-project
  document deletions and two protected untracked adversarial-review documents,
  plus intended work before each scoped commit
- Dirty state after execution: only the same four protected user-owned paths and
  this report; no implementation, generated, process, or container resource
  remained uncommitted

## Native environment

- Operating system and release/build: Fedora Linux 44 Workstation
- Architecture: `x86_64` / `linux/amd64`
- Kernel: `Linux 7.1.10-200.fc44.x86_64`
- Rust/Cargo: `rustc 1.97.1 (8bab26f4f 2026-07-14)`;
  `cargo 1.97.1 (c980f4866 2026-06-30)`
- Node/package manager/Vite+: Node 26.5.0; pnpm 11.15.0 through Vite+; global
  Vite+ 0.3.0, repository Vite+ 0.2.5, Vitest 4.1.10
- Native compiler/SDK/runtime dependencies: installed Linux development
  dependencies supported the complete server and desktop test, build, and
  Clippy matrices
- Optional capabilities: Docker CLI 29.7.2 used context `pathfinder-podman` and
  Podman Engine 5.8.4 with `crun` 1.28 on Linux/amd64. Windows/WSL, macOS,
  signing, notarization, and packaged installers were unavailable.

## Requested inputs and ancestry

- Expected product version: 0.4.1
- Observed version sources: server and desktop Cargo manifests
- Required commits: the 18 linear commits in the tested range, beginning with
  `52f0bfb3` and ending with `65af2c10`
- Ancestry result: every listed remediation commit is an ancestor of the tested
  HEAD, and the range is linear above the pre-execution `origin/develop`
- Inputs that were unavailable: supported-platform packaged desktop builds, a
  second physical LAN device, Windows firewall/WSL evidence, macOS bundle
  evidence, signing/notarization, and authenticated provider credentials

## Focused validation

| Command or focused group                                                                                                    | Result/exit code   | Evidence and warnings                                                                                                   |
| --------------------------------------------------------------------------------------------------------------------------- | ------------------ | ----------------------------------------------------------------------------------------------------------------------- |
| Migration 49 and migration inventory (`cargo test -p bibcode-server persistence::migrations::tests:: --lib -- --nocapture`) | PASS / 0           | 24 tests passed.                                                                                                        |
| Pairing repository, RPC contract/parity, E2EE, and real-binary interop focused suites                                       | PASS / 0           | Repository 24/24, E2EE integration 23/23, RPC wire 13/13, and current/prior real-binary interop 3/3 each passed.        |
| Six final TypeScript behavior files                                                                                         | PASS / 0           | 118/118 passed across pairing add, Noise socket/session, share offer/tab, and exposure reconciliation.                  |
| Auth, session admission, firewall, endpoint, update, database recovery, WSL, and transport lifecycle suites                 | PASS / 0           | Auth 36/36, session 15/15, desktop bridge 50/50, and firewall 10/10 passed, including cancellation and ownership seams. |
| `vp test scripts/remote-architecture-contract.test.ts`                                                                      | PASS / 0           | 1/1 living-documentation contract passed.                                                                               |
| `vp fmt --check` on the eight changed living architecture/runbook documents                                                 | PASS / 0           | All eight documents used repository formatting.                                                                         |
| Exact OpenCode reaper watchdog case after a parallel-suite timeout                                                          | PASS / 0           | 1/1 passed in 0.11 s; Git history places the test before this range.                                                    |
| Exact live-checkpoint server-start case                                                                                     | INTERMITTENT / 101 | Pass/fail/pass in isolation; blame places the stress test at `bfbecf595` from 2026-07-31.                               |

## Workspace and static gates

| Command                                                                                                                                              | Result/exit code | Test totals or warning summary                                                                                              |
| ---------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------- | --------------------------------------------------------------------------------------------------------------------------- |
| `vp run check:contracts`                                                                                                                             | PASS / 0         | 4 fixture-exporter, 5 Rust-parity, and 13 Rust RPC wire tests passed.                                                       |
| `vp run check:dependency-ledger`                                                                                                                     | PASS / 0         | 81 direct JavaScript dependencies, 82 registry Rust crates, 1 local Rust crate, 9 actions, 9 toolchain pins, 0 unaccounted. |
| `vp check . '!docs/plans/remote-servers/2026-08-29-adversarial-review.md' '!docs/plans/remote-servers/2026-08-29-remediation-adversarial-review.md'` | PASS / 0         | 2,002 files formatted and 1,412 files linted with no warning/error; protected untracked review documents were excluded.     |
| `vp run typecheck`                                                                                                                                   | PASS / 0         | 11/11 targets passed; existing non-failing Effect finite-number suggestions were emitted.                                   |
| `vp test`                                                                                                                                            | PASS / 0         | 615 files and 8,630 tests passed; 2 files and 29 tests skipped.                                                             |
| `cargo fmt --all --check`                                                                                                                            | PASS / 0         | No Rust formatting drift.                                                                                                   |
| `cargo test -p bibcode-server --no-fail-fast -- --test-threads=1`                                                                                    | FAIL / 101       | 1,734 active library tests passed, 2 performance tests were ignored; two integration assertions failed as detailed below.   |
| `cargo test -p bibcode-server --test e2ee_ws -- --test-threads=1`                                                                                    | PASS / 0         | All 23 E2EE integration tests passed after the selector correction.                                                         |
| `cargo test -p bibcode-desktop --no-fail-fast`                                                                                                       | PASS / 0         | 338 library plus 8 integration tests passed.                                                                                |
| Forced affected-target Clippy recompilation                                                                                                          | PASS / 0         | Touched each crate root before linting so both changed crates were rechecked without deleting shared build artifacts.       |
| `cargo clippy -p bibcode-server --all-targets -- -D warnings`                                                                                        | PASS / 0         | Forced affected-target recheck; no warnings.                                                                                |
| `cargo clippy -p bibcode-desktop --all-targets -- -D warnings`                                                                                       | PASS / 0         | Forced affected-target recheck; no warnings.                                                                                |
| `cargo build -p bibcode-server`                                                                                                                      | PASS / 0         | Fresh current Linux/amd64 debug server built for direct and container interop.                                              |
| `BIBCODE_E2EE_SERVER_BIN="$PWD/target/debug/bibcode" vp test packages/client-runtime/src/e2ee/serverInterop.test.ts`                                 | PASS / 0         | 3/3 tests passed against the real current server binary.                                                                    |
| Cross-container block from `docs/testing/cross-platform-validation.md`                                                                               | PASS / 0         | Final opt-in smoke passed in 5.81 s; 6.24 s total; cleanup assertions passed; no credential was printed or retained.        |
| `git diff --check`                                                                                                                                   | PASS / 0         | No whitespace errors.                                                                                                       |

An independent SOL/xhigh adversarial audit of the final executable and
documentation range found no Critical or Important issue. It verified
absent-flag rolling compatibility against the exact `7d44829a` server,
confirmation ownership across handler cancellation and ambiguous responses,
and fail-closed native public/custom topology behavior. Its one documentation
inconsistency was corrected by `65af2c10` and re-reviewed.

The broad server command was not misreported as green. It exposed two distinct
issues:

- `delivered_pairing_session_stays_pending_until_confirm_rpc` selected the
  lexicographically latest session when the administrator and negotiated
  pairing shared one millisecond timestamp. It could therefore inspect the
  active administrator instead of the pending off-host session. Commit
  `9a211c90` selects the grant by `reach = another-device` and `off_host = 1`;
  the exact regression and complete 23-test E2EE target then passed.
- `server_starts_while_live_store_is_continuously_committed_and_checkpointed`
  intermittently returned `AuthInitialize(Internal("SQLite operation failed"))`
  under its injected concurrent writer/checkpoint. It failed in the broad run,
  then produced pass/fail/pass in three isolated executions. The test and
  exercised startup/checkpoint behavior predate this remediation range, and no
  changed authentication assertion failed around it.

## Cross-container remote-server gate

The current runbook block created a distinct Debian server container and
Node client container on a test-owned network, mounted the current binary and
checkout read-only, used a dedicated state volume, and gave only Vite's cache
path a writable tmpfs.

The first behavior run passed but revealed that Docker-compatible Podman treats
a multi-name `docker rm -f` atomically when one auto-removed name is absent. The
helper therefore left the server, network, and volume behind. Those exact
test-owned resources were removed individually, commit `21fbb198` split the two
container removals, and the complete gate was rerun. The final run passed both
behavior and empty-resource assertions.

- Runtime: Docker CLI 29.7.2 with API 1.44 against Podman Engine 5.8.4,
  `crun` 1.28, Fedora 44, Linux/amd64
- Server image: `debian:trixie-slim`,
  `sha256:e426a54f50cc4cf82dd5cab8ba8426ed02c391840cb5a62dfd987542dbabea3b`,
  Linux/amd64
- Client image: `node:26-bookworm`,
  `sha256:55d3eaf406db9ece10a4d68c6780e41020683453e7a8654d68370a2abddf41ed`,
  Linux/amd64
- Covered: descriptor and administrative exchange; off-host pairing; pinned
  Noise NK; pending share state; bootstrap `server.getConfig`; rejected bearer
  reconnect before confirmation; idempotent `auth.confirmPairing`; active bearer
  reconnect; maximum record plus delayed continuation; 16 MiB-plus-one plain
  frame rejection; updater check/manual failure/later health; same-peer pre-auth
  cap; 2,049-record rejection; browser exposure retention; active E2EE
  revocation; cancellation replay protection; and final loopback convergence
- Evidence partition: the fixed-IP client cannot independently exercise `/24`
  aggregation, which passed Rust classifier/admission tests. The headless
  container cannot inject a hung desktop updater delegate or supervisor
  acquisition; 30-second whole-operation timeout and slot release passed
  client-runtime tests.
- Cleanup: an independent post-run assertion produced
  `{"containers":"","network":"","volume":""}`. No scoped host process
  using the current binary and port 3773 remained.

## VCS observation evidence

- Execution host and route: unavailable for this remote-server remediation
- Physical repositories/worktrees/active full subscribers/passive subscribers:
  not measured
- Watcher health and fallback state: not measured
- Automatic-fetch interval and passive-summary interval: not in scope

## Native package artifacts and packaged UI

| Artifact or scenario              | Path/evidence                           | State                                                                           |
| --------------------------------- | --------------------------------------- | ------------------------------------------------------------------------------- |
| Native debug server               | `target/debug/bibcode` during execution | Built from the tested SHA and exercised by direct and Docker interop.           |
| Native installer/package          | Unavailable                             | No package, signing, or release claim.                                          |
| Packaged remote sharing/update UI | Unavailable                             | Source, component, integration, and runbook evidence only; no screenshot claim. |

No installed Coddy Studio copy was launched.

## Process and temporary-root cleanup

- Scoped surviving processes: none from direct interop or Docker smoke
- New test-owned roots: harness temporary roots were released; the Docker state
  volume was explicitly removed
- Package mounts or platform resources released: server/client containers,
  test-owned network, and test-owned volume were removed
- Pre-existing roots/processes intentionally left untouched: unrelated host,
  CodeGraph, workspace, and container-image state

## Non-native compatibility evidence

### Windows

- Evidence class: compatibility evidence plus unavailable native evidence
- Source/contracts reviewed: Windows desktop runbook, desktop exposure,
  firewall, WSL topology, endpoint enumeration, Tailscale, IndexedDB recovery,
  update, and shared transport contracts
- Native-only evidence still required: packaged x64 UI, WSL transitions, live
  firewall policy/timeout and late-cleanup behavior, WebView2, Authenticode, and
  updater validation

### macOS

- Evidence class: compatibility evidence plus unavailable native evidence
- Source/contracts reviewed: macOS desktop runbook, endpoint/Tailscale,
  deep-link, IndexedDB, exposure, update, and shared transport contracts
- Native-only evidence still required: arm64/x64 package UI, bundle custom
  scheme, signing/notarization, and updater validation

### Linux

- Evidence class: native source/build/test and container integration evidence;
  packaged desktop visual evidence unavailable
- Native-only evidence still required: packaged WebKitGTK UI, deep-link desktop
  integration, and manual LAN pairing on a second physical device

The Linux, macOS, Windows, and cross-platform runbooks were reviewed and
updated. `docs/architecture/connection-runtime.md` is the current owner for the
connection-runtime procedures; the planned `docs/testing/connection-runtime.md`
path has never existed.

## Source changes and commits created

- `52f0bfb3` — design second adversarial remediation
- `6148982a` — plan second adversarial remediation
- `b890a372` — bound encrypted byte admission progress
- `100a19b3` — harden WebSocket admission and cleanup
- `867d257b` — preserve remote connection intent
- `feead7db` — make connection database reset explicit
- `df1f7950` — normalize pairing trust inputs
- `44d6c548` — converge exposure and firewall state
- `e5e1a9aa` — fail closed on advertised endpoints
- `334fb2b5` — confirm durable pairing delivery
- `bd25c066` — extend the remediation Docker smoke
- `eb6df4fc` — align remediation regression gates
- `7d44829a` — align remediation architecture and runbooks
- `e7ab93ba` — close final remediation gaps
- `21fbb198` — make remote Docker cleanup portable
- `d0acdfb8` — close pairing ownership and topology gaps
- `9a211c90` — select the negotiated pairing session in E2EE validation
- `65af2c10` — align ambiguous confirmation ownership documentation

The range changes 88 files with 8,696 insertions and 1,292 deletions. Every
commit used explicit path-scoped publication so the four protected user-owned
paths were never included.

## Commands not run

| Command or scenario                                                              | Reason                                                                                                                                                                              | Required follow-up owner                                                             |
| -------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------ |
| Raw `vp check` including the two untracked adversarial-review documents          | Both files are protected user-owned historical review input and are not formatter-clean; changing or committing them was outside scope.                                             | Owner of those review documents, if later committed.                                 |
| A wholly green `cargo test -p bibcode-server --no-fail-fast -- --test-threads=1` | The run passed the changed library/auth surface but exposed the corrected E2EE selector and the separately reproduced pre-existing SQLite checkpoint/startup flake described above. | Server persistence owner should make the live-checkpoint startup seam deterministic. |
| Packaged desktop UI on Linux, Windows, and macOS                                 | No package/signing environment or supported-platform native hosts were available.                                                                                                   | Release validation on each supported platform.                                       |
| Physical second-device LAN pairing                                               | The requested repeatable integration boundary was exercised in distinct containers.                                                                                                 | Native/manual platform validation.                                                   |

## Residual risks

- Automatic native desktop sharing deliberately accepts only a private IPv4 or
  CGNAT endpoint. Public-only hosts must use an externally managed listener or
  reverse proxy; native public-address automation remains fail-closed.
- A server may commit `auth.confirmPairing` while its response is lost. The
  server now owns database-to-cache-to-latch activation after handler
  cancellation, and the client retains the durable credential whenever remote
  activation is possible. A bounded pinned bearer proof runs immediately and
  the saved supervisor owns later recovery; the UI does not yet distinguish
  immediate proof from retained recovery as separate success variants.
- A truly old protocol-v1 server activates its credential before the client can
  durably register it. Failure of that first local registration write can still
  leave a legacy server grant requiring manual revocation; eliminating that
  compatibility limit requires a legacy-compatible self-revoke protocol.
- A native-managed server that is already wide on a public-only topology safely
  exposes no selectable address and disables offer generation, but the UI's
  custom-listener/reverse-proxy guidance currently renders only while the
  server is local-only.
- Admission caps are process-local. They bound one server process and partition
  peer/subnet/principal pressure, but do not form a distributed cap across
  multiple simultaneously live processes sharing one store.
- Docker cannot independently prove the subnet classifier with one fixed client
  IP or inject the desktop updater/supervisor timeout seams. Focused Rust and
  client-runtime tests provide that evidence instead.
- Native firewall, WSL, packaged updater/deep-link, and visual flows remain
  unverified on Windows and macOS. Unit, contract, and Linux evidence bounds but
  does not replace native package validation.
- The pre-existing live-checkpoint server-start stress test remains
  intermittently red even in isolation. Its two passing and two failing
  executions in this validation bound the failure honestly but do not prove a
  production fix.

## Publication state

- Commits created: the 17 listed remediation commits; this report is part of a
  pending final evidence commit
- Pushed: no at report creation; publication to remote `develop` follows final
  review as explicitly requested
- Branch merged: no
- Pull request opened: no
- Artifacts published: no
