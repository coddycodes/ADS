/** Host-thread-scoped, persisted center-panel surface and group layout state. */
import { parseScopedThreadKey, scopedThreadKey } from "@coddy/client-runtime/environment";
import {
  TERMINAL_LAUNCH_LABEL_MAX_LENGTH,
  type ScopedThreadRef,
  type TerminalLaunchCommand,
  type ThreadId,
} from "@coddy/contracts";
import { create } from "zustand";
import { createJSONStorage, persist } from "zustand/middleware";

import {
  CENTER_PANEL_ROOT_GROUP_ID,
  MAX_CENTER_PANEL_GROUPS,
  canDropCenterPanelSurface,
  createCenterPanelLayoutState,
  dropCenterPanelSurface,
  findCenterPanelGroup,
  insertCenterPanelSurface,
  mergeCenterPanelGroup,
  removeCenterPanelSurfaceIds,
  repairCenterPanelLayoutState,
  setCenterPanelSplitRatio,
  type CenterPanelDropRequest,
  type CenterPanelDropTarget,
  type CenterPanelGroup,
  type CenterPanelLayoutPath,
  type CenterPanelLayoutState,
} from "./centerPanelLayout";
import { decodePersistedTerminalLaunchCommand } from "./lib/terminalLaunchCommand";
import { resolveStorage } from "./lib/storage";
import { reserveTerminalId } from "./terminalIdReservations";

export const HOST_SURFACE_ID = "chat:host" as const;

export const CENTER_PANEL_KINDS = ["chat-host", "chat", "terminal"] as const;
export type CenterPanelKind = (typeof CENTER_PANEL_KINDS)[number];

export type CenterSurface =
  | { id: typeof HOST_SURFACE_ID; kind: "chat-host" }
  | { id: `chat:${string}`; kind: "chat"; threadId: ThreadId; providerLabel?: string }
  | {
      id: `terminal:${string}`;
      kind: "terminal";
      terminalId: string;
      label?: string;
      command?: TerminalLaunchCommand;
    };

export interface OpenTerminalPanelOptions {
  readonly label?: string;
  readonly command?: TerminalLaunchCommand;
}

export type CenterTerminalPlacement =
  | { readonly type: "tab"; readonly groupId: string }
  | {
      readonly type: "split";
      readonly groupId: string;
      readonly direction: "right" | "down";
    };

export type CenterTerminalPlacementValidation =
  | { readonly ok: true }
  | { readonly ok: false; readonly reason: "missing-group" | "pane-limit" };

export interface ThreadCenterPanelState extends CenterPanelLayoutState {
  readonly surfaces: readonly CenterSurface[];
}

interface CenterPanelStoreState {
  byThreadKey: Record<string, ThreadCenterPanelState>;
  readonly pendingChatPanelThreadKeys: ReadonlySet<string>;
  openChatPanel: (ref: ScopedThreadRef, threadId: ThreadId, providerLabel?: string) => void;
  reserveChatPanel: (ref: ScopedThreadRef, threadId: ThreadId, providerLabel?: string) => void;
  releaseChatPanelReservation: (ref: ScopedThreadRef) => void;
  validateTerminalPanelPlacement: (
    ref: ScopedThreadRef,
    placement: CenterTerminalPlacement,
  ) => CenterTerminalPlacementValidation;
  placeTerminalPanel: (
    ref: ScopedThreadRef,
    terminalId: string,
    placement: CenterTerminalPlacement,
    options?: OpenTerminalPanelOptions,
  ) => boolean;
  openTerminalPanel: (
    ref: ScopedThreadRef,
    terminalId: string,
    options?: OpenTerminalPanelOptions,
  ) => void;
  /** Adds a panel another client opened, as an inactive tab; no-op when already present. */
  adoptTerminalPanel: (
    ref: ScopedThreadRef,
    terminalId: string,
    options?: OpenTerminalPanelOptions,
  ) => void;
  adoptChatPanel: (ref: ScopedThreadRef, threadId: ThreadId, providerLabel?: string) => void;
  /** Replaces an existing terminal surface's launch command; never adds a surface. */
  syncTerminalPanelCommand: (
    ref: ScopedThreadRef,
    terminalId: string,
    command: TerminalLaunchCommand | undefined,
  ) => void;
  /** Drops a terminal surface whose session the server removed; the session needs no cleanup. */
  removeTerminalPanel: (ref: ScopedThreadRef, terminalId: string) => void;
  replaceMainWithTerminal: (
    ref: ScopedThreadRef,
    existingTerminalIds: ReadonlyArray<string>,
    options: OpenTerminalPanelOptions,
  ) => string;
  focusGroup: (ref: ScopedThreadRef, groupId: string) => void;
  activateSurface: (ref: ScopedThreadRef, groupId: string, surfaceId: string) => void;
  dropSurface: (ref: ScopedThreadRef, surfaceId: string, target: CenterPanelDropRequest) => boolean;
  mergeGroup: (ref: ScopedThreadRef, groupId: string) => boolean;
  setSplitRatio: (ref: ScopedThreadRef, path: CenterPanelLayoutPath, ratio: number) => void;
  closeSurface: (ref: ScopedThreadRef, groupId: string, surfaceId: string) => CenterSurface[];
  closeOtherSurfaces: (ref: ScopedThreadRef, groupId: string, surfaceId: string) => CenterSurface[];
  closeSurfacesToRight: (
    ref: ScopedThreadRef,
    groupId: string,
    surfaceId: string,
  ) => CenterSurface[];
  closeAllSurfaces: (ref: ScopedThreadRef, groupId: string) => CenterSurface[];
  removeThread: (ref: ScopedThreadRef) => void;
}

