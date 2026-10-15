# SDD ledger — plan: docs/superpowers/plans/2026-10-15-ios-simulator-previews.md
Spec: docs/superpowers/specs/2026-10-15-ios-simulator-previews-design.md
Branch: feat/android-device-previews; plan start HEAD b4b0fbfa. Android ledger is a separate plan (not ours).

## Pre-flight scan
| Pair / task | Produces vs consumes | Finding |
|---|---|---|
| T1→T6/T7/T8 | §0 results (selectors, signatures) → FFI | OK; later tasks take names from spec §0 |
| T2→T3 | backend traits, Arc<dyn RunningDevice> slot → FakeBackend | OK |
| T2→T7 | ErasedVideoSource, device/video.rs VideoUnit → SimulatorVideo | OK; blanket impl + Box impl do not overlap |
| T3→T4 | Rust catalog shape (platforms, imageId), open(platform,id) → wire/TS/RPC | F1: T3 changes Rust wire shape + open() signature, but T4 updates RPC/MCP/TS — T3 alone would not compile or would break server JSON tests |
| T3→T5 | busy refusal | F2: T3 tests Busy as a Failed slot; T5 adds check_available and rewrites that test |
| T4→T9 | TS shapes → UI | OK |
| T5→T6 | XcodeStatus, view-only RunningSimulator → guard + live frames | OK |
| T5→T8 | SimctlRunner::pbcopy → paste | OK |
| T6→T7 | Screen::surface/size/damage → encoder | OK |
| T6 png | png crate present in Cargo.lock (2 versions) | OK — PNG path usable; JPEG fallback only if direct use is problematic |
| T1 self | S1 headless needs a logged-out console | F3: agent cannot log out the GUI; sshd answers on localhost but no known_hosts/keys |
| T2 self | plan keeps public API until T4, yet T3 changes open() | see F1 |
| T3 self | tests vs code | consistent after F2 ruling |
| T5 self | jpeg_size test uses encode_rgb888 | OK |
| T7 self | bitrate numbers | 0.1 bit/px×60fps: 400x800→1.92M→2M; 1000²→6M; 1206x2622→18.97M→12M — consistent |
| T8 self | landscape ratio test | implementer must derive from input::to_device_pixels; reviewer checks the axis rule |
| T9 self | helper names | implementer adds missing helpers |
| T10 self | profiler test hook may not exist | plan gives fallback (rg) |

