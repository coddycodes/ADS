# Open File Disk Changes

Status: approved (2026-10-23). Guarded saves with version polling of the
visible file; server push deferred. Implementation needs Pierre's undo boundary
exposed first (see rule 3 under Reconciling a disk version).

## Goal

When a file open in the right-panel editor changes on disk, for example because
an agent edited it, the editor should show the new text, and BiBCode should
never save over a change the user has not seen.

Today the source view keeps the text it first loaded until the file's tab
closes, and the next save writes that text, plus any edits, over the outside
change. When an agent edits a file the user has open and edited, the agent's
work is lost without a trace.

## Current behavior

- The editable surface passes `session.retainedContents ?? file.data.contents`
  to Pierre (`apps/web/src/components/files/FilePreviewPanel.tsx:931`), and
  `retainedContents` stays set after an editor change is saved
  (`apps/web/src/components/files/fileEditingSession.ts:81`). Pierre runs with
  `persistState: true`; once it holds a document for the session's `cacheKey`,
  `Editor.__prepareFile` returns that document's text and ignores new
  `contents` (`@pierre/diffs` 1.3.6, `dist/editor/editor.js:371`). The
  `cacheKey` changes only in `changeOutsideEditor`
  (`fileEditingSession.ts:202`), so a newer file read never reaches the source
  view while the tab stays open.
- The file query is re-read on mount once 30 s stale, after a save, and when
  the connection generation changes; it has no refresh interval
  (`packages/client-runtime/src/state/projectCommands.ts:91`).
- `read_file` returns no version (`apps/server/src/workspace/service.rs:91`), and
  `write_file` is an unconditional in-place `tokio::fs::write` that creates a
  missing file and its parents (`service.rs:133`, `:156`). The server runs up to
  32 workspace operations at once (`service.rs:17`); the client serializes
  writes per path only within one client.
- The server's `WriteInput` does not reject unknown fields
  (`apps/server/src/workspace/rpc.rs:1594`), so an older server given a new
  precondition field ignores it and writes anyway.
- The workspace sweep stats directories every 2 s and ignores in-place content
  edits by design (`apps/server/src/workspace/rpc.rs:88`,
  `docs/plans/2026-08-18-workspace-change-detection-design.md`). The Git watcher
  uses `notify` (already a dependency, `apps/server/Cargo.toml:47`) and does see
  content edits, but only to drive status.
- A failed re-read keeps the previous success's data (`AsyncResult.value`
  returns `previousSuccess`), and the panel shows the read error only when
  `file.data === null` (`FilePreviewPanel.tsx:883`). A file deleted on disk
  keeps showing its old text.
- Every save path writes without knowing what is on disk: the debounced
  autosave, the flush when the user switches right-panel tabs or hides the panel
  (`apps/web/src/components/files/fileEditingSessionRegistry.ts:93`), the settle
  on tab close, and the farewell write on dispose
  (`apps/web/src/components/files/fileSaveCoordinator.ts:72`).
- The rendered Markdown view already shows the file query once the session has
  no unsaved edits (`FileEditingSession.unsavedContents`, in the same release
  as this design). It shares the save-overwrite problem.

## Alternatives

**A. Status quo, documented.** Rejected: silent loss of agent work.

**B. Guarded saves.** Reads return a content version; saves carry the version
they were based on, and the server refuses one whose base is no longer on disk.
Prevents silent overwrites from any writer, including a second BiBCode client.
Alone it reveals an outside change only at save time.

**C. Re-read on cheap triggers.** Re-read the visible file on activation,
window focus (`Atom.swr` supports `revalidateOnFocus` with a `focusSignal`),
reconnect, and `thread.turn-diff-completed` listing it. No server work. Misses
an agent editing mid-turn while the user watches. Turn-diff lists are
checkpoint differences, not authorship, so they are a hint only.

**D. Poll the visible file's version.** A unary `projects.getFileVersion`
returns the hash of the file's current bytes, polled every 2 s while the file
is shown and the window is visible (`document.visibilityState`). A version-sized response, no stream lifecycle, and it reuses the
query runtime's reconnect handling. Costs one request per visible file per tick
and one bounded read and hash on the server.

**E. Server push per file.** A `projects.subscribeFile` stream, stat-polled or
fed by `notify`. Lower traffic than D, but a new stream lifecycle (leases,
resync, subscriber retirement), and `notify` is weakest on network mounts and
translation layers such as Windows paths seen through WSL, so it can only be a
hint.

**F. Poll `readFile`.** Transfers the whole file, up to 1 MB, every tick.
Rejected on cost.

## Decision

