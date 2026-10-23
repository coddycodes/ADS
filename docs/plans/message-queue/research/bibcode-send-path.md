# Research — BibCode composer and send path (2026-09-22)

Worktree `main-3` at `4d57551f`. Line numbers drift; re-verify before editing.

## Components

- `apps/web/src/components/chat/ChatComposer.tsx` (~2800 lines): form,
  local composer state, keyboard handling, attachments. Rendered by
  `apps/web/src/components/ChatView.tsx` (~6300 lines), which owns send,
  interrupt and delivery resolution.
- Supporting: `ComposerPromptEditor.tsx` (Lexical, plain-text mode with
  decorator nodes), `ComposerPrimaryActions.tsx`, `composer-logic.ts`,
  `composerDraftStore.ts`, `ChatView.logic.ts`.

## Submit path

1. `ChatComposer.tsx:1878-1881` — `Enter && !shiftKey` → `submitComposer()`
   (`:1789-1822`), which intercepts standalone `:` actions and otherwise calls
   the `onSend` prop. Shift+Tab toggles interaction mode (`:1856-1860`).
   IME guard in `ComposerPromptEditor.tsx:1095`.
2. `ChatView.tsx:4742-4752` `onSend` guard: `!activeThread || isSendBusy ||
   isConnecting || activeEnvironmentUnavailable || workspaceUnavailable !==
   null || providerBinding.conflict !== null || sendInFlightRef.current`.
   **`phase === "running"` is not in the guard.**
3. Payload built from `composerRef.current.getSendContext()`
   (`ChatComposer.tsx:483-496`, `:2162-2175`) → `deriveComposerSendState`
   (`ChatView.logic.ts:333-364`) → text augmentation helpers → attachments
   to data URLs.
4. `startThreadTurn(...)` at `ChatView.tsx:5052-5071` through the atom command
   `threadEnvironment.startTurn` (`packages/client-runtime/src/state/threadCommands.ts:106-111`,
   `operations/commands.ts:190-200`). Optimistic user row via
   `setOptimisticUserMessages` (`:4915-4929`). Draft cleared at `:4944-4946`
   and restored on failure at `:5083-5116`.

## Busy state

`useLocalDispatchState` (`ChatView.tsx:951-1040`):

- `activeLocalDispatch` is the optimistic "sent, not yet acknowledged"
  snapshot; `hasServerAcknowledgedLocalDispatch` (`ChatView.logic.ts:563-617`)
  clears it once the server's turn or delivery state moved.
- `cancellableDelivery = findLastCancellableDeliveryMessage(messages)`
  (`ChatView.logic.ts:514-522`): last message whose delivery is `pending` or
  `sending`.
- `isSendBusy = activeLocalDispatch !== null || cancellableDelivery !== null`.
- `isSendActivelyWorking = activeLocalDispatch !== null || activeDelivery !== null`.

Consequence: after the provider accepted the first prompt (delivery
`delivered`), `isSendBusy` is false while the turn still runs, so Enter
submits a second `thread.turn.start`. The server delivers it as soon as the
provider accepts it (see `bibcode-turn-lifecycle.md`). **BibCode already
performs an unlabelled mid-turn steer today.**

## Stop button

`ComposerPrimaryActions.tsx:126-152`: rendered when `isRunning ||
canCancelPendingSend`; label `"Stop generation"` vs `"Cancel queued message"`
(`:127`). Handler → `onInterrupt` → `ChatView.tsx:5132-5165`:
`resolveTurnDelivery(dismiss)` when not running, else
`interruptThreadTurn(buildThreadTurnInterruptInput(activeThread))`. The editor
is not disabled while running (`ChatComposer.tsx:2653-2658`).

## Phase and timers

`phase = derivePhase(session)` (`ChatView.tsx:2429`; `session-logic.ts:1364-1376`)
maps `session.status` to `disconnected | connecting | ready | running`.
`isWorking` (`ChatView.tsx:2522-2523`). Timer source `activeWorkStartedAt`
(`:2524-2528`). Sidebar "Working" pill from `session.status === "running"`
(`Sidebar.logic.ts:468-474`).

## Drafts

`apps/web/src/composerDraftStore.ts` — zustand + persist, key
`bibcode:composer-drafts:v1` (`:61`), debounced localStorage. Per-target
drafts keyed by `composerTargetKey` (`:1242`). `ComposerThreadDraftState`
(`:261-289`) lists the exact fields a queued item snapshots: prompt,
attachments, terminal/element contexts, preview annotations, review
comments, model selection per provider, active provider, runtime mode,
interaction mode.

## Timeline

`MessagesTimeline.tsx` uses LegendList (`:25`). Rows from
`deriveMessagesTimelineRows` (`MessagesTimeline.logic.ts:378-559`), kinds
`work | work-toggle | turn-fold | message | proposed-plan | working`
(`:97-151`). The `working` row is pushed last (`:551-556`). User rows:
`UserTimelineRow` (`:846`), card style `:885`, delivery notice at `:959-965`
(`TurnDeliveryNotice.tsx`, renders only `uncertain | failed`). Message
delivery contract `TurnDeliveryState` (`packages/contracts/src/orchestration.ts:290-306`).

## Tests and e2e

Vitest: `chat/ChatComposer.test.tsx`, `ComposerPrimaryActions.behavior.test.tsx`,
`MessagesTimeline.logic.test.ts`, `ChatView.test.tsx`, `ChatView.hooks.test.tsx`,
`ChatView.logic.test.ts`, `composerDraftStore.test.ts`,
`packages/contracts/src/orchestration.test.ts:212-1005`.
**End to end is WebdriverIO + mocha** (`apps/desktop/e2e/wdio.conf.ts`),
spec `apps/desktop/e2e/specs/composer-native-triggers.e2e.ts`, XPath on data
attributes (`data-chat-composer-form="true"`), helpers in
`apps/desktop/e2e/support/` (`provider-input-log.ts` records what the stub
provider received).

## Keybindings

`packages/contracts/src/keybindings.ts` (command list), defaults documented
in `docs/user/keybindings.md`, validated server-side
(`apps/server/src/production/keybindings.rs`). No Mod+Enter binding exists in
the chat composer today.
