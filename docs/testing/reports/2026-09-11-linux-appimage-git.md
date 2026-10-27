# Linux Git/AppImage validation — 2026-09-11

**Result:** PASS WITH RESIDUAL RISKS — Git compatibility checks pass on all six
container targets; packaged desktop qualification remains separate.

## Tested revision and scope

- Repository: Coddy Studio, isolated worktree `.worktrees/linux-appimage-git`.
- Branch: `codex/linux-appimage-git`.
- Base HEAD: `5423cf16b291673c756cc28ebade9a973e567140`, plus the uncommitted patch.
- The original `develop` checkout's staged documentation and untracked outputs
  were preserved. No version bump, commit, push, merge, PR, or release was made.
- Scope: prevent AppImage libraries from breaking system Git on Debian,
  Ubuntu, Fedora, and Arch. This report qualifies the Git subprocess change,
  not complete desktop distribution support.

The Git runner was identical between the inspected local `develop` and `main`
refs before the patch. The diagnostic AppImage reported version 0.5.8; test
builds use the checked-out source manifests (0.5.1), not a new packaged release.

## Environments

- Native failure/retest host: Fedora 44 x64, kernel
  `7.1.12-200.fc44.x86_64`, reached through the `fedora-bibcode` SSH alias.
- Local checks: macOS 26.6.2, build 25G83, ARM64.
- Repository Rust toolchain: 1.97.1. The Mac's default Homebrew Rust/Clippy was
  1.98.1/0.1.98, so the final Clippy check explicitly prepended the pinned
  toolchain's `bin` directory to `PATH`.
- Compatibility executable: built with Rust 1.97.1 inside Ubuntu 22.04,
  `CARGO_PROFILE_TEST_DEBUG=0`, then run in native x64 Podman containers on
  the Fedora host. No emulation or host package installation was used.

## Diagnosis and real-repository check

The running AppImage supplied a library path containing its current mount and
several earlier AppImage mounts. Under that environment, system Git's HTTPS
helper loaded Fedora's `libcurl` with the bundled `libnghttp2`; the latter did
not export `nghttp2_option_set_no_rfc9113_leading_and_trailing_ws_validation`.

- The helper reproduced the exact symbol failure twice, exit 127.
- `git ls-remote --exit-code origin HEAD` on the affected main checkout failed
  with exit 128 under the AppImage environment and passed with exit 0 when
  `LD_LIBRARY_PATH` alone was removed.
- A temporary executable using the patched production `git::ProcessRunner`
  inherited the actual AppImage environment and ran
  `git fetch --dry-run --no-write-fetch-head --prune --recurse-submodules=on-demand origin`.
  It exited 0 with no loader error. Hashes of refs and `FETCH_HEAD` matched
  before and after. The installed app was not replaced or restarted.

## Regression and review evidence

The regression compiles an incompatible library with the soname actually used
by the distribution's Git HTTPS helper, then executes the real production
runner in an isolated process inheriting AppImage variables. It covers:

- extracted AppImages and paths containing spaces;
- current and stale mount paths;
- text and binary output;
- preservation of custom directories and the SSH agent environment;
- explicit command overrides and ordinary non-AppImage launches;
- colon/semicolon separators and a surviving current-directory entry;
- unchanged parent environment.

The original runner failed the regression with exit 127. The initial filter
passed, then additional tests exposed loss of semicolon-separated custom paths
and the lone empty/current-directory entry. Both went red before their fixes.
The latter was also identified by the independent read-only review.

The first matrix run exposed a fixture limitation on Debian: requesting usage
could return before curl initialization. The final fixture sends a valid
`capabilities` request and a terminating blank line, forcing initialization
without an HTTP request. The first Debian failures were fixture failures;
the final results below use the corrected executable.

## Distribution matrix

Command: `CONTAINER_ENGINE=podman bash scripts/test-linux-git-compatibility.sh TEST_BINARY IMAGE`.
Each row runs `git_subprocesses_ignore_appimage_libraries`; the driver also
verifies that this test exists in the executable before running it.

