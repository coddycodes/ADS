# macOS Desktop Validation

Read [Cross-platform validation](./cross-platform-validation.md) first. This
page contains only native macOS additions.

## Supported native targets

The supported release targets are macOS 11 or newer on Apple Silicon arm64 and
Intel x64. A native run proves only the host architecture unless another native
Mac executes the other artifact. Record macOS build, hardware architecture,
and whether translation is involved.

Standalone server releases provide native ARM64 and x64 `.tar.gz` archives containing
`bibcode` and the matching web client. Validate the archive matching the native host.

## Host and toolchain inventory

Record:

```sh
sw_vers
uname -a
uname -m
sysctl -n hw.optional.arm64 2>/dev/null || true
xcodebuild -version
git --version
gh --version
rustc -Vv
cargo -V
rustup show
node --version
vp --version
```

Record available signing identities and notarization credentials without
printing secrets. Ordinary repository builds use the current release policy;
do not import or invent credentials for a local validation.

## Focused macOS contracts

Select focused tests from affected source and verify at least:

- symlinked ancestors such as `/tmp` and `/private/tmp` resolve to one physical
  worktree identity;
- persisted display path spelling remains unchanged;
- external-worktree adoption and restart remain idempotent;
- worktree removal fails closed when physical identity cannot be proven;
- Unix process-group ownership survives cancellation and natural leader exit,
  reaps late descendants, does not signal peer-runtime roots, and remains
  independent of the Windows-only Job implementation;
- desktop login-shell PATH hydration accepts a complete delimited result
  without waiting for shell exit or pipe EOF, then terminates and reaps the
  probe group before startup continues, and remains usable when the desktop
  process inherits a controlling terminal;
- Local desktop presentation omits WSL controls while saved remote environments
  remain selectable in the environment rail without bypassing desktop-owned
  connection controls;
- Claude, Codex, Cursor, and OpenCode remain visible while Grok is absent;
- Activity observation timestamps and keyboard navigation remain correct; and
- update protection treats long-lived read subscriptions as reads, reports
  staged progress and active mutation counts while preparing, rejects a forged
  first-attempt bypass, and offers the acknowledged no-backup path only after a
  real protection failure; and
- app/DMG identity and updater security tests remain green.

Run affected concurrency-sensitive owners at default, 8, and 12 harness
threads without serializing broad suites.

## External worktree and symlink fixture

Use a unique test-owned root outside a user project:

```sh
run_root=$(mktemp -d /private/tmp/bibcode-macos-validation.XXXXXX)
repository="$run_root/Repository With Spaces"
worktrees="$run_root/External Worktrees"
alias_root="$run_root/Symlink Alias"
mkdir -p "$repository" "$worktrees"
git -C "$repository" init
git -C "$repository" config user.name 'BiBCode Test'
git -C "$repository" config user.email 'bibcode-test@example.invalid'
printf 'fixture\n' >"$repository/README.md"
git -C "$repository" add README.md
git -C "$repository" commit -m 'fixture baseline'
git -C "$repository" worktree add "$worktrees/Feature Alpha" -b feature-alpha
mkdir -p "$worktrees/feature-long-path/nested"
git -C "$repository" worktree add "$worktrees/feature-long-path/nested/candidate" -b feature-long
ln -s "$worktrees/Feature Alpha" "$alias_root"
git -C "$repository" worktree list --porcelain
realpath "$worktrees/Feature Alpha"
realpath "$alias_root"
```

Also record the physical identity of any fixture created through `/tmp` and its
`/private/tmp` spelling. Confirm all aliases map to one BiBCode owner while the
UI retains useful Git-reported display spelling. Exercise discovery, adoption,
**Add all**, **Keep hidden**, restart, and final on-disk existence only with the
fixture worktrees.

## Native tests and static gates