**B with D, plus C's triggers.** B is the correctness floor: any detector
miss, from a background tab, an equal-size edit, or an older client, ends in a
conflict the user resolves rather than a lost change. D shows changes as they
happen. C costs nothing and makes returning to the window or finishing a turn
feel immediate. E is deferred until D's request rate is shown to matter; it
would replace D's trigger, not the reconciliation.

What this trades away: latency is bounded below by the 2 s poll, only the
visible file is polled, and the guard cannot be atomic against writers outside
BiBCode (below). Each miss still ends at B. One loss remains: when the editing
registry itself is disposed (switching to another project), a session whose
saves are blocked by a conflict is disposed with its edits, as a session whose
save failed is today. Keeping such buffers past disposal is a follow-up.

## Server

- **Version.** SHA-256 (existing `sha2` dependency) of the exact bytes
  `read_file` returns, as lowercase hex. `ProjectReadFileResult` gains
  `version`. Content, not mtime, is the truth: mtime is coarse on some
  filesystems and can repeat within a second.
- **Version query.** `projects.getFileVersion({ cwd, relativePath })` returns
  `{ _tag: "present", version }`, `{ _tag: "missing" }`, or
  `{ _tag: "unreadable", failure, version }` for a file that became binary or
  too large to edit. It hashes the file and returns no contents. An editable
  file is within the read limit, so its version matches the one `readFile`
  returns; an unreadable one is hashed whole, streamed, so the user can still
  overwrite exactly the state they were shown. That costs a full read per poll
  only while such a file is shown, which needs it to turn binary or past 1 MB
  under an open editor.
- **Guarded write.** A new `projects.writeFileIfUnchanged` with a required
  precondition, `{ _tag: "version", version }` or `{ _tag: "missing" }`, rather
  than an optional field on `writeFile`: an older server would silently ignore
  the field. It refuses with a new `ProjectFileFailure` literal,
  `file_changed_on_disk` (or `file_missing_on_disk` when a save based on an
  existing file finds none), and returns the committed `version` on success.
  The check and the write run under a per-canonical-path lock, so two BiBCode
  clients cannot both pass the check. A writer outside BiBCode can still land
  between the check and the write; that window is milliseconds, it is named
  here rather than hidden, and closing it needs locking other tools would not
  honour. The write stays in place, preserving today's symlink-target behaviour.
- **Capability.** A client uses the guarded write and the version query only
  when the server's `readFile` returned a `version`; the three ship together.
  Without one, the editor behaves as today and claims no protection.

## Client

### Session state

`FileEditingSession` keeps four things apart instead of inferring "dirty" from
revision counters (`fileSaveCoordinator.ts:59`):

- **base**: the disk snapshot the document derives from, `{ version, contents }`;
- **local**: the document's text and revision (the coordinator's latest);
- **saving**: the revision and contents of the save in flight, if any;
- **conflict**: a newer disk state the user has not resolved, if any.

The coordinator gains `adopt(contents)`: set the latest contents and mark them
persisted, with no write. Every save is a guarded write against `base.version`
and, on success, sets `base` to the committed version and the saved contents.

### Reconciling a disk version

A small controller beside the session (not a component) runs D's poll and C's
triggers for the visible session. When a version differs from `base.version`:

1. **A save is in flight or the version predates it.** Skip; the save's result
   settles `base`, and the next poll re-checks. Results are fenced by a request
   generation bumped on every save, never by a time window.
2. **Read the file.** Clear any optimistic overlay for the path first:
   `useProjectFileQuery` prefers the overlay over the query
   (`apps/web/src/components/files/projectFilesQueryState.ts:227`), so a
   lingering one would hide the new read in both views. If the text equals the
   local text (our own save, or the same edit made elsewhere), adopt the
   version as `base`. Nothing else changes.
