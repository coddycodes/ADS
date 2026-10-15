import { useAtomValue } from "@effect/atom-react";
import type {
  AtomCommandFailure,
  AtomCommandResult,
  AtomCommandSuccess,
} from "@bibcode/client-runtime/state/runtime";
import {
  VcsActionUnavailableError,
  type VcsActionOperation,
} from "@bibcode/client-runtime/state/vcs";
import type {
  EnvironmentId,
  GitActionProgressEvent,
  GitPullRequestCreateOptions,
  GitResolvePullRequestResult,
  GitStackedAction,
  SourceControlCloneProtocol,
  SourceControlRepositoryVisibility,
} from "@bibcode/contracts";
import * as Cause from "effect/Cause";
import * as Option from "effect/Option";
import { AsyncResult } from "effect/unstable/reactivity";
import { useCallback } from "react";

import { appAtomRegistry } from "../rpc/atomRegistry";
import { gitEnvironment } from "./git";
import { useEnvironmentQuery } from "./query";
import { sourceControlEnvironment } from "./sourceControl";
import { useAtomCommand } from "./use-atom-command";
import { vcsActionManager, vcsEnvironment } from "./vcs";

export type SourceControlActionKind =
  | "init"
  | "pull"
  | "publishRepository"
  | "runStackedAction"
  | "preparePullRequestThread"
  | "stageFiles"
  | "unstageFiles"
  | "discardFiles"
  | "generateCommitMessage";

export interface SourceControlActionScope {
  readonly environmentId: EnvironmentId | null;
  readonly cwd: string | null;
}

interface SourceControlActionState<
  TArgs extends ReadonlyArray<unknown>,
  R extends AtomCommandResult<unknown, unknown>,
> {
  readonly isPending: boolean;
  readonly error: unknown;
  readonly run: (
    ...args: TArgs
  ) => Promise<
    AtomCommandResult<AtomCommandSuccess<R>, AtomCommandFailure<R> | VcsActionUnavailableError>
  >;
  readonly resetError: () => void;
}

const ACTION_OPERATION = {
  init: "init",
  pull: "pull",
  publishRepository: "publish_repository",
  runStackedAction: "run_change_request",
  preparePullRequestThread: "prepare_pull_request_thread",
  stageFiles: "stage_files",
  unstageFiles: "unstage_files",
  discardFiles: "discard_files",
  generateCommitMessage: "generate_commit_message",
} as const satisfies Record<SourceControlActionKind, VcsActionOperation>;

function useAction<
  TArgs extends ReadonlyArray<unknown>,
  R extends AtomCommandResult<unknown, unknown>,
>(input: {
  readonly kind: SourceControlActionKind;
  readonly label: string;
  readonly scope: SourceControlActionScope;
  readonly action: (...args: TArgs) => Promise<R>;
  readonly onSuccess?: () => void;
  readonly managedExternally?: boolean;
}): SourceControlActionState<TArgs, R> {
  const { action, label, managedExternally, onSuccess, scope } = input;
  const operation = ACTION_OPERATION[input.kind];
  const state = useAtomValue(vcsActionManager.stateAtom(scope));
  const ownsState = state.operation === operation;

  const resetError = useCallback(() => {
    vcsActionManager.resetError(appAtomRegistry, scope, operation);
  }, [scope, operation]);

  const run = useCallback(
    async (...args: TArgs) => {
      const execute = async (): Promise<
        AtomCommandResult<AtomCommandSuccess<R>, AtomCommandFailure<R>>
      > => {
        const result = await action(...args);
        if (AsyncResult.isSuccess(result)) {
          onSuccess?.();
        }
        return result as AtomCommandResult<AtomCommandSuccess<R>, AtomCommandFailure<R>>;
      };
      return managedExternally === true
        ? execute()
        : vcsActionManager.track(
            appAtomRegistry,
            scope,
            {
              operation,
              label,
            },
            execute,
          );
    },
    [action, label, managedExternally, onSuccess, scope, operation],
  );

  return {
    error: ownsState ? state.error : null,
    isPending: ownsState && state.isRunning,
    resetError,
    run,
  };
}

function resolveScope(scope: SourceControlActionScope) {
  if (scope.environmentId === null || scope.cwd === null) {
    return null;
  }
  return {
    environmentId: scope.environmentId,
    cwd: scope.cwd,
  };
}

export function useSourceControlActionRunning(
  scope: SourceControlActionScope,
  kinds: ReadonlyArray<SourceControlActionKind>,
): boolean {
  const state = useAtomValue(vcsActionManager.stateAtom(scope));
  return (
    state.isRunning &&
    state.operation !== null &&
    kinds.some((kind) => ACTION_OPERATION[kind] === state.operation)
  );
}

export function useVcsInitAction(scope: SourceControlActionScope) {
  const init = useAtomCommand(vcsEnvironment.init, { reportFailure: false });
  const action = useCallback(async () => {
    const target = resolveScope(scope);
    if (target === null) {
      return AsyncResult.failure<never, VcsActionUnavailableError>(
        Cause.fail(
          new VcsActionUnavailableError({
            operation: "init",
            environmentId: scope.environmentId,
            cwd: scope.cwd,
          }),
        ),
      );
    }
    return init({
      environmentId: target.environmentId,
      input: { cwd: target.cwd },
    });
  }, [init, scope]);
  return useAction({ kind: "init", label: "Initializing repository", scope, action });
}

