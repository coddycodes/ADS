import { scopeThreadRef } from "@coddy/client-runtime/environment";
import type { EnvironmentThreadShell } from "@coddy/client-runtime/state/models";
import type { ScopedThreadRef } from "@coddy/contracts";

export interface BesideThreadCandidate {
  readonly ref: ScopedThreadRef;
  readonly title: string;
  readonly onBranch: boolean;
}

/** Threads to open a request beside: on its branch first, then the most recently active. */
export function rankBesideThreadCandidates(
  threads: ReadonlyArray<EnvironmentThreadShell>,
  headBranch: string,
): BesideThreadCandidate[] {
  return threads
    .filter((thread) => thread.archivedAt === null && thread.kind !== "panel")
    .map((thread) => ({ thread, onBranch: thread.branch === headBranch }))
    .toSorted(
      (left, right) =>
        Number(right.onBranch) - Number(left.onBranch) ||
        right.thread.updatedAt.localeCompare(left.thread.updatedAt),
    )
    .map(({ thread, onBranch }) => ({
      ref: scopeThreadRef(thread.environmentId, thread.id),
      title: thread.title,
      onBranch,
    }));
}
