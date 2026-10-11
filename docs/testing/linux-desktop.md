# Linux Desktop Validation

Read [Cross-platform validation](./cross-platform-validation.md) first. This
page contains only native Linux additions.

## Supported native target

Linux distribution support targets are Debian, Ubuntu, Fedora, and Arch. Desktop
release artifacts are ARM64 and x64 AppImages built on matching Ubuntu 22.04
runners. Record the distribution release, architecture, and desktop session for
each native validation; a passing Git subprocess check alone does not qualify
the complete desktop, installer, or updater on that system.

Standalone server releases add ARM64/x64 `.tar.gz`, `.deb`, and `.rpm` artifacts. Native
package evidence covers Ubuntu 22.04, Ubuntu 24.04, Debian 12, Rocky Linux 9, and Fedora
44 as prescribed by the shared runbook.

## Distribution, desktop, and toolchain inventory

Record:

```sh
cat /etc/os-release
uname -a
uname -m
printf 'desktop=%s\nsession=%s\nwayland=%s\ndisplay=%s\n' \
  "$XDG_CURRENT_DESKTOP" "$XDG_SESSION_TYPE" "$WAYLAND_DISPLAY" "$DISPLAY"
gsettings get org.gnome.desktop.interface font-hinting
gsettings get org.gnome.desktop.interface font-antialiasing
git --version
gh --version
rustc -Vv
cargo -V
rustup show
node --version
vp --version
```

On a non-GNOME desktop record the equivalent hinting and antialiasing settings
instead. The host keys its hinting pin off the GTK `gtk-xft-hintstyle` value,
so any desktop that produces `hintfull` exercises it.

Check WebKitGTK, Tauri/AppImage, FUSE, graphics, and display dependencies
against current CI and release workflows. Use Xvfb for packaged E2E when the
host has no suitable interactive display. Do not install system packages
without permission.

Linux AppImages bundle WebKitGTK from their build host. Isolate XDG config,
cache, and data roots when running locally built AppImages on a newer
distribution (for example Fedora with WebKitGTK 2.52); otherwise they can
migrate IndexedDB metadata that a CI AppImage built on Ubuntu 22.04 with
WebKitGTK 2.50 cannot reopen. If an older build reuses that data root, expect
the connection-database reset dialog. On Linux the connection catalog lives in
IndexedDB, so that reset also deletes saved servers and credentials; isolate
the data root before the first launch rather than resetting afterwards.

## Focused Linux contracts

Select focused tests from affected source and verify at least:

- a symlinked ancestor and its physical path resolve to one worktree owner;
- persisted display path spelling is not replaced by canonical identity;
- repeated external-worktree adoption and restart remain idempotent;
- destructive worktree removal validates the registered physical identity and
  fails closed on replacement;
- Unix process groups retain one signal/wait/reap owner through cancellation,
  natural root exit, and late descendants, independently of the Windows-only
  Job implementation;
- independent runtimes cannot terminate each other's process roots;
- Local desktop presentation omits WSL controls while saved remote environments
  remain selectable in the environment rail without bypassing desktop-owned
  connection controls;
- Claude, Codex, Cursor, and OpenCode remain visible while Grok is absent; and
- update protection treats long-lived read subscriptions as reads, reports
  staged progress and active mutation counts while preparing, rejects a forged
  first-attempt bypass, and offers the acknowledged no-backup path only after a
  real protection failure; and
- Linux AppImage/desktop identity and taskbar behavior remain covered.

Run affected concurrency-sensitive owners at default, 8, and 12 harness
threads without serializing broad suites.

## External worktree and symlink fixture

Use a unique test-owned root:

```sh
run_root=$(mktemp -d "${TMPDIR:-/tmp}/bibcode-linux-validation.XXXXXX")
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

Confirm the real path and symlink alias produce one BiBCode owner while the UI
retains the useful Git-reported display path. Exercise discovery, individual
adoption, **Add all**, **Keep hidden**, restart, and on-disk existence using
only fixture worktrees. Include the host's symlinked temporary-directory
behavior when applicable.

## Native tests and static gates

Run the shared focused tests and sequential broad/static gate set. Confirm
Linux-only tests are not filtered unexpectedly and record any AppImage,
WebKitGTK, X11, or Wayland diagnostic.

Run the AppImage GTK wrapper regression on Linux:

```sh
vp test scripts/tauri-linuxdeploy-plugin-gtk.test.ts
```

It covers Wayland library removal, the generated backend export, removal of
forced theme selection, user theme overrides, missing and drifted hooks failing
without post-processing mutations, discovery passthrough, and upstream failure
propagation.

Do not run `vp run test` and a separate broad Cargo command concurrently. Do
not replace the normal Rust test harness with a serial harness.

For update validation, isolate `BIBCODE_HOME` plus XDG config, cache, and data
roots and use a disposable project. Keep a read subscription open while
installing an available test update and confirm it does not block protection.
During a deliberately held mutation, confirm the dialog promptly shows the
waiting stage, elapsed time, and an active-operation count. After the bounded
failure, verify that normal retry is still the primary action, the no-backup
action requires acknowledgement, a forged first-attempt bypass is rejected by
the native host, and an installer failure restarts the exact pre-update backend
set.

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

## AppImage build and inspection

Use the Ubuntu 22.04 release runners for release AppImage qualification. A
supported runtime distribution may have a newer build-host library layout
that the pinned bundler does not support. In particular, newer Arch
GdkPixbuf packages can omit the legacy loader directory that the GTK plugin
copies. The wrapper checks `pkg-config --variable=gdk_pixbuf_moduledir
gdk-pixbuf-2.0` before invoking the plugin, and refuses a missing/empty loader
path with an actionable build-host diagnostic. Missing package metadata also
fails before the plugin can modify the AppDir. Record the package versions and
advertised path; build with the documented native-architecture Ubuntu 22.04
baseline or use an official release AppImage on the newer runtime distribution.
This does not add Glycin bundling support. Do not create an empty loader
directory, downgrade desktop libraries, or bypass the pinned plugin to make
packaging pass. `--plugin-api-version` discovery does not require GTK metadata.
Successful local DEB/RPM packaging does not qualify the AppImage; its release
build and artifact checks must still pass on the documented baseline.

### User-facing child environment isolation

Validate the [AppImage child environment policy](../architecture/overview.md#appimage-child-environments).
The regressions must cover:

- Each covered production launch class, including chat-mode providers, Claude
  runtime probes, provider inventory probes including Codex, the Codex usage
  app-server helper, Codex/OpenCode helpers, both review Git commands,
  editor/file-manager launches, and desktop-owned SSH, Tailscale, and `kill`.
  Removing a site's isolation call must make its regression fail.
- Library, executable, GTK, Python, Qt, GStreamer, and unknown plugin variables;
  launcher markers and forced settings; mixed and bundled-only values; real
  launcher trailing colons; host credentials; non-path lists; and raw-byte
  values and command overrides. Include colon/semicolon library paths and
  space/colon preloads, plus a planted working-directory library that must not
  load.
- Extracted AppDirs, paths containing spaces, stale absolute `.mount_*` paths,
  the five ordinary-launch no-op cases (`unset-appimage`, `empty-appimage`,
  `relative-appdir`, `root-appdir`, and `unset-appdir`), and the positive and
  negative extracted AppDir gate cases. Exercise both text and binary Git
  output and a real PTY shell and Python process.
- An unchanged complete parent environment and the login-shell PATH-probe
  exemption, including its AppDir-first PATH and original launcher values.
  Each re-executed test phase must prove entry and completion; a successful
  zero-test exit is insufficient.
- File-manager prompt return, survival after the launch call returns, a
  separate process group, eventual reaping, typed spawn errors, and candidate
  fallback order after spawn failure.

Run on Linux with a C compiler and system Git:

```sh
cargo test --locked -p bibcode-server --test linux_appimage_git_environment -j 2 -- --nocapture
cargo test --locked -p bibcode-server --test linux_appimage_child_environment -j 2 -- --nocapture
cargo test --locked -p bibcode-server --lib appimage -j 2 -- --nocapture
cargo test --locked -p bibcode-server --lib file_manager_launch -j 2 -- --nocapture
cargo test --locked -p bibcode-desktop --lib appimage -j 2 -- --nocapture
```

The Git regression must first demonstrate the incompatible fixture library's
loader failure, then pass through the real Git runner. The Python regression
requires a real PTY child to exit zero and print `ok`. Capture its completion
marker with `--nocapture`; to run it separately:

```sh
cargo test --locked -p bibcode-server --test linux_appimage_child_environment python_in_appimage_terminal_uses_host_standard_library -- --nocapture
```

Require `BIBCODE_PYTHON_PTY_RAN` and absence of `BIBCODE_PYTHON_PTY_SKIP` in the
captured stdout. A skip is **unavailable evidence, not a pass**; record it and
rerun on a host with Python.

`.github/workflows/linux-git-compatibility.yml` builds the Git regression on Ubuntu 22.04
and runs the executable with each distribution's system Git in Debian 12/13,
Ubuntu 22.04/24.04, Fedora 44, and Arch rolling containers. The build baseline
matters: a test built against a newer glibc cannot qualify older distributions.
The matrix tests x64 in CI; native ARM64 runs remain required for ARM64 evidence.

To repeat the matrix, pass the executable path from Cargo's
`--message-format=json` compiler-artifact event:

```sh
bash scripts/test-linux-git-compatibility.sh /absolute/path/to/test-executable
CONTAINER_ENGINE=podman bash scripts/test-linux-git-compatibility.sh /absolute/path/to/test-executable
```

Optional trailing arguments select specific image names from the script's
matrix. Package installation is confined to disposable containers; each
container has a bounded timeout and is removed after its result. Record every
distribution result separately, including failures.

For release qualification, also use the packaged app to fetch from a disposable
HTTPS repository on each target distribution, both through Git Manager and a
BiBCode terminal (`git ls-remote origin HEAD`). Confirm the terminal resolves
`xdg-open` and `xdg-mime` to host copies, then run
`python3 -c 'print("ok")'` and require exit zero and `ok` output. Confirm a
configured Claude Code status line whose script invokes Python renders normally.
Open a configured Flatpak editor and a file manager from the app and check for
GLib symbol errors and AppImage-rooted `xdg-open` selection. Repeat terminal
Git/Python and tool launch checks from an extracted `squashfs-root/AppRun`
without `APPIMAGE`, including Ubuntu without libfuse2. Record desktop URL/path
opening, Linux file reveal, deep-link registration, and the GTK hook's
`XDG_DATA_DIRS` limitation separately against the
[current limitations](../architecture/overview.md#appimage-child-environments).
Verify that the server's file-manager launch returns promptly, the running
file manager survives the RPC return, and its exited child is eventually
reaped.
Inspect a running provider's environment for the same isolation and confirm the
desktop still retains its bundled paths and renders correctly. Record the Git
Manager, terminal Git/Python, status line, and provider results, library-loader
diagnostics, and desktop/update checks separately from these subprocess
regressions.

### Native artifact

Build the supported artifact:

```sh
vp run dist:desktop:linux
```

Discover the AppImage under the artifact output, record its absolute path,
verify it is nonempty and executable, and inspect version/architecture without
modifying the artifact. Record whether FUSE is available or the AppImage needs
its supported extraction fallback for testing.

Build and run packaged E2E with:

```sh
export BIBCODE_E2E_PLATFORM=linux
vp run test:ui:desktop:build
export BIBCODE_E2E_APP_PATH="$(find "$PWD/target/$(rustc -vV | sed -n 's/^host: //p')/release/bundle/appimage" -maxdepth 1 -name '*.AppImage' -print -quit)"
test -n "$BIBCODE_E2E_APP_PATH"
WAYLAND_DISPLAY=bibcode-no-wayland xvfb-run --auto-servernum vp run test:ui:desktop
```

The packaged AppImage prefers the Wayland backend, and libwayland connects to
`$XDG_RUNTIME_DIR/wayland-0` even when `WAYLAND_DISPLAY` is unset. On a host
with a live Wayland session the app would therefore open on the real desktop
instead of Xvfb, and document reloads fail the suite. Pointing
`WAYLAND_DISPLAY` at a socket that does not exist makes the Wayland connection
fail and exercises the X11 fallback inside Xvfb; `BIBCODE_GDK_BACKEND=x11`
is the alternative and skips the fallback path.

`BIBCODE_E2E_APP_PATH` deliberately selects the AppImage produced by the E2E
build in the current worktree, not an installed production copy. The E2E build
always passes the host target triple to Tauri, so the bundle lands under
`target/<host triple>/release/bundle/appimage/`, not `target/release/`.

Use the direct E2E command instead of Xvfb when a verified interactive display
is required and available. Isolate BiBCode application data and XDG config,
cache, and data roots for the test process without changing the parent shell or
user profile globally.

With that isolated instance running from the absolute `BIBCODE_E2E_APP_PATH`
on a FUSE-capable test host, perform an update relaunch or **Restart BiBCode**.
Once the replacement window is ready, require exactly one `.mount_` entry for
this AppImage in `/proc/mounts` and one AppImage runtime process. Use a host
where this is the only running BiBCode AppImage; the runtime's mount prefix uses
the first six characters of the artifact name. Both checks below must exit zero
and print exactly one entry; extraction mode cannot supply this FUSE evidence.

```sh
appimage_mount_prefix="/.mount_$(basename "$BIBCODE_E2E_APP_PATH" | cut -c1-6)"
awk -v prefix="$appimage_mount_prefix" '
  index($2, prefix) { print; mounts++ }
  END { exit (mounts != 1) }