| Distribution/image                     | System Git | Result       |
| -------------------------------------- | ---------- | ------------ |
| `docker.io/library/debian:12-slim`     | 2.39.5     | PASS, exit 0 |
| `docker.io/library/debian:13-slim`     | 2.47.3     | PASS, exit 0 |
| `docker.io/library/ubuntu:22.04`       | 2.34.1     | PASS, exit 0 |
| `docker.io/library/ubuntu:24.04`       | 2.43.0     | PASS, exit 0 |
| `registry.fedoraproject.org/fedora:44` | 2.55.0     | PASS, exit 0 |
| `docker.io/library/archlinux:base`     | 2.55.0     | PASS, exit 0 |

Arch reported rolling image version `20260906.0.587075`. These are observed
versions, not claims about every historical or future release of each distro.

## Workspace and static gates

| Command/check                                                                                                                 | Result                       |
| ----------------------------------------------------------------------------------------------------------------------------- | ---------------------------- |
| `cargo test --locked -p bibcode-server --test git_rpc -j 2` on macOS                                                          | PASS, 43 tests               |
| Existing `git::process` parallel-child unit test on macOS                                                                     | PASS, 1 test                 |
| Final `cargo test --offline --locked -p bibcode-server --test linux_appimage_git_environment -j 2 -- --nocapture` on Fedora   | PASS, 1 test                 |
| `vp test run scripts/ci-platform-contract.test.ts`                                                                            | PASS, 29 tests               |
| `vp check`                                                                                                                    | PASS                         |
| `vp run typecheck`                                                                                                            | PASS, all 11 workspace tasks |
| Pinned-toolchain `cargo fmt --all --check`                                                                                    | PASS                         |
| Pinned-toolchain `cargo clippy --locked -p bibcode-server --all-targets -j 2 -- -D warnings` on macOS                         | PASS                         |
| `cargo clippy --offline --locked -p bibcode-server --lib --test linux_appimage_git_environment -j 2 -- -D warnings` on Fedora | PASS                         |
| `bash -n scripts/test-linux-git-compatibility.sh`, parsed workflow shell blocks, `git diff --check`                           | PASS                         |

The initial Homebrew Clippy run reported an existing `chunks_exact_to_as_chunks`
lint in unchanged `git/manager/conflicts.rs`; using the repository's pinned
Clippy resolved that mismatch. The baseline macOS unit-test link also emitted
an existing large `__eh_frame` warning. Neither required unrelated source edits.

## Source identity

Local and Fedora test-workspace SHA-256 values matched:

- `apps/server/src/git/process.rs`:
  `9db9f4f970e538b1794bfd287bef325db7e51ecfc4591f6e2b2c323520264e02`.
- `apps/server/tests/linux_appimage_git_environment.rs`:
  `2ed863d303486e188cdbab9ed4d715ed7771f605c15d275b7afc4193686d4aae`.

## Cleanup and remaining qualification

All test containers exited and were removed. The temporary Fedora build
workspace, including the diagnostic example source, and the task-owned builder
image were removed. The original AppImage process remained running. Normal
Cargo caches and downloaded distribution images were retained.

Raw first-run and final (`*-v2.log`) matrix logs were retained locally under
`/Users/admin/.codex/visualizations/2026/09/11/01a08f78-30eb-78d3-8230-65c19fc6bdf3/linux-git-validation/`.
The source patch remains in its isolated local worktree for review.

No packaged AppImage was built, installed, or published. Full packaged Git
Manager UI, HTTPS-fetch, desktop/updater validation on each distribution, and
native ARM64 validation remain release-qualification work. The Git regression
does not establish those results. No full workspace test suite was run;
focused Git integration, CI contracts, and workspace static gates were selected
for this change. The Linux runbook, CI reference, script reference, and runtime
architecture were updated with the new policy and validation scope.
