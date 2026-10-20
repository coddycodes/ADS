# macOS measurements for #119 (Source Control rendering) and #96 (reconnect memory)

**Result:** PASS WITH RESIDUAL RISKS

- **#119:** every render step was measured this time, on an unlocked session with the window visible. No sustained jank. Each scroll pass has one long frame, and opening the panel or restoring the right panel causes one 58–80 ms frame. The Layers count is still unmeasured.
- **#96:** no per-reconnect copy growth across 8 reconnects. Footprint went from 355.0 MB to 354.4 MB, where a copy kept per changed resend would have added at least 36 MB. A Web Inspector heap snapshot at baseline shows about 32 copies of each 60 KB prompt. Snapshots after the reconnects could not be read (see Deviations), so the "fixed number of copies" row stays unverified.

Follow-up to `docs/testing/reports/2026-10-18-v095-macos-device-previews-and-scm.md`, which was blocked by a locked screen. Covers issue #134.

## Tested revision

- **Repository:** `mubeda/BibCode`, branch `claude/ios-macos-android-emulator-9b9e63` (PR #146).
- **App built from:** `b82a616a` (v0.10.1 plus the PR #146 fixes). Commit `8046ec42`, made during this run, changes only test helpers.
- **Dirty state:** clean before and after; the specs and fixtures live outside the repository.

## Native environment

- **Machine:** macOS 27.0.1 (26A434), arm64, Apple M2 Max, 96 GB.
- **Toolchain:** Rust/Cargo 1.98.0 (pinned rustup toolchain on `PATH`), Node 26.12.0, Vite+ 0.2.5.
- **Packaged app:** built with `tauri build --features desktop-e2e --config ./src-tauri/tauri.e2e.conf.json --config '{"identifier":"com.bibcode.desktop.e2e.m134"}' --bundles dmg --target aarch64-apple-darwin`.
  - The `.app` was built. The DMG step (`bundle_dmg.sh`) failed: it scripts Finder, and a pending macOS Automation permission prompt was blocking that.
  - The `.app` was installed by `ditto` into a temporary folder, then checked with `cmp` and `codesign --verify --deep --strict`. It is ad-hoc signed.
- **Driver:** WebDriver (`vp run test:ui:desktop`) with the scratch specs from the 2026-10-18 run.
  - #119 changed in one place: after closing the Source Control tab, the right panel is reopened, because in v0.10 closing the last tab closes the panel.
  - #96 gained `console.takeHeapSnapshot` calls and an Inspector gate.
- **Screen:** unlocked and visible throughout (`CGSSessionScreenIsLocked` absent), with `caffeinate -dimsu` running.

## #119: Source Control with 3,850 changes

Fixture: 312 staged, 50 conflicted (`UU`, merge in progress), and about 3,488 untracked files, with Group by folder on. These are the second full pass's figures; the first pass agreed except for step 2, which failed on a stale selector.

| Step                                                              | Result                     | Measured                                                                                                                                                                                                                                                                                                      |
| ----------------------------------------------------------------- | -------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2. Paints at once; input immediate                                | PASS WITH RESIDUAL RISK    | <ul><li>Click to painted file rows: 368 ms on first open; 52–86 ms on reopen.</li><li>One frame of 58–76 ms follows each open.</li><li>Typing: 20–28 ms from key event to paint, with 0–1 ms input delay.</li></ul>                                                                                           |
| 3. About 30–40 rows, not thousands                                | PASS                       | 17–26 rows mounted out of 3,952; the merge strip mounts 15 of 50                                                                                                                                                                                                                                              |
| 4. No frame over 50 ms while scrolling or during a status refresh | PASS WITH RESIDUAL RISK    | <ul><li>Down in 120 px steps: 1,108 frames, p50 17 ms, p95 19 ms, p99 19 ms. One frame per pass was over 50 ms: 191 ms (frame 37) in this pass; 180, 56 and 130 ms (frames 0, 1 and 4) in the first.</li><li>Up by viewport jumps: worst frame 32 ms.</li><li>Status refresh: worst frame 34–35 ms.</li></ul> |
| 5. Dozens of layers, not thousands                                | NOT VERIFIED               | <ul><li>The Web Inspector Layers tab was not reached.</li><li>Proxy: 0 `will-change`, 0 `transform` and 17 `position: fixed` in the panel.</li></ul>                                                                                                                                                          |
| 6. No compositor functions in `sample` hot frames                 | PASS                       | <ul><li>The top of stack is idle waits (`__psynch_cvwait`, `__workq_kernreturn`), then style matching (`collectMatchingRulesForListSlow`, 36 samples) and JSC.</li><li>`RenderLayerCompositor` appears in stacks (about 550 lines) but never at the top.</li></ul>                                            |
| 7. Tab order, Space toggles staging, VoiceOver order              | PASS / PASS / NOT VERIFIED | <ul><li>Tab order: 277 stops, top to bottom, none out of view.</li><li>Space stages and unstages (confirmed by `git`, 15 and 130 ms).</li><li>DOM order equals visual order, 25/25 and 15/15.</li></ul>                                                                                                       |
| 8. Merge Changes on rows reached by scrolling                     | PASS                       | Ours, Theirs and Mark resolved, each confirmed by `git`                                                                                                                                                                                                                                                       |
| 9. Panel, section and folder toggles never stall                  | PASS WITH RESIDUAL RISK    | <ul><li>Section, folder and Group by folder toggles paint in 18–32 ms, with no frame over 31 ms.</li><li>Restoring the right panel paints in 61–91 ms, with one 58–80 ms frame.</li></ul>                                                                                                                     |

## #96: thread memory across reconnects

- **Setup:**
  - A thread of 200 messages: 100 prompts of 60,000 characters, each with a unique token, answered by the Codex shim.
  - 2 unchanged and 6 changed in-page reconnects, with no reload.
  - Memory is the WebContent `phys_footprint` median of 3 samples, after a 30 s settle.
- **Inspector:** Safari Web Inspector was attached to the app, recording only the Memory and JavaScript Allocations instruments.

| Expectation                                       | Result                      | Measured                                                                                                                                                                                                                                                                                                                           |
| ------------------------------------------------- | --------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| An unchanged reconnect does not resend the thread | PASS                        | Both answered `unchanged`; about 50 KB inbound                                                                                                                                                                                                                                                                                     |
| No growth of about 7 MB per reconnect             | PASS                        | <ul><li>Run 2: 355.0 MB at baseline, 354.4 MB after 8 reconnects, with each changed reconnect resending about 6.2 MB. A copy kept per resend would have added at least 36 MB.</li><li>Run 1 rose 51 MB, but it kept a heavy Inspector recording (10 snapshots) running throughout, so it is not used.</li></ul>                    |
| About 0.4 MB per reconnect                        | NOT VERIFIED (inconclusive) | Run 2 gives −0.1 MB per reconnect, within ±20–40 MB of GC noise                                                                                                                                                                                                                                                                    |
| A fixed number of copies of each message          | NOT VERIFIED                | <ul><li>Baseline snapshot, exported and parsed offline: a 238.96 MB heap with 69,854 strings (196.67 MB). 3,222 strings are about 60 KB each, about 32 copies of each prompt. These totals match the Inspector's own.</li><li>The snapshots after the reconnects were recorded but could not be opened (see Deviations).</li></ul> |

## Deviations

- **The Inspector's snapshot list stays filtered** to the first 15 s of a recording. Clicking a snapshot marker or the ruler didn't widen it, so snapshots taken later in the run could not be opened.
  - In run 1, stopping the recording closed the Inspector window and lost the recording; no crash report was written.
  - A rerun needs one of these: a way to set the timeline range, a snapshot exported right after it's taken, or the spec pausing at each point while a snapshot is taken and exported by hand.
- **Run 2's last two points are excluded** ("final idle" 411 MB, "final idle +120 s"). An extra snapshot taken from the Console at that moment distorted them, and the closing WebDriver screenshot timed out (exit 1).
- **System state changed by the automation.**
  - **Do Not Disturb:** keystrokes into the Inspector Console turned it off.
  - **Safari:** "Show features for web developers" was turned on, with the user's go-ahead.
- **Not acted on:** a Claude network-access prompt for `windows.net`, and a second Automation prompt.

## Cleanup

- **Processes:** no test app, WebDriver or `caffeinate` from this run remains.
- **Left in place:**
  - The temporary install `/private/tmp/bibcode-m134-install.*`.
  - Heap exports in `/private/tmp/m134-heap/`.
  - The E2E WebKit profile `~/Library/WebKit/com.bibcode.desktop.e2e.m134` (7 MB, a new identifier used only for this run).
- **Untouched:** the user's production `com.bibcode.desktop` profile.

## Commands not run

| Item                                         | Reason                                           | Follow-up                                                                                          |
| -------------------------------------------- | ------------------------------------------------ | -------------------------------------------------------------------------------------------------- |
| #119 Layers count (Web Inspector Layers tab) | Not reached in this session                      | Attach the Inspector while the #119 spec runs, and read the Layers tab on the Source Control panel |
| #96 copies after reconnects                  | The snapshot list was filtered to the first 15 s | Rerun with a snapshot exported at each point                                                       |
| VoiceOver reading order                      | Needs an interactive screen reader pass          | Manual pass                                                                                        |

## Residual risks

- **#119:** a single long frame occurs per scroll pass (130–191 ms), and one 58–80 ms frame on panel open or right-panel restore. These are isolated hitches, not sustained jank, but they break the strict "no frame over 50 ms" wording.
- **#96:** the baseline heap holds about 32 copies of each prompt. The constant footprint across 8 reconnects shows the count doesn't grow per reconnect, but whether 32 is the expected fixed number is not established.

## Runbook review

`docs/testing/macos-desktop.md` was reviewed and remains accurate. It already requires an unlocked session and a visible window for render measurements.
