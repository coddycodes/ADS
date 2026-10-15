# Task 7 report: VideoToolbox H.264 source for iOS simulators

Status: DONE_WITH_CONCERNS (concerns at the end).

Commit (base `b9676a7c`): `5d3056e8 feat(server): stream iOS simulators as H.264 through VideoToolbox (#70)`.

## What was built

- **`device/ios/encoder.rs`** (new):
  - Pure functions: `avcc_to_annex_b`, `config_unit`, `bitrate_for` (6 bit/px/s, clamped to 2–12 Mbit/s), `rotated_size`, `has_idr` (a key is a sample that contains a NAL unit of type 5), and `sps_dimensions` (removes emulation-prevention bytes, reads Exp-Golomb fields, handles the high-profile chroma block, scaling lists and frame cropping).
  - `EncoderState::on_sample` sends `Config` before a `Key` whose size or SPS/PPS changed. `on_lost` drops deltas until the next key, and that key carries its config again. `resend_config` is used by resets.
  - `Output` (the refcon) holds the `mpsc::Sender` with capacity 8, the `Mutex<EncoderState>` and the shared `Requests { force_key, resend_config }`.
  - `on_output` is the `VTCompressionOutputCallback`, wrapped in `catch_unwind`. It reads the block buffer with `copy_data_bytes`, reads SPS/PPS with `CMVideoFormatDescriptionGetH264ParameterSetAtIndex`, and requires a 4-byte NAL header length. It reads the dimensions from the format description and the PTS from the sample. Each unit goes out with `try_send`. On a full or closed queue it calls `on_lost`, sets `force_key` and drops the rest. It makes no Tokio calls other than `try_send`, and logs only status codes.
  - `Compressor` owns a `VTCompressionSession` at one output size. It sets:
    - RealTime=true
    - AllowFrameReordering=false
    - MaxKeyFrameInterval=120
    - MaxKeyFrameIntervalDuration=2.0
    - ProfileLevel=H264_Main_AutoLevel
    - AverageBitRate=`bitrate_for`

    A refused property is logged with a warning and the session keeps its default. On `Drop` it calls `complete_frames(kCMTimeInvalid)` and then `invalidate`, so no callback outlives the `Output`.
  - `Rotator` owns a `VTPixelRotationSession` (CW90 / CCW90 / 180) and a `CVPixelBufferPool` of IOSurface-backed 32BGRA buffers at the rotated size. On `Drop` it calls invalidate. An `Invalidate` guard covers the case where pool creation fails.
  - `Pipeline` belongs to the encoder thread. Its fields drop in this order: compressor, rotator, wrapped surface, options, output. Each call to `encode(&IOSurfaceRef, orientation, force_key)`:
    1. rebuilds the compressor when the rotated size changes;
    2. wraps a new surface once (`CVPixelBufferCreateWithIOSurface`, cached by address);
    3. for portrait, passes the wrapped buffer straight to the encoder (zero-copy);
    4. otherwise rotates into a pool buffer;
    5. encodes with PTS = µs since start (timescale 1e6) and `ForceKeyFrame` when requested.
  - `SimulatorVideo::start(Arc<Screen>)` (async) spawns the `bibcode-ios-encoder` thread and waits on a oneshot until the first frame has encoded. A failure returns the error after joining the thread.
    - The thread uses a small std `block_on` (park/unpark waker) to wait on `damage.changed()`, the reset `Notify` or the stop token. It sleeps to keep frames at least 16 ms apart, coalesces with `borrow_and_update`, and reads the orientation watch.
    - Three encode failures in a row end the stream as failed.
  - `VideoSource` impl:
    - `next_unit` receives from the channel.
    - `reset_video` sets `resend_config` and `force_key` and notifies, so a static screen still produces config and key at once.
    - `failed` reads an atomic.
    - `stop` cancels and joins the thread (with `spawn_blocking`). `Drop` cancels.
