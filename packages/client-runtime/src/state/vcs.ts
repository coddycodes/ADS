import {
  type GitCancelCloneInput,
  type VcsStatusResult,
  type VcsStatusStreamEvent,
  type VcsStatusSummary,
  WS_METHODS,
} from "@coddy/contracts";
import { applyGitStatusStreamEvent } from "@coddy/shared/git";
import * as Cause from "effect/Cause";
import * as Effect from "effect/Effect";
import * as Equal from "effect/Equal";
import * as Option from "effect/Option";
import * as Stream from "effect/Stream";
import * as SubscriptionRef from "effect/SubscriptionRef";
import { Atom, AtomRegistry } from "effect/reactivity";

import {
  createEnvironmentCommand,
  createEnvironmentRpcCommand,
  createEnvironmentRpcQueryAtomFamily,
  createEnvironmentSubscriptionAtomFamily,
  environmentRpcKey,
  followStreamInEnvironment,
  parseEnvironmentRpcKey,
} from "./runtime.ts";
import type { EnvironmentRegistry } from "../connection/registry.ts";
import type { ConnectionAttemptError } from "../connection/model.ts";
import { EnvironmentSupervisor } from "../connection/supervisor.ts";
import { safeErrorLogAttributes } from "../errors/safeLog.ts";
import {
  subscribe,
  subscribeInSession,
  type EnvironmentRpcInput,
  type EnvironmentRpcStreamFailure,
} from "../rpc/client.ts";
import {
  vcsCloneCancelScheduler,
  vcsCloneCommandConcurrency,
  vcsCommandConcurrency,
  vcsCommandScheduler,
  vcsGenerateScheduler,
  vcsStatusRefreshConcurrency,
  vcsStatusRefreshScheduler,
} from "./vcsCommandScheduler.ts";
import {
  cancelCloneOnNextSession,
  cloneWithReattach,
  registerCloneCancel,
  type VcsCloneCommandInput,
} from "./vcsClone.ts";

export type VcsPassiveStatus = VcsStatusSummary | VcsStatusResult;

// Matches GIT_STATUS_FELL_BEHIND_OPERATION in apps/server/src/git/broadcaster.rs.
const GIT_STATUS_FELL_BEHIND_OPERATION = "GitStatusBroadcaster.fellBehind";

type VcsSummaryStreamFailure = EnvironmentRpcStreamFailure<
  typeof WS_METHODS.subscribeVcsStatusSummary
>;

/**
 * The status after one stream event, and what to emit. An event that leaves the status unchanged
 * (the snapshot every reconnect resends, an unchanged poll) emits nothing and keeps the object:
 * a new copy re-rendered every reader, and on a large workspace each render's closures pinned
 * another copy of the file list.
 */
export function projectVcsStatusEvent(
  current: VcsStatusResult | null,
  event: VcsStatusStreamEvent,
): readonly [VcsStatusResult | null, ReadonlyArray<VcsStatusResult>] {
  const next = applyGitStatusStreamEvent(current, event);
  return current !== null && Equal.equals(current, next) ? [current, []] : [next, [next]];
}

function reduceStatusEvents<E, R>(
  stream: Stream.Stream<VcsStatusStreamEvent, E, R>,
): Stream.Stream<VcsStatusResult, E, R> {
  return stream.pipe(Stream.mapAccum(() => null as VcsStatusResult | null, projectVcsStatusEvent));
}

function subscribeToVcsSummary<E, R>(
  input: EnvironmentRpcInput<typeof WS_METHODS.subscribeVcsStatusSummary>,
  legacyStatus: Stream.Stream<VcsStatusResult, E, R>,
): Stream.Stream<
  VcsPassiveStatus,
  E | VcsSummaryStreamFailure | ConnectionAttemptError,
  R | EnvironmentSupervisor
