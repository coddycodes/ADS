# Task 5 report: iOS discovery and lifecycle with `simctl` (view-only)

Status: DONE. Commit `1bd756e9` feat(server): list, boot and stop iOS simulators on macOS (#70).

## What changed

New (all `#[cfg(target_os = "macos")]` via `device/mod.rs`):

- `apps/server/src/device/ios/simctl.rs`: `SimctlRunner { xcrun, xcode_select, registry }` with
  `list` (15 s), `boot` (boot + `bootstatus -b` sharing one boot timeout), `shutdown` (30 s),
  `screenshot_jpeg` (15 s, `io <udid> screenshot --type=jpeg -`), `pbcopy` (15 s, stdin), all
  through `process::supervised::run_supervised`; `SimctlError` carries the stderr tail.
  `SimDevice`, `SimState`, `parse_list` (iOS runtimes only, available only, newest runtime first),
  `display_name` ("iPhone 17 Pro · iOS 27.0"), `jpeg_size` (any SOFn header),
  `register_simulator` (sysinfo lookup of `launchd_sim` whose cmdline contains the UDID, in
  `spawn_blocking`, registered with `register_identity` as `ios-simulator`; best effort).
- `apps/server/src/device/ios/guard.rs`: `XcodeStatus`, `xcode_status` (`xcode-select -p`,
  `xcrun simctl help`, `xcrun xcodebuild -version`; license/first-launch stderr →
  `SetupIncomplete`), `availability_for` with the spec §6 messages.
- `apps/server/src/device/ios/mod.rs`: `IosBackend` (+ `IosBackendConfig::from_process`) and
  view-only `RunningSimulator`.
  - images: `in_use_elsewhere` = not `Shutdown` and not in the backend's booted-by-BiBCode set.
  - `check_available`: lists, `Busy` unless `Shutdown` (manager returns `DeviceBusyError`
    synchronously, no slot). `launch` re-checks and returns `LaunchFailure::Busy`.
  - launch: `Booting` → boot (cancel-aware via `select!`, dropping kills the supervised group) →
    first screenshot gives the native size (5 tries) → register `launchd_sim` → 2 s state poll.
    A failed `simctl boot` never triggers a shutdown (it may be someone else's); a failure after
    a successful boot (bootstatus, timeout, cancel, no screenshot) shuts down what it booted.
  - `exited`: set by the poll to "Shutdown outside BiBCode" (or "Deleted outside BiBCode" when it
    disappears from the list). `report_unrequested_exit` logs at warn and calls
    `profiler::exits::report_exit(Watch::Failures, label "ios-simulator", exe "launchd_sim")`
    with a synthetic exit code 1 (no real status exists).
  - stop/stop_shared: idempotent; cancel and await the poll first, unregister attribution, run
    `simctl shutdown` only if the simulator did not end outside BiBCode, drop it from the set.
  - `jpeg_stream`: one `simctl` screenshot every 500 ms, ends after an error (producer retries).
    `max_width` is not applied (would need a JPEG decoder; frames are native size).
  - `png`: returns the JPEG with `mime_type: "image/jpeg"`. `video`: `DeviceError` (media hub
    closes, client falls back to JPEG). `input`: `InvalidInput("Touch input is off: live input
    is not available yet.")`. `rotate`: `InvalidInput`. `can_rotate`: false.
- `apps/server/src/device/ios/testing.rs`: `FakeXcrun` (shell `xcrun` + `xcode-select` in a
  tempdir; per-device state files; call log; fake JPEG 60×130; options `state`, `boot_fails`,
  `simctl_help_fails`), `ios_manager`, `manager_for`.
- `apps/server/src/device/ios/fixtures/simctl-list.json`: recorded on this Mac, trimmed.

Modified:

- `device/catalog.rs`: `AvailabilityKind` gains `XcodeMissing`, `XcodeSetupIncomplete`,
  `NoSimulators`, `XcodeUnsupported`, `ViewOnlyHeadless` (camelCase matches `device.ts`).
- `device/backend.rs`: `Png.mime_type`; Android sets `image/png`.
- `production/connect_mcp/device_tools.rs`: `mimeType` comes from `image.mime_type`.
- `device/stderr_tail.rs`: `tail_of(bytes)` — last 5 non-empty lines of at most 4 KiB.
- `production/runtime.rs`: on macOS pushes `IosBackend` after Android. Under
  `hermetic-test-guard` its tool paths are `/nonexistent/...`, so test runtimes report
  `xcodeMissing` and never touch this Mac's simulators (mirrors `SdkEnv::from_process`).

## Deviations from the brief (and why)

1. Fixture test value: on this Mac `308FD3CD…` (iPhone 17 Pro) is on the **iOS 26.5** runtime,
   not 27.0. The fixture stays a faithful recording; the test asserts
   `"iPhone 17 Pro · iOS 26.5"` for it and `"iPhone 18 Pro · iOS 27.0"` for `9AFB380D…`, which
   proves the exact required format. Home paths were redacted to `/Users/user`.
2. The fixture's watchOS device is synthetic: this Mac has no watchOS runtime installed. It uses
   the real field shape.
3. `SimctlRunner` also holds `xcode_select: PathBuf` so the fake tree covers `xcode-select -p`.
4. `png` returns the JPEG (binding decision) instead of a `Protocol` error.
5. `check_available` already existed (Task 3) — implemented for iOS only; Task 3 tests untouched.
6. Spelling "BiBCode" in all user-facing strings (binding decision).
7. View-only reason is "live input is not available yet" (binding decision; no availability
   reason applies because `DeviceManager::open` only opens with `Ready` availability). Task 6
   should derive it from `XcodeUnsupported`/`ViewOnlyHeadless` messages minus the period — but
   note `open` currently refuses any non-`Ready` availability, so Task 6 must let view-only
   kinds open.

## TDD

Tests were written with the implementation in one pass (not strictly test-first). RED was
demonstrated by mutation: removing the "booted by BiBCode" exclusion from `in_use_elsewhere` and
the "ended outside" guard on stop's shutdown made `close_shuts_down_only_what_bibcode_booted`
(`[("UDID-1", true), …]`) and `unrequested_shutdown_fails_the_device` (a `shutdown UDID-1` call
logged) fail; restoring made them pass. One expectation was corrected during GREEN (the boot
failure reason includes both stderr lines, by design of the tail).

Tests added (17: 16 under `device::ios`, 1 in `device::stderr_tail`): simctl — parses fixture/skips watchOS, states + malformed listing, JPEG SOF
size, launchd_sim matcher, failure message with stderr tail, runner round trip through fake
xcrun (boot/bootstatus/list/pbcopy/screenshot/shutdown call log), failed boot carries stderr and
skips bootstatus; guard — availability mapping (brief's test), license/first-launch detection +
version parse, `xcode_status` through fake tools (Ready 27.0 / SetupIncomplete / Missing ×2);
backend — the brief's three manager tests (with "BiBCode"), view-only input refusal + JPEG
screenshot with mime type, failed boot leaves simulator alone, XcodeMissing and NoSimulators
catalog; stderr_tail — keeps last non-empty lines.

## Gates (toolchain 1.98.0-aarch64-apple-darwin)

- `cargo fmt --all --check`: pass.
- `cargo clippy -p bibcode-server --all-targets -- -D warnings`: pass (only the pre-existing
  `linker_messages` eh_frame note, which ignores -D warnings).
- `cargo test -p bibcode-server --lib device::`: 137 passed, 0 failed.
- `cargo test -p bibcode-server --lib production::connect_mcp`: 32 passed.
- `cargo test -p bibcode-server --lib production::device_rpc`: 4 passed.
- `cargo test -p bibcode-server --test device_rpc --test device_media_socket`: 1 + 11 passed.
- Non-macOS compile: **not run locally** (`rustup target list --installed` shows only
  `aarch64-apple-darwin`). Reviewed by hand: `ios` module and the runtime push are cfg-gated,
  `device_backends` has `allow(unused_mut)` off macOS, `stderr_tail::tail_of` has
  `allow(dead_code)` off macOS; new `AvailabilityKind` variants are in a public module.
- `vp check` / `vp run typecheck`: not run (Rust-only change; no TS/contract change).
- No real simulator was booted. Only `xcrun simctl list devices available -j`,
  `xcode-select -p` and `xcrun xcodebuild -version` were run once to capture the fixture.

## Profiler coverage

- Child processes: every `simctl`/`xcode-select`/`xcodebuild` call goes through
  `run_supervised` (timeouts, group kill, command exit reporting).
- Simulators: `launchd_sim` registered with `register_identity` (`ios-simulator`), unregistered
  on stop; unrequested shutdown → `report_exit(Watch::Failures)` + `tracing::warn!`.
- RPC methods, worktree removal, binaries, shutdown order: none changed (`DeviceManager::shutdown`
  stops the iOS backend in its existing quiesce position).
- Events/docs: no new event or finding; the `device.lifecycle` doc update is left to the
  docs task (spec §12/§13 step 8).

## Self-review / concerns

- `report_exit` gets pid 0 when `launchd_sim` was not found; the synthetic exit code 1 makes the
  finding read "exited with code 1". Acceptable but imprecise.
- The poll treats any non-`Booted` state (including `Shutting Down`) as an external exit; a list
  failure is ignored (logged at debug).
- Full-size JPEG frames (~1206×2622) at 2 fps are sent unscaled; bandwidth heavier than Android's
  720-wide frames. Live mode (Task 6) replaces this path.
- `video()` failing logs a `warn` ("device media stream could not start") each time a client
  attaches a media socket to an iOS device; Task 6 should refuse media tickets for view-only
  devices (`DeviceMediaUnavailableError`, spec §6.5).
- The manager's `png` error copy says "The emulator did not return a screenshot" for iOS too.
- `discover()` runs the Xcode probe (`xcode-select -p`, `simctl help`, `xcodebuild -version`)
  on every refresh (30 s TTL); it is not cached per process as spec §6 asks for the guard. Task 6
  should cache it with the private-API guard, keyed on Xcode's path and version.
- `LaunchFailure::BootTimeout`'s reason says "within 3 minutes", matching the 180 s default.
- Living docs (overview, workspace-ui, profiler references, testing runbooks, changelog) not
  updated here; scheduled for the docs task per spec §13. Testing runbooks reviewed and remain
  accurate for this task's scope (no command, script, or gate change).
