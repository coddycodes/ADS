import type {
  CompatVerdict,
  ConnectionTarget,
  EnvironmentConnectionPhase,
} from "@coddy/client-runtime/connection";
import type { EnvironmentId } from "@coddy/contracts";

import { isDesktopLocalConnectionTarget } from "../../connection/desktopLocal";
import { compareSidebarDisplayText } from "../../sidebarProjectGrouping";

/** The rail's width. The thread sidebar header reserves it when the rail is hidden. */
export const ENVIRONMENT_RAIL_WIDTH_PX = 52;

export type EnvironmentRailStatus = "connected" | "disconnected" | "attention" | "error";

export interface EnvironmentRailCandidate {
  readonly environmentId: EnvironmentId;
  readonly label: string;
  readonly isPrimary: boolean;
  readonly isDesktopLocal: boolean;
  readonly phase: EnvironmentConnectionPhase;
  readonly compat: CompatVerdict | null;
  readonly updateAvailable: boolean;
}

export function toEnvironmentRailCandidate(input: {
  readonly environmentId: EnvironmentId;
  readonly label: string;
  readonly target: ConnectionTarget;
  readonly phase: EnvironmentConnectionPhase;
  readonly compat: CompatVerdict | null;
  readonly updateAvailable: boolean;
}): EnvironmentRailCandidate {
  return {
    environmentId: input.environmentId,
    label: input.label,
    isPrimary: input.target._tag === "PrimaryConnectionTarget",
    isDesktopLocal: isDesktopLocalConnectionTarget(input.target),
    phase: input.phase,
    compat: input.compat,
    updateAvailable: input.updateAvailable,
  };
}

export function isLocalRailCandidate(
  candidate: Pick<EnvironmentRailCandidate, "isPrimary" | "isDesktopLocal">,
): boolean {
  return candidate.isPrimary || candidate.isDesktopLocal;
}

export function resolveEnvironmentRailStatus(
  input: Pick<EnvironmentRailCandidate, "phase" | "compat" | "updateAvailable">,
): EnvironmentRailStatus {
  if (input.phase === "error") {
    return "error";
  }
  if (input.phase !== "connected") {
    return "disconnected";
  }
  if (
    input.compat !== null &&
    (input.compat.kind === "server-too-old" || input.compat.kind === "client-too-old")
  ) {
    return "error";
  }
  if (input.updateAvailable || input.compat?.kind === "legacy") {
    return "attention";
  }
  return "connected";
}

export function environmentLetterAvatar(label: string): string {
  const words = label
    .trim()
    .split(/[\s_-]+/)
    .filter((word) => word.length > 0);
  const first = words[0];
  if (first === undefined) {
    return "?";
  }
  const second = words[1];
  return second === undefined
    ? first.slice(0, 2).toUpperCase()
    : `${first[0] ?? ""}${second[0] ?? ""}`.toUpperCase();
}

export interface EnvironmentRailEntry {
  readonly environmentId: EnvironmentId;
  readonly label: string;
  readonly avatar: string;
  readonly status: EnvironmentRailStatus;
  readonly selected: boolean;
}

export interface EnvironmentRailModel {
  readonly localSelected: boolean;
  readonly localStatus: EnvironmentRailStatus;
  readonly localSubEntries: ReadonlyArray<EnvironmentRailEntry>;
  readonly localTargetEnvironmentId: EnvironmentId | null;
  readonly remotes: ReadonlyArray<EnvironmentRailEntry>;
}

export function buildEnvironmentRailModel(input: {
  readonly candidates: ReadonlyArray<EnvironmentRailCandidate>;
  readonly activeEnvironmentId: EnvironmentId | null;
}): EnvironmentRailModel {
  const locals = input.candidates.filter(isLocalRailCandidate);
  const primary = locals.find((local) => local.isPrimary) ?? null;
  const desktopLocals = locals.filter((local) => local.isDesktopLocal);
  const localIds = new Set(locals.map((local) => local.environmentId));
  const localSelected =
    input.activeEnvironmentId === null || localIds.has(input.activeEnvironmentId);

  const toEntry = (candidate: EnvironmentRailCandidate): EnvironmentRailEntry => ({
    environmentId: candidate.environmentId,
    label: candidate.label,
    avatar: environmentLetterAvatar(candidate.label),
    status: resolveEnvironmentRailStatus(candidate),
    selected: candidate.environmentId === input.activeEnvironmentId,
  });

  return {
    localSelected,
    localStatus: primary === null ? "disconnected" : resolveEnvironmentRailStatus(primary),
    localSubEntries:
      desktopLocals.length === 0
        ? []
        : [
            ...(primary === null ? [] : [{ ...toEntry(primary), label: "This device" }]),
            ...desktopLocals.map(toEntry),
          ],
    localTargetEnvironmentId: primary?.environmentId ?? null,
    remotes: input.candidates
      .filter((candidate) => !isLocalRailCandidate(candidate))
      .map(toEntry)
      .sort((left, right) => compareSidebarDisplayText(left.label, right.label)),
  };
}

export interface RailEnvironmentScopeCandidate {
  readonly environmentId: EnvironmentId;
  readonly isLocal: boolean;
}

/**
 * Returns the panel's presentation scope. Null and stale selections resolve to
 * Local; `null` (no filtering) is reserved for the transient remote-only
 * catalog that has no local environment to select.
 */
export function selectRailVisibleEnvironmentIds(input: {
  readonly candidates: ReadonlyArray<RailEnvironmentScopeCandidate>;
  readonly activeEnvironmentId: EnvironmentId | null;
}): ReadonlySet<EnvironmentId> | null {
  const localIds = input.candidates
    .filter((candidate) => candidate.isLocal)
    .map((candidate) => candidate.environmentId);
  const localScope = (): ReadonlySet<EnvironmentId> | null =>
    localIds.length === 0 ? null : new Set(localIds);

  if (input.activeEnvironmentId === null) {
    return localScope();
  }
  const active = input.candidates.find(
    (candidate) => candidate.environmentId === input.activeEnvironmentId,
  );
  if (active === undefined || active.isLocal) {
    return localScope();
  }
  return new Set([input.activeEnvironmentId]);
}

export function resolveAddProjectTargetLabel(input: {
  readonly candidates: ReadonlyArray<RailEnvironmentScopeCandidate & { readonly label: string }>;
  readonly activeEnvironmentId: EnvironmentId | null;
}): string | null {
  if (input.activeEnvironmentId === null) {
    return null;
  }
  const active = input.candidates.find(
    (candidate) => candidate.environmentId === input.activeEnvironmentId,
  );
  if (active === undefined || active.isLocal) {
    return null;
  }
  return active.label;
}
