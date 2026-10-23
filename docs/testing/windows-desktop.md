# Windows Desktop Validation

Read [Cross-platform validation](./cross-platform-validation.md) first. This
page contains only native Windows additions.

## Supported native targets

The supported Windows release targets are Windows 10 or 11 on x64 and Windows 11 on
ARM64. Release and native smoke workflows use the matching MSVC toolchain and build an
NSIS installer. Native ARM64 evidence must come from `windows-11-vs2026-arm`; emulation
or an x64 application mislabeled as ARM64 does not count.

Standalone server releases provide native ARM64 and x64 `.zip` archives with the
matching web client.

Record the exact Windows edition, build, architecture, and whether the host is
physical or virtual. Do not silently substitute Wine, WSL, or a cross-compiled
binary for native Windows evidence.

## Host and toolchain inventory

Use PowerShell 7 when available:

```powershell
Get-ComputerInfo |
  Select-Object WindowsProductName, WindowsVersion, OsBuildNumber, OsArchitecture
$PSVersionTable
$env:PROCESSOR_ARCHITECTURE
git --version
gh --version
rustc -Vv
cargo -V
rustup show
node --version
vp --version
Get-Command cl.exe -ErrorAction SilentlyContinue
Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\*' -ErrorAction SilentlyContinue |
  Where-Object { $_.name -match 'WebView2' } |
  Select-Object name, pv
wsl.exe --status
wsl.exe --list --verbose
```

Record missing MSVC, Windows SDK, WebView2, WSL, or distribution capabilities.
Do not install or enable system components without permission. Package scripts
already route Rust/Tauri commands through `scripts/run-msvc.mjs`; use the
documented scripts rather than constructing an unverified Visual Studio
environment.

## Parallels guest execution preflight

When the Windows host is a Parallels virtual machine driven from macOS, complete
this preflight before dependency installation, packaging, packaged E2E, or
Computer Use capture:

- The VM is running and not paused or suspended.
- Every guest command runs as the logged-in interactive Windows account:

  ```sh
  prlctl exec 'Windows 11' --current-user -- pwsh -NoProfile -Command '...'
  ```

  Without `--current-user`, `prlctl exec` runs as `NT AUTHORITY\SYSTEM`. Tauri
  then caches the NSIS toolset under
  `C:\Windows\System32\config\systemprofile\AppData\Local`, where x86
  filesystem redirection on ARM64 makes the x86 NSIS bootstrapper fail with
  `Unable to start child process, error 0x2` after the Rust release build has
  already completed. `scripts/run-msvc.mjs` refuses `tauri build` and
  `tauri bundle` under the SYSTEM profile before compiling or packaging and
  names the offending environment variables; record that exit code 3 as a
  harness misconfiguration, switch to `--current-user`, and rerun. Confirm the
  account with `whoami` and `$env:LOCALAPPDATA` before the first expensive
  command.

- Install workspace dependencies with that same account, from the checkout
  root, so pnpm's hard-linked store stays readable to the account that runs the
  tests and builds.
- Invoke Vite+ through the checkout-local launcher instead of a global `vp`
  whenever the command is `vp test ...`; a global installation can load a second
  Vitest runtime and fail with `Vitest failed to find the runner`:

  ```powershell
  node scripts/run-local-vp.mjs test run packages/client-runtime/src/state/vcs.test.ts
  ```

  Package `test` scripts, `check:contracts`, and `test:coverage:ts` already
  route their Vitest invocations through this launcher, so `vp run test` and
  `vp run check:contracts` stay correct under a global `vp`; record which
  launcher each directly typed `vp test` command used.

- Use an isolated `BIBCODE_HOME` for every launch and keep the production
  installer output (`release/desktop`) and the E2E build
  (`target/<triple>/release/bibcode-desktop.exe` from `test:ui:desktop:build`)
  in distinct, recorded paths.
- Before Codex Computer Use captures the guest, return Parallels to **Windowed**
  (not Full Screen or Coherence) mode; ScreenCaptureKit cannot capture the
  full-screen Parallels surface. Switching back is a host-tool limitation, not
  an application defect, so record it without changing app code.
