# Task 6 report: CoreSimulator loader, private-API guard and live JPEG frames

Status: DONE_WITH_CONCERNS. The concerns are listed at the end.

Commits (base `1bd756e9`):

- `4872a3f4 feat(server): read iOS simulator frames through CoreSimulator (#70)`
- `991f5b55 test(server): keep the simulator framework probe test off the host's frameworks (#70)`. The probe unit test now passes nonexistent paths for both frameworks, so the hermetic unit suite never `dlopen`s the host's CoreSimulator.

## What was built

- **`device/ios/coresim.rs`** (new):
  - `Library` dlopens CoreSimulator (`/Library/Developer/PrivateFrameworks/…`) and SimulatorKit (`<developer_dir>/../SharedFrameworks/…`). The handles are never closed, because Objective-C classes cannot be unloaded.
  - `DlsymResolver` implements the guard's `SymbolResolver`. It reports:
    - classes, through `objc_getClass`;
    - protocols, through `objc_getProtocol`;
    - type encodings for instance methods, class methods and protocol methods (required, then optional), through `method_getTypeEncoding` and `protocol_getMethodDescription`;
    - C symbols, through `dlsym` on SimulatorKit, then `RTLD_DEFAULT`.
  - `probe(dir)` is `probe_with(&DlsymResolver)`. It never panics.
  - `CoreSim` holds the developer dir and `HidSymbols` (Indigo builders and the xpc `_4sim` functions). The symbols are unused until Task 8.
  - `guarded` sends every message through a typed `objc_msgSend` transmute (`send0`/`send1`/`send2`/`send5`) inside `objc2::exception::catch`. `msg_send!` is not used: its debug verification panics on ROCK proxies (MethodNotFound) and on nil receivers.
  - `Object` wraps `Retained<AnyObject>`. Each `get`/`scalar` is gated by `respondsToSelector:`, and display selection is gated by `conformsToProtocol:`.
  - The lookup chain follows §0 exactly: `+sharedServiceContextForDeveloperDir:error:` → `-defaultDeviceSetWithError:` → `-devices` → `-UDID.UUIDString` → `-io` → `-ioPorts` → `-descriptor`.
  - The main display is the descriptor that meets all of these:
    - it conforms to SimScreen, SimDisplayIOSurfaceRenderable and SimDisplayRenderable;
    - it responds to `framebufferSurface`;
    - `state.displayClass == 0`.
  - `Screen::attach(sim, udid)`:
    - reads the initial `framebufferSurface` and `screenProperties.uiOrientation`;
    - registers `registerScreenCallbacksWithUUID:callbackQueue:frameCallback:surfacesChangedCallback:propertiesChangedCallback:` on a private serial `DispatchQueue`;
    - waits up to 5 s for a surface.
  - The three callbacks only do non-blocking work: bump the `damage` watch, replace the surface under a mutex, or publish the orientation with `send_if_modified`.
  - `Screen` methods:
    - `damage()` returns a `watch<u64>`;
    - `orientation()` returns a `watch<IosOrientation>`;
    - `surface()` returns an `IoSurface` (a retained `CFRetained<IOSurfaceRef>` that is `Send`/`Sync`, with `as_surface()` for Task 7);
    - `size()` returns the portrait size;
    - `read_rgb(max_width)` locks the surface read-only, checks BGRA, 4 bytes per element and 0 planes, honours bytes-per-row, and returns the frame upright and scaled;
    - `detach()` unregisters once (guarded by an atomic flag), and `Drop` calls it.
- **`device/ios/pixels.rs`** (new, pure Rust):
  - `IosOrientation::from_ui(1..=4)` maps 1→portrait, 2→reversePortrait, 3→landscape, 4→reverseLandscape. The 3/4 mapping is provisional and the comment points at Task 8.
  - `orientation()` gives the catalog value.
  - `turn()` follows the observed `uiOrientation` value: 3 is clockwise (UI top along the physical left edge) and 4 is counter-clockwise.
  - `upright_rgb()` rotates, scales (nearest neighbour), converts BGRA to RGB and honours the stride in one pass. Each output offset is `columns[x] + rows[y]`.
