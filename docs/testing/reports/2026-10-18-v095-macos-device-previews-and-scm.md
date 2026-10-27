# Platform Validation Execution Report: v0.9.5 merge into PR #123 on macOS

**Result:** BLOCKED

- **Blocked:** #119 rendering (paint, frames, compositor, stalls) and the #96 heap-snapshot copy count could not be measured. The screen was locked, and Safari Web Inspector is GUI only.
- **Passed:**
  - the build, lint, format and typecheck gates, and the test suites of every package except the server (whose remaining failures are listed under Failed);
  - device previews on both platforms (the Android paste bug was found and fixed);
  - #119 rows, keyboard order, Space staging and Merge Changes;
  - #96 unchanged reconnects.
- **Failed:**
  - five server tests that fail the same way on `main` on this Mac (host-specific);
  - Git signal tests, which fail only in this machine's main checkout (environmental, see below).
- **Not verified:** the #96 memory expectations (inconclusive proxy), VoiceOver order, and Android ⌘V.

This covers PR #123 (device previews, #70) after the v0.9.5 integration branch (PR #126) was merged into it. It also covers the macOS checks still owed from v0.9.5: #119 (Source Control with thousands of changes) and #96 (thread memory across reconnects).

## Tested revision

- Repository: `mubeda/BibCode`. Remote: `origin`.
- Branch: `feat/android-device-previews`, starting at `73cbd34c` (the v0.9.5 merge).
- Merge base with `origin/main`: `62c18c63`. The branch is 257 commits ahead.
- Dirty state before execution: clean. Dirty state after: none beyond the commits listed under "Source changes".

## Native environment

