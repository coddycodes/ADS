/**
 * Thread-scoped right-panel surface state.
 *
 * This is intentionally a shallow workspace model: it owns an ordered set of
 * surface descriptors and the active surface, while each feature continues to
 * own its durable resource state. Browser surfaces point at preview tab ids,
 * terminal surfaces point at terminal session ids, file surfaces point at
 * workspace paths, and diff/plan/files/sourceControl remain singleton
 * surfaces.
 */
import { scopedThreadKey } from "@bibcode/client-runtime/environment";
import {
  ACTIVITY_ID_MAX_LENGTH,
  type ScopedThreadRef,
  SourceControlProviderKind,
} from "@bibcode/contracts";
import * as Schema from "effect/Schema";
import { create } from "zustand";
import { createJSONStorage, persist } from "zustand/middleware";

import { resolveStorage } from "./lib/storage";
import type { PullRequestsDetailTab } from "./pullRequestsStore";
import { MAX_TERMINALS_PER_GROUP } from "./types";

export const RIGHT_PANEL_KINDS = [
  "plan",
  "diff",
  "sourceControl",
  "files",
  "file",
  "preview",
  "terminal",
  "activity",
  "pullRequest",
] as const;
export type RightPanelKind = (typeof RIGHT_PANEL_KINDS)[number];

export type ActivityRightPanelSurface = {
  id: "activity";
  kind: "activity";
  scope: { _tag: "thread" } | { _tag: "terminal"; terminalId: string };
  section: "subagents" | "backgroundTasks";
  selectedRecordKind: "actor" | "workItem" | null;
  selectedRecordId: string | null;
};

export type PullRequestRightPanelSurface = {
  id: `pullRequest:${number}`;
  kind: "pullRequest";
  number: number;
  provider: SourceControlProviderKind;
  tab: PullRequestsDetailTab;
};

export type RightPanelSurface =
  | { id: `browser:${string}`; kind: "preview"; resourceId: string }
  | { id: "browser:new"; kind: "preview"; resourceId: null }
  | {
      id: `terminal:${string}`;
      kind: "terminal";
      resourceId: string;
      terminalIds: string[];
      activeTerminalId: string;
      splitDirection?: "horizontal" | "vertical";
    }
  | { id: "diff"; kind: "diff" }
  | { id: "sourceControl"; kind: "sourceControl" }
  | { id: "files"; kind: "files" }
  | {
      id: `file:${string}`;
      kind: "file";
      relativePath: string;
      revealLine: number | null;
      revealRequestId: number;
    }
  | { id: "plan"; kind: "plan" }
  | ActivityRightPanelSurface
  | PullRequestRightPanelSurface;

const RIGHT_PANEL_STORAGE_KEY = "bibcode:right-panel-state:v2";
const RIGHT_PANEL_STORAGE_VERSION = 9;

export interface ThreadRightPanelState {
  isOpen: boolean;
  activeSurfaceId: string | null;
  surfaces: RightPanelSurface[];
}

type GenericRightPanelKind = Exclude<
  RightPanelKind,
  "file" | "terminal" | "activity" | "pullRequest"
>;

interface RightPanelStoreState {
  byThreadKey: Record<string, ThreadRightPanelState>;
  open: (ref: ScopedThreadRef, kind: GenericRightPanelKind) => void;
  openActivity: (
    ref: ScopedThreadRef,
    section: ActivityRightPanelSurface["section"],
    scope?: ActivityRightPanelSurface["scope"],
  ) => void;
  navigateActivity: (
    ref: ScopedThreadRef,
    route: Pick<ActivityRightPanelSurface, "section" | "selectedRecordKind" | "selectedRecordId">,
  ) => void;
  openBrowser: (ref: ScopedThreadRef, tabId: string | null) => void;
  openFile: (ref: ScopedThreadRef, relativePath: string, line?: number) => void;
  openPullRequest: (
    ref: ScopedThreadRef,
    request: { number: number; provider: SourceControlProviderKind },
    tab?: PullRequestsDetailTab,
  ) => void;
  setPullRequestTab: (ref: ScopedThreadRef, number: number, tab: PullRequestsDetailTab) => void;
  openTerminal: (ref: ScopedThreadRef, terminalId: string) => void;
  splitTerminal: (
    ref: ScopedThreadRef,
    surfaceId: string,
    terminalId: string,
    direction?: "horizontal" | "vertical",
  ) => boolean;
  activateTerminal: (ref: ScopedThreadRef, surfaceId: string, terminalId: string) => void;
  closeTerminal: (ref: ScopedThreadRef, surfaceId: string, terminalId: string) => void;
  activateSurface: (ref: ScopedThreadRef, surfaceId: string) => void;
  closeSurface: (ref: ScopedThreadRef, surfaceId: string) => void;
  closeOtherSurfaces: (ref: ScopedThreadRef, surfaceId: string) => void;
  closeSurfacesToRight: (ref: ScopedThreadRef, surfaceId: string) => void;
  closeAllSurfaces: (ref: ScopedThreadRef) => void;
  reconcileBrowserSurfaces: (ref: ScopedThreadRef, tabIds: readonly string[]) => void;
  reconcileFileSurfaces: (ref: ScopedThreadRef, workspaceAvailable: boolean) => void;
  remapFileSurfaces: (
    ref: ScopedThreadRef,
    oldRelativePath: string,
    newRelativePath: string,
  ) => void;
  closeFileSurfacesUnder: (ref: ScopedThreadRef, relativePath: string) => void;
  show: (ref: ScopedThreadRef) => void;
  close: (ref: ScopedThreadRef) => void;
  toggleVisibility: (ref: ScopedThreadRef) => void;
  toggle: (ref: ScopedThreadRef, kind: GenericRightPanelKind) => void;
  removeThread: (ref: ScopedThreadRef) => void;
}

