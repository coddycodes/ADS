# Task 9 report: client grouped picker, per-platform toolbar, view-only notice

Status: DONE_WITH_CONCERNS (see Concerns)
Commit: 43dd1bf8 `feat(web): show iOS simulators in the Devices pane (#70)` (base 5ef723ba)

## What changed

- **Picker** (`DeviceToolbar`): one `SelectGroup` per catalog platform, labelled
  **Android** / **iOS**, in catalog order (the server lists Android first), only the
  platforms present.
  - A platform that is not ready and not view only shows its availability message as a
    disabled item under its heading.
  - `xcodeUnsupported` / `viewOnlyHeadless` list their images, plus the availability
    message as a disabled item, so users know before they start one that it is view only.
  - `inUseElsewhere` images are disabled items with the suffix " — Running outside BiBCode".
    The default selection skips them.
- **Toolbar**: on iOS it shows Rotate left/right, Home, Lock (`power`), Volume down/up and
  Paste, with no Back or Recents. Android keeps Back, Home, Recents and Power, and also
  gains Paste (see Decisions). When `canRotate === false`, both Rotate buttons are disabled
  with "Rotation isn't available for iOS simulators yet".
- **View-only**: when the shown device's platform availability is `xcodeUnsupported` or
  `viewOnlyHeadless`, `DeviceScreen` gets `inputDisabledReason={availability.message}` and
  renders it as one `text-xs` line above the canvas.
  - The canvas then has `cursor: default` and an aria-label of "<name> screen, view only.".
    It ignores pointer, key and paste input.
  - Home, Lock, Volume, Paste and Rotate are disabled with "This simulator is view only".
    This reason takes priority over the canRotate reason.
- **Selection** is `{platform, imageId}` (`selectedImageOf`, `selectShownDevice`, both
  exported and unit-tested).
  - The selected image comes from, in order: the user's pick while the catalog lists it;
    the device this pane started, while it runs; any running device; the device this pane
    started, if it failed; the first free image of a platform that can open.
  - The shown device is the selected image's running device. If there is none, it is the
    device this pane started on that image, when that device failed.
  - "Newest" cannot be read from the catalog, because the server sorts devices by UUID. The
    device this pane started is the only sign of which device is newest.
  - The picker locks only while a start is pending. You can therefore switch between an
    Android device and an iOS device that run at the same time.
  - Start is disabled only when the selected image's platform already runs a device.
  - Picking another image on a platform that already runs a device shows "<name> is
    running. Stop it to start another device." with a **Show <name>** button.
- **Full-pane unavailable state**: shown when no platform can open a device and nothing
  runs. It now lists every platform's title and message instead of only the first
  platform's.
- **Setting**: the description is exactly "Android emulators and, on macOS servers, iOS
  simulators."
  - The toggle title "Device previews (Android emulator)" became "Device previews".
  - The running line, the confirmation dialog and its button use display names and list
    every running device, for example "Pixel 8 and iPhone 17 Pro · iOS 27.0 are running on
    Work Server.". With more than one device running, the button reads "Stop devices and
    turn off".
- **Carry-overs**
  - (a) Stop-on-limit: the pane remembers the platform of the failed start. **Stop <name>**
    stops the running device on that platform, and no longer matches by display name.
  - (b) A test now covers the same imageId on two platforms ("keeps the same image id apart
    on two platforms").
  - (c) "Starting …", "Stopping …", the canvas aria-label and the Settings section now use
    the image's display name.
  - The fallback failure text names "the simulator" on iOS.
- **Task 6 carry-over (media)**:
  - Any `DeviceMediaUnavailableError` now maps to the new still-images reason `noVideo`, and
    carries the server's own message (`TicketUnavailable.message`, then the `VideoMode` jpeg
    `message`, then `StillImages {reason, message}` passed to the toolbar badge).
  - The no-auth server message is the same text as the old `noSignIn` copy. View-only iOS
    shows "Video is off: <reason>." instead of the sign-in advice. The `noSignIn` reason is
    removed.
  - The immediate JPEG fallback with no retries is unchanged (`fallBack` on the first
    failed mint, pinned by the existing tests).
- **iOS input errors**: input errors appear on the existing single `role="status"` line
  (now `data-input-error`). The line re-renders only when its message changes and clears on
  the next accepted batch, so it is not spammy. The server's messages ("not accepting
  keyboard input yet. Try again in a few seconds.", "Touch input is off: …") are shown
  verbatim, because they already say what to do. "iOS has no Back key." cannot happen from
  the UI, because iOS has no Back button and the key map never produces `back`. Rotation
  and input in view-only mode are blocked on the client before the server can refuse them.