- **`device/ios/mod.rs`**:
  - `video()` returns `Box::new(SimulatorVideo::start(screen).await?)` for `Display::Live`.
  - `video_unavailable()` is `None` for live devices and still "Video is off: …" for view-only ones.
  - `LIVE_VIDEO_PENDING` is removed and the module docs are updated.
- **`device/ios/coresim.rs`**: removed the `dead_code` allowance on `IoSurface::as_surface`.
- **`device/media.rs`**: `#[doc(hidden)] pub fn DeviceManager::media_frames_for_tests` and `MediaFramesForTests::next_frame`. They exist only for the opt-in integration test, because `attach_media` and `MediaSubscriber` are `pub(crate)`.
- **`device/mod.rs`**: `#[doc(hidden)] pub use ios::encoder::sps_dimensions as h264_sps_dimensions_for_tests` (macOS only).
- **`device/ios/fixtures/`**:
  - `vt-key-128x256.bin` is an AVCC key sample recorded from VideoToolbox.
  - `vt-sps-1206x2622.bin` is the SPS VideoToolbox writes for the iPhone 17 Pro framebuffer size: `27 4d 00 32 …`, Main profile, level 5.0, 76×164 MBs cropped to 1206×2622.
  - Both were recorded with `BIBCODE_RECORD_VT_FIXTURES=1` by the VideoToolbox unit test.
