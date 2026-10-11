/**
 * Per-thread preview UI state.
 *
 * Each thread owns an independent atom. Most consumers read exactly one
 * thread; the desktop browser host uses the aggregate session atom because it
 * is the one place that must enumerate every live preview tab.
 */
import { useAtomValue } from "@effect/atom-react";
import { parseScopedThreadKey, scopedThreadKey } from "@bibcode/client-runtime/environment";
import {
  type PreviewEvent,
  type PreviewSessionSnapshot,
  type ScopedThreadRef,
} from "@bibcode/contracts";
import { Atom } from "effect/unstable/reactivity";

import { canFrameGatewayPreviews } from "./components/preview/previewBridge";
import { PREVIEW_RECENT_URL_LIMIT } from "./components/preview/previewConstants";
import { appAtomRegistry } from "./rpc/atomRegistry";

export interface DesktopPreviewOverlay {
  url: string | null;
  title?: string;
  canGoBack: boolean;
  canGoForward: boolean;
  loading: boolean;
  zoomFactor: number;
  controller: "human" | "agent" | "none";
}

/**
 * A page this client couldn't load although other clients may: its own SSH
 * forward, session or network reach failed. `code` 0 means `description` is a
 * full sentence from BiBCode rather than a network error name.
 */
export interface PreviewLocalFailure {
  readonly url: string;
  readonly code: number;
  readonly description: string;
}

export interface ThreadPreviewState {
  snapshot: PreviewSessionSnapshot | null;
  sessions: Record<string, PreviewSessionSnapshot>;
  /** Tabs intentionally closed by this client. Stale list snapshots must not resurrect them. */
  suppressedTabIds: ReadonlySet<string>;
  activeTabId: string | null;
  desktopOverlay: DesktopPreviewOverlay | null;
  desktopByTabId: Record<string, DesktopPreviewOverlay>;
  /** This client's own load failures by tab; never sent to the server. */
  localFailures: Record<string, PreviewLocalFailure>;
  recentlySeenUrls: string[];
}

const EMPTY_THREAD_PREVIEW_STATE: ThreadPreviewState = Object.freeze({
  snapshot: null,
  sessions: {},
  suppressedTabIds: new Set<string>(),
  activeTabId: null,
  desktopOverlay: null,
  desktopByTabId: {},
  localFailures: {},
  recentlySeenUrls: [] as string[],
});

const emptyPreviewStateAtom = Atom.make<ThreadPreviewState>(EMPTY_THREAD_PREVIEW_STATE).pipe(
  Atom.withLabel("preview:empty-thread"),
);

export const previewStateAtom = Atom.family((threadKey: string) =>
  Atom.make<ThreadPreviewState>(EMPTY_THREAD_PREVIEW_STATE).pipe(
    Atom.keepAlive,
    Atom.withLabel(`preview:thread:${threadKey}`),
  ),
);

// Native preview hosts need a cross-thread view. Keep that index
// separate so thread-local readers never subscribe to unrelated previews.
interface ActivePreviewThreadIndex {
  readonly keys: ReadonlySet<string>;
}

const activePreviewThreadKeysAtom = Atom.make<ActivePreviewThreadIndex>({
  keys: new Set<string>(),
}).pipe(Atom.keepAlive, Atom.withLabel("preview:active-thread-keys"));

const activePreviewSessionsAtom = Atom.make((get) => {
  const byThreadKey: Record<string, ThreadPreviewState> = {};
  for (const threadKey of get(activePreviewThreadKeysAtom).keys) {
    const state = get(previewStateAtom(threadKey));
    if (Object.keys(state.sessions).length > 0) {
      byThreadKey[threadKey] = state;
    }
  }
  return byThreadKey;
}).pipe(Atom.withLabel("preview:active-sessions"));

const changedPreviewThreadKeys = new Set<string>();