' /proc/mounts &&
  ps -ww -eo pid=,args= | awk -v appimage="$BIBCODE_E2E_APP_PATH" '
    { pid = $1; sub(/^[[:space:]]*[0-9]+[[:space:]]+/, "") }
    $0 == appimage || index($0, appimage " ") == 1 { print pid, $0; runtimes++ }
    END { exit (runtimes != 1) }
  '
```

### Manual headless development checks

Use the maintained packaged E2E procedure above for release evidence. For a
manual development check beside an installed app, choose a separate Tauri
identifier: Linux single-instance ownership is keyed by the identifier, so a
second launch with the production identifier can exit successfully without
opening a test window. An identifier alone does not isolate the backend data;
also set a disposable `BIBCODE_HOME` and XDG config, cache, and data roots for
every test process.

With the isolated development graph already running, invoke Tauri from
`apps/desktop` with an identifier override and an empty `beforeDevCommand`.
Set `build.devUrl` to that graph's actual web URL if its port differs from the
default in `tauri.conf.json`. Keep these overrides in the command's `--config`
JSON; do not edit the production configuration. Do not use
`tauri.e2e.conf.json` for a plain debug launch: its WebDriver permissions need
the `desktop-e2e` feature selected by the packaged E2E builder.

Use a test-owned Xvfb display and select X11 for the test process. Without a
window manager, `xdotool windowactivate` may not work; focus the exact test
window with `xdotool windowfocus --sync "$window_id"` before sending keyboard
input, including input to a native picker. Capture that window with
`import -window "$window_id" "$screenshot_file"` and inspect the image. Use
only disposable fixture directories in pickers. Follow the scoped process
cleanup below rather than terminating processes by application name.

Xvfb interaction evidence does not establish native Wayland scaling or the
session's font hinting. For hinting-dependent checks, record the actual GTK
hinting values and the host's override diagnostic on the test display. Follow
the [text-rendering contract](../architecture/overview.md#linux-webview-text-rendering);
do not change the user's desktop settings to make a harness reproduce it.

### GTK backend and fractional scaling

Inspect an extracted copy of the built AppImage: no `libwayland-client.so*`
files or symlinks may remain under `usr/lib*`, and
`apprun-hooks/linuxdeploy-plugin-gtk.sh` must contain exactly one
`export GDK_BACKEND="${BIBCODE_GDK_BACKEND:-wayland,x11}"` line. The hook must
contain no `gsettings get org.gnome.desktop.interface gtk-theme` line, no
`APPIMAGE_GTK_THEME="${APPIMAGE_GTK_THEME:-` default, and no unconditional
`export GTK_THEME=` line. Require exactly one conditional override:

```sh
if [ -n "${APPIMAGE_GTK_THEME:-}" ]; then export GTK_THEME="$APPIMAGE_GTK_THEME"; fi
```

Record the backend actually used by the packaged BiBCode window, alongside the
desktop session, monitor scale, `GDK_SCALE`, `GDK_DPI_SCALE`, and any
`BIBCODE_GDK_BACKEND` override:

- On Hyprland, capture `hyprctl clients -j` and identify BiBCode by its PID,
  class, and title. Its `xwayland` field must be `false` for native Wayland and
  `true` for Xwayland. Record monitor scaling with `hyprctl monitors -j`.
- Elsewhere, record `WAYLAND_DISPLAY` and `DISPLAY`, then use
  `xprop WM_CLASS _NET_WM_PID` to identify an X11 BiBCode window (Xwayland on a
  Wayland session). For native Wayland, use the compositor's window inspector
  to confirm the app's backend. `WAYLAND_DISPLAY` alone only shows that Wayland
  is available; it does not prove BiBCode used it. If the backend cannot be
  established, record that evidence as unavailable.

On Hyprland/Omarchy, launch with `BIBCODE_GDK_BACKEND` unset at a fractional
monitor scale such as 1.5×. Verify native Wayland and capture normal and minimum
window sizes: text and controls must no longer render about 1.33× too large.
Record the compositor's Xwayland scaling configuration, including
`xwayland:force_zero_scaling` when present.