const HOST_SURFACE: CenterSurface = { id: HOST_SURFACE_ID, kind: "chat-host" };
const CENTER_PANEL_STORAGE_KEY = "coddy-studio:center-panel-state:v1";
const CENTER_PANEL_STORAGE_VERSION = 3;

const EMPTY_THREAD_STATE: ThreadCenterPanelState = {
  surfaces: [HOST_SURFACE],
  ...createCenterPanelLayoutState([HOST_SURFACE_ID], HOST_SURFACE_ID),
};

const chatSurface = (threadId: ThreadId, providerLabel?: string): CenterSurface => ({
  id: `chat:${threadId}`,
  kind: "chat",
  threadId,
  ...(providerLabel !== undefined ? { providerLabel } : {}),
});

const terminalSurface = (
  terminalId: string,
  options?: OpenTerminalPanelOptions,
): CenterSurface => ({
  id: `terminal:${terminalId}`,
  kind: "terminal",
  terminalId,
  ...(options?.label !== undefined ? { label: options.label } : {}),
  ...(options?.command !== undefined ? { command: options.command } : {}),
});

function boundedTrimmed(value: unknown, maxLength: number): string | undefined {
  if (typeof value !== "string") return undefined;
  const trimmed = value.trim();
  return trimmed.length > 0 && trimmed.length <= maxLength ? trimmed : undefined;
}

function sanitizeCommand(value: unknown): TerminalLaunchCommand | undefined {
  return decodePersistedTerminalLaunchCommand(value) ?? undefined;
}

function normalizeSurfaces(
  surfaces: readonly CenterSurface[],
  hostWasPresent: boolean,
): CenterSurface[] {
  const seen = new Set<string>();
  const rest: CenterSurface[] = [];
  for (const surface of surfaces) {
    if (surface.id === HOST_SURFACE_ID || seen.has(surface.id)) continue;
    seen.add(surface.id);
    rest.push(surface);
  }
  return hostWasPresent ? [HOST_SURFACE, ...rest] : rest;
}

function isImplicitDefault(state: ThreadCenterPanelState): boolean {
  return (
    state.surfaces.length === 1 &&
    state.surfaces[0]?.id === HOST_SURFACE_ID &&
    state.groups.length === 1 &&
    state.groups[0]?.id === CENTER_PANEL_ROOT_GROUP_ID &&
    state.groups[0]?.surfaceIds.length === 1 &&
    state.groups[0]?.surfaceIds[0] === HOST_SURFACE_ID &&
    state.groups[0]?.activeSurfaceId === HOST_SURFACE_ID &&
    state.layout.type === "leaf" &&
    state.layout.groupId === CENTER_PANEL_ROOT_GROUP_ID &&
    state.focusedGroupId === CENTER_PANEL_ROOT_GROUP_ID
  );
}