- **`device/ios/guard.rs`**:
  - `GuardResult { Live(Arc<CoreSim>), ViewOnly { kind, missing } }`.
  - The `REQUIRED` table lists 5 classes, 7 protocols, 17 methods with §0 encodings (compared without frame offsets) and 6 C functions, including the HID names for Task 8.
  - `probe_with`, `GuardCache` (probes per `(developer_dir, version)`), `XcodeCache`, `unsupported_message`, `HEADLESS_MESSAGE`.
  - `availability_for(status, devices, guard)` applies the §6 order. `viewOnlyHeadless` is mapped but unreachable, as §0 says.
- **`device/ios/mod.rs`**:
  - `IosBackendConfig.guard` is injected. Production uses the real guard; the test manager uses a fake that is always view-only, so unit tests never load the host frameworks.
  - `discover` computes the guard on a blocking thread and records a `LaunchMode`.
  - `launch` either attaches a `Screen` (with `spawn_blocking`, selected against cancel) or falls back to view-only with the reason "BiBCode could not attach to this simulator's screen." if the attach fails.
  - `RunningSimulator` has a `Display::Live(Arc<Screen>) | ViewOnly { reason }`.
  - Live `jpeg_stream`:
    - waits for `damage` with a 1 s timeout;
    - after the timeout, re-yields the last frame;
    - is rate-limited to `JPEG_MIN_INTERVAL`;
    - reads, rotates and encodes in `spawn_blocking`.
  - Live `png` is a real PNG (`png` crate).
  - `orientation_changes()` exposes the Screen's watch.
  - `stop` detaches the screen (in `spawn_blocking`) before `simctl shutdown`.
  - `host_manager_for_tests()` is `#[doc(hidden)]` and re-exported as `device::ios_host_manager_for_tests`. It uses `/usr/bin` tools and the real guard whatever the features, for the opt-in tests only.
- **Manager** (`device/mod.rs`, `backend.rs`, `media.rs`):
  - New `RunningDevice::orientation_changes()` (default `None`). The manager spawns `follow_orientation` after Ready, which calls `set_reported_orientation` with `oriented_size`. The catalog therefore shows `uiOrientation` with no viewer attached.
  - New `RunningDevice::video_unavailable()` (default `None`). `create_media_ticket` refuses with the new `DeviceOpError::NoVideo(reason)`, which maps to `_tag: DeviceMediaUnavailableError`.
  - `RawJpeg` is now `Clone`.

## Task 5 follow-ups

1. **View-only opens.** `open` uses `opens(kind)`, which accepts Ready, XcodeUnsupported and ViewOnlyHeadless. Other kinds still refuse.
   - Tests: `view_only_platforms_open_and_others_do_not` and `an_unsupported_xcode_still_opens_simulators_view_only`.
2. **Media tickets.** Every iOS device now returns `DeviceMediaUnavailableError` at ticket time, so `video()` is never reached and does not warn on each attach.
   - View-only devices say "Video is off: <reason>."
   - Live devices say "Video from iOS simulators is not available in this version of BiBCode yet, so the screen updates as still images." until Task 7. Task 7 should make `video_unavailable()` return `None` for `Display::Live`.
   - Tests: `a_device_without_video_refuses_media_tickets_at_once` and the view-only test.
3. **Caching.** `XcodeCache` reuses the last *Ready* status while `xcode-select -p` and the mtime of `<Xcode.app>/Contents/version.plist` are unchanged. Once cached, the Xcode check runs one process (`xcode-select -p`) instead of three (`xcode-select -p`, `simctl help`, `xcodebuild -version`). A 30 s refresh is therefore 2 processes instead of 4, because `simctl list` still runs to pick up device states. A status that is not Ready is never cached, so finishing the license or setup shows on the next refresh. `GuardCache` probes once per `(developer_dir, version)`.
   - Tests: `a_cached_xcode_status_runs_one_process_per_refresh`, `guard_reprobes_when_xcode_changes` and `the_guard_is_cached_per_developer_dir_and_version`.
