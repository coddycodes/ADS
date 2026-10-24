import { WS_METHODS, type EnvironmentId } from "@bibcode/contracts";
import * as Effect from "effect/Effect";
import * as Stream from "effect/Stream";
import { Atom } from "effect/reactivity";

import type { EnvironmentRegistry } from "../connection/registry.ts";
import { ConnectionWakeups } from "../connection/wakeups.ts";
import {
  createEnvironmentRpcCommand,
  createEnvironmentQueryAtomFamily,
  createEnvironmentRpcQueryAtomFamily,
  createEnvironmentRpcStreamCommand,
  createEnvironmentRpcSubscriptionAtomFamily,
  environmentRpcKey,
  forwardEnvironmentQueryRetry,
  parseEnvironmentRpcKey,
} from "./runtime.ts";
import { request } from "../rpc/client.ts";
import { createSignalWithDegradedFocusRefresh } from "./gitManagerRefresh.ts";
import { vcsCommandConcurrency, vcsCommandScheduler } from "./vcsCommandScheduler.ts";

export function createGitManagerEnvironmentAtoms<R, E>(
  runtime: Atom.AtomRuntime<EnvironmentRegistry | ConnectionWakeups | R, E>,
) {
  const signal = createEnvironmentRpcSubscriptionAtomFamily(runtime, {
    label: "environment-data:git-manager:signal",
    tag: WS_METHODS.subscribeGitManagerSignal,
    idleTtlMs: 0,
  });
  const focusVisibility = runtime
    .atom(
      Stream.unwrap(ConnectionWakeups.pipe(Effect.map((wakeups) => wakeups.focusVisibility))).pipe(
        Stream.zipWithIndex,
        Stream.map(([, index]) => index),
      ),
    )
    .pipe(Atom.setIdleTTL(0), Atom.withLabel("git-manager:focus-visibility"));
  const focusRefresh = Atom.family((key: string) =>
    Atom.make(0).pipe(Atom.setIdleTTL(0), Atom.withLabel(`git-manager:focus:${key}`)),
  );
  const refreshSignal = Atom.family((key: string) =>
    createSignalWithDegradedFocusRefresh(
      signal(parseEnvironmentRpcKey<{ readonly cwd: string }>(key)),
      focusVisibility,
      focusRefresh(key),
    ),
  );
  const refreshOnDegradedFocus = <Input extends { readonly cwd: string }, A>(
    query: (target: {
      readonly environmentId: EnvironmentId;
      readonly input: Input;
    }) => Atom.Atom<A>,
  ) => {
    const family = Atom.family((key: string) => {
      const target = parseEnvironmentRpcKey<Input>(key);
      const scopeKey = environmentRpcKey({
        environmentId: target.environmentId,
        input: { cwd: target.input.cwd },
      });
      const source = query(target);
      return forwardEnvironmentQueryRetry(
        source,
        source.pipe(Atom.makeRefreshOnSignal(focusRefresh(scopeKey)), Atom.setIdleTTL(0)),
      );
    });
    return (target: Parameters<typeof query>[0]) => family(environmentRpcKey(target));
  };
  return {
    getRefs: refreshOnDegradedFocus(
      createEnvironmentRpcQueryAtomFamily(runtime, {
        label: "environment-data:git-manager:get-refs",
        tag: WS_METHODS.gitManagerGetRefs,
        staleTimeMs: 5_000,
      }),
    ),
    getCommits: refreshOnDegradedFocus(
      createEnvironmentRpcQueryAtomFamily(runtime, {
        label: "environment-data:git-manager:get-commits",
        tag: WS_METHODS.gitManagerGetCommits,
        staleTimeMs: 10_000,
      }),
    ),
    getHistoryFirstPage: refreshOnDegradedFocus(
      createEnvironmentQueryAtomFamily(runtime, {
        label: "environment-data:git-manager:history-first-page",
        staleTimeMs: 10_000,
        idleTtlMs: 0,
        transportCutoffKey: (input) => ({ cwd: input.cwd, limit: input.limit }),
        execute: (input: {
          readonly cwd: string;
          readonly limit: number;
          /** Local query-cache identity, unique per read and never reused; never sent over RPC. */
          readonly refreshCacheKey: string;
        }) => request(WS_METHODS.gitManagerGetCommits, { cwd: input.cwd, limit: input.limit }),
      }),
    ),
    getRetainedCommitPages: createEnvironmentQueryAtomFamily(runtime, {
      label: "environment-data:git-manager:retained-history",
      staleTimeMs: 10_000,
      idleTtlMs: 0,
      transportCutoffKey: (input) => ({
        cwd: input.cwd,
        pinnedTips: input.pinnedTips,
        offsets: input.offsets,
        limit: input.limit,
      }),
      execute: (input: {
        readonly cwd: string;
        readonly pinnedTips: ReadonlyArray<string>;
        readonly offsets: ReadonlyArray<number>;
        readonly limit: number;
        /** Local query-cache identity, unique per read and never reused; never sent over RPC. */
        readonly refreshCacheKey: string;
      }) =>
        Effect.forEach(
          input.offsets,
          (offset) =>
            request(WS_METHODS.gitManagerGetCommits, {
              cwd: input.cwd,
              pinnedTips: input.pinnedTips,
              offset,
              limit: input.limit,
            }),
          { concurrency: 2 },
        ),
    }),
    getDiff: createEnvironmentRpcQueryAtomFamily(runtime, {
      label: "environment-data:git-manager:get-diff",
      tag: WS_METHODS.gitManagerGetDiff,
      staleTimeMs: 5_000,
    }),
    // The Stashes panel reads the list only while it is open. Dropping the query
    // when it closes keeps a reconnect from re-reading a list nobody shows.
    getStashes: refreshOnDegradedFocus(
      createEnvironmentRpcQueryAtomFamily(runtime, {
        label: "environment-data:git-manager:get-stashes",
        tag: WS_METHODS.gitManagerGetStashes,
        staleTimeMs: 5_000,
        idleTtlMs: 0,
      }),
    ),
    getRemoteTags: createEnvironmentRpcQueryAtomFamily(runtime, {
      label: "environment-data:git-manager:get-remote-tags",
      tag: WS_METHODS.gitManagerGetRemoteTags,
      staleTimeMs: 30_000,
    }),
    previewMerge: createEnvironmentRpcQueryAtomFamily(runtime, {
      label: "environment-data:git-manager:preview-merge",
      tag: WS_METHODS.gitManagerPreviewMerge,
      staleTimeMs: 5_000,
    }),
    listPullRequests: createEnvironmentRpcQueryAtomFamily(runtime, {
      label: "environment-data:git-manager:list-pull-requests",
      tag: WS_METHODS.gitManagerListPullRequests,
      staleTimeMs: 5_000,
    }),
    signal,
    signalWithDegradedFocusRefresh: (target: Parameters<typeof signal>[0]) =>
      refreshSignal(environmentRpcKey(target)),
    commit: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:git-manager:commit",
      tag: WS_METHODS.gitManagerCommit,
      scheduler: vcsCommandScheduler,
      concurrency: vcsCommandConcurrency,
    }),
    undoCommit: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:git-manager:undo-commit",
      tag: WS_METHODS.gitManagerUndoCommit,
      scheduler: vcsCommandScheduler,
      concurrency: vcsCommandConcurrency,
    }),
    discard: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:git-manager:discard",
      tag: WS_METHODS.gitManagerDiscard,
      scheduler: vcsCommandScheduler,
      concurrency: vcsCommandConcurrency,
    }),
    stagePartial: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:git-manager:stage-partial",
      tag: WS_METHODS.gitManagerStagePartial,
      scheduler: vcsCommandScheduler,
      concurrency: vcsCommandConcurrency,
    }),
    unstagePartial: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:git-manager:unstage-partial",
      tag: WS_METHODS.gitManagerUnstagePartial,
      scheduler: vcsCommandScheduler,
      concurrency: vcsCommandConcurrency,
    }),
    discardPartial: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:git-manager:discard-partial",
      tag: WS_METHODS.gitManagerDiscardPartial,
      scheduler: vcsCommandScheduler,
      concurrency: vcsCommandConcurrency,
    }),
    runOperation: createEnvironmentRpcStreamCommand(runtime, {
      label: "environment-data:git-manager:run-operation",
      tag: WS_METHODS.gitManagerRunOperation,
      scheduler: vcsCommandScheduler,
      concurrency: vcsCommandConcurrency,
    }),
  };
}
