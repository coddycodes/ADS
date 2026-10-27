# Platform Validation Execution Report

**Result:** PASS WITH RESIDUAL RISKS

## Tested revision

- Repository: Coddy Studio (`https://github.com/mubeda/BibCode.git`)
- Remote: `origin`; requested publication branch `develop`
- Branch or requested revision: local `mubeda/develop`; adversarial-remediation
  range `e3cd9d81b4cee897fb42709f88ca9f1509c85ba1..f3eb0ab763e4a521ad5efbdccce365991f66fdfd`
- Local HEAD: `f3eb0ab763e4a521ad5efbdccce365991f66fdfd` was the executable
  remediation HEAD during validation. The final documentation/evidence commit
  follows it.
- Remote HEAD before publication: `origin/develop` at
  `e3cd9d81b4cee897fb42709f88ca9f1509c85ba1`
- Merge base and ahead/behind before publication: merge base `e3cd9d81`; remote
  versus local `0 16`
- Dirty state before execution: two pre-existing staged environment-project
  document deletions and the untracked adversarial-review document, plus the
  intended Task 14 documentation and Docker-test edits
- Dirty state after execution: the same three protected user-owned paths plus
  the intended Task 14 edits and this report; no generated or container-owned
  resource remained

## Native environment

- Operating system and release/build: Fedora Linux 44 Workstation
- Architecture: `x86_64` / `linux/amd64`
- Kernel: `Linux 7.1.10-200.fc44.x86_64`
- Rust/Cargo: `rustc 1.97.1 (8bab26f4f 2026-07-14)`;
  `cargo 1.97.1 (c980f4866 2026-06-30)`
- Node/package manager/Vite+: Node 26.5.0; pnpm 11.15.0 through Vite+; global
  Vite+ 0.3.0, repository Vite+ 0.2.5, Vitest 4.1.10
- Native compiler/SDK/runtime dependencies: the installed Linux development
  dependencies supported complete server and desktop test and Clippy matrices
- Optional capabilities: Docker CLI 29.7.2 used context `pathfinder-podman` and
  Podman Engine 5.8.4 on Linux/amd64. Windows/WSL, macOS, signing, notarization,
  and packaged installers were unavailable.

## Requested inputs and ancestry

- Expected product version: 0.4.1
- Observed version sources: server and desktop Cargo manifests
- Required commits: the 16 commits in the tested remediation range, beginning
  with `d4fe12a2` and ending with `f3eb0ab7`
- Ancestry result: the range is linear above `origin/develop`; every listed
  remediation commit is an ancestor of the tested HEAD
- Inputs that were unavailable: supported-platform packaged desktop builds,
  a second physical LAN device, Windows firewall/WSL evidence, macOS bundle
  evidence, signing/notarization, and authenticated provider credentials

## Focused validation

| Command                                                                                                                                                      | Result/exit code | Duration                      | Evidence and warnings                                                                                                         |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------- | ----------------------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| `vp test scripts/remote-architecture-contract.test.ts packages/client-runtime/src/e2ee/dockerRemoteSmoke.test.ts`                                            | PASS / 0         | 0.41 s                        | Architecture contract passed; Docker test skipped as designed outside its container environment.                              |
| `vp test apps/web/src/components/settings/remote-servers/ConnectTab.test.tsx`                                                                                | PASS / 0         | 1.15 s                        | 40 tests passed after preserving the shared transport-policy seam in the module mock.                                         |
| `cargo test -p bibcode-server --test activity_repository monitoring_disabled_finalization_interrupts_once_and_preserves_completed_history -- --exact`        | PASS / 0         | 1.10 s build plus 0.02 s test | RED was deterministic after the July fixture crossed the 30-day retention cutoff; GREEN uses the established 2099 test epoch. |
| Five consecutive exact runs of `agent_activity_toggle_opencode_busy_dormant_stream_does_not_starve_handoff_deadline`                                         | PASS / 0         | 0.43–0.47 s each              | Bounds the pre-existing full-suite timing failure observed in an earlier attempt.                                             |
| `(cd packages/client-runtime && BIBCODE_E2EE_SERVER_BIN="$(git rev-parse --show-toplevel)/target/debug/bibcode" vp test run src/e2ee/serverInterop.test.ts)` | PASS / 0         | 1.61 s                        | 3 tests passed against the freshly built native server.                                                                       |

## VCS observation evidence

- Execution host and route: unavailable for this remote-server remediation
- Physical repositories/worktrees/active full subscribers/passive subscribers:
  not measured
- Watcher health and fallback state: not measured
- Automatic-fetch interval and passive-summary interval: not in scope