4. **Input messages.**
   - View-only: "Touch input is off: <availability reason without trailing period>." For example: "Touch input is off: Xcode 27.0 changed the simulator interfaces BiBCode uses. Simulators open as screenshots without touch input."
   - **Live devices still refuse input until Task 8**, with: "Input to iOS simulators is not available in this version of BiBCode yet."
5. **`run_error`.** It now maps every `SupervisedRunError` variant to words through `run_failure` and no longer uses Debug formatting. Test: `run_errors_read_as_words`.
6. **Screenshot copy.** The manager's screenshot error is now platform-neutral: "The device did not return a screenshot. Try again in a moment."
7. **Closing while Booting.** `closing_a_booting_simulator_shuts_down_only_it_once` covers it, using `FakeXcrun::bootstatus_blocks()` (`sleep 60`). The calls contain exactly `["shutdown UDID-1"]` and nothing for the other, externally booted simulator.

## Findings beyond the brief

- **Block signatures are required.** With signature-less `RcBlock::new` blocks, the real registration raised `NSInternalInconsistencyException: Block is missing signature field`, because CoreSimulator sends the blocks over XPC.
  - The exception was caught by `guarded`, and the device fell back to view-only as designed.
  - The fix is `RcBlock::with_encoding` with `v8@?0`, `v24@?0@8@16` and `v16@?0@8`.
  - A correction note was added to spec §0. **Task 7 and Task 8 completion blocks sent to proxies need signatures too.**
- **Xcode 27 `simctl io screenshot` no longer writes to stdout.** `-` becomes a file named `-` in the working directory.
  - Task 5's view-only path therefore never worked on a real simulator: the boot failed with "the screenshot is not a JPEG".
  - `screenshot_jpeg` now writes to a temporary file (`$TMPDIR/bibcode-simulator-<uuid>.jpg`), checks its size, reads it and removes it.
  - The fake `xcrun` copies to `$5`. A correction note was added to spec §4. The stray `-` files were deleted.
- **Close race.** `Inner::finish` cancelled `reaped` before publishing the catalog, so a `close` could return while the catalog still said `Stopping`. The real-simulator test hit this.
  - `finish` now publishes first and cancels `reaped` afterwards.
- **The §0 names behaved as recorded.** A runtime probe (`scratchpad/task6/proto.m`) confirmed:
  - every class, protocol, selector and encoding the guard checks;
  - that `descriptor` lives on `SimDeviceIOPortInterface`, `state` on `SimDeviceIOPortDescriptorInterface`, and `ioPorts` on `SimDeviceIOProtocol`.

## TDD

- **RED.** I wrote the brief's three guard tests first. `cargo test -p bibcode-server --lib device::ios::guard` failed to compile with 7 errors: `GuardResult`, `GuardCache`, `FakeResolver`, `probe_with`, `counting_prober` and `unsupported_message` were missing.
  - The test selectors were adjusted to the amendment: the SimScreen registration selector instead of the damage one, and the "BiBCode" spelling.
- **GREEN.** After implementing, all 160 tests in `device::` pass.
- **Not strictly test-first:** the pixel/orientation tests, the follow-up tests and the real-simulator tests were written alongside the code. The real-simulator run caught 3 bugs (block signatures, screenshot to stdout, close race), and each was fixed and re-run.

## Gates (toolchain 1.98.0 on PATH)

| Command                                                                                        | Result                                                                                          |
| ---------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------- |
| `cargo fmt --all --check`                                                                       | pass                                                                                            |
| `cargo clippy -p bibcode-server --all-targets -- -D warnings`                                   | pass (only the pre-existing `linker_messages` eh_frame note)                                    |
| `cargo test -p bibcode-server --lib device::`                                                   | 160 passed                                                                                      |
| `cargo test -p bibcode-server --lib production::device`                                         | 4 passed                                                                                        |
| `cargo test -p bibcode-server --lib connect_mcp`                                                | 32 passed                                                                                       |
| `cargo test -p bibcode-server --test device_rpc --test device_media_socket`                     | 1 + 11 passed                                                                                   |
| `BIBCODE_IOS_SIM_TESTS=1 cargo test -p bibcode-server --test ios_simulator -- --nocapture`      | 2 passed (output below)                                                                         |
| `cargo test -p bibcode-server --test ios_simulator` (without the env var)                       | 2 passed (they skip)                                                                            |
| `vp run check:dependency-ledger`                                                                | "Dependency ledger is complete; … 138 registry Rust crates …"                                   |
| `vp check`                                                                                      | pass                                                                                            |
| `vp run typecheck`                                                                              | pass with the pinned toolchain on PATH                                                          |

