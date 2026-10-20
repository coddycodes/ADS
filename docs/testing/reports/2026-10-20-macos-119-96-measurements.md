# macOS measurements for #119 (Source Control rendering) and #96 (reconnect memory)

**Result:** FAIL

- **#119 passes with residual risk.** Every step was measured on an unlocked session with the window visible, and the Web Inspector Layers tab counts 81 layers. No sustained jank. Each scroll pass has one long frame, and opening the panel or restoring the right panel causes one 58–80 ms frame.
- **#96 fails, and is improved by a fix made during this run.**
  - Before the fix (run 4), each changed reconnect added about 3 copies of each message. Heap snapshots (JSC runs a full GC before each one) went from 2,985 strings of about 60 KB (30 copies of each prompt) to 4,800 (48 copies) after 8 reconnects, and the heap from 220.17 MB to 335.67 MB. Most of it was the Timeline minimap: each derivation made a full-length copy of every user message, and those arrays accumulated.
  - Commit `1b3e1df8` bounds the minimap preview and keeps the callbacks handed to the list stable. After it (run 6), the same 8 reconnects went from 3,008 to 3,474 strings (+466, against +1,815), and the heap from 222.01 MB to 255.44 MB (+33 MB, against +115 MB). The minimap holds no copies at all.
  - Growth remains: about 0.66 copies of each message per changed reconnect, all in message `text`. The footprint still rose about 31 MB per changed reconnect in run 6, so a follow-up bug is needed.

Follow-up to `docs/testing/reports/2026-10-18-v095-macos-device-previews-and-scm.md`, which was blocked by a locked screen. Covers issue #134.

## Tested revision