Close the test instance and repeat with the override, using the same isolated
application and XDG roots:

```sh
BIBCODE_GDK_BACKEND=x11 /absolute/path/to/BiBCode.AppImage
```

Confirm X11/Xwayland through the window evidence above; the override restores
the previous behavior and may reproduce the oversized rendering. Also launch
without the override on an X11-only session (or Xvfb with `WAYLAND_DISPLAY`
unset) to verify automatic X11 fallback. Report unavailable desktop sessions
separately; Xvfb evidence alone does not validate native Wayland scaling.

#### Preview child webview geometry

The BiBCode browser is a child webview placed over the right panel. Run this
check in both the native Wayland launch and the `BIBCODE_GDK_BACKEND=x11`
launch above, at the fractional scale. In a thread, serve a page in its
terminal (`python3 -m http.server 8123 --bind 127.0.0.1`), open
`http://localhost:8123/` in the BiBCode browser, then:

- resize the window, including to its minimum size and back;
- drag the split between the conversation and the right panel both ways;
- hide and show the right panel, and switch to another right-panel tab and
  back.

After each step, the preview must fill exactly the browser panel's content
area: no offset, no gap, no overlap of the panel header or the conversation,
and no stale area left behind. Record a screenshot per backend after the split
drag. A preview that does not follow its panel is a product failure; record
the backend, scale, and step that broke it.

### GTK light/dark theme

Use an isolated test session with a working desktop portal settings backend.
Record its `org.freedesktop.appearance` `color-scheme` and GTK theme name;
Xvfb alone does not provide a portal. Launch the AppImage with `GTK_THEME` and
`APPIMAGE_GTK_THEME` unset for the system-following checks:

- With **Settings → General → Theme** on **System**, launch once with a dark
  system scheme and once with a light scheme. Switch both ways while each
  instance runs. The webview, native menubar, and native dialogs must match at
  launch and after every switch.
- Include a light system scheme with the legacy GTK theme `Adwaita-dark`, and
  a dark system scheme with GTK theme `Adwaita`. The portal color scheme must
  determine the variant; the AppImage's process-local theme name stays Adwaita.
- Select **Light**, then **Dark**, and switch the system scheme both ways for
  each. The webview and native menubar/dialogs must retain the explicit choice.
  Return to **System** and verify immediate adoption and subsequent live changes.
- Separately launch with a user `GTK_THEME` override, then with a nonempty
  `APPIMAGE_GTK_THEME` override. Confirm those values are honored; the latter
  takes precedence if both are set. Explicit environment overrides may pin a
  variant and are excluded from the system-following acceptance checks.

Record screenshots at launch and after changes, the portal/backend setup, and
any unavailable native sessions. Restore settings only within the test session.

## Manual visual workflow registration