function updateThread(
  byThreadKey: Record<string, ThreadCenterPanelState>,
  threadKey: string,
  updater: (current: ThreadCenterPanelState) => ThreadCenterPanelState,
): Record<string, ThreadCenterPanelState> {
  const current = byThreadKey[threadKey] ?? EMPTY_THREAD_STATE;
  const next = updater(current);
  if (next === current) return byThreadKey;
  if (isImplicitDefault(next)) {
    if (!(threadKey in byThreadKey)) return byThreadKey;
    const { [threadKey]: _removed, ...rest } = byThreadKey;
    return rest;
  }
  return { ...byThreadKey, [threadKey]: next };
}

function withUpdatedThread(
  state: CenterPanelStoreState,
  ref: ScopedThreadRef,
  updater: (current: ThreadCenterPanelState) => ThreadCenterPanelState,
): CenterPanelStoreState | Pick<CenterPanelStoreState, "byThreadKey"> {
  const byThreadKey = updateThread(state.byThreadKey, scopedThreadKey(ref), updater);
  return byThreadKey === state.byThreadKey ? state : { byThreadKey };
}

function insertSurface(
  current: ThreadCenterPanelState,
  surface: CenterSurface,
): ThreadCenterPanelState {
  const mutation = insertCenterPanelSurface(current, surface.id, current.focusedGroupId);
  const surfaces = current.surfaces.some((entry) => entry.id === surface.id)
    ? current.surfaces
    : [...current.surfaces, surface];
  if (!mutation.changed && surfaces === current.surfaces) return current;
  return { ...mutation.state, surfaces };
}

function appendInactiveSurface(
  current: ThreadCenterPanelState,
  surface: CenterSurface,
): ThreadCenterPanelState {
  if (current.surfaces.some((entry) => entry.id === surface.id)) return current;
  const group = findCenterPanelGroup(current, current.focusedGroupId) ?? current.groups[0];
  if (!group) return current;
  return {
    ...current,
    surfaces: [...current.surfaces, surface],
    groups: current.groups.map((entry) =>
      entry.id === group.id
        ? {
            ...entry,
            surfaceIds: [...entry.surfaceIds, surface.id],
            activeSurfaceId: entry.activeSurfaceId ?? surface.id,
          }
        : entry,
    ),
  };
}

function validateTerminalPanelPlacement(
  current: ThreadCenterPanelState,
  placement: CenterTerminalPlacement,
): CenterTerminalPlacementValidation {
  if (!findCenterPanelGroup(current, placement.groupId)) {
    return { ok: false, reason: "missing-group" };
  }
  if (placement.type === "split" && current.groups.length >= MAX_CENTER_PANEL_GROUPS) {
    return { ok: false, reason: "pane-limit" };
  }
  return { ok: true };
}

function activateGroupSurface(
  current: ThreadCenterPanelState,
  groupId: string,
  surfaceId: string,
): ThreadCenterPanelState {
  const group = findCenterPanelGroup(current, groupId);
  if (!group || !group.surfaceIds.includes(surfaceId)) return current;
  if (group.activeSurfaceId === surfaceId && current.focusedGroupId === groupId) return current;
  return {
    ...current,
    groups: current.groups.map((entry) =>
      entry.id === groupId ? { ...entry, activeSurfaceId: surfaceId } : entry,
    ),
    focusedGroupId: groupId,
  };
}

function applySurfaceRemoval(
  current: ThreadCenterPanelState,
  requestedIds: ReadonlySet<string>,
): { readonly state: ThreadCenterPanelState; readonly removed: CenterSurface[] } {
  const mutation = removeCenterPanelSurfaceIds(current, requestedIds);
  if (!mutation.changed) return { state: current, removed: [] };
  const removedIdSet = new Set(mutation.removedSurfaceIds);
  return {
    state: {
      ...mutation.state,
      surfaces: current.surfaces.filter((surface) => !removedIdSet.has(surface.id)),
    },
    removed: current.surfaces.filter((surface) => removedIdSet.has(surface.id)),
  };
}

