# Platform Validation Execution Report

**Result:** PASS WITH RESIDUAL RISKS

## Tested revision

- Repository: Coddy Studio (`https://github.com/mubeda/BibCode.git`)
- Remote: `origin`; remote branch `develop`
- Branch or requested revision: local `mubeda/develop`; remote-server range
  `3b1864eff4b5e476bf725910fc84c11f34b31c1a..71cd7fc3f0e19e04ea8cccb11e6092146496e42e`
- Local HEAD: `71cd7fc3f0e19e04ea8cccb11e6092146496e42e` was the final tested product/test
  commit. The later documentation-only commit containing the final report and
  checklist does not change executable behavior.
- Remote HEAD: `origin/develop` and `git ls-remote origin refs/heads/develop`
  both reported `3b1864eff4b5e476bf725910fc84c11f34b31c1a`.
- Merge base and ahead/behind: merge base `3b1864eff4b5e476bf725910fc84c11f34b31c1a`;
  remote/local counts `0 140` at the tested product commit.
- Dirty state before final-review remediation: clean at `98189128`; the two
  environment-project plan/spec files were present and clean while product
  commits and validation ran.
- Dirty state after executable validation: clean at `71cd7fc3`; this report and
  final Task 14 checklist bookkeeping were then the only intended changes. The
  controller reapplies the two original user-owned staged deletions only after
  the last repository commit.

## Native environment

- Operating system and release/build: Fedora Linux 44 Workstation. This is a
  native source/test host but not one of the documented Ubuntu 22.04/24.04 or
  Debian 12 release-validation distributions, so package conclusions are not
  inferred from it.
- Architecture: `x86_64` / `linux/amd64`
- Kernel: `Linux 7.1.10-200.fc44.x86_64`
- Desktop environment/display protocol, when applicable: GNOME, Wayland,
  `WAYLAND_DISPLAY=wayland-0`, `DISPLAY=:0`
- Rust/Cargo: `rustc 1.97.1 (8bab26f4f 2026-07-14)`, LLVM 22.1.6;
  `cargo 1.97.1 (c980f4866 2026-06-30)`
- Node/package manager/Vite+: Node 26.5.0; pnpm 11.15.0 through Vite+; Vite+
  launcher 0.3.0 and repository Vite+ 0.2.5; Vitest 4.1.10
- Native compiler/SDK/runtime dependencies: GCC 16.2.1; the installed Linux
  desktop development dependencies were sufficient for complete server and
  desktop Rust tests and Clippy. No native package build was requested or run.
- Optional capabilities: Docker CLI 29.7.2 used context `pathfinder-podman`;
  its server was Podman 5.8.4 on Fedora 44, Linux/amd64. Windows/WSL/MSVC,
  macOS/Xcode, signing, notarization, and native installers were unavailable.

## Requested inputs and ancestry

- Expected product version: 0.4.1
- Observed version sources: `apps/server/Cargo.toml` and
  `apps/desktop/src-tauri/Cargo.toml`; the built `target/debug/bibcode` was an
  x86-64 ELF from this checkout.
- Required commits: `3b1864eff`, hidden relay delta `81eff018`, relay retry
  `e932cd3f`, Task 11 `465c55e1..ad1b44bc`, Task 12
  `48e80ba1..a5be7f8d`, Task 13 `996e3d6b..ec626fb6`, documentation tip
  `4c1cf683`, validation fixture repair `301ea75c`, final-review CORS repair
  `d51fdba5`, and verified Windows firewall cleanup `71cd7fc3`.
- Ancestry result for each commit: `git merge-base --is-ancestor <commit> HEAD`
  exited 0 for every listed commit at the tested product commit.
- Inputs that were unavailable: supported-distribution native packages,
  packaged UI automation and screenshots, a second physical LAN device,
  Windows/WSL/firewall/Authenticode evidence, macOS bundle/signing/notarization
  evidence, and authenticated provider credentials.

## Focused validation

Every duration below is wall-clock `real` time from `/usr/bin/time -p`.

The exact current remote-stabilization TypeScript owner command was:

```sh
vp test scripts/toolchain-contract.test.ts scripts/tauri-hardening.test.ts scripts/check-dependency-upgrade-ledger.test.ts scripts/remote-architecture-contract.test.ts packages/contracts/scripts/export-rust-auth-fixtures.test.ts packages/contracts/src/remoteUpdate.test.ts packages/shared/src/pairingCode.test.ts packages/shared/src/advertisedEndpoint.test.ts packages/client-runtime/src/connection/presentation.test.ts packages/client-runtime/src/connection/pairingAdd.test.ts packages/client-runtime/src/e2ee/frame.test.ts packages/client-runtime/src/e2ee/noise.test.ts packages/client-runtime/src/e2ee/socket.test.ts apps/web/src/desktopDeepLink.test.ts apps/web/src/state/shareExposureReconciler.test.tsx apps/web/src/components/settings/remote-servers/connectPresentation.test.ts apps/web/src/components/settings/remote-servers/shareOffer.test.ts apps/web/src/components/settings/remote-servers/ShareThisHostTab.test.tsx apps/web/src/tauriDesktopBridge.test.ts apps/web/src/environments/primary/auth.helpers.test.ts packages/shared/src/httpReadiness.test.ts
```

| Command                                                                                                                                                                                                                              | Result/exit code | Duration | Evidence and warnings                                                                                                          |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------- | -------- | ------------------------------------------------------------------------------------------------------------------------------ |
| `vp test run apps/web/src/connection/storage.test.ts packages/client-runtime/src/platform/storageDocument.test.ts packages/client-runtime/src/connection/registry.test.ts packages/client-runtime/src/connection/pairingAdd.test.ts` | PASS / 0         | 0.99 s   | Task 11 durable compensation: 4 files, 103 tests passed.                                                                       |
| `vp test scripts/remote-architecture-contract.test.ts`                                                                                                                                                                               | PASS / 0         | 0.43 s   | 1 file, 1 test passed.                                                                                                         |
| `vp test packages/client-runtime/src/rpc/session.test.ts`                                                                                                                                                                            | PASS / 0         | 0.79 s   | 1 file, 7 tests passed.                                                                                                        |
| Current 21-file remote-stabilization TypeScript suite listed in the Task 9 plan, excluding deleted historical `remote-transport-hardening.test.ts` and the separately opt-in interop/Docker files                                    | PASS / 0         | 2.53 s   | 21 files, 243 tests passed. Node emitted a non-failing experimental `localStorage` warning.                                    |
| `cargo test -p bibcode-server --test auth_http plain_websocket_connected_state_tracks_the_completed_upgrade_lifecycle -- --exact`                                                                                                    | PASS / 0         | 0.25 s   | 1 passed.                                                                                                                      |
| `cargo test -p bibcode-server --test auth_http auth_routes_include_browser_cors_and_preflight_headers -- --exact`                                                                                                                    | PASS / 0         | 0.21 s   | Post-fix real `/api/auth/pairing-offer` preflight passed with `authorization`, `content-type`, `dpop`, and `idempotency-key`.  |
| `cargo test -p bibcode-server --test e2ee_ws oversized_pre_auth_websocket_message_is_rejected -- --exact`                                                                                                                            | PASS / 0         | 0.92 s   | 1 passed.                                                                                                                      |
| `cargo test -p bibcode-server --test e2ee_ws established_capacity_is_partitioned_by_principal_and_released_on_close -- --exact`                                                                                                      | PASS / 0         | 1.84 s   | 1 passed.                                                                                                                      |
| `cargo test -p bibcode-server --test e2ee_ws inbound_plaintext_capacity_is_partitioned_by_principal_and_released_on_close -- --exact`                                                                                                | PASS / 0         | 16.85 s  | 1 passed.                                                                                                                      |
| `cargo test -p bibcode-server rpc::e2ee::tests::completed_messages_retain_their_global_buffer_budget --lib -- --exact`                                                                                                               | PASS / 0         | 0.19 s   | 1 passed.                                                                                                                      |
| `cargo test -p bibcode-server rpc::session::tests::slow_socket_cannot_hide_more_than_one_large_response_in_the_session_queue --lib -- --exact`                                                                                       | PASS / 0         | 3.13 s   | 1 passed.                                                                                                                      |
| `cargo test -p bibcode-server rpc::session::tests::slow_sockets_share_one_process_outbound_plaintext_budget --lib -- --exact`                                                                                                        | PASS / 0         | 7.74 s   | 1 passed.                                                                                                                      |
| `cargo test -p bibcode-server rpc::session::tests::response_larger_than_the_connection_budget_fails_the_session_closed --lib -- --exact`                                                                                             | PASS / 0         | 0.15 s   | 1 passed.                                                                                                                      |
| `cargo test -p bibcode-server rpc::session::tests::byte_and_queue_admission_share_one_five_second_deadline --lib -- --exact`                                                                                                         | PASS / 0         | 0.15 s   | 1 passed.                                                                                                                      |
| `cargo test -p bibcode-server auth::service::tests::completed_pairing_offer_replays_and_cancels_after_restart --lib -- --exact`                                                                                                      | PASS / 0         | 0.18 s   | 1 passed.                                                                                                                      |
| `cargo test -p bibcode-server auth::service::tests::pending_pairing_offer_can_be_cancelled_after_restart --lib -- --exact`                                                                                                           | PASS / 0         | 0.19 s   | 1 passed.                                                                                                                      |
| `cargo test -p bibcode-server auth::service::tests::remote_offer_cancellation_converges_dormant_share_state_and_access_events --lib -- --exact`                                                                                      | PASS / 0         | 0.44 s   | 1 passed.                                                                                                                      |
| `cargo test -p bibcode-server --lib keeps_one_service_watcher -- --nocapture`                                                                                                                                                        | PASS / 0         | 0.97 s   | 3 watcher-ownership tests passed.                                                                                              |
| `cargo test -p bibcode-server auth::service::tests::cross_service_authentication_starts_watcher_for_the_cached_session --lib -- --exact`                                                                                             | PASS / 0         | 1.44 s   | 1 passed.                                                                                                                      |
| `cargo test -p bibcode-server --test repositories pairing_offer_reservations_enforce_the_shared_ -- --nocapture`                                                                                                                     | PASS / 0         | 1.79 s   | Principal and global quota cases: 2 passed.                                                                                    |
| `cargo test -p bibcode-server --test auth_http pairing_offer_authority_is_shared_across_simultaneously_live_servers -- --exact`                                                                                                      | PASS / 0         | 0.22 s   | 1 passed.                                                                                                                      |
| `cargo test -p bibcode-server --test auth_http remote_revocation_closes_an_acked_live_stream_before_later_events -- --exact`                                                                                                         | PASS / 0         | 0.46 s   | 1 passed.                                                                                                                      |
| `cargo test -p bibcode-server auth::service::tests --lib`                                                                                                                                                                            | PASS / 0         | 1.58 s   | 31 passed.                                                                                                                     |
| `cargo test -p bibcode-server persistence::migrations::tests --lib`                                                                                                                                                                  | PASS / 0         | 1.00 s   | 23 passed.                                                                                                                     |
| `cargo test -p bibcode-server --test repositories`                                                                                                                                                                                   | PASS / 0         | 1.17 s   | 22 passed.                                                                                                                     |
| `cargo test -p bibcode-server --test auth_http`                                                                                                                                                                                      | PASS / 0         | 1.36 s   | 29 passed.                                                                                                                     |
| `cargo test -p bibcode-server --test production_maintenance`                                                                                                                                                                         | PASS / 0         | 3.75 s   | 9 passed; the injected timeout case emitted its expected warning.                                                              |
| `cargo test -p bibcode-server rpc::e2ee::tests --lib`                                                                                                                                                                                | PASS / 0         | 14.80 s  | Fresh requested rerun: 18 passed.                                                                                              |
| `cargo test -p bibcode-server rpc::session::tests --lib`                                                                                                                                                                             | PASS / 0         | 7.75 s   | 7 passed.                                                                                                                      |
| `cargo test -p bibcode-server activity::rpc::tests --lib`                                                                                                                                                                            | PASS / 0         | 0.28 s   | 6 passed.                                                                                                                      |
| `cargo test -p bibcode-server --test e2ee_ws`                                                                                                                                                                                        | PASS / 0         | 37.37 s  | 16 passed.                                                                                                                     |
| `cargo test -p bibcode-server --test rpc_wire`                                                                                                                                                                                       | PASS / 0         | 0.56 s   | 13 passed.                                                                                                                     |
| `cargo test -p bibcode-server --test crypto_compat`                                                                                                                                                                                  | PASS / 0         | 0.25 s   | 3 passed.                                                                                                                      |
| `cargo test -p bibcode-server remote_update::tests --lib`                                                                                                                                                                            | PASS / 0         | 0.18 s   | 6 passed.                                                                                                                      |
| `cargo test -p bibcode-server --test remote_update_rpc`                                                                                                                                                                              | PASS / 0         | 2.12 s   | 2 passed.                                                                                                                      |
| `cargo test -p bibcode-server --test server_runtime route_inventory_covers_every_current_http_method_and_path -- --exact --nocapture`                                                                                                | PASS / 0         | 1.74 s   | 1 passed.                                                                                                                      |
| `cargo test -p bibcode-desktop firewall::tests --lib -- --nocapture`                                                                                                                                                                 | PASS / 0         | 0.23 s   | Post-fix 5 passed: exact persistent-rule removal, absence verification, policy/spawn failures, and delete-before-add ordering. |
| `cargo test -p bibcode-desktop server_exposure::tests --lib`                                                                                                                                                                         | PASS / 0         | 0.23 s   | Post-fix 12 passed, including fail-closed cleanup and serialized topology mutation.                                            |
| `cargo test -p bibcode-desktop backend::tests --lib`                                                                                                                                                                                 | PASS / 0         | 4.13 s   | 76 passed.                                                                                                                     |
| `cargo test -p bibcode-desktop bridge::tests --lib`                                                                                                                                                                                  | PASS / 0         | 7.85 s   | 45 passed.                                                                                                                     |
| `env RUST_TEST_THREADS=2 cargo test -p bibcode-server --test provider_opencode reconciliation_defers_history_that_cannot_fit_without_poisoning_its_signature -- --exact --nocapture` before repair                                   | FAIL / 101       | 0.24 s   | Deterministic RED: request count was 3, not the aggregate 4 read before reconciliation settled.                                |
| The same exact command after the minimal test-only repair and formatting                                                                                                                                                             | PASS / 0         | 2.91 s   | 1 passed; the test now waits for settlement and asserts semantic per-child bounds.                                             |

