# Task 3 report: Fake backend and multi-platform manager

Status: DONE. Commit `012dd036` — feat(server): run one device per platform behind the device manager (#70).

## What changed
- `device/catalog.rs`: added `Platform` (`"android"`/`"ios"`, Serialize + Deserialize, Hash), `Image { id, displayName, inUseElsewhere }` (the bool is left out when false), and `PlatformCatalog { platform, availability, images }`. `DeviceCatalog` is now `{ platforms, devices }`. `DeviceSnapshot` gains `platform` and `canRotate`, and `avdName` becomes `imageId`. `Avd` is removed.
- `device/backend.rs`: `DeviceBackend` gains `platform()`, `looking()` (the availability shown before the first discovery) and `check_available(image_id)`, which defaults to `Ok(())` (ruling F2). `PlatformDiscovery.images` is now `Vec<Image>`. `can_rotate` no longer needs `allow(dead_code)`.
- `device/mod.rs`:
  - `DeviceManagerConfig { backends: Vec<Arc<dyn DeviceBackend>>, enabled }`.
  - Each backend has its own discovery cache (`PlatformBackend` with its own tokio mutex), so a slow listing on one platform does not block another.
  - `refresh` and `refresh_now`, and the setting follower, discover all platforms through `join_all`. The shutdown `select!` still drops it.
  - `open(platform, image_id)` runs these steps in order: backend lookup, that platform's discovery, the Ready and listed checks, `check_available`, then the per-platform limit and the slot.
    - If `check_available` returns `Busy`, open returns `DeviceOpError::Busy { image_id }` and creates no slot.
    - Any other `check_available` failure becomes `Emulator("The device cannot start: …")`.
  - The launch receives the backend `Arc`. On Ready, `can_rotate` is taken from the running device; before that it is `true`.
  - Error types:
    - `UnknownAvd(String)` is now `UnknownImage { platform, image_id }`, with the Android message unchanged and the iOS message as the ruling gives it.
    - `Busy` maps to the exact `DeviceBusyError` JSON.
    - `LaunchFailure::Busy` becomes the slot failure "in use outside BibCode".
  - Cancel-safe close, stop_device, shutdown, the setting follower and the exit watcher are unchanged.
- `device/fake_backend.rs` (new, `#[cfg(test)]`):
  - `FakeBackend` has `new`, `busy` (check_available refuses), `busy_on_launch` (a launch that finds busy late), `never_boots`, `without_rotation`, `exit_running`, `stops`, `discover_calls` and `launches`.
  - `FakeRunning` returns errors for frames, png and video, never panics, and counts `stop` and `stop_shared` once.
  - Helpers: `fake_manager` and `wait_state`.
- `android/mod.rs`: implements `platform()` and `looking()` with "Looking for the Android SDK on the server.". `android/sdk.rs` builds `Image`.
- Callers (ruling F1):
  - `production/runtime.rs`: `backends: vec![Arc::new(android)]`.
  - `production/device_rpc.rs`: still decodes `{avdName}` and calls `open(Platform::Android, …)`.
  - `connect_mcp/device_tools.rs`: Android open; `only_avd` reads the Android `PlatformCatalog`.
  - Test updates: `android/testing.rs`, `media_socket.rs`, and the Android tests in `mod.rs` (open signature, `UnknownImage`, the new catalog shape); the `device_rpc.rs` unit test and `connect_mcp/tests.rs` (new catalog JSON paths).
  - `apps/server/tests/device_rpc.rs` needed no change.
  - packages/contracts, fixtures and apps/web were not touched.

## TDD evidence
- RED: I wrote `platform_tests` (12 tests) and `fake_backend.rs` before any implementation. `cargo test -p bibcode-server --lib device::platform_tests` failed to compile with 35 errors: no `Platform`/`Image`, no `platform`/`looking`/`check_available` trait members, no `backends` config field, `open` takes 1 argument, no `Busy`/`UnknownImage`, and no `platforms`/`image_id`/`can_rotate` fields.
- GREEN: all of them pass. The tests:
  - one_device_per_platform_runs_side_by_side
  - a_busy_image_is_refused_and_leaves_no_slot (per F2: `Err(Busy{image_id})`, empty devices, 0 launches)
  - an_image_found_busy_at_launch_fails_its_slot ("in use outside BibCode")
  - an_unrequested_exit_fails_only_that_device
  - the_catalog_lists_each_platform_with_its_own_availability (also checks the JSON shape and that `inUseElsewhere` is omitted)
  - the_catalog_starts_with_each_platform_looking
  - a_snapshot_reports_whether_its_device_can_rotate
  - an_unconfigured_or_unknown_image_is_refused
  - closing_a_booting_fake_device_cancels_its_launch
  - turning_the_setting_off_stops_every_platform
  - a_dropped_close_still_stops_a_fake_device
  - busy_and_unknown_image_errors_name_the_platform (exact DeviceBusyError JSON, the iOS message, and the `launch_failure(Busy)` text)

## Gates (toolchain 1.98.0)
- `cargo fmt --all --check`: OK
- `cargo clippy -p bibcode-server --all-targets -- -D warnings`: exit 0 (the only warning is the existing linker `__eh_frame` note, which ignores `-D`)
- `cargo test -p bibcode-server --lib device::`: 120 passed
- `cargo test -p bibcode-server --lib production::connect_mcp`: 29 passed
- `cargo test -p bibcode-server --lib production::device_rpc`: 4 passed
- `cargo test -p bibcode-server --test device_rpc --test device_media_socket`: 1 + 11 passed

## Deviations / decisions
- The brief asks for `UnknownImage(String)`; it is a struct variant `{ platform, image_id }`, because the message depends on the platform.
- The brief lists `registry` in `DeviceManagerConfig`; it is left out. The manager never uses it, so it would only be dead code. `AndroidBackendConfig` keeps its registry, and its existing config struct stands in for `AndroidBackend::new(env, dirs, registry, port)`.
- The brief asks for `HashMap<Platform, Discovery>`; each backend has its own cache in a Vec instead. Configuration order fixes catalog order, and per-backend locks avoid one platform blocking another.
- Opening a platform with no configured backend returns `UnknownImage { platform, .. }`. For iOS this means "No simulator with id … exists on the server" (choice; no contract case covers it yet).
- The `Limit.running` value is the running image id. For iOS that is a UDID, not a display name; Task 4/5 may want the display name.
- The `LaunchFailure::Busy` failure text was changed to "in use outside BibCode" (it was "is in use outside BiBCode").
- I added a `without_rotation` builder, a `busy_on_launch` builder and a `launches()` counter to cover canRotate, late busy and F2.

## Concerns / residual risk
- The wire shape changed (platforms/images/imageId/platform/canRotate). check:contracts, fixtures and the web tests are expected to be red until Task 4 (ruling F1).
- `docs/architecture/overview.md` still describes one running device in total; the code now allows one per platform. This task left the docs alone, so that change belongs to the docs task.
- The fake device has no frames or video, so the media and frame paths are covered only by the Android tests.
- Profiler coverage: no new process, RPC, removal path or event. `device.lifecycle` gains a failureKind value `"busy"` for a late busy launch; the profiler docs should mention it when the iOS docs land.

## Fix round 1 (commit 42c521fd)

Changes (apps/server/src/device/mod.rs, fake_backend.rs):
1. `Slot.display_name` now stores the opened image's `display_name`, taken from the discovered images at open time (`Discovery::display_name`). `DeviceLimitError.running` and its message use it, through `DeviceTable::running_on(platform)`. Android AVD "Pixel" has the display name "Pixel", so the Android limit tests are unchanged. The fake's display name is now `"iPhone {id}"`. `one_device_per_platform_runs_side_by_side` asserts `running == "iPhone UDID-1"`, not the UDID, and the catalog JSON test expects `displayName: "iPhone UDID-1"`.
2. The copy now uses "BiBCode":
   - Busy message: "This simulator is already running outside BiBCode. Shut it down in Simulator, then open it here."
   - Late-busy failure: "in use outside BiBCode".
   - The pinning tests (error JSON, `launch_failure`, the late-busy slot test) and the fake exit reason string are updated to match.
3. A non-Busy `check_available` failure now logs `tracing::warn!` and returns one message: `Emulator("The server could not check whether this device can start. Try again in a moment.")`.
4. `open` does an unlocked per-platform limit pre-check before `check_available`, in a separate statement so the guard is dropped before the await. The locked check stays authoritative. A new `FakeBackend::check_calls()` lets the side-by-side test assert that the doomed UDID-2 open never ran the check (`check_calls() == 1`).

Gates (toolchain 1.98.0), run through `gates.sh`:
- `cargo fmt --all --check`: FMT_OK
- `cargo clippy -p bibcode-server --all-targets -- -D warnings`: exit 0
- `cargo test -p bibcode-server --lib production::connect_mcp`: 29 passed
- `cargo test -p bibcode-server --lib production::device_rpc`: 4 passed
- `cargo test -p bibcode-server --lib device::`: 120 passed
- `cargo test -p bibcode-server --test device_rpc --test device_media_socket`: 1 + 11 passed