Run the shared focused and sequential broad/static gate set. Record macOS
linker, compact-unwind, process-signal, WebKit, signing, and DMG diagnostics
with their affected test or artifact. Do not suppress a warning without
classifying it. The known `ld: __eh_frame section too large (max 16MB) to
encode dwarf unwind offsets in compact unwind table` message on the
`bibcode-server` library test binary is benign: the unoptimized test binary
exceeds the compact-unwind encoding limit, so the linker keeps DWARF unwind
tables for it and only exception-unwinding performance of that test binary is
affected. It is not emitted for shipped release artifacts; report it with the
affected test target rather than suppressing the `linker_messages` lint.

Validate Objective-C recovery with the actual optimized profile:

```sh
rust_target="$(rustc -Vv | sed -n 's/^host: //p')"
cargo run --release --target "$rust_target" -p bibcode-desktop --example objc_exception_probe
```

The probe must catch its native exception and exit successfully. The workspace
release profile preserves unwinding because Wry relies on it when a custom
URL-scheme task is cancelled during navigation. The macOS desktop crate rejects
`panic=abort`; do not disable that guard to produce an installer.

For update validation, use an isolated `BIBCODE_HOME` and disposable project.
Keep a read subscription open while installing an available test update and
confirm it does not block protection. During a deliberately held mutation,
confirm the dialog promptly shows the waiting stage, elapsed time, and an
active-operation count. After the bounded failure, verify that normal retry is
still the primary action, the no-backup action requires acknowledgement, a
forged first-attempt bypass is rejected by the native host, and an installer
failure restarts the exact pre-update backend set.

With the same isolated test instance, arrange an installer failure and have a
test-owned listener acquire its backend port after shutdown, keeping it bound
past the 3 s restart window. Confirm **Update not installed** names that port
as in use, offers **Restart server**, and disables **Retry installation** with
the visible restart explanation. Keep an unsent composer draft and verify it
survives the outage. Release only the test listener, choose **Restart server**,
and confirm the same backend port reconnects, the draft remains, and **Retry
installation** becomes available without reopening the dialog. Record the
listener/installer fixture and observed port in the execution report; never use
the user's running instance for this check.

## Application and DMG build inspection