3. **No unsaved edits, editor attached.** Apply the disk text with
   `Editor.applyEdits`, as disjoint character-level edits (a line diff, then
   each changed line's common prefix and suffix kept), because Pierre moves an
   offset inside a replaced range to that range's end
   (`dist/editor/selection.js:775`); a whole-line replacement would move a
   caret sitting on a changed line. Comment annotations remap and the change
   joins the undo timeline, so Undo brings the old text back as an ordinary
   unsaved edit, provided the reload is its own undo step. Pierre merges
   consecutive same-shape edits into one entry
   (`dist/editor/textDocument.js:98`); `TextDocument` takes an `undoBoundary`
   flag, but `Editor.applyEdits` in 1.3.6 does not pass it (`editor.js:183`).
   The implementation therefore needs that flag exposed, upstream or through a
   dependency patch, and must not ship without it: otherwise Undo after a
   reload also undoes the user's last typing. `applyEdits` calls `onChange`
   synchronously
   (`editor.js:2684`), so the session sets a flag around the call and adopts
   instead of recording a change; the editor change handler still runs. Wait
   for an IME composition to end before applying.
4. **No unsaved edits, editor detached** (another right-panel tab or the
   rendered Markdown view is showing; `applyEdits` throws when detached,
   `editor.js:178`). Clean up the editor and take a new `cacheKey`, as
   `changeOutsideEditor` does, but adopt instead of calling
   `coordinator.change`. The next attach seeds from the query, which holds the
   new text.
5. **Unsaved edits.** Leave the document alone, record the conflict, and block
   every write: debounce, flush, settle, resume, and dispose all refuse while a
   conflict stands. Closing the tab keeps the session in the registry, as a
   failed save does today, so reopening the file shows the edits and the bar.

A missing file follows the same rules with "deleted" as the disk state. An
unreadable one becomes a conflict whose only action is _Overwrite disk_,
guarded by the unreadable state's version.

### The conflict bar

One line under the toolbar, in both views: **"Changed on disk."** (or
**"Deleted on disk."**) with **Reload from disk** and **Overwrite disk**.

- _Reload from disk_ applies the disk text, discarding the local edits. In the
  source view this is rule 3 and Undo restores the edits, so it asks nothing. In
  the rendered view no editor holds history, so it asks: "Discard your unsaved
  edits?" with **Discard edits** and **Cancel** (UI.md: confirm only what cannot
  be undone).
- _Overwrite disk_ saves against the conflict's disk state, the one the user
  saw; if the file changed again meanwhile, the conflict updates instead. For a
  deleted file it recreates the file.
- While the bar shows, Save is disabled with the reason "Changed on disk.
  Choose Reload from disk or Overwrite disk.", and Ctrl/Cmd+S does not write.
  The toolbar's generic "Save failed" retry is not used for a conflict.
- A background tab with a conflict shows it on its tab, so it is visible before
  the user returns.

A clean session whose file is deleted shows an empty state, "Deleted on disk",
instead of the stale text the query keeps. A file renamed outside BiBCode
appears as deleted at its old path; following renames is out of scope.

## Remote environments

The version query runs where the workspace's filesystem lives, as the
directory sweep does, so WSL and SSH workspaces are covered in the same cut.
Hashing every poll avoids depending on mtime fidelity; Windows paths accessed
through WSL still need a real validation run, because their read latency sets
the poll's cost.

## Scope boundary

Not in this design: a Compare view inside the bar (Pierre's diff renderer makes
it the natural next step); polling background tabs; server push (E); following
outside renames; files over the 1 MB read limit, which are already read-only;
and the directory sweep's behaviour.

## Verification

- Server: the version is the hash of the bytes returned; a guarded write with a
  stale version is refused with `file_changed_on_disk` and leaves the file
  untouched; one based on an existing file that is now missing is refused; a
  `missing` precondition recreates; two concurrent guarded writes from the same
  base let exactly one through. `getFileVersion` reports an in-place write that
  bypasses every RPC, deletion, and a file turned binary.
- Client, with real Pierre in happy-dom (the session tests mock Pierre, so they
  cannot show undo or selection behaviour; extend
  `apps/web/src/components/files/pierreEditProvider.integration.test.tsx`): a
  clean attached session reloads without a write, keeps a caret on a changed
  line in place, and Undo restores the old text as unsaved; Reload from disk
  after deleting `c` from `abc` against an empty file, then Undo, yields `ab`,
  not `abc`; a clean detached session gets a new `cacheKey` and no write; a
  dirty session is never changed and every save path is blocked; _Overwrite
  disk_ sends the conflict's version, including for an unreadable file; a poll
  result that arrives while its save is in flight is skipped and the save's
  response settles `base`, with no reload, including while the user keeps
  typing; a tab switch during a conflict does not write; an older server (no
  `version`) keeps today's behaviour.
- Contracts: `check:contracts` regenerates fixtures, `rpcRustParity.test.ts`
  lists the two new methods, and `setProjectFileQueryData` builds a
  `ProjectReadFileResult` by hand and must carry the saved version.
- Profiler: register both methods through the `rpc/session.rs` wrappers; neither
  waits on anything external, so neither is `is_long_running`.
- Gates: focused Rust and web tests, `cargo fmt --all --check`, Clippy with
  warnings denied, `vp check`, `vp run typecheck`, and the UI.md and
  `vercel-react-best-practices` reviews for the bar.
- Living documents to change with the implementation:
  `docs/user/workspace-ui.md` (the editor paragraph under Files, and Current
  limitations), `docs/architecture/rpc-and-orchestration.md` (the two methods
  and failures), and the Files coverage in
  `docs/testing/cross-platform-validation.md` (edit an open file from outside: a
  clean tab reloads, an edited tab shows the bar, a save is refused).
