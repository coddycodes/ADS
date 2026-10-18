# Platform Validation Execution Report

**Result:** PASS | PASS WITH RESIDUAL RISKS | BLOCKED | FAIL

Delete the unused result values above. Do not leave an ambiguous status.

## Tested revision

- Repository:
- Remote:
- Branch or requested revision:
- Local HEAD:
- Remote HEAD:
- Merge base and ahead/behind:
- Dirty state before execution:
- Dirty state after execution:

## Native environment

- Operating system and release/build:
- Architecture:
- Kernel:
- Desktop environment/display protocol, when applicable:
- Rust/Cargo:
- Node/package manager/Vite+:
- Native compiler/SDK/runtime dependencies:
- Optional capabilities such as WSL, signing, or notarization:

## Requested inputs and ancestry

- Expected product version:
- Observed version sources:
- Required commits:
- Ancestry result for each commit:
- Inputs that were unavailable:

## Focused validation

| Command | Result/exit code | Duration | Evidence and warnings |
| ------- | ---------------- | -------- | --------------------- |
|         |                  |          |                       |

## VCS observation evidence

- Execution host and route: Native | WSL direct | SSH/server | Unavailable
- Physical repositories/worktrees/active full subscribers/passive subscribers:
- Watcher health and fallback state:
- Automatic-fetch interval and passive-summary interval:

| Scenario                           | Signal source | Git launches after baseline | Publication result | Evidence class |
| ---------------------------------- | ------------- | --------------------------- | ------------------ | -------------- |
| Idle through 59 seconds            |               |                             |                    |                |
| 60-second safety boundary          |               |                             |                    |                |
| Worktree/index/HEAD/refs           |               |                             |                    |                |
| Structured terminal exit           |               |                             |                    |                |
| Overflow/setup unavailable         |               |                             |                    |                |
| Reconnect/hidden/reveal/focus/menu |               |                             |                    |                |

## Pull Requests evidence

- Packaged sidebar route smoke (`pierre-pull-requests-route.png`), native target and exact executable:
- Authenticated read-only list → detail → Files smoke: request, route/`tab=files`, real patch and reload screenshots:
- Unavailable/no-remote fixture evidence (separate from list/detail/files pass):
- Web zero-telemetry regression: image/avatar fixtures, actor initials, idle hour and explicit query/action counts:
- Server source/command tripwires, complete module inventory and checkout deadline exception:
- Listener-bound checks blocked with exact command, test name and bind error (never passed/skipped):
- Separate GitHub and GitLab live-host outcomes; unavailable host/authentication and remaining verification:

- Review surface: comment/edit/delete confirmation, reactions, reply/resolve,
  gutter single/range anchors, pending counter, suggestion single/batch apply,
  dismissal message/confirmation, reviewer re-request:
- Draft navigation/reload/failure retention and loaded-head submission:
- Full/partial/zero-landed receipts, posted-summary handling, failed-body matching, and stale-head Refresh:
- Checkout current/other/new worktree, branch/head, dirty/in-progress/occupied guards:
- Checkout project/repository identity proof, divergent/occupied PR-suffix naming and free matching-branch reuse, catalog owner/sidebar evidence:
- Initial-only worktree loading status in Pull Requests/Git Manager, including empty snapshots and visible catalog errors:
- Checkout pre-write cancellation versus background write completion after disconnect/deadline/admission loss, retained locks/admission, clean tree/no stale index.lock, shutdown drain/bounds and reconnect refresh:
- Checkout Git-failure recovery via git status, current eligibility on toast retry, and explicit Git Manager result navigation:
- Unsupported host and environment mutation denial reasons:

- Project/environment, selected checkout, provider/host, and evidence class:
- Advertised read/mutation capabilities and client setting:
- List/detail route persistence and grouped-row highlighting:
- Server detail/tab RPC shapes and permission/readiness reason inventory:
- Reviewer/version gates, timeline ids/positions, nullable check timing/grouping:
- Review action argv/env/body evidence, private-file mode and cleanup:
- GitLab Note-subtype GlobalIDs, inline suggestion reads and label/milestone resource events:
- Cold/warm per-action process counts, bounded context expiry/Rescan and fresh permission/head refusals:
- Fresh denial/stale-head no-mutation evidence and duplicate-toggle no-ops:
- GitHub atomic / GitLab partial review failures, posted counts and refresh-before-retry:
- New GraphQL documents and separate live-host validation or exact blocked error:
- Diff refs, binary rows, per-file/whole-patch caps, and pagination limits:
- Per-host authentication display and account redaction:
- Tabs/counts, filters, lazy vocabularies, pagination, Refresh, and scroll restoration:
- Detail active-tab reads, cached-open refresh, and authentication recovery:
- Conversation kinds, remote-image links, file anchors, and responsive metadata:
- Real GitHub/GitLab FileDiff shapes, lazy mounting, Viewed persistence and whitespace:
- Merge readiness states/defaults and every disabled control's hover/screen-reader reason:
- Title/description Save, Cancel/Escape, Preview, and reload/failure draft retention:
- Lazy metadata/branch pickers, truncated-search debounce, exact deltas, numeric milestone Clear, and five-second Undo:
- Undo during another action, disabled picker controls until post-Undo detail refresh, correct next-toggle direction, refresh failure/Retry, and late initial vocabulary response:
- Base-change pending-comment warning, cancel/failure preservation, and success-only clearing:
- Merge confirmation host terminology/method/target/deletion/auto/bypass, host-specific recovery and accessibility copy, loaded-head pinning and retained merge-message drafts:
- Read-only merge without a method selector; GitHub auto/bypass exclusion; GitLab current dual permission:
- Auto-merge enabled versus merged result, Disable, Update branch/Rebase and GitLab Skip CI:
- Draft/state/lock actions, host lock reasons, Revert creation/navigation and permanent GitLab Delete/list navigation:
- Partial-progress messages, no automatic write retry, late completion without navigation hijack, and no Base UI console errors:
- Edit/merge/state exact commands, numeric milestone lookup, and full-list people updates:
- Merge head/method/permission rejections, bypass ordering, auto-merge result, and private message transport:
- Revert branch/commit/create sequence, per-step failures and partial progress, GitLab deletion, and new live GraphQL validation:
- Rescan after origin/account changes; absence of stale rows/picker data:
- Unavailable/error messages, copyable auth advice, Retry, and Clear filters:
- Disconnected-environment behavior and no unintended connection:
- Git Manager link checkout scope and creation-dialog cancellation:
- Module DOM/network/idle evidence (no external images or provider polling):
- React best-practice review and UI.md review:
- Blocked commands and exact errors; browser/native scenarios still unverified:

## Git Manager evidence

- Project/environment and selected checkout:
- Environment kind: Local | WSL direct | SSH/server | Other remote | Unavailable
- Advertised Git Manager capabilities:
- Repository shape: ordinary | linked worktrees | unborn | detached | conflicted
- Ref storage coverage: files | reftable | reftable unsupported (record Git version/skip)
- Watch plan (native recursive worktree watch; targeted external metadata stores; object stores filtered) and sibling HEAD/reftable-only invalidation:
- Submodule commit/checkout refreshes the superproject; hundreds of nested ref directories start no registration work or rescan:
- Stash push/pop/drop after idle (empty and existing stash reflog, non-top drop) with Stashes open; closed pane reads no stash list and opening it lists the new entry:
- Windows upper-case metadata path classification:
- Non-blocking wakeups (stalled focus consumer; `application-active` never dropped):
- History retained refresh (one batch per external change, labels equal `git log --decorate`, selection/scroll kept, Retry banner):
- History first-page reads (one read on open; in-app operation while degraded refreshes; one change never causes two reads):
- Remote/local reconciliation stress-loop count and signature-failure publication:
- Idle interval provider/browser request evidence:
- Streaming operation event sequence and cancellation result:
- Interrupted and disconnected inline read against a stalled remote (interrupt exit; transport-helper connection closed):
- Competing catalog/Git Manager mutation and `operation-in-flight` result:

| Scenario                                                                  | Result | Screenshot, command, or log evidence | Findings and unavailable behavior |
| ------------------------------------------------------------------------- | ------ | ------------------------------------ | --------------------------------- |
| Open from the project-header button and route persistence                 |        |                                      |                                   |
| Main checkout and linked-worktree selection                               |        |                                      |                                   |
| Changes, file diff, partial-stage gutter, commit/amend/undo/discard       |        |                                      |                                   |
| History paging, selected commit, and commit diff                          |        |                                      |                                   |
| External refs/stashes at default interval (files and reftable)            |        |                                      |                                   |
| Retained History labels/order equal Git `%D`, including origin/HEAD       |        |                                      |                                   |
| Degraded-focus fallback, healthy/legacy no-op, no alt-tab probes, cleanup |        |                                      |                                   |
| Ref debounce under continuous writes and mutation-fence retry             |        |                                      |                                   |
| Branch create/checkout/rename/delete and occupied-branch redirect         |        |                                      |                                   |
| Fetch/pull/push/publish/force-with-lease states                           |        |                                      |                                   |
| Native stash list, entry diff, apply/pop/drop, and merge preview          |        |                                      |                                   |
| Merge into a non-checked-out branch; Source Control merge entry and Fetch |        |                                      |                                   |
| In-progress and conflicted repository presentation                        |        |                                      |                                   |
| Tag create/delete/push and all four image-diff modes                      |        |                                      |                                   |
| Explicit pull-request/check refresh and no idle provider refresh          |        |                                      |                                   |
| Disconnect/reconnect and one missing-capability degradation               |        |                                      |                                   |
| Slow-link liveness: no disconnect; 4408 within 33 s when frozen           |        |                                      |                                   |
| Slow-link establishment: 16/8/4 KiB/s, notice, and deadlines              |        |                                      |                                   |
| Local-only author identity and no external image source                   |        |                                      |                                   |
| Two-project selection, filter, tab, and repository-data isolation         |        |                                      |                                   |
| Three-project visit with two-entry least-recently-used eviction           |        |                                      |                                   |
| Manual idle third-party Network and rendered-image-source check           |        |                                      |                                   |

## Workspace and static gates

| Command                                                                           | Result/exit code | Duration | Test totals or warning summary |
| --------------------------------------------------------------------------------- | ---------------- | -------- | ------------------------------ |
| `vp run -r --concurrency-limit 1 test`                                            |                  |          |                                |
| `cargo test --workspace -j 2 -- --test-threads=2` or documented native equivalent |                  |          |                                |
| `vp check`                                                                        |                  |          |                                |
| `vp run typecheck`                                                                |                  |          |                                |
| `cargo fmt --all --check`                                                         |                  |          |                                |
| Relevant Clippy with `-D warnings`                                                |                  |          |                                |
| `git diff --check`                                                                |                  |          |                                |

## Native package artifacts

| Kind | Artifact | Absolute path | Version/architecture | Identity/trust verification |
| ---- | -------- | ------------- | -------------------- | --------------------------- |
|      |          |               |                      |                             |

## Standalone server distribution evidence

- Exact staged or installed executable:
- Packaged web discovery result:
- Environment descriptor result:
- Pairing/token-exchange result:
- Shutdown/exit result:
- Checksum result:
- Optional Minisign result or explicitly unsigned state:
- Package install/remove result, when applicable:
- Isolated data sentinel preserved after removal:
- Container image and native architecture, when applicable:

## Packaged UI and visual evidence

| Scenario | Screenshot absolute path | State | Pixel-review finding |
| -------- | ------------------------ | ----- | -------------------- |
|          |                          |       |                      |

- Exact executable launched:
- Exact PID/start identity:
- Other installed or development copies excluded:
- External tool, command, and path used for the Files Refresh rescan:
- Authentication-dependent scenarios unavailable:
- Linux preview child webview geometry, per backend (native Wayland and
  `BIBCODE_GDK_BACKEND=x11`): scale, screenshot after the split drag, and any
  step where the preview did not follow its panel:

## Profiler crash smoke evidence

- Symbol build command and post-bundle check output:
- Executable launched and test-owned `BIBCODE_HOME`:
- Capabilities (`nativeDumps`, `osCrashRecords`, `storage`) and the cause of any
  `unavailable` or `disabled` value:
- Crash monitor process (pid, parent, socket directory and mode) and its exit
  with the profiler and the app:
- Crash trigger and observed process exit (signal or exit code, Windows Error
  Reporting dialog):
- Dump id, `.dmp` size in bytes, and sidecar `processKind`, `parentPid`,
  `bootId`, `version`, `os`, and `arch`:
- Finding after the restart (kind, severity, `crashId` and `crashIds`,
  `faultModule`, attached OS crash record, absence of a duplicate crash
  finding):
- Symbolication command, symbol source (local store or `--download`), crash
  reason, first named frame, and duration:
- Platform manual checks, each pass, fail, or unavailable with evidence (webview
  crash, shutdown, signing and bundle checks, Windows console and MCP stdio):
- Profiler settings and status-bar screenshots:

## External-worktree scenario

