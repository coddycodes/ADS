# Research — chat view comparison, upstream reference vs BibCode (2026-09-22)

Two read-only inventories were taken on 2026-09-22: the upstream project at `aff9318bf`
and BibCode `main-3` at `4d57551f`. This note records the differences that
matter for future work. "Absent" claims come from negative searches of the
relevant directories unless a file is named; verify before acting on one.
Thread rename, listed as unverified by the BibCode inventory, **is present**
(inline rename in `apps/web/src/components/Sidebar.tsx:531-855`).

## Present upstream, absent or partial in BibCode

Cheap wins on existing infrastructure:

- Ctrl/Cmd+Enter to send, with a `sendShortcut` setting (`enter`,
  `mod-enter`, `mod-enter-multiline`); the upstream project `composer-logic.ts:27-45`.
  BibCode has no modifier send in the chat composer.
- Prompt history recall: ArrowUp on an empty composer walks the current
  thread's sent prompts, derived from thread messages on each keypress;
  the upstream project `components/chat/composerPromptHistory.ts`. BibCode: none.
- Hover timestamps on tool rows and turn folds, not only messages; the upstream project
  `MessagesTimeline.tsx:2307`. BibCode shows them on user and terminal
  assistant rows only.
- Steer shortcut on the head queued card (this plan).

Reliability and performance fixes worth porting:

- Attachment upload retry after socket reconnect, with ownership checks so a
  stale job never deletes a file the draft still owns; the upstream project
  `lib/attachmentUploadQueue.ts`, commit `aff9318bf`.
- Hold the outgoing timeline during a huge-thread switch so the pane never
  blanks; `ChatView.logic.ts:297-384` (`resolveThreadSwitchTimeline`),
  commit `211618fd9`.
- Scroll restore that also saves disclosure state (expanded folds, groups,
  reasoning, per-group inner offsets) in an LRU of 100 threads;
  `components/chat/timelineScrollAnchoring.ts:110-140`, commit `f17165a76`.
  BibCode restores offsets only.
- Streaming markdown: reuse completed prefixes, keep completed code-block
  DOM, resume highlighting from completed lines, never unmount during
  streaming; commits `a9dabbf10`, `8078c532c`, `d7d7f8f3e`, `887ece307`,
  `355fbd96d`. BibCode `ChatMarkdown.tsx` only bypasses the highlight cache
  while streaming.
- Warning from their history: `163d50846` reverts "reuse one row for live
  activity" (`a924fbe08`); do not attempt that optimisation.

Bigger features absent in BibCode:

- Completion notifications: `off | notifications | sound |
  notifications-and-sound`, two bundled sounds, OS notification, dock badge
  through the desktop bridge and a favicon badge in the browser;
  `threadNotifications.ts`, `components/ThreadNotificationCoordinator.tsx`.
- "Edit from here": rewind to a user message that restores the prompt and
  attachments into the composer, with a conversation-only variant
  (`thread.conversation.revert`, `restoreFiles: false`) that keeps file
  changes; `MessagesTimeline.tsx:2267`, `ChatView.tsx:6997-7120`.
  BibCode has "Revert to this message" (files only).
- User-initiated compaction (`/compact`, gated by
  `providerSupportsManualCompaction`) with a compaction timeline row;
  `ContextWindowMeter.logic.ts:15-110`. BibCode displays provider
  compaction but cannot request it.
- Per-turn token and cost readout on assistant messages
  (`AssistantMessageMeta`, `MessagesTimeline.tsx:2424`) and a Usage page.
- Prompt stash with attachments (`promptStashStore.ts`, `Mod+S`), assistant
  text citations into the composer (`AssistantSelectionToolbar.tsx`), PR
  mention chips, timeline minimap with previous/next turn buttons,
  settle/snooze thread states, `Mod+Z` thread undo.

## Present in BibCode, absent upstream

MCP status control in the composer, per-actor subagent stop with subtree
retry, multi-panel chat splits (up to four panes), Git Manager, Pull
Requests module, `$` dollar-skills, structured multi-question user input
with attachments in answers, provider lock on started threads with reasons.

## Absent in both

Regenerate a response, fork a thread from a message, transcript export or
share, thread tags, find-in-thread, voice input on desktop (the upstream project has a
mobile-only controller in `packages/client-runtime/src/voice-input/`).
