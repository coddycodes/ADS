import type { GitManagerRefEntry } from "@coddy/contracts";

const RECENT_BRANCH_LIMIT = 5;

export interface BranchGroups {
  readonly default: ReadonlyArray<GitManagerRefEntry>;
  readonly recent: ReadonlyArray<GitManagerRefEntry>;
  readonly other: ReadonlyArray<GitManagerRefEntry>;
  readonly remote: ReadonlyArray<GitManagerRefEntry>;
}

export function groupBranches(input: {
  readonly refs: ReadonlyArray<GitManagerRefEntry>;
  readonly remoteRefs?: ReadonlyArray<GitManagerRefEntry>;
  readonly recentNames: ReadonlyArray<string>;
  readonly filter: string;
}): BranchGroups {
  const query = input.filter.trim().toLocaleLowerCase();
  const filterRefs = (refs: ReadonlyArray<GitManagerRefEntry>) =>
    query.length === 0 ? refs : refs.filter((ref) => ref.name.toLocaleLowerCase().includes(query));
  const visible = filterRefs(input.refs);
  const byName = new Map(visible.map((ref) => [ref.name, ref]));
  const defaultBranches = visible.filter((ref) => ref.isDefault);
  const assigned = new Set(defaultBranches.map((ref) => ref.name));
  const recentBranches: GitManagerRefEntry[] = [];

  for (const name of input.recentNames) {
    if (recentBranches.length >= RECENT_BRANCH_LIMIT) break;
    if (assigned.has(name)) continue;
    const ref = byName.get(name);
    if (ref === undefined) continue;
    assigned.add(name);
    recentBranches.push(ref);
  }

  return {
    default: defaultBranches,
    recent: recentBranches,
    other: visible.filter((ref) => !assigned.has(ref.name)),
    remote: filterRefs(input.remoteRefs ?? []),
  };
}