- **Host:** macOS 27.0.1 (26A434), Darwin 27.0.0, arm64 (M2 Max, 96 GiB).
- **Toolchains:** Rust and Cargo 1.98.0 (rustup pin first on `PATH`), Node v26.12.0, vp v0.2.5.
- **Apple tools:** Xcode 27.1 (27A9275).
- **Android:** emulator 37.2.12.0, adb 1.0.41 (platform-tools 37.0.1). AVD `Pixel_8`, API 35, `google_apis` arm64-v8a. Acceleration is Hypervisor.Framework.
- **iOS simulator:** iPhone 17 Pro, iOS 26.5, UDID `308FD3CD-F46A-44B9-9B68-9A849F337C80`. No simulator was booted before the run.
- **Clients:** browser mode with Playwright Chromium on `localhost` against `vp run dev`, and the packaged E2E app `com.bibcode.desktop.e2e` 0.9.5 (WKWebView) driven over its embedded WebDriver.
- **Screen:** the session locked at 17:13:45Z and again from 17:50:51Z (`ioreg` `CGSSessionScreenIsLocked`), and it could not be unlocked. This blocks every measurement that needs rendering (see #119).

## Requested inputs and ancestry

The merge resolutions made on Linux could not be compiled there. Both now compile on macOS:

- `device/ios/mod.rs`: `report_simulator_exit` now takes the start `Instant`.
- `process/registered_helper.rs`: `Exited { started }`.

The live check for the first is in "iOS simulators" below.

## Workspace and static gates

These gates ran at `73cbd34c`, the merge, before any fix. The final code revision, `22c5fba8` (the Android paste fix), was re-checked:

| Command                                                 | Result                        |
| ------------------------------------------------------- | ----------------------------- |
| `cargo fmt --all --check`                               | pass                          |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0                        |
| `vp run typecheck`                                      | exit 0                        |
| `cargo test -p bibcode-server --lib device::`           | 299 passed                    |
| `--test device_rpc --test device_media_socket`          | 1 + 11 passed                 |
| `vp test run apps/web/src/components/device`            | 180 passed                    |
| `BIBCODE_IOS_SIM_TESTS=1 … --test ios_simulator`        | 8/8, no simulator left booted |

The broad package and integration runs below were not repeated at `22c5fba8`. That commit changes only Android input, its tests, a profiler comment and docs.

| Command                                                                                         | Result   | Totals or notes                                                                                                                                        |
| ----------------------------------------------------------------------------------------------- | -------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `cargo check --workspace --all-targets --locked`                                                | exit 0   | 2 min 5 s                                                                                                                                              |
| `cargo fmt --all --check`                                                                       | exit 0   |                                                                                                                                                        |
| `cargo clippy --workspace --all-targets -- -D warnings`                                         | exit 0   | Covers the macOS-only `device/ios`                                                                                                                     |
| `cargo test -p bibcode-server --lib device::`                                                   | exit 0   | 290 passed                                                                                                                                             |
| `cargo test -p bibcode-server --test device_rpc --test device_media_socket`                     | exit 0   | 1 + 11 passed                                                                                                                                          |
| `BIBCODE_IOS_SIM_TESTS=1 cargo test -p bibcode-server --test ios_simulator -- --test-threads=1` | exit 0   | 8/8, real simulator, none left booted                                                                                                                  |
| `vp check`                                                                                      | exit 0   | 2,904 files formatted, no lint findings                                                                                                                |
| `vp run typecheck`                                                                              | exit 0   |                                                                                                                                                        |
| `vp run -r --concurrency-limit 1 test`                                                          | exit 1   | Stops at `bibcode` (server lib): 3,681 passed, 4 failed (host-specific, see below). The packages after it were run one by one.                         |
| `vp run --filter <pkg> test`                                                                    | exit 0   | web 8,685, client-runtime 1,137, contracts 797, shared 830, scripts 750, relay 241. Desktop 490 and oxlint plugin passed inside the graph run.         |
| `cargo test -p bibcode-server --tests --no-fail-fast`                                           | exit 101 | 87 targets OK. Failures: the 4 lib host failures, `preview_gateway` (host), and 3 `production_git_manager_rpc` signal tests (environmental, see below) |

**Host-specific failures on this Mac.** They fail the same way at `main`: `provider_runtime an_exited_provider_process_is_read_without_reaping_it`, three `server_terminal::input_admission_tests`, and `preview_gateway a_refused_upstream_is_retried_once_on_the_other_loopback_family`.

**`production_git_manager_rpc` signal tests.** These are `signal_tracks_a_repository_whose_head_is_a_clone_placeholder`, `git_manager_signal_generation_bumps_after_an_external_commit` and `git_manager_signal_observes_a_sibling_worktree_head_switch`. In the main checkout (`/Users/admin/projects/BibCode`, whose `target/` holds 325 GB), these tests fail with `operation event timeout` (`production_git_manager_rpc.rs:3797`, a 15 s wait for a watcher signal), even on a quiet machine: 1–4 of 68 fail per run.

From fresh worktrees with a separate target directory, the same target passed 68/68 three times on both v0.9.5 (`57b6df16`) and this PR head (`31d8c3c0`), about 8 s per run against about 20 s in the main checkout.

So the failure comes from this machine's main checkout, not from the code. The branch's Git code and this test file are identical to v0.9.5's.

## Device previews evidence

Evidence is in the session scratchpad's `v095-native/` and `screenshots-v095/`. Clients: Chromium (Playwright) at `http://localhost:5733`, 1600×1000.

### Android (Pixel_8, run with `hw.keyboard=yes` and `hw.gpu.enabled=yes`, restored afterwards)

| Scenario                    | Result           | Evidence                                                                    | Message or finding                                                                                                                                      |
| --------------------------- | ---------------- | --------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Start to Ready, live video  | PASS             | `android-home.png`; log `device media stream started width=576 height=1280` | 17.1 s from the click (`ready` `durationMs` 16960). H.264, no still-images badge                                                                        |
| Touch, keys, typing         | PASS             | `android-tap.png`, `android-typing.png`, `android-keys-sheet.png`           | Chrome opens; Back, Home and Recents work                                                                                                               |
| Non-ASCII text refused      | PASS             | `android-refusal.png`                                                       | "This device can type only plain ASCII text. Accented letters, emoji, and other characters can't be sent from here yet." shown for 5.0 s; nothing typed |
| Toolbar paste of ASCII text | FAIL, then fixed | `android-paste-*.png`, `android-len*.png`; fix below                        | Text needing about 32+ key events was dropped whole, with no error                                                                                      |
| Rotation left and right     | PASS             | `android-landscape.png`, `07-android-getevent-landscape-tap.txt`            | `ROTATION_90`, cutout on the left, landscape tap within 1 CSS px                                                                                        |
| Refused rotation (launcher) | PASS             | `android-refused-rotation.png`                                              | "Pixel 8 didn't rotate. The app on screen may lock its orientation, or auto-rotate may be off on the device."                                           |
| Stop and cleanup            | PASS             | `09-android-after-stop.txt`                                                 | No qemu, no adb device, no forwards, discovery directory empty                                                                                          |

**Android paste: root cause and fix.** Two limits were measured live with `getevent` and `logcat`:

1. **Kernel.** The guest's evdev buffer takes 64 events per burst. A text of 32 key events plus their SYNs overflows it: `SYN_DROPPED`, and nothing arrives.
2. **Android's InputDispatcher.** It drops any key queued for more than 10 s ("Dropped event because it is stale"). Chrome's omnibox takes about 10–12 characters a second.

The fix types one character per send, paced at 5 ms, and refuses (all or nothing, before sending) a batch whose text totals more than 100 characters, with "This device can paste up to 100 characters at a time. Paste longer text in parts.". On the final build (`22c5fba8`), in Chrome's address bar: ` PASTED-ok`, ` CCCCCCCCCC` and ` abcdefghijklmnopqrs` arrive exactly, 100 characters arrive exactly (twice, with no stale drops), and 150 are refused with that message, with nothing typed (`android-paste-fixed-*.png`). Measurements while diagnosing: with no pause, ` PASTED-ok` was dropped whole (`SYN_DROPPED`); 300 characters at 1 ms gave 104 arrivals; at 100 ms they arrived exactly but took 36 s; in Settings search at 5 ms, 300 were exact and 495 of 1,000 arrived.

### iOS simulators

- **Xcode:** 27.1. The simulator is listed above, and no other simulator was booted.
- **Mode:** live (DTUHID), not view-only.
- **iOS sim tests:** pass, 8/8.

| Scenario                   | Result         | Evidence                                                                    | Message or finding                                                                      |
| -------------------------- | -------------- | --------------------------------------------------------------------------- | --------------------------------------------------------------------------------------- |
| Start to Ready, live video | PASS           | `ios-portrait.png`; log `H.264 encoder started … width=1206 height=2622`    | 7.9 s from the click                                                                    |
| Tap, Home, Lock, volume    | PASS           | `ios-tap.png`, `ios-keys-sheet.png`                                         |                                                                                         |
| Typing, Paste button       | PASS           | `ios-typing.png`, `ios-paste-ascii.png`, `ios-paste-unicode-1.png`/`-2.png` | `café 😀` goes through the pasteboard, and iOS asks "Allow Paste"                       |
| Rotation left and right    | PASS           | `ios-landscape.png`, `ios-rot-sheet.png`                                    | Island on the expected side; landscape tap lands                                        |
| Refused rotation note      | Not applicable | `ios-rotate-refused.png`                                                    | iOS `rotate` returns the requested orientation by design (pinned by a test), so no note |
| Stop                       | PASS           | `01-ios-after-stop.txt`                                                     | `Shutdown`, no pid, 3.2 s                                                               |
| Ended outside Coddy Studio      | PASS           | `ios-ended-outside.png`; findings JSON                                      | See below                                                                               |

**Simulator shut down outside Coddy Studio** (the merge's `Exited { started }` change). The simulator was booted from the pane, and `xcrun simctl shutdown 308FD3CD-…` ran 90.014 s after the server's `ready`.

- **The pane:** "Failed: Shutdown outside Coddy Studio".
- **The finding:** exactly one `subprocess.crash` for `ios-simulator`: "The command process ios-simulator exited with code 1 after 91 s; Coddy Studio did not stop it.", `uptimeMs` 91375.
- **The uptime:** the measured interval from `ready` to detection was 91375.6 ms, a difference of −0.6 ms.
- **Lifecycle:** `device.lifecycle` recorded `fail` with `failureKind` `exited`.

## Packaged UI and visual evidence: #119 and #96

- **The app:** `target/aarch64-apple-darwin/release/bundle/dmg/Coddy Studio_0.9.5_aarch64.dmg`, from `vp run test:ui:desktop:build`.
  - It was installed by mount → `ditto` → `cmp` → `codesign --verify --deep --strict`.
  - The signature is ad-hoc; the app is not notarized.
- **The driver:** WebDriver (`vp run test:ui:desktop`), with scratch specs outside the repository.
- **Safari Web Inspector** (Layers, Elements, Timelines, heap snapshots) was not available. In-page and process-level proxies stand in for it.

### #119: Source Control with 3,850 changes

The fixture has 312 staged, 50 conflicted (`UU`, merge in progress) and about 3,488 untracked files. Group by folder was on, with every section expanded.

| Step                                                              | Result                        | Measured                                                                                                                                                                                            |
| ----------------------------------------------------------------- | ----------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2. Paints at once; input immediate                                | NOT VERIFIED (screen locked)  | Proxy only: 402 ms from the open click to the first rows in the DOM                                                                                                                                 |
| 3. About 30–40 rows, not thousands                                | PASS                          | 17–26 rows mounted out of 3,952 (300 px viewport, 8 overscan rows); the merge strip mounts 15 of 50; the panel has 526–801 nodes                                                                    |
| 4. No frame over 50 ms while scrolling or during a status refresh | NOT VERIFIED (screen locked)  | A hidden page fires no animation frames                                                                                                                                                             |
| 5. Dozens of layers, not thousands                                | NOT VERIFIED (Inspector only) | Proxy: 0 `will-change`, 0 `transform`, 17 `position: fixed` (about one per mounted checkbox)                                                                                                        |
| 6. No compositor functions in `sample` hot frames                 | NOT VERIFIED (screen locked)  | A hidden page does no compositing                                                                                                                                                                   |
| 7. Tab order, Space toggles staging, VoiceOver order              | PASS / PASS / NOT VERIFIED    | Tab walks checkbox, file, action top to bottom with 0 of 70 stops out of view while scrolling; Space `??`→`A ` in 34 ms (confirmed by `git`); DOM order equals visual order on 25/25 and 15/15 rows |
| 8. Merge Changes on rows reached by scrolling                     | PASS                          | Ours, Theirs and Mark resolved confirmed by `git` (324, 180 and 44 ms); open-in-editor fires `shell.openInEditor` with the conflict path                                                            |
| 9. Panel, section and folder toggles never stall                  | NOT VERIFIED (screen locked)  |                                                                                                                                                                                                     |

### #96: thread memory across reconnects

**Setup.**

- **The thread:** 200 messages (100 shim-answered turns with unique tokens, about 6 MB).
- **The reconnects:** forced by closing the RPC WebSocket from the page. The first run had 5 unchanged and 4 changed; the rerun, used for the memory figures, had 2 unchanged and 8 changed. There was no reload: the same document and WebContent pid throughout.
- **Memory:** WebContent `phys_footprint` (`footprint`/`vmmap`), the median of 3 samples after a 45 s settle.

| Expectation                                       | Result                      | Measured                                                                                                                                                                                                                                                               |
| ------------------------------------------------- | --------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| An unchanged reconnect does not resend the thread | PASS                        | 7/7 answered `unchanged`; 53–69 KB inbound in the first 8 s                                                                                                                                                                                                            |
| No growth of about 7 MB per reconnect             | NOT VERIFIED (inconclusive) | Rerun: 297.1 → 307.3 MB over 10 reconnects, about +1.0 MB per reconnect including 8 added turns. A copy kept per changed resend would add about 48 MB; the 38 MB gap sits within the ±20–40 MB GC noise of endpoint footprints. No evidence of growth, but not a proof |
| About 0.4 MB per reconnect                        | NOT VERIFIED (inconclusive) | The footprint can't tell 0.4 from 1.0 MB per reconnect within its noise; it needs a heap snapshot                                                                                                                                                                      |
| A fixed number of copies of each message          | NOT VERIFIED                | Needs a heap snapshot; the timeline is virtualized, so DOM counts can't show it                                                                                                                                                                                        |

## Process and temporary-root cleanup

- **Devices and servers:** no emulator, qemu or netsimd; no adb device or forward; no booted simulator; no dev server or E2E app of this run.
- **Restored files:** `~/.bibcode/dev/settings.json` and `Pixel_8.avd/config.ini`, matching their backups by hash.
- **Left untouched:** the user's own Coddy Studio (pid 64525), and a Vite on :5740 that this run did not start.
- **Renderer-data isolation deviation.** `~/Library/WebKit/com.bibcode.desktop.e2e` already existed (584 KB). WebKit used it despite the harness's `HOME` and wrote that origin's IndexedDB and LocalStorage to it (now 6,676 KB). The runbook says to stop and get authority first; this run did not. Nothing was deleted. The user's production `com.bibcode.desktop` profile and production store were not touched; the dev store's `settings.json` was changed for the run and restored from its backup.

## Source changes and commits created

- **Docs:** `docs/testing/macos-desktop.md` now says that timing and rendering measurements need an unlocked session and a visible window.
- **Android paste fix:** `22c5fba8`, "fix(server): type Android text a character at a time, paced, and refuse pastes over 100 characters (#70)". It changes `device/android/input.rs`, the device tests, `production/device_rpc.rs`, the profiler `device.input` comment, `docs/architecture/overview.md`, `docs/user/workspace-ui.md` and `docs/testing/cross-platform-validation.md`.
  - **Red evidence:** before the fix, a 40-character text went out as one send, the gap between sends was 0 ns, and the three refusal tests returned `Ok`.
  - **Green evidence:** 299 `device::` tests pass, plus Clippy, `vp run typecheck` and the live check.
- **This report and the runbook note:** added together in one docs commit, apart from the code fix.

## Commands not run

| Command or scenario                                        | Reason                                                   | Follow-up                                                                                   |
| ---------------------------------------------------------- | -------------------------------------------------------- | ------------------------------------------------------------------------------------------- |
| #119 steps 2, 4, 6 and 9 (paint, frames, `sample`, stalls) | Screen locked and could not be unlocked                  | Rerun the packaged #119 spec on an unlocked, visible session (about 3 minutes of rendering) |
| Safari Web Inspector Layers, Timelines and heap snapshots  | GUI only                                                 | Manual Inspector pass                                                                       |
| VoiceOver reading order                                    | Needs an interactive session                             | Manual pass                                                                                 |
| Android ⌘V on the focused screen                           | Not run; it uses the same text path as the toolbar Paste | Covered by the same refusal and pacing                                                      |

## Residual risks

- **#119 render performance:** paint, scroll jank and compositor work are still unmeasured on macOS WebKit. The DOM proxies (virtualized rows, no transformed rows) bound the risk.
- **#96 copy count:** it needs a heap snapshot. The footprint can't separate 0.4 from 1.0 MB per reconnect.
- **Android long pastes:** over 100 characters is refused, not delivered. The follow-up is `setClipboard` plus a paste key, which would also carry Unicode.
- **Duplicate simulator names:** two simulators with the same name appear as identical picker rows ("iPhone 17 · iOS 27.0").
- **Git signal tests:** the `production_git_manager_rpc` signal tests time out in this machine's main checkout but pass from fresh worktrees. The environmental cause (likely file-event pressure around the large checkout) is not isolated further.

## Publication state

- **Commits created by this run:** `22c5fba8` (Android paste), and the docs commit that adds this report and the runbook note.
- **Push target:** `feat/android-device-previews` only.
- **Not done:** no merge, no new pull request, and no artifacts published.
