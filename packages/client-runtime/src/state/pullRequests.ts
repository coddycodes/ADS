import {
  WS_METHODS,
  type EnvironmentId,
  type PullRequestsContext,
  type PullRequestsGetContextInput,
  type PullRequestsListInput,
} from "@bibcode/contracts";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";
import { Atom } from "effect/reactivity";
import { RpcClientError } from "effect/rpc";

import type { EnvironmentRegistry } from "../connection/registry.ts";
import { EnvironmentSupervisor } from "../connection/supervisor.ts";
import { EnvironmentRpcUnavailableError, request } from "../rpc/client.ts";
import {
  createEnvironmentQueryAtomFamily,
  createEnvironmentRpcCommand,
  createEnvironmentRpcQueryAtomFamily,
  createEnvironmentRpcSubscriptionAtomFamily,
  environmentRpcKey,
} from "./runtime.ts";
import { vcsCommandConcurrency, vcsCommandScheduler } from "./vcsCommandScheduler.ts";

const isRpcClientError = Schema.is(RpcClientError.RpcClientError);
const isEnvironmentRpcUnavailableError = Schema.is(EnvironmentRpcUnavailableError);

/** Success or the server's typed failure; not an interruption, a lost transport or a defect. */
function answered<A, E>(exit: Exit.Exit<A, E>): boolean {
  if (Exit.isSuccess(exit)) return true;
  if (Exit.hasInterrupts(exit)) return false;
  const error = Exit.findErrorOption(exit);
  return (
    Option.isSome(error) &&
    !isRpcClientError(error.value) &&
    !isEnvironmentRpcUnavailableError(error.value)
  );
}

/**
 * One-shot bypass requests for a query: `request` marks one environment and input,
 * and executions of that exact query send `field: true` until one is answered. An
 * interrupted read or a lost connection leaves the request for the next execution.
 * The query key never changes, so the current value stays visible meanwhile.
 */
export function createOneShotBypass<Field extends string>(field: Field) {
  const pending = new Set<string>();
  return {
    request(target: { readonly environmentId: EnvironmentId; readonly input: object }): void {
      pending.add(environmentRpcKey(target));
    },
    run<Input extends object & Partial<Record<Field, boolean | undefined>>, A, E, R>(
      environmentId: EnvironmentId,
      input: Input,
      send: (input: Input) => Effect.Effect<A, E, R>,
    ): Effect.Effect<A, E, R> {
      const key = environmentRpcKey({ environmentId, input });
      if (!pending.has(key)) return send(input);
      // The wire input declares the bypass field as an optional boolean.
      return send({ ...input, [field]: true }).pipe(
        Effect.onExit((exit) =>
          Effect.sync(() => {
            if (answered(exit)) pending.delete(key);
          }),
        ),
      );
    },
  };
}

/**
 * A context's first read may answer from the server's stored copy (`stale`); the
 * next read of that input, which the panel starts at once, goes to the host.
 */
export function createStoredContextFirst() {
  const staleAnswered = new Set<string>();
  return {
    run<E, R>(
      environmentId: EnvironmentId,
      input: object,
      send: (allowStale: boolean) => Effect.Effect<PullRequestsContext, E, R>,
    ): Effect.Effect<PullRequestsContext, E, R> {
      const key = environmentRpcKey({ environmentId, input });
      return send(!staleAnswered.has(key)).pipe(
        // Only a stale answer is remembered. After any other outcome, including a
        // failed live read, the next read may paint the stored copy again.
        Effect.onExit((exit) =>
          Effect.sync(() => {
            if (
              Exit.isSuccess(exit) &&
              exit.value.status === "available" &&
              exit.value.stale === true
            ) {
              staleAnswered.add(key);
            } else {
              staleAnswered.delete(key);
            }
          }),
        ),
      );
    },
  };
}

