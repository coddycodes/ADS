import type { EnvironmentId } from "@coddy/contracts";

import { createAtomCommandScheduler, type AtomCommandConcurrency } from "./runtime.ts";

export const vcsCommandScheduler = createAtomCommandScheduler();

export const vcsStatusRefreshScheduler = createAtomCommandScheduler();

/**
 * A separate scheduler instance for `generateCommitMessage`. It's a
 * read-only operation (no mutation of working tree/index), so it must not
 * queue behind — or block — stage/unstage/discard/commit on the same
 * `vcsCommandScheduler` lane. Scheduler state lives in a per-instance
 * WeakMap, so a distinct instance is sufficient to isolate the lane even
 * though the concurrency key (environmentId + cwd) is identical.
 */
export const vcsGenerateScheduler = createAtomCommandScheduler();

/**
 * Own lane for `vcs.cancelClone`: the clone it cancels holds the destination's serial lane on
 * `vcsCommandScheduler` for as long as it runs, so a cancel queued there would never go out.
 */
export const vcsCloneCancelScheduler = createAtomCommandScheduler();

export const vcsCommandConcurrency: AtomCommandConcurrency<{
  readonly environmentId: EnvironmentId;
  readonly input: { readonly cwd: string };
}> = {
  mode: "serial",
  key: ({ environmentId, input }) => JSON.stringify([environmentId, input.cwd]),
};

export const vcsStatusRefreshConcurrency: AtomCommandConcurrency<{
  readonly environmentId: EnvironmentId;
  readonly input: { readonly cwd: string };
}> = {
  mode: "latest",
  key: ({ environmentId, input }) => JSON.stringify([environmentId, input.cwd]),
};

// TODO(orca-port): `clone` has no pre-existing `cwd` (it targets `parentDir` + an optional
// `directoryName`, since the destination repo doesn't exist yet) — key concurrency on those
// instead, to serialize repeated clone attempts into the same destination.
export const vcsCloneCommandConcurrency: AtomCommandConcurrency<{
  readonly environmentId: EnvironmentId;
  readonly input: {
    readonly url: string;
    readonly parentDir: string;
    readonly directoryName?: string | undefined;
  };
}> = {
  mode: "serial",
  key: ({ environmentId, input }) =>
    JSON.stringify([environmentId, input.parentDir, input.directoryName ?? null]),
};