const EMPTY_THREAD_STATE: ThreadRightPanelState = {
  isOpen: false,
  activeSurfaceId: null,
  surfaces: [],
};

const singletonSurface = (kind: Exclude<GenericRightPanelKind, "preview">): RightPanelSurface => {
  switch (kind) {
    case "diff":
      return { id: "diff", kind };
    case "sourceControl":
      return { id: "sourceControl", kind };
    case "files":
      return { id: "files", kind };
    case "plan":
      return { id: "plan", kind };
  }
};

const DEFAULT_ACTIVITY_SCOPE: ActivityRightPanelSurface["scope"] = { _tag: "thread" };

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object";
}

function isNonEmptyString(value: unknown): value is string {
  return typeof value === "string" && value.length > 0;
}

function isBoundedActivityId(value: unknown): value is string {
  return (
    typeof value === "string" &&
    value.length > 0 &&
    value.length <= ACTIVITY_ID_MAX_LENGTH &&
    value.trim() === value
  );
}

function normalizeActivityScope(value: unknown): ActivityRightPanelSurface["scope"] {
  if (isRecord(value) && value._tag === "terminal" && isBoundedActivityId(value.terminalId)) {
    return { _tag: "terminal", terminalId: value.terminalId };
  }
  return { _tag: "thread" };
}

function normalizeActivitySection(value: unknown): ActivityRightPanelSurface["section"] {
  return value === "backgroundTasks" ? "backgroundTasks" : "subagents";
}

function normalizeActivitySelection(
  section: ActivityRightPanelSurface["section"],
  kind: unknown,
  id: unknown,
): Pick<ActivityRightPanelSurface, "selectedRecordKind" | "selectedRecordId"> {
  const kindMatchesSection =
    (section === "subagents" && kind === "actor") ||
    (section === "backgroundTasks" && kind === "workItem");
  if (kindMatchesSection && isBoundedActivityId(id)) {
    return { selectedRecordKind: kind, selectedRecordId: id };
  }
  return { selectedRecordKind: null, selectedRecordId: null };
}

function activityScopesEqual(
  left: ActivityRightPanelSurface["scope"],
  right: ActivityRightPanelSurface["scope"],
): boolean {
  return (
    left._tag === right._tag &&
    (left._tag === "thread" || (right._tag === "terminal" && left.terminalId === right.terminalId))
  );
}

function normalizeActivitySurface(
  value: Record<string, unknown>,
): ActivityRightPanelSurface | null {
  if (value.id !== "activity" || value.kind !== "activity") return null;
  const section = normalizeActivitySection(value.section);
  const selection = normalizeActivitySelection(
    section,
    value.selectedRecordKind,
    value.selectedRecordId,
  );
  return {
    id: "activity",
    kind: "activity",
    scope: normalizeActivityScope(value.scope),
    section,
    ...selection,
  };
}

const browserSurface = (tabId: string | null): RightPanelSurface =>
  tabId
    ? { id: `browser:${tabId}`, kind: "preview", resourceId: tabId }
    : { id: "browser:new", kind: "preview", resourceId: null };

const fileSurface = (
  relativePath: string,
  revealLine: number | null,
  revealRequestId: number,
): RightPanelSurface => ({
  id: `file:${relativePath}`,
  kind: "file",
  relativePath,
  revealLine,
  revealRequestId,
});