function syncActivePreviewThread(threadKey: string, state: ThreadPreviewState): void {
  const active = Object.keys(state.sessions).length > 0;
  appAtomRegistry.update(activePreviewThreadKeysAtom, (current) => {
    if (current.keys.has(threadKey) === active) return current;
    const next = new Set(current.keys);
    if (active) next.add(threadKey);
    else next.delete(threadKey);
    return { keys: next };
  });
}

function updateThreadPreviewState(
  ref: ScopedThreadRef,
  update: (current: ThreadPreviewState) => ThreadPreviewState,
): void {
  const threadKey = scopedThreadKey(ref);
  const atom = previewStateAtom(threadKey);
  let nextState = appAtomRegistry.get(atom);
  const changed = appAtomRegistry.modify(atom, (current) => {
    nextState = update(current);
    return [nextState !== current, nextState];
  });
  if (!changed) return;
  changedPreviewThreadKeys.add(threadKey);
  syncActivePreviewThread(threadKey, nextState);
}

const dedupeRecentUrls = (existing: string[], url: string): string[] => {
  const next = [url, ...existing.filter((entry) => entry !== url)];
  return next.slice(0, PREVIEW_RECENT_URL_LIMIT);
};

const rememberSnapshotUrl = (
  recentlySeenUrls: string[],
  snapshot: PreviewSessionSnapshot,
): string[] =>
  snapshot.navStatus._tag === "Idle"
    ? recentlySeenUrls
    : dedupeRecentUrls(recentlySeenUrls, snapshot.navStatus.url);

const latestSnapshot = (
  sessions: Record<string, PreviewSessionSnapshot>,
): PreviewSessionSnapshot | null =>
  Object.values(sessions)
    .toSorted((a, b) => a.updatedAt.localeCompare(b.updatedAt))
    .at(-1) ?? null;

const removeSession = (current: ThreadPreviewState, tabId: string): ThreadPreviewState => {
  if (!current.sessions[tabId]) return current;
  const { [tabId]: _closed, ...sessions } = current.sessions;
  const { [tabId]: _desktop, ...desktopByTabId } = current.desktopByTabId;
  const { [tabId]: _failure, ...localFailures } = current.localFailures;
  const nextSnapshot = latestSnapshot(sessions);
  const activeTabId =
    current.activeTabId === tabId ? (nextSnapshot?.tabId ?? null) : current.activeTabId;
  const snapshot = activeTabId ? (sessions[activeTabId] ?? nextSnapshot) : nextSnapshot;
  return {
    ...current,
    sessions,
    desktopByTabId,
    localFailures,
    activeTabId: snapshot?.tabId ?? null,
    snapshot,
    desktopOverlay: snapshot ? (desktopByTabId[snapshot.tabId] ?? null) : null,
  };
};

export function useThreadPreviewState(ref: ScopedThreadRef | null | undefined): ThreadPreviewState {
  const atom = ref ? previewStateAtom(scopedThreadKey(ref)) : emptyPreviewStateAtom;
  return useAtomValue(atom);
}

export function useActivePreviewSessions(): Record<string, ThreadPreviewState> {
  return useAtomValue(activePreviewSessionsAtom);
}

export function findPreviewThreadForTab(tabId: string): ScopedThreadRef | null {
  const sessions = appAtomRegistry.get(activePreviewSessionsAtom);
  for (const [threadKey, state] of Object.entries(sessions)) {
    if (state.sessions[tabId]) return parseScopedThreadKey(threadKey);
  }
  return null;
}

export function readThreadPreviewState(ref: ScopedThreadRef): ThreadPreviewState {
  return appAtomRegistry.get(previewStateAtom(scopedThreadKey(ref)));
}

export function subscribeThreadPreviewState(
  ref: ScopedThreadRef,
  listener: (state: ThreadPreviewState, previous: ThreadPreviewState) => void,
): () => void {
  const atom = previewStateAtom(scopedThreadKey(ref));
  let previous = appAtomRegistry.get(atom);
  return appAtomRegistry.subscribe(atom, (state) => {
    const prior = previous;
    previous = state;
    listener(state, prior);
  });
}