function sanitizeSurface(surface: unknown): CenterSurface[] {
  if (!surface || typeof surface !== "object") return [];
  const kind = (surface as { kind?: unknown }).kind;
  if (kind === "chat-host") return [];
  if (kind === "chat") {
    const threadId = (surface as { threadId?: unknown }).threadId;
    if (typeof threadId !== "string") return [];
    const providerLabel = (surface as { providerLabel?: unknown }).providerLabel;
    return [
      chatSurface(
        threadId as ThreadId,
        typeof providerLabel === "string" ? providerLabel : undefined,
      ),
    ];
  }
  if (kind === "terminal") {
    const candidate = surface as Record<string, unknown>;
    if (typeof candidate.terminalId !== "string") return [];
    const label = boundedTrimmed(candidate.label, TERMINAL_LAUNCH_LABEL_MAX_LENGTH);
    const command = sanitizeCommand(candidate.command);
    return [
      terminalSurface(candidate.terminalId, {
        ...(label ? { label } : {}),
        ...(command ? { command } : {}),
      }),
    ];
  }
  return [];
}

export function migratePersistedCenterPanelState(persistedState: unknown): {
  byThreadKey: Record<string, ThreadCenterPanelState>;
} {
  if (!persistedState || typeof persistedState !== "object") return { byThreadKey: {} };
  const raw =
    "byThreadKey" in persistedState &&
    persistedState.byThreadKey &&
    typeof persistedState.byThreadKey === "object"
      ? (persistedState.byThreadKey as Record<string, unknown>)
      : {};
  const byThreadKey: Record<string, ThreadCenterPanelState> = {};
  for (const [threadKey, value] of Object.entries(raw)) {
    const threadState =
      value && typeof value === "object" ? (value as Record<string, unknown>) : null;
    if (!Array.isArray(threadState?.surfaces)) continue;
    const rawSurfaces = threadState.surfaces;
    const hostWasPresent = rawSurfaces.some(
      (surface) =>
        surface !== null &&
        typeof surface === "object" &&
        (surface as { kind?: unknown }).kind === "chat-host",
    );
    const surfaces = normalizeSurfaces(
      rawSurfaces.flatMap<CenterSurface>(sanitizeSurface),
      hostWasPresent,
    );
    const legacyActiveSurfaceId =
      typeof threadState.activeSurfaceId === "string" ? threadState.activeSurfaceId : null;
    const layoutState = repairCenterPanelLayoutState(
      {
        groups: threadState.groups,
        layout: threadState.layout,
        focusedGroupId: threadState.focusedGroupId,
      },
      surfaces.map((surface) => surface.id),
      legacyActiveSurfaceId,
    );
    const state: ThreadCenterPanelState = { surfaces, ...layoutState };
    if (!isImplicitDefault(state)) byThreadKey[threadKey] = state;
  }
  return { byThreadKey };
}