const terminalSurface = (terminalId: string): RightPanelSurface => ({
  id: `terminal:${terminalId}`,
  kind: "terminal",
  resourceId: terminalId,
  terminalIds: [terminalId],
  activeTerminalId: terminalId,
});

const PULL_REQUEST_DETAIL_TABS: ReadonlySet<unknown> = new Set<PullRequestsDetailTab>([
  "conversation",
  "commits",
  "checks",
  "files",
]);
const isPullRequestDetailTab = (value: unknown): value is PullRequestsDetailTab =>
  PULL_REQUEST_DETAIL_TABS.has(value);
const isSourceControlProviderKind = Schema.is(SourceControlProviderKind);

export function pullRequestSurfaceId(number: number): `pullRequest:${number}` {
  return `pullRequest:${number}`;
}

function normalizePullRequestSurface(
  value: Record<string, unknown>,
): PullRequestRightPanelSurface | null {
  const { number, provider, tab } = value;
  if (typeof number !== "number" || !Number.isSafeInteger(number) || number < 1) return null;
  if (value.id !== pullRequestSurfaceId(number) || !isSourceControlProviderKind(provider)) {
    return null;
  }
  return {
    id: pullRequestSurfaceId(number),
    kind: "pullRequest",
    number,
    provider,
    tab: isPullRequestDetailTab(tab) ? tab : "conversation",
  };
}

const upsertSurface = (
  current: ThreadRightPanelState,
  surface: RightPanelSurface,
  activate = true,
): ThreadRightPanelState => ({
  isOpen: true,
  surfaces: current.surfaces.some((entry) => entry.id === surface.id)
    ? current.surfaces
    : [...current.surfaces, surface],
  activeSurfaceId: activate ? surface.id : current.activeSurfaceId,
});

const updateThread = (
  byThreadKey: Record<string, ThreadRightPanelState>,
  threadKey: string,
  updater: (current: ThreadRightPanelState) => ThreadRightPanelState,
): Record<string, ThreadRightPanelState> => {
  const current = byThreadKey[threadKey] ?? EMPTY_THREAD_STATE;
  const next = updater(current);
  if (!next.isOpen && next.activeSurfaceId === null && next.surfaces.length === 0) {
    if (!(threadKey in byThreadKey)) return byThreadKey;
    const { [threadKey]: _removed, ...rest } = byThreadKey;
    return rest;
  }
  if (next === current) return byThreadKey;
  return { ...byThreadKey, [threadKey]: next };
};

function normalizeRevealLine(line: number | undefined): number | null {
  if (line === undefined || !Number.isFinite(line)) return null;
  return Math.max(1, Math.trunc(line));
}

function normalizePersistedSurface(value: unknown): RightPanelSurface | null {
  if (!isRecord(value) || typeof value.kind !== "string") return null;

  switch (value.kind) {
    case "activity":
      return normalizeActivitySurface(value);
    case "diff":
      return value.id === "diff" ? { id: "diff", kind: "diff" } : null;
    case "sourceControl":
      return value.id === "sourceControl" ? { id: "sourceControl", kind: "sourceControl" } : null;
    case "files":
      return value.id === "files" ? { id: "files", kind: "files" } : null;
    case "plan":
      return value.id === "plan" ? { id: "plan", kind: "plan" } : null;
    case "preview": {
      if (value.id === "browser:new" && value.resourceId === null) {
        return { id: "browser:new", kind: "preview", resourceId: null };
      }
      if (isNonEmptyString(value.resourceId) && value.id === `browser:${value.resourceId}`) {
        return {
          id: `browser:${value.resourceId}`,
          kind: "preview",
          resourceId: value.resourceId,
        };
      }
      return null;
    }
    case "file": {
      if (!isNonEmptyString(value.relativePath) || value.id !== `file:${value.relativePath}`) {
        return null;
      }
      const revealLine =
        typeof value.revealLine === "number" && Number.isFinite(value.revealLine)
          ? Math.max(1, Math.trunc(value.revealLine))
          : null;
      const revealRequestId =
        typeof value.revealRequestId === "number" &&
        Number.isSafeInteger(value.revealRequestId) &&
        value.revealRequestId >= 0
          ? value.revealRequestId
          : 0;
      return {
        id: `file:${value.relativePath}`,
        kind: "file",
        relativePath: value.relativePath,
        revealLine,
        revealRequestId,
      };
    }
    case "terminal": {
      if (!isNonEmptyString(value.resourceId) || value.id !== `terminal:${value.resourceId}`) {
        return null;
      }
      const terminalIds = Array.isArray(value.terminalIds)
        ? [
            ...new Set(
              value.terminalIds.filter((terminalId): terminalId is string =>
                isNonEmptyString(terminalId),
              ),
            ),
          ]
        : [value.resourceId];
      const normalizedTerminalIds = terminalIds.length > 0 ? terminalIds : [value.resourceId];
      const activeTerminalId =
        isNonEmptyString(value.activeTerminalId) &&
        normalizedTerminalIds.includes(value.activeTerminalId)
          ? value.activeTerminalId
          : normalizedTerminalIds[0]!;
      return {
        id: `terminal:${value.resourceId}`,
        kind: "terminal",
        resourceId: value.resourceId,
        terminalIds: normalizedTerminalIds,
        activeTerminalId,
        ...(value.splitDirection === "horizontal" || value.splitDirection === "vertical"
          ? { splitDirection: value.splitDirection }
          : {}),
      };
    }
    case "pullRequest":
      return normalizePullRequestSurface(value);
    default:
      return null;
  }
}

