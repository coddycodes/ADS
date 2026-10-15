# Handoff: iOS Simulator device previews (issue #70)

Written 2026-10-15 17:15 -03 by the controller session, for whichever session
continues. Read this file, then `progress.md` in this directory. A copy of this folder, with the ledger, the Task 10 brief and every task report, is committed at `docs/superpowers/handoffs/2026-10-15-ios-simulator-previews/`. On a fresh clone, use that copy. The original `.superpowers/sdd/...` folder is git-ignored and exists only on the first Mac (the ledger:
every task, review, fix round and `Ruling:` line).

## Where things stand

- **Repo and branch:** `/Users/admin/projects/BibCode`, branch
  `feat/android-device-previews`. Nothing is pushed. The branch holds both
  Android device previews and the iOS work, on top of `origin/main` v0.9.3,
  which was merged in at `14737d97`.
- **Plan:** `docs/superpowers/plans/2026-10-15-ios-simulator-previews.md`.
  Tasks 6–8 carry "Amended after Task 1" blocks.
- **Spec:**
  `docs/superpowers/specs/2026-10-15-ios-simulator-previews-design.md`. §0
  holds the spike results (private API names). §4 and §5 were amended after
  the spikes, and §5 again for the whole-event paste rule.
- **Execution method:** chosen by the user as subagent-driven
  (`superpowers:subagent-driven-development`). Tasks 1–9 are **complete and
  reviewed clean**; see the ledger for commits.
- **Task 10 was STOPPED part-way** (17:15) for this handoff. It left two
  commits:
  - `1454ed6a` docs: overview, runbooks and changelog. This is part of the
    docs work, and it is unreviewed.
  - A `wip(server)` commit that adds the missing `devicePreviews: true` to
    the environment descriptor in `production/control.rs`, with a test
    assertion. It is untested.

  No `task-10-report.md` exists.

  **Next step: re-dispatch Task 10** with `task-10-brief.md` (it is in this
  handoff folder). Tell the implementer:
  - Verify and finish what `1454ed6a` started, against the brief's docs
    list.
  - Run the `control.rs` test and squash or keep the WIP commit.
  - Add the `device.lifecycle` `platform` field and its test, if it is not
    there yet.
  - Assert `level_idc >= 51` in `tests/ios_simulator.rs`.
  - Run the full validation and the native run on this Mac. The simulator was
    left Shutdown.

## What remains

1. **Task 10 review.**
   - Build the package:
     `bash ~/.claude/plugins/cache/claude-plugins-official/superpowers/6.4.1/skills/subagent-driven-development/scripts/review-package docs/superpowers/plans/2026-10-15-ios-simulator-previews.md 75283da3 HEAD` (Task 10's base is `75283da3`).
   - Dispatch a task reviewer with `task-10-brief.md`, `task-10-report.md`
     and that diff.
   - Run the fix loop if needed (resume or re-dispatch the implementer, then
     a scoped re-review).
   - Ledger it: `Task 10: complete (...)`.
2. **Final whole-branch review** on the most capable model, using the
   skill's `requesting-code-review/code-reviewer.md`.
   - **Package:** `review-package ... b623035b HEAD`. The branch's merge base
     with the old main is `b623035b`. The `origin/main` merge brought in
     unrelated v0.9.3 changes, so either use `git merge-base origin/main HEAD`
     (that is `3b7ca077` or later) as the base, or tell the reviewer to ignore
     the merge's main-side changes.
   - **Scope:** say the review covers both the iOS work and the Android work,
     and that the Android work was already reviewed as "Ready to merge" in an
     earlier session. Point it at the ledger's `minor (deferred)` and
     `Ruling:` lines.
   - **Fixes:** one fix dispatch for all findings, one scoped re-review,
     adjudicate any residuals.
3. **Finish** with `superpowers:finishing-a-development-branch`.
   - Present the report: validation commands and results, "Rulings I made"
     (collect every `Ruling:` line from the ledger, in order, each with its
     cost if wrong), a profiler coverage line, runbooks changed or reviewed,
     React review **NOT RUN** (the skill is unavailable; manual checks
     done), and residual risk.
   - Then ask exactly: merge to main locally / push and create a PR / keep
     as-is. **Do not push or merge without the user's choice.**
   - Delete this workspace only after the final review is clean. Also delete `docs/superpowers/handoffs/2026-10-15-ios-simulator-previews/` in a final commit before merging.

## Rules that bind the next session

- Read `AGENTS.md`. Use the pinned Rust toolchain:
  `export PATH=$HOME/.rustup/toolchains/1.98.0-aarch64-apple-darwin/bin:$PATH`.
  Homebrew cargo shadows it otherwise, and `vp run typecheck` needs it too.
- Commit trailers:
  - `Co-Authored-By: <model> <noreply@anthropic.com>`
  - `Claude-Session: https://claude.ai/code/session_013wRcoKGahv3KuuNuNQn2tV`
  - The new session may use its own session URL.
- PR bodies end with `🤖 Generated with [Claude Code](https://claude.com/claude-code)`.
- Never edit `.repos/` or `.codegraph/`. The controller never fixes code
  itself; it dispatches implementers. Only one implementer at a time.
- **Product spelling in user copy is "BiBCode"** (ruling in the ledger).
- Never leave simulator `308FD3CD-F46A-44B9-9B68-9A849F337C80` booted. Real
  simulator tests are opt-in with `BIBCODE_IOS_SIM_TESTS=1`.

## Known pre-existing failures (not caused by this branch)

- **macOS server lib:**
  - `provider_runtime::tests::an_exited_provider_process_is_read_without_reaping_it`
  - three `server_terminal::input_admission_tests`
- **Web:** ChatView "keeps the persisted terminal activity scope pinned when
  another terminal pane becomes active". It fails on clean `main` too.
- **Flaky under load:** a desktop SSH test and
  `a_provider_process_bibcode_stopped_is_not_reported`. Both pass when run
  alone.

## Open items from before the iOS work (Android branch)

- No `CHANGELOG.md` entry yet. Task 10 adds `[Unreleased]` covering Android
  and iOS.
- `scripts/check-agent-delivery.mjs` has not been run.
- Android native validation needs a Linux host with the SDK and KVM. There
  are 13 NOT CONFIRMED items; see the Android spec and
  `docs/testing/cross-platform-validation.md`.

## Residual risks already known (surface them in the final report)

- **Coverage:**
  - Only the iOS 26.5 runtime was exercised; 27.0 is untested.
  - Headless use (SSH, logged-out console) is NOT CONFIRMED.
  - The Indigo fallback never ran live.
  - Android's −90 rotation vector is unverified on a real SDK. The iOS
    landscape convention follows it.
- **Behavior:** the clipboard read for the Paste button is untested in the
  Tauri webviews unless Task 10's native run covered it.
- **CI:** macOS-only code (`device/ios`) is not built, linted or tested in
  CI, which runs `cargo test` on Linux only.
- **Deferred minors:** see the ledger.