The default-branch `qualify-release-visuals.yml` is registration only. GitHub
requires a manually dispatched workflow to exist on the default branch; the
`--ref` option selects the workflow version on the nominated QA branch or tag.
The default-branch version always fails with the closed
`QUALIFICATION_REQUIRES_NOMINATED_QA_REF` classification before checkout,
dependencies, runtime admission, or artifact retention. That refusal is not a
native result. It does not install the QA owner or fixtures on `main`.

Review and nominate the fully merged QA source before dispatch. Require that
ref to contain the full workflow at the same path and its current owner,
fixtures, helper tests, guarded CLI build, web build, static gates, and explicit
evidence retention. Dispatch each existing selection separately:

```sh
qualification_ref="<nominated QA branch or tag>"
gh workflow run qualify-release-visuals.yml --ref "$qualification_ref" -f scene_selection=release-visual-core
gh workflow run qualify-release-visuals.yml --ref "$qualification_ref" -f scene_selection=release-visual-settings
gh workflow run qualify-release-visuals.yml --ref "$qualification_ref" -f scene_selection=release-visual-git-project
```

Record the reviewed commit and require the actual run's head SHA to match it.
The checkout, compiled inputs, and owner provenance must all name that same
source; do not substitute a different checkout while retaining the dispatch
SHA. Preserve the selected workflow's existing scene predicates, process and
namespace ownership, bounds, cleanup proofs, and named light/dark originals.
A completed preparation group does not prove the full 82-scene/164-original
requirement, packaged Tauri scenarios, or other platforms. Keep execution refs,
SHAs, counts, timings, and screenshot findings in the execution report.

Verify registration policy without starting the qualification runtime:

```sh
vp test run scripts/qualify-release-visuals-registration.test.ts scripts/ci-platform-contract.test.ts
```

## Manual remote update UI registration

The default-branch `qualify-release-ui.yml` also registers dispatch only. It
has the existing `matrix` choices `core` and `full`, with `core` as the default,
and refuses execution before checkout or runtime admission. Its historical
presence in the Actions workflow list is insufficient: verify that the file
exists on the current default branch before dispatching the nominated QA ref.

Choose one matrix on the reviewed QA source. The full workflow on that ref
retains its guarded server, maintained interactive fake host, source web build,
private PID/network owners, static/helper gates, and explicit original-image
and receipt retention. Its existing QA-branch push trigger stays in that
version; the default registration stub has no automatic trigger.

```sh
qualification_ref="<nominated QA branch or tag>"
qualification_matrix="full"
gh workflow run qualify-release-ui.yml --ref "$qualification_ref" -f matrix="$qualification_matrix"
```

Require the actual run's head SHA, compiled inputs and owner provenance to
match the reviewed nomination. Check joined controller/supervisor/namespace
cleanup and unchanged build inputs before independently inspecting the
originals. The controlled browser matrix does not qualify Tauri native dialogs,
real remote installers, other platforms, or the full screenshot obligation.
Record source and scope in the execution report; do not turn a registration
refusal or a partial matrix into acceptance evidence.

The static registration policy is safe to verify without starting its owner:

```sh
vp test run scripts/qualify-release-ui-registration.test.ts
```

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

For a browser primary behind a configured loopback UI proxy, verify that
descriptor discovery and the stored broker HTTP target use the same resolved
UI HTTP route as authentication, from one topology snapshot. The registration
cache identity includes that effective HTTP address; the WebSocket address
stays at the backend endpoint. Cover changes to the development origin,
desktop-managed bypass, non-loopback direct access and withheld-topology
retention. A malformed development-origin hint must retain the cached
registration through the existing failure path and recover on a later valid
poll. Helper checks do not replace the real connection scenario.

Use Codex Computer Use to operate the packaged executable. Capture the actual
X11/Wayland and desktop environment in the report. At normal and minimum sizes
verify:

- when only the local Linux environment is configured, the rail shows Local and
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
  Linux remote host: use `pgrep -f "bibcode serve"` or the process list to find
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
  preselected and requires explicit public-address/firewall acknowledgement. If
  Tailscale is installed in its packaged location, discovery must not depend on
  shell `PATH` and must suppress unusable, public, or IPv6 candidates. Confirm
  Linux firewall management remains explicitly operator-owned. The local-machine
  flow still has no Host selector; remote targeting is driven by the environment
  rail;
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
  For the CI AppImage remote-install lane, use its isolated filename prefix:
  `/proc/mounts` must contain exactly one corresponding `.mount_` entry after
  restart, and the recorded runtime process count must be one. Count and stop
  only processes whose current environment has that lane's exact `BIBCODE_HOME`.

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
- provider settings and action menus show Claude, Codex, Cursor, and OpenCode
  without Early Access labels and omit Grok/Grok Terminal;
- text rendering: body text in the sidebar and transcript shows no uneven
  whole-pixel letter gaps, including when the session hinting is `full` (the
  host pins `hintslight`); in a screenshot crop enlarged to 300%, text in the
  sidebar, transcript, settings column, and Agents list shows colored fringes
  at glyph edges like the window title (LCD subpixel text) rather than
  gray-only edges; and the sidebar and timeline have no top or bottom scroll
  fade, which is expected on Linux only. Include the Create PR/MR review
  dialog and its open branch and option lists in the text-rendering review;
  their scrolling text and opaque popover backing must retain the same
  readable rendering as other Linux reading areas;
- AppImage window identity, icon, launcher, and taskbar grouping are correct;
- external worktree grouping, paths, actions, physical identity, and restart
  are correct;
- sidebar menus: right-click a worktree card, the primary card and a project
  header, and open the header's **⋯** from the keyboard (Tab to it, then
  **Enter**). Each native menu separates Open in/Pull, the copy actions,
  Pin/Unread/Rename and the destructive item, with no doubled separator before
  **Delete Worktree…** or **Remove Project…**, and **⋯** shows the same items
  as the header's right-click menu. Tab to a card and press **Shift+F10**
  (and, on Linux, the **Menu** key): exactly one native menu opens at the card,
  and no error toast reports a second menu;
- thread switching, terminal I/O, Activity elapsed time, keyboard focus, and
  responsive overlays work, including reopening the global right panel after
  a sibling chat suppresses a previously active Activity surface; and
- repeated loaded interaction does not freeze, duplicate events, or grow an
  unbounded process tree.

Capture original-resolution screenshots plus focused crops and keep diagnostic
frames separate from acceptance evidence.

Inspect the Git Manager branch popup footer with a long branch name in both
themes. Its New branch and merge-instruction buttons and label spans must stay
inside the bounded popup and use ellipsis while retaining complete accessible
text and disabled explanations. Reject text clipped by the popup viewport; do
not widen the window to hide a layout failure. Compiled CSS and component
regressions establish the containment policy; fresh native originals establish
pixel quality.

## Process-group cleanup

Capture PID, PPID, process group, start time, executable, and command line for
scoped AppImage, desktop, server, provider, terminal, WebDriver, Xvfb, and
fixture processes. Confirm cancellation and shutdown converge on bounded
terminate/wait/reap, late descendants cannot escape after a natural root exit,
peer runtimes remain alive, and no run-owned process survives.

Run a terminal command that exits immediately, then close its terminal after
observing completion. Also close while the command is finishing. Both paths
must complete without an exit-wait timeout or a surviving process. The native
PTY regression covers exit before any subscriber exists and verifies that a
late subscriber receives the retained completion.

Remove only exact fixture, profile, artifact, and evidence roots created by the
run. Report pre-existing processes or temporary roots without terminating or
deleting them.

## Windows and macOS compatibility audit

Audit that Linux fixes do not leak `/proc`, POSIX signals, Unix permissions,
shell syntax, or Linux-only paths into unguarded Windows/macOS code, and that
Linux-only CSS stays inside the `html[data-linux-webkit]` scope. Confirm
Windows native identity/Jobs/WSL contracts and macOS bundle/path/process-group
contracts remain present in host-independent tests. Report the result as
compatibility evidence only.

## Report and cleanup

Complete [the execution report template](./execution-report-template.md), then
perform the shared cleanup and final Git audit. Include distribution, display
protocol, the app's verified GTK backend and override, monitor and GTK scaling,
AppImage execution mode, unsupported host differences, screenshot
paths, zero-survivor evidence, and whether anything was pushed.