Ruling: F1 — T3 also updates the Rust callers (device_rpc.rs, MCP device_tools, server JSON expectations) so the server compiles and its tests pass with the new Rust shape; TS contracts, fixtures and web stay for T4 (check:contracts and web may be red between T3 and T4) — why: keeps each task's Rust suite green while the plan's contract/UI split stands — cost if wrong: one intermediate commit where the TS contract disagrees with the server (unshipped branch).
Ruling: F2 — check_available moves into T3: device.open refuses a busy image synchronously with DeviceOpError::Busy and creates no slot; T3's busy test asserts Err(Busy) and no launch; T5 implements it for iOS only — why: spec §8 requires DeviceBusyError from open, and writing a test only to rewrite it is waste — cost if wrong: none beyond T3 scope growing slightly.
Ruling: F3 — T1 runs S1 over `ssh localhost` with a scratch known_hosts (StrictHostKeyChecking=accept-new, UserKnownHostsFile in scratch, never ~/.ssh) only if key auth already works; the logged-out-console case is recorded NOT CONFIRMED and viewOnlyHeadless stays implemented but unreachable — why: logging the user out is outside what an agent may do — cost if wrong: headless Macs may show live mode and fail input at runtime until a native check.
Task 1: dispatched (base b4b0fbfa, implementer opus)
Task 1: implementer DONE_WITH_CONCERNS (commit 26b2af03): S3 works (SimScreen callbacks, BGRA 1206x2622 stride 4864, surface stays portrait), S1 input works via DTUHID (keys only via DTUHID; Indigo keyboard dropped), SSH/logged-out NOT CONFIRMED, S2 rotation works via Purple GSEvent.
Ruling: S1 — all input (touch, buttons, keys) goes through DTUHID (com.apple.coredevice.feature.remote.hid.digitizer over the libxpc *_4sim functions, barrier liveness check with 5 attempts × 4 s timeout × 4 s backoff after boot); legacy Indigo touch and buttons are the fallback when DTUHID never answers; with no DTUHID, keys and text return DeviceInputError ("The simulator is not accepting keyboard input yet. Try again in a few seconds.") — why: the spike shows Indigo keyboard input is dropped on CoreSimulator 1171.7 and idb treats legacy touch as unreliable from 1155.4 — cost if wrong: an extra XPC path to maintain; if DTUHID breaks on a future Xcode, the guard reports view-only.
Ruling: S3/S2 — the server rotates frames into the current UI orientation before encoding (VTPixelRotationSession for H.264, the BGRA→RGB loop for JPEG), so frames swap width/height on rotation exactly like Android and the client stays platform-neutral; orientation comes from SimScreen propertiesChanged uiOrientation — why: the surface stays portrait while the guest draws rotated content; client-side rotation would fork the pane per platform — cost if wrong: one GPU rotation per frame (VTPixelRotationSession is hardware-backed) and an encoder restart per rotation.
Ruling: S2 — rotate sends the Purple GSEvent (PurpleWorkspacePort, msgh_id 0x7B, type 50|0x20000); canRotate is true; the mapping of Orientation landscape/reverseLandscape to Purple 4/3 is fixed by Task 8's real-simulator test against the Android convention (Landscape = rotation vector -90 = top edge to the left) — why: §0 records effects, not names — cost if wrong: left/right swapped until the native test catches it.
Ruling: every Objective-C send to a ROCK proxy goes through objc2's exception catching (`objc2::exception::catch`) and only to selectors listed in §0, gated by conformsToProtocol/respondsToSelector — why: an ObjC exception unwinding through Rust is UB and the proxies raise on unknown selectors — cost if wrong: small per-call overhead.
Task 1: review Needs fixes — 6 Important (doc precision in §0: coordinate claim scope, Purple 2 label, Indigo re-envelope recipe, DTUHID send protocol, iOS 26.5-only caveat, headless overstatement); fix round 1 dispatched (resume, FIX_BASE 21681d22)
Task 1: minor (deferred): idb constants untested (0xBB8), volume step fraction, Purple 112 vs 108 bytes, inferred block signatures, missing encodings, misaligned tables (vp fmt skips the spec)
Task 1: fix round 1 implementer done (commit b83b3a56); re-review dispatched
Task 1: fix round 1/5 (6 addressed, 0 open; commits 21681d22..b83b3a56)
Task 1: minor (deferred): spec §0 long unwrapped lines (formatter skips spec); field-value table values asserted from probe data, not shown in report
Task 1: complete (commits b4b0fbfa..b83b3a56, review clean; spec/plan amended by controller in 21681d22)
Task 2: dispatched (base b83b3a56, implementer opus)
Task 2: implementer DONE_WITH_CONCERNS (37880a7d, b134aeeb): no platform() yet, stop_shared added to trait, Arc 5 s wait may delay a close racing input
Task 2: minor (deferred): stop_shared racing an in-flight batch can reopen input into a closed tracker (send should refuse reopen when tasks.is_closed()) — android/mod.rs:756
Task 2: minor (deferred): launch reads the latest discovered SDK, not the one open validated — android/mod.rs:686
Task 2: minor (deferred): frame orientation now from backend-tracked orientation; a frame can rewrite catalog orientation — frames.rs publish
Task 2: minor (deferred): warn text hardcodes "5 s" instead of STOP_RELEASE_TIMEOUT — mod.rs:3452
Task 2: minor (deferred): LaunchFailure::Busy copy chosen by implementer; Task 3 sets spec copy
Task 2: complete (commits b83b3a56..b134aeeb, review clean)
Task 3: dispatched (base b134aeeb, implementer opus)
Task 3: implementer DONE (012dd036); limit 'running' is image id (UDID on iOS); overview doc + 'busy' failure kind docs deferred to T10
Task 3: review Approved with 1 Important (DeviceLimitError.running holds the image id → UDID on iOS) + minors
Ruling: product spelling in user-facing copy is "BiBCode" (799 uses vs 53) — the spec/plan's "BibCode" in the Busy message, "in use outside BibCode", "Running outside BibCode" and the setting copy become "BiBCode" — why: consistency with every other string — cost if wrong: a later find-and-replace.
Ruling: DeviceLimitError keeps today's Android copy ("<name> is already running. Stop it before starting another device.") rather than spec §8's "Close it first." — why: the pane's button says Stop and Android copy is already reviewed — cost if wrong: one string.
Task 3: minor (deferred): open on a platform with no backend returns UnknownImage (should say the server cannot run iOS simulators) — mod.rs open
Task 3: minor (deferred): no debug_assert for one backend per platform
Task 3: minor (deferred): the_catalog_starts_with_each_platform_looking relies on the current-thread runtime
Task 3: fix round 1 dispatched (resume; FIX_BASE 012dd036): Limit.running = display name; BiBCode spelling; check_available error wording; unlocked limit pre-check
Task 3: fix round 1 implementer done (42c521fd); re-review dispatched
Task 3: fix round 1/5 (4 addressed, 0 open; commits 012dd036..42c521fd)
Task 3: minor (deferred): no test for the non-Busy check_available branch; no test for two opens racing past the pre-check
Task 3: complete (commits b134aeeb..42c521fd, review clean)
Task 4: dispatched (base 42c521fd, implementer opus)
Task 4: implementer DONE (9f533648); fixture totals unchanged (BusyError inside union); MCP device_open rejects unknown args; raw image id still shown in some pane text → Task 9
Task 4: ⚠️ Busy rpc_error JSON — resolved by controller: Task 3's busy_and_unknown_image_errors_name_the_platform pins the exact JSON
Task 4: minor (deferred → Task 9): Stop-on-limit button finds the device by display name; add a same-imageId-on-two-platforms test; pane text/aria/settings still show imageId (UDID on iOS)
Task 4: minor (deferred): overview.md device_open sentence is one long line; MCP screenshot/close results now carry platform (unreported)
Task 4: complete (commits 42c521fd..9f533648, review clean)
Task 5: dispatched (base 9f533648, implementer opus)
Task 5: implementer DONE (1bd756e9); tests not strictly test-first (mutation-checked); Task 6 follow-ups: open refuses non-Ready availability (view-only must open), refuse media tickets for view-only, cache xcode check, replace temp input reason; full-size 2fps screenshots; exit code 1/pid 0 in report_exit
Task 5: minor (deferred → Task 6): no test for cancel/bootstatus-timeout after boot; run_error Debug formatting in user text (simctl.rs:1482); PNG error says "emulator" for iOS; video() warns per attach; xcode probe per refresh (uncached); open refuses non-Ready availability
Task 5: minor (deferred → Task 10 docs): report_exit uses synthetic exit 1 / pid 0 for an outside shutdown — document in profiler references
Task 5: minor (deferred): tiny race — shutdown+reboot by someone else between 2 s polls lets BiBCode's stop shut down their boot
Task 5: complete (commits 9f533648..1bd756e9, review clean)
Task 6: dispatched (base 1bd756e9, implementer opus)
Task 6: implementer DONE_WITH_CONCERNS (4872a3f4, 991f5b55): live JPEG + rotation work on 26.5; fixed simctl screenshot (no stdout on Xcode 27) and close-before-Stopped; signed blocks needed (spec §0 correction); damage is a watch; read_rgb replaces read_bgra; Task 9: MediaUnavailable reason shown as no-sign-in
Task 6: review Needs fixes — 1 Important (IoSurface::retain outside guarded on callback-provided objects; wrong SAFETY claim) + minors
Task 6: fix round 1 dispatched (resume; FIX_BASE 991f5b55): Important #1 + minors #2 (Xcode switch: no second dlopen, ViewOnly + restart log), #3 (temp screenshot file guard), #4 (unregister after partial registration), #5 (gate/guard elements()/UDID sends)
Task 6: minor (deferred): Drop for Screen can make a sync XPC call on a Tokio worker if dropped without stop; live_frames pacing has no hermetic test; ios_host_manager_for_tests is pub doc(hidden) (cfg-gate it); no IOSurface seed check (tearing possible)
Task 6: fix round 1 implementer done (b9676a7c); re-review dispatched
Task 6: fix round 1/5 (5 addressed, 0 open; commits 991f5b55..b9676a7c)
Task 6: minor (deferred): GuardCache marks loaded only after Live — a ViewOnly probe may still have loaded SimulatorKit, then a different Xcode loads a second copy; RemovedOnDrop uses blocking unlink on async path (negligible)
Task 6: complete (commits 1bd756e9..b9676a7c, review clean)
Task 7: dispatched (base b9676a7c, implementer opus)
Task 7: implementer DONE_WITH_CONCERNS (5d3056e8): portrait frames zero-copy (tearing), H.264 level 5.0 below spec for 1206x2622@60, 60 fps unmeasured, VT unit tests macOS-only, two doc(hidden) pub test hooks
Task 7: review Needs fixes — Important: (1) encoder failure/panic reported as a clean end (sender drops before failed is stored); (2) after an overflow or failed encode on a still screen, viewers keep a stale picture
Ruling: Task 7 #2 uses Option A — while force_key is pending the encoder thread re-encodes the current surface after a short timeout (≈100 ms), instead of having the VT callback call Notify — why: keeps the callback contract (try_send only) — cost if wrong: one extra wake per 100 ms only while recovering.
Task 7: fix round 1 dispatched (resume; FIX_BASE 5d3056e8): #1, #2 (Option A), #3 ExpectedFrameRate=60, #5 zero-length NAL, #6 empty sample check, #10 16,667 µs cap
Task 7: minor (deferred): two copies per sample + SPS/PPS read per delta; doc(hidden) pub test hooks (media_frames_for_tests, sps_dimensions); VT tests fail rather than skip without an H.264 encoder; zero-copy portrait may tear (rotated path too, shorter window); macOS-only code is not linted/tested in CI (Linux only)
Task 7: fix round 1 implementer done (77157c3b; SPS now level 5.1); re-review dispatched
Task 7: fix round 1/5 (6 addressed, 0 open; commits 5d3056e8..77157c3b)
Task 7: minor (deferred): hub stall makes forced IDRs repeat every 100 ms (bounded ~10 Hz); SPS level print not asserted (assert level_idc >= 51); idle 100 ms wake could be narrowed
Task 7: complete (commits b9676a7c..77157c3b, review clean)
Task 8: dispatched (base 77157c3b, implementer opus)
Task 8: implementer DONE_WITH_CONCERNS (f2d39740, 4ae10b0c): landscape fixed to uiOrientation 4 / Purple 3; Allow Paste prompt reorders keys after a paste; Indigo fallback never exercised live; Android -90 unverified natively
Task 8: review Needs fixes — Important: (1) a refused/failed batch commits pointer state (next tap dropped); (2) several paste runs in one text event can overwrite the pasteboard before iOS reads it; "Allow Paste" reorders later keys
Ruling: Task 8 #2 — a text event containing any character that is not US-typeable is sent whole as ONE paste (pbcopy of the full event text, then Cmd+V); all-US text is still typed as keys; mixed_text_is_split_in_order becomes mixed_text_is_pasted_whole; spec §5 amended to match; the remaining cross-event reordering while "Allow Paste" is up is documented (pane note + workspace-ui, Task 9/10) — why: one pasteboard write per event removes the overwrite race and in-event reordering; the per-run split bought nothing users need — cost if wrong: an all-ASCII-but-one-emoji message pastes rather than types (same final text).
Task 8: fix round 1 dispatched (resume; FIX_BASE 4ae10b0c): #1, #2 per ruling, minor: path() returns Released early after release; check stopping before pbcopy
Task 8: minor (deferred): dead DTUHID mid-gesture logs warn per move; stop test doesn't exercise the release race; legacy builder leaks on exception (comment); text real-sim check weak (pixel diff only)
Task 8: fix round 1 implementer done (f7c0ea20); re-review dispatched
Task 8: fix round 1/5 (4 addressed, 1 new Important — mid-batch Move/Up send failure commits the planned end state while the guest may still hold a touch; commits 3902cd28..f7c0ea20)
Task 8: fix round 2 dispatched (resume; FIX_BASE f7c0ea20): on any send failure, best-effort lift a possibly-held touch and reset pointer state
Task 8: fix round 2 implementer done (5ef723ba); re-review dispatched
Task 8: fix round 2/5 (1 addressed, 0 open; commits f7c0ea20..5ef723ba)
Task 8: minor (deferred): failed Down still triggers a best-effort Up (harmless); same-path Up relies on sink behavior (untested); no test for pbcopy failure after a held Down
Task 8: complete (commits 77157c3b..5ef723ba, review clean)
Task 9: dispatched (base 5ef723ba, implementer opus)
Task 9: implementer DONE_WITH_CONCERNS (43dd1bf8): Paste button added on both platforms (spec §7 lists it for Android; brief said Android unchanged); clipboard API untested in desktop webviews; view-only per platform not per device; React review NOT RUN (skill unavailable, manual check)
Cross-task regression (found during Task 9): Tasks 6/7 added macOS crates directly in apps/server/Cargo.toml instead of [workspace.dependencies] + `workspace = true` (scripts/rust-workspace.test.ts fails), and scripts/check-dependency-upgrade-ledger.test.ts still expects rustRegistry 131 (now 141) — fix dispatched as a separate small task (sonnet), parallel to Task 9 review (disjoint files)
Task 9: review Needs fixes — Important: picker clears the pick on a successful open before the catalog shows the new device, so with two platforms running the pane can briefly show/target the other device
Ruling: Task 9 keeps the Paste button on Android — spec §7 lists Paste for Android; brief's "Android unchanged" yields to the spec — cost if wrong: one button to remove.
Task 9: fix round 1 queued (after the workspace-deps fix commits, to avoid concurrent commits): #1 + minors #2 (stale pushInput after clipboard await), #3 (Allow Paste note only when a paste goes through the pasteboard), #5 (notice contrast), #6 (empty platforms fallback)
Task 9: minor (deferred): view-only test clicks an already-disabled button; tooltip check via title attr; clipboard message on dismissed prompt; view-only per platform not per device (needs contract change — follow-up)
Cross-task regression fix: 63c81d5d (workspace deps centralized; rustRegistry 138; objc2-foundation now default features in server; shared pins tightened for desktop)
Task 9: fix round 1 implementer done (75283da3); re-review dispatched
Task 9: fix round 1/5 (5 addressed, 0 open; commits 63c81d5d..75283da3)
Task 9: minor (deferred): client US-typeable regex mirrors server keymap by comment only (shared constant / cross-package test); shownDeviceIdRef lags one commit
Task 9: complete (commits 5ef723ba..75283da3 incl. regression fix 63c81d5d, review clean)
Task 10: dispatched (base 75283da3, implementer opus)
