# Task 4 report: contracts, fixtures, RPC, MCP and web adapted to platforms (Android only)

Status: DONE. Commit `9f533648 feat(contracts): describe devices per platform (#70)`.

## What changed

- `packages/contracts/src/device.ts`: added `DevicePlatform`, `DeviceImage`, and `DevicePlatformCatalog`. Added the five iOS availability kinds. `DeviceSnapshot` now has `platform`, `imageId` and `canRotate` (`avdName` is removed). `DeviceCatalog` is now `{ platforms, devices }` (`availability` and `avds` are removed). `DeviceOpenInput` is now `{ platform, imageId }`. `DeviceBusyError { imageId, message }` is in `DeviceRpcError`. `DeviceLimitError` is documented with `running` as a display name. `DeviceBusyError` follows the file's existing `Schema.TaggedError` pattern. The brief's `TaggedErrorClass` does not match the file's other errors.
- `production/device_rpc.rs`: decodes `{ platform, imageId }` and calls `devices.open(platform, &image_id)`. `Busy` already mapped through `DeviceOpError::to_rpc_error`. The tests use the new payload and assert that the old `{ avdName }` payload is a `Defect`.
- `production/connect_mcp/device_tools.rs`:
  - `device_open` reads `platform`, which defaults to `android`, then `imageId`. `only_avd` became `only_image(platform)`. Its messages name the platform: "This server cannot run iOS simulators.", "Several simulators exist. Pass imageId as one of: …".
  - `device_open` now rejects unknown arguments, including `avdName`, with `DeviceInputError`. Silently ignoring `avdName` could open the platform's only image under the wrong assumption.
  - `device_screenshot` and `device_close` outputs carry `platform`, as spec §2 asks.
  - All four tool descriptions say "Android emulators and iOS simulators (macOS servers)". `device_open`'s schema has `platform` (enum `android`/`ios`, default `android`) and `imageId`.
- `apps/server/tests/device_rpc.rs`: new open payload.
- Fixtures were regenerated: the catalog stream shape changed, and so did the `-00` typed-failure samples for close, createMediaTicket and input.
- Web (shape only; Android behavior unchanged):
  - `DevicePanel` flattens `catalog.platforms` into `{ platform, id, displayName, inUseElsewhere? }` with `catalogImages`. The selection is `{ platform, imageId }`, and it opens with `{ platform, imageId }`.
  - When no platform is ready and nothing runs, `DevicePanel` shows the first platform's own availability title and message.
  - A device limit finds the running device by matching `running`, a display name, against the images' `displayName`. The button reads "Stop <display name>".
  - `DeviceToolbar` takes `images`, `selectedImage` and `onSelectImage(platform, id)`. The Select value is `${platform}:${id}`, resolved by lookup rather than parsing.
  - `deviceMessages`: `runningAvd` became `runningName`. There are titles for the five new kinds, a platform-neutral "Devices ready", and a `DeviceBusyError` case.
  - `DeviceScreen`'s aria-label and the Settings section use `imageId`, which is where `avdName` was used before.
- Docs: `docs/architecture/rpc-and-orchestration.md` (catalog/open rows), `docs/architecture/overview.md` (catalog shape, one device per platform, `DeviceBusyError`, `device_open` arguments).

## TDD

- Contracts. RED: `device.test.ts` failed at import (`DeviceImage` undefined). GREEN: 8/8 after `device.ts`. The new cases cover:
  - a two-platform catalog
  - `inUseElsewhere`, which accepts only `true`
  - `canRotate` and `platform` required on snapshots
  - rejecting the `avds` shape and a platform entry without `platform`
  - `DeviceOpenInput`, which rejects `{ avdName }` and a missing `platform`
  - `DeviceBusyError`, plus the iOS kind `xcodeMissing`

  The decoders are hoisted to module scope.
- Web. RED: 11 DevicePanel tests failed after the fixtures moved to the new shape. GREEN: 848/848. The tests added or changed:
  - the open payload is `{ platform, imageId }`
  - the selection's `data-value` is `android:Pixel_8`
  - `DeviceBusyError` shows the server's message
  - the limit test now uses `running: "Pixel 8"`, a display name, and clicks "Stop Pixel 8". The old id-matching code would fail it.
- Rust. I edited the test payloads and the implementation together, so there was no separate RED run. The tests added are:
  - `device_open_takes_a_platform_and_an_image` (descriptor schema and descriptions)
  - `device_open_defaults_to_android`
  - `device_open_rejects_an_unknown_platform_and_the_old_argument`, which also covers `platform: "ios"` without an iOS backend and leaves the catalog empty
  - non-string `platform` and `imageId`
  - `platform` in the list, screenshot and close outputs
  - `device_rpc`: the old `avdName` payload is now a `Defect`.

## Fixture counts

The generator printed `rpcMethods 158, streamMethods 24, typedFailureFixtures 335, fixtures 449, schemaFingerprints 413`. Those totals did not change, so I changed no count assertion. The brief expected each to go up by one. They did not because each device method's error schema is `Union([DeviceRpcError, EnvironmentRpcError])`, and the generator emits one fixture per top-level member. `DeviceBusyError` sits inside the nested `DeviceRpcError`, so only the `-00` fingerprints and samples changed.

## Gates (all pass)

- `cargo fmt --all --check`: OK.
- `cargo clippy -p bibcode-server --all-targets -- -D warnings`: OK.
- `cargo test -p bibcode-server --lib production::connect_mcp`: 32 passed.
- `cargo test -p bibcode-server --lib device::`: 120 passed.
- `cargo test -p bibcode-server --test device_rpc --test device_media_socket --test rpc_wire`: 1, 11 and 15 passed.
- `vp test run packages/contracts/src/device.test.ts --root packages/contracts`: 8 passed.
- `vp test run apps/web/src/components/device apps/web/src/components/settings`: 48 files, 848 tests passed.
- `vp check`: all files formatted, no warnings or lint errors.
- `vp run typecheck` (Rust PATH set): OK.
- `vp run check:contracts` (Rust PATH set): OK after the commit. Before the commit it failed only its final `git diff --exit-code` fixture step.

## Reviews

- `vercel-react-best-practices`: **not run**, because the skill is not available to this agent. I checked the manual equivalents:
  - `useMemo` on the flattened images keyed by `catalog`
  - a stable `useCallback` for `onSelectImage`
  - no hooks added after the early returns
  - no new effects
- UI.md: this is copy and shape only.
  - The availability titles for the new kinds are short and specific.
  - The `DeviceBusyError` fallback tells the user what to do.
  - Disabled reasons are unchanged.
  - The limit-error button now names the device the same way the server's message does ("Pixel 8").
- Profiler coverage: none applies. No new RPC method, process, removal path, binary, service, or event. The `device.*` methods stay registered through the same wrappers.
- Testing runbooks: no test commands, scripts or flows changed. They were **reviewed and remain accurate**.

## Concerns / follow-ups

- In the panel, the Starting and Stopping text and `DeviceScreen`'s aria-label show `imageId`, as they showed `avdName` before. For iOS that would be a UDID. Task 9 should switch these to display names, along with the grouped picker. Settings → Device previews also shows `imageId`.
- `inUseElsewhere` is carried on the picker images but not rendered (Task 9).
- MCP `device_screenshot` without `deviceId` still needs exactly one live device. With one device per platform, two running devices will need `deviceId`.
- The availability gate uses the first platform when none is ready. Task 9's per-platform headings replace it.