With Homebrew cargo first on PATH, `vp run typecheck` fails on pre-existing `fetch_update` deprecations in unrelated files. That is the known toolchain shadowing problem, not this change.

## Real-simulator output

The run used iPhone 17 Pro `308FD3CD-F46A-44B9-9B68-9A849F337C80` on iOS 26.5 and Xcode 27.0, with Simulator.app not running. It was started with `BIBCODE_IOS_SIM_UDID=308FD3CD…` and `BIBCODE_IOS_SIM_ROTATE_TOOL=<scratchpad>/ios-spike/hid`, which is the spike's Purple rotate probe, used until Task 8 rotates natively.

```
running 2 tests
iOS availability: Availability { kind: Ready, message: "" }
308FD3CD-F46A-44B9-9B68-9A849F337C80 ready in 8.289299083s: Some(1206)xSome(2622) Portrait
com.apple.mobilesafari: 30013
mach_msg -> 0 ((os/kern) successful)
wrote …/scratchpad/task6/landscape.jpg
landscape frame: 720x332 Landscape
mach_msg -> 0 ((os/kern) successful)
wrote …/scratchpad/task6/reverse-landscape.jpg
mach_msg -> 0 ((os/kern) successful)
wrote …/scratchpad/task6/portrait.jpg
308FD3CD-F46A-44B9-9B68-9A849F337C80 Shutdown after 3.3216795s
test live_frames_turn_with_the_simulator ... ok
iOS availability: Availability { kind: Ready, message: "" }
308FD3CD-F46A-44B9-9B68-9A849F337C80 ready in 9.786689959s: Some(1206)xSome(2622) Portrait
wrote …/scratchpad/task6/first.jpg
first frame after 317.98475ms: 720x1566 Portrait, 25008 base64 bytes
frame 2 followed frame 1
308FD3CD-F46A-44B9-9B68-9A849F337C80 Shutdown after 3.608569s
test live_frames_arrive_from_a_booted_simulator ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 35.34s
```

Lines from the spike tool (surface hashes) are omitted above; the full output is in `scratchpad/task6/real-sim.txt`. Afterwards `xcrun simctl list devices | grep 308FD3CD` printed `iPhone 17 Pro (308FD3CD-F46A-44B9-9B68-9A849F337C80) (Shutdown)`.

I looked at the saved JPEGs:

- `first.jpg` (from an earlier run) is the home screen: upright, correct colours, so there is no R/B swap.
- `landscape.jpg` (`uiOrientation` 3) is Safari upright, with the Dynamic Island on the right.
- `reverse-landscape.jpg` (`uiOrientation` 4) is Safari upright, with the Dynamic Island on the left.

So the rotation direction matches §0's observation for both values.

## Dependencies

All are macOS-only (`[target.'cfg(target_os = "macos")'.dependencies]` in `apps/server/Cargo.toml`) and exactly pinned. The lock already resolved every version, so `Cargo.lock` gained only dependency edges.

| Crate                   | Pin        | Released   | Note                                                                                   |
| ----------------------- | ---------- | ---------- | -------------------------------------------------------------------------------------- |
| `objc2`                 | `=0.6.4`   | 2026-02-26 | feature `exception`; 0.6.5 (2026-10-05) is under 14 days old                           |
| `objc2-foundation`      | `=0.3.2`   | 2025-10-04 | features `NSError`, `NSString`, `NSUUID`                                               |
| `objc2-core-foundation` | `=0.3.2`   | 2025-10-04 | `CFRetained` and the downcast to `IOSurfaceRef`                                        |
| `objc2-io-surface`      | `=0.3.2`   | 2025-10-04 | features `IOSurfaceRef`, `IOSurfaceTypes`, `libc`, `objc2`, `objc2-core-foundation`    |
| `block2`                | `=0.6.2`   | 2025-10-04 |                                                                                        |
| `dispatch2`             | `=0.3.1`   | 2026-02-26 | features `std`, `objc2`                                                                |
| `png`                   | `=0.18.1`  | 2026-02-14 | already in the lock                                                                    |