const RFC3339_FRACTION = /^(.*?)(?:\.(\d+))?(Z|[+-]\d\d:\d\d)$/i;

/** Whole seconds plus the fraction padded to a fixed width, so digit strings compare. */
function instant(value: string): readonly [number, string] | null {
  const match = RFC3339_FRACTION.exec(value);
  const seconds = match ? Date.parse(`${match[1]}${match[3]}`) : Number.NaN;
  return Number.isNaN(seconds) ? null : [seconds, (match?.[2] ?? "").padEnd(12, "0")];
}

/**
 * Server timestamps are RFC 3339 with variable fractional digits ("…01.1Z" is
 * earlier than "…01.15Z") and finer than milliseconds, so compare instants at
 * full precision, never raw strings or `Date.parse`.
 */
function isAfter(a: string | undefined, b: string): boolean {
  if (a === undefined) return false;
  const x = instant(a);
  const y = instant(b);
  if (!x || !y) return a > b;
  return x[0] !== y[0] ? x[0] > y[0] : x[1] > y[1];
}

export function applyPreviewServerEvent(ref: ScopedThreadRef, event: PreviewEvent): void {
  updateThreadPreviewState(ref, (current) => {
    switch (event.type) {
      case "opened":
      case "navigated":
      case "resized": {
        const snapshot = event.snapshot;
        if (current.suppressedTabIds.has(snapshot.tabId)) return current;
        // A replayed event (the events atom outlives session syncs) must not regress newer state.
        if (isAfter(current.sessions[snapshot.tabId]?.updatedAt, snapshot.updatedAt)) {
          return current;
        }
        const recentlySeenUrls =
          snapshot.navStatus._tag === "Idle"
            ? current.recentlySeenUrls
            : dedupeRecentUrls(current.recentlySeenUrls, snapshot.navStatus.url);
        const sessions = { ...current.sessions, [snapshot.tabId]: snapshot };
        const activeTabId = event.type === "opened" ? snapshot.tabId : current.activeTabId;
        const activeSnapshot = sessions[activeTabId ?? snapshot.tabId] ?? snapshot;
        return {
          ...current,
          sessions,
          activeTabId: activeTabId ?? snapshot.tabId,
          snapshot: activeSnapshot,
          desktopOverlay: current.desktopByTabId[activeSnapshot.tabId] ?? null,
          recentlySeenUrls,
        };
      }
      case "failed": {
        const existing = current.sessions[event.tabId];
        if (!existing || isAfter(existing.updatedAt, event.createdAt)) return current;
        const failedSnapshot = {
          ...existing,
          navStatus: {
            _tag: "LoadFailed" as const,
            url: event.url,
            title: event.title,
            code: event.code,
            description: event.description,
          },
          updatedAt: event.createdAt,
        };
        const sessions = { ...current.sessions, [event.tabId]: failedSnapshot };
        return {
          ...current,
          sessions,
          snapshot: current.activeTabId === event.tabId ? failedSnapshot : current.snapshot,
        };
      }
      case "closed":
        return removeSession(current, event.tabId);
      case "openRequested":
        return current;
    }
  });
}

export function applyPreviewServerSnapshot(
  ref: ScopedThreadRef,
  snapshot: PreviewSessionSnapshot | null,
): void {
  updateThreadPreviewState(ref, (current) => {
    if (!snapshot && current.snapshot === null) return current;
    if (!snapshot) {
      return {
        ...current,
        snapshot: null,
        sessions: {},
        activeTabId: null,
        desktopOverlay: null,
        desktopByTabId: {},
        localFailures: {},
      };
    }
    if (current.suppressedTabIds.has(snapshot.tabId)) return current;
    const existing = current.sessions[snapshot.tabId];
    if (isAfter(existing?.updatedAt, snapshot.updatedAt)) return current;
    const recentlySeenUrls = rememberSnapshotUrl(current.recentlySeenUrls, snapshot);
    return {
      ...current,
      snapshot,
      sessions: { ...current.sessions, [snapshot.tabId]: snapshot },
      activeTabId: snapshot.tabId,
      desktopOverlay: current.desktopByTabId[snapshot.tabId] ?? null,
      recentlySeenUrls,
    };
  });
}