export function useVcsPullAction(scope: SourceControlActionScope) {
  const pull = useAtomCommand(vcsEnvironment.pull, { reportFailure: false });
  const status = useEnvironmentQuery(
    scope.environmentId !== null && scope.cwd !== null
      ? vcsEnvironment.status({
          environmentId: scope.environmentId,
          input: { cwd: scope.cwd },
        })
      : null,
  );
  const action = useCallback(async () => {
    const target = resolveScope(scope);
    if (target === null) {
      return AsyncResult.failure<never, VcsActionUnavailableError>(
        Cause.fail(
          new VcsActionUnavailableError({
            operation: "pull",
            environmentId: scope.environmentId,
            cwd: scope.cwd,
          }),
        ),
      );
    }
    return pull({
      environmentId: target.environmentId,
      input: { cwd: target.cwd },
    });
  }, [pull, scope]);
  return useAction({
    kind: "pull",
    label: "Pulling latest changes",
    scope,
    action,
    onSuccess: status.revalidate,
  });
}

function useVcsFileAction(
  scope: SourceControlActionScope,
  kind: "stageFiles" | "unstageFiles" | "discardFiles",
  command: typeof vcsEnvironment.stageFiles,
  label: string,
) {
  const run = useAtomCommand(command, { reportFailure: false });
  const status = useEnvironmentQuery(
    scope.environmentId !== null && scope.cwd !== null
      ? vcsEnvironment.status({ environmentId: scope.environmentId, input: { cwd: scope.cwd } })
      : null,
  );
  const action = useCallback(
    async (filePaths: string[]) => {
      const target = resolveScope(scope);
      if (target === null) {
        return AsyncResult.failure<never, VcsActionUnavailableError>(
          Cause.fail(
            new VcsActionUnavailableError({
              operation: ACTION_OPERATION[kind],
              environmentId: scope.environmentId,
              cwd: scope.cwd,
            }),
          ),
        );
      }
      return run({ environmentId: target.environmentId, input: { cwd: target.cwd, filePaths } });
    },
    [run, scope, kind],
  );
  return useAction({ kind, label, scope, action, onSuccess: status.revalidate });
}

export function useVcsStageAction(scope: SourceControlActionScope) {
  return useVcsFileAction(scope, "stageFiles", vcsEnvironment.stageFiles, "Staging files");
}
export function useVcsUnstageAction(scope: SourceControlActionScope) {
  return useVcsFileAction(scope, "unstageFiles", vcsEnvironment.unstageFiles, "Unstaging files");
}
export function useVcsDiscardAction(scope: SourceControlActionScope) {
  return useVcsFileAction(scope, "discardFiles", vcsEnvironment.discardFiles, "Discarding files");
}

export function useVcsGenerateCommitMessageAction(scope: SourceControlActionScope) {
  const generate = useAtomCommand(vcsEnvironment.generateCommitMessage, { reportFailure: false });
  const action = useCallback(
    async (input: { filePaths?: string[] }) => {
      const target = resolveScope(scope);
      if (target === null) {
        return AsyncResult.failure<never, VcsActionUnavailableError>(
          Cause.fail(
            new VcsActionUnavailableError({
              operation: "generate_commit_message",
              environmentId: scope.environmentId,
              cwd: scope.cwd,
            }),
          ),
        );
      }
      return generate({
        environmentId: target.environmentId,
        input: {
          cwd: target.cwd,
          ...(input.filePaths?.length ? { filePaths: input.filePaths } : {}),
        },
      });
    },
    [generate, scope],
  );
  return useAction({
    kind: "generateCommitMessage",
    label: "Generating commit message",
    scope,
    action,
  });
}