| Scenario                                                                                                    | Signal source | Git launches after baseline | Publication result | Evidence class       |
| ----------------------------------------------------------------------------------------------------------- | ------------- | --------------------------- | ------------------ | -------------------- |
| Idle, safety boundary, worktree/index/HEAD/refs, structured terminal exit, overflow, and reconnect UI cases | N/A           | N/A                         | Not executed       | Unavailable evidence |

## Workspace and static gates

| Command                                                                              | Result/exit code | Duration                                | Test totals or warning summary                                                                                                                                    |
| ------------------------------------------------------------------------------------ | ---------------- | --------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `vp run check:contracts`                                                             | PASS / 0         | Not separately timed                    | 4 TypeScript fixture-exporter tests, 5 Rust-parity tests, and 13 Rust RPC wire tests passed.                                                                      |
| `vp run check:dependency-ledger`                                                     | PASS / 0         | Not separately timed                    | 81 direct JavaScript dependencies, 82 registry Rust crates, 1 local Rust crate, 9 actions, 9 toolchain pins, 0 unaccounted.                                       |
| `vp check . '!docs/plans/remote-servers/2026-08-29-adversarial-review.md'`           | PASS / 0         | 3.67 s combined formatting/lint work    | 1,999 files formatted and 1,412 files linted with no warning/error. The protected untracked review was excluded because it is user-owned and not formatter-clean. |
| `vp run typecheck`                                                                   | PASS / 0         | Not separately timed                    | 11/11 targets passed; existing non-failing Effect suggestions were emitted.                                                                                       |
| `vp test`                                                                            | PASS / 0         | 21.05 s                                 | 615 files and 8,565 tests passed; 2 files and 29 tests skipped.                                                                                                   |
| `cargo fmt --all --check`                                                            | PASS / 0         | Not separately timed                    | No Rust formatting drift.                                                                                                                                         |
| `cargo test -p bibcode-server --no-fail-fast`                                        | PASS / 0         | Not separately timed                    | Final complete invocation exited 0; 1,713 unit tests passed, 2 performance tests ignored, and every integration/binary/doc-test target passed.                    |
| `cargo test -p bibcode-desktop --no-fail-fast`                                       | PASS / 0         | About 37 s                              | 325 unit plus 8 integration tests passed.                                                                                                                         |
| `cargo clippy -p bibcode-server --all-targets -- -D warnings`                        | PASS / 0         | Included in 13.94 s combined Clippy run | No warnings.                                                                                                                                                      |
| `cargo clippy -p bibcode-desktop --all-targets -- -D warnings`                       | PASS / 0         | Included in 13.94 s combined Clippy run | No warnings.                                                                                                                                                      |
| `cargo build -p bibcode-server`                                                      | PASS / 0         | 36.06 s                                 | Current Linux/amd64 debug server built for direct and container interop.                                                                                          |
| Cross-container remote-server block from `docs/testing/cross-platform-validation.md` | PASS / 0         | 0.81 s test duration                    | 1 opt-in smoke test passed; no credential was printed or retained.                                                                                                |
| `git diff --check`                                                                   | PASS / 0         | Under 0.1 s                             | No whitespace errors.                                                                                                                                             |

Two non-green attempts were investigated rather than classified as baseline by
assumption:

- The first server run exposed a deterministic date-rotted activity fixture.
  The fixture predates the branch; its timestamp was repaired and the exact
  test plus a later complete server run passed.
- The next complete server attempt hit a pre-existing one-second OpenCode
  connection timeout under suite load. Git blame places the wait before this
  branch, five immediate isolated repeats passed, and the final complete server
  invocation passed.

## Cross-container remote-server gate

The exact current runbook block created a distinct server container and client
container on a test-owned network, mounted the current binary and checkout
read-only, used a dedicated state volume, and gave only Vite's cache path a
writable tmpfs.

- Runtime: Docker CLI 29.7.2 with API 1.44 against Podman Engine 5.8.4,
  `crun` 1.28, Fedora 44, Linux/amd64
- Server image: `debian:trixie-slim`,
  `sha256:e426a54f50cc4cf82dd5cab8ba8426ed02c391840cb5a62dfd987542dbabea3b`,
  Linux/amd64
- Client image: `node:26-bookworm`,
  `sha256:55d3eaf406db9ece10a4d68c6780e41020683453e7a8654d68370a2abddf41ed`,
  Linux/amd64
- Covered: descriptor/support negotiation, administrative exchange, four
  same-peer pre-auth leases and fifth-socket rejection, off-host pairing,
  pinned Noise NK authentication, authenticated RPC, status/check/manual
  install update behavior, a 2,049-record logical-message rejection,
  browser-session exposure retention, revocation and final loopback state,
  ambiguous offer cancellation, and delayed-retry rejection
