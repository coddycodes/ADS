# CI Quality Gates

`.github/workflows/ci.yml` runs on pull requests and pushes to `main`. It has
five job groups.

CI and Seeded Packaged Desktop Upgrade each set
`concurrency.group` to `${{ github.workflow }}-${{ github.ref }}` with
`cancel-in-progress: true`. A later push to the same branch or pull request
cancels that workflow's older run, so a burst of merges does not leave a
native matrix queued for every superseded commit. GitHub keeps a workflow
run's status at `queued` until every job has left the queue. One macOS job
still waiting for `macos-26` or `macos-26-intel` therefore lists the whole
run as queued after Check, Test, and the other native rows have already
started. Inspect the jobs, not only the run status, before treating the
workflow as idle.

- **Check** runs `vp check` (which fails on any lint warning), workspace
  typechecking (`vpr typecheck`),
  `cargo fmt --all --check`, Clippy with warnings denied, and the complete
  desktop build pipeline on Ubuntu 24.04. A separate
  `cargo check -p bibcode-server --lib --bins -j 2` also checks production
  feature wiring without dev units: all-targets Clippy enables the hermetic
  test guard, while ordinary server builds do not.
  It also runs `node --test scripts/check-agent-delivery.node-test.mjs` to verify
  rejection of stale or partial proof, duplicate runs, unchanged failed recipes,
  and repeated rounds without a revised diagnosis.
  Its 40-minute job budget is the projected cold-cache desktop build (33
  minutes, from the 26-minute cold Check on `b96fe993` and the longer compile
  on pull request 65) plus 7 minutes of margin.
- **Test** runs every workspace package `test` script one task at a time with
  `vp run -r --concurrency-limit 1 test`, then runs `cargo test --workspace -j 2`
  explicitly on Ubuntu 24.04. Serial tasks keep `rustc` from competing with a
  running server or desktop suite, whose 2-second test deadlines have failed
  on starved hosted runners. Its 75-minute job budget is a completed cold Test
  (44 minutes on `438136c0`) plus the 21-minute apt stall seen on pull request
  65 and 10 minutes for the workspace suite and SSH step that were still
  running when the old 60-minute limit cancelled the job. Test-owned deadlines
  stay unchanged. The `-j 2` bound limits concurrent Cargo compilation jobs; Rust
  test binaries use the default parallel harness threads. Exact subprocess
  tests may still select `--test-threads=1` inside an isolated child process
  that intentionally owns process-global state.
  The job then builds the guarded `bibcode` through
  `cargo test -p bibcode-server --test cli_smoke --no-run -j 2`
  and runs the ignored desktop SSH integration test
  (`cargo test -p bibcode-desktop --test ssh_environment -- --ignored`), which
  needs that fresh guarded binary and fakes only the SSH hop; see
  [Desktop-managed SSH environments](../testing/ssh-environments.md).
- **Release Smoke** runs `scripts/release-smoke.ts` to exercise release-only
  version rewriting, nightly metadata, and lockfile generation without
  publishing.
- **Native desktop** builds the web application, tests the desktop Rust host,
  and creates an unpublished native bundle on Linux ARM64/x64, Windows ARM64/x64,
  and macOS ARM64/x64 runners. Each matrix row declares its complete-job budget:
  240 minutes for macOS Intel, and 120 minutes for the other five targets.
  That budget includes setup, host tests, the macOS optimized exception-recovery
  probe, and native packaging without shortening any check or test-owned deadline.
  Debug host tests cannot warm the optimized artifacts; the optimized example
  also enables dev-dependency features (`tauri/test` and `tokio/test-util`) that
  the ordinary production bundle does not use. Both optimized commands select
  the same native target triple, but their feature differences require separate
  compilation. Keep the real exception probe: a larger complete-job allowance
  accommodates these deliberate checks without treating a cached build as the
  cold-build baseline. The shared `scripts/run-msvc.mjs` launcher selects
  the requested MSVC architecture. After the Rust host tests,
  the Windows row alone runs
  `vp test run apps/desktop/e2e/support/test-project.test.ts apps/web/src/components/gitManager/gitManagerRepositoryAvailability.test.ts`. That step is the
  supported native proof that the generated Cursor `.cmd` shim executes through
  the Windows command processor and writes its exact action record. Simulated
  target fixture assertions on other hosts are compatibility evidence, not a
  native Windows pass.
  The repository-availability contract additionally executes the generated
  drive/UNC trust commands through native Windows PowerShell and Git, including
  spaces, dollar signs, backticks, and typographic quotes. Every Git/home
  configuration input is fixture-owned. Other platforms skip that native case;
  their string assertions remain compatibility evidence.
  The Windows rows then run
  `node scripts/run-msvc.mjs cargo check -p bibcode-server --all-targets` so
  Unix-only test helpers or imports that are unused on Windows fail there under
  `-D warnings` instead of surfacing only during native validation; Clippy's
  `--all-targets` pass otherwise runs on Linux alone.
  The repository-availability contract additionally executes the generated
  drive/UNC trust commands through native Windows PowerShell and Git, including
  spaces, dollar signs, backticks, and typographic quotes. Every Git/home
  configuration input is fixture-owned. Other platforms skip that native case;
  their string assertions remain compatibility evidence.