- Disposable repository root:
- Git-reported worktrees:
- Physical/path-alias identities:
- Discovery result:
- Adoption/idempotence result:
- Restart result:
- Hide/remove non-destructive result:
- Final on-disk verification:

## Remote-server updates

- Settings/card confirmation, fresh counts, Cancel, and Retry reconfirmation:
- Progress/queued/restart/version checking, closing the view, and row Dismiss:
- Manual archive/package/OS/architecture/SSH steps and Copy:
- Browser explicit Reload and composer input retained:
- Host request notice and Manage devices (native | tests-only | unavailable):
- Fake-host UI/session evidence and light/dark screenshots:
- Real seeded CI run link, target, lane and `widened` result:
- Before/after boot, version, storage identity and project retention:
- Observed percentages/stages, verified backup and requester log counts:
- Linux scoped AppImage mount/runtime counts and cleanup evidence:
- Exact commands, unavailable checks and residual risk:

## Clone from URL network scenario

- Throttled remote size, rate, and clone duration (> 30 s) with dialog close after registration:
- Cancel: notice, destination removal, immediate retry result:
- Stalled transfer: failure time and exact message, destination removal:
- Incomplete-clone refusal message, folder kept, Git Manager "No commits yet":
- Connection drop: outage length, reconnecting line shown, clone duration, registration after reconnect:
- Fixture duration or slow-rate method used to keep the clone running through each event:
- Cancel across a reconnect: Cancelling… line; for each variant (disconnected, reconnected, Cancel then drop), whether the clone was partial ("Clone cancelled.", folder removed) or completed (the finished-before-cancel notice, folder kept and not added, next Clone adds it):
- Dialog closed while reconnecting: close button shown, dialog closed, folder removed after the connection returned, and whether a new Clone of that URL into that folder waited for the cancel, then started:
- Server restart mid-clone: partial or completed at the stop; partial folder removed at shutdown and the exact "No clone is in progress…" message, or the completed clone added:
- Window closed mid-clone: whether the host kept cloning, and the result of re-cloning the same URL into the same folder (joined or added):

## Slow-link liveness scenario

- Server, web, and proxy ports; fixture diff size:
- First config snapshot size in bytes (network panel); size ÷ rate exceeds 15 seconds at the tested rates:
- 16 KiB/s: time to connected, attempt count, setup timeout seen (none expected):
- 8 KiB/s: time to connected, attempt count, setup timeout seen (none expected):
- 4 KiB/s: time to connected or session end, attempt count, deciding bound (buffered delivery or server write deadline):
- Slow-setup text after 5 seconds receiving settings, and whether it cleared on connection:
- Freeze during configuring: seconds from the freeze until the 'No data from' status (at most 33), exact banner text, reconnect after thawing:
- 64 KiB/s: transfer duration, negotiated subprotocol, binary frames seen, disconnects (none expected), slow-request warning text and whether it cleared:
- 256 KiB/s: transfer duration, disconnects (none expected):
- Frozen link: seconds until the 4408 close, exact status text, reconnect after thawing; the next attempt's failure time (at most 15 seconds after it starts) and text:
- Idle freeze: freeze-start and server teardown timestamps; elapsed seconds (at most 50), observed before thaw:
- Transfer freeze: freeze-start and server teardown timestamps; elapsed seconds (at most 33), observed before thaw; subscription cleanup evidence:

### Staged chat attachment upload evidence

- Carrying-session `attachmentStaging`, plain/pinned E2EE transport, source byte count and digest:
- Upstream rate/latency and duration at 64 KiB/s and 16 KiB/s; acknowledged progress samples and maximum outstanding appends:
- Concurrent stream and Stop latency; upload completion without 4408; provider fixture received-byte/digest comparison:
- Multi-file duration over ten minutes, completed-stage get activity touches, non-overlapping keeper requests and joined cleanup on all exits:
- Freeze timestamp, 4408 timestamp (within 33 seconds), resume checkpoint after thaw, earlier-file restart/expiry outcome and once-per-file budget:
- Cancel using pointer, Tab/Enter and Tab/Space on separate uploads; abort count, error-banner absence and usable restored previews:
- New composer edits plus restored outgoing prompt, attachments, terminal/element context, annotations and review comments:
- Queued upload local row before admission, queue acceptance and exactly-once local-row removal:
- Route/unmount during pending admission: no premature cancel, keeper join at outcome, old-draft restoration/new-draft preservation:
- Real unmount/remount before/after ambiguity settlement; unchanged replay, zero restaging, fresh-edit retention and concurrent-view fence:
- Current permission/session rotation on same store; blocked/removal and host/store replacement fences with unresolved intent retained:
- Lost accepted reply: exact command/message/timestamp/upload-ID replay, zero reupload, changed-composer retention; typed not_found and subsequent fresh send:
- Old-server inline fallback, no `uploads.*` probes, failed-admission reuse, and abandoned-stage cleanup:
- Light/dark upload and reconnect/Cancel screenshots:
- Browser queued-data-after-close/Ping-Pong probe; WebKitGTK measured or explicitly not measured:

## High-latency measurement

- Server build profile, round trips and jitter measured:
- Table printed by `scripts/measure-remote-latency.ts` at each round trip:
- Actions that gained RPCs, sequential round trips or echo lag against the previous measurement, with the reason:

## SSH environment evidence

See [Desktop-managed SSH environments](./ssh-environments.md).

- Automated harness command and result (`ssh_environment`, `--ignored`):
- Remote host OS and login shell:
- Authentication methods exercised (key, password):
- Remote `bibcode --version`:
- Devices for this desktop on the host before (count, access):

| #   | Scenario                     | Evidence class | Result | Exact error or message |
| --- | ---------------------------- | -------------- | ------ | ---------------------- |
| 1   | Add                          |                |        |                        |
| 2   | Remote restart, tunnel alive |                |        |                        |
| 3   | Disconnect, then Connect     |                |        |                        |
| 4   | Reload                       |                |        |                        |
| 5   | Desktop restart              |                |        |                        |
| 6   | Dead link (optional)         |                |        |                        |
| 7   | Revocation (optional)        |                |        |                        |
| 8   | Hung pairing (optional)      |                |        |                        |
| 9   | Preview: dev server          |                |        |                        |
| 10  | Preview: Reload/HTTPS/502    |                |        |                        |
| 11  | Preview: forward lifetime    |                |        |                        |
| 12  | Preview: system browser      |                |        |                        |
| 13  | Preview: brainstorming       |                |        |                        |
| 14  | Preview: revocation          |                |        |                        |

- Preview forward `ssh` children and their listen addresses (rows 9–12):
- Devices for this desktop on the host after (count, access):
- Cleanup (environment removed, device revoked, no leftover managed server or
  pairing command):

## Preview gateway evidence

See [Preview gateway](./cross-platform-validation.md#preview-gateway).

- Client (desktop or browser tab), environment address, and evidence class:
- Server bind host and each gateway port with its listen address:
- Gateway cookie attributes (never the value):
- Exact `bibcode-open-url` path, exit codes, and printed output:

| Scenario                         | Result | Screenshot, command, or log evidence | Exact message or finding |
| -------------------------------- | ------ | ------------------------------------ | ------------------------ |
| LAN preview                      |        |                                      |                          |
| Bind and reach (proxied refusal) |        |                                      |                          |
| Browser-mode cross-site boot     |        |                                      |                          |
| Cross-port 403                   |        |                                      |                          |
| Revocation                       |        |                                      |                          |
| Commands that open a browser     |        |                                      |                          |
| Off-screen open-request prompt   |        |                                      |                          |
| Agent open in a browser tab      |        |                                      |                          |

- Pending native checks (Windows console attach, Codex environment and
  `workspace-write` sandbox, 8-hour credential fallback): result or unavailable:

## Process and temporary-root cleanup

- Before snapshot:
- After snapshot:
- Scoped surviving processes:
- New test-owned roots:
- Pre-existing roots/processes intentionally left untouched:
- Package mounts or platform resources released:

## Non-native compatibility evidence

### Platform

- Evidence class: Compatibility evidence | Unavailable evidence
- Source/contracts reviewed:
- Commands and results:
- Native-only evidence still required:

Repeat the subsection for every non-native supported platform.

## Source changes and commits created

- Files changed:
- Behavioral reason:
- RED evidence:
- GREEN evidence:
- Local commits:

## Commands not run

| Command or scenario | Reason | Required follow-up owner |
| ------------------- | ------ | ------------------------ |
|                     |        |                          |

## Residual risks

- Risk:
- Impact:
- Evidence that bounds it:
- Required follow-up:

## Publication state

- Commits created:
- Pushed: yes/no
- Branch merged: yes/no
- Pull request opened: yes/no
- Artifacts published: yes/no