export function migratePersistedRightPanelState(persistedState: unknown): {
  byThreadKey: Record<string, ThreadRightPanelState>;
} {
  if (!persistedState || typeof persistedState !== "object") {
    return { byThreadKey: {} };
  }
  const byThreadKey =
    "byThreadKey" in persistedState &&
    persistedState.byThreadKey &&
    typeof persistedState.byThreadKey === "object"
      ? Object.fromEntries(
          Object.entries(persistedState.byThreadKey as Record<string, unknown>).map(
            ([threadKey, threadState]) => {
              const validThreadState = isRecord(threadState) ? threadState : null;
              const surfaces: RightPanelSurface[] = [];
              const seenSurfaceIds = new Set<string>();
              if (Array.isArray(validThreadState?.surfaces)) {
                for (const candidate of validThreadState.surfaces) {
                  const surface = normalizePersistedSurface(candidate);
                  if (!surface || seenSurfaceIds.has(surface.id)) continue;
                  seenSurfaceIds.add(surface.id);
                  surfaces.push(surface);
                }
              }
              const activeSurfaceId = surfaces.some(
                (surface) => surface.id === validThreadState?.activeSurfaceId,
              )
                ? (validThreadState?.activeSurfaceId as string)
                : null;
              const allPersistedSurfacesWereInvalid =
                Array.isArray(validThreadState?.surfaces) &&
                validThreadState.surfaces.length > 0 &&
                surfaces.length === 0;
              const isOpen = allPersistedSurfacesWereInvalid
                ? false
                : typeof validThreadState?.isOpen === "boolean"
                  ? validThreadState.isOpen
                  : activeSurfaceId !== null;
              return [threadKey, { isOpen, surfaces, activeSurfaceId }];
            },
          ),
        )
      : {};
  return { byThreadKey };
}