- **Crash smoke** runs on `ubuntu-latest`, `windows-latest`, and
  `macos-latest`. It builds with the release symbol profile from
  [Symbol assets](./release.md#symbol-assets) and runs
  `cargo test -p bibcode-server --test crash_smoke --release` with
  `CRASH_SMOKE_OUT` set to `${{ runner.temp }}/crash-smoke`, so the minidump that
  the `crash-smoke` binary leaves under the crash monitor survives the test. The
  test treats that directory as a data root (the dump lands in
  `userdata/profiler/crashes/`), and `bibcode profiler symbolicate <id> --json`
  must read the dump's crash reason and crashing-thread frames.
  `scripts/dump-symbols.ts` then dumps that binary (with its dSYM on macOS and
  its PDB on Windows), and `minidump-stackwalk --json` must name
  `crash_here_in_named_function`, with its caller unwound through CFI. The
  release build's own
  `bibcode profiler symbolicate <id> --symbols <store> --json` must name it too,
  from the same store, so BiBCode's symbolication is proven on every OS and not
  only the third-party walker. `dump_syms` 2.3.9 and `minidump-stackwalk`
  0.27.0 are installed with `cargo install --locked`, and Rust Cache keeps them
  in `~/.cargo/bin`. A failed run uploads the dump, its sidecar, the symbol
  store, and the stack walk as the `crash-smoke-<os>` artifact. The Windows
  ARM64 dump path stays disabled, so the job has no ARM64 rows.

The Check and Test jobs install the Linux libraries required by Tauri. The
native matrix installs them only on Linux and otherwise uses each platform's
native toolchain. Node.js and Vite+ are development/build dependencies; release
artifacts contain the Tauri/Rust application and built web assets, not a Node
runtime or TypeScript server.

All workflows use the repository's declared Node.js 26.8.1, pnpm 11.25.0,
Vite+ 0.3.0, and Rust 1.98.0 toolchains. External setup actions are immutable
SHA-pinned with audited tag comments: Checkout 7.0.1
(`3d3c42e5aac5ba805825da76410c181273ba90b1`), Rust Cache 2.9.2
(`6323deb102c322ba6fcbdcafc7e3dddab59af2b6`), setup-vp 1.18.0
(`1b32467adbe183473499fd9d5d372c3ed9641754`), action-gh-release 3.0.3
(`efb35369e0ad2afab669f228072c1b0d510eae64`), and Rust toolchain 1.98.0
(`62ae3a85dbdd2bedbb5819da8ce45635129289a1`). Reverify a tag before changing
its immutable SHA.

Check, Test, Native desktop, and Crash smoke restore the Swatinem Rust cache on every run
and save it only when `github.ref` is `refs/heads/main`, including when a later
step fails (`cache-on-failure`). Pull requests read that default-branch cache.
The pnpm store stays on the setup-vp cache. A job cancelled by
`timeout-minutes` still skips the cache post step, so the Check and Test
budgets above include a cold compile. Native desktop keeps its 120-minute and
240-minute row budgets. Release workflows keep their own cache settings.

## Other Workflows

- `.github/workflows/linux-git-compatibility.yml` builds the real server Git
  runner regression test on Ubuntu 22.04, then executes it against system Git
  in Debian 12/13, Ubuntu 22.04/24.04, Fedora 44, and Arch rolling containers.
  It verifies AppImage library isolation for text and binary subprocesses,
  including inherited mounts from previous updates. This is Git compatibility
  evidence; native packaged desktop and updater validation remain separate.
- `.github/workflows/desktop-ui-smoke.yml` is a manual or reusable packaged-app
  UI smoke matrix for all six supported native targets.
- `.github/workflows/desktop-upgrade-smoke.yml` runs real seeded updater flows on
  all six targets, including the isolated `remote-install` lane. That lane uses
  the product coordinator, verifies boot/version/storage/protection evidence,
  and scopes Linux process cleanup to its exact data root. The harness rejects
  local invocation; unit/syntax checks do not constitute real upgrade evidence.
  The seeded macOS Intel row has a 360-minute complete-job allowance for three
  cold, isolated 90-minute packaging children, three 10-minute frozen installs,
  and 60 minutes for setup, runtime verification, evidence, and cleanup.
  The other five rows and the separate WSL job keep 240 minutes; their packaging
  children keep 45 minutes. If owned-child cleanup does not observe `close`
  within five seconds, the harness releases that child and exits nonzero so the
  job cannot stay open for the rest of the 240-minute budget.
  `remote-install` reuses the protected package.
  Repository `target` caching cannot warm the run root's three Cargo outputs.
  These packaging allowances are separate from the native desktop CI job above
  and preserve all existing product, WebDriver, and restart deadlines.
  Windows lanes wait for the exact candidate's installed PE version before
  stopping and relaunching the host, retaining bounded handoff observations.
  Its WSL-specific lane remains Windows x64.
- `.github/workflows/release.yml` runs the stable/nightly release pipeline. See
  the [Release Checklist](./release.md). Its separate server matrix builds six
  archives, four Linux packages, and native distribution evidence before release
  assembly can create a draft. Both matrices also publish Breakpad
  [symbol archives](./release.md#symbol-assets).
- `.github/workflows/deploy-relay.yml` deploys the BiBCode Connect relay from
  `main` only when the required Cloudflare repository configuration exists.
- `.github/workflows/issue-labels.yml`, `pr-size.yml`, and `pr-vouch.yml` enforce
  repository-maintenance policy independently of the application quality
  gates.

When changing a workflow, update its focused workflow-contract tests and run
the repository gates documented in [Scripts](../reference/scripts.md).
Repeatable native manual and packaged validation follows the
[shared cross-platform runbook](../testing/cross-platform-validation.md) plus
the matching Windows, Linux, or macOS page in the
[testing runbook index](../testing/README.md).
