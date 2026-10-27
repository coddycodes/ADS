import type { GitManagerCommitEntry } from "@coddy/contracts";
import { LRUCache } from "../../../lib/lruCache";

interface CommitIdentity {
  readonly sha: string;
}

interface SpliceCommitGenerationInput<T extends CommitIdentity> {
  readonly loaded: ReadonlyArray<T>;
  readonly incoming: ReadonlyArray<T>;
  readonly pinnedTips: ReadonlyArray<string>;
}

interface SpliceCommitGenerationResult<T extends CommitIdentity> {
  readonly commits: ReadonlyArray<T>;
  readonly requiresReset: boolean;
}

export function spliceCommitGeneration<T extends CommitIdentity>({
  loaded,
  incoming,
  pinnedTips,
}: SpliceCommitGenerationInput<T>): SpliceCommitGenerationResult<T> {
  const loadedShas = new Set(loaded.map((commit) => commit.sha));
  const incomingAdditions = incoming.filter((commit) => {
    if (loadedShas.has(commit.sha)) return false;
    loadedShas.add(commit.sha);
    return true;
  });
  const incomingBySha = new Map(incoming.map((commit) => [commit.sha, commit]));
  const incomingShas = new Set(incomingBySha.keys());
  return {
    commits: [
      ...incomingAdditions,
      ...loaded.map((commit) => incomingBySha.get(commit.sha) ?? commit),
    ],
    requiresReset: pinnedTips.length > 0 && !pinnedTips.some((tipSha) => incomingShas.has(tipSha)),
  };
}

/** Replace only server-owned decorations while retaining pinned rows and order. */
export function mergeCommitDecorations(
  loaded: ReadonlyArray<GitManagerCommitEntry>,
  refreshed: ReadonlyArray<GitManagerCommitEntry>,
): ReadonlyArray<GitManagerCommitEntry> {
  const bySha = new Map(refreshed.map((commit) => [commit.sha, commit.decorations]));
  let changed = false;
  const result = loaded.map((commit) => {
    const decorations = bySha.get(commit.sha);
    if (
      decorations === undefined ||
      (decorations.length === commit.decorations.length &&
        decorations.every((value, index) => value === commit.decorations[index]))
    )
      return commit;
    changed = true;
    return { ...commit, decorations };
  });
  return changed ? result : loaded;
}

/** The next page loads once the rendered end is this many rows from the last row. */
export const NEXT_PAGE_ROW_THRESHOLD = 10;
/** Minimum interval between two next-page requests. */
export const NEXT_PAGE_REQUEST_INTERVAL_MS = 500;

interface ShouldLoadNextPageInput {
  readonly renderedIndex: number;
  readonly totalRows: number;
  readonly isLoading: boolean;
  readonly lastRequestAtMs: number;
  readonly nowMs: number;
}

export function shouldLoadNextPage({
  renderedIndex,
  totalRows,
  isLoading,
  lastRequestAtMs,
  nowMs,
}: ShouldLoadNextPageInput): boolean {
  return (
    totalRows > 0 &&
    totalRows - renderedIndex <= NEXT_PAGE_ROW_THRESHOLD &&
    !isLoading &&
    nowMs - lastRequestAtMs >= NEXT_PAGE_REQUEST_INTERVAL_MS
  );
}

export interface CommitLookup<T extends CommitIdentity> {
  readonly get: (sha: string) => T | null;
  readonly set: (commit: T) => void;
  readonly clear: () => void;
}

function approximateCommitSize(commit: CommitIdentity): number {
  return Math.max(1, JSON.stringify(commit).length * 2);
}

export function createCommitLookup<T extends CommitIdentity>(
  maxEntries: number,
  maxMemoryBytes: number,
): CommitLookup<T> {
  const cache = new LRUCache<T>(maxEntries, maxMemoryBytes);
  return {
    get: (sha) => cache.get(sha),
    set: (commit) => cache.set(commit.sha, commit, approximateCommitSize(commit)),
    clear: () => cache.clear(),
  };
}