## Decisions to review

1. **Paste button (new, on both platforms).** The binding decision lists Paste in the iOS
   toolbar, and spec §7 says "Android keeps … Paste". However, no Paste button existed:
   paste only worked through the canvas `onPaste` handler.
   - I added a Paste button that reads `navigator.clipboard.readText()`, splits the text
     with `splitDeviceText`, and queues text events. I added it to both platforms, for
     consistency (UI.md "Be consistent") and to match spec §7.
   - If the clipboard can't be read (insecure context, permission denied, API missing), the
     status line says "BiBCode can't read the clipboard here. Click the screen and paste
     with Ctrl+V or ⌘V."
   - The brief's "Android is unchanged" conflicts with this. If Android must stay
     button-for-button identical, it is a one-line `ios ? … : null` change.
2. **Paste note (spec §5).** The note appears in two places:
   - The iOS Paste tooltip reads "Paste. iOS may ask to Allow Paste first.".
   - After the first paste into an iOS simulator, from the button or ⌘V, one dismissible
     status line appears: "If iOS asks to Allow Paste, answer it before typing more: keys
     typed meanwhile can arrive before the pasted text." It shows once per simulator
     session and never again after Dismiss.
   - To support this, `DeviceScreen` gained an `onPasted` prop (beyond the brief's
     `inputDisabledReason`), plus `name` for the aria-label (carry-over c).
3. **View-only also disables toolbar input controls.** Spec §7 names only the canvas.
   Leaving Home/Lock/Volume enabled would let users click a control that the server only
   refuses ("Touch input is off: …"), which violates UI.md's rule for disabled states.

## TDD

- RED: `vp test run apps/web/src/components/device apps/web/src/components/settings/DevicePreviewsSection.test.tsx`
  → 33 failed | 95 passed. The failures were the new picker, toolbar, view-only, selection,
  limit, display-name, paste and media-reason tests, and the updated Settings copy.
- GREEN: the same command → 9 files, 128 passed.
- The brief's snippets use Testing Library (`screen`, `userEvent`), which this package does
  not depend on. They were translated into the existing `createRoot`/`act`/`querySelector`
  harness. The `../ui/select` mock gained `SelectGroup` (`role="group"`), `SelectGroupLabel`
  and option items with `aria-disabled` that pick on click. The `platform()`, `image()` and
  `readyIosDevice()` builders were added, and `catalog()` takes `platforms`.

## Gates

- `vp test run apps/web/src/components/device apps/web/src/components/settings` → 48 files,
  874 passed.
- `vp test` (full) → 3 failed | 12523 passed | 55 skipped:
  - The known ChatView failure, "keeps the persisted terminal activity scope pinned…".
  - `scripts/check-dependency-upgrade-ledger.test.ts` and `scripts/rust-workspace.test.ts`.
    Both also fail with this change stashed, at base 5ef723ba. They come from earlier
    server-side dependency and manifest work and are unrelated to `apps/web`.
- `vp check` (re-run on the committed tree) → formatting pass and "Found no warnings or lint errors in 1988 files".
  - A first run reported React Compiler `preserve-manual-memoization` warnings. The cause
    was that `selectedImage` and `device` were plain derived objects that the compiler
    could not treat as frozen. Wrapping both in `useMemo` cleared the warnings.
- `vp run typecheck` (with the rustup 1.98.0 PATH) → no errors. One `exactOptionalPropertyTypes`
  error in the new DeviceScreen test was fixed.
- No Rust changes, so the cargo gates do not apply. No new npm dependencies (only
  `ClipboardPasteIcon` from the existing lucide-react). No schema compiles were added.

## UI.md review