export function createPullRequestsEnvironmentAtoms<R, E>(
  runtime: Atom.AtomRuntime<EnvironmentRegistry | R, E>,
) {
  // Only Rescan and auth recovery bypass the server's bounded context caches.
  const contextRescans = createOneShotBypass("rescan");
  const storedContexts = createStoredContextFirst();
  // Only an explicit list Refresh re-reads the repository-wide tab totals.
  const totalsRefreshes = createOneShotBypass("refreshTotals");
  return {
    getContext: createEnvironmentQueryAtomFamily(runtime, {
      label: "environment-data:pull-requests:get-context",
      staleTimeMs: 60_000,
      execute: (input: PullRequestsGetContextInput) =>
        EnvironmentSupervisor.pipe(
          Effect.flatMap((supervisor) => {
            const environmentId = supervisor.target.environmentId;
            return storedContexts.run(environmentId, input, (allowStale) =>
              contextRescans.run(environmentId, input, (payload) =>
                request(
                  WS_METHODS.pullRequestsGetContext,
                  allowStale ? { ...payload, allowStale: true } : payload,
                ),
              ),
            );
          }),
        ),
    }),
    /** The next `getContext` read of this input bypasses the server's context caches. */
    requestContextRescan: contextRescans.request,
    getVocabulary: createEnvironmentRpcQueryAtomFamily(runtime, {
      label: "environment-data:pull-requests:get-vocabulary",
      tag: WS_METHODS.pullRequestsGetVocabulary,
      staleTimeMs: 60_000,
    }),
    getCreateDefaults: createEnvironmentRpcQueryAtomFamily(runtime, {
      label: "environment-data:pull-requests:get-create-defaults",
      tag: WS_METHODS.pullRequestsGetCreateDefaults,
      staleTimeMs: 60_000,
    }),
    list: createEnvironmentQueryAtomFamily(runtime, {
      label: "environment-data:pull-requests:list",
      staleTimeMs: 5_000,
      execute: (input: PullRequestsListInput) =>
        EnvironmentSupervisor.pipe(
          Effect.flatMap((supervisor) =>
            totalsRefreshes.run(supervisor.target.environmentId, input, (payload) =>
              request(WS_METHODS.pullRequestsList, payload),
            ),
          ),
        ),
    }),
    /** The next `list` read of this input re-reads the repository-wide tab totals. */
    requestListTotalsRefresh: totalsRefreshes.request,
    get: createEnvironmentRpcQueryAtomFamily(runtime, {
      label: "environment-data:pull-requests:get",
      tag: WS_METHODS.pullRequestsGet,
      staleTimeMs: 5_000,
    }),
    getTimeline: createEnvironmentRpcQueryAtomFamily(runtime, {
      label: "environment-data:pull-requests:get-timeline",
      tag: WS_METHODS.pullRequestsGetTimeline,
      staleTimeMs: 5_000,
    }),
    getCommits: createEnvironmentRpcQueryAtomFamily(runtime, {
      label: "environment-data:pull-requests:get-commits",
      tag: WS_METHODS.pullRequestsGetCommits,
      staleTimeMs: 30_000,
    }),
    getChecks: createEnvironmentRpcQueryAtomFamily(runtime, {
      label: "environment-data:pull-requests:get-checks",
      tag: WS_METHODS.pullRequestsGetChecks,
      staleTimeMs: 5_000,
    }),
    getFiles: createEnvironmentRpcQueryAtomFamily(runtime, {
      label: "environment-data:pull-requests:get-files",
      tag: WS_METHODS.pullRequestsGetFiles,
      staleTimeMs: 30_000,
    }),
    // A stored payload is a point-in-time copy; every call re-reads it, so the
    // caller's own `newerObservedAt` comparison decides whether to paint it.
    readSnapshot: createEnvironmentRpcQueryAtomFamily(runtime, {
      label: "environment-data:pull-requests:read-snapshot",
      tag: WS_METHODS.pullRequestsReadSnapshot,
      staleTimeMs: 0,
      idleTtlMs: 0,
    }),
    /** The poller's change events for the mounted list key, number, and tab. */
    subscribe: createEnvironmentRpcSubscriptionAtomFamily(runtime, {
      label: "environment-data:pull-requests:subscribe",
      tag: WS_METHODS.pullRequestsSubscribe,
      idleTtlMs: 0,
    }),
    runAction: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:pull-requests:run-action",
      tag: WS_METHODS.pullRequestsRunAction,
      scheduler: vcsCommandScheduler,
      concurrency: vcsCommandConcurrency,
    }),
    checkout: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:pull-requests:checkout",
      tag: WS_METHODS.pullRequestsCheckout,
      scheduler: vcsCommandScheduler,
      concurrency: vcsCommandConcurrency,
    }),
  };
}