The CI-only seeded-upgrade matrix gives Intel x64 packaging children 90 minutes
and its complete job 360 minutes. ARM64 keeps 45-minute packaging children and
a 240-minute job. There are three sequential cold packages with separate Cargo
outputs (candidate, previous stable, protected baseline), each with a separate
10-minute frozen install; the cached repository `target` does not warm these
outputs. Intel's remaining 60 minutes cover setup, all upgrade lanes, evidence,
and cleanup. `remote-install` reuses the protected package. These allowances
leave product, WebDriver, and restart deadlines intact. Verify retries once
when WebKit reports `Load failed` or the shell snapshot is not ready yet, and
it still requires the candidate version, retained project, and storage
identity. Record a packaging
timeout separately from native upgrade results, and never run this CI harness
locally. See the [seeded matrix procedure](../operations/release.md#seeded-packaged-upgrade-matrix).

Build the host-native artifact:

```sh
vp run dist:desktop:dmg
```

Discover the generated DMG and `.app`. Read the actual executable and version
from `Info.plist` rather than assuming that the display name equals the binary
name:

```sh
plist="/absolute/path/BiBCode.app/Contents/Info.plist"
executable=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$plist")
/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$plist"
/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$plist"
test -x "/absolute/path/BiBCode.app/Contents/MacOS/$executable"
codesign --verify --deep --strict --verbose=2 /absolute/path/BiBCode.app
```

Mount the exact test-owned DMG read-only at a fresh mount point, inspect the
contained application, and detach that mount during cleanup. Report configured
signing, ad-hoc signing, and notarization separately. Never claim notarization
when credentials or a notarized ticket are absent.

Before publishing a macOS release, verify the Finder-rendered icon from that
mounted payload with `swift scripts/check-macos-app-icon.swift` and its exact
application path. The release workflow runs this check on both native macOS
targets after recursive signature verification and before upload. A failed icon
check is a failed native build; a check against a different installed bundle
does not qualify the release DMG.

The release producer hosts are macOS 26 ARM64 and supported macOS 15 Intel for
both desktop and standalone server artifacts. Native Intel main CI, packaged
UI, and upgrade validation retain macOS 26. The producer hosts run the same
mounted-application signature and Finder pixel criteria, with all platform
rows and updater targets intact. macOS 15 Intel evidence cannot establish a
macOS 26 Intel Finder pass. Record that distinction in execution reports;
neither producer selection nor lookup-only controls qualify another OS image.

The CI verifier emits closed `mac-icon-observation` stage records to stderr and
flushes them individually. These locate Swift entry, workspace icon retrieval,
bitmap and drawing-context creation, icon drawing, pixel scanning, and the existing verdict.
The scan-start record includes capped bitmap dimensions; scan-return includes
capped opaque, dark, and pale counts. Diagnostic caps never change the scan or
verdict calculations.
The workflow records Swift dispatch/success and cleanup entry/return; cleanup
return alone does not verify detach success. A missing record remains unobserved.
Do not classify an icon pixel defect, compiler hang, or native service failure
without the corresponding boundary or verdict evidence. The native payload,
thresholds, platform rows, and existing job budget remain required.

The verifier rasterizes the workspace-provided image at 256 points into a fixed
1024-square RGBA bitmap, avoiding serialization of every image representation
through TIFF. Both macOS release jobs first run
`swift scripts/check-macos-app-icon.swift --self-test` in CI. These fixtures
exercise the same renderer and pixel verdict with opaque black, opaque white,
transparent, and ratio-boundary images. They preserve the existing alpha,
luminance, dark-area, and pale-area limits. Their pass does not replace the
mandatory check against the exact mounted DMG application.

Build and run packaged E2E with:

```sh
export BIBCODE_E2E_PLATFORM=mac
export BIBCODE_E2E_ARCH="$(node -p process.arch)"
case "$BIBCODE_E2E_ARCH" in
  arm64) rust_target=aarch64-apple-darwin ;;
  x64) rust_target=x86_64-apple-darwin ;;
  *) echo 'Unsupported macOS architecture' >&2; exit 1 ;;
esac
vp run test:ui:desktop:build

dmg=$(find "$PWD/target/$rust_target/release/bundle/dmg" -maxdepth 1 -type f -name 'BiBCode_*.dmg' -print -quit)
test -n "$dmg"
mount_dir=$(mktemp -d /private/tmp/bibcode-macos-e2e-mount.XXXXXX)
install_dir=$(mktemp -d /private/tmp/bibcode-macos-e2e-install.XXXXXX)
cleanup_e2e_mount() {
  hdiutil detach "$mount_dir" >/dev/null 2>&1 || true
  rmdir "$mount_dir" 2>/dev/null || true
}
trap cleanup_e2e_mount EXIT HUP INT TERM
hdiutil attach -readonly -nobrowse -mountpoint "$mount_dir" "$dmg"

ditto "$mount_dir/BiBCode.app" "$install_dir/BiBCode.app"
executable=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$mount_dir/BiBCode.app/Contents/Info.plist")
cmp "$mount_dir/BiBCode.app/Contents/MacOS/$executable" "$install_dir/BiBCode.app/Contents/MacOS/$executable"
codesign --verify --deep --strict "$install_dir/BiBCode.app"
export BIBCODE_E2E_APP_PATH="$install_dir/BiBCode.app"
test -d "$BIBCODE_E2E_APP_PATH"
vp run test:ui:desktop
```

`BIBCODE_E2E_APP_PATH` selects the isolated installation copied from the current
worktree's E2E DMG. The DMG-only bundler removes its transient staging `.app`;
mount the resulting image to inspect and copy its signed payload, then validate
the installed copy as in the normal macOS installation flow. Binary comparison
and signature verification bind the test to that exact payload. Keep the cleanup
trap active until WebDriver and the packaged application have exited. Retain the
installation directory with the execution evidence for inspection.

## Renderer-data isolation

Prefer the E2E bundle identifier and isolated `BIBCODE_HOME` so the packaged
test cannot read a user's normal project store. Resolve the exact bundle
identifier from `Info.plist`; never target renderer data by display name.

If WebKit still uses a pre-existing profile for that exact bundle identifier,
stop before mutation and obtain authority to isolate it. With authority:

1. prove the exact application and related processes are absent;
2. inventory exact WebKit/cache roots, counts, sizes, ownership, permissions,
   and content/hash evidence;
3. move only those exact roots to a unique backup outside the active path;
4. launch the exact worktree bundle with isolated application data;
5. quit and prove all exact processes are absent;
6. move fresh test-created renderer roots to a separate evidence directory;
7. restore the original exact roots; and
8. compare the retained inventory and report any restoration discrepancy.

Do not delete user renderer data, Preferences, credentials, or an ambiguous
directory. If exact restoration cannot be proven, preserve both copies and
report the residual instead of attempting a destructive repair.

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

Use Codex Computer Use to operate the packaged executable. Confirm the
executable path and PID before using any frame as evidence. At normal and
minimum sizes verify:

- when only the local Mac environment is configured, the rail shows Local and
  Add Project has no remote target; saved remote environments appear as separate
  rail entries and become the Add Project target when selected, and Add Project
  then shows a **Host** selector offering this device and those servers;
- in the **Repositories** view, the header's Add Project button opens the dialog
  with the **Host** selector; choosing a remote server makes **Browse folder**
  and the clone and create **Browse…** buttons list that server's folders;
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
  macOS remote host: use `pgrep -f "bibcode serve"` or the process list to find
  the test server's PID, then run `kill -STOP <pid>` on that host. Hover the
  Settings row's status dot and wait for
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
  Run `kill -CONT <pid>` on the remote host to resume
  the same test server and confirm it reconnects;
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
  grant-driven sharing appears because the desktop bridge is present. Generate
  an **Another device** offer, verify the restart completes before the browser
  URL, deep link, pairing code, and QR code appear, then revoke the final
  native-managed **Another device** offer or client and verify exposure returns
  to loopback. Capture the
  shared runbook's four explicit ceremony outcomes: authoritative local-only
  confirmation even after cancellation failure, another live access reason kept
  wide, cancellation and cleanup both unconfirmed, and cleanup topology
  unverified. Also cover the three-pass/five-second reconciliation retry and
  terminal warning toast, last-browser-session
  revocation, one compensating widen during a concurrent grant, bounded handling
  of a blackholed create response, and explicit legacy resume after a local-only
  restart. The address picker lists only usable IPv4 candidates until a
  dual-stack listener exists, displays IP addresses (or endpoint hostnames)
  instead of network labels while retaining **Automatic (LAN)**, uses stable
  address/port IDs, safely preselects a
  private default, reports off-host interface observations unavailable before
  widening, and leaves generation disabled with externally managed
  listener/reverse-proxy guidance when native discovery has only a public or
  non-default private address. Public interface candidates remain
  non-actionable even after native exposure is wide. A custom off-host address
  mints without changing the native listener or firewall, and later auth
  revisions do not widen it. An externally managed public endpoint is never
  preselected and requires explicit public-address/firewall acknowledgement. A
  packaged Tailscale installation must be found without shell `PATH` and must
  suppress unusable, public, or IPv6 candidates. Confirm macOS firewall
  management remains explicitly operator-owned. The local-machine flow still
  has no Host selector; remote targeting is driven by the environment rail;
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
  packaged app is running lands on Add Server with the code prefilled. The
  custom scheme is bundle-time-only on macOS and is not an unbundled dev-mode
  check;
- provider settings and action menus show Claude, Codex, Cursor, and OpenCode
  without Early Access labels and omit Grok/Grok Terminal;
- external worktree grouping, full paths, actions, physical identity, and
  restart are correct;
- sidebar menus: right-click a worktree card, the primary card and a project
  header, and open the header's **⋯** from the keyboard (Tab to it, then
  **Enter**). Each native menu separates Open in/Pull, the copy actions,
  Pin/Unread/Rename and the destructive item, with no doubled separator before
  **Delete Worktree…** or **Remove Project…**, and **⋯** shows the same items
  as the header's right-click menu. Tab to a card and press **Shift+F10**
  (and, on Linux, the **Menu** key): exactly one native menu opens at the card,
  and no error toast reports a second menu;