> {
  return Stream.unwrap(
    EnvironmentSupervisor.pipe(
      Effect.map((supervisor) =>
        SubscriptionRef.changes(supervisor.session).pipe(
          Stream.switchMap(
            Option.match({
              onNone: () => Stream.empty,
              onSome: (session) =>
                Stream.fromEffect(session.initialConfig).pipe(
                  Stream.flatMap(
                    (config): Stream.Stream<VcsPassiveStatus, E | VcsSummaryStreamFailure, R> =>
                      config.environment.capabilities.vcsStatusSummary === true
                        ? subscribeInSession(
                            session,
                            supervisor.target.environmentId,
                            WS_METHODS.subscribeVcsStatusSummary,
                            input,
                            {
                              onExpectedFailure: (cause) =>
                                Effect.logWarning(
                                  "Could not refresh the passive VCS summary; retrying.",
                                ).pipe(
                                  Effect.annotateLogs({
                                    ...safeErrorLogAttributes(Cause.squash(cause)),
                                  }),
                                ),
                              retryExpectedFailureAfter: "30 seconds",
                            },
                          ).pipe(Stream.map((value): VcsPassiveStatus => value))
                        : legacyStatus.pipe(Stream.map((value): VcsPassiveStatus => value)),
                  ),
                ),
            }),
          ),
        ),
      ),
    ),
  );
}

/**
 * The clone loops follow the environment's current supervisor through the registry, so their
 * effects also need `EnvironmentRegistry`, which the command runtime provides. The explicit type
 * arguments below say so; inference would leave the registry out of the allowed services.
 */
type CloneEffect = ReturnType<typeof cloneWithReattach>;
type CancelCloneEffect = ReturnType<typeof cancelCloneOnNextSession>;

/**
 * `vcs.cancelClone` as a command. Its `run` records the pending cancel synchronously, before
 * anything is scheduled, so a later clone of the same URL into the same leaf waits for it,
 * even from a remounted dialog. The record ends when the command settles: acknowledged,
 * stopped, or failed. The dialog that asked never owns it.
 */
function createCancelCloneCommand<R, E>(runtime: Atom.AtomRuntime<EnvironmentRegistry | R, E>) {
  const command = createEnvironmentCommand<
    EnvironmentRegistry | R,
    E,
    GitCancelCloneInput,
    Effect.Success<CancelCloneEffect>,
    Effect.Error<CancelCloneEffect>
  >(runtime, {
    label: "environment-data:vcs:cancel-clone",
    scheduler: vcsCloneCancelScheduler,
    concurrency: vcsCloneCommandConcurrency,
    execute: (input: GitCancelCloneInput) => cancelCloneOnNextSession(input),
  });
  return {
    ...command,
    run: (...args: Parameters<typeof command.run>) => {
      const [, target] = args;
      const release = registerCloneCancel(target.environmentId, target.input);
      return command.run(...args).finally(release);
    },
  };
}

