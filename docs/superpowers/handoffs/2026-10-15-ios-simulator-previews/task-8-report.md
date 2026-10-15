# Task 8 report: iOS input, text and rotation

Status: DONE_WITH_CONCERNS (concerns at the end).

Commits (base `77157c3b`):

- `f2d39740 feat(server): send touch, keys and text to iOS simulators (#70)`
- `4ae10b0c test(server): tap a turned iOS simulator and record the landscape mapping (#70)`: adds a landscape tap to the convention test and records the final mapping in spec §5.

## What was built

- **`device/ios/keymap.rs`** (new): `us_usage(c) -> Option<(u32, bool)>`, the US layout on HID page 0x07.
  - Covers letters, digits and all printable ASCII. `\n` and `\r` are Return (0x28), `\t` is Tab (0x2B).
- **`device/ios/hid.rs`** (new, FFI-free):
  - Types: `HidAction`, `TouchPhase`, `SimButton` and `PointerState`. `HidAction`'s `Debug` prints a paste's length, never its text.
  - `plan_events` refuses `back` and `appSwitch` for the whole batch before it touches pointer state ("iOS has no Back key." / "iOS has no Recents key.").
  - Single-pointer rules: a second `down` and events from other pointers are dropped, and `cancel` becomes `up`.
  - Touch ratios are clamped, then mapped into portrait space by `to_portrait`, keyed on `IosOrientation::turn()`:
    - Clockwise is (v, 1−u).
    - CounterClockwise is (1−v, u).
    - Half is (1−u, 1−v).
  - Text is split into maximal runs. A US-typeable run becomes key down/up pairs, with Shift 0xE1 around each shifted character. Any other run becomes `Paste`. `\r\n` counts as one Return.
  - `paste_keys()` returns Cmd 0xE3 down, V 0x19 down, V up, Cmd up.
  - `XpcValue` (U64/Double/Bool/Str/Dict, with `&'static CStr` keys) is the Rust-side description. `dtu_message` builds `IndigoDigitizerEvent`, `IndigoButtonEvent` (page 0x0C; Home 0x40, Lock 0x30, VolUp 0xE9, VolDown 0xEA) and `IndigoKeyboardButtonEvent`. `liveness_message` is the barrier key-up.
  - Legacy fallback:
    - `IndigoMessage` / `indigo_message` handle touch and buttons only. Keys and pastes return `None`.
    - A move is sent as a repeated down, the way idb does it.
    - Home and Lock go through `IndigoHIDMessageForButton` (sources 0 and 1). Volume goes through `HIDArbitrary(0x32, 0x0C, usage)`.
  - `single_touch` is §0's 0x140-byte re-envelope as a pure function. `purple_orientation_message` is §0's 112-byte Purple buffer as 28 u32 words.
  - `HidError { KeyboardUnavailable, Released, Failed }`.
  - `trait HidSink { keyboard_ready, send, rotate, release }`. The real sink is `coresim::HidClient`; tests use `RecordingHid`.
  - Constants: `KEYBOARD_NOT_READY`, `INPUT_FAILED` and `ROTATE_FAILED`.
