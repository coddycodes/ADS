/**
 * Adopts center panels other clients opened. Panel layout is per client, so
 * each client adds the AI terminals and chat panels the server reports for a
 * host thread when it first sees them, as inactive tabs. A later local close
 * is not undone by status updates, because adoption happens on first sight.
 * Only an explicit server `remove` drops a terminal tab; a reconnect or server
 * restart snapshot never does, so terminals can relaunch from their tab. A
 * tracked chat panel that leaves the live thread list (closed, so archived, or
 * deleted elsewhere) loses its tab, and is adopted again if it is reopened.
 */
import { scopedThreadKey, scopeThreadRef } from "@coddy/client-runtime/environment";
import {
  ThreadId,
  type EnvironmentId,
  type OrchestrationThreadShell,
  type TerminalMetadataStreamEvent,
  type TerminalSummary,
} from "@coddy/contracts";

import { useCenterPanelStore } from "./centerPanelStore";
import { isTerminalIdReserved } from "./terminalIdReservations";

export interface CenterPanelAdoptionTracker {
  readonly terminalKeys: Set<string>;
  readonly panelThreadIds: Set<string>;
}

export function createCenterPanelAdoptionTracker(): CenterPanelAdoptionTracker {
  return { terminalKeys: new Set(), panelThreadIds: new Set() };
}

const PANEL_TITLE_PREFIX = "Panel — ";

/** The provider label a chat panel thread was titled with, if it has one. */
export function panelProviderLabel(title: string): string | undefined {
  return title.startsWith(PANEL_TITLE_PREFIX) ? title.slice(PANEL_TITLE_PREFIX.length) : undefined;
}

const terminalKey = (threadId: string, terminalId: string) => `${threadId}\u0000${terminalId}`;

function adoptTerminal(
  environmentId: EnvironmentId,
  tracker: CenterPanelAdoptionTracker,
  terminal: TerminalSummary,
): void {
  if (!terminal.centerPanel) return;
  const key = terminalKey(terminal.threadId, terminal.terminalId);
  const hostRef = scopeThreadRef(environmentId, ThreadId.make(terminal.threadId));
  const store = useCenterPanelStore.getState();
  // A tab this client still shows follows the command its process runs, so a
  // relaunch from it starts the same program; a closed tab is never re-added.
  store.syncTerminalPanelCommand(hostRef, terminal.terminalId, terminal.command);
  if (tracker.terminalKeys.has(key)) return;
  tracker.terminalKeys.add(key);
  // An in-flight local open places its own surface with the requested placement.
  if (isTerminalIdReserved(hostRef, terminal.terminalId)) return;
  const label = terminal.command?.label;
  store.adoptTerminalPanel(hostRef, terminal.terminalId, {
    ...(label !== undefined ? { label } : {}),
    ...(terminal.command !== undefined ? { command: terminal.command } : {}),
  });
}

export function applyTerminalMetadataAdoption(
  environmentId: EnvironmentId,
  tracker: CenterPanelAdoptionTracker,
  event: TerminalMetadataStreamEvent,
): void {
  switch (event.type) {
    case "snapshot":
      for (const terminal of event.terminals) adoptTerminal(environmentId, tracker, terminal);
      return;
    case "upsert":
      adoptTerminal(environmentId, tracker, event.terminal);
      return;
    case "remove":
      // Restart failures and server shutdown keep the tab so it can relaunch.
      if (event.reason !== undefined && event.reason !== "closed") return;
      tracker.terminalKeys.delete(terminalKey(event.threadId, event.terminalId));
      useCenterPanelStore
        .getState()
        .removeTerminalPanel(
          scopeThreadRef(environmentId, ThreadId.make(event.threadId)),
          event.terminalId,
        );
      // Another client closed it: drop this client's retained input binding
      // without a second server close. Loaded on demand, off the root bundle.
      void import("./components/ThreadTerminalPanel").then(({ releaseTerminalUiResources }) =>
        releaseTerminalUiResources(environmentId, event.threadId, event.terminalId),
      );
  }
}

export function applyPanelThreadAdoption(
  environmentId: EnvironmentId,
  tracker: CenterPanelAdoptionTracker,
  threads: ReadonlyArray<
    Pick<OrchestrationThreadShell, "id" | "kind" | "hostThreadId" | "title" | "archivedAt">
  >,
): void {
  const liveThreadIds = new Set<string>(
    threads.flatMap((thread) => (thread.archivedAt === null ? [thread.id] : [])),
  );
  const store = useCenterPanelStore.getState();
  for (const thread of threads) {
    const hostThreadId = thread.hostThreadId;
    if (thread.kind !== "panel" || hostThreadId === undefined || thread.archivedAt !== null) {
      continue;
    }
    if (tracker.panelThreadIds.has(thread.id) || !liveThreadIds.has(hostThreadId)) continue;
    tracker.panelThreadIds.add(thread.id);
    const panelKey = scopedThreadKey(scopeThreadRef(environmentId, thread.id));
    if (store.pendingChatPanelThreadKeys.has(panelKey)) continue;
    store.adoptChatPanel(
      scopeThreadRef(environmentId, hostThreadId),
      thread.id,
      panelProviderLabel(thread.title),
    );
  }
  for (const threadId of tracker.panelThreadIds) {
    if (liveThreadIds.has(threadId)) continue;
    tracker.panelThreadIds.delete(threadId);
    const panelRef = scopeThreadRef(environmentId, ThreadId.make(threadId));
    if (!store.pendingChatPanelThreadKeys.has(scopedThreadKey(panelRef))) {
      store.removeThread(panelRef);
    }
  }
}