export function createVcsEnvironmentAtoms<R, E>(
  runtime: Atom.AtomRuntime<EnvironmentRegistry | R, E>,
) {
  const status = createEnvironmentSubscriptionAtomFamily(runtime, {
    label: "environment-data:vcs:status",
    subscribe: (input: EnvironmentRpcInput<typeof WS_METHODS.subscribeVcsStatus>) =>
      reduceStatusEvents(
        subscribe(WS_METHODS.subscribeVcsStatus, input, {
          onExpectedFailure: (cause) =>
            Effect.logWarning("The Git status stream fell behind; resubscribing.").pipe(
              Effect.annotateLogs({ ...safeErrorLogAttributes(Cause.squash(cause)) }),
            ),
          retryExpectedFailure: {
            when: (error) =>
              error._tag === "GitCommandError" &&
              error.operation === GIT_STATUS_FELL_BEHIND_OPERATION,
            initialDelay: "250 millis",
            maxDelay: "30 seconds",
            resetAfter: "30 seconds",
          },
        }),
      ),
    idleTtlMs: 0,
  });
  const summaryFamily = Atom.family((key: string) => {
    const target =
      parseEnvironmentRpcKey<EnvironmentRpcInput<typeof WS_METHODS.subscribeVcsStatusSummary>>(key);
    return runtime
      .atom((get) =>
        followStreamInEnvironment(
          target.environmentId,
          subscribeToVcsSummary(
            target.input,
            AtomRegistry.toStreamResult(get.registry, status(target)),
          ),
        ),
      )
      .pipe(Atom.setIdleTTL(0), Atom.withLabel(`environment-data:vcs:summary:${key}`));
  });
  const summary = (target: Parameters<typeof status>[0]) =>
    summaryFamily(environmentRpcKey(target));

  return {
    listRefs: createEnvironmentRpcQueryAtomFamily(runtime, {
      label: "environment-data:vcs:list-refs",
      tag: WS_METHODS.vcsListRefs,
      staleTimeMs: 5_000,
    }),
    listCommits: createEnvironmentRpcQueryAtomFamily(runtime, {
      label: "environment-data:vcs:list-commits",
      tag: WS_METHODS.vcsListCommits,
      staleTimeMs: 10_000,
    }),
    status,
    summary,
    pull: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:vcs:pull",
      tag: WS_METHODS.vcsPull,
      scheduler: vcsCommandScheduler,
      concurrency: vcsCommandConcurrency,
    }),
    refreshStatus: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:vcs:refresh-status",
      tag: WS_METHODS.vcsRefreshStatus,
      scheduler: vcsStatusRefreshScheduler,
      concurrency: vcsStatusRefreshConcurrency,
    }),
    // React owns no retry loops: the re-attach loop lives in the command itself.
    clone: createEnvironmentCommand<
      EnvironmentRegistry | R,
      E,
      VcsCloneCommandInput,
      Effect.Success<CloneEffect>,
      Effect.Error<CloneEffect>
    >(runtime, {
      label: "environment-data:vcs:clone",
      scheduler: vcsCommandScheduler,
      concurrency: vcsCloneCommandConcurrency,
      execute: (input: VcsCloneCommandInput) => cloneWithReattach(input),
    }),
    cancelClone: createCancelCloneCommand(runtime),
    createRef: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:vcs:create-ref",
      tag: WS_METHODS.vcsCreateRef,
      scheduler: vcsCommandScheduler,
      concurrency: vcsCommandConcurrency,
    }),
    switchRef: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:vcs:switch-ref",
      tag: WS_METHODS.vcsSwitchRef,
      scheduler: vcsCommandScheduler,
      concurrency: vcsCommandConcurrency,
    }),
    init: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:vcs:init",
      tag: WS_METHODS.vcsInit,
      scheduler: vcsCommandScheduler,
      concurrency: vcsCommandConcurrency,
    }),
    generateCommitMessage: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:vcs:generate-commit-message",
      tag: WS_METHODS.vcsGenerateCommitMessage,
      // Own lane: a slow generation must not block stage/unstage/discard/commit.
      scheduler: vcsGenerateScheduler,
      concurrency: vcsCommandConcurrency,
    }),
    stageFiles: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:vcs:stage-files",
      tag: WS_METHODS.vcsStageFiles,
      scheduler: vcsCommandScheduler,
      concurrency: vcsCommandConcurrency,
    }),
    unstageFiles: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:vcs:unstage-files",
      tag: WS_METHODS.vcsUnstageFiles,
      scheduler: vcsCommandScheduler,
      concurrency: vcsCommandConcurrency,
    }),
    discardFiles: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:vcs:discard-files",
      tag: WS_METHODS.vcsDiscardFiles,
      scheduler: vcsCommandScheduler,
      concurrency: vcsCommandConcurrency,
    }),
  };
}

export * from "./gitActions.ts";
export * from "./vcsAction.ts";
export * from "./vcsStatus.ts";
export * from "./vcsClone.ts";