/**
 * Merge a server mutation without changing which tab the user is viewing.
 *
 * Commands such as resize can target background tabs. Their response is
 * authoritative for that tab, but it is not a request to focus the tab.
 */
export function updatePreviewServerSnapshot(
  ref: ScopedThreadRef,
  snapshot: PreviewSessionSnapshot,
): void {
  updateThreadPreviewState(ref, (current) => {
    if (current.suppressedTabIds.has(snapshot.tabId)) return current;
    const existing = current.sessions[snapshot.tabId];
    if (isAfter(existing?.updatedAt, snapshot.updatedAt)) return current;
    const sessions = { ...current.sessions, [snapshot.tabId]: snapshot };
    const activeTabId =
      current.activeTabId && sessions[current.activeTabId] ? current.activeTabId : snapshot.tabId;
    const activeSnapshot = sessions[activeTabId] ?? snapshot;
    return {
      ...current,
      sessions,
      activeTabId,
      snapshot: activeSnapshot,
      desktopOverlay: current.desktopByTabId[activeTabId] ?? null,
      recentlySeenUrls: rememberSnapshotUrl(current.recentlySeenUrls, snapshot),
    };
  });
}

/**
 * Replace the local session index from an authoritative preview.list result.
 * Missing tabs are removed while the current active tab is preserved whenever
 * it still exists in the server result.
 */
export function reconcilePreviewServerSessions(
  ref: ScopedThreadRef,
  snapshots: ReadonlyArray<PreviewSessionSnapshot>,
): void {
  updateThreadPreviewState(ref, (current) => {
    const sessions: Record<string, PreviewSessionSnapshot> = {};
    let recentlySeenUrls = current.recentlySeenUrls;
    for (const snapshot of snapshots) {
      if (current.suppressedTabIds.has(snapshot.tabId)) continue;
      const existing = current.sessions[snapshot.tabId];
      const next =
        existing && isAfter(existing.updatedAt, snapshot.updatedAt) ? existing : snapshot;
      sessions[next.tabId] = next;
      recentlySeenUrls = rememberSnapshotUrl(recentlySeenUrls, next);
    }

    const fallback = latestSnapshot(sessions);
    const activeTabId =
      current.activeTabId && sessions[current.activeTabId]
        ? current.activeTabId
        : (fallback?.tabId ?? null);
    const snapshot = activeTabId ? (sessions[activeTabId] ?? null) : null;
    const desktopByTabId = Object.fromEntries(
      Object.entries(current.desktopByTabId).filter(([tabId]) => sessions[tabId] !== undefined),
    );
    const localFailures = Object.fromEntries(
      Object.entries(current.localFailures).filter(([tabId]) => sessions[tabId] !== undefined),
    );
    return {
      ...current,
      sessions,
      activeTabId,
      snapshot,
      desktopByTabId,
      localFailures,
      desktopOverlay: activeTabId ? (desktopByTabId[activeTabId] ?? null) : null,
      recentlySeenUrls,
    };
  });
}

export function applyPreviewDesktopState(
  ref: ScopedThreadRef,
  tabId: string,
  overlay: DesktopPreviewOverlay | null,
): void {
  updateThreadPreviewState(ref, (current) => {
    const desktopByTabId = { ...current.desktopByTabId };
    if (overlay) desktopByTabId[tabId] = overlay;
    else delete desktopByTabId[tabId];
    return {
      ...current,
      desktopByTabId,
      desktopOverlay: current.activeTabId === tabId ? overlay : current.desktopOverlay,
    };
  });
}