- **Repository:** `mubeda/BibCode`, branch `claude/ios-macos-android-emulator-9b9e63` (PR #146).
- **App built from:** `b82a616a` (v0.10.1 plus the PR #146 fixes) for runs 1 to 4 and #119. Commit `8046ec42`, made during this run, changes only test helpers.
- **Run 6 built from:** `1b3e1df8`, the minimap fix, with the same build command.
- **Dirty state:** clean before and after; the specs and fixtures live outside the repository.

## Native environment

- **Machine:** macOS 27.0.1 (26A434), arm64, Apple M2 Max, 96 GB.
- **Toolchain:** Rust/Cargo 1.98.0 (pinned rustup toolchain on `PATH`), Node 26.12.0, Vite+ 0.2.5.
- **Packaged app:** built with `tauri build --features desktop-e2e --config ./src-tauri/tauri.e2e.conf.json --config '{"identifier":"com.bibcode.desktop.e2e.m134"}' --bundles dmg --target aarch64-apple-darwin`.
  - The `.app` was built. The DMG step (`bundle_dmg.sh`) failed: it scripts Finder, and a pending macOS Automation permission prompt was blocking that.
  - The `.app` was installed by `ditto` into a temporary folder, then checked with `cmp` and `codesign --verify --deep --strict`. It is ad-hoc signed.
- **Driver:** WebDriver (`vp run test:ui:desktop`) with the scratch specs from the 2026-10-18 run.
  - #119 changed in one place: after closing the Source Control tab, the right panel is reopened, because in v0.10 closing the last tab closes the panel.
  - #96 gained `console.takeHeapSnapshot` calls, and gates that pause the spec around each snapshot so it can be exported.
  - #119 gained a gate at the end that keeps the Source Control panel open for the Layers tab.
- **Screen:** unlocked and visible throughout (`CGSSessionScreenIsLocked` absent), with `caffeinate -dimsu` running.

## #119: Source Control with 3,850 changes

Fixture: 312 staged, 50 conflicted (`UU`, merge in progress), and about 3,488 untracked files, with Group by folder on. These are the second full pass's figures; the first pass agreed except for step 2, which failed on a stale selector.

| Step                                                              | Result                     | Measured                                                                                                                                                                                                                                                                                                                                  |
| ----------------------------------------------------------------- | -------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2. Paints at once; input immediate                                | PASS WITH RESIDUAL RISK    | <ul><li>Click to painted file rows: 368 ms on first open; 52–86 ms on reopen.</li><li>One frame of 58–76 ms follows each open.</li><li>Typing: 20–28 ms from key event to paint, with 0–1 ms input delay.</li></ul>                                                                                                                       |
| 3. About 30–40 rows, not thousands                                | PASS                       | 17–26 rows mounted out of 3,952; the merge strip mounts 15 of 50                                                                                                                                                                                                                                                                          |
| 4. No frame over 50 ms while scrolling or during a status refresh | PASS WITH RESIDUAL RISK    | <ul><li>Down in 120 px steps: 1,108 frames, p50 17 ms, p95 19 ms, p99 19 ms. One frame per pass was over 50 ms: 191 ms (frame 37) in this pass; 180, 56 and 130 ms (frames 0, 1 and 4) in the first.</li><li>Up by viewport jumps: worst frame 32 ms.</li><li>Status refresh: worst frame 34–35 ms.</li></ul>                             |
| 5. Dozens of layers, not thousands                                | PASS                       | <ul><li>Web Inspector Layers tab: **Layer Count 81**, layer memory 78.54 MB, with the panel scrolled to the middle of 3,952 rows.</li><li>The largest layers are the document and `body::after` (21.60 MB each) and two overflow scrollers (10.53 and 8.47 MB). Each mounted checkbox row adds a small layer (16 B to 7.92 KB).</li></ul> |
| 6. No compositor functions in `sample` hot frames                 | PASS                       | <ul><li>The top of stack is idle waits (`__psynch_cvwait`, `__workq_kernreturn`), then style matching (`collectMatchingRulesForListSlow`, 36 samples) and JSC.</li><li>`RenderLayerCompositor` appears in stacks (about 550 lines) but never at the top.</li></ul>                                                                        |
| 7. Tab order, Space toggles staging, VoiceOver order              | PASS / PASS / NOT VERIFIED | <ul><li>Tab order: 277 stops, top to bottom, none out of view.</li><li>Space stages and unstages (confirmed by `git`, 15 and 130 ms).</li><li>DOM order equals visual order, 25/25 and 15/15.</li></ul>                                                                                                                                   |
| 8. Merge Changes on rows reached by scrolling                     | PASS                       | Ours, Theirs and Mark resolved, each confirmed by `git`                                                                                                                                                                                                                                                                                   |
| 9. Panel, section and folder toggles never stall                  | PASS WITH RESIDUAL RISK    | <ul><li>Section, folder and Group by folder toggles paint in 18–32 ms, with no frame over 31 ms.</li><li>Restoring the right panel paints in 61–91 ms, with one 58–80 ms frame.</li></ul>                                                                                                                                                 |

## #96: thread memory across reconnects

- **Setup:**
  - A thread of 200 messages: 100 prompts of 60,000 characters, each with a unique token, answered by the Codex shim.
  - 2 unchanged and 6 changed in-page reconnects, with no reload. Each changed reconnect resends about 6.2 MB.
  - Memory is the WebContent `phys_footprint` median of 3 samples, after a 30 s settle.
- **Heap snapshots:**
  - Taken with `console.takeHeapSnapshot` in Safari Web Inspector, with the JavaScript Allocations timeline recording only around each snapshot. Recording all the time makes the Inspector snapshot the page every 10 s, which stalled WebDriver in runs 1 and 3.
  - Each snapshot was exported and parsed offline (`analyze.py`). The parser's totals match the Inspector's own.
- **Runs used:** run 4 for the copy counts before the fix, run 3 for the midpoint, and run 6 after the fix.

| Expectation                                       | Result | Measured                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| ------------------------------------------------- | ------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| An unchanged reconnect does not resend the thread | PASS   | Every unchanged reconnect answered `unchanged`; 50–62 KB inbound                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| No growth of about 7 MB per reconnect             | FAIL   | <ul><li>Run 4 footprint, from the idle control to after reconnect 8: 332.5, 356.4, 364.7, 385.2, 397.8, 412.4, 424.6, 442.8, 458.8 MB. That's about 18 MB per changed reconnect.</li><li>Run 4 heap: 220.17 MB at baseline, 335.67 MB after 8 reconnects.</li><li>Run 2 footprint looked flat (355.0 to 354.4 MB), but the heap snapshots override it.</li></ul>                                                                                                                                                                    |
| About 0.4 MB per reconnect                        | FAIL   | About +14 MB of JS heap per reconnect over 8                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| A fixed number of copies of each message          | FAIL   | <ul><li>Strings of about 60 KB:<ul><li>Run 4: 2,985 at baseline (30 copies of each prompt), 4,800 after 8 reconnects (48 copies).</li><li>Run 3: 3,032 at baseline, 3,461 after 4.</li></ul></li><li>By retaining field, run 4 baseline then after 8 reconnects:<ul><li>`userText`: 303, then 1,503. That's +200 per changed reconnect, two more copies of every prompt.</li><li>`text`: 2,780, then 3,238.</li><li>New after the reconnects: 100 Lexical `__text` and 100 `__cachedText`, about one copy each.</li></ul></li></ul> |

**Where the copies come from.**

- `userText` is only set by `deriveTimelineMinimapItems` in `apps/web/src/components/chat/MessagesTimeline.tsx`.
  - Its `compactMinimapPreview` runs `replace(/\s+/g, " ").trim()` over the whole message.
  - So every derivation allocates a fresh full-length copy of every user message, although the minimap only shows a short preview.
- The item arrays are held in module closure scopes, which accumulate across resends.
  - The heap has 17 such arrays after 8 reconnects, holding 1,503 items, against 5 arrays and 303 items at baseline.
  - The minified names (`ge`, `he`) can't be mapped to source without source maps.
- The `text` growth (+458, about 4.6 copies) runs through closure-held arrays of message parts. It wasn't attributed further.

### After the minimap fix (run 6)

Same thread and reconnect sequence, built from `1b3e1df8`, with the Inspector recording only around the two snapshots.

| Expectation                                       | Result | Measured                                                                                                                                                                                                                                                                                                                                             |
| ------------------------------------------------- | ------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| An unchanged reconnect does not resend the thread | PASS   | Both unchanged reconnects answered `unchanged`; 62 KB inbound                                                                                                                                                                                                                                                                                        |
| No growth of about 7 MB per reconnect             | FAIL   | <ul><li>Footprint: 387.6 MB at the idle control, 405.5 and 393.9 MB after the unchanged reconnects, then 420.3, 449.8, 473.5, 507.4, 539.5 and 574.9 MB after the changed ones. That's about 31 MB per changed reconnect.</li><li>After the run it settled to 496.7 MB, and stayed there 120 s later.</li></ul>                                      |
| About 0.4 MB per reconnect                        | FAIL   | JS heap 222.01 MB at baseline, 255.44 MB after 8 reconnects: about +5.6 MB per changed reconnect, against about +19 MB before the fix                                                                                                                                                                                                                |
| A fixed number of copies of each message          | FAIL   | <ul><li>Strings of about 60 KB: 3,008 at baseline (30 copies), 3,474 after 8 reconnects (34.7 copies).</li><li>`userText` (the minimap): 0 at baseline and after, against 303 then 1,503 before the fix.</li><li>`text`: 3,007, then 3,404. Lexical `__text` and `__cachedText`: 99, then 100 each, so the composer holds one steady copy.</li></ul> |

**Where the remaining copies are held.** The 466 new strings are all message `text`. The shortest paths from a root run through the thread store's subscriber list (`subscribe` … `messages`), through React fiber state (`memoizedState` … `messages`), and through `<root>.Internal`. Some paths also run through `onPaste` closures reached from WebDriver's `__wd_el_*` element references on `window`; those are a test artifact and not present in normal use.

## Deviations

- **Inspector handling:**
  - The snapshot list is filtered to the first 15 s of a recording until **Clear Filters** is clicked; it can be clicked while the banner is showing.
  - The labeled snapshots were found with the list's Filter field ("baseline", "changed").
- **Runs 1 to 3 were partial:**
  - Run 1: stopping a long recording closed the Inspector and lost the recording.
  - Run 2: its last two points were distorted by an extra Console snapshot.
  - Run 3: WebDriver stalled after reconnect 4 under the Inspector's 10 s snapshots.
- **Run 4** recorded only around its two snapshots and finished cleanly (exit 0).
- **Run 5** stalled at a snapshot gate after a Safari menu click hung its AppleEvents, and was stopped. **Run 6** replaced it and finished cleanly (exit 0, 2,474 s).
- **The Inspector may have added to the heap,** since it was attached for all of run 4. The growth is in app closures, not Inspector objects, and the footprint grew with it, but a run without an attached Inspector would confirm it.
- **System state changed by the automation.**
  - **Do Not Disturb:** keystrokes into the Inspector Console turned it off.
  - **Safari:** "Show features for web developers" was turned on, with the user's go-ahead.
- **Not acted on:** Claude network-access prompts for `windows.net` and `dc.services.visualstudio.com`, and a second Automation prompt.

## Cleanup

- **Processes:** no test app, WebDriver or `caffeinate` from this run remains.
- **Left in place:**
  - The temporary installs `/private/tmp/bibcode-m134-install.*` and `/private/tmp/bibcode-m134-install2.*`.
  - Heap exports in `/private/tmp/m134-heap/`.
  - The E2E WebKit profile `~/Library/WebKit/com.bibcode.desktop.e2e.m134` (7 MB, a new identifier used only for this run).
- **Untouched:** the user's production `com.bibcode.desktop` profile.

## Commands not run

| Item                              | Reason                                  | Follow-up                                                                   |
| --------------------------------- | --------------------------------------- | --------------------------------------------------------------------------- |
| #96 without an attached Inspector | The copy count needs the Inspector      | A footprint-only run, to confirm the remaining growth without the Inspector |
| VoiceOver reading order           | Needs an interactive screen reader pass | Manual pass                                                                 |

## Residual risks

- **#119:** a single long frame occurs per scroll pass (130–191 ms), and one 58–80 ms frame on panel open or right-panel restore. These are isolated hitches, not sustained jank, but they break the strict "no frame over 50 ms" wording.
- **#96 regression:** before `1b3e1df8`, thread memory grew by about 3 copies of the thread per changed reconnect, mostly the minimap's full-length `userText` copies. That fix removes them. About 0.66 copies per changed reconnect remain, held through the thread store's subscribers and React fiber state, and the footprint still rises about 31 MB per changed reconnect. A bug should be filed for the remaining growth.

## Runbook review

`docs/testing/macos-desktop.md` was reviewed and remains accurate. It already requires an unlocked session and a visible window for render measurements.