## VCS observation evidence

- Execution host and route: Unavailable for this stabilization scope. The
  validation did not construct a VCS observation scenario.
- Physical repositories/worktrees/active full subscribers/passive subscribers:
  not measured.
- Watcher health and fallback state: not measured.
- Automatic-fetch interval and passive-summary interval: current configured
  values (180 seconds / 30 seconds) were reviewed in living documentation, not
  executed as measurement evidence.

| Scenario                           | Signal source | Git launches after baseline | Publication result | Evidence class       |
| ---------------------------------- | ------------- | --------------------------- | ------------------ | -------------------- |
| Idle through 59 seconds            | N/A           | N/A                         | Not executed       | Unavailable evidence |
| 60-second safety boundary          | N/A           | N/A                         | Not executed       | Unavailable evidence |
| Worktree/index/HEAD/refs           | N/A           | N/A                         | Not executed       | Unavailable evidence |
| Structured terminal exit           | N/A           | N/A                         | Not executed       | Unavailable evidence |
| Overflow/setup unavailable         | N/A           | N/A                         | Not executed       | Unavailable evidence |
| Reconnect/hidden/reveal/focus/menu | N/A           | N/A                         | Not executed       | Unavailable evidence |

## Workspace and static gates

| Command                                                                                                                                                      | Result/exit code  | Duration | Test totals or warning summary                                                                                                                                                           |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------ | ----------------- | -------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `vp run test`                                                                                                                                                | NOT RUN           | N/A      | Task 14 explicitly requested the built-in full `vp test`; package-script graph duplication was not substituted for that command.                                                         |
| `cargo test --workspace -j 2 -- --test-threads=2`                                                                                                            | NOT RUN           | N/A      | Task 14 requested complete server and desktop package matrices, which were run separately with failures visible; updater-verifier workspace tests were outside this stabilization scope. |
| `vp check` (post-product, before report refresh)                                                                                                             | PASS / 0          | 3.67 s   | 1,995 files formatted; 1,408 files linted with no warnings/errors.                                                                                                                       |
| `vp check` (first refreshed-report attempt)                                                                                                                  | FAIL / 1          | 1.19 s   | Found only Markdown table-formatting drift in this report.                                                                                                                               |
| `vp check --fix`                                                                                                                                             | PASS / 0          | 4.90 s   | Mechanically formatted checked files and reported no lint errors.                                                                                                                        |
| `vp check` (final refreshed-report gate)                                                                                                                     | PASS / 0          | 3.85 s   | All 1,995 files correctly formatted; 1,408 files linted with no warnings/errors.                                                                                                         |
| `vp run typecheck`                                                                                                                                           | PASS / 0          | 15.09 s  | Post-final-review product gate: 11/11 tasks completed; 123 non-failing Effect suggestion diagnostics were emitted.                                                                       |
| `vp test`                                                                                                                                                    | PASS / 0          | 22.05 s  | Post-final-review product gate: 613 files and 8,548 tests passed; 2 files and 29 tests skipped.                                                                                          |
| `cargo fmt --all --check`                                                                                                                                    | PASS / 0          | 2.05 s   | Post-final-review product gate; no Rust formatting drift.                                                                                                                                |
| `env RUST_TEST_THREADS=2 cargo test -p bibcode-server --no-fail-fast -j 2` (first attempt)                                                                   | FAIL / 101        | 301.82 s | 2,795 passed, 1 failed, 2 ignored; only `provider_opencode` failed.                                                                                                                      |
| `env RUST_TEST_THREADS=1 cargo test -p bibcode-server --no-fail-fast -j 2` (justified serialized attempt)                                                    | FAIL / 101        | 410.28 s | Same matrix: 2,795 passed, 1 failed, 2 ignored; same exact assertion failed.                                                                                                             |
| `env RUST_TEST_THREADS=2 cargo test -p bibcode-server --no-fail-fast -j 2` (final after repair)                                                              | PASS / 0          | 255.16 s | 2,796 passed, 0 failed, 2 ignored; every unit, integration, binary, and doc-test target completed.                                                                                       |
| Post-final-review `env RUST_TEST_THREADS=2 cargo test -p bibcode-server --no-fail-fast -j 2` attempt                                                         | INTERRUPTED / 130 | 369.99 s | All reported cases were green, but an existing native OpenCode helper fixture stalled after its 60-second warning; the harness was manually stopped rather than reported as a pass.      |
| Exact isolated stalled OpenCode helper fixture                                                                                                               | PASS / 0          | 0.20 s   | `ownership_freeze_rejects_and_reaps_an_opencode_helper_before_admission` passed 1/1 immediately.                                                                                         |
| Post-final-review `env RUST_TEST_THREADS=1 cargo test -p bibcode-server --no-fail-fast -j 2` authoritative rerun                                             | PASS / 0          | 414.32 s | All targets passed: 1,702 library tests passed, 2 performance tests ignored, and every integration, binary, and doc-test target completed.                                               |
| Post-final-review `env RUST_TEST_THREADS=2 cargo test -p bibcode-desktop --no-fail-fast -j 2`                                                                | PASS / 0          | 37.51 s  | 321 library + 1 bridge + 2 deep-link + 5 SSH tests = 329 passed; all integration targets executed after the firewall refactor.                                                           |
| `cargo clippy -p bibcode-server --all-targets -- -D warnings`                                                                                                | PASS / 0          | 17.36 s  | Post-final-review product gate; no warnings.                                                                                                                                             |
| `cargo clippy -p bibcode-desktop --all-targets -- -D warnings`                                                                                               | PASS / 0          | 4.46 s   | Post-final-review product gate after the firewall helper refactor; no warnings.                                                                                                          |
| `cargo build -p bibcode-server`                                                                                                                              | PASS / 0          | 19.51 s  | Current 0.4.1 x86-64 ELF built at `target/debug/bibcode`; size 550,479,928 bytes; Build ID `e10c5b43dc0a2991af2a645715afd5dd114e8246`.                                                   |
| `(cd packages/client-runtime && BIBCODE_E2EE_SERVER_BIN="$(git rev-parse --show-toplevel)/target/debug/bibcode" vp test run src/e2ee/serverInterop.test.ts)` | PASS / 0          | 1.87 s   | 3 passed: pairing/pinned Noise RPC/reconnect, fragmented request reassembly, and bad-token rejection. Fragmentation evidence belongs here, not to Docker.                                |
| Exact Debian `trixie-slim` server / Node 26 Bookworm client Docker block from `docs/testing/cross-platform-validation.md`                                    | PASS / 0          | 1.81 s   | Fresh post-final-review client run: 1 test passed; no credential value was printed or retained.                                                                                          |
| `git diff --check`                                                                                                                                           | PASS / 0          | 0.00 s   | No whitespace errors in the staged report/checklist diff.                                                                                                                                |