- Cleanup: the exact filtered container, network, and volume listings produced
  no output. No custom image was built, so there was no test-created image to
  remove.

## Native package artifacts and packaged UI

| Artifact or scenario              | Path/evidence | State                                                                 |
| --------------------------------- | ------------- | --------------------------------------------------------------------- |
| Native installer/package          | Unavailable   | No package or signing claim.                                          |
| Packaged remote sharing/update UI | Unavailable   | Source-level behavior and runbook coverage only; no screenshot claim. |

The exact executable exercised outside Cargo's test harness was this checkout's
`target/debug/bibcode`. No installed Coddy Studio copy was launched.

## Process and temporary-root cleanup

- Before and after snapshots: no `bibcode-remote-*` container,
  `bibcode-remote-stabilization` network, or
  `bibcode-remote-stabilization-data` volume remained
- Scoped surviving processes: none from direct interop or Docker smoke
- New test-owned roots: harness temporary roots were released; the Docker state
  volume was explicitly removed
- Pre-existing roots/processes intentionally left untouched: unrelated host,
  CodeGraph, workspace, and container-image state

## Non-native compatibility evidence

### Windows

- Evidence class: compatibility evidence plus unavailable native evidence
- Source/contracts reviewed: Windows desktop runbook, desktop exposure,
  firewall, WSL, endpoint enumeration, Tailscale, IndexedDB, update, and shared
  transport contracts
- Native-only evidence still required: packaged x64 UI, WSL topology changes,
  live firewall policy/timeout behavior, WebView2, Authenticode, and updater

### macOS

- Evidence class: compatibility evidence plus unavailable native evidence
- Source/contracts reviewed: macOS desktop runbook, endpoint/Tailscale,
  deep-link, IndexedDB, exposure, update, and shared transport contracts
- Native-only evidence still required: arm64/x64 package UI, bundle custom
  scheme, signing/notarization, and updater

The Linux, macOS, Windows, and cross-platform runbooks were reviewed and
updated. The connection-runtime runbook was reviewed and remains accurate.

## Source changes and commits created

- Remediation commits before this evidence patch:
  `d4fe12a2`, `2776f39b`, `e7b23a56`, `a0fecd32`, `1bcff049`,
  `9dcaf37d`, `a9c8bf09`, `072cad35`, `a06c40b5`, `89f4ec89`,
  `133ab5d8`, `6622d9a2`, `95e8c0c4`, `1b0c68c8`, `1e0a9b77`, and
  `f3eb0ab7`
- This final patch aligns living architecture, supersession notes, native
  runbooks, the architecture gate, the Docker smoke, its real-socket test
  helper, the Connect-tab mock seam, and the date-stable activity fixture.
- RED evidence: the architecture gate rejected stale exchange wording; the
  Connect-tab file failed 5 cases for a missing shared policy export; the
  activity test failed after crossing the real retention cutoff.
- GREEN evidence: exact focused reruns, the complete TypeScript suite, the final
  complete Rust package matrices, both Clippy targets, direct interop, and the
  cross-container test all passed.

## Commands not run

| Command or scenario                                                                   | Reason                                                                                                                                  | Required follow-up owner                                 |
| ------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------- |
| Raw `vp check` including `docs/plans/remote-servers/2026-08-29-adversarial-review.md` | The untracked review is protected user-owned content and is not formatter-clean; changing it would violate the preservation constraint. | Owner of that review document, if it is later committed. |
| Packaged desktop UI on Linux, Windows, and macOS                                      | No package build or supported-platform signing environment was requested or available.                                                  | Release validation on each supported platform.           |
| Physical second-device LAN pairing                                                    | The two-container boundary was the requested repeatable integration evidence.                                                           | Native/manual platform validation.                       |

## Residual risks

- Packaged updater, deep-link, exposure, and firewall visuals remain unverified
  on Windows and macOS. Contract, source, unit, integration, and Linux
  cross-container evidence bounds the risk but does not replace native package
  validation.
- The pre-existing OpenCode supervisor test can miss its one-second setup
  deadline under full-suite host contention. It passed five isolated repetitions
  and the final complete server run; this is test-harness timing risk outside the
  remote-server implementation.

## Publication state

- Commits created: the 16 listed remediation commits; this report is part of a
  pending final documentation/evidence commit
- Pushed: no at report creation; publication to remote `develop` follows final
  review as explicitly requested
- Branch merged: no
- Pull request opened: no
- Artifacts published: no