- The objc2 family is on matching minors (0.6 / 0.3).
- `objc2-core-video`, `objc2-core-media` and `objc2-video-toolbox` were left for Task 7, because nothing here uses them.
- Each crate has a ledger entry `rust:apps/server:<name>` (cohort `ios-simulator-previews`, status green), and `inventorySummary.rustRegistry` went from 131 to 138.
- The workspace still declares `objc2 = "0.6"` and friends for desktop. Cargo unifies both to the same versions.

## Unsafe inventory (coresim.rs; pixels.rs and guard.rs have none)

Every `unsafe` block and `unsafe impl` has a `// SAFETY:` comment.

- `unsafe impl Send/Sync`:
  - `Library` (a dlopen handle);
  - `Symbol` (a code address);
  - `Object`: CoreSimulator objects are XPC-backed and messaged cross-thread by Simulator.app and idb; a shared `Object` is only `Screen`'s descriptor and UUID, which only the once-only `detach` messages after attach;
  - `IoSurface`: IOSurface is thread-safe, and its pixels are read only under `IOSurfaceLock`.
- `ManualBlockEncoding` impls for the three block signatures: each signature matches its `Arguments`/`Return`.
- `dlopen`/`dlsym` with valid C strings.
- `protocol_getMethodDescription` and `method_getTypeEncoding` read live runtime data. `CStr::from_ptr` reads runtime-owned NUL-terminated encodings.
- `msg_send_fn`:
  - transmutes `objc_msgSend` to a typed `extern "C-unwind" fn`;
  - a `const` assertion checks that the size matches;
  - each call site's signature comes from §0 and was re-verified by the guard;
  - no §0 method returns a struct.
- `Retained::retain` and `cast_unchecked` are applied to objects returned by getters whose classes §0 records (`NSUUID`, `NSError`).
- `CFRetained::retain` on a non-null object is followed by a checked `downcast` to `IOSurfaceRef`.
- `IOSurfaceLock`/`Unlock` take a null seed. The slice `from_raw_parts` is bounded by `min(bytes_per_row × height, alloc_size)` and lives only while the read lock is held. `upright_rgb` re-validates the geometry.
- Exceptions: every send runs inside `objc2::exception::catch`. The closures hold no values with destructors, so no Rust frame with drop glue is unwound. `an_objective_c_exception_becomes_an_error` proves the catch on a real `NSString` with an unknown selector.
- Callbacks:
  - each block is `'static` and captures only an `Arc<ScreenState>`, and a `const` assertion checks that `ScreenState` is `Send + Sync`;
  - the bodies only call `watch::send_modify`/`send_if_modified` and take a short `std::Mutex`. They never touch Tokio and never block.
  - the properties callback messages the props proxy through `guarded` inside an `autoreleasepool`;
  - the IOSurface is retained before it crosses threads.

## Self-review

- No `msg_send!` is used on anything from CoreSimulator. NSUUID `UUIDString` uses objc2-foundation's typed API on a real `NSUUID`.
- Autorelease pools wrap `attach`, `detach` and the properties callback. Every object we keep is retained.
- On the hot path (≤15 fps), one frame costs:
  - a 12 MB read under a read lock;
  - a single pass producing a 720-wide RGB buffer (about 3.4 MB portrait or 0.7 MB landscape);
  - a JPEG encode on a blocking thread.

  There is one registration per device, and the damage watch has no queue to grow.