## Cross-container remote-server gate

The exact command was the current block under **Cross-container remote-server
gate** in `docs/testing/cross-platform-validation.md`: `docker version`, the
named cleanup trap, network/volume creation, a `debian:trixie-slim` server with
the current binary mounted read-only, bounded administrative pairing issuance,
a `node:26-bookworm` client invoking
`./node_modules/.bin/vp test packages/client-runtime/src/e2ee/dockerRemoteSmoke.test.ts`,
credential-variable unsetting, cleanup, and the three filtered absence checks.
No command interpolation or credential value is retained in this report.

- Runtime: Docker CLI 29.7.2, API downgraded to 1.44, context
  `pathfinder-podman`; Podman Engine 5.8.4, `crun` 1.28, Linux/amd64 on Fedora 44. This is not a Docker Engine daemon and is reported as such.
- Server image: `debian:trixie-slim`,
  `sha256:e426a54f50cc4cf82dd5cab8ba8426ed02c391840cb5a62dfd987542dbabea3b`,
  Linux/amd64, 81,081,546 bytes.
- Client image: `node:26-bookworm`,
  `sha256:55d3eaf406db9ece10a4d68c6780e41020683453e7a8654d68370a2abddf41ed`,
  Linux/amd64, 1,178,872,719 bytes.