export const useCenterPanelStore = create<CenterPanelStoreState>()(
  persist(
    (set, get) => ({
      byThreadKey: {},
      pendingChatPanelThreadKeys: new Set<string>(),
      openChatPanel: (ref, threadId, providerLabel) =>
        set((state) =>
          withUpdatedThread(state, ref, (current) =>
            insertSurface(current, chatSurface(threadId, providerLabel)),
          ),
        ),
      reserveChatPanel: (ref, threadId, providerLabel) =>
        set((state) => {
          const panelKey = scopedThreadKey({ environmentId: ref.environmentId, threadId });
          const updated = withUpdatedThread(state, ref, (current) =>
            insertSurface(current, chatSurface(threadId, providerLabel)),
          );
          if (
            updated.byThreadKey === state.byThreadKey &&
            state.pendingChatPanelThreadKeys.has(panelKey)
          ) {
            return state;
          }
          return {
            byThreadKey: updated.byThreadKey,
            pendingChatPanelThreadKeys: new Set(state.pendingChatPanelThreadKeys).add(panelKey),
          };
        }),
      releaseChatPanelReservation: (ref) =>
        set((state) => {
          const threadKey = scopedThreadKey(ref);
          if (!state.pendingChatPanelThreadKeys.has(threadKey)) return state;
          const pendingChatPanelThreadKeys = new Set(state.pendingChatPanelThreadKeys);
          pendingChatPanelThreadKeys.delete(threadKey);
          return { pendingChatPanelThreadKeys };
        }),
      validateTerminalPanelPlacement: (ref, placement) => {
        const current = get().byThreadKey[scopedThreadKey(ref)] ?? EMPTY_THREAD_STATE;
        return validateTerminalPanelPlacement(current, placement);
      },
      placeTerminalPanel: (ref, terminalId, placement, options) => {
        const surface = terminalSurface(terminalId, options);
        let changed = false;
        set((state) =>
          withUpdatedThread(state, ref, (current) => {
            if (!validateTerminalPanelPlacement(current, placement).ok) return current;
            const inserted = insertCenterPanelSurface(current, surface.id, placement.groupId);
            if (placement.type === "tab") {
              if (!inserted.changed) return current;
              changed = true;
              const surfaces = current.surfaces.some((entry) => entry.id === surface.id)
                ? current.surfaces
                : [...current.surfaces, surface];
              return { ...inserted.state, surfaces };
            }

            // @effect-diagnostics-next-line cryptoRandomUUID:off -- Group identifiers are generated at the impure Zustand store boundary.
            const newGroupId = `center-group:${crypto.randomUUID()}`;
            const mutation = dropCenterPanelSurface(inserted.state, surface.id, {
              groupId: placement.groupId,
              splitDirection: placement.direction,
              newGroupId,
            });
            if (!mutation.changed) return current;
            changed = true;
            const surfaces = current.surfaces.some((entry) => entry.id === surface.id)
              ? current.surfaces
              : [...current.surfaces, surface];
            return { ...mutation.state, surfaces };
          }),
        );
        return changed;
      },
      openTerminalPanel: (ref, terminalId, options) => {
        const current = get().byThreadKey[scopedThreadKey(ref)] ?? EMPTY_THREAD_STATE;
        get().placeTerminalPanel(
          ref,
          terminalId,
          { type: "tab", groupId: current.focusedGroupId },
          options,
        );
      },
      adoptTerminalPanel: (ref, terminalId, options) =>
        set((state) =>
          withUpdatedThread(state, ref, (current) =>
            appendInactiveSurface(current, terminalSurface(terminalId, options)),
          ),
        ),
      adoptChatPanel: (ref, threadId, providerLabel) =>
        set((state) =>
          withUpdatedThread(state, ref, (current) =>
            appendInactiveSurface(current, chatSurface(threadId, providerLabel)),
          ),
        ),
      syncTerminalPanelCommand: (ref, terminalId, command) => {
        const threadKey = scopedThreadKey(ref);
        const current = get().byThreadKey[threadKey];
        const surfaceId = `terminal:${terminalId}`;
        const surface = current?.surfaces.find((entry) => entry.id === surfaceId);
        // Checked before `set`: the persist middleware writes the whole layout on every call.
        if (
          !current ||
          surface?.kind !== "terminal" ||
          JSON.stringify(surface.command) === JSON.stringify(command)
        ) {
          return;
        }
        // A label taken from the previous command follows the new one; a custom label stays.
        const label = surface.label === surface.command?.label ? command?.label : surface.label;
        const next = terminalSurface(terminalId, {
          ...(label !== undefined ? { label } : {}),
          ...(command !== undefined ? { command } : {}),
        });
        set((state) => ({
          byThreadKey: {
            ...state.byThreadKey,
            [threadKey]: {
              ...current,
              surfaces: current.surfaces.map((entry) => (entry.id === surfaceId ? next : entry)),
            },
          },
        }));
      },
      removeTerminalPanel: (ref, terminalId) =>
        set((state) =>
          withUpdatedThread(
            state,
            ref,
            (current) => applySurfaceRemoval(current, new Set([`terminal:${terminalId}`])).state,
          ),
        ),
      replaceMainWithTerminal: (ref, existingTerminalIds, options) => {
        const threadKey = scopedThreadKey(ref);
        const current = get().byThreadKey[threadKey] ?? EMPTY_THREAD_STATE;
        const storedTerminalIds = current.surfaces.flatMap((surface) =>
          surface.kind === "terminal" ? [surface.terminalId] : [],
        );
        const reservation = reserveTerminalId(ref, [...existingTerminalIds, ...storedTerminalIds]);
        try {
          const surface = terminalSurface(reservation.terminalId, options);
          const layoutState = createCenterPanelLayoutState([surface.id], surface.id);
          set((state) => ({
            byThreadKey: {
              ...state.byThreadKey,
              [threadKey]: { surfaces: [surface], ...layoutState },
            },
          }));
          return reservation.terminalId;
        } finally {
          reservation.release();
        }
      },
      focusGroup: (ref, groupId) =>
        set((state) =>
          withUpdatedThread(state, ref, (current) => {
            if (!findCenterPanelGroup(current, groupId) || current.focusedGroupId === groupId) {
              return current;
            }
            return { ...current, focusedGroupId: groupId };
          }),
        ),
      activateSurface: (ref, groupId, surfaceId) =>
        set((state) =>
          withUpdatedThread(state, ref, (current) =>
            activateGroupSurface(current, groupId, surfaceId),
          ),
        ),
      dropSurface: (ref, surfaceId, target) => {
        let changed = false;
        set((state) =>
          withUpdatedThread(state, ref, (current) => {
            if (!canDropCenterPanelSurface(current, surfaceId, target)) return current;
            const completedTarget: CenterPanelDropTarget =
              "splitDirection" in target
                ? {
                    groupId: target.groupId,
                    splitDirection: target.splitDirection,
                    // @effect-diagnostics-next-line cryptoRandomUUID:off -- Group identifiers are generated at the impure Zustand store boundary.
                    newGroupId: `center-group:${crypto.randomUUID()}`,
                  }
                : target;
            const mutation = dropCenterPanelSurface(current, surfaceId, completedTarget);
            if (!mutation.changed) return current;
            changed = true;
            return { ...mutation.state, surfaces: current.surfaces };
          }),
        );
        return changed;
      },
      mergeGroup: (ref, groupId) => {
        let changed = false;
        set((state) =>
          withUpdatedThread(state, ref, (current) => {
            const mutation = mergeCenterPanelGroup(current, groupId);
            if (!mutation.changed) return current;
            changed = true;
            return { ...mutation.state, surfaces: current.surfaces };
          }),
        );
        return changed;
      },
      setSplitRatio: (ref, path, ratio) =>
        set((state) =>
          withUpdatedThread(state, ref, (current) => {
            const mutation = setCenterPanelSplitRatio(current, path, ratio);
            return mutation.changed ? { ...mutation.state, surfaces: current.surfaces } : current;
          }),
        ),
      closeSurface: (ref, groupId, surfaceId) => {
        let removed: CenterSurface[] = [];
        set((state) =>
          withUpdatedThread(state, ref, (current) => {
            const group = findCenterPanelGroup(current, groupId);
            if (!group || !group.surfaceIds.includes(surfaceId)) return current;
            const result = applySurfaceRemoval(current, new Set([surfaceId]));
            removed = result.removed;
            return result.state;
          }),
        );
        return removed;
      },
      closeOtherSurfaces: (ref, groupId, surfaceId) => {
        let removed: CenterSurface[] = [];
        set((state) =>
          withUpdatedThread(state, ref, (current) => {
            const group = findCenterPanelGroup(current, groupId);
            if (!group || !group.surfaceIds.includes(surfaceId)) return current;
            const requestedIds = new Set(
              group.surfaceIds.filter((id) => id !== surfaceId && id !== HOST_SURFACE_ID),
            );
            const result = applySurfaceRemoval(current, requestedIds);
            removed = result.removed;
            return result.state;
          }),
        );
        return removed;
      },
      closeSurfacesToRight: (ref, groupId, surfaceId) => {
        let removed: CenterSurface[] = [];
        set((state) =>
          withUpdatedThread(state, ref, (current) => {
            const group = findCenterPanelGroup(current, groupId);
            const index = group?.surfaceIds.indexOf(surfaceId) ?? -1;
            if (!group || index < 0) return current;
            const result = applySurfaceRemoval(current, new Set(group.surfaceIds.slice(index + 1)));
            removed = result.removed;
            return result.state;
          }),
        );
        return removed;
      },
      closeAllSurfaces: (ref, groupId) => {
        let removed: CenterSurface[] = [];
        set((state) =>
          withUpdatedThread(state, ref, (current) => {
            const group = findCenterPanelGroup(current, groupId);
            if (!group) return current;
            const result = applySurfaceRemoval(current, new Set(group.surfaceIds));
            removed = result.removed;
            return result.state;
          }),
        );
        return removed;
      },
      removeThread: (ref) =>
        set((state) => {
          const threadKey = scopedThreadKey(ref);
          let byThreadKey = state.byThreadKey;
          const pendingKeysToRemove = new Set([threadKey]);
          for (const surface of state.byThreadKey[threadKey]?.surfaces ?? []) {
            if (surface.kind === "chat") {
              pendingKeysToRemove.add(
                scopedThreadKey({ environmentId: ref.environmentId, threadId: surface.threadId }),
              );
            }
          }
          if (threadKey in byThreadKey) {
            const { [threadKey]: _removed, ...rest } = byThreadKey;
            byThreadKey = rest;
          }
          for (const [hostThreadKey, hostState] of Object.entries(state.byThreadKey)) {
            if (hostThreadKey === threadKey) continue;
            const hostRef = parseScopedThreadKey(hostThreadKey);
            if (!hostRef || hostRef.environmentId !== ref.environmentId) continue;
            const referencedSurfaceIds = new Set(
              hostState.surfaces.flatMap((surface) =>
                surface.kind === "chat" && surface.threadId === ref.threadId ? [surface.id] : [],
              ),
            );
            if (referencedSurfaceIds.size === 0) continue;
            byThreadKey = updateThread(
              byThreadKey,
              hostThreadKey,
              (current) => applySurfaceRemoval(current, referencedSurfaceIds).state,
            );
          }
          const pendingChatPanelThreadKeys = new Set(
            [...state.pendingChatPanelThreadKeys].filter((key) => !pendingKeysToRemove.has(key)),
          );
          if (
            byThreadKey === state.byThreadKey &&
            pendingChatPanelThreadKeys.size === state.pendingChatPanelThreadKeys.size
          ) {
            return state;
          }
          return { byThreadKey, pendingChatPanelThreadKeys };
        }),
    }),
    {
      name: CENTER_PANEL_STORAGE_KEY,
      version: CENTER_PANEL_STORAGE_VERSION,
      storage: createJSONStorage(() =>
        resolveStorage(typeof window !== "undefined" ? window.localStorage : undefined),
      ),
      partialize: (state) => ({ byThreadKey: state.byThreadKey }),
      migrate: migratePersistedCenterPanelState,
    },
  ),
);

