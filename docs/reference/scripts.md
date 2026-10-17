# Scripts

Run JavaScript/TypeScript workspace commands through Vite+ (`vp`). Run commands
from the repository root unless noted otherwise.

The declared development toolchain is Node.js 26.8.1, pnpm 11.25.0, Rust
1.98.0, and Vite+ 0.3.0. The root workspace and application packages use
TypeScript 7.0.2. `apps/marketing` remains a TypeScript 6.0.3 compatibility
island until Astro's checker supports TypeScript 7 through its public API.

## Setup And Development

- `vp install`: install workspace dependencies.
- `vp run dev`: start the native server and React development graph.
- `vp run dev:server`: start only the Rust WebSocket/HTTP server.
- `vp run dev:web`: start only the Vite frontend.
- `vp run dev:desktop`: start the Tauri 2 desktop app with frontend HMR.
- `vp run dev:marketing`: start the Astro marketing site.
- `vp run start`: run the native `bibcode` CLI through its package script.
- `vp run start:desktop`: alias the Tauri development command.
- `vp run start:marketing`: preview the built marketing site.
- `vp run start:mock-update-server`: run the local desktop-updater fixture
  server.
- `cargo run -p bibcode-server -- serve`: run the native headless server
  directly from this checkout.

`scripts/dev-runner.ts` uses server/web ports `13773` and `5733` by default.
Set `BIBCODE_DEV_INSTANCE` to shift both deterministically, or set
`BIBCODE_PORT_OFFSET` to an explicit numeric offset. These are development-runner
defaults; the standalone native server's default port is `3773`.

The desktop development runner defaults `BIBCODE_HOME` to `~/.bibcode`. Debug
builds use its `dev` state kind while installed builds use `userdata`; a dev
launch does not make either store disposable. Desktop Rust tests use explicit
temporary roots and fail closed if a Tauri mock attempts to resolve the real
default user root, so `cargo test -p bibcode-desktop` must not mutate either
state kind.

## Build And Quality

- `vp run build`: build application, package, lint-plugin, and script outputs.
- `vp run build:desktop`: build the React assets, Rust backend, and Tauri app.
  On Linux, the desktop build launcher scopes `NO_STRIP=1` to the Tauri child
  process so linuxdeploy does not invoke its older bundled `strip` against
  modern RELR-enabled system libraries; Windows and macOS inherit the caller's
  environment unchanged.
- `vp run build:marketing`: build the Astro marketing site.
- `vp run check:contracts`: typecheck the schema-only contracts package, verify
  deterministic RPC fixture export, regenerate the fixtures, run the
  TypeScript/Rust RPC parity and Rust fixture round-trip checks, and finally
  fail if the regenerated fixtures differ from the committed tree
  (`git diff --exit-code --stat -- packages/contracts/fixtures`). A contract
  change must therefore commit its regenerated fixtures. It does not create a
  distributable contracts build.
- `vp check`: run the Vite+ formatting and lint checks. Any lint warning fails
  it (`lint.options.denyWarnings` in `vite.config.shared.ts`).
- `vp run fmt` / `vp run fmt:check`: write or verify Vite+ formatting.
- `vp run lint`: run Vite+ linting with unused-disable reporting.
- `vp run typecheck` (or `vp run tc`): run package TypeScript and Rust checks.
- `vp test`: run the built-in Vite+ unit test command.
- `vp run test`: run every package `test` script, including Rust packages.
- `vp run test:desktop`: run the Tauri Rust test suite.
- `vp run test:ui:desktop`: run packaged desktop WebdriverIO tests.
- `vp run test:ui:desktop:build`: build the packaged app used by desktop UI
  tests.
- `vp run test:coverage`: run TypeScript and Rust coverage gates.
- `vp run test:coverage:ts` / `vp run test:coverage:rust`: run one coverage
  side independently.
- `vp run release:smoke`: exercise release versioning and lockfile generation.
- `vp run check:dependency-ledger`: validate the machine-consumed dependency
  upgrade ledger.