- Isolation: distinct server and client containers on the test-owned
  `bibcode-remote-stabilization` network; server state in the test-owned
  `bibcode-remote-stabilization-data` volume; checkout mounted read-only; only
  Vite's cache path was tmpfs-writable; SELinux process labeling was disabled
  for both read-only host bind mounts as the runbook specifies.
- Covered by the one Docker test: descriptor compatibility/update support,
  administrative credential exchange, off-host pairing offer, pinned-host
  Noise NK authentication, authenticated E2EE RPC, `updater.status`,
  `updater.check`, typed `remote_update_manual_required` install failure,
  browser-session exposure retention, client revocation, final loopback share
  state, an intentionally unread successful offer response, cancellation, and
  rejection of the delayed retry. The Docker test did **not** exercise
  fragmented RPC; that was proven only by direct interop and server tests.
- Cleanup: the three exact filtered container/network/volume listings produced
  zero bytes. `docker inspect bibcode-remote-server`, network inspect, and
  volume inspect each exited 1 afterward. `pgrep -x bibcode` also found no
  host-side server survivor.

## Native package artifacts

| Artifact                 | Absolute path | Version/architecture | Identity/trust verification                                                   |
| ------------------------ | ------------- | -------------------- | ----------------------------------------------------------------------------- |
| Native installer/package | Unavailable   | N/A                  | No AppImage, DMG, or NSIS artifact was built; no signing/trust claim is made. |