- **`device/ios/coresim.rs`** (FFI):
  - One `extern "C"` block for libxpc and Mach (libSystem). No new crates, so the ledger is unchanged.
  - `XpcObject` (owned, `xpc_release` on drop) and `xpc_dictionary` (`XpcValue` → `xpc_object_t`).
  - `SendRight`: `mach_port_deallocate` on drop.
  - `lookup()`: `-[SimDevice lookup:error:]`, `respondsToSelector:`-gated, `guarded`, inside an autorelease pool.
  - `DtuConnection`:
    - `open`: endpoint → connection → sim2host → event handler → resume.
    - `liveness`: the reply barrier with a 4 s timeout.
    - `send`: message + barrier, with a 2 s timeout.
    - `cancel` runs once and also runs on drop.
  - `LegacyHid`: `alloc`/`initWithDevice:error:` (taken at +1 via `Object::owned`), the C builders through `Symbol::as_fn`, and `sendWithMessage:freeWhenDone:YES` on a private serial queue with a 2 s completion timeout.
  - `purple_rotate`: the lookup, then `mach_msg(SEND|SEND_TIMEOUT, 108, timeout 2000 ms)`, then the right is released.
  - `HidClient`:
    - `start` finds the device and spawns `handshake`: 5 attempts of open + liveness, each with a 4 s reply timeout, with 4 s backoff between them, and cancellable.
    - State machine: `Dtu::{Connecting, Live, Down}`. A dead or failed connection starts a new handshake the next time a send or `keyboard_ready` asks for it.
    - A touch gesture's moves and up follow the path its down took (`InputPath`), so a gesture never splits across DTUHID and legacy.
    - `release`: sets `released`, cancels the handshake, cancels the live connection, and drops the legacy client. `Drop` also cancels.
  - Block encodings renamed `VoidBlock` (`v8@?0`) and `ObjectBlock` (`v16@?0@8`). Every XPC handler, barrier and legacy completion is a signed block. The `dead_code` allowances on `Symbol`/`HidSymbols`/`CoreSim::hid` are removed.
  - Every FFI submission happens in a synchronous helper (`send_liveness`, `send_with_barrier`, `submit`), so no block or raw pointer lives across an `.await`. The async side only awaits tokio oneshots.
- **`device/ios/mod.rs`**:
  - `Control::{Live(Arc<dyn HidSink>), Off { reason }}` is kept separate from `Display`.
  - `attach_display` starts `HidClient` next to `Screen::attach` (in `spawn_blocking`). If the client cannot start, the device gets `Control::Off("BiBCode could not connect to this simulator's input.")`.
  - `Shared::input`:
    1. View-only refuses with "Touch input is off: <reason>.".
    2. A per-device `tokio::Mutex` serializes batches.
    3. Input after `stopping` gets `NotReady(Stopped)`.
    4. The batch is planned. If it needs the keyboard and DTUHID is not live, the whole batch is refused with `KEYBOARD_NOT_READY` before anything is sent or copied.
    5. Actions are sent in order. A paste is `pbcopy`, then the four keys.
  - Errors map through `sink_error`: `Failed` → `warn!` with the detail (never text) and `Emulator(INPUT_FAILED)`; `Released` → `NotReady(Stopped)`; keyboard → `InvalidInput`.
  - `Shared::rotate`:
    1. Sends `IosOrientation::from_orientation(to).purple()`.
    2. Reads the screen's orientation watch 5 times, 300 ms apart.
    3. Calls `report(observed, oriented_size(native, observed))` each time and stops once `to` is reached.
    4. Returns `Ok` even when the app does not turn.
  - `can_rotate()` is true for `Control::Live`.
  - `stop_now` releases the sink first (before the poll, the screen detach and `simctl shutdown`) and resets the pointer.
  - `LIVE_INPUT_PENDING` is removed. A test-only `IosBackendConfig::hid_for_tests` factory injects a sink.
- **`device/ios/pixels.rs`**: the orientation-convention fix (see below). Adds `from_orientation` and `purple()`; the tests pin `from_ui`, `turn` and `purple` together.
- **`device/media.rs` / `media_socket.rs`**:
  - `MediaSubscriber::end_reason(enabled)` is shared by the socket's close path and by the new `#[doc(hidden)] MediaFramesForTests::end_reason`, so the real-simulator test checks the socket's own reason.
  - **`device/mod.rs`**: adds `#[doc(hidden)] DeviceManager::png_for_tests`.