- thread creation/switching, terminal I/O, Activity elapsed time, subagent row
  layout, background tasks, keyboard focus, and Shift+Tab work; and
- narrow panels, menus, overlays, icons, and focus states remain contained.

Capture original-resolution screenshots and focused crops. Exclude an installed
application splash or any frame from a different PID/bundle from acceptance
evidence.

## Profiler crash smoke

Run the shared [Profiler crash smoke](./cross-platform-validation.md#profiler-crash-smoke)
against a symbol build. Install the pinned `dump_syms` once
(`cargo install dump_syms --locked --version 2.3.9`), then build for the host
architecture:

```sh
node scripts/build-desktop-artifact.ts --platform mac --target dmg --arch arm64 --symbols   # Apple silicon
node scripts/build-desktop-artifact.ts --platform mac --target dmg --arch x64 --symbols     # Intel
```

The symbol build also bundles the `.app`, because the wrapper's post-bundle
check reads it: the check must pass, confirming that the `.app` binary, which
the bundler re-signs, has the stripped binary's build ID and symbol table.
Record its output, then run the `codesign --verify --deep --strict` step from
[Application and DMG build inspection](#application-and-dmg-build-inspection)
on that `.app`. The symbol store is `target/<triple>/release/symbols`.

- **Executable.** Read `$executable` from the bundle's `Info.plist` as in
  [Application and DMG build inspection](#application-and-dmg-build-inspection),
  start `BIBCODE_HOME=<root> "/absolute/path/BiBCode.app/Contents/MacOS/$executable"`,
  and pass the profiler commands to that same path.
- **Capabilities.** Expect `nativeDumps: "available"` and
  `osCrashRecords: "available"` (the DiagnosticReports folder). While the
  profiler is on, `ls -la "$TMPDIR"/bibcode-$(id -u)-*` shows one `drwx------`
  directory holding a `srw-------` `monitor.sock`.
- **Crash.** Use `BIBCODE_PROFILER_CRASH_TEST=1`. On macOS the hook catches
  hardware faults through Mach exception ports and `SIGABRT` through a signal
  handler, so a `kill -SEGV` from another process tests only the OS crash
  report. After the restart, `~/Library/Logs/DiagnosticReports/` holds the
  `.ips` report of the crash.

Then perform these native checks and record each result, including unavailable
ones:

- **Webview crash and reload.** With the profiler on and a thread open, send
  `kill -SEGV` to the main window's `com.apple.WebKit.WebContent` process.
  Quit other WebKit apps first, and record how the process was identified
  (`pgrep -fl com.apple.WebKit.WebContent` before and after the app starts).
  The window must reload by itself with no blank window and no panic, preview
  tabs reload too, and Diagnostics lists one `crash.webview` finding
  (reason `web-content-terminated`). Within a minute
  `profiler findings --kind crash.webview --json` shows the WebContent `.ips`
  report's `source`, `signalOrCode`, and `frames`; record that report's file
  name. A `kill -9` also exercises the reload, but writes no report.
- **Shutdown.** With the profiler on and a provider session and a terminal
  open, quit with **⌘Q**. After the next start no new `crash.webview` or
  `subprocess.crash` finding exists.
- **Stripped arm64 signature.** On Apple silicon, build the standalone server
  as the release job does and verify the stripped copy's ad-hoc signature:

  ```sh
  CARGO_PROFILE_RELEASE_STRIP=none CARGO_PROFILE_RELEASE_DEBUG=line-tables-only \
    CARGO_PROFILE_RELEASE_SPLIT_DEBUGINFO=packed \
    cargo build --release -p bibcode-server --bin bibcode
  strip -o target/release/bibcode.stripped target/release/bibcode
  codesign --verify --verbose=2 target/release/bibcode.stripped
  ```

  The release server job runs the same `codesign --verify`, so a failure here
  predicts a failed release; record it.

## Android device previews

Run the shared [Android device previews](./cross-platform-validation.md#android-device-previews)
procedure with this machine as the server host, and record these platform
values:

- **Acceleration.** `emulator -accel-check` (the Hypervisor framework) and the
  AVD's ABI, which must match the host (`arm64-v8a` on Apple silicon).
- **Discovery directories.** `~/Library/Caches/TemporaryItems/avd/running`,
  then `${ANDROID_EMULATOR_HOME:-$HOME/.android}/avd/running`.
- **Process check.** `pgrep -fl 'emulator|qemu-system'` prints nothing after
  stop, after the setting is turned off, and after server shutdown.
- **WebView.** The desktop client's macOS version (WKWebView), and whether it
  showed video or still images with the "Video isn't available" reason.

## iOS Simulator previews

Run this on a Mac with Xcode set up and at least one `Shutdown` iPhone
simulator, after the shared
[iOS simulator previews](./cross-platform-validation.md#ios-simulator-previews)
focused and opt-in tests:

```sh
BIBCODE_IOS_SIM_TESTS=1 cargo test -p bibcode-server --test ios_simulator -- --nocapture
```

Record `xcodebuild -version`, `xcode-select -p`, the simulator's name, UDID,
and runtime (`xcrun simctl list devices available`), and every simulator that
is already `Booted` before the run; BiBCode must leave those alone. Use the
packaged app from [Application and DMG build inspection](#application-and-dmg-build-inspection)
with its local environment and a fresh `BIBCODE_HOME`, and turn on the
[Profiler](../architecture/overview.md#profiler). After each numbered step
that ends a device, run:

```sh
xcrun simctl list devices | grep <udid>   # Shutdown
pgrep -fl <udid>                          # no process BiBCode started
```

1. **Setting.** Turn on **Settings → Diagnostics → Device previews**. Open
   **Device** from the right panel's `+` menu. The picker lists the simulator
   under **iOS**, named like `iPhone 17 Pro · iOS 26.5`; a simulator already
   booted outside BiBCode is listed disabled with "— Running outside
   BiBCode". If the iOS group says view only, record its message and run the
   view-only checks in step 7 instead of steps 3–5.
2. **Open.** Pick the simulator and **Start**. The pill reaches **Ready**;
   record the boot time. The screen shows video with no **Still images**
   badge within 5 s, and it moves (the clock, an animation).
3. **Touch and Home.** Tap **Settings** on the home screen: it opens. Drag a
   list and release outside the panel: nothing stays pressed. Press **Home**:
   the home screen returns. Press **Lock**, then **Home** to wake it.
4. **Keys and paste.** Open Safari and click its address bar. Type a URL and
   use Backspace, the arrow keys, and Enter: the text and the page follow.
   Type a long all-ASCII string (about 200 characters) without pasting: every
   character arrives in order. Copy text with an accent or emoji (`café 😀`)
   on the Mac, focus the screen, and choose the toolbar's **Paste**: in the
   packaged app this reads the clipboard inside WKWebView; record whether
   macOS asks for permission. iOS may show **Allow Paste**; allow it and
   confirm the text arrives and the panel shows its one-time note. Repeat
   with ⌘V on the focused screen.
5. **Rotate.** Use **Rotate left**, then **Rotate right** twice, then
   **Rotate left**, in an app that rotates (Safari). Record the catalog's
   orientation and the video's size after each: **Rotate left** from portrait
   gives `landscape`, with the status bar at the screen's top and the
   simulator's home indicator at the bottom, and the video keeps playing at
   the new size. Tap a link in landscape: the tap lands where you clicked.
6. **Close.** **Stop** the device: within 30 s the pill shows **Stopped**, and
   the checks above pass.
7. **View only** (when Xcode's interfaces are unsupported, or after switching
   Xcode with `xcode-select` while the server runs). The iOS group shows the
   reason; a started simulator shows **Still images** with "Video is off: …",
   and every input control is disabled with "This simulator is view only".
8. **Setting off.** Start the simulator again, wait for **Ready**, and turn
   the setting off: the confirmation names the simulator, and after it the
   checks pass.
9. **Quit.** Start it once more, wait for **Ready**, and quit the app with
   ⌘Q: after it exits the checks pass.
10. **Profiler.** Run
    `profiler events --kind device.lifecycle --json`: each start has
    `platform` `ios` and the phases `start`, `ready`, `stop`, with no
    simulator name or UDID. Optionally start the device and run
    `xcrun simctl shutdown <udid>` from a shell: the pill shows **Failed:
    Shutdown outside BiBCode** within about 3 s, and
    `profiler findings --kind subprocess.crash --json` lists one labelled
    `ios-simulator` with outcome `exit 1`.

A simulator BiBCode booted that stays `Booted`, a simulator booted before the
run that BiBCode shut down, a stuck touch, or a blank screen without a
fallback fails the run.

## Process-group cleanup

Capture PID, PPID, process group, start time, executable, and command line for
scoped app, server, provider, terminal, WebDriver, mount, and fixture roots.
Confirm bounded terminate/wait/reap, late-descendant cleanup after natural root
exit, peer-runtime isolation, and zero run-owned survivors.

Run a terminal command that exits immediately, then close its terminal after
observing completion. Also close while the command is finishing. Both paths
must complete without an exit-wait timeout or a surviving process. The native
PTY regression covers exit before any subscriber exists and verifies that a
late subscriber receives the retained completion.

Detach only the exact DMG mount. Remove only exact fixture, profile, artifact,
and evidence directories created by the run. Leave pre-existing targets and
processes untouched.

## Windows and Linux compatibility audit

Audit that macOS fixes do not leak `/private/tmp`, Darwin APIs, bundle paths,
WebKit locations, `hdiutil`, or macOS signal behavior into unguarded shared
Windows/Linux code. Confirm Windows file identity/Jobs/WSL and Linux
AppImage/process-group/display contracts remain present in host-independent
tests. Report these as compatibility evidence only.

## Report, restoration, and cleanup

Complete [the execution report template](./execution-report-template.md), then
perform shared cleanup and final Git audit. Include the actual
`CFBundleExecutable`, bundle identifier, signing/notarization classification,
DMG mount/detach evidence, renderer restoration evidence, screenshot paths,
zero-survivor evidence, and whether anything was pushed.

## Source-control launch diagnostics

Use an isolated fixture environment and test-owned hosting executables to verify
launch-not-found and permission-denied failures. Do not retry a real merge
request, inspect provider credentials, or modify the desktop's launch PATH to
create the failure. Use the maintained executable fixture writer and preserve
its directory until the attempted child has settled.

Verify that the create-request dialog retains the selected branches, title and
description, shows the safe launch category and guidance, and offers an explicit
Retry without submitting again automatically. A not-found category must not
claim that a missing CLI is the unique cause. Preserve the existing indication
of any already-published branch; a later failure does not imply rollback.

Confirm one corresponding failed stacked action in Diagnostics. New unary and
action failures have measured server-operation time. Historic zero-duration
records and untimed events show **Not recorded**; measured positive durations
below one millisecond show **<1 ms**. Untimed samples count as occurrences and
failures but do not enter average, maximum or slow-span timing statistics.
Capture only closed failure categories, numeric error codes, hydration outcome,
and verified availability categories; omit raw PATH, credentials, process output
and private repository details. Fixture validation does not establish the cause
of an earlier user incident without that incident's native evidence.