- **`tests/ios_simulator.rs`**: `first_key_frame_decodes_to_the_screen_size`, with the `screen_size`, `media`, `first_video_units`, `next_unit` and `config_of_size` helpers. It does the following:
  1. checks that a media ticket is now issued for a live simulator;
  2. checks that the first two units are Config then Key, and that the SPS gives exactly the catalog's 1206×2622;
  3. watches the stream for 2 s, checking that PTS never goes backwards;
  4. with `BIBCODE_IOS_SIM_ROTATE_TOOL` set (spike S2's Purple rotate probe), launches Safari, rotates to landscape and waits for a 2622×1206 config followed by a key, then rotates back and waits for 1206×2622 again;
  5. shuts down and verifies `Shutdown` (the existing `ShutdownOnDrop` also covers failures).
- **Deps**: `Cargo.toml` (macOS table, alphabetical) adds the three crates. `objc2-core-foundation` now names the `CFDictionary`, `CFNumber` and `CFString` features it uses. The ledger gains 3 entries and `rustRegistry` goes from 138 to 141.
- **Runbook**: `docs/testing/cross-platform-validation.md` now says that the real-simulator tests check the H.264 stream and mentions the rotate tool.

## TDD

- **RED**: I wrote the brief's four pure tests first. `cargo test -p bibcode-server --lib device::ios::encoder` failed to compile with 13 errors: `avcc_to_annex_b`, `bitrate_for`, `config_unit`, `EncoderState` and `Sample` were missing.
- **GREEN**: after the implementation, all 11 encoder tests pass. The 7 tests beyond the brief are:
  - `a_lost_unit_skips_deltas_until_a_key_with_config` (pure)
  - `only_idr_slices_make_a_key` (pure)
  - `sps_dimensions_apply_cropping` (fixture)
  - `a_recorded_key_sample_converts_to_annex_b` (fixture)
  - `rotation_turns_frames_like_the_jpeg_path`: real `VTPixelRotationSession` output, compared pixel by pixel with `pixels::upright_rgb` for Landscape, ReverseLandscape and ReversePortrait. This pins the H.264 rotation direction to the JPEG path, which Task 6 checked visually.
  - `videotoolbox_sends_config_before_key_and_again_after_a_turn`: real VideoToolbox on synthetic IOSurfaces.
    - Portrait gives Config+Key with SPS 128×256.
    - A new picture gives a Delta with a larger PTS.
    - Landscape gives Config+Key with SPS 256×128.
    - A forced key gives only a Key.
    - Going back to portrait gives Config+Key with 128×256.
    - Dropping the pipeline closes the channel.
  - `a_full_queue_drops_to_the_next_key_frame`: with capacity 2 and nobody draining, the config and key fill the queue. The next delta then overflows, which sets `force_key`. After a drain, the following delta is dropped (stream broken), and the forced key gets through as `[Config, Key]`. (After review, I rewrote this test from a capacity-1 version that never showed the recovery.)
- The first GREEN run failed the two fixture tests only because the placeholder files were empty when `include_bytes!` ran. Recording the fixtures and running again passed.

## Gates (toolchain 1.98.0 on PATH)

| Command | Result |
| --- | --- |
| `cargo fmt --all --check` | pass |
| `cargo clippy -p bibcode-server --all-targets -- -D warnings` | pass (only the pre-existing `linker_messages` eh_frame note) |
| `cargo test -p bibcode-server --lib device::` | 174 passed |
| `cargo test -p bibcode-server --test device_rpc --test device_media_socket` | 1 + 11 passed |
| `BIBCODE_IOS_SIM_TESTS=1 BIBCODE_IOS_SIM_UDID=308FD3CD-… BIBCODE_IOS_SIM_ROTATE_TOOL=<scratchpad>/ios-spike/hid cargo test -p bibcode-server --test ios_simulator -- --nocapture` | 3 passed (output below) |
| `vp run check:dependency-ledger` | "Dependency ledger is complete; … 141 registry Rust crates … 0 unaccounted entries" |
| `vp check` | pass |
| `vp run typecheck` | pass |

## Real-simulator output (iPhone 17 Pro 308FD3CD…, iOS 26.5, Xcode 27.0)

```
iOS availability: Availability { kind: Ready, message: "" }
308FD3CD-F46A-44B9-9B68-9A849F337C80 ready in 7.404332625s: Some(1206)xSome(2622) Portrait
config (23 bytes) and key (78766 bytes) after 216.688666ms
1 more units (160051 bytes) in 2 s on a quiet screen
com.apple.mobilesafari: 71925
landscape config after 1.681150333s; 4 units, 344912 bytes while turning (1.6 Mbit/s)
portrait config again
308FD3CD-F46A-44B9-9B68-9A849F337C80 Shutdown after 3.355127708s
test first_key_frame_decodes_to_the_screen_size ... ok
test live_frames_arrive_from_a_booted_simulator ... ok
test live_frames_turn_with_the_simulator ... ok
test result: ok. 3 passed; 0 failed
iPhone 17 Pro (308FD3CD-F46A-44B9-9B68-9A849F337C80) (Shutdown)
```

What the run measured:

- **Time to the first key**: 217 ms from attach. The first key frame is 79 KB.
- **Quiet screen**: frames go out only on the frame signal, so the home screen produced 1 unit in 2 s. That unit was the forced key from the subscriber's reset, 160 KB. `MediaHub::subscribe` calls `maybe_reset` for every first subscriber, on Android too, so this is the hub's existing behaviour, not the encoder's.
- **Decodable by a real decoder.** A second run with `BIBCODE_IOS_SIM_FRAMES_DIR` wrote config+key to `first.h264`:
  - `ffprobe` reports `codec_name=h264|profile=Main|width=1206|height=2622|pix_fmt=yuv420p|level=50`.
  - `ffmpeg` decoded the key to a PNG that shows the upright home screen.
- **Not measured: 60 fps throughput and steady-state bitrate.** No test drove an animated screen. The 1.6 Mbit/s figure covers only the rotation window. The cap is 16 ms between frames, which the unit tests pin.

## Dependencies

All three crates were released 2025-10-04 (crates.io `created_at`), more than a year before 2026-10-15. Each is an exact pin in the macOS-only table and has a ledger entry with cohort `ios-simulator-previews`.

| Crate | Pin | Features |
| --- | --- | --- |
| `objc2-core-media` | `=0.3.2` | std, CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMTime |
| `objc2-core-video` | `=0.3.2` | std, CVBase, CVBuffer, CVImageBuffer, CVPixelBuffer, CVPixelBufferIOSurface, CVPixelBufferPool, CVReturn, objc2-io-surface |
| `objc2-video-toolbox` | `=0.3.2` | std, VTCompressionProperties, VTCompressionSession, VTErrors, VTPixelRotationProperties, VTPixelRotationSession, VTSession, objc2-core-media, objc2-core-video |

`objc2-core-foundation` (already pinned) now names CFDictionary, CFNumber and CFString. `Cargo.lock` adds only `objc2-core-media` and `objc2-video-toolbox` 0.3.2. `objc2-core-video` was already in the lock, through another crate.

## Unsafe inventory (`encoder.rs`)

Every unsafe block has a `SAFETY` comment; I checked this with a script that scans the six lines before each site.

- **Callback**:
  - the `unsafe extern "C-unwind" fn on_output`;
  - dereferencing the refcon (`Arc::as_ptr` of an `Output` that outlives every session, guaranteed by `complete_frames` + `invalidate` before drop and by field drop order);
  - `NonNull::as_ref` of the sample.
- **CoreMedia reads**: `data_buffer`/`format_description` (retained), `data_length`, `copy_data_bytes` into a correctly sized Vec, `GetH264ParameterSetAtIndex` with valid out-pointers and the slice copied immediately, `GetDimensions`, `presentation_time_stamp`.
- **VideoToolbox**:
  - `VTCompressionSession::create` and `VTPixelRotationSession::create`, with `CFRetained::from_raw` on their +1 results;
  - `VTSessionSetProperty` with static keys and values;
  - `prepare_to_encode_frames`, `encode_frame`, `complete_frames`, `invalidate` (×3 sites), `rotate_image`.
- **CoreVideo**: `CVPixelBufferPool::create` / `create_pixel_buffer` and `CVPixelBufferCreateWithIOSurface`, each with `from_raw` on the +1 result.
- **Statics and values**: reads of the VT/CV extern statics, and `CMTime::new`.
- **Tests only**: `IOSurfaceRef::new`, surface lock/unlock and pixel writes, CVPixelBuffer lock/unlock and pixel reads.

CF objects are always held as `CFRetained`. Sessions are invalidated when the stream stops, when the session is rebuilt for a new size, and on error paths: a half-built `Rotator` is covered by the `Invalidate` guard, and a failed `Compressor::new` after creation drops the compressor, which invalidates it. The `Output` Arc is dropped only after the sessions.

## Self-review

- Every binding decision is met:
  - Rotation happens on the server with a pool at the rotated size.
  - Output size is the rotated size, and a change rebuilds the session and sends Config before Key (the pure test and the VideoToolbox test both pin this).
  - Pacing follows the frame signal, at most 60 fps.
  - Resets force a key and wake the loop.
  - The callback only converts and `try_send`s into a channel of 8; when that fails it sets `force_key` and drops the frame.
  - Live devices accept tickets; view-only devices still refuse them (`view_only_simulators_refuse_input_and_screenshot_as_jpeg` still passes).
- Logs carry only status codes, sizes and error strings, never pixels.
- Profiler coverage: no child process, RPC method, worktree removal, binary mode or shutdown service is added. The encoder thread is in-process and stops with the media hub, which already stops sources before the device stops. Failures at the VideoToolbox boundary are logged with `tracing::warn!`. **None of the Profiler/Crash/Trace items apply.**
- UI.md and vercel-react-best-practices reviews: not applicable, because this is a server-only change. The visible effect (live iOS simulators now show H.264 video instead of still JPEGs) goes through the existing client pipeline unchanged.
- Living docs: `docs/architecture/overview.md` does not describe iOS yet. I updated the testing runbook. The other affected runbooks were **reviewed and remain accurate**.

## Concerns

1. **Portrait encoding is zero-copy from the live framebuffer.** VideoToolbox reads the surface asynchronously while the guest may be drawing into it, so tearing is possible in portrait. Rotated frames go through a GPU copy into the pool and are not affected. If tearing shows up, the fix is to always go through the rotation session with `kVTRotation_0`.
2. **VideoToolbox picks level 5.0 for 1206×2622** (SPS `4d 00 32`). At 60 fps that is above level 5.0's MaxMBPS on paper.
   - ffmpeg decodes the stream correctly, but browser WebCodecs decode is still unverified.
   - `kVTCompressionPropertyKey_ExpectedFrameRate` is the setting that would move the auto-level. It is outside the brief's verbatim property list, so I did not set it.
3. **Steady-state fps and bitrate on an animated screen were not measured** (see the measurements above).
4. **`resend_config` can be consumed by a key frame that was already in flight** when the reset arrived. The receiver still gets Config before a Key, so this is harmless.
5. **Key detection uses NAL type 5 (IDR)** in the sample, not the `NotSync` attachment. VideoToolbox's forced and periodic keys are IDR, so this is equivalent here, and the check is pure and testable.
6. **Three unit tests run real VideoToolbox** (public framework, no simulator, synthetic IOSurfaces) in the hermetic `device::` suite. They need a macOS host where VideoToolbox can create an H.264 encoder. Apple's software encoder covers a CI VM without a hardware encoder, but this has not been run on CI.
7. **`sps_dimensions` lives in `encoder.rs`** as a `#[doc(hidden)] pub` re-export (`h264_sps_dimensions_for_tests`), not in the test helper the brief described. That keeps one parser for the unit tests and the integration test. `media_frames_for_tests` is likewise a `doc(hidden)` public test hook.
8. **The `uiOrientation` 3/4 mapping is still provisional (Task 8).** The H.264 rotation follows `IosOrientation::turn()`, so it changes together with the JPEG path.

## Fix round 1 (commit `77157c3b fix(server): report iOS encoder failures and recover still screens (#70)`)

### Changes

1. **Failures reach the hub (Important).**
   - `SimulatorVideo::launch` spawns the thread around an encoder closure that `make_encode` builds on that thread. In production the closure owns the `Pipeline` and, through it, the `Output` and the units' sender.
   - `run_encoder` now borrows the encoder (`&mut impl Encode`) and runs inside `catch_unwind`.
   - The thread stores `failed = true` (on an error result, or on a panic, which is also logged with `tracing::error!`) **before** it explicitly drops the encoder. So the sender closes only after `failed()` is already true.
   - A clean cancel returns `false` and leaves `failed` unset.
2. **Still screens recover (Important, Option A).**
   - The wait (`block_on`, now with a park timeout) wakes every `RETRY_INTERVAL` = 100 ms. On a timeout with no signal, it encodes the current surface again, but only while `force_key` is set (by the callback after an overflow or a lost sample, or left set after a failed forced encode) or after a failed encode.
   - Otherwise it goes back to waiting, so an idle stream costs one thread wake every 100 ms and no encode.
   - The VideoToolbox callback is unchanged: it only `try_send`s and sets flags.
   - The first attempt computed the timeout only at the start of the wait. The new test showed this missed a `force_key` the callback set after the thread had parked, which is the real overflow case. The wait therefore always has a timeout.
3. **Level.** I added `kVTCompressionPropertyKey_ExpectedFrameRate = 60`. With it, AutoLevel declares **level_idc 51 (5.1)**, confirmed by the real-simulator SPS (`profile_idc 77 level_idc 51`) and by `ffprobe` (`h264|profile=Main|width=1206|height=2622|level=51`). I did not need the explicit `Main_5_1` fallback. The `vt-sps-1206x2622.bin` fixture still records the earlier level-5.0 SPS; it is only used for the dimension parse.
4. **`has_idr`** splits each NAL unit by its length, so a zero-length NAL unit is skipped instead of its following length byte being read as a header. Tests were added for both cases.
5. **`read_sample`** returns an error when `data_length() == 0`. The destination pointer is now `NonNull::from(&mut avcc[..])`.
6. **`MIN_FRAME_INTERVAL`** is now `Duration::from_micros(16_667)`, which caps the stream at 60 fps or less.
7. **Real-simulator test** now prints the SPS profile and level, and how many of the units after the first key were keys.

### Tests

New tests, driven by a scripted encoder through the real thread, `launch` and `started` (`scripted()`):

- `an_encoder_failure_is_reported_when_the_units_end`: the first frame is OK. One damage event then makes encodes fail. The retries fail too without any further signal. `next_unit()` returns `None`, and at that moment `failed()` is already `true`.
- `a_panicking_encoder_is_reported_as_failed`: the encoder panics on its second call. `next_unit()` returns `None` and `failed()` is `true`.
- `a_still_screen_is_encoded_again_while_a_key_is_pending`: after the first encode, `force_key` is set the way the callback sets it, with no damage. A forced encode follows within 2 s. Once nothing is pending, there are no further encodes within 300 ms.
- `a_failed_encode_is_retried_without_a_new_frame`: one damage event, then the encode fails. It is retried with no new signal, succeeds, and the source is not failed.
- `only_idr_slices_make_a_key` gained the two zero-length NAL cases.

The encoder suite now has 15 tests.

### Commands and outputs (toolchain 1.98.0)

| Command | Result |
| --- | --- |
| `cargo fmt --all --check` | pass |
| `cargo clippy -p bibcode-server --all-targets -- -D warnings` | pass |
| `cargo test -p bibcode-server --lib device::` | 178 passed |
| `cargo test -p bibcode-server --test device_rpc --test device_media_socket` | 1 + 11 passed |
| `BIBCODE_IOS_SIM_TESTS=1 BIBCODE_IOS_SIM_UDID=308FD3CD-… BIBCODE_IOS_SIM_ROTATE_TOOL=<hid> BIBCODE_IOS_SIM_FRAMES_DIR=<scratch> cargo test -p bibcode-server --test ios_simulator -- --nocapture` | 3 passed; simulator `Shutdown` afterwards |

```
config (23 bytes) and key (1053 bytes) after 202.424208ms
SPS profile_idc 77 level_idc 51
43 more units (1115726 bytes) in 2 s on a quiet screen
landscape config after 1.644600625s; 4 units, 345577 bytes while turning (1.7 Mbit/s)
portrait config again
test result: ok. 3 passed; 0 failed
iPhone 17 Pro (308FD3CD-F46A-44B9-9B68-9A849F337C80) (Shutdown)
```

**The 43 units in that run were real screen activity, not a retry loop.** That run's first key was 1 KB, a blank screen, so the home screen was still drawing in after boot. I ran `first_key_frame_decodes_to_the_screen_size` twice more to check:

| Run | First key | Units in the next 2 s |
| --- | --- | --- |
| 2 | 59 KB | 2 (both keys, 243 KB) |
| 3 | 59 KB | 1 (a key, 115 KB) |

Both settled runs show `SPS profile_idc 77 level_idc 51`. On a still screen the stream sends only the reset and interval keys; a retry loop would send about 10 per second. `ffmpeg` decodes the first key of the last run to the upright home screen.

### Remaining concerns

- Concerns 1, 3, 5, 6, 7 and 8 above are unchanged.
- Concern 2 is reduced: the stream now declares level 5.1, and ffmpeg decodes it. Browser WebCodecs decode is still unverified.
- An idle stream now wakes its thread every 100 ms (a flag check, no encode). This is the cost of Option A, because the callback may not wake the thread.