- Interface deviations from the brief:
  - `Screen::attach(sim, udid)` has no `damage` mpsc parameter. One device-lifetime Screen feeds both the catalog orientation and any number of frame consumers, through `damage()`/`orientation()` watches, so Task 7 can subscribe the same way.
  - `read_rgb` replaces `read_bgra`: the JPEG and PNG paths want RGB, and converting in the same pass avoids a second 12 MB buffer.
  - `orientation()` yields `IosOrientation` (with `.orientation()` for the catalog), so `turn()` stays tied to the raw `uiOrientation` value.
  - `SymbolResolver::responds` returns the type encoding (`Option<String>`), so a changed signature is caught as well as a missing selector. There is also `protocol(name)` and `load()`.
- The guard result is computed on discovery, which runs only while Device previews is on. CoreSimulator is therefore never loaded otherwise.

## UI.md and React reviews (AGENTS.md items 5 and 6)

- **Item 5 (`vercel-react-best-practices`) does not apply.** Nothing in `apps/web` changed.
- **Item 6 (UI.md) reviewed for the changed user-visible copy:**
  - Each message names the state, and gives the fix where one exists.
  - The view-only reasons reuse the platform's availability message, so input, video and the picker give the same reason:
    - "Touch input is off: <reason>."
    - "Video is off: <reason>."
    - The `xcodeUnsupported` text is the controller's ruling.
  - The live "Input … is not available in this version of BiBCode yet." and "Video … not available … yet, so the screen updates as still images." are transitional until Tasks 8 and 7. They carry no action because there is none; the video one says what the user sees instead.
  - "The device did not return a screenshot. Try again in a moment." is now platform-neutral and still actionable.
  - "BiBCode could not attach to this simulator's screen." says why that device runs view-only.

## Profiler, crash and trace coverage

- No new child processes. The frameworks are loaded in-process, and the `simctl` children still go through `run_supervised`.
- No new RPC methods.
- Shutdown:
  - the screen is detached in `Shared::stop_now` before `simctl shutdown`, inside the existing device stop path, which runs before the profiler's teardown;
  - FFI failures log at `warn` with the selector or symbol name (`guarded`, `missing`, `error_from`, `probe_with`, IOSurface lock failures);
  - no screen content is logged.
- No new profiler events or findings.

## Docs

- Spec §0 has a one-line correction about block signatures, and spec §4 a correction that the screenshot goes to a file.
- `docs/testing/cross-platform-validation.md` (Android device previews section) now has the opt-in real-simulator command.
- `docs/architecture/overview.md` (Device previews) was left for Task 10's docs pass, as `progress.md` routes it there. It still describes the device manager accurately but does not yet describe iOS.
- The other runbooks were reviewed and remain accurate.

## Concerns

1. **Live devices refuse input (Task 8) and video (Task 7).** The client gets live JPEG frames at up to 15 fps and `DeviceMediaUnavailableError` at ticket time.
2. **`IosOrientation::from_ui` 3→landscape and 4→reverseLandscape are provisional**, per the ruling; Task 8 confirms them. `turn()` is tied to the observed values, and a test pins both together.
3. **Only iOS 26.5 was exercised.** iOS 27.0 is still unconfirmed, as in §0. The rotation check needs `BIBCODE_IOS_SIM_ROTATE_TOOL` (the spike probe in the scratchpad) until Task 8 can rotate.
4. **Attach failure goes view-only for that device only.** If the guard is Live but `attach` fails, the device runs view-only with a generic reason while the platform availability still says `ready`. This is logged at warn.
5. **`Screen::detach` runs inline in `Drop`.** That is a synchronous proxy call. Stop paths call it on a blocking thread first, so `Drop` is then a no-op.
6. **`host_manager_for_tests` is `#[doc(hidden)] pub`.** It is a public test hook that bypasses `hermetic-test-guard` by design, so the opt-in test can reach the host's simulators.
7. **Client reason mapping, for Task 9.** `apps/web/src/components/device/useDeviceVideo.ts` treats every `DeviceMediaUnavailableError` as "the server runs without sign-in" (`isMediaUnavailable`). Until Task 9 reads the error's message, an iOS device's still-images badge will likely show the sign-in reason instead of the server's "Video is off: …" or "not available yet" text.
8. **Shared-code fixes beyond iOS:** the close ordering fix in `Inner::finish`, and the platform-neutral screenshot copy. Both are covered by the existing manager tests.