| Area | Finding | Resolution |
| --- | --- | --- |
| Picker grouping | Users think "Android or iPhone" first. Headings match that, and the order is stable. | Groups follow catalog order and appear only for platforms the server lists. |
| Unavailable platform | One missing SDK must not hide the other platform. Users must see why a platform is empty. | The message is a disabled item under its heading. The full-pane state lists every platform, not just the first. |
| View-only platform before start | Starting a simulator and only then learning it is view only is a surprise. | The view-only message is also listed (disabled) under the iOS heading. |
| Disabled items | " — Running outside BiBCode" says why the item is disabled in the item itself. Disabled select items have no tooltip, so the text is the reason. | The suffix is used as given. The default selection skips busy images. |
| Disabled controls | Every disabled control explains itself (title plus sr-only text): canRotate, view only, not booted, stopping, running. | The view-only reason takes priority over canRotate, because it is the true cause. |
| View-only notice | A short line above the canvas uses the server's reason, which already says what users get (screenshots) and why. There is no dialog and no repetition. The cursor does not suggest clicking does anything. | `text-xs text-muted-foreground`, the smallest allowed size. |
| Toolbar differences | iOS users have no Back or Recents, so those buttons are absent rather than disabled. Lock is the iPhone name for the power button. | Done. |
| Paste note | Must not nag and must appear where it matters. | Tooltip on the button, plus one dismissible line after the first paste per simulator. |
| Errors | Clipboard failure says what to do next (⌘V on the screen). iOS input errors pass through verbatim and already carry the action ("Try again in a few seconds"). | One status line that updates only on change. |
| Copy | "Pick a device and press Start." replaces "virtual device" wording in shared places, so it fits iOS too. The Android-only empty state keeps its Android Studio instruction. | Done. |
| Still images | View-only users were told to restart without `--unsafe-no-auth`, which is wrong and not actionable. | The tooltip now shows the server's reason. |

Residual UI notes:

- The full-pane unavailable state stacks one title and message per platform. This is fine
  for two platforms.
- The "<name> is running. Stop it to start another device." state requires switching back
  (**Show <name>**) to stop the device. This is deliberate: Stop always acts on the shown
  device.

## React best-practices review

`vercel-react-best-practices`: **NOT RUN (skill unavailable)**: the skill is not in this
session's skill list. I checked manually instead:

- **Memoization.** The pane is React-Compiler-compiled. `selectedImage` and `device` are
  `useMemo`'d so that the compiler keeps the memoization of `pushInput`, `notePaste` and
  `showInputError`. `DeviceScreen` receives stable callbacks (`pushInput`, `releaseInput`,
  `setStillImages`, `notePaste`), so its effects do not re-run on catalog ticks.
- **Effect deps.**
  - The still-images effect depends on the `reason` and `message` strings, not on a fresh
    object, so it does not loop.
  - The lane-cleanup effect is keyed on `shownDeviceId` and is unchanged.
  - `notePaste` uses a functional `setPasteNote` update, so repeat pastes do not re-render.
- **Render work.** The picker groups are derived per render from the small catalog
  (at most a few dozen images). `findImage` is a linear lookup per name, which is fine at
  that size. Input errors still re-render only when their message changes.
- **Async paste.** `pasteClipboard` checks `mountedRef` after the await.

## Documentation (for Task 10)

Docs belong to Task 10 (`docs/user/workspace-ui.md`, overview). Items to fold in:

- `docs/user/workspace-ui.md:1173` still names the toggle "Device previews (Android
  emulator)". It is now "Device previews", with the new description.
- Describe the Paste button on both platforms, the iOS Allow Paste note, the grouped
  picker, the view-only notice, and the "Running outside BiBCode" items.
- The overview's still-images reasons list: `noSignIn` is replaced by `noVideo`, which
  carries the server's message.

Testing runbooks: the `docs/testing/` runbooks were not modified. Task 10 owns them in the
plan. `cross-platform-validation.md` (~3043–3047, the Still images badge check) needs the
items above, plus checks for the Paste button, the view-only line and the grouped picker.

## Profiler coverage

None applies. The change is client-only UI with no child processes, RPC methods, worktree
removal, binaries, shutdown services or profiler events.

## Files

- /Users/admin/projects/BibCode/apps/web/src/components/device/DevicePanel.tsx
- /Users/admin/projects/BibCode/apps/web/src/components/device/DeviceToolbar.tsx
- /Users/admin/projects/BibCode/apps/web/src/components/device/DeviceScreen.tsx
- /Users/admin/projects/BibCode/apps/web/src/components/device/deviceMessages.ts
- /Users/admin/projects/BibCode/apps/web/src/components/device/useDeviceVideo.ts
- /Users/admin/projects/BibCode/apps/web/src/components/device/DevicePanel.test.tsx
- /Users/admin/projects/BibCode/apps/web/src/components/device/DeviceScreen.test.tsx
- /Users/admin/projects/BibCode/apps/web/src/components/device/useDeviceVideo.test.ts
- /Users/admin/projects/BibCode/apps/web/src/components/settings/DevicePreviewsSection.tsx
- /Users/admin/projects/BibCode/apps/web/src/components/settings/DevicePreviewsSection.test.tsx