Use `vp test` for the built-in Vite+ test command. Use `vp run test` when the
workspace package-script graph is specifically required. The graph keeps
package test tasks concurrent, while server and desktop Rust test commands use
the default parallel harness threads with Cargo compilation bounded by `-j 2`.
CI and release verification use `vp run -r --concurrency-limit 1 test` to run
one package task at a time. The direct recursive invocation is required:
placing the limit on the root `test` wrapper leaves its nested graph concurrent.
On Windows the launcher runs every Cargo test target through
`scripts/run-windows-cargo-target.mjs`, which writes a sidecar manifest next to
the binary declaring the Common Controls v6 dependency and
`requestedExecutionLevel asInvoker`, then touches the binary so Windows
re-reads the manifest. Rust test binaries have no embedded manifest, so without
the `asInvoker` declaration UAC installer detection refuses to start any target
whose name contains `install`, `setup`, `update`, or `patch` (for example the
`remote_update_rpc` and `bibcode-updater-verifier` tests) with
"The requested operation requires elevation", which Node reports as `EACCES`.
When an explicit `CARGO_TARGET_DIR` is supplied to a Cargo test command, the
shared Cargo launcher resolves it from the launch directory, creates it, and
passes its native canonical filesystem path to Cargo. This gives each platform
one unambiguous test-binary identity and keeps Tauri's secured starting-binary
lookup valid when an isolated target was requested through an alias such as
macOS `/tmp`; non-test commands and implicit Cargo targets are unchanged.
Exact subprocess tests may use `--test-threads=1` only inside an isolated child
process that intentionally owns process-global state.