/** Sets or clears (`null`) a tab's client-local load failure. */
export function setPreviewLocalFailure(
  ref: ScopedThreadRef,
  tabId: string,
  failure: PreviewLocalFailure | null,
): void {
  updateThreadPreviewState(ref, (current) => {
    if (failure === null) {
      if (!(tabId in current.localFailures)) return current;
      const { [tabId]: _cleared, ...localFailures } = current.localFailures;
      return { ...current, localFailures };
    }
    return { ...current, localFailures: { ...current.localFailures, [tabId]: failure } };
  });
}

export function beginPreviewSessionClose(ref: ScopedThreadRef, tabId: string): void {
  updateThreadPreviewState(ref, (current) => {
    const suppressedTabIds = new Set(current.suppressedTabIds);
    suppressedTabIds.add(tabId);
    return {
      ...removeSession(current, tabId),
      suppressedTabIds,
    };
  });
}

export function cancelPreviewSessionClose(
  ref: ScopedThreadRef,
  snapshot: PreviewSessionSnapshot | null,
  tabId: string,
): void {
  updateThreadPreviewState(ref, (current) => {
    if (!current.suppressedTabIds.has(tabId)) return current;
    const suppressedTabIds = new Set(current.suppressedTabIds);
    suppressedTabIds.delete(tabId);
    if (!snapshot) {
      return { ...current, suppressedTabIds };
    }
    const recentlySeenUrls =
      snapshot.navStatus._tag !== "Idle"
        ? dedupeRecentUrls(current.recentlySeenUrls, snapshot.navStatus.url)
        : current.recentlySeenUrls;
    return {
      ...current,
      snapshot,
      sessions: { ...current.sessions, [snapshot.tabId]: snapshot },
      suppressedTabIds,
      activeTabId: snapshot.tabId,
      desktopOverlay: current.desktopByTabId[snapshot.tabId] ?? null,
      recentlySeenUrls,
    };
  });
}

export function setActivePreviewTab(ref: ScopedThreadRef, tabId: string): void {
  updateThreadPreviewState(ref, (current) => {
    const snapshot = current.sessions[tabId];
    if (!snapshot || current.activeTabId === tabId) return current;
    return {
      ...current,
      activeTabId: tabId,
      snapshot,
      desktopOverlay: current.desktopByTabId[tabId] ?? null,
    };
  });
}

export function rememberPreviewUrl(ref: ScopedThreadRef, url: string): void {
  if (url.trim().length === 0) return;
  updateThreadPreviewState(ref, (current) => ({
    ...current,
    recentlySeenUrls: dedupeRecentUrls(current.recentlySeenUrls, url),
  }));
}

export function removePreviewThread(ref: ScopedThreadRef): void {
  const threadKey = scopedThreadKey(ref);
  appAtomRegistry.set(previewStateAtom(threadKey), EMPTY_THREAD_PREVIEW_STATE);
  syncActivePreviewThread(threadKey, EMPTY_THREAD_PREVIEW_STATE);
  changedPreviewThreadKeys.delete(threadKey);
}

/** The preview panel can show pages: the desktop's native views, or framed gateway previews in browser mode. */
export function isPreviewSupportedInRuntime(): boolean {
  return canPreviewAnyUrlInRuntime() || canFrameGatewayPreviews();
}

/**
 * The panel can show any address, including file previews. Browser mode frames
 * only gateway previews: other pages would share BiBCode's own origin or refuse
 * to be framed.
 */
export function canPreviewAnyUrlInRuntime(): boolean {
  if (typeof window === "undefined") return false;
  return Boolean(window.desktopBridge?.preview);
}

export function resetPreviewStateForTests(): void {
  for (const threadKey of changedPreviewThreadKeys) {
    appAtomRegistry.set(previewStateAtom(threadKey), EMPTY_THREAD_PREVIEW_STATE);
  }
  changedPreviewThreadKeys.clear();
  appAtomRegistry.set(activePreviewThreadKeysAtom, { keys: new Set<string>() });
}

export const __testing = {
  EMPTY_THREAD_PREVIEW_STATE,
  RECENT_URL_LIMIT: PREVIEW_RECENT_URL_LIMIT,
};