## Packaged UI and visual evidence

| Scenario                          | Screenshot absolute path | State        | Pixel-review finding                                                              |
| --------------------------------- | ------------------------ | ------------ | --------------------------------------------------------------------------------- |
| Packaged remote sharing/update UI | Unavailable              | Not executed | No screenshot or pixel-review claim.                                              |
| Native compensation outcomes      | Unavailable              | Not executed | Covered by source-level tests only, not substituted for packaged visual evidence. |

- Exact executable launched: no packaged desktop executable. Direct interop
  launched this checkout's `target/debug/bibcode`; Docker mounted that exact
  binary read-only at `/usr/local/bin/bibcode`.
- Exact PID/start identity: test harness/container-owned and reaped; no packaged
  desktop PID existed.
- Other installed or development copies excluded: no installed Coddy Studio copy
  was launched.
- External tool, command, and path used for the Files Refresh rescan: not in
  scope and not executed.
- Authentication-dependent scenarios unavailable: authenticated provider UI
  and provider session flows had no suitable credentials.

## External-worktree scenario

- Disposable repository root: not created; external-worktree behavior was not
  part of the remote-server stabilization boundary.
- Git-reported worktrees: unavailable.
- Physical/path-alias identities: unavailable.
- Discovery result: not executed.
- Adoption/idempotence result: not executed.
- Restart result: not executed.
- Hide/remove non-destructive result: not executed.
- Final on-disk verification: not applicable; no fixture was created.

## Process and temporary-root cleanup