export function selectThreadCenterPanelState(
  byThreadKey: Record<string, ThreadCenterPanelState>,
  ref: ScopedThreadRef | null | undefined,
): ThreadCenterPanelState {
  return ref ? (byThreadKey[scopedThreadKey(ref)] ?? EMPTY_THREAD_STATE) : EMPTY_THREAD_STATE;
}

export interface VisibleCenterSurface {
  readonly groupId: string;
  readonly surface: CenterSurface;
  readonly focused: boolean;
}

export function selectFocusedCenterPanelGroup(state: ThreadCenterPanelState): CenterPanelGroup {
  return (
    findCenterPanelGroup(state, state.focusedGroupId) ??
    state.groups[0] ??
    EMPTY_THREAD_STATE.groups[0]!
  );
}

export function selectFocusedCenterSurface(state: ThreadCenterPanelState): CenterSurface | null {
  const group = selectFocusedCenterPanelGroup(state);
  return state.surfaces.find((surface) => surface.id === group.activeSurfaceId) ?? null;
}

export function selectVisibleCenterSurfaces(state: ThreadCenterPanelState): VisibleCenterSurface[] {
  const surfacesById = new Map<string, CenterSurface>(
    state.surfaces.map((surface) => [surface.id, surface]),
  );
  return state.groups.flatMap((group) => {
    const surface =
      group.activeSurfaceId === null ? undefined : surfacesById.get(group.activeSurfaceId);
    return surface
      ? [{ groupId: group.id, surface, focused: group.id === state.focusedGroupId }]
      : [];
  });
}

/** Compatibility wrapper until all callers select the focused surface directly. */
export function selectActiveCenterSurface(
  byThreadKey: Record<string, ThreadCenterPanelState>,
  ref: ScopedThreadRef | null | undefined,
): CenterSurface | null {
  return selectFocusedCenterSurface(selectThreadCenterPanelState(byThreadKey, ref));
}