## Concerns

1. The Paste button on Android deviates from the brief's "Android is unchanged" and
   follows spec §7 instead (see Decision 1).
2. Selection recency relies on the device this pane started, because the catalog cannot
   say which device is newest.
3. The two failing `scripts/` tests are pre-existing on the branch base and are not caused
   by this change.
4. **Clipboard API across webviews is unverified.** `navigator.clipboard.readText()`
   behaves differently in each host:
   - WKWebView shows a native Paste callout.
   - WebView2 prompts for permission.
   - WebKitGTK may not expose the API at all.
   Failure falls back to the ⌘V/Ctrl+V message, but the success path in the desktop app
   needs Task 10's native validation.
5. **View-only is judged from platform availability, not from the device.**
   - guard.rs flips iOS availability to `xcodeUnsupported` ("Xcode changed while the
     server was running…") when Xcode changes mid-run.
   - A simulator that was already running live keeps live control on the server. The
     client would still show it as view only and block its input until the server
     restarts. That is safe but over-cautious.
   - The opposite case is a device that is view only while its platform reports `ready`.
     It still surfaces the server's "Touch input is off: …" on the status line.
   - A per-device flag on `DeviceSnapshot` would make this exact. That would be a
     contract change, so it is out of scope here.
6. There was no live browser or desktop run against a real simulator. Coverage is limited
   to unit and component tests.

## Fix round 1 (commit 75283da3)

1. **(Important) Started device selected until it appears.**
   - A successful `open` now sets the pick to the started `{platform, imageId}`. Before,
     it cleared the pick.
   - Until the catalog lists the new device, the selection stays on that image. The pane
     shows no screen, the input controls are disabled, and no input reaches the other
     platform's device.
   - The pick is kept after the device appears, instead of being cleared. Once the device
     appears, `selectedImageOf` would return the same image either way.
   - Keeping it also means that if that device later stops, the pane stays on its image
     rather than jumping to another platform's device.
   - Test: "keeps the started image selected until the catalog lists its device". Android
     runs, iOS starts, and the next catalog lacks the simulator. The picker stays on
     `ios:UDID-1`, no screen is shown, Home is disabled and sends nothing. Once the catalog
     lists `i-new`, the pane shows it.
2. **Clipboard race.**
   - `pasteClipboard` records the shown device before awaiting `readText()`. After the
     await, it drops the text if `shownDeviceIdRef` (updated in an effect) differs.
   - Test: "drops clipboard text when the shown device changed while reading it". The
     paste is pending, the simulator stops and the user switches to Android. When the text
     resolves, no input is sent.
3. **Allow Paste note only for pasted text.**
   - New `isUsTypeable` in `deviceKeyMap.ts`: printable ASCII 0x20–0x7E plus `\n`, `\r`
     and `\t`. This matches `us_usage` in `apps/server/src/device/ios/keymap.rs`.
   - The pane wraps input in `sendInput`. It notes the prompt, once per simulator and on
     iOS only, when a text event contains other characters. This covers ⌘V, the Paste
     button and typed characters such as é, which the server also pastes.
   - `DeviceScreen`'s `onPasted` prop is removed. It is no longer needed.
   - Tests: "notes the Allow Paste prompt once, on the first iOS text that is pasted"
     (all-US text gives no note; `héllo` gives the note; after Dismiss, `ü` gives no note)
     and "does not note Allow Paste on Android".
4. **Picker notice is text, not an option.** A platform's availability message is a
   `data-platform-notice` `div` inside the group (`text-xs text-muted-foreground`, no
   disabled opacity). Screen readers no longer announce an unavailable option. The test
   now asserts that the Android group has no option.
5. **Empty platforms.** `UnavailablePlatforms` says "This server can't run device
   previews." when the catalog lists no platforms. Test: "says so when the server lists no
   platforms".

TDD: 5 new or changed tests failed before the fix (125 run, 120 passed), and all pass
after.

Gates:
- `vp test run apps/web/src/components/device apps/web/src/components/settings` → 877
  passed.
- `vp check` → formatting pass and no warnings or lint errors.
- `vp run typecheck` (pinned Rust PATH) → no errors.

63c81d5d (Cargo/ledger) was not touched.