- Before snapshot: no `bibcode-remote-*` container,
  `bibcode-remote-stabilization` network, or
  `bibcode-remote-stabilization-data` volume; no exact `bibcode` process.
- After snapshot: the same three Docker resource filters were empty; all three
  inspect calls reported absent; no exact `bibcode` process remained.
- Scoped surviving processes: none from direct interop or Docker smoke.
- New test-owned roots: Rust/Vitest temporary roots were owned and removed by
  their harnesses; the Docker state volume was explicitly removed.
- Pre-existing roots/processes intentionally left untouched: existing
  CodeGraph/application-agent processes and all unrelated host state.
- Package mounts or platform resources released: server/client containers,
  network, and volume removed; no package mount was created.

## Non-native compatibility evidence

### Windows

- Evidence class: Compatibility evidence plus unavailable native evidence
- Source/contracts reviewed: Windows desktop runbook, desktop exposure/bridge
  owners, deep-link configuration, SSH public contract, remote architecture,
  and host-independent TypeScript/Rust contracts.
- Commands and results: the post-fix 329-test desktop matrix, focused firewall
  and exposure owner suites, remote TypeScript matrix, and both Clippy targets
  passed on Linux.
- Native-only evidence still required: Windows x64 package/UI, WSL topology,
  firewall creation/removal and policy denial, WebView2, deep-link routing,
  Authenticode classification, and updater installer behavior.

### macOS

- Evidence class: Compatibility evidence plus unavailable native evidence
- Source/contracts reviewed: macOS runbook, deep-link configuration, remote
  architecture, desktop process/exposure contracts, and cross-platform tests.
- Commands and results: the same host-independent contract and Rust desktop
  matrices passed on Linux.
- Native-only evidence still required: arm64/x64 app/DMG, bundled custom scheme,
  package UI, process groups, signing, notarization, and updater installer.

The Linux and macOS platform runbooks and shared testing index were reviewed
and remain accurate. The Windows and cross-platform runbooks were updated in
`71cd7fc3` to require verified persistent-firewall-rule removal and explicit
policy/spawn failure evidence.

## Source changes and commits created

- Task 14 fixture repair: `apps/server/tests/provider_opencode.rs` changed only
  its observation boundary so reconnect reconciliation can settle before the
  semantic per-child bounds are asserted. Commit:
  `301ea75c test(server): stabilize deferred history reconciliation`.
- Final-review CORS repair: `apps/server/src/http.rs` now permits the
  `idempotency-key` request header, and `apps/server/tests/auth_http.rs`
  preflights the real pairing-offer route with every required header. RED was
  an omitted allowed-header assertion; GREEN is the exact case plus the
  complete 29-test auth HTTP target. Commit:
  `d51fdba5 fix(server): allow pairing offer idempotency preflight`.
- Final-review firewall repair: `apps/desktop/src-tauri/src/firewall.rs` now
  removes the exact persistent Windows rule with terminating PowerShell error
  behavior, re-queries to prove absence, propagates launch/policy failures, and
  verifies cleanup before a program-scoped add. Five focused tests cover the
  scripts, both error classes, and ordering; the complete 329-test desktop
  matrix is green. `docs/architecture/remote.md`, the Windows runbook, and the
  cross-platform runbook document the guarantee. Commit:
  `71cd7fc3 fix(desktop): verify remote firewall cleanup`.
- This execution report and the final Task 14 checklist bookkeeping are the
  final documentation-only commit. No executable behavior follows
  `71cd7fc3`.

History disclosure: `929d0e80` violated the execution prompt's hard rule by
committing the two pre-existing user-staged environment-project deletions into
the remote-server work. `f458ce6c` restored those documents and the baseline
provider test as out-of-scope product state. The controller will reapply those
two exact staged deletions only after the final documentation/review commit;
Task 14 did not itself stage, delete, or edit either document. `26052fc4`
originally introduced the provider fixture stabilization. Task 14 reproduced
that failure under the ordinary, exact, and serialized commands before
restoring only the test change as `301ea75c`. `ec626fb6` is the Task 13
production fix that makes authentication and live WebSocket registration one
atomic in-memory authority decision.

Final adversarial review disclosure: review of `3b1864eff..98189128` found the
pairing-offer CORS omission, best-effort Windows firewall deletion, and the two
still-pending user-owned staged deletions. The two product findings were fixed
in `d51fdba5` and `71cd7fc3`; independent re-review found no remaining
Critical, High, or Medium product issue. The staged deletions are intentionally
reapplied after this report/checklist commit so they cannot enter feature
history again.

