# Platform Validation Execution Report

**Result:** PASS WITH RESIDUAL RISKS

Native Windows 11 ARM64 remediation run for the findings in the historical
Windows report at `c24fbd26`. All Rust compile, focused test, static, packaging,
and packaged-E2E gates ran natively on the validation guest. Two items remain
open and are listed under Residual risks: one load-dependent timeout in the
production Git Manager RPC test file under the default harness width (passes in
isolation), and the manual packaged-UI regression, which was not driven because
no Computer Use surface was available in this session.

## Tested revision

- Repository: `mubeda/BibCode` (checkout `C:\Users\admin\Projects\GitHub\BibCode`)
- Remote: `origin`
- Branch or requested revision: `main`
- Local HEAD: `e87efa97` (Merge pull request #14 from mubeda/main-2) plus the
  uncommitted working tree described under Source changes
- Remote HEAD: not queried
- Merge base and ahead/behind: not queried
- Dirty state before execution: clean
- Dirty state after execution: the working-tree changes listed under Source
  changes; no generated files, no `.codegraph/` edits, zero fixture drift
- Historical evidence SHA: `c24fbd26`. Commit `56f329c8` landed between that
  SHA and HEAD and already replaced the History live-signal refresh with
  repository-generation tracking; it is the primary fix for the stale-History
  finding and was verified in source and by unit tests here.

## Native environment

- Operating system and release/build: Windows 11 Pro 10.0.26100 (Parallels
  guest, interactive account `maurorodrig988d\admin`, `LOCALAPPDATA` under
  `C:\Users\admin`)
- Architecture: ARM64 (`PROCESSOR_ARCHITECTURE=ARM64`)
- Kernel: NT 10.0.26100
- Rust/Cargo: rustc 1.97.1 (8bab26f4f 2026-07-14), cargo 1.97.1, default host
  `aarch64-pc-windows-msvc`, `CARGO_HOME=C:\bibcode-validation\cargo`,
  `RUSTUP_HOME=C:\bibcode-validation\rustup`
- Node/package manager/Vite+: Node 26.5.0, pnpm 11.15.0, global vp 0.2.5
  (`C:\bibcode-validation\npm-global`), workspace `vite-plus` 0.2.5
- Native compiler/SDK/runtime dependencies: Visual Studio 2026 (v18.0) ARM64
  developer environment via `scripts/run-msvc.mjs`; NSIS 3.11 downloaded by
  Tauri into `target/.tauri/NSIS`
- Optional capabilities such as WSL, signing, or notarization: WSL absent
  (not installed, by design); artifacts unsigned; Git for Windows has
  `core.autocrlf=true` at system and global scope

## Requested inputs and ancestry

- Expected product version: `0.4.2`
- Observed version sources: package manifests; installer and executable
  `ProductVersion`/`FileVersion` 0.4.2
- Required commits: `c24fbd26` (evidence), `56f329c8` (History generation fix)
- Ancestry result for each commit: both are ancestors of `e87efa97`
- Inputs that were unavailable: the macOS-side evidence directory
  (`/Users/admin/.codex/visualizations/...`) is not present on the guest;
  findings were taken from the request text and reproduced against source

## Focused validation

All commands ran from the repository root with
`C:\bibcode-validation\cargo\bin;C:\bibcode-validation\npm-global` prepended to
`PATH` and the retained `CARGO_HOME`/`RUSTUP_HOME`. The 8.6 GB dependency cache
from `C:\bibcode-validation\BibCode-c24fbd26\target` was copied into `target/`
first, so only workspace crates compiled.

| Command                                                                                                                                                                                                                                                                                                                                                              | Result/exit code                                                                                                                                                                                                            | Duration              | Evidence and warnings                                                                                                                                                 |
| -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `node scripts/run-msvc.mjs cargo check -p bibcode-server --all-targets`                                                                                                                                                                                                                                                                                              | Native: first run exit 101 (`ROLLUP_COMMAND` dead code in `source_control/checks.rs`, Unix-only); after gating, exit 0                                                                                                      | 1m 53s                | The original `mod.rs` and `production_git_manager_rpc.rs` findings were already fixed; the third instance came from `56f329c8`                                        |
| `... cargo test -p bibcode-server --lib git::broadcaster::tests::ref_poll_is_replaced_by_watcher_and_safety_status_reads -- --exact --nocapture`                                                                                                                                                                                                                     | Native: exit 0                                                                                                                                                                                                              | 5m 31s incl. compile  | 1 passed                                                                                                                                                              |
| `... cargo test -p bibcode-server --lib git::watcher -- --nocapture`                                                                                                                                                                                                                                                                                                 | Native: exit 0                                                                                                                                                                                                              | 0m 04s                | 31 passed                                                                                                                                                             |
| `... cargo test -p bibcode-server --lib production::runtime::tests::structured_terminal_process_exit_immediately_invalidates_status_under_watcher_fallback -- --exact --nocapture`                                                                                                                                                                                   | Native: exit 0                                                                                                                                                                                                              | 0m 04s                | 1 passed                                                                                                                                                              |
| `... cargo test -p bibcode-server --lib git::manager:: -- --nocapture`                                                                                                                                                                                                                                                                                               | Native: first run 95 passed / 3 failed (`"base\r\n"` vs `"base\n"`); after pinning `core.autocrlf=false` in the fixtures, exit 0                                                                                            | 3m 25s                | 98 passed                                                                                                                                                             |
| `... cargo test -p bibcode-server --lib source_control::checks:: -- --nocapture`                                                                                                                                                                                                                                                                                     | Native: exit 0                                                                                                                                                                                                              | 0m 04s                | 2 passed (6 Unix-only tests filtered)                                                                                                                                 |
| `... cargo test -p bibcode-server --test production_git_vcs_rpc native_watcher_publishes_external_worktree_and_head_changes_to_status_subscribers -- --exact --nocapture`                                                                                                                                                                                            | Native: exit 0                                                                                                                                                                                                              | 3m 35s                | 1 passed                                                                                                                                                              |
| `... cargo test -p bibcode-server --test git_manager_reads -- --nocapture`                                                                                                                                                                                                                                                                                           | Native: exit 0                                                                                                                                                                                                              | 0m 58s                | 6 passed                                                                                                                                                              |
| `... cargo test -p bibcode-server --test git_manager_commit -- --nocapture`                                                                                                                                                                                                                                                                                          | Native: first run 1 failed (CRLF); after fixture pin, exit 0                                                                                                                                                                | 1m 13s                | 1 passed                                                                                                                                                              |
| `... cargo test -p bibcode-server --test git_rpc -- --nocapture`                                                                                                                                                                                                                                                                                                     | Native: exit 0                                                                                                                                                                                                              | 0m 41s                | 44 passed                                                                                                                                                             |
| `... cargo test -p bibcode-server --test production_git_manager_rpc -- --nocapture`                                                                                                                                                                                                                                                                                  | Native: run 1 (before fixture pin) 15 passed / 6 failed; run 2 (after pin, default harness) 20 passed / 1 failed: `branch_and_sync_operations_execute_through_the_streaming_adapter`, `operation stream timeout` after 15 s | 1m 54s                | Isolated `--exact` run of that test: exit 0, 12 operations in 14.82 s. Classified as a load-dependent test-deadline timeout on the 6-thread guest; see Residual risks |
| `... cargo test -p bibcode-desktop --lib backend::tests::port_probe_reports_occupied_ports_without_a_listening_socket -- --exact --nocapture`                                                                                                                                                                                                                        | Native: exit 0                                                                                                                                                                                                              | 10m 33s incl. compile | 1 passed (new bind-only probe test)                                                                                                                                   |
| `... cargo test -p bibcode-desktop --lib backend::tests::port_selection_excludes_requested_ports_and_wsl_candidates_cover_both_architectures -- --exact --nocapture`                                                                                                                                                                                                 | Native: exit 0                                                                                                                                                                                                              | 0m 43s                | 1 passed                                                                                                                                                              |
| `node scripts/run-local-vp.mjs test run scripts/run-msvc.test.mjs scripts/run-local-vp.test.mjs scripts/remove-test-firewall-rules.test.ts scripts/tauri-hardening.test.ts scripts/ci-platform-contract.test.ts apps/web/src/components/gitManager/history/GitManagerHistoryView.test.tsx apps/web/src/components/gitManager/changes/GitManagerChangesView.test.tsx` | Native: first run 97 passed / 1 failed (pre-existing host-architecture assumption in `run-msvc.test.mjs`); after pinning the test to x64, exit 0                                                                            | 20.5 s                | 98 passed                                                                                                                                                             |
| `node scripts/run-local-vp.mjs test run scripts/toolchain-contract.test.ts scripts/coverage-config.test.ts scripts/run-local-vp.test.mjs`                                                                                                                                                                                                                            | Native: exit 0                                                                                                                                                                                                              | 1.5 s                 | 22 passed                                                                                                                                                             |
| Global `C:\bibcode-validation\npm-global\vp.cmd test run packages/contracts/scripts/export-rust-rpc-fixtures.test.ts`                                                                                                                                                                                                                                                | Native: fails with `Vitest failed to find the runner`                                                                                                                                                                       |                       | Reproduces the duplicate-runtime finding                                                                                                                              |
| `node_modules\.bin\vp.CMD test run` and `node scripts/run-local-vp.mjs test run` of the same file                                                                                                                                                                                                                                                                    | Native: exit 0 each                                                                                                                                                                                                         |                       | 1 passed each                                                                                                                                                         |
| `node scripts/remove-test-firewall-rules.ts --executable <E2E exe> --dry-run`                                                                                                                                                                                                                                                                                        | Native: exit 0, no rules                                                                                                                                                                                                    | 3 s                   | Read-only query against the live firewall store                                                                                                                       |
| `node scripts/remove-test-firewall-rules.ts --executable "C:\Program Files\Coddy Studio\Coddy Studio.exe"`                                                                                                                                                                                                                                                                     | Native: exit 2, refused                                                                                                                                                                                                     |                       | Installed-location guard                                                                                                                                              |

## VCS observation evidence

- Execution host and route: Native (focused owners only; the ten-minute idle
  measurement was not repeated in this run)
- Physical repositories/worktrees/active full subscribers/passive subscribers: n/a
- Watcher health and fallback state: `git::watcher` 31/31 and the native
  watcher RPC test passed
- Automatic-fetch interval and passive-summary interval: unchanged

| Scenario                           | Signal source           | Git launches after baseline | Publication result | Evidence class |
| ---------------------------------- | ----------------------- | --------------------------- | ------------------ | -------------- |
| Idle through 59 seconds            | paused-time unit test   | 0                           | ok                 | Native (unit)  |
| 60-second safety boundary          | paused-time unit test   | 1                           | ok                 | Native (unit)  |
| Worktree/index/HEAD/refs           | native watcher RPC test | n/a                         | ok                 | Native         |
| Structured terminal exit           | unit test               | 1                           | ok                 | Native (unit)  |
| Overflow/setup unavailable         | `git::watcher` tests    | n/a                         | ok                 | Native (unit)  |
| Reconnect/hidden/reveal/focus/menu | not repeated            |                             |                    | Unavailable    |

## Git Manager evidence

- Project/environment and selected checkout: packaged E2E fixture project (WDIO)
- Environment kind: Local
- Advertised Git Manager capabilities: unchanged
- Repository shape: ordinary
- Idle interval provider/browser request evidence: `constructing_git_manager_services_starts_no_timer_or_process` passed natively
- Streaming operation event sequence and cancellation result: cancellation test is Unix-only (filtered)
- Competing catalog/Git Manager mutation and `operation-in-flight` result: covered in the passing RPC tests

Root cause of the stale-History finding, from source: at `c24fbd26` the History
view refreshed its first page only when the live signal generation changed
while the view was mounted. The Changes tab unmounts History, so after a
commit the remounted view treated the first signal it saw as its baseline and
kept the cached page until **Refresh**. Commit `56f329c8` replaced that with the
panel's refs-snapshot generation. This change adds a guard so a first page that
resolves behind the loaded generation never regresses the tip or counter, and
adds unit coverage for remount-after-commit, stale responses, failed commits not
refreshing reads, and per-repository scoping.

| Scenario                                                            | Result         | Screenshot, command, or log evidence                               | Findings and unavailable behavior                                                           |
| ------------------------------------------------------------------- | -------------- | ------------------------------------------------------------------ | ------------------------------------------------------------------------------------------- |
| Open from the project-header button and route persistence           | Unavailable    |                                                                    | Manual packaged pass not driven                                                             |
| Main checkout and linked-worktree selection                         | Unavailable    |                                                                    |                                                                                             |
| Changes, file diff, partial-stage gutter, commit/amend/undo/discard | Native (tests) | `git_manager_commit`, `production_git_manager_rpc`                 |                                                                                             |
| History paging, selected commit, and commit diff                    | Native (unit)  | `GitManagerHistoryView.test.tsx` 12 cases                          | Post-commit History without Refresh covered by unit tests; packaged screenshot not captured |
| Branch create/checkout/rename/delete and occupied-branch redirect   | Native (tests) | isolated `branch_and_sync...` pass                                 | Load timeout under default width                                                            |
| Fetch/pull/push/publish/force-with-lease states                     | Native (tests) | same                                                               | same                                                                                        |
| Native stash list, entry diff, apply/pop/drop, and merge preview    | Native (tests) | `stash_and_merge_operations...` ok                                 |                                                                                             |
| In-progress and conflicted repository presentation                  | Native (tests) | `cherry_pick_conflict_lifecycle...` ok                             |                                                                                             |
| Tag create/delete/push and all four image-diff modes                | Native (tests) | `tag_create_push_and_delete...`, image diff tests ok               |                                                                                             |
| Explicit pull-request/check refresh and no idle provider refresh    | Native (tests) | `source_control::checks::` 2 passed; provider stub tests Unix-only |                                                                                             |
| Disconnect/reconnect and one missing-capability degradation         | Unavailable    |                                                                    |                                                                                             |
| Local-only author identity and no external image source             | Unavailable    |                                                                    |                                                                                             |
| Two-project selection, filter, tab, and repository-data isolation   | Native (unit)  | "requests history only for the scoped repository"                  |                                                                                             |
| Three-project visit with two-entry least-recently-used eviction     | Unavailable    |                                                                    |                                                                                             |
| Manual idle third-party Network and rendered-image-source check     | Unavailable    |                                                                    |                                                                                             |

## Workspace and static gates

| Command                                                                                    | Result/exit code                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               | Duration                                     | Test totals or warning summary                                                                                                                 |
| ------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| `vp run test` (Vitest packages)                                                            | Native: with the root `build` script's path filters (all packages except `infra/relay`), every Vitest package passed: contracts 35 files, shared 46, client-runtime 58 (2 skipped), oxlint-plugin 6, scripts 46 (1 skipped), web 457. Two timing tests (`activityLoad` 5,000-revision budget, `measure-vcs-runtime` orphan reaping) failed once each only while Rust test targets were compiling concurrently and passed in isolation (client-runtime 713 tests)                                                                                                                                                                               | 2m–17m per attempt                           | `infra/relay` cannot run on Windows ARM64: `workerd` has no `win32 arm64` binary                                                               |
| `node scripts/run-msvc.mjs cargo test --workspace -j 2 --no-fail-fast -- --test-threads=2` | Native: 67 test binaries passed (desktop 312, server lib 1801, all Git Manager and VCS integration files); 4 failed. `remote_update_rpc` and `bibcode-updater-verifier` could not launch (`EACCES`, UAC installer detection on `*update*` names) and pass after the runner-manifest fix (2/2, 4/4). `production_worktree_catalog_rpc::dedicated_create_panel_and_retarget_resolve_workspace_authority_server_side` failed once under the workspace-wide run and passes alone and with its full file (57/57). `e2ee_ws::inbound_plaintext_capacity_backpressures_by_principal_and_releases_on_close` fails deterministically, also in isolation | 37m 27s first pass (rebuild), 11m 54s matrix | The first attempt hit a full disk (`LNK1104`/`LNK1140`) after `targetdebug` grew past 79 GB; the checkout's debug tree was deleted and rebuilt |
| `vp check`                                                                                 | Native: exit 0 (after `vp fmt` of 4 authored files and two lint-rule disable comments with reasons)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            | 10 s                                         | 2200 files formatted, 0 lint findings                                                                                                          |
| `vp run typecheck`                                                                         | Native: exit 0                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 | 1m 41s                                       | 0 errors                                                                                                                                       |
| `vp run check:contracts`                                                                   | Native: first run exit 1 (`Vitest failed to find the runner` from the inner global `vp test`, then `Failed to format RPC fixtures (exit unknown)` because the exporter spawned bare `vp` without a shell); after routing both through `scripts/run-local-vp.mjs`, exit 0                                                                                                                                                                                                                                                                                                                                                                       | 29 s                                         | `rpc_wire` 13 passed; `git diff --exit-code -- packages/contracts/fixtures` clean                                                              |
| `cargo fmt --all --check`                                                                  | Native: exit 0                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 | 8 s                                          |                                                                                                                                                |
| Relevant Clippy with `-D warnings`                                                         | Not run (the `--all-targets` check compiled every server test target with warnings denied)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |                                              |                                                                                                                                                |
| `git diff --check`                                                                         | Native: exit 0                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |                                              |                                                                                                                                                |

## Native package artifacts

| Kind                                    | Artifact                        | Absolute path                                                                                                                                                                        | Version/architecture                                                      | Identity/trust verification                                                                                                       |
| --------------------------------------- | ------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------- |
| NSIS installer                          | `Coddy Studio_0.4.2_arm64-setup.exe` | `C:\Users\admin\Projects\GitHub\BibCode\release\desktop\win-arm64\Coddy Studio_0.4.2_arm64-setup.exe`                                                                                     | 0.4.2; PE machine x86 (I386) NSIS bootstrapper carrying the ARM64 payload | 12,887,779 bytes; SHA-256 `92F1B18355B2E7619F328A5692A2FA948766F9F025F8DA3AD1EE8587335113D3`; Authenticode `NotSigned` (expected) |
| Production application                  | `bibcode-desktop.exe`           | `C:\Users\admin\Projects\GitHub\BibCode\release\desktop\win-arm64-app\bibcode-desktop.exe` (copied from `target\aarch64-pc-windows-msvc\release\` before the E2E build overwrote it) | 0.4.2; PE machine ARM64                                                   | 31,261,184 bytes; SHA-256 `855F58A930B4A39C3FA8090375C052E53796B0CB580A96E8AD883F413741246E`; `NotSigned`                         |
| E2E application (`desktop-e2e` feature) | `bibcode-desktop.exe`           | `C:\Users\admin\Projects\GitHub\BibCode\target\aarch64-pc-windows-msvc\release\bibcode-desktop.exe`                                                                                  | ARM64                                                                     | SHA-256 `80EA7D6DEB1975B8E0FB8F067212A50702D1C2024D99F7BA3E439B972207741A`                                                        |

Build commands: `vp run dist:desktop:win:arm64` (exit 0, 17m 05s; Rust
release phase 15m 50s; NSIS 3.11 and `nsis_tauri_utils` 0.5.3 downloaded into
`target/.tauri`), then `BIBCODE_E2E_PLATFORM=win BIBCODE_E2E_ARCH=arm64 vp run
test:ui:desktop:build` (exit 0, 14m 07s).

## Standalone server distribution evidence

- Not in scope for this remediation; unchanged.

## Packaged UI and visual evidence

`vp run test:ui:desktop` with `BIBCODE_E2E_APP_PATH` set to the E2E executable:
exit 0 in 2m 48s; WDIO reported 6 passing in one worker
(`Spec Files: 1 passed, 1 total`), artifacts under
`C:\Users\admin\AppData\Local\Temp\bibcode-desktop-ui-artifacts-WDZWk7`.

| Scenario                                                                                                    | Screenshot absolute path | State       | Pixel-review finding                    |
| ----------------------------------------------------------------------------------------------------------- | ------------------------ | ----------- | --------------------------------------- |
| `main-window.e2e.ts`: adds a Git project through Browse folder and persists it after restart                | WDIO artifact dir        | passed      | not reviewed                            |
| `project-session-terminal.e2e.ts`: streams a fixture response, reconnects, and exercises terminal lifecycle | WDIO artifact dir        | passed      | not reviewed                            |
| `platform-capabilities.e2e.ts`: settings, updater-disabled state, provider shims, and openers               | WDIO artifact dir        | passed      | not reviewed                            |
| `terminal-font.e2e.ts`: bundled Nerd glyphs and device-local preset                                         | WDIO artifact dir        | passed      | not reviewed                            |
| `composer-native-triggers.e2e.ts`: native composer triggers                                                 | WDIO artifact dir        | passed      | not reviewed                            |
| `chat-activity-panel.e2e.ts`: responsive activity experience                                                | WDIO artifact dir        | passed      | not reviewed                            |
| Manual post-commit History without Refresh                                                                  | not captured             | Unavailable | No Computer Use surface in this session |

- Exact executable launched: the E2E executable above (SHA-256 `80EA7D6D...`)
- Exact PID/start identity: recorded by WDIO in the artifact directory
- Other installed or development copies excluded: no installed Coddy Studio present; production binary preserved separately
- External tool, command, and path used for the Files Refresh rescan: n/a
- Authentication-dependent scenarios unavailable: GitHub CLI unauthenticated (unchanged)

## External-worktree scenario

- Not exercised in this remediation.

## Process and temporary-root cleanup

- Before snapshot: no `bibcode-desktop`, `msedgedriver`, or WDIO `node` processes
- After snapshot: `Get-Process bibcode-desktop, msedgedriver` and the WDIO
  `node` command-line filter returned nothing after the E2E run
- Scoped surviving processes: none
- New test-owned roots: WDIO artifact directory above; `target/`,
  `release/desktop/win-arm64`, `release/desktop/win-arm64-app` in the checkout
  (all git-ignored)
- Pre-existing roots/processes intentionally left untouched: `C:\bibcode-validation`
  and every retained toolchain component
- Package mounts or platform resources released: none used
- Firewall: `Get-NetFirewallApplicationFilter | Where-Object Program -like '*bibcode*'`
  returned no rules for any Coddy Studio executable after the packaged runs; no
  Windows Security Alert appeared for either freshly built executable

## Non-native compatibility evidence

### Linux and macOS

- Evidence class: Compatibility evidence
- Source/contracts reviewed: the bind-only probe keeps `SO_REUSEADDR` on Unix
  to match `std::net::TcpListener`; `cfg(unix)` tests are unchanged in
  behavior; `useLocalToolsDir` for Windows mirrors `tauri.linux.conf.json`;
  the launcher-routed package scripts execute the same local `vite-plus` bin
  on every platform
- Commands and results: none
- Native-only evidence still required: `cargo test -p bibcode-desktop` on
  Linux and macOS for the new probe test; `vp run test` on Linux to confirm
  the launcher-routed scripts under the CI setup-vp installation

## Source changes and commits created

- Files changed:
  - `apps/server/src/git/manager/mod.rs`, `apps/server/src/source_control/checks.rs`,
    `apps/server/tests/production_git_manager_rpc.rs`: Unix-only imports,
    helpers, and constants scoped to their `cfg(unix)` consumers.
  - `apps/server/src/git/manager/operations.rs`, `apps/server/src/git/repository.rs`
    (`git_manager_undo_restores_deleted_files_from_an_initial_commit`),
    `apps/server/tests/production_git_manager_rpc.rs`,
    `apps/server/tests/git_manager_commit.rs`: fixtures pin
    `core.autocrlf=false` after `git init` (Git for Windows defaults to `true`).
  - `.github/workflows/ci.yml`, `scripts/ci-platform-contract.test.ts`,
    `docs/operations/ci.md`: Windows native rows run
    `cargo check -p bibcode-server --all-targets`.
  - `apps/web/src/components/gitManager/history/GitManagerHistoryView.tsx` and
    test, `apps/web/src/components/gitManager/changes/GitManagerChangesView.test.tsx`:
    stale-generation guard plus five new cases.
  - `apps/desktop/src-tauri/src/backend.rs`, `apps/desktop/src-tauri/Cargo.toml`,
    `Cargo.toml`, `Cargo.lock`: port probe binds without listening via
    `socket2`, with a unit test.
  - `apps/desktop/src-tauri/tauri.windows.conf.json`,
    `scripts/tauri-hardening.test.ts`: NSIS tools cached under `target/.tauri`.
  - `scripts/run-msvc.mjs` and test: SYSTEM-profile packaging preflight (exit
    3 before any build); the wrapper test pins x64 instead of assuming the host.
  - `scripts/run-windows-cargo-target.mjs` and test: the sidecar manifest now
    declares `requestedExecutionLevel asInvoker` and the runner touches the
    binary so Windows re-reads it, which lets UAC installer-detected names such
    as `remote_update_rpc-*.exe` and `bibcode_updater_verifier-*.exe` start.
  - `scripts/run-local-vp.mjs` and test: checkout-local Vite+ launcher.
  - `package.json`, `apps/web/package.json`, `infra/relay/package.json`,
    `packages/*/package.json`, `oxlint-plugin-bibcode/package.json`,
    `scripts/package.json`, `scripts/coverage-config.test.ts`,
    `scripts/toolchain-contract.test.ts`: every Vitest package script,
    `check:contracts`, and `test:coverage:ts` route through the launcher, with
    a contract test.
  - `packages/contracts/scripts/export-rust-rpc-fixtures.ts` and test: the
    fixture formatter spawns the launcher through Node instead of bare `vp`.
  - `scripts/remove-test-firewall-rules.ts` and test: exact-executable
    firewall rule cleanup with verification.
  - `docs/dependency-upgrades/2026-07-17-ledger.json`: `socket2` ledger entry and
    registry count, required by the dependency-ledger test.
  - `scripts/install-nfpm.test.ts`: expected paths built with `NodePath.join` so the
    assertion holds on Windows separators.
  - `scripts/smoke-server-distribution.test.ts`: the POSIX shebang-fixture case is
    skipped on native Windows with the reason stated in the test.
  - `docs/testing/windows-desktop.md`, `docs/testing/cross-platform-validation.md`,
    `docs/reference/scripts.md`: Parallels preflight, launcher usage, NSIS
    cache location, autocrlf fixture rule, firewall cleanup, Computer Use
    windowed-mode step, post-commit History evidence.
- Behavioral reason: findings 1 through 6 of the Windows ARM64 request plus
  the three additional Windows-only failures discovered while running the gates
  (Unix-only constant, CRLF fixtures, bare `vp` spawn in the fixture exporter).
- RED evidence: first-run exit codes recorded in the tables above.
- GREEN evidence: second-run exit codes recorded in the tables above.
- Local commits: none created.

## Commands not run

| Command or scenario                                                                                                        | Reason                                                                                                                                                                                                                                                              | Required follow-up owner                 |
| -------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------- |
| Manual packaged regression (add project via native picker, commit, open History without Refresh, screenshots before/after) | No Computer Use surface available in this session; the behavior is covered by unit tests and the History component contract only                                                                                                                                    | Operator with Parallels in Windowed mode |
| `cargo clippy --workspace --all-targets -- -D warnings` after `cargo clean`                                                | Time; the `--all-targets` check with `-D warnings` compiled every server test target                                                                                                                                                                                | Next full gate run                       |
| `production_git_manager_rpc` at 8 and 12 harness threads                                                                   | Not run                                                                                                                                                                                                                                                             | Next full gate run                       |
| `infra/relay` Vitest suites inside `vp run test`                                                                           | Cloudflare `workerd` has no `win32 arm64` binary (`Unsupported platform: win32 arm64 LE`); every relay test file fails at load and `vp run` cancels the Rust suites, so the broad gate ran with the root `build` script's path filters, which exclude `infra/relay` | Linux and macOS rows                     |

## Residual risks

- Risk: `e2ee_ws::inbound_plaintext_capacity_backpressures_by_principal_and_releases_on_close`
  fails on native Windows ARM64 both under the workspace run and alone
  (`principal pressure must backpressure without closing the waiting socket`).
  - Impact: a Windows-specific E2EE backpressure discrepancy or a
    timing-sensitive assertion; it is outside the Git Manager scope and was not
    observable before because the server test crate did not compile on Windows.
  - Evidence that bounds it: the other 22 `e2ee_ws` tests pass; no E2EE code
    changed in this remediation.
  - Required follow-up: the E2EE owner reproduces on `windows-11-vs2026-arm`
    and decides whether the 100 ms silence window or the principal accounting
    is at fault.
- Risk: `branch_and_sync_operations_execute_through_the_streaming_adapter`
  exceeds its own 15-second `collect_events` deadline under the default
  harness width on this 6-thread guest while passing alone in 14.8 s.
  - Impact: the production RPC file is red under load on small Windows VMs.
  - Evidence that bounds it: 20 of 21 tests pass under load; the isolated run
    completes all twelve operations; no production deadline is involved.
  - Required follow-up: run the file on `windows-11-vs2026-arm` CI hardware;
    if it reproduces there, treat it as a product performance finding for the
    branch and sync operations rather than widening the test deadline.
- Risk: the firewall-prompt attribution rests on Windows raising its alert for
  listening sockets on wildcard addresses.
  - Impact: none observed; no prompt and no rules appeared for either freshly
    built executable in this run.
  - Evidence that bounds it: post-run firewall query returned zero Coddy Studio
    rules; `portpicker::pick_unused_port` remains a last-resort fallback that
    still listens on wildcard TCP and UDP.
  - Required follow-up: none unless a prompt reappears.
- Risk: `useLocalToolsDir` relocates the NSIS download for every Windows build,
  including CI.
  - Impact: first CI build after this change downloads NSIS into `target/.tauri`.
  - Evidence that bounds it: identical to the existing Linux AppImage setup;
    the download succeeded here in seconds.
  - Required follow-up: observe one Windows CI run.
- Risk: package `test` scripts now depend on `scripts/run-local-vp.mjs`.
  - Impact: a checkout without installed dependencies fails with exit 3 and an
    install instruction instead of falling back to a global `vp`.
  - Evidence that bounds it: launcher unit tests and the toolchain contract
    test; `vp run check:contracts` and the focused suites passed through it.
  - Required follow-up: observe one Linux CI run.

## Publication state

- Commits created: none
- Pushed: no
- Branch merged: no
- Pull request opened: no
- Artifacts published: no (installer left under the git-ignored `release/` directory)

## Addendum: broad gates

`vp run test` cannot pass as a single command on Windows ARM64 because the
relay package's Vitest suites fail at load (no `workerd` binary) and `vp run`
then cancels the Rust suites. The Vitest packages were therefore run through the
root `build` script's path filters and the Rust suites through the documented
native equivalent `cargo test --workspace -j 2 -- --test-threads=2`; both rows
above record the complete matrices.