export function useGitStackedAction(scope: SourceControlActionScope) {
  const runStackedAction = useAtomCommand(vcsActionManager.runStackedAction(scope), {
    reportFailure: false,
  });
  const status = useEnvironmentQuery(
    scope.environmentId !== null && scope.cwd !== null
      ? vcsEnvironment.status({
          environmentId: scope.environmentId,
          input: { cwd: scope.cwd },
        })
      : null,
  );

  const action = useCallback(
    async (input: {
      actionId: string;
      action: GitStackedAction;
      commitMessage?: string;
      featureBranch?: boolean;
      filePaths?: string[];
      commitStagedIndexAsIs?: boolean;
      pullRequestTitle?: string;
      pullRequestBody?: string;
      pullRequestBaseBranch?: string;
      pullRequestHeadBranch?: string;
      pullRequestOptions?: GitPullRequestCreateOptions;
      onProgress?: (event: GitActionProgressEvent) => void;
    }) => {
      if (resolveScope(scope) === null) {
        return AsyncResult.failure<never, VcsActionUnavailableError>(
          Cause.fail(
            new VcsActionUnavailableError({
              operation: "run_change_request",
              environmentId: scope.environmentId,
              cwd: scope.cwd,
            }),
          ),
        );
      }
      return runStackedAction({
        actionId: input.actionId,
        action: input.action,
        ...(input.commitMessage ? { commitMessage: input.commitMessage } : {}),
        ...(input.featureBranch ? { featureBranch: true } : {}),
        ...(input.filePaths?.length ? { filePaths: input.filePaths } : {}),
        ...(input.commitStagedIndexAsIs ? { commitStagedIndexAsIs: true } : {}),
        ...(input.pullRequestTitle ? { pullRequestTitle: input.pullRequestTitle } : {}),
        ...(input.pullRequestBaseBranch !== undefined
          ? { pullRequestBaseBranch: input.pullRequestBaseBranch }
          : {}),
        ...(input.pullRequestHeadBranch !== undefined
          ? { pullRequestHeadBranch: input.pullRequestHeadBranch }
          : {}),
        ...(input.pullRequestBody !== undefined ? { pullRequestBody: input.pullRequestBody } : {}),
        ...(input.pullRequestOptions !== undefined
          ? { pullRequestOptions: input.pullRequestOptions }
          : {}),
        ...(input.onProgress ? { onProgress: input.onProgress } : {}),
      });
    },
    [runStackedAction, scope],
  );

  return useAction({
    kind: "runStackedAction",
    label: "Running source control action",
    scope,
    action,
    onSuccess: status.revalidate,
    managedExternally: true,
  });
}

export function useSourceControlPublishRepositoryAction(scope: SourceControlActionScope) {
  const publishRepository = useAtomCommand(sourceControlEnvironment.publishRepository, {
    reportFailure: false,
  });
  const status = useEnvironmentQuery(
    scope.environmentId !== null && scope.cwd !== null
      ? vcsEnvironment.status({
          environmentId: scope.environmentId,
          input: { cwd: scope.cwd },
        })
      : null,
  );
  const action = useCallback(
    async (input: {
      provider: "github" | "gitlab" | "bitbucket" | "azure-devops";
      repository: string;
      visibility: SourceControlRepositoryVisibility;
      remoteName: string;
      protocol: SourceControlCloneProtocol;
    }) => {
      const target = resolveScope(scope);
      if (target === null) {
        return AsyncResult.failure<never, VcsActionUnavailableError>(
          Cause.fail(
            new VcsActionUnavailableError({
              operation: "publish_repository",
              environmentId: scope.environmentId,
              cwd: scope.cwd,
            }),
          ),
        );
      }
      return publishRepository({
        environmentId: target.environmentId,
        input: {
          cwd: target.cwd,
          ...input,
        },
      });
    },
    [publishRepository, scope],
  );
  return useAction({
    kind: "publishRepository",
    label: "Publishing repository",
    scope,
    action,
    onSuccess: status.revalidate,
  });
}

export function usePreparePullRequestThreadAction(scope: SourceControlActionScope) {
  const preparePullRequestThread = useAtomCommand(gitEnvironment.preparePullRequestThread, {
    reportFailure: false,
  });
  const action = useCallback(
    async (input: { reference: string }) => {
      const target = resolveScope(scope);
      if (target === null) {
        return AsyncResult.failure<never, VcsActionUnavailableError>(
          Cause.fail(
            new VcsActionUnavailableError({
              operation: "prepare_pull_request_thread",
              environmentId: scope.environmentId,
              cwd: scope.cwd,
            }),
          ),
        );
      }
      return preparePullRequestThread({
        environmentId: target.environmentId,
        input: {
          cwd: target.cwd,
          reference: input.reference,
          mode: "local",
        },
      });
    },
    [preparePullRequestThread, scope],
  );
  return useAction({
    kind: "preparePullRequestThread",
    label: "Preparing pull request thread",
    scope,
    action,
  });
}

export interface PullRequestResolutionTarget {
  readonly environmentId: EnvironmentId | null;
  readonly cwd: string | null;
  readonly reference: string | null;
}

export function readCachedPullRequestResolution(
  target: PullRequestResolutionTarget,
): GitResolvePullRequestResult | null {
  if (target.environmentId === null || target.cwd === null || target.reference === null) {
    return null;
  }
  return Option.getOrNull(
    AsyncResult.value(
      appAtomRegistry.get(
        gitEnvironment.pullRequestResolution({
          environmentId: target.environmentId,
          input: { cwd: target.cwd, reference: target.reference },
        }),
      ),
    ),
  );
}

export function usePullRequestResolutionState(target: PullRequestResolutionTarget) {
  const query = useEnvironmentQuery(
    target.environmentId !== null && target.cwd !== null && target.reference !== null
      ? gitEnvironment.pullRequestResolution({
          environmentId: target.environmentId,
          input: {
            cwd: target.cwd,
            reference: target.reference,
          },
        })
      : null,
  );
  const cached = readCachedPullRequestResolution(target);

  return {
    data: query.data ?? cached,
    error: query.error,
    isPending: query.isPending && cached === null,
    isFetching: query.isPending,
    refresh: query.refresh,
  };
}