- **`tests/ios_simulator.rs`**:
  - Removed `BIBCODE_IOS_SIM_ROTATE_TOOL`; rotation now goes through `manager.rotate`.
  - `live_frames_turn_with_the_simulator` is now `rotation_changes_the_screen_size`.
  - New tests: `rotation_matches_android_convention`, `a_tap_changes_the_screen`, `text_arrives_in_a_text_field` (retries for up to 45 s while the keyboard is "not accepting … yet") and `external_shutdown_fails_device`.
  - `first_key_frame_decodes_to_the_screen_size` always checks the turned stream now.
  - New helpers: PNG decode (the `png` crate, already a dependency), a simctl PNG screenshot, and pixel-diff utilities.
  - `IosHarness::stop` accepts a device that already failed.
- **Runbook**: the real-simulator paragraph in `docs/testing/cross-platform-validation.md` now describes the new checks, drops the rotate tool and mentions `BIBCODE_IOS_SIM_FRAMES_DIR`.

## Orientation convention and the final mapping

Reasoning chain:

1. **The product's definition.** `apps/web/src/components/device/DevicePanel.tsx` (`ORIENTATIONS_LEFT`): turning the device **left** goes portrait → `landscape`. Turning a phone left is counter-clockwise.
2. **Android.**
   - `rotation_vector(Landscape) = [0, 0, -90]` and the emulator reports `SkinRotation::Landscape` = `Surface.ROTATION_90`. That value's documented meaning is "the device turned 90° counter-clockwise from natural (top to the left)". It is the standard Android phone landscape: camera on the left.
   - Note: the repo marks the -90 vector itself as an unverified native item ("native verification item 2", overview.md). No Android SDK is installed here (`~/Library/Android/sdk` is absent), so it could not be checked directly.
   - iOS therefore matches Android's **documented** semantics and the client's left/right definition. If item 2 later proves reversed, the Android vector should change, not iOS.
3. **Geometry.** A device turned counter-clockwise has its UI top along the device's physical **right** edge.
4. **§0 observations.** Dumping the surface showed the UI's top along the **right** edge at `uiOrientation` 4 (sent as Purple 3) and along the **left** edge at 3 (Purple 4).

Conclusion: Task 6's provisional `3 → landscape` was reversed. Final table:

| Catalog orientation | `uiOrientation` | Purple value sent | `turn()` (frame upright) | Touch ratio → portrait |
| --- | --- | --- | --- | --- |
| portrait | 1 | 1 | None | (u, v) |
| landscape | **4** | **3** | CounterClockwise (surface's right edge is the top) | (1 − v, u) |
| reversePortrait | 2 | 2 | Half | (1 − u, 1 − v) |
| reverseLandscape | **3** | **4** | Clockwise (surface's left edge is the top) | (v, 1 − u) |

What pins it:

- `pixels::tests::the_turn_and_purple_value_follow_the_observed_ui_value` pins `from_ui`, `turn` and `purple` together, including `Landscape.turn() == CounterClockwise`.
- `hid::tests::landscape_ratios_map_to_portrait_space` has one case per orientation. For (0.25, 0.5): landscape → (0.5, 0.25), reverseLandscape → (0.5, 0.75), reversePortrait → (0.75, 0.5).
- `mod::tests::rotate_sends_the_purple_value_for_the_android_convention` checks Landscape → 3, ReverseLandscape → 4, ReversePortrait → 2 and Portrait → 1.
- Real simulator, `rotation_matches_android_convention`:
  - `landscape` is reported back as `Landscape` at 2622×1206.
  - The frame BiBCode turned upright with `Landscape`'s turn (CounterClockwise) matches `simctl`'s own upright screenshot: mean difference **2.9**, against **18.3** for the same picture upside down.
  - Together with the unit test, this proves the turn used for `landscape` is physically right, so the UI's top really is along the right edge.
- In that test's landscape frame, the Dynamic Island sits on the frame's **left**. That is a direct visual check that `landscape` is the phone turned left, with its top to the left.
- In the same test, a tap at landscape ratio (0.5, 0.08) goes through BiBCode's own chain: catalog `Landscape` → `from_orientation` → `turn()` → `to_portrait` → DTUHID. It focused Safari's address field (10.5% of pixels changed; the saved frame shows the field and the keyboard). This proves the touch mapping on a turned simulator.
- The encoder's `rotation_turns_frames_like_the_jpeg_path` derives from `turn()` and still passes.
- Earlier, a DTUHID tap in landscape Safari worked at the mapped coordinates (§0), and the full rotation tests turn Safari correctly.

## TDD

- **RED**: I wrote the `hid.rs` tests first (the brief's five, plus pointer-across-batches, per-orientation mapping, CRLF, key mapping, paste keys, a redacted Debug, the DTUHID builders, legacy messages and the single-touch re-envelope), and `keymap.rs` before its implementation. `cargo test -p bibcode-server --lib device::ios` failed to compile: `us_usage` was unused and `HidAction`, `plan_events`, `PointerState` and the others were undefined.
- **GREEN**: after the implementation, all 73 `device::ios` tests passed. The new `mod.rs` tests are:
  - `input_after_stop_is_refused_without_ffi`: the recording sink gets the tap while Ready. After close it is released, input and rotate return `NotReady(Stopped)`, and `sent_after_release() == 0`.
  - `text_is_typed_and_the_rest_pasted_through_the_pasteboard`: `a` as keys, then `é` through the fake `pbcopy` (pasteboard = "é") and Cmd+V, then the Home button.
  - `keys_wait_for_the_keyboard_and_buttons_do_not`: with no keyboard, a touch+text batch is refused whole, with nothing sent and nothing copied. A volume button still goes out.
  - `rotate_sends_the_purple_value_for_the_android_convention`.
- I added `coresim::tests::dtuhid_messages_become_typed_xpc_dictionaries`: real libxpc conversion, read back with the `xpc_dictionary_get_*` getters (string, bool, nested dictionary, u64, double).

## Gates (rustup 1.98.0 first on PATH)

| Command | Result |
| --- | --- |
| `cargo fmt --all --check` | pass |
| `cargo clippy -p bibcode-server --all-targets -- -D warnings` | pass (only the existing `linker_messages` eh_frame note) |
| `cargo test -p bibcode-server --lib device::` | 198 passed |
| `cargo test -p bibcode-server --test device_rpc --test device_media_socket` | 1 + 11 passed |
| `BIBCODE_IOS_SIM_TESTS=1 BIBCODE_IOS_SIM_UDID=308FD3CD-… cargo test -p bibcode-server --test ios_simulator -- --nocapture` | 7 passed (output below) |
| `vp check` | pass |
| `vp run typecheck` | pass (with the rustup pin; Homebrew's newer cargo fails on existing `fetch_update` deprecations in unrelated files) |
| `vp run check:dependency-ledger` | not run: no dependency changed |

## Real-simulator output (iPhone 17 Pro 308FD3CD…, iOS 26.5, Xcode 27.0; logs filtered)

```
running 7 tests
ready in 7.48s: 1206x2622 Portrait
config (23 bytes) and key (59183 bytes) after 250.5ms
simulator HID service is ready attempt=1
rotate Landscape -> Landscape 2622x1206 in 304.9ms
landscape config after 305.3ms; 162 units, 1620641 bytes while turning
rotate Portrait -> Portrait 1206x2622 in 303.3ms
portrait config again
test first_key_frame_decodes_to_the_screen_size ... ok
rotate Landscape -> Landscape 2622x1206 in 305.1ms
landscape frame: 720x332 Landscape
rotate ReverseLandscape -> ReverseLandscape 2622x1206 in 304.0ms
rotate Portrait -> Portrait 1206x2622 in 304.9ms
test rotation_changes_the_screen_size ... ok
simulator HID service is ready attempt=1
typed after 1.02ms
5.9% of pixels changed after typing
test text_arrives_in_a_text_field ... ok
ios simulator ended outside BiBCode reason="Shutdown outside BiBCode"
failed 3.01s after simctl shutdown started: Some("Shutdown outside BiBCode")
simctl shutdown took 3.33s
test external_shutdown_fails_device ... ok
first frame after 414.7ms: 720x1566 Portrait
test live_frames_arrive_from_a_booted_simulator ... ok
simctl screenshot 2622x1206, BiBCode frame 400x184
mean difference: upright 2.9, upside down 18.3
test rotation_matches_android_convention ... ok
(rerun after 4ae10b0c adds the landscape tap:)
mean difference: upright 2.9, upside down 18.3
10.5% of pixels changed after a landscape tap
test rotation_matches_android_convention ... ok
83.6% of pixels changed 820.7ms after the tap
test a_tap_changes_the_screen ... ok
test result: ok. 7 passed; 0 failed; finished in 124.69s
iPhone 17 Pro (308FD3CD-F46A-44B9-9B68-9A849F337C80) (Shutdown)
```

Every test printed "<udid> Shutdown after ~3.3 s" and `simctl` lists the device as Shutdown at the end.

Measurements:

- **DTUHID liveness**: answered on attempt 1 in every run, about 0.1–2.7 s after `Ready`.
- **Pacing**: barrier-only pacing typed "bibcode" and "BiBCode" with no dropped or doubled characters. I checked the saved frames (`text-typed.png` shows `bibcode` in Safari's field). No per-key delay was added.
- **Barrier latency**: a 7-character batch (28 key actions) returned in about 1 ms. XPC barriers confirm hand-off to the kernel, not processing by `dtuhidd`.
- **Paste**: one manual run typed `"BiBCode é!"`. Shift worked and iOS showed its "Allow Paste" prompt for `é` (CoreSimulatorBridge), as spec §5 anticipated. The final test types `BiBCode` only, to stay deterministic.
- **Port references**, from a scratch probe:
  - `lookup:error:` inside a drained pool leaves exactly one send right.
  - The XPC endpoint does not take that right; the connection takes its own.
  - BiBCode keeps its right for the connection's lifetime (Purple: until after `mach_msg`), then deallocates it.

## Unsafe inventory (new code)

- **`hid.rs` and `keymap.rs`**: none.
- **`coresim.rs`**: 48 added lines contain `unsafe`, each with a `SAFETY` comment:
  - one `extern "C"` block (libxpc, `mach_msg`, `mach_port_deallocate`, `mach_task_self_`, `mach_absolute_time`);
  - `Symbol::as_fn` (transmute to the §0 signature);
  - `send4`;
  - `Object::owned` (`Retained::from_raw` for `init…`);
  - `XpcObject` `Send`/`Sync` and drop;
  - `xpc_is`;
  - dictionary setters;
  - `SendRight` drop;
  - the `lookup:error:` send (`guarded`);
  - the 4sim calls, event handler and resume;
  - the liveness reply send;
  - message + barrier send;
  - `xpc_connection_cancel`;
  - `IndigoBuffer` drop (`free`);
  - legacy `alloc`/`init` (`guarded`);
  - the builder calls, `malloc_size`, slice read, `calloc` and copy;
  - the `NSError` cast after an `is_instance_of` check;
  - `sendWithMessage:` (`guarded`; leaks on exception instead of a possible double free);
  - `mach_msg`.
- **Objective-C exceptions**: every ObjC send goes through `guarded`. The C calls (libxpc, the Indigo builders, Mach) raise no ObjC exceptions.
- **Blocks**: every block is signed (`VoidBlock` / `ObjectBlock`) and captures only `Send + Sync` data: an atomic flag or a mutex-wrapped oneshot sender.
- **Blocking work**: none on Tokio workers.
  - Lookups, legacy init and `mach_msg` run in `spawn_blocking`.
  - Every barrier, reply and completion is awaited through a tokio oneshot with a timeout.

## Self-review

- **Brief and spec**:
  - DTUHID first, with a fixed `featureIdentifier`.
  - Liveness: 5 × 4 s reply timeout, 4 s backoff.
  - Legacy fallback for touch and buttons only; keys and text refused with the exact copy.
  - Button usages and the `back`/`appSwitch` copy as specified.
  - Text runs and paste as specified. The ≤1000-character `validate` stays in `DeviceManager::input`.
  - No text is logged, only `chars` counts and error details.
  - Purple with a 2000 ms send timeout; `can_rotate` true for live simulators.
  - The catalog follows the observed orientation.
- **Input before liveness never waits for the handshake**: touch and buttons use legacy, and keys return the copy at once.
- **Ordering**: one batch at a time per device, and actions are awaited one by one.
- **Stop path**:
  - The sink is released before the screen detaches and before `simctl shutdown`.
  - A send that races the stop returns `NotReady(Stopped)`.
  - The handshake task is cancelled on release or drop, so a cancelled launch leaves no task running for 40 s.
- **Copy** uses "BiBCode".
- **Profiler coverage**: no new child process (`pbcopy` already goes through the supervised `SimctlRunner::simctl`), no new RPC method (`device.input` and `device.rotate` are already registered and in `is_long_running`), no worktree removal, binary mode or shutdown service. The handshake task is cancelled by the existing stop. Send failures log at `warn` through `tracing` without content. Nothing else applies.
- **Living docs**:
  - Runbook updated.
  - `docs/architecture/overview.md` has no iOS section yet. The plan gives that section to Task 10, so the iOS input/rotation description (DTUHID, the legacy fallback, Purple, the orientation convention above) is for Task 10 to write.

## Concerns

1. **Paste order with the iOS prompt.** When iOS shows "Allow Paste", the pasted run lands only after the user taps Allow. Keys sent after it (`!` in `BiBCode é!`) appear first. This is iOS behaviour; the client copy should say a paste prompt may appear (spec §5 already plans that pane note).
2. **Android's -90 vector is still unverified** (native item 2). iOS follows the documented Android semantics and the client's "turn left → landscape". If item 2 proves reversed on a real SDK, change the Android vector.
3. **The legacy fallback was not exercised end to end in this task**: DTUHID answered on the first attempt every time. Its message layouts are unit-tested against §0, and §0's spike confirmed the same calls, but the Rust legacy path (and moves as repeated downs) has no real-simulator run. A test that forces legacy would need a hook to disable DTUHID.
4. **Weak text check, and untested bursts.** `text_arrives_in_a_text_field` passes on "the frame changed by more than 5%" after typing (5.9% in the run). The saved frame shows the exact text, but the test does no OCR. Pacing was measured on 7 characters only. A barrier confirms hand-off to the kernel, not processing by `dtuhidd`, so a 1,000-character event (about 4,000 messages sent in well under a second) is untested. Task 10's native validation should try a long string with no paste in it.
5. **Only iOS 26.5 was exercised**, as in §0. iOS 27.0 is unconfirmed.
6. **`docs/architecture/overview.md` has no iOS section yet.** The plan gives that section to Task 10, which now owns describing:
   - iOS input over DTUHID, with the legacy fallback;
   - rotation through the Purple port, with `canRotate` true for live simulators;
   - the orientation convention.

   The runbook and spec §5 are updated here.

## Fix round 1

Commit: `f7c0ea20` fix(server): keep iOS pointer state and paste whole text events (#70). This round builds on the controller's spec §5 amendment `3902cd28` and leaves the spec unedited.

1. **Pointer state (Important).**
   - `Shared::input` now plans against a clone of `PointerState` and stores it only after the keyboard gate passes.
   - If a touch Down fails to send, the pointer is reset to none.
   - `PointerState` now derives `Clone`, `Debug` and `PartialEq`.
   - New test `a_refused_batch_leaves_the_pointer_up`: after `[down(0), text "é"]` is refused with KEYBOARD_NOT_READY, the next tap's Down is sent.
2. **Whole-event paste (Important, per amended §5).**
   - `plan_text`: a text event that contains any character `us_usage` cannot type becomes exactly one `Paste { text }` of the full event, followed by Cmd+V at send time.
   - An all-US event is still typed as keys.
   - `mixed_text_is_split_in_order` is replaced by `mixed_text_is_pasted_whole`: "Hi é😀!" becomes exactly one Paste.
   - New test `us_text_is_typed_as_keys`: "Hi !" becomes the Shift/key sequence.
   - The fake-backed test is now `text_with_a_non_us_character_is_pasted_whole`: "aé" gives pasteboard "aé" and then only Cmd+V.
   - Review Focus naming: `mixed_text_is_split_in_order` is now **`mixed_text_is_pasted_whole`**.
3. **Released before path (Minor).** `HidInner::path` returns `HidError::Released` as soon as the client is released. `legacy()` also checks `released` before it spawns the blocking `LegacyHid::new`.
4. **Stopping before pbcopy (Minor).** `Shared::input` checks `stopping` right before spawning `simctl pbcopy` and returns `NotReady(Stopped)` if it is set.

Gates (rustup 1.98.0):

| Gate | Result |
| --- | --- |
| `cargo fmt --all --check` | pass |
| `cargo clippy -p bibcode-server --all-targets -D warnings` | pass |
| `cargo test -p bibcode-server --lib device::` | 200 passed |
| `cargo test -p bibcode-server --test device_rpc --test device_media_socket` | 1 + 11 passed |
| `BIBCODE_IOS_SIM_TESTS=1 cargo test --test ios_simulator` | 7 passed |

Real-simulator results:

- `external_shutdown_fails_device`: the device failed 2.84 s after the shutdown started.
- `rotation_matches_android_convention`: mean difference 3.3 upright against 17.8 upside down; the landscape tap changed 13.1% of pixels.
- `a_tap_changes_the_screen`: 83.6% of pixels changed.
- `text_arrives_in_a_text_field`: 5.9% of pixels changed.
- 308FD3CD is Shutdown afterwards.

## Fix round 2

Commit: `5ef723ba` fix(server): lift a held iOS touch when a send fails (#70).

**The gap.** After a send failed partway through a batch on anything other than a Down, the committed pointer state was the batch's planned end state. The host could then think the pointer was up while the guest still had a finger down.

**The fix:**

- `Shared` tracks `held: Mutex<Option<(f64, f64)>>`, the portrait position a touch the guest may still hold was last sent to.
  - It is set before a Down or Move goes out, so an attempted Down counts as held.
  - It is cleared only after an Up was sent successfully.
  - It carries over between batches and is cleared on stop.
- `Shared::input` sends the plan through `send_plan`. On **any** error (a touch, key, paste-key or `pbcopy` failure, or a stop racing the batch), `lift_held_touch`:
  1. sends a best-effort Up at the held position if one is held, ignoring its result and logging at debug;
  2. resets `PointerState` to idle and clears `held`;
  3. returns the original error.
- The Up goes on the path its Down took, because `HidClient` pins a gesture's path at Down and the Up takes that pin.
- `RecordingHid` gained `fail_next(predicate)`: a one-shot failure, and the failed action is not recorded.

New tests (in `device/ios/mod.rs`):

- (a) `a_failed_move_lifts_the_touch_and_frees_the_pointer`: `[Down, Move(fails), Up]` sends Down(0.1,0.2) then a best-effort Up(0.3,0.4). The next tap's Down is sent.
- (b) `a_failed_up_for_a_held_pointer_is_retried_and_resets`: a pointer held from earlier batches (Down, then Move to 0.2,0.2), then `[Up(fails)]`. A best-effort Up is sent at (0.2,0.2) and the next tap's Down is sent.
- (c) `a_failed_key_after_a_held_down_lifts_the_touch`: `[Down, Key Enter(fails)]` sends the Down, then a best-effort Up at (0.1,0.2). The next tap's Down is sent.

Gates (rustup 1.98.0):

| Gate | Result |
| --- | --- |
| `cargo fmt --all --check` | pass |
| `cargo clippy -p bibcode-server --all-targets -- -D warnings` | pass |
| `cargo test -p bibcode-server --lib device::` | 203 passed |
| Real-simulator suite | not re-run: no FFI changed |
