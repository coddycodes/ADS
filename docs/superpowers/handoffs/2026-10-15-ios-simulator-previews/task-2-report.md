# Task 2 report: Backend traits and Android behind them

Status: DONE_WITH_CONCERNS

## Commits
- `37880a7d` refactor(server): move the Android device code into device/android (#70)
- `b134aeeb` refactor(server): drive Android devices through a backend trait (#70)

## What changed
- Step 1: `git mv` of `sdk`, `emulator`, `grpc`, `scrcpy` (plus the `scrcpy/` jar and NOTICE directory that `include_bytes!` needs) and `testing` into `device/android/`. `VideoUnit` and `UnitKind` moved to a new `device/video.rs`. Import paths were updated, with no re-export alias. Two living-doc paths were fixed: `docs/architecture/overview.md` and `docs/testing/cross-platform-validation.md` now say `device/android/scrcpy.rs`.
- Step 2: `device/backend.rs` adds `PlatformDiscovery`, `LaunchFailure`, `RawJpeg`, `Png`, `JpegStream`, `DeviceBackend`, `RunningDevice` and `ErasedVideoSource`. `media.rs` adds the blanket `impl<S: VideoSource> ErasedVideoSource for S` and `impl VideoSource for Box<dyn ErasedVideoSource>`.
- Step 3: `android/mod.rs` adds `AndroidBackend`/`AndroidBackendConfig` and `RunningAndroid`.
  - Discovery stores the found `AndroidSdk` for `launch`.
  - `LaunchError` maps to `LaunchFailure`.
  - The whole frame pump moved behind `jpeg_stream`: keep the newest image, encode at most every 66 ms on `spawn_blocking`, warn about the first malformed frame only. Frames the throttle skips are still never encoded.
  - `png`, `video` (`ScrcpySession::start` boxed, with its debug log), `input`, `rotate`, `exited`, `report_unrequested_exit` (re-reads `helper.exited()` and calls `helper.report_unrequested_exit(HELPER_LABEL, …)`), `stop` and `stop_shared`.
  - The `impl VideoSource for ScrcpySession` moved here from `media.rs`, and `emulator_failure` moved here from `mod.rs`.
  - The Android input pieces moved to `android/input.rs`: `w3c_key`, `rotation_vector`, `reported_orientation`, `InputState`, send/reopen/deliver, `send_batch` (the old loop body, unchanged) and `rotate`. Their three unit tests moved with them.
- Step 4:
  - `DeviceManagerConfig` is now `{ backend: Box<dyn DeviceBackend>, enabled }`. `runtime.rs` and `ManagerFixture` build the `AndroidBackend`.
  - A slot holds `Option<Arc<Box<dyn RunningDevice>>>`. `client()` and `Inner::input_state` are deleted, and `Inner::running(id) -> Result<(SharedDevice, Orientation), DeviceOpError>` replaces them.
  - Frames, input, rotate, media tickets, `attach_media` and the new `DeviceManager::png` all go through `Inner::running`. The MCP screenshot calls `manager.png(id, 1080)` and keeps the same width computation, warn text, error string and JSON.
  - Production code in `mod.rs`, `frames.rs`, `media.rs` and `input.rs` names no Android type.
  - `overview.md` gained a paragraph on the backend split.

## Stop and ownership (the escalation point)
I kept the brief's plan:
- The trait keeps `stop(self: Box<Self>)`, and the slot holds `Arc<Box<dyn RunningDevice>>`.
- `stop_device` runs only on the manager's TaskTracker, from the `close` spawn and the exit watcher, so a dropped `close` stays cancel-safe.
- It tries `Arc::try_unwrap` every 50 ms for up to 5 s. If the device is still shared, it logs a `warn` and calls `stop_shared(&self)`.

`stop_shared` is added to the trait, because the brief's prose requires it but its trait block left it out. `RunningAndroid` keeps its `RunningEmulator` in a `Mutex<Option<…>>` so either stop can take it. Every consumer holds the `Arc` for one call only:
- `jpeg_stream` and `video` return `'static` values;
- `attach_media`'s start future is dropped when the hub cancels.

The "closed while booting" path stops the fresh `Box` directly.

`RunningAndroid` has its own input `TaskTracker`. Its stop drops the input sender, stops the emulator, then closes and waits for that tracker, so shutdown still waits for the input stream task.

## Deviations from the brief
1. **`Platform` and `Image` do not exist yet; Task 3 adds them.** So `DeviceBackend` has no `fn platform()`, and `PlatformDiscovery.images` is `Vec<Avd>` for now. The field name is kept.
2. **`rotate` takes `report: &'a (dyn Fn…)` with an explicit `'a`.** The brief's elided form cannot compile, because the returned future borrows `report`.
3. **Two test-only trait hooks.** Besides `#[cfg(test)] pid()`, I added `#[cfg(test)] input_stream_closed()` with a default of `true`, so the manager helper the input tests use keeps working.
4. **`#[allow(dead_code)]` on three interface items.** They are `LaunchFailure::Busy`, `RunningDevice::can_rotate` and `Png::orientation`, which later tasks consume. `launch_failure` maps `Busy` to `("busy", "is in use outside BiBCode")`; that text is my choice.
5. **The brief's media test helpers do not exist.** There is no `ChannelSource::new()` or `read_frames`. I built the source from its fields and subscribe *before* feeding, since a later subscriber gets only the cached config. I also assert that the erased `stop` forwarded `device_ended = false`.
6. **`attach_media` boxes `run(...)` into a `BoxFuture<'static, ()>` before spawning.** Without that, rustc reports "implementation of `VideoSource` is not general enough": region erasure in the async block's witness meets the `Box<dyn ErasedVideoSource + 'static>` impl.
7. **`RawJpeg.orientation` stays a plain `Orientation`, as in the brief.** Today a frame with no rotation keeps the catalog's orientation. To keep that, `RunningAndroid` tracks the last orientation the emulator reported, from frames and from `rotate`'s reports, and uses it for such frames. Those are the only two writers of the catalog orientation, apart from one test-only setter, so the result is equivalent. The fake controller always sets a rotation.
8. **Log text.** The frame stream's warn messages now say "device frame stream ended/failed" instead of "emulator screenshot stream …".

## Tests
TDD for the new test: I wrote `an_erased_source_feeds_the_hub_like_the_concrete_one` first. It failed to compile with `E0433: cannot find backend in device` (red). After adding `backend.rs` and the impls, `cargo test --lib device::media` passed: 31 passed, including the new test (green).

Test changes are import paths plus the removed `client()`:
- `crash_marks_failed_and_frees_the_slot` and `closing_a_crashed_device_waits_for_its_process` use `inner.running(&id)`, which gives the same `NotReady(Failed)` fact.
- `ready_device_exposes_its_client_and_orientation` asserts `running.native_size()` and the orientation (Portrait, then Landscape) and `NotFound` for a missing id, all at the same precision as before.
- The `serial.starts_with("emulator-")` assertion moved to `emulator::tests::launches_boots_and_stops_a_fake_emulator`, on `running.serial`. It moved; it was not dropped.
- `launch_failures_name_their_reason` uses `LaunchFailure::` in place of `LaunchError::`.

| Suite | Before Step 1 | After Step 1 | After Step 6 |
|---|---|---|---|
| `cargo test --lib device::` | 107 | 107 | 108 (107 plus the new test) |
| `--lib production::connect_mcp` | 29 | | 29 |
| `--test device_rpc` | 11 | | 11 |
| `--test device_media_socket` | 1 | | 1 |

Also passing: `--lib production::device_rpc` (4) and `--lib registered_helper` (2).

## Gates (toolchain 1.98.0 pinned on PATH)
- Passed: `cargo fmt --all --check`, `cargo clippy -p bibcode-server --all-targets -- -D warnings`, `vp check` and `vp run typecheck`.
- `vp run typecheck` fails without the PATH pin. Homebrew's newer cargo denies `fetch_update` deprecations in files this task does not touch; that is a known environment issue.

## Self-review
- The manager only names Android in its initial catalog message ("Looking for the Android SDK on the server."). I kept it to avoid a behavior change; Task 3 reshapes the catalog.
- The MCP screenshot error still says "The emulator did not return a screenshot", byte-identical to before.
- Behavior delta: a `close` that races an in-flight input batch or rotate now waits up to 5 s, or until the call ends, before the VM shutdown starts. Before, the stop started at once.
- Behavior delta: the frame producer now holds the device while the stream opens, which is one gRPC call.
- CodeGraph: `codegraph sync .` succeeded.
- Profiler coverage:
  - The emulator and scrcpy are still spawned through `spawn_registered_helper`, and unrequested exits still reach `report_exit(Watch::Failures)` through `RunningDevice::report_unrequested_exit`, called before the stop as before.
  - `device.lifecycle` events are unchanged.
  - No new RPC methods, removal paths, binaries or shutdown services.
- Testing runbooks: reviewed. One path was updated (scrcpy.rs); the rest remain accurate.
- Not run: the React and UI reviews (no web change) and native validation (an internal refactor).

## Concerns
- The deviations above, mainly the missing `platform()` and `Vec<Avd>`, which Task 3 must add, and the extra `stop_shared` and `input_stream_closed` trait methods that Task 3's `FakeBackend` must implement or inherit.
- The 5 s release wait is new latency on a close that races an in-flight call.