## Fix round 1

Commit: `b9676a7c fix(server): guard CoreSimulator objects before CF calls (#70)`.

### Changes

1. **(Important) IOSurface retain.** `IoSurface::retain` now does nothing with an object until its real class is known.
   - It first calls `is_instance_of(object, "IOSurface")`, which walks `object_getClass`/`superclass` and sends no message. An NSProxy or ROCK proxy therefore cannot answer for its target.
   - The `CFRetained::retain` + `downcast` then runs inside `guarded`, so a raised exception refuses the object.
   - The SAFETY comment now says this.
   - Both call sites are covered: the `framebufferSurface` result in `register`, and the raw argument of the surfacesChanged block. On a refusal, the block ignores the surface and logs at warn once (`ScreenState::refused_surface`).
2. **(Minor) Second Xcode.** `GuardCache` records which `(developer_dir, version)` loaded live frameworks. A different key is not probed. It returns `ViewOnly { XcodeUnsupported, missing: RESTART_NEEDED }`, with a warn naming both Xcodes.
   - The availability message for that case is: "Xcode changed while the server was running. Restart the server to use live simulators; until then they open as screenshots without touch input."
   - Switching back to the loaded Xcode is live again.
3. **(Minor) Screenshot temp file.** The file is now held by `RemovedOnDrop`, whose `Drop` removes it. A future dropped mid-await no longer leaks it, and the earlier async removal is gone.
4. **(Minor) Registration that raises.** The `Screen` is now built before the registration result is checked. When the send raises, the `Screen` is dropped and `Drop → detach` makes a best-effort guarded `unregisterScreenCallbacksWithUUID:`.
5. **(Minor) Arrays and UUIDs.**
   - `elements()` gates `count` and `objectAtIndex:` with `respondsToSelector:`.
   - Before the cast, the UDID must pass `is_instance_of(…, "NSUUID")`. `UUIDString` runs inside `guarded`.

### Tests added

- `guard::a_second_xcode_never_loads_while_one_is_live`: uses a fake live `CoreSim`, checks that other dirs and versions return `RESTART_NEEDED` with the restart message, that there is one probe, and that switching back is live.
- `coresim::only_a_real_iosurface_is_retained_as_one`: an NSString and nil are refused; `is_instance_of` accepts subclasses and rejects other classes.
- `simctl::a_temporary_screenshot_is_removed_on_drop`.

### Commands and outputs (toolchain 1.98.0)

- `cargo fmt --all --check`: pass.
- `cargo clippy -p bibcode-server --all-targets -- -D warnings`: pass.
- `cargo test -p bibcode-server --lib device::`: `test result: ok. 163 passed; 0 failed`.
- `BIBCODE_IOS_SIM_TESTS=1 cargo test -p bibcode-server --test ios_simulator -- --nocapture`, run on 308FD3CD with the rotate tool. No `WARN` lines, so the real framebuffer passes the class check:

```
iOS availability: Availability { kind: Ready, message: "" }
308FD3CD-F46A-44B9-9B68-9A849F337C80 ready in 7.6082085s: Some(1206)xSome(2622) Portrait
landscape frame: 720x332 Landscape
308FD3CD-F46A-44B9-9B68-9A849F337C80 Shutdown after 3.428731s
test live_frames_turn_with_the_simulator ... ok
iOS availability: Availability { kind: Ready, message: "" }
308FD3CD-F46A-44B9-9B68-9A849F337C80 ready in 9.845087834s: Some(1206)xSome(2622) Portrait
first frame after 340.503292ms: 720x1566 Portrait, 24996 base64 bytes
frame 2 followed frame 1
308FD3CD-F46A-44B9-9B68-9A849F337C80 Shutdown after 3.522244625s
test live_frames_arrive_from_a_booted_simulator ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 34.78s
```

- `xcrun simctl list devices | grep 308FD3CD` printed `iPhone 17 Pro (308FD3CD-…) (Shutdown)`.
- No `bibcode-simulator-*` files were left in `$TMPDIR`.