The two entry points compile web tests differently. `vp run test` runs each
package's own configuration; `apps/web`'s applies the React Compiler to Vite's
client environment, so web tests marked `// @vitest-environment happy-dom` run
compiled, as in the client build. Node-environment web tests run in Vite's `ssr`
environment, which the compiler preset skips, so they are uncompiled. Root
`vp test` uses the root configuration, which runs no React plugins, so no web
test is compiled there, and `vp run test:coverage:ts` measures source rather
than the compiler's generated memoization code.
`apps/web/src/reactCompiler.test.tsx` guards the compiled lane and is excluded
from root runs; see
[Web unit tests and the React Compiler](../testing/README.md#web-unit-tests-and-the-react-compiler).

`node scripts/run-local-vp.mjs <vp arguments>` runs the checkout-local Vite+
installation from `node_modules/vite-plus` with the current Node, ignoring any
globally installed `vp`. Use it wherever a global `vp` might shadow the
workspace copy (native Windows validation, Parallels `prlctl exec
--current-user`), because a second Vitest runtime fails with `Vitest failed to
find the runner` before collecting tests. The launcher exits with code 3 and an
install instruction when workspace dependencies are missing, and otherwise
forwards the child exit code unchanged. Every package `test` script,
`check:contracts`, and `test:coverage:ts` invoke Vitest through this launcher
for the same reason, so `vp run test` stays correct wherever it runs. Vite+
0.3.0 supplies the workspace's single Vitest 4.1.11 runtime; packages must not
add or invoke a second local Vitest installation.

## Desktop Artifacts

- `vp run dist:desktop:artifact -- ...`: invoke the generic artifact wrapper.
- `vp run dist:desktop:dmg`: macOS DMG for the host architecture.
- `vp run dist:desktop:dmg:arm64`: macOS arm64 DMG.
- `vp run dist:desktop:dmg:x64`: macOS Intel DMG.
- `vp run dist:desktop:linux`: Linux x64 AppImage.
- `vp run dist:desktop:win`: Windows NSIS installer for the host architecture.
- `vp run dist:desktop:win:arm64`: Windows ARM64 NSIS installer.
- `vp run dist:desktop:win:x64`: Windows x64 NSIS installer.

On Windows the shared `scripts/run-msvc.mjs` launcher refuses `tauri build` and
`tauri bundle` before any compilation or packaging when the process runs under
the SYSTEM profile (`USERNAME` is a machine or service account, or
`LOCALAPPDATA`, `APPDATA`, or `USERPROFILE` resolves under
`C:WindowsSystem32configsystemprofile`). It exits with code 3 and names the
detected variables; run the build as the logged-in interactive user instead.
`apps/desktop/src-tauri/tauri.windows.conf.json` additionally sets
`bundle.useLocalToolsDir`, so Tauri caches the NSIS toolset in `target/.tauri`
rather than a per-account cache directory.

`node scripts/remove-test-firewall-rules.ts --executable <path> [--dry-run]`
removes the Windows Defender Firewall rules that Windows generates for one exact
repository-built test executable after a "Windows Security Alert" prompt, then
re-queries and fails unless zero matching rules remain. It refuses installed
locations (Program Files, per-user Programs, the Windows directory) and never
removes the app-managed `BiBCode Remote Access` rule. Run it from an elevated
PowerShell; on other platforms it exits 0 without doing anything.

The wrapper, `scripts/build-desktop-artifact.ts`, rejects cross-platform builds
by default, invokes the canonical `@bibcode/desktop` Tauri package, and copies
bundle output under `release/desktop/<platform>-<arch>` unless an output
directory is supplied. It compiles with `tauri build --no-bundle` and then
packages that binary with `tauri bundle` (the desktop package's `build` and
`bundle` scripts); the Tauri CLI's bundle step does not run Cargo. Each step
runs with at most three attempts. Every failed attempt is reported with its exit
code, and before retrying a macOS DMG build the wrapper reads
`hdiutil info -plist` and detaches only the intermediate `rw.*.dmg` image inside
that build's bundle `dmg` directory, reporting each detach or its failure; other
mounted images are never touched. The final error names the first failure, not
only the last. macOS DMG bundles pass `--verbose` to the Tauri CLI because the
bundler logs the generated `bundle_dmg.sh` output (`create-dmg`, `hdiutil`,
AppleScript) only at debug level.

`--symbols`, which release CI passes, adds Breakpad symbols between the two
steps. The build then uses the symbol profile:
`CARGO_PROFILE_RELEASE_STRIP=none`,
`CARGO_PROFILE_RELEASE_DEBUG=line-tables-only`, plus
`CARGO_PROFILE_RELEASE_SPLIT_DEBUGINFO=packed` on macOS (for a dSYM) and
`RUSTFLAGS=-C link-arg=-Wl,--build-id=sha1` on Linux. `scripts/dump-symbols.ts`
dumps the unstripped binary, with its dSYM or PDB, into
`target/<triple>/release/symbols`. The wrapper then strips the binary as rustc
strips executables (`strip --strip-all` on Linux, plain `strip` on macOS;
Windows ships the binary as linked and keeps the names in the PDB) and bundles
it. It strips into a copy that replaces the binary: Cargo's binary is a hard
link to its cached `deps/` artifact, and an in-place strip would leave the next
build of unchanged sources without symbols to dump. Every bundle attempt starts
from the stripped bytes, written back over the same file, because the bundler
patches its bundle-type marker into the binary in place and restores it only
after a successful bundle. The bytes go back only while the binary is still the
file that was fingerprinted (same inode and link count). On Linux and macOS that
is the stripped copy, and a binary Cargo re-linked over it fails the build
instead, so the restore never strips Cargo's `deps/` artifact through that hard
link. On Windows nothing is stripped and the binary stays Cargo's hard link to
`deps/`; the restore writes through it, but with the bytes it already holds.
After bundling the wrapper fails unless the binary is
still the stripped file (same file and SHA-256), and unless every copy it can
read in a bundle has the binary's build ID and symbol table: the macOS app,
which the bundler re-signs (a DMG symbol build also requests `app`, because the
bundler otherwise deletes the app it built), the package tree of this version's
`.deb`, and the AppImage, which it extracts with `--appimage-extract`. Windows
installers pack the binary itself. Cargo would re-link its unstripped artifact
over the binary even when nothing needs rebuilding, so this check, not the Tauri
CLI's current behavior, keeps an unstripped binary out of the bundles. The
output directory also receives
`bibcode-desktop-symbols-<version>-<os>-<arch>.zip`; see
[Symbol assets](../operations/release.md#symbol-assets). `--symbols` needs
`dump_syms` on `PATH` and cannot be combined with `--skip-build`.

`scripts/dump-symbols.ts` runs `dump_syms --inlines --check-cfi --store <store>`
on one release binary:

```sh
node scripts/dump-symbols.ts --binary <path> --module-name <shipped-name> --out <store> \
  [--pdb <path>] [--dsym <path>] [--zip <archive>]
```

The binary is staged under its shipped file name first, because `dump_syms`
names the module after its input. The script fails unless the `.sym` has `STACK`
records (unwind data) and functions with source lines. A stripped binary or a
missing dSYM yields no `FUNC` records, and a Windows binary without its PDB
yields only `<unknown in ...>` placeholders without lines. `--zip` writes an
archive holding just that `.sym` at its store path. The script uses only Node
built-ins. It needs `dump_syms` (pinned to 2.3.9 in CI) with its default
features, because the lookup of the PDB that a Windows binary names is part of
the `http` feature.

Release assembly, `scripts/assemble-release-assets.ts`, requires twelve symbol
archives, `bibcode-{desktop,server}-symbols-<version>-<os>-<arch>.zip` for each
of the six release targets, and still rejects any file outside its allowlist.
`bibcode profiler symbolicate --download` fetches exactly these names, so a
renamed archive breaks symbol downloads for that release; see
[Symbol assets](../operations/release.md#symbol-assets).

The desktop artifact contains the Tauri host, in-process Rust server, and built
web assets. It does not stage Node.js, a TypeScript server, or helper sidecars;
the profiler's crash monitor is the same executable relaunched.

## Standalone Server Artifacts

- `vp run dist:server:artifact -- ...`: generic standalone-server builder.
- `vp run dist:server:mac:arm64` / `:x64`: macOS server archive.
- `vp run dist:server:linux:arm64` / `:x64`: Linux archive plus `.deb` and `.rpm`.
- `vp run dist:server:win:arm64` / `:x64`: Windows server archive.

The builder stages the native `bibcode` executable, production web client,
`docs/user/server-installation.md`, and license under one versioned root. Use
`scripts/smoke-server-distribution.ts` against that staged root. Linux CI additionally
uses `scripts/test-linux-server-package.ts` to install, start, remove, and verify data
preservation in the documented native-architecture containers.

## Remote update verification tooling

`vp run test:desktop:upgrade` invokes the CI-only seeded packaged-upgrade
harness. Ordinary native matrix jobs include `remote-install` after the existing
previous-stable/protected lanes; it reuses the current-source baseline in its
own root. Never run this command locally. For code changes, run only:

```sh
vp test run scripts/seeded-desktop-upgrade-smoke.test.ts scripts/lib/remote-install-driver.test.ts
node --check scripts/seeded-desktop-upgrade-smoke.ts
node --check scripts/lib/remote-install-driver.ts
vp run --filter @bibcode/scripts typecheck
```

The Node driver imports the product coordinator through the client-runtime's
public subpath; scripts declares that workspace dependency only for tooling.
It exchanges a private test grant/bootstrap for its own bearer/tickets, records
boot/version/storage and progress, and never prints credentials. Syntax and
unit checks qualify the tooling, not actual install or native UI behavior.
The loopback fake host below qualifies scripted UI/reconnect states without
performing a production install.

## Repository Maintenance

- `cargo run -p bibcode-server --example remote_update_fake_host -- <base-dir> <port 4800-4899> <server-version> [label]`:
  development-only, loopback-only scripted interactive host for remote-update
  live checks (default label: `Fake-host`). Stdin accepts one JSON object per
  line: `{"status":{...}}` replaces the camelCase update status,
  `{"restart":{"serverVersion":"9.9.1","afterMs":2000}}` restarts on the same
  data root and port with a fresh boot ID and idle status (`afterMs` defaults to
  2000), `{"stop":true}` stops serving permanently while the process stays alive,
  and `{"exit":true}` or EOF shuts down and exits. Pair through
  `bibcode pairing offer --base-dir <base-dir> --endpoint http://127.0.0.1:<port> --reach this-computer --json`;
  the example prints only a `started` JSON event with `port`, `serverVersion`,
  and `bootId`, without pairing credentials.
- `bash scripts/measure-gitlab-merge-request-load.sh [rtt-ms] [runs]`: measure a
  cold GitLab merge-request list load and a single merge-request load against
  the local harness in `apps/server/tests/pull_requests_gitlab_load_harness.rs`.
  The default round trip is 200 ms and the default sample count is 5. The
  command prints `HARNESS_JSON` and writes the same document to
  `BIBCODE_GITLAB_HARNESS_REPORT` (`/tmp/bibcode-gitlab-mr-load.json` when unset).
  It also runs the ignored
  `gitlab_subscribed_merge_request_polling_records_host_requests` scenario,
  which waits through several real 20 s poll ticks and prints
  `HARNESS_SUBSCRIBE_JSON`; the default `cargo test` suite skips it.
- `bash scripts/test-linux-git-compatibility.sh TEST_BINARY [IMAGE ...]`: run the
  compiled Linux Git-runner regression in disposable Debian, Ubuntu, Fedora,
  and Arch containers. Build the executable on the Ubuntu 22.04 glibc baseline;
  set `CONTAINER_ENGINE=podman` to use Podman instead of Docker. The script
  reports every selected image's result and fails if any check fails. See the
  [Linux runbook](../testing/linux-desktop.md) for build and validation scope.
- `vp run sync:repos`: fetch and stage exact read-only reference snapshots under
  `.repos`; pass `--repo <id>` to synchronize one entry. The command requires a
  clean index and working tree, preserves fetched path casing, modes, and
  upstream-tracked ignored files, applies configured pruning, treats every
  configured prefix and prune path as a literal Git path, and creates no commit.
  It holds the atomic file
  `<absolute-git-common-dir>/bibcode-reference-repos-sync.lock` from the initial
  clean check through fetch, snapshot construction, apply, and any verified
  rollback, so linked worktrees share one writer. A concurrent invocation fails
  before fetch without changing or stealing the lock. Success, pre-apply
  failure, verified rollback, and recovered interruption remove the lock. A
  timed-out, failed, or unverified rollback keeps the lock because the
  index/worktree may require manual recovery. The mutating apply owns a nested
  child-process scope, so interruption closes and reaps that command before
  rollback starts. Every timed rollback command likewise owns a nested scope;
  timeout, failure, and success finish its child cleanup before the next
  recovery step or return. If a process crash or failed recovery leaves the
  lock behind, first use process inspection to verify that no
  `sync-reference-repos` process owns the repository and verify or recover the
  index/worktree. Then obtain the exact common directory with
  `git rev-parse --path-format=absolute --git-common-dir` and remove only its
  `bibcode-reference-repos-sync.lock` file; the command never removes a stale
  lock automatically.
- `vp run measure:desktop-runtime -- ...`: capture startup, memory, and
  process-tree measurements.
- `node scripts/throttle-proxy.ts --listen <host:port> --target <host:port> --control <host:port> [--down <bytes/s>] [--up <bytes/s>] [--delay <ms>] [--jitter <ms>]`:
  development-only throttling TCP proxy for the
  [slow-link liveness scenario](../testing/cross-platform-validation.md#slow-link-liveness-scenario).
  Each direction is paced separately with backpressure. `--delay` holds every
  byte that long in each direction (125 ms is a 250 ms round trip), plus up to
  `--jitter` more, without reordering bytes. `GET /set?down=&up=&freeze=0|1&delay=&jitter=`
  on the control address changes the link (0 bytes/s is unlimited), `GET /drop`
  closes every open connection, and `GET /state` reports the settings.
- `node scripts/measure-remote-latency.ts [--rtt 0,250] [--jitter <ms>] [--typing-interval <ms>] [--seed-messages <n>] [--out <json>] [--keep]`:
  development-only
  [high-latency measurement](../testing/cross-platform-validation.md#high-latency-measurement).
  For each round trip it starts a disposable `bibcode serve` (`--server-bin`,
  default `target/debug/bibcode`) with the packaged-UI provider fixtures, puts
  the throttle proxy between it and a headless Chromium (`--chromium`, default
  `/usr/bin/chromium`), and drives user actions. It prints one JSON line per
  action and a Markdown table of RPCs, sequential round trips, bytes, time to
  visible result and terminal echo lag. It serves the web build from
  `--static-dir` (default `apps/web/dist`) itself, so only server traffic is
  delayed; build that with `VITE_HOSTED_APP_URL=https://hosted.invalid` so
  the page treats its own origin as its server.
- `node scripts/measure-vcs-runtime.ts`: on Windows, build and run the
  current-source server VCS idle-process measurement plus the production-Atom
  foreground queue benchmark. It defaults to 600 seconds and writes all
  machine-specific evidence and an isolated Cargo target to a unique temporary
  directory, selecting exact executables from Cargo artifact JSON even when a
  target triple is configured; use `--duration-ms` only for short harness
  validation.
- `vp run clean`: remove generated dependency, build, target, and Vite+ cache
  directories. This is destructive to local build output.

See [CI Quality Gates](../operations/ci.md) for the commands enforced in CI.