Hidden production-delta disclosure: `81eff018` has a documentation-oriented
subject but also changed `apps/server/src/production/relay.rs` to await pending
Tokio file work and drop the writable OS handle before checksum validation or
execution, preventing Linux `ETXTBSY`. `e932cd3f` then added a Unix-only,
10 ms `ETXTBSY` retry inside the existing bounded validation timeout. Other
errors are not retried, and the validation deadline remains authoritative.

## Commands not run

| Command or scenario                                          | Reason                                                                                                                                                | Required follow-up owner                                |
| ------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------- |
| `vp run test`                                                | Task 14 explicitly required full built-in `vp test`; the package-script graph was not required and would duplicate separately recorded Rust matrices. | CI/package-graph validation when specifically requested |
| `cargo test --workspace -j 2 -- --test-threads=2`            | Complete server and desktop matrices were the requested Rust scope; updater-verifier was unaffected.                                                  | CI/workspace validation                                 |
| `vp run dist:desktop:linux` and packaged UI/visual scenarios | Fedora 44 is not a documented release-validation distro and native package/UI evidence was outside this source/integration stabilization run.         | Linux native release validation                         |
| Windows package/UI/WSL/firewall/updater scenarios            | Windows host unavailable.                                                                                                                             | Windows native release validation                       |
| macOS app/DMG/UI/signing/notarization/updater scenarios      | macOS host and credentials unavailable.                                                                                                               | macOS native release validation                         |
| VCS observation and external-worktree scenarios              | No VCS/worktree behavior changed in Tasks 11–13, the Task 14 fixture repair, or the final CORS/firewall fixes.                                        | Feature-specific native validation                      |
| Authenticated provider visual/session scenarios              | No suitable provider credentials; secrets were not substituted.                                                                                       | Credentialed native validation                          |

## Residual risks

- Risk: no packaged native artifact or UI/visual evidence on a supported release
  host. Impact: OS firewall, WSL routing, bundle/deep-link identity, signing,
  installer, and rendered compensation messages remain unproven. Evidence that
  bounds it: complete source-level server/desktop matrices, bridge/deep-link/SSH
  integrations, direct interop, and cross-container smoke. Required follow-up:
  native release validation on each supported platform.
- Risk: Fedora 44 is outside the documented Linux release-validation set.
  Impact: this run cannot qualify an Ubuntu/Debian AppImage. Evidence that
  bounds it: Rust and TypeScript gates were native on x86-64 Linux, while the
  server/client boundary also ran in Debian/Node containers. Required follow-up:
  supported-distribution package build and UI smoke.
- Risk: simultaneously live auth services were tested with independent SQLite
  workers/connections in one process, while Docker used one server process and
  one client container. Impact: a two-server-process shared-WAL deployment is
  not directly exercised here. Evidence that bounds it: repository
  transactions, independent-connection tests, watcher convergence, restart,
  and cross-container client/server traffic passed. Required follow-up: an
  explicit two-server-process shared-store test if that deployment is promoted.
- Risk: separate desktop processes are not coordinated by an OS/file lock for
  the protected connection catalog. Impact: cross-process catalog replacement
  remains a documented limitation. Evidence that bounds it: Task 11's 103-test
  CAS/registry suite covers runtimes sharing the configured catalog authority.
  Required follow-up: a separately designed cross-process catalog lock.
- Risk: outbound E2EE admission is intentionally process/per-connection rather
  than principal-fair. Impact: one principal's slow sockets may delay unrelated
  output until the five-second admission/write bounds converge. Evidence that
  bounds it: process and connection byte-budget regressions plus full E2EE and
  server matrices passed. Required follow-up: principal-fair output admission
  only if product requirements change.

## Publication state

- Final completion commits: `301ea75c`, report commit `98189128`, CORS fix
  `d51fdba5`, firewall fix `71cd7fc3`, plus the final documentation-only commit
  containing this refreshed report and checklist update
- User-owned final staged state: the controller reapplies the two exact
  environment-project deletions immediately after this documentation/review
  commit; they remain staged and outside remote-server history
- Pushed: no
- Branch merged: no
- Pull request opened: no
- Artifacts published: no