export const useRightPanelStore = create<RightPanelStoreState>()(
  persist(
    (set) => ({
      byThreadKey: {},
      open: (ref, kind) =>
        set((state) => ({
          byThreadKey: updateThread(state.byThreadKey, scopedThreadKey(ref), (current) => {
            if (kind === "preview") {
              const existing = current.surfaces.find((surface) => surface.kind === "preview");
              return upsertSurface(current, existing ?? browserSurface(null));
            }
            return upsertSurface(current, singletonSurface(kind));
          }),
        })),
      openActivity: (ref, section, scope = DEFAULT_ACTIVITY_SCOPE) =>
        set((state) => ({
          byThreadKey: updateThread(state.byThreadKey, scopedThreadKey(ref), (current) => {
            const normalizedSection = normalizeActivitySection(section);
            const normalizedScope = normalizeActivityScope(scope);
            const existing = current.surfaces.find(
              (surface): surface is ActivityRightPanelSurface => surface.kind === "activity",
            );
            const routeChanged =
              existing === undefined ||
              existing.section !== normalizedSection ||
              !activityScopesEqual(existing.scope, normalizedScope);
            const surface: ActivityRightPanelSurface = {
              id: "activity",
              kind: "activity",
              scope: normalizedScope,
              section: normalizedSection,
              selectedRecordKind: routeChanged ? null : existing.selectedRecordKind,
              selectedRecordId: routeChanged ? null : existing.selectedRecordId,
            };
            return {
              isOpen: true,
              activeSurfaceId: "activity",
              surfaces: existing
                ? current.surfaces.map((entry) => (entry.id === "activity" ? surface : entry))
                : [...current.surfaces, surface],
            };
          }),
        })),
      navigateActivity: (ref, route) =>
        set((state) => ({
          byThreadKey: updateThread(state.byThreadKey, scopedThreadKey(ref), (current) => {
            if (!current.isOpen) return current;
            const existing = current.surfaces.find(
              (surface): surface is ActivityRightPanelSurface => surface.kind === "activity",
            );
            if (!existing) return current;
            const section = normalizeActivitySection(route.section);
            const selection =
              section === existing.section
                ? normalizeActivitySelection(
                    section,
                    route.selectedRecordKind,
                    route.selectedRecordId,
                  )
                : { selectedRecordKind: null, selectedRecordId: null };
            const surface: ActivityRightPanelSurface = {
              ...existing,
              section,
              ...selection,
            };
            if (
              surface.section === existing.section &&
              surface.selectedRecordKind === existing.selectedRecordKind &&
              surface.selectedRecordId === existing.selectedRecordId
            ) {
              return current;
            }
            return {
              ...current,
              surfaces: current.surfaces.map((entry) =>
                entry.id === "activity" ? surface : entry,
              ),
            };
          }),
        })),
      openBrowser: (ref, tabId) =>
        set((state) => ({
          byThreadKey: updateThread(state.byThreadKey, scopedThreadKey(ref), (current) => {
            const surface = browserSurface(tabId);
            const withoutPlaceholder = tabId
              ? current.surfaces.filter((entry) => entry.id !== "browser:new")
              : current.surfaces;
            return upsertSurface({ ...current, surfaces: withoutPlaceholder }, surface);
          }),
        })),
      openPullRequest: (ref, request, tab) =>
        set((state) => ({
          byThreadKey: updateThread(state.byThreadKey, scopedThreadKey(ref), (current) => {
            const id = pullRequestSurfaceId(request.number);
            const existing = current.surfaces.find((surface) => surface.id === id);
            if (existing) {
              return {
                isOpen: true,
                activeSurfaceId: id,
                surfaces:
                  tab === undefined
                    ? current.surfaces
                    : current.surfaces.map((surface) =>
                        surface.id === id ? { ...surface, tab } : surface,
                      ),
              };
            }
            return upsertSurface(current, {
              id,
              kind: "pullRequest",
              number: request.number,
              provider: request.provider,
              tab: tab ?? "conversation",
            });
          }),
        })),
      setPullRequestTab: (ref, number, tab) =>
        set((state) => ({
          byThreadKey: updateThread(state.byThreadKey, scopedThreadKey(ref), (current) => {
            const id = pullRequestSurfaceId(number);
            if (!current.surfaces.some((surface) => surface.id === id)) return current;
            return {
              ...current,
              surfaces: current.surfaces.map((surface) =>
                surface.id === id && surface.kind === "pullRequest" ? { ...surface, tab } : surface,
              ),
            };
          }),
        })),
      openFile: (ref, relativePath, line) =>
        set((state) => ({
          byThreadKey: updateThread(state.byThreadKey, scopedThreadKey(ref), (current) => {
            const withoutStandaloneExplorer = current.surfaces.filter(
              (surface) => surface.kind !== "files",
            );
            const surfaceId = `file:${relativePath}` as const;
            const existing = withoutStandaloneExplorer.find(
              (surface): surface is Extract<RightPanelSurface, { kind: "file" }> =>
                surface.id === surfaceId && surface.kind === "file",
            );
            const surface = fileSurface(
              relativePath,
              normalizeRevealLine(line),
              (existing?.revealRequestId ?? 0) + 1,
            );
            return {
              isOpen: true,
              activeSurfaceId: surface.id,
              surfaces: existing
                ? withoutStandaloneExplorer.map((entry) =>
                    entry.id === surface.id ? surface : entry,
                  )
                : [...withoutStandaloneExplorer, surface],
            };
          }),
        })),
      openTerminal: (ref, terminalId) =>
        set((state) => ({
          byThreadKey: updateThread(state.byThreadKey, scopedThreadKey(ref), (current) =>
            upsertSurface(current, terminalSurface(terminalId)),
          ),
        })),
      splitTerminal: (ref, surfaceId, terminalId, direction = "horizontal") => {
        let didSplit = false;
        set((state) => ({
          byThreadKey: updateThread(state.byThreadKey, scopedThreadKey(ref), (current) => {
            const surface = current.surfaces.find((entry) => entry.id === surfaceId);
            if (
              surface?.kind !== "terminal" ||
              surface.terminalIds.includes(terminalId) ||
              surface.terminalIds.length >= MAX_TERMINALS_PER_GROUP
            ) {
              return current;
            }
            didSplit = true;
            const { splitDirection: _splitDirection, ...baseSurface } = surface;
            const nextSurface: RightPanelSurface = {
              ...baseSurface,
              terminalIds: [...surface.terminalIds, terminalId],
              activeTerminalId: terminalId,
              ...(direction === "vertical" ? { splitDirection: "vertical" as const } : {}),
            };
            return {
              ...current,
              isOpen: true,
              activeSurfaceId: surfaceId,
              surfaces: current.surfaces.map((entry) =>
                entry.id === surfaceId ? nextSurface : entry,
              ),
            };
          }),
        }));
        return didSplit;
      },
      activateTerminal: (ref, surfaceId, terminalId) =>
        set((state) => ({
          byThreadKey: updateThread(state.byThreadKey, scopedThreadKey(ref), (current) => ({
            ...current,
            activeSurfaceId: surfaceId,
            surfaces: current.surfaces.map((surface) =>
              surface.id === surfaceId &&
              surface.kind === "terminal" &&
              surface.terminalIds.includes(terminalId)
                ? { ...surface, activeTerminalId: terminalId }
                : surface,
            ),
          })),
        })),
      closeTerminal: (ref, surfaceId, terminalId) =>
        set((state) => ({
          byThreadKey: updateThread(state.byThreadKey, scopedThreadKey(ref), (current) => {
            const surface = current.surfaces.find(
              (entry) => entry.id === surfaceId && entry.kind === "terminal",
            );
            if (!surface || surface.kind !== "terminal") return current;
            const terminalIds = surface.terminalIds.filter((id) => id !== terminalId);
            if (terminalIds.length === 0) {
              const index = current.surfaces.findIndex((entry) => entry.id === surfaceId);
              const surfaces = current.surfaces.filter((entry) => entry.id !== surfaceId);
              const fallback = surfaces[Math.min(index, surfaces.length - 1)] ?? null;
              return {
                ...current,
                isOpen: surfaces.length > 0 && current.isOpen,
                surfaces,
                activeSurfaceId:
                  current.activeSurfaceId === surfaceId
                    ? (fallback?.id ?? null)
                    : current.activeSurfaceId,
              };
            }
            return {
              ...current,
              surfaces: current.surfaces.map((entry) =>
                entry.id === surfaceId && entry.kind === "terminal"
                  ? {
                      ...entry,
                      terminalIds,
                      activeTerminalId:
                        entry.activeTerminalId === terminalId
                          ? (terminalIds.at(-1) ?? terminalIds[0]!)
                          : entry.activeTerminalId,
                    }
                  : entry,
              ),
            };
          }),
        })),
      activateSurface: (ref, surfaceId) =>
        set((state) => ({
          byThreadKey: updateThread(state.byThreadKey, scopedThreadKey(ref), (current) =>
            current.surfaces.some((surface) => surface.id === surfaceId)
              ? { ...current, isOpen: true, activeSurfaceId: surfaceId }
              : current,
          ),
        })),
      closeSurface: (ref, surfaceId) =>
        set((state) => ({
          byThreadKey: updateThread(state.byThreadKey, scopedThreadKey(ref), (current) => {
            const index = current.surfaces.findIndex((surface) => surface.id === surfaceId);
            if (index < 0) return current;
            const surfaces = current.surfaces.filter((surface) => surface.id !== surfaceId);
            if (current.activeSurfaceId !== surfaceId) {
              return { ...current, isOpen: surfaces.length > 0 && current.isOpen, surfaces };
            }
            const fallback = surfaces[Math.min(index, surfaces.length - 1)] ?? null;
            return {
              ...current,
              isOpen: surfaces.length > 0 && current.isOpen,
              surfaces,
              activeSurfaceId: fallback?.id ?? null,
            };
          }),
        })),
      closeOtherSurfaces: (ref, surfaceId) =>
        set((state) => ({
          byThreadKey: updateThread(state.byThreadKey, scopedThreadKey(ref), (current) => {
            const surface = current.surfaces.find((entry) => entry.id === surfaceId);
            if (!surface || current.surfaces.length === 1) return current;
            return {
              ...current,
              isOpen: true,
              surfaces: [surface],
              activeSurfaceId: surface.id,
            };
          }),
        })),
      closeSurfacesToRight: (ref, surfaceId) =>
        set((state) => ({
          byThreadKey: updateThread(state.byThreadKey, scopedThreadKey(ref), (current) => {
            const index = current.surfaces.findIndex((surface) => surface.id === surfaceId);
            if (index < 0 || index === current.surfaces.length - 1) return current;
            const surfaces = current.surfaces.slice(0, index + 1);
            const activeStillExists = surfaces.some(
              (surface) => surface.id === current.activeSurfaceId,
            );
            return {
              ...current,
              surfaces,
              activeSurfaceId: activeStillExists ? current.activeSurfaceId : surfaceId,
            };
          }),
        })),
      closeAllSurfaces: (ref) =>
        set((state) => ({
          byThreadKey: updateThread(state.byThreadKey, scopedThreadKey(ref), (current) =>
            current.surfaces.length === 0
              ? current
              : { ...current, isOpen: false, surfaces: [], activeSurfaceId: null },
          ),
        })),
      reconcileBrowserSurfaces: (ref, tabIds) =>
        set((state) => ({
          byThreadKey: updateThread(state.byThreadKey, scopedThreadKey(ref), (current) => {
            const validIds = new Set(tabIds.map((tabId) => `browser:${tabId}`));
            const nonBrowser = current.surfaces.filter((surface) => surface.kind !== "preview");
            const existingBrowser = current.surfaces.filter(
              (surface): surface is Extract<RightPanelSurface, { kind: "preview" }> =>
                surface.kind === "preview" &&
                surface.id !== "browser:new" &&
                validIds.has(surface.id),
            );
            const knownIds = new Set(existingBrowser.map((surface) => surface.id));
            const added = tabIds
              .filter((tabId) => !knownIds.has(`browser:${tabId}`))
              .map((tabId) => browserSurface(tabId));
            const surfaces = [...nonBrowser, ...existingBrowser, ...added];
            const activeStillExists = surfaces.some(
              (surface) => surface.id === current.activeSurfaceId,
            );
            const fallbackBrowser = surfaces.find((surface) => surface.kind === "preview");
            return {
              ...current,
              surfaces,
              activeSurfaceId: activeStillExists
                ? current.activeSurfaceId
                : (fallbackBrowser?.id ?? surfaces[0]?.id ?? null),
            };
          }),
        })),
      reconcileFileSurfaces: (ref, workspaceAvailable) =>
        set((state) => ({
          byThreadKey: updateThread(state.byThreadKey, scopedThreadKey(ref), (current) => {
            if (workspaceAvailable) return current;
            const surfaces = current.surfaces.filter(
              (surface) => surface.kind !== "files" && surface.kind !== "file",
            );
            if (surfaces.length === current.surfaces.length) return current;
            const activeStillExists = surfaces.some(
              (surface) => surface.id === current.activeSurfaceId,
            );
            return {
              ...current,
              isOpen: surfaces.length > 0 ? current.isOpen : false,
              surfaces,
              activeSurfaceId: activeStillExists
                ? current.activeSurfaceId
                : (surfaces.at(-1)?.id ?? null),
            };
          }),
        })),
      // Retarget open file surfaces after a rename/move. Rewrites the exact `file:${old}` surface and,
      // for a directory rename, every `file:${old}/…` descendant to sit under the new path — keeping
      // each surface's reveal state and the active-surface selection. A `file:${old}` prefix WITHOUT
      // the trailing "/" boundary is deliberately NOT matched (so renaming "src" leaves "srcfoo/x").
      remapFileSurfaces: (ref, oldRelativePath, newRelativePath) =>
        set((state) => ({
          byThreadKey: updateThread(state.byThreadKey, scopedThreadKey(ref), (current) => {
            const oldId = `file:${oldRelativePath}`;
            const childPrefix = `${oldId}/`;
            const idRemap = new Map<string, string>();
            for (const surface of current.surfaces) {
              if (surface.kind !== "file") continue;
              if (surface.id === oldId) {
                idRemap.set(surface.id, `file:${newRelativePath}`);
              } else if (surface.id.startsWith(childPrefix)) {
                idRemap.set(
                  surface.id,
                  `file:${newRelativePath}/${surface.id.slice(childPrefix.length)}`,
                );
              }
            }
            if (idRemap.size === 0) return current;
            const seen = new Set<string>();
            const surfaces: RightPanelSurface[] = [];
            for (const surface of current.surfaces) {
              const nextId = surface.kind === "file" ? idRemap.get(surface.id) : undefined;
              const next: RightPanelSurface =
                nextId !== undefined && surface.kind === "file"
                  ? {
                      ...surface,
                      id: nextId as `file:${string}`,
                      relativePath: nextId.slice("file:".length),
                    }
                  : surface;
              if (seen.has(next.id)) continue;
              seen.add(next.id);
              surfaces.push(next);
            }
            const activeSurfaceId =
              current.activeSurfaceId !== null
                ? (idRemap.get(current.activeSurfaceId) ?? current.activeSurfaceId)
                : null;
            return { ...current, surfaces, activeSurfaceId };
          }),
        })),
      // Close the file surface at `relativePath` and, for a deleted directory, every descendant under
      // it (same trailing-slash boundary as remap). Falls the active selection back to a neighbor the
      // way closeSurface does, and closes the panel when nothing is left.
      closeFileSurfacesUnder: (ref, relativePath) =>
        set((state) => ({
          byThreadKey: updateThread(state.byThreadKey, scopedThreadKey(ref), (current) => {
            const targetId = `file:${relativePath}`;
            const childPrefix = `${targetId}/`;
            const isRemoved = (surface: RightPanelSurface): boolean =>
              surface.kind === "file" &&
              (surface.id === targetId || surface.id.startsWith(childPrefix));
            const activeIndex = current.surfaces.findIndex(
              (surface) => surface.id === current.activeSurfaceId,
            );
            const surfaces = current.surfaces.filter((surface) => !isRemoved(surface));
            if (surfaces.length === current.surfaces.length) return current;
            const activeRemoved =
              current.activeSurfaceId !== null &&
              !surfaces.some((surface) => surface.id === current.activeSurfaceId);
            const fallback = activeRemoved
              ? (surfaces[Math.min(activeIndex, surfaces.length - 1)] ?? null)
              : null;
            return {
              ...current,
              isOpen: surfaces.length > 0 && current.isOpen,
              surfaces,
              activeSurfaceId: activeRemoved ? (fallback?.id ?? null) : current.activeSurfaceId,
            };
          }),
        })),
      show: (ref) =>
        set((state) => ({
          byThreadKey: updateThread(state.byThreadKey, scopedThreadKey(ref), (current) =>
            current.isOpen ? current : { ...current, isOpen: true },
          ),
        })),
      close: (ref) =>
        set((state) => ({
          byThreadKey: updateThread(state.byThreadKey, scopedThreadKey(ref), (current) =>
            current.isOpen ? { ...current, isOpen: false } : current,
          ),
        })),
      toggleVisibility: (ref) =>
        set((state) => ({
          byThreadKey: updateThread(state.byThreadKey, scopedThreadKey(ref), (current) => ({
            ...current,
            isOpen: !current.isOpen,
          })),
        })),
      toggle: (ref, kind) =>
        set((state) => ({
          byThreadKey: updateThread(state.byThreadKey, scopedThreadKey(ref), (current) => {
            const active = current.surfaces.find(
              (surface) => surface.id === current.activeSurfaceId,
            );
            if (current.isOpen && active?.kind === kind) {
              return { ...current, isOpen: false };
            }
            if (kind === "preview") {
              const existing = current.surfaces.find((surface) => surface.kind === "preview");
              return upsertSurface(current, existing ?? browserSurface(null));
            }
            return upsertSurface(current, singletonSurface(kind));
          }),
        })),
      removeThread: (ref) =>
        set((state) => {
          const threadKey = scopedThreadKey(ref);
          if (!(threadKey in state.byThreadKey)) return state;
          const { [threadKey]: _removed, ...rest } = state.byThreadKey;
          return { byThreadKey: rest };
        }),
    }),
    {
      name: RIGHT_PANEL_STORAGE_KEY,
      version: RIGHT_PANEL_STORAGE_VERSION,
      storage: createJSONStorage(() =>
        resolveStorage(typeof window !== "undefined" ? window.localStorage : undefined),
      ),
      partialize: (state) => ({ byThreadKey: state.byThreadKey }),
      migrate: migratePersistedRightPanelState,
      merge: (persistedState, currentState) => {
        if (persistedState === undefined) return currentState;
        const sanitized = migratePersistedRightPanelState(persistedState);
        return { ...currentState, byThreadKey: sanitized.byThreadKey };
      },
    },
  ),
);

export function selectThreadRightPanelState(
  byThreadKey: Record<string, ThreadRightPanelState>,
  ref: ScopedThreadRef | null | undefined,
): ThreadRightPanelState {
  if (!ref) return EMPTY_THREAD_STATE;
  return byThreadKey[scopedThreadKey(ref)] ?? EMPTY_THREAD_STATE;
}

export function selectActiveRightPanel(
  byThreadKey: Record<string, ThreadRightPanelState>,
  ref: ScopedThreadRef | null | undefined,
): RightPanelKind | null {
  const state = selectThreadRightPanelState(byThreadKey, ref);
  if (!state.isOpen) return null;
  return state.surfaces.find((surface) => surface.id === state.activeSurfaceId)?.kind ?? null;
}

export function selectActiveRightPanelSurface(
  byThreadKey: Record<string, ThreadRightPanelState>,
  ref: ScopedThreadRef | null | undefined,
): RightPanelSurface | null {
  const state = selectThreadRightPanelState(byThreadKey, ref);
  if (!state.isOpen) return null;
  return state.surfaces.find((surface) => surface.id === state.activeSurfaceId) ?? null;
}