- Plan the exact process and firewall cleanup checks from
  [Process and Job cleanup](#process-and-job-cleanup) before launching anything.

## Focused Windows contracts

Verify the Git Manager trust command with native Windows PowerShell and Git:

```powershell
node scripts/run-local-vp.mjs test run apps/web/src/components/gitManager/gitManagerRepositoryAvailability.test.ts
```

The Windows-only round-trip case executes the application-generated commands
for drive and UNC paths containing spaces, dollar signs, backticks, and
typographic quotes, then checks the exact `safe.directory` values stored by
Git. It uses temporary global/system configuration and HOME, so it never adds
trust to the user's Git configuration. The UNC check covers command parsing
and registration, without contacting a network share. A real-share access or
packaged-terminal/clipboard check is separate evidence. A skipped case or a
PowerShell-on-Linux run is not a native Windows result. Both Windows CI rows
run this contract alongside the desktop shell fixture tests.

Select focused tests from affected source and verify at least:

- case-only drive or path variants do not create duplicate worktree owners;
- slash/backslash spelling and directory junction aliases preserve one physical
  identity while persisted display spelling remains exact;
- replacement or reuse of a path/file identity is rejected before destructive
  worktree removal;
- Windows handles remain live across validation and deletion where required;
- late process admission during shutdown fails closed and the child is waited;
- Windows Job ownership reaps descendants before shutdown returns, and repeated
  status probes after termination cannot consume or lose the terminal Job
  state;
- independent runtimes cannot terminate each other's process roots;
- local Windows and WSL presentation follows current environment capability;
- desktop-managed SSH child I/O runs on the desktop's own SSH runtime: with the
  calling runtime's only blocking thread held, a stand-in ssh's pairing line is
  still parsed, and after a descendant holding its pipes makes the drain give
  up, no pipe read stays parked
  (`node scripts/run-msvc.mjs cargo test -p bibcode-desktop --lib ssh::tests::windows_drain`;
  the tests start `cmd.exe` and the built-in `powershell.exe`, and kill the
  descendant they record);
- saved remote environments appear in the environment rail without exposing
  privileged SSH, Tailscale, relay, or connection-lifecycle controls outside
  their owning settings and desktop-bridge boundaries; and
- update protection treats long-lived read subscriptions as reads, reports
  staged progress and active mutation counts while preparing, rejects a forged
  first-attempt bypass, and offers the acknowledged no-backup path only after a
  real protection failure; and
- Claude, Codex, Cursor, and OpenCode are visible while Grok is absent.

Run affected concurrency-sensitive owners at default, 8, and 12 harness
threads. Do not replace the default harness with a serial run.

## VCS idle and foreground measurement

Before measuring an idle window, verify the current event-driven observation
boundary on native Windows:

```powershell
node scripts/run-msvc.mjs cargo test -p bibcode-server --lib git::broadcaster::tests::ref_poll_is_replaced_by_watcher_and_safety_status_reads -- --exact --nocapture
node scripts/run-msvc.mjs cargo test -p bibcode-server --lib git::watcher -- --nocapture
node scripts/run-msvc.mjs cargo test -p bibcode-server --lib production::runtime::tests::structured_terminal_process_exit_immediately_invalidates_status_under_watcher_fallback -- --exact --nocapture
node scripts/run-msvc.mjs cargo test -p bibcode-server --test production_git_vcs_rpc native_watcher_publishes_external_worktree_and_head_changes_to_status_subscribers -- --exact --nocapture
node scripts/run-local-vp.mjs test run packages/client-runtime/src/state/vcs.test.ts apps/web/src/components/GitActionsControl.test.tsx
```

Record the idle 59/60-second boundary, native content/index/`HEAD`/refs events,
watcher fallback and lifecycle outcomes, terminal exit invalidation, reconnect,
and hidden/reveal/focus/menu catch-up separately. A native Windows pass does not
claim real WSL, SSH, Linux, or macOS execution. When WSL is usable, repeat the
disposable-project branch and terminal scenarios in the selected distribution;
otherwise record them as unavailable.

Run the maintained controller from the repository root. It builds both
Windows-only examples through the root Cargo workspace and lockfile, then runs
the complete Git and production-Atom measurements:

```powershell
node scripts/measure-vcs-runtime.ts
```

Use a short window only to validate the harness itself, never as default-change
evidence:

```powershell
node scripts/measure-vcs-runtime.ts --duration-ms 3000 --queue-warmups 2 --queue-samples 10
```

The controller pre-resolves the real Git executable, creates a unique
test-owned data root plus one disposable physical repository/worktree and bare
origin, and completes fixture Git work before adding the shim to the server's
PATH. It overrides inherited Cargo target configuration with an isolated target
inside that evidence root, consumes Cargo `compiler-artifact` JSON, and builds
and launches the exact resulting `measure_vcs_runtime_server` executable even
when Cargo uses a configured target-triple directory. The example constructs the production
`ServerRuntime`, RPC registry,
`StatusBroadcaster`, and `GitRepository`. The example opens the real WebSocket
RPC path, keeps exactly one `subscribeVcsStatus` stream alive, acknowledges
chunks, and makes no focus, menu, mutation, or external Git changes during the
idle window. Desktop UI automation is not required for this server-owned path.

Record the server PID, creation time, executable path, fixture common directory,
worktrees, subscribers, exact interval boundaries, and any other process that
could share attribution. Capture process-start events for direct `git.exe`
children of that exact server identity. The controller copies the tracked
`measure_vcs_git_shim` example to the test-owned PATH as `git.exe`. It appends
the timestamp, PID, process and parent creation identities, and argument vector
under one named mutex, delegates once to the pre-resolved real Git executable
with inherited standard handles/environment, and returns its exit status. Count
the shim launch only; do not also count its delegated Git child.

Before starting the ten-minute clock, run a short probe that proves all of the
following: the subscription received its snapshot, the physical-repository
owner attached, the recorder captured a direct Git child with its arguments,
and the recorded parent still has the same creation identity. Clear the probe
records. Then keep the verified scenario idle for at least 600 seconds and
summarize command arguments into discovery, status/diff, and fetch categories.
If the evidence exposes command lines but not Rust `ProcessRequest.operation`,
report that exact limitation instead of assigning internal operation names.

On completion or failure the controller uses one bounded cleanup routine. One
atomic Windows process snapshot binds PID, parent PID, decimal FILETIME, and
normalized executable before stop. While the root remains alive the controller
captures its exact child/grandchild closure. Graceful success also reaps and
verifies captured orphans; timeout revalidates the immutable identities,
terminates verified descendants leaf-first and the owned server handle last,
awaits the parent exit, and rejects any survivor. After a clean completion it
parses only the
half-open `[start, start + duration)` window, filters the exact direct-parent
identity, reports non-direct and wrong-identity records, calculates the
per-minute/per-physical-repository rate, and prints and writes every evidence
path. A parse, identity, snapshot, common-directory, quiescence, shutdown, or
queue-summary failure makes the command fail.

`scripts/measure-desktop-runtime.ts` remains the supported startup, memory, and
point-in-time process-tree sampler. Its Windows ownership monitor does not
retain process-start history, so it cannot by itself prove the VCS Git-launch
rate. Use it only as additional current-process identity/memory evidence.

The same controller runs the tracked Vite+ production-Atom benchmark with the
requested warm-up and sample counts. It uses the real `createVcsEnvironmentAtoms`
commands, keeps `refreshStatus` deferred while same-key `stageFiles` is
scheduled, and measures with `performance.now()` until the stage RPC effect
begins. Record its warm-up and measured sample counts, sorted nearest-rank p95,
maximum, and the 250 ms comparison.

## External worktree and junction fixture

Use a unique test-owned root, never a user project. One example shape is:

```powershell
$runId = [guid]::NewGuid().ToString('N')
$testRoot = Join-Path $env:TEMP "bibcode-win-validation-$runId"
$repository = Join-Path $testRoot 'Repository With Spaces'
$worktrees = Join-Path $testRoot 'External Worktrees'
$junction = Join-Path $testRoot 'Junction Alias'

New-Item -ItemType Directory -Path $testRoot | Out-Null
New-Item -ItemType Directory -Path $repository, $worktrees | Out-Null
git -C $repository init
git -C $repository config user.name 'BiBCode Test'
git -C $repository config user.email 'bibcode-test@example.invalid'
Set-Content -LiteralPath (Join-Path $repository 'README.md') -Value 'fixture'
git -C $repository add README.md
git -C $repository commit -m 'fixture baseline'
git -C $repository worktree add (Join-Path $worktrees 'Feature Alpha') -b feature-alpha
New-Item -ItemType Directory -Path (Join-Path $worktrees 'feature-long-path\nested') | Out-Null
git -C $repository worktree add (Join-Path $worktrees 'feature-long-path\nested\candidate') -b feature-long
$junctionTarget = Join-Path $worktrees 'Feature Alpha'
cmd.exe /d /c "mklink /J `"$junction`" `"$junctionTarget`""
git -C $repository worktree list --porcelain
Resolve-Path -LiteralPath (Join-Path $worktrees 'Feature Alpha')
Resolve-Path -LiteralPath $junction
```

Check that a drive-letter case variant, separator variant, and junction alias
cannot create another BiBCode owner for the same physical worktree. Confirm the
UI retains useful Git-reported display spelling and restart reconstructs the
same identity. **Keep hidden** and removal from BiBCode must not unexpectedly
delete the external Git worktree.

Use only fixture-owned paths for deletion/replacement identity tests. Record
the exact target before and after each destructive action.

## WSL matrix

Record:

```powershell
wsl.exe --status
wsl.exe --list --verbose
```

When WSL and a supported distribution are usable:

- Settings shows **Local environment** and WSL status/setup controls;
- Add Project targets the selected environment; Local offers **This device**
  plus only WSL locations with a matching usable bootstrap, while saved remote
  rail selections remain valid remote hosts;
- native and WSL paths do not collapse into one project identity;
- a disposable WSL project can launch its supported session and terminal;
- entering WSL-only first persists and verifies native local-only exposure and
  removes the managed firewall rule before switching topology; leaving WSL-only
  restarts the native backend explicitly local-only before share-state may
  request a later widen. Confirm both transitions serialize with concurrent
  exposure/settings mutations;
- restart retains the correct environment identity; and
- shutdown does not terminate unrelated WSL processes.

When WSL is unavailable:

- Local Environment still renders a meaningful unavailable state, not an empty
  section;
- refresh/retry remains accessible;
- Add Project does not present an unusable distribution; and
- local Windows projects remain usable.

Do not install a distribution or change system WSL configuration without
permission. Remote-server targets and exposure controls are validated in the
packaged UI scenarios independently of the WSL branch.

For update validation, use an isolated `BIBCODE_HOME` and disposable native
project; include a disposable WSL project when WSL is usable. Keep a read
subscription open while installing an available test update and confirm it
does not block protection. During a deliberately held mutation, confirm the
dialog promptly shows the waiting stage, elapsed time, and an active-operation
count. After the bounded failure, verify that normal retry is still the primary
action, the no-backup action requires acknowledgement, a forged first-attempt
bypass is rejected by the native host, and an installer failure restarts the
exact pre-update native and WSL backend set.

With the same isolated test instance, arrange an installer failure and have a
test-owned listener acquire the native backend port after shutdown, keeping it
bound past the 3 s restart window. Confirm **Update not installed** names that
port as in use, offers **Restart server**, and disables **Retry installation**
with the visible restart explanation. Keep an unsent composer draft and verify
it survives the outage in the packaged WebView. Release only the test listener,
choose **Restart server**, and confirm the same backend port reconnects, the
draft remains, and **Retry installation** becomes available without reopening
the dialog. Record the listener/installer fixture and observed port in the
execution report; never use the user's running instance for this check.

## Native tests and static gates

Follow the shared focused and broad gates. Root workspace scripts already use
the package-specific MSVC launcher. When a direct native Rust command needs the
same environment, use:

```powershell
node scripts/run-msvc.mjs cargo check -p bibcode-server --all-targets
node scripts/run-msvc.mjs cargo test --workspace -j 2 -- --test-threads=2
node scripts/run-msvc.mjs cargo clean -p bibcode-server -p bibcode-desktop -p bibcode-updater-verifier
node scripts/run-msvc.mjs cargo clippy --workspace --all-targets -- -D warnings
```

Run the `--all-targets` check first. Unix-only test helpers and imports must sit
behind the same `cfg(unix)` as their consumers; an unused import in a Windows
test target fails the whole `bibcode-server` test build under `-D warnings` and
blocks every focused server test below. CI's Windows native rows run the same
command.

A test target that fails with `Failed to launch Windows Cargo target ... EACCES`
while the same binary runs from a neutral file name is UAC installer detection
(names containing `install`, `setup`, `update`, or `patch`). The shared runner's
sidecar manifest declares `asInvoker` and touches the binary to bypass the
manifest cache; report the launch error rather than elevating the shell or
renaming targets.

Check free space before the broad Rust gates: on ARM64 MSVC the `target\debug`
test binaries and their PDBs grow to tens of gigabytes across the server
integration targets, and a full disk surfaces as `LNK1104: cannot open file`
or `LNK1140: limit exceeded for program database` rather than a disk error.
Delete only the checkout's own `target\debug` to recover; never the retained
validation tree.

Git for Windows installs with `core.autocrlf=true` in the system and global
configuration. Test fixtures that assert on checked-out file bytes pin
`core.autocrlf=false` on the repository they create right after `git init`;
a `"base\r\n"` versus `"base\n"` assertion failure means a fixture is missing
that pin, not that the product rewrote line endings. Do not change the host
Git configuration to make a suite pass.

Run the desktop E2E support contract and the launcher contracts natively on
Windows:

```powershell
node scripts/run-local-vp.mjs test run apps/desktop/e2e/support/test-project.test.ts
node scripts/run-local-vp.mjs test run scripts/run-msvc.test.mjs scripts/run-local-vp.test.mjs scripts/remove-test-firewall-rules.test.ts scripts/tauri-hardening.test.ts
```

The `native_desktop` Windows CI row runs this exact command after the desktop
Rust host tests. A native pass includes real execution of the generated Cursor
`.cmd` shim through the Windows command processor and verification of its exact
action-log record. The same file's simulated macOS/Linux/Windows fixture and
filesystem assertions on a non-Windows host are compatibility evidence; the
guarded `.cmd` case is unavailable there and must not be reported as passed.

Keep `vp run test`, `vp check`, `vp run typecheck`, `cargo fmt --all --check`,
and `git diff --check` in the recorded gate set. Do not run separate broad
Cargo commands concurrently.

On Windows ARM64 the relay package cannot execute its Vitest suites: Cloudflare's
`workerd` ships no `win32 arm64` binary, so every relay test file fails at load
and `vp run` then cancels the still-running Rust suites. Run the broad gate
there with the same path filters the root `build` script uses, which selects
every workspace package except `infra/relay`:

```powershell
vp run --filter './apps/*' --filter './packages/*' --filter './oxlint-plugin-bibcode' --filter './scripts' test
```

Record the relay package as unavailable evidence and leave its coverage to the
Linux and macOS rows. Do not exclude any other package.

## NSIS package build and inspection

Build the supported artifact for the host architecture as the interactive
account (see the Parallels preflight above):

```powershell
vp run dist:desktop:win:x64     # x64 hosts
vp run dist:desktop:win:arm64   # Windows 11 ARM64 hosts
```

`tauri.windows.conf.json` sets `bundle.useLocalToolsDir`, so the NSIS toolset
is cached under the checkout's `target/.tauri` directory rather than
`%LOCALAPPDATA%`; retain it with the Cargo cache. If `run-msvc.mjs` exits with
code 3 before compiling, the account is wrong: do not copy tools into Windows
system directories, change ACLs, or modify the installed toolchain.

Discover the produced installer and executable from the command output and
`release/desktop`, then record absolute paths. Inspect PE version metadata and
architecture. Classify Authenticode without changing the package:

```powershell
Get-AuthenticodeSignature -FilePath $installer |
  Select-Object Status, StatusMessage, SignerCertificate
```

Current release documentation states that Windows artifacts are not
Authenticode-signed. An unsigned local artifact is expected evidence, not a
signed pass. Do not use production secrets.

In the CI-only seeded upgrade lanes, the updater host exiting is not proof of
installation. Before cleanup and relaunch, the harness waits for the installed
executable's exact candidate `ProductVersion`, a readable SHA-256, and no
candidate-named updater installer. Its bounded `windows-install-handoff.log`
records path, version, hash, read and installer-lookup timings, and installer
PID observations without command lines or credentials. The probe reads the
executable in-process and lists installers with `tasklist.exe`. PowerShell
produced no JSON within a twenty-second sample on the ARM64 runner, including
after the host had exited, so a sample is no longer a PowerShell process. Each
file read and installer lookup has its own short bound. The probe hashes the
executable only after its product version is already the candidate. A read or
lookup that hits its bound is an unavailable sample: the log keeps the timings
and any version already read, and that sample does not satisfy the predicate.
After the candidate is installed, the harness also requires the relaunched app:
`tasklist.exe /V` must list the installed executable with a window (helpers such
as the crash monitor have none) within the restart bound, so an update that
closes BiBCode without starting the new version fails the lane instead of
passing a cleanup that found nothing to stop.
When either wait times out, the harness saves `windows-processes-at-timeout.txt`
(`tasklist.exe /V`), `windows-ui-at-timeout.txt` (top-level windows and their
UI Automation names, which shows a waiting installer dialog), and
`windows-process-audit-at-timeout.txt`: the updater's temporary directories,
the BiBCode uninstall entries, then process starts and exits from the Security
log for BiBCode executables (including the updater's installer) and the
WebDriver processes, newest first and capped, with command lines only for
`bibcode-desktop.exe` and its updater installer. The workflow turns on
process-creation and process-termination auditing before the lane runs; a local
run without it still records the directories and uninstall entries and notes
that no audit events were found. The workflow also turns on Windows Error
Reporting minidumps for `bibcode-desktop.exe` and uploads any it writes as the
`seeded-upgrade-crash-dumps-<arch>` artifact: an exit status of `0xc00000fd`
in the audit is a stack overflow, and its dump holds the crash site.
The protected-baseline install command stays open until the host exits into that
installer. Ending the command on the first `protecting` event stops the host
before NSIS starts. Inspect that artifact on timeout; the later public
runtime-version and retained-data checks remain required.
A command deadline becomes an unavailable sample only after the exact child emits
`close`; the next sample remains inside the original overall installation
deadline. Cleanup that cannot prove `close` within its existing five-second
budget fails the lane, including after forced termination, and the smoke
process exits nonzero instead of waiting out the job budget. Spawn and other
cleanup errors remain fatal. An unavailable sample never satisfies the exact
version/hash/no-installer predicate.

Build and run packaged E2E with the supported platform value
`BIBCODE_E2E_PLATFORM=win`:

```powershell
$env:BIBCODE_E2E_PLATFORM = 'win'
vp run test:ui:desktop:build
$env:BIBCODE_E2E_APP_PATH = (Resolve-Path 'target/release/bibcode-desktop.exe').Path
vp run test:ui:desktop
```

When the run ends, the artifact directory (`BIBCODE_E2E_ARTIFACT_DIR`) also holds
`app-*.log`: the last 4 MiB of each of the packaged app's own logs, with the native
page-load lifecycle and webview process failures, next to the screenshots and
WebdriverIO logs.

Use a unique test profile supported by the E2E harness. Do not launch an
installed BiBCode executable or overwrite `%APPDATA%`/`%LOCALAPPDATA%` user
data. `BIBCODE_E2E_APP_PATH` deliberately selects the executable produced by
the E2E build in the current worktree, not an installed production copy.

The embedded test WebDriver server (`tauri-plugin-wdio-webdriver`) binds
`127.0.0.1` only and never opens UDP, so it does not by itself raise a Windows
Defender Firewall prompt. Desktop port selection used to listen briefly on
`0.0.0.0` and `[::]`, which made Windows treat any freshly built executable as
a network server and, when the prompt was cancelled, create program-scoped
Public inbound Block rules (one TCP, one UDP). The probe now binds without
listening. If a prompt still appears for a test executable, do not approve it;
cancel it, finish the run, and remove the generated rules with the helper in
[Process and Job cleanup](#process-and-job-cleanup). Changing firewall exposure,
approving a prompt, or generating an **Another device** pairing offer requires
explicit operator approval recorded in the report.

## Packaged UI scenarios

Include the shared [Pull Requests smoke](./cross-platform-validation.md#pull-requests-web-shell-validation):
open the project-header sidebar button, inspect the repository list, open a
request, and render a real text patch on Files changed/Changes with `tab=files`
retained after reload. Record unavailable fixture/host states separately from
that pass. The packaged Pierre spec's sibling checks route entry only; the
shared procedure owns authenticated list/detail/files evidence.

Include the shared [slow-link liveness scenario](./cross-platform-validation.md#slow-link-liveness-scenario)
when a browser client and a development or standalone server are available on
this platform; otherwise record it as unavailable evidence.

Use Codex Computer Use to operate the packaged executable. Capture normal,
minimum-size, and relevant Windows DPI states. Verify:

- the environment rail groups **This device** and usable WSL locations under
  Local, shows saved remote servers separately, and Add Project targets the
  current rail selection;
- in the **Repositories** view, the header's Add Project button opens the dialog
  with the **Location** selector; choosing a remote server makes **Browse folder**
  and the clone and create **Browse…** buttons list that server's folders;
- **Local environment** is visible at `/settings/local-environment` and never
  empty;
- Add a server by pairing code with a **Server alias**. Confirm the saved-server
  list and environment rail show that alias after reconnecting and restarting
  the app. Blank aliases use the pairing code's server name; failed pairing
  preserves the alias for retry. The remote server's own name remains unchanged;
- Add a headless web-mode `bibcode serve` server without an alias (pairing code
  or **Advanced: manual endpoint and token**) and confirm its name is the
  machine's hostname, with "Local" only if no usable hostname is reported.
  `bibcode start`, bare `bibcode`, and desktop-owned backends keep "Local".
  Select this server in the rail, then open **Settings → Remote Servers**.
  Choose **Rename…** from its **⋯** menu: the dialog is prefilled, refuses a
  blank name with **Enter a name for this server.**, and Enter in the name field
  saves. Enter on **Cancel** cancels; Enter on **Use the server's name** restores
  the reported name without saving. During saving, Escape and backdrop clicks
  keep the dialog open. A failed save explains the known reason and preserves
  the typed name for retry. Save a distinct name such as **Edge box** and confirm
  it appears immediately in the Settings row title, environment rail entry, and
  selected server's workspace card. Confirm the rail avatar initials update
  (**EB** for **Edge box**). Hover the Settings row's status dot before and after
  saving: it stays **Connected**, with no reconnect during the rename. Reload
  the app, then restart it; after each, confirm the saved name and initials
  persist. Disconnect reasons also use the saved name: rename the connected server, then
  close or interrupt its connection and confirm the reconnecting detail names
  the new alias. Repeat with a liveness timeout. Storage-identity errors still
  use the server's reported name;
- With that renamed test server connected, check a stalled connection on a
  Windows remote host: open Resource Monitor (`resmon.exe`) on that host, select
  the **CPU** tab, find the test server's `bibcode` process in the process list,
  right-click it, and choose **Suspend process**. Hover the Settings row's status
  dot and wait for
  `No data from <saved name> for 30 seconds. The connection is too slow or was lost. Reconnecting…`,
  followed by
  `Remote environment endpoint <base URL>/.well-known/bibcode/environment timed out after 10000ms. Reconnecting…`
  with the test server's endpoint URL. The disconnect reason uses the saved
  name (**Edge box**) and appears 27–33 seconds after the last data from the
  stopped server, when the client's liveness timeout closes the socket with
  code 4408. Keep BiBCode visible and focused for the whole stall: the
  disconnect then appears before a health-check message can become visible. If
  the window is restored from hidden or minimized during the stall, the
  application-active health check can report
  `<saved name> did not respond to a connection health check.` after 15
  seconds instead; repeat the check with the window kept visible.
  In Resource Monitor, right-click the same test
  server process, choose **Resume process**, and confirm it reconnects;
- select a saved server within a second of launch and confirm the rail keeps it
  selected for at least ten seconds while provider and settings updates arrive;
  repeat while the first primary welcome is delayed. A primary identity change
  follows the new primary only when the old primary was selected; a selected
  remote remains selected, including on `/agents` and with the mobile rail
  closed;
- Pair a loopback offer through a local connection or tunnel and confirm the
  already-active standard credential remains saved without administrative
  confirmation scope. Pending and off-host scope denials must still fail;
- Settings shows **Remote Servers** with **Connect to a host** and **Share this
  host** tabs; `/settings/connections` redirects there. SSH discovery and
  grant-driven sharing appears because the desktop bridge is present.
  remote targeting is driven by the environment rail rather than mixing saved
  servers into the Local WSL picker;
- Headless pairing: on a second machine or VM run `bibcode serve --host
  <routable address>`, confirm the startup line contains `pairingCode`, and add it
  through **Add Server → Pairing code**. Confirm the saved server appears
  alongside — not in place of — the app's own **Local** environment, since both
  hosts declare the environment id `local`. Mint a second offer with `bibcode
  pairing offer --endpoint http://<address>:3773` and confirm the dialog refuses
  it by name ("<label> is already saved."): two offers describe one environment.
  Then restart the headless server and confirm the saved server reconnects
  without re-pairing.
- Headless service: on the second machine run `bibcode service install --host
  <routable address>`, confirm `bibcode service status` reports `active`,
  reboot that machine, and confirm the desktop's saved server reconnects
  without re-pairing. On Linux confirm `loginctl show-user $USER` reports
  `Linger=yes`; on macOS confirm automatic login is enabled; on Windows confirm
  the `BiBCode Server` task shows `Running` after logon. Finish with
  `bibcode service uninstall` and confirm the definition is gone.
- Remote server updates: with a second BiBCode server saved (headless
  `bibcode serve` is sufficient), open Remote Servers settings, run **Check for
  Server Updates**, and confirm each saved server row shows an update badge
  (**Manual updates** for a headless server) and a manual-instructions block
  with a copy button. An offline server must not block the rest of the batch and
  shows no update failure while it is disconnected; a blackholed check must settle
  after 30 seconds across supervisor acquisition, readiness, and RPC execution,
  then release its batch worker. A check that fails over a live connection shows
  **Check failed** with the reason as its tooltip, and the row's button reads
  **Check again**. When the second server is a desktop-hosted release build,
  restart it and confirm its sidebar card reads **Not checked yet**, then changes
  to **Up to date** or **Update to v…** within about a minute without **Check
  for updates**;
  Use a desktop-hosted release shared through **Another device** as the second
  host. From both Settings and its card, **Update to v…** must open a named
  confirmation; with one terminal open there, counts must say it will stop.
  Cancel starts nothing. Confirm, observe downloading/backup/restart/version
  checking and success; closing Settings must not cancel the run. A failed
  restart beyond the three-minute budget shows actionable failure; **Retry**
  reconfirms with fresh counts, and row **Dismiss** clears settled feedback.
  A manual host offers **Show update steps** and **Copy** matching its platform
  and install kind. The host notice names the requester and **Manage devices**
  opens sharing controls. Update a browser page's own host from another client;
  verify its explicit Reload prompt preserves typed composer input. Capture
  both themes. The notice may be classified as tests-only when no controlled
  release host is available; do not run the CI seeded harness locally.
  If remote protection fails on a WSL secondary, the error must instruct you
  to finish the update on the host; remote clients cannot exclude that backend.

- in **Settings → Remote Servers → Share this host**, generate an **Another
  device** offer. Confirm the local server restarts before the pairing offer is
  shown, and that the result contains the browser URL, `bibcode://` deep link,
  pairing code, and QR code. Then inspect the firewall rule:

  ```powershell
  netsh advfirewall firewall show rule name="BiBCode Remote Access"
  ```

  Confirm it is enabled, program-scoped to the exact packaged executable,
  TCP-only, and limited to Domain/Private profiles. Revoke the final
  native-managed **Another device** offer or paired client, verify exposure
  returns to loopback, and confirm the
  named rule is absent. Record an elevation or policy denial as failed native
  evidence; do not substitute a manually created rule. Run the host-independent
  deletion-spawn and policy-denial tests, then reproduce a deletion denial
  natively and confirm the app reports incomplete cleanup rather than claiming
  the rule was removed. A missing rule is benign when `netsh advfirewall firewall show rule`
  reports that no rule matches. On a non-English display language that sentence
  is translated and the command exits 1, so absence also requires a successful
  `netsh advfirewall show allprofiles state` before the miss is accepted. A
  failed policy read is still incomplete cleanup. Deletion uses the
  same `netsh` program, not PowerShell: on the Windows ARM64 runner a
  PowerShell firewall query did not finish inside the five-second caller
  budget, the widen failed closed, and the another-device grant was never
  minted. Capture the shared
  runbook's four explicit ceremony outcomes: authoritative local-only
  confirmation even after cancellation failure, another live access reason kept
  wide, cancellation and cleanup both unconfirmed, and cleanup topology
  unverified. Also cover the three-pass/five-second reconciliation retry and
  terminal warning toast, last-browser-session revocation
  with a local-only restart and removed rule, one compensating widen during a
  concurrent grant, bounded handling of a blackholed create response, and
  explicit legacy resume after a local-only restart. Confirm the caller returns
  a bounded failure after five seconds even when process spawn is delayed; the
  firewall worker must retain ownership, remove and verify absence of any rule
  enabled after that deadline, and complete that cleanup before a later enable.
  Burst multiple requests while one command is in flight and confirm the worker
  retains only the latest pending desired state, reports superseded callers
  explicitly, and applies that latest state after mandatory late cleanup.
  Separately confirm a hung `netsh` child is terminated and reaped
  by its 15-second process timeout and never retains the exposure coordinator
  indefinitely;

- with WSL-only primary mode active, generate an **Another device** offer from
  a usable WSL advertised endpoint. Confirm the native Windows backend process,
  native exposure state, and `BiBCode Remote Access` firewall rule do not change;
  the ceremony and reconciler must not call the native exposure bridge for this
  topology. In the Exposure section, confirm the available off-host WSL URL is
  shown, exposure is described as externally managed by WSL/Hyper-V policy, and
  the native-only **Limited to this machine** and **Managed automatically** copy
  is absent. Start one native reconciliation before the
  switch and let it resume after WSL-only becomes active; work that has not
  applied must produce no exposure side effects. Separately unmount after a
  local-only apply commits and prove its authoritative refetch and one required
  compensating widen still complete. A direct native exposure bridge invocation
  after the switch must be rejected by the host-side topology guard;

- the address picker lists only usable IPv4 candidates until a dual-stack
  listener exists, displays IP addresses (or endpoint hostnames) instead of
  network labels while retaining **Automatic (LAN)**, uses stable address/port
  IDs, safely preselects a private
  default, reports off-host interface observations unavailable before widening,
  and leaves generation disabled with externally managed listener/reverse-proxy
  guidance when native discovery has only a public or non-default private
  address. Public interface candidates remain non-actionable even after native
  exposure is wide. A custom off-host address mints without changing the native
  listener or firewall rule, and later auth revisions do not widen it. An
  externally managed public endpoint is never preselected and requires an
  explicit public-address/firewall warning. A packaged Tailscale CLI is
  discovered without shell `PATH` and unusable, public, or IPv6 candidates are
  suppressed;
- seed an incompatible newer connection IndexedDB version and confirm the
  boot-level recovery dialog lists the deleted data classes, keeps **Reload** as
  a non-destructive exit, requires a separately acknowledged confirmation that a
  double-click cannot trigger, and treats a blocked deletion as visibly queued
  until the original request succeeds or errors. It must not reload while
  blocked or after failure, and reloads automatically only after success;
- open a hosted `/pair` link whose host includes an IDN and explicit port and
  confirm the normalized punycode host shown is exactly the destination used.
  Reject a target containing username/password, and confirm legacy `code` query
  parameters are removed from both `/pair` and Remote Servers history after
  being retained for the current attempt;

- from the OS, opening a well-formed `bibcode://pair?code=...` link while the
  packaged app is running focuses that instance and lands on Add Server with
  the code prefilled;
- provider settings and action menus contain Claude, Codex, Cursor, and
  OpenCode without Early Access labels and omit Grok/Grok Terminal;
- external worktrees group by parent, expose full paths accessibly, adopt
  idempotently through junction/case aliases, and persist across restart;
- sidebar menus use the in-app menu: separators split the groups and are
  never doubled or at an edge. From the keyboard, focus starts on the first
  enabled item, the arrow keys skip separators but land on disabled items
  (focus is visible, the reason stays readable, and **Enter**, **Space** or a
  click leaves the menu open), **Home** and **End** jump to the ends, **→** and
  **←** open and close **Open in**, **Enter** or **Space** chooses, and
  **Escape** closes the menu and returns focus to the card or **⋯**.
  **Shift+F10** and the **Menu** key on a focused card open the menu once, at
  the card;
- thread creation, switching, persistence, terminal I/O, and panel switching
  work;
- Activity subagents/background tasks align, show realistic elapsed time, and
  support keyboard focus/Shift+Tab; and
- narrow overlays and menus remain contained and reachable.

Record unavailable authenticated provider scenarios instead of substituting a
different executable or exposing credentials.

If a packaged run stops at "Tauri did not report the replacement desktop
document finishing its load", and screenshots then time out, look for a native
dialog left open before suspecting WebView2. Search `app-server.log` and the
WDIO logs for `desktop_bridge_pick_folder`, `pick_files`, `save_diagnostic_logs`
or `confirm` calls that never answered: on #177 an unmocked folder picker opened
a real dialog twice, and while dialogs blocked Tauri's async runtime the next
page's own scripts were never served. Dialog commands now await the dialog
without holding a runtime thread (see [Boundaries and invariants](../architecture/overview.md#boundaries-and-invariants)),
so an open dialog only fails the step that waited for it. A real dialog on a
runner still means a mock was missed; fix the mock rather than the timeout.

## Profiler crash smoke

Run the shared [Profiler crash smoke](./cross-platform-validation.md#profiler-crash-smoke)
against a symbol build, as the interactive account. Install the pinned
`dump_syms` once (`cargo install dump_syms --locked --version 2.3.9`), then:

```powershell
node scripts/build-desktop-artifact.ts --platform win --target nsis --arch x64 --symbols
```

Windows ships the binary as linked, and its symbols come from the PDB: the store
under `target\<triple>\release\symbols` names the module after the PDB. On a
Windows ARM64 host, build with `--arch arm64` and expect
`nativeDumps: "disabled"`: no hook is attached there, so
`BIBCODE_PROFILER_CRASH_TEST` does nothing, and the crash steps are unavailable
evidence.

- **Executable.** Use the release `bibcode-desktop.exe` that the symbol build
  produced under `target\<triple>\release\`, never an installed copy, with
  `$env:BIBCODE_HOME` set to the test-owned root. The release build is a
  GUI-subsystem program, so PowerShell waits for it only when its output is
  piped: run `& $exe profiler status --json | Out-String`, then read
  `$LASTEXITCODE`.
- **Capabilities.** Expect `nativeDumps: "available"` on x64 and
  `osCrashRecords: "available"`. While the profiler is on, the per-user
  `%TEMP%` holds one `bibcode-<random>` directory with `monitor.sock`.
- **Crash.** Set `$env:BIBCODE_PROFILER_CRASH_TEST = '1'`, start the app, and
  remove the variable from the shell once it has started. The callback leaves
  the crash to Windows Error Reporting after the dump, so the process ends with
  an access violation (`0xC0000005`), an Application log Event 1000 is written,
  and an interactive desktop may show the "stopped working" dialog. Record
  whether it appeared. Dismiss it before the restart: until then the crashed
  process may remain, and the single-instance plugin would hand the new launch
  to it. After the restart, confirm the Event 1000 record with
  `Get-WinEvent -FilterHashtable @{ LogName = 'Application'; Id = 1000 } -MaxEvents 5 | Format-List TimeCreated, Message`.

Then perform these native checks and record each result, including unavailable
ones:

- **WebView2 failure report.** Crash a WebView2 process of the main window
  where the harness allows it, for example by navigating it to `edge://crash`
  from the packaged E2E WebDriver session. Diagnostics lists a `crash.webview`
  finding named after the failure kind, such as `render-process-exited`, with
  `exitCode` in its evidence. The profiler looks for WebView2's minidump once,
  right after the failure, and adds its `dump` file name when it is already
  there: record whether it was. Record where WebView2 wrote the minidump:
  directly in the folder that
  `FailureReportFolderPath` names, or in its `reports` subfolder. Ending the
  renderer from Task Manager is a deliberate termination and must add no
  finding. Record the crash case as unavailable when no route exists.
- **Provider exit code.** With the profiler on and a provider session running,
  end the provider's root process with `Stop-Process -Id <pid> -Force`, and
  record whether its Job still held other processes. Within seconds, a
  `subprocess.crash` finding reports "exited with code -1" (`Stop-Process`
  ends the process with `0xFFFFFFFF`): the exit code is read from
  the root process handle without waiting for the whole Job to end. Record the
  delay.
- **Shutdown.** With the profiler on and a provider session and a terminal
  open, quit the app normally. After the next start no new `crash.webview`
  (a `ProcessFailed` raised during shutdown) or `subprocess.crash` finding
  exists.
- **Windows ends the session.** With the profiler on and the app open, sign
  out of Windows (or shut down) without quitting BiBCode. After signing back in
  and starting the app, `profiler findings --json` holds no new `crash.panic`
  (`cannot move state from Destroyed`), `crash.unclean-exit` or `crash.system`
  finding for that run, and `session.json` from before the start recorded
  `exit.cause` `session-end` (#102). A virtual machine you can sign out of is
  enough; a CI runner cannot end its own session.
- **Console attach.** Run `profiler status` from `cmd.exe` and from
  PowerShell, with and without piping the output. The executable borrows the
  calling console, so `cmd.exe` returns its prompt at once: record whether the
  output prints over the next prompt and whether `%ERRORLEVEL%` or
  `$LASTEXITCODE` holds the exit code, and whether `start /wait` changes that.
- **MCP over piped stdio.** Run
  `'{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | & $exe profiler mcp`.
  Standard output must carry exactly one JSON-RPC line listing the five
  `profiler_*` tools, and the process must exit with code 0 at the end of
  input. Then register the same command in an MCP client and call
  `profiler_status`.
- **Update with the profiler on.** With the profiler on, an MCP server from
  the step above running, and the app started with `$env:BIBCODE_HOME` and
  `$env:BIBCODE_PORT` set, install an update through the in-app updater. Record
  that the passive installer ended the MCP server, that the app came back on
  the same data root and port, and that `%TEMP%\bibcode-desktop-restart.json`
  is gone once it is running. After the restart, `profiler findings --json`
  holds no new `crash.unclean-exit` or `shutdown.incomplete` finding: the update
  quiesced the server and marked the session clean first. Then quit the app
  and start it again from its shortcut: it uses the default data root and
  port, not the update's.

## Android device previews

Run the shared [Android device previews](./cross-platform-validation.md#android-device-previews)
procedure with this machine as the server host, and record these platform
values:

- **Acceleration.** `emulator -accel-check` (WHPX or AEHD) and whether
  Hyper-V or Windows Hypervisor Platform is on.
- **Discovery directories.** `%LOCALAPPDATA%\Temp\avd\running`, then
  `%ANDROID_EMULATOR_HOME%\avd\running` or `%USERPROFILE%\.android\avd\running`.
- **Process check.**
  `Get-Process | Where-Object { $_.ProcessName -match '^(emulator|qemu-system)' }`
  returns nothing after stop, after the setting is turned off, and after
  server shutdown. The emulator runs in its own Job object, so record any
  survivor's parent (`Get-CimInstance Win32_Process`).
- **WebView.** The desktop client's WebView2 runtime version, and whether it
  showed video or still images with the "Video isn't available" reason.

## Process and Job cleanup

Capture PID, parent PID, executable, creation time, and command line for scoped
desktop, server, provider, terminal, WSL, WebDriver, and fixture processes.
Use exact identity before terminating anything. Confirm Job-owned descendants
and canceled children are waited before ownership is released, independent app
instances remain alive, and the final snapshot contains no process launched by
the run.

Run a terminal command that exits immediately, then close its terminal after
observing completion. Also close while the command is finishing. Both paths
must complete without an exit-wait timeout or a surviving process. The native
PTY regression covers exit before any subscriber exists and verifies that a
late subscriber receives the retained completion.

Remove the junction before its target, then remove only the exact fixture and
profile roots created by this run. Never delete a pre-existing `%TEMP%`, build,
or user directory.

After the last packaged launch, confirm no BiBCode, WebDriver, or Tauri process
from the run survives, then remove any firewall rules Windows generated for the
exact test executable and prove none remain. From an elevated PowerShell:

```powershell
Get-Process | Where-Object { $_.Path -and ($_.Path -ieq $env:BIBCODE_E2E_APP_PATH) }
node scripts/remove-test-firewall-rules.ts --executable $env:BIBCODE_E2E_APP_PATH --dry-run
node scripts/remove-test-firewall-rules.ts --executable $env:BIBCODE_E2E_APP_PATH
Get-NetFirewallApplicationFilter |
  Where-Object { $_.Program -ieq $env:BIBCODE_E2E_APP_PATH } |
  Get-NetFirewallRule
```

The helper selects rules by that exact program path, refuses installed
locations, never removes `BiBCode Remote Access`, and exits non-zero unless the
final query returns zero matching rules; the last command must print nothing.
Record the rule names removed and the empty verification in the report.

## Linux and macOS compatibility audit

Audit that Windows fixes do not introduce drive letters, backslashes,
case-folding, handles, Jobs, PowerShell, or WSL into unguarded shared Unix code.
Run host-independent Linux/macOS contracts and source-inclusion tests. Confirm
Unix physical identity, process groups, local-only presentation, package
contracts, and provider/Activity behavior remain unchanged. Report this as
compatibility evidence, not a native pass.

## Report and cleanup

Complete [the execution report template](./execution-report-template.md), then
perform the shared cleanup and final Git audit. Report whether WSL was usable,
which distributions were exercised, Authenticode status, any native command
that could not run, and whether anything was pushed.
