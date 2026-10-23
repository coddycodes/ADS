/**
 * Behavior tests for the source-control action hooks.
 *
 * These hooks are thin wrappers that (a) resolve a `{ environmentId, cwd }`
 * scope and short-circuit to a `VcsActionUnavailableError` when it is
 * incomplete, and (b) otherwise run an atom command, optionally tracked by the
 * shared `vcsActionManager` and refreshing VCS status on success. We exercise
 * them by mocking `react`'s `useCallback` to a passthrough (so the hooks can be
 * called as plain functions) and stubbing the atom/command/query seams, then
 * driving the returned `run`/`resetError` directly. `VcsActionUnavailableError`,
 * `AsyncResult`, `Cause`, and `Option` are kept real.
 */
import { EnvironmentId } from "@bibcode/contracts";
import { AsyncResult } from "effect/reactivity";
import * as Cause from "effect/Cause";
import { describe, expect, it, vi, beforeEach } from "vite-plus/test";

const h = vi.hoisted(() => ({
  actionState: { operation: null, error: null, isRunning: false } as {
    operation: string | null;
    error: unknown;
    isRunning: boolean;
  },
  commandCalls: [] as Array<{ command: unknown; input: unknown }>,
  nextCommandResult: undefined as unknown,
  trackCalls: [] as Array<{ scope: unknown; opts: unknown }>,
  resetErrorCalls: [] as Array<{ scope: unknown; operation: unknown }>,
  refresh: (() => undefined) as () => void,
  revalidate: (() => undefined) as () => void,
  queryData: null as unknown,
  queryError: null as unknown,
  queryPending: false,
  registryValue: undefined as unknown,
  registryGetCalls: [] as unknown[],
}));

vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  return {
    ...actual,
    useCallback: (<T>(fn: T) => fn) as typeof actual.useCallback,
  };
});

vi.mock("@effect/atom-react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@effect/atom-react")>();
  return {
    ...actual,
    useAtomValue: () => h.actionState,
  };
});

vi.mock("./use-atom-command", () => ({
  useAtomCommand: (command: unknown) => (input: unknown) => {
    h.commandCalls.push({ command, input });
    return Promise.resolve(h.nextCommandResult);
  },
}));

vi.mock("./query", () => ({
  useEnvironmentQuery: () => ({
    data: h.queryData,
    error: h.queryError,
    isPending: h.queryPending,
    refresh: h.refresh,
    revalidate: h.revalidate,
    requiresRetry: false,
  }),
}));

vi.mock("./vcs", () => ({
  vcsEnvironment: {
    init: { label: "init" },
    pull: { label: "pull" },
    status: (args: unknown) => ({ __atom: "status", args }),
    stageFiles: { label: "stageFiles" },
    unstageFiles: { label: "unstageFiles" },
    discardFiles: { label: "discardFiles" },
    generateCommitMessage: { label: "generateCommitMessage" },
  },
  vcsActionManager: {
    stateAtom: (scope: unknown) => ({ __atom: "state", scope }),
    resetError: (_registry: unknown, scope: unknown, operation: unknown) => {
      h.resetErrorCalls.push({ scope, operation });
    },
    track: (_registry: unknown, scope: unknown, opts: unknown, execute: () => unknown) => {
      h.trackCalls.push({ scope, opts });
      return execute();
    },
    runStackedAction: (scope: unknown) => ({ __atom: "runStackedAction", scope }),
  },
}));

vi.mock("./git", () => ({
  gitEnvironment: {
    preparePullRequestThread: { label: "preparePullRequestThread" },
    pullRequestResolution: (args: unknown) => ({ __atom: "pullRequestResolution", args }),
  },
}));

vi.mock("./sourceControl", () => ({
  sourceControlEnvironment: {
    publishRepository: { label: "publishRepository" },
  },
}));

vi.mock("../rpc/atomRegistry", () => ({
  appAtomRegistry: {
    get: (atom: unknown) => {
      h.registryGetCalls.push(atom);
      return h.registryValue;
    },
  },
}));

import {
  readCachedPullRequestResolution,
  useGitStackedAction,
  usePreparePullRequestThreadAction,
  usePullRequestResolutionState,
  useSourceControlActionRunning,
  useSourceControlPublishRepositoryAction,
  useVcsDiscardAction,
  useVcsGenerateCommitMessageAction,
  useVcsInitAction,
  useVcsPullAction,
  useVcsStageAction,
  useVcsUnstageAction,
} from "./sourceControlActions";
import { VcsActionUnavailableError } from "@bibcode/client-runtime/state/vcs";

const environmentId = EnvironmentId.make("environment-1");
const fullScope = { environmentId, cwd: "/repo" } as const;
const nullScope = { environmentId: null, cwd: null } as const;

beforeEach(() => {
  h.actionState = { operation: null, error: null, isRunning: false };
  h.commandCalls.length = 0;
  h.nextCommandResult = AsyncResult.success({ ok: true });
  h.trackCalls.length = 0;
  h.resetErrorCalls.length = 0;
  h.refresh = vi.fn();
  h.revalidate = vi.fn();
  h.queryData = null;
  h.queryError = null;
  h.queryPending = false;
  h.registryValue = AsyncResult.initial();
  h.registryGetCalls.length = 0;
});

async function expectUnavailable(result: unknown, operation: string): Promise<void> {
  expect(AsyncResult.isFailure(result as never)).toBe(true);
  // The failure carries a VcsActionUnavailableError describing the missing scope.
  const error = Cause.squash((result as { cause: Cause.Cause<unknown> }).cause);
  expect(error).toBeInstanceOf(VcsActionUnavailableError);
  expect((error as VcsActionUnavailableError).operation).toBe(operation);
}

describe("scope short-circuit (VcsActionUnavailableError)", () => {
  it("useVcsInitAction fails fast when the scope is incomplete", async () => {
    const state = useVcsInitAction(nullScope);
    const result = await state.run();
    await expectUnavailable(result, "init");
    // No command ran and the manager was not asked to track anything.
    expect(h.commandCalls).toHaveLength(0);
  });

  it("useVcsPullAction fails fast when the scope is incomplete", async () => {
    const result = await useVcsPullAction(nullScope).run();
    await expectUnavailable(result, "pull");
  });

  it("staging/unstaging/discarding fail fast with their mapped operation", async () => {
    await expectUnavailable(await useVcsStageAction(nullScope).run([]), "stage_files");
    await expectUnavailable(await useVcsUnstageAction(nullScope).run([]), "unstage_files");
    await expectUnavailable(await useVcsDiscardAction(nullScope).run([]), "discard_files");
  });

  it("generate/publish/prepare/stacked fail fast with their mapped operation", async () => {
    await expectUnavailable(
      await useVcsGenerateCommitMessageAction(nullScope).run({}),
      "generate_commit_message",
    );
    await expectUnavailable(
      await useSourceControlPublishRepositoryAction(nullScope).run({
        provider: "github",
        repository: "acme/demo",
        visibility: "private",
        remoteName: "origin",
        protocol: "https",
      }),
      "publish_repository",
    );
    await expectUnavailable(
      await usePreparePullRequestThreadAction(nullScope).run({
        reference: "main",
      }),
      "prepare_pull_request_thread",
    );
    await expectUnavailable(
      await useGitStackedAction(nullScope).run({ actionId: "a1", action: "commit" as never }),
      "run_change_request",
    );
  });
});

describe("command dispatch on a complete scope", () => {
  it("useVcsInitAction runs the init command and tracks it", async () => {
    const result = await useVcsInitAction(fullScope).run();
    expect(AsyncResult.isSuccess(result as never)).toBe(true);
    expect(h.commandCalls).toHaveLength(1);
    expect(h.commandCalls[0]!.input).toEqual({
      environmentId,
      input: { cwd: "/repo" },
    });
    // Tracked (not managedExternally) with the init operation + label.
    expect(h.trackCalls).toHaveLength(1);
    expect(h.trackCalls[0]!.opts).toEqual({ operation: "init", label: "Initializing repository" });
  });

  it("useVcsPullAction refreshes status on success", async () => {
    await useVcsPullAction(fullScope).run();
    expect(h.commandCalls).toHaveLength(1);
    // A finished action re-reads automatically; only an explicit Retry clears a cut-off.
    expect(h.revalidate).toHaveBeenCalledTimes(1);
    expect(h.refresh).not.toHaveBeenCalled();
  });

  it("does not refresh status when the command fails", async () => {
    h.nextCommandResult = AsyncResult.failure(Cause.fail(new Error("boom")));
    await useVcsPullAction(fullScope).run();
    expect(h.revalidate).not.toHaveBeenCalled();
    expect(h.refresh).not.toHaveBeenCalled();
  });

  it("file actions forward the file paths and refresh on success", async () => {
    await useVcsStageAction(fullScope).run(["a.ts", "b.ts"]);
    expect(h.commandCalls[0]!.input).toEqual({
      environmentId,
      input: { cwd: "/repo", filePaths: ["a.ts", "b.ts"] },
    });
    expect(h.revalidate).toHaveBeenCalledTimes(1);
  });

  it("generate commit message omits filePaths when empty and includes them otherwise", async () => {
    await useVcsGenerateCommitMessageAction(fullScope).run({});
    expect(h.commandCalls[0]!.input).toEqual({ environmentId, input: { cwd: "/repo" } });

    h.commandCalls.length = 0;
    await useVcsGenerateCommitMessageAction(fullScope).run({ filePaths: ["x.ts"] });
    expect(h.commandCalls[0]!.input).toEqual({
      environmentId,
      input: { cwd: "/repo", filePaths: ["x.ts"] },
    });
  });

  it("publish repository merges the provider input under the resolved scope", async () => {
    await useSourceControlPublishRepositoryAction(fullScope).run({
      provider: "gitlab",
      repository: "acme/demo",
      visibility: "public",
      remoteName: "origin",
      protocol: "ssh",
    });
    expect(h.commandCalls[0]!.input).toEqual({
      environmentId,
      input: {
        cwd: "/repo",
        provider: "gitlab",
        repository: "acme/demo",
        visibility: "public",
        remoteName: "origin",
        protocol: "ssh",
      },
    });
    expect(h.revalidate).toHaveBeenCalledTimes(1);
  });

  it("prepares pull requests only in safe local mode", async () => {
    await usePreparePullRequestThreadAction(fullScope).run({ reference: "main" });
    expect(h.commandCalls[0]!.input).toEqual({
      environmentId,
      input: { cwd: "/repo", reference: "main", mode: "local" },
    });
  });

  it("stacked action is managed externally (bypasses track) and forwards optional fields", async () => {
    const onProgress = () => undefined;
    const createOptions = {
      draft: true,
      assignees: ["7"],
      reviewers: [],
      labels: ["bug"],
      milestone: null,
      removeSourceBranch: null,
      squash: null,
    };
    await useGitStackedAction(fullScope).run({
      actionId: "act-1",
      action: "commit_push_pr",
      commitMessage: "msg",
      featureBranch: true,
      filePaths: ["a.ts"],
      commitStagedIndexAsIs: true,
      pullRequestBaseBranch: "release/next",
      pullRequestHeadBranch: "feature/other",
      pullRequestTitle: "Reviewed title",
      pullRequestBody: "Reviewed body",
      pullRequestOptions: createOptions,
      onProgress,
    });
    expect(h.trackCalls).toHaveLength(0);
    expect(h.commandCalls[0]!.input).toEqual({
      actionId: "act-1",
      action: "commit_push_pr",
      commitMessage: "msg",
      featureBranch: true,
      filePaths: ["a.ts"],
      commitStagedIndexAsIs: true,
      pullRequestBaseBranch: "release/next",
      pullRequestHeadBranch: "feature/other",
      pullRequestTitle: "Reviewed title",
      pullRequestBody: "Reviewed body",
      pullRequestOptions: createOptions,
      onProgress,
    });
    // onSuccess still refreshes status even though tracking is external.
    expect(h.revalidate).toHaveBeenCalledTimes(1);
  });

  it("stacked action drops optional fields that are absent", async () => {
    await useGitStackedAction(fullScope).run({ actionId: "act-2", action: "amend" as never });
    expect(h.commandCalls[0]!.input).toEqual({ actionId: "act-2", action: "amend" });
  });
});

describe("owned action state (error / isPending / resetError)", () => {
  it("surfaces error and pending flags only when the manager owns this operation", () => {
    const boom = new Error("init failed");
    h.actionState = { operation: "init", error: boom, isRunning: true };
    const owned = useVcsInitAction(fullScope);
    expect(owned.error).toBe(boom);
    expect(owned.isPending).toBe(true);

    // A hook for a different operation does not adopt the manager's error.
    const other = useVcsPullAction(fullScope);
    expect(other.error).toBeNull();
    expect(other.isPending).toBe(false);
  });

  it("resetError forwards the mapped operation to the manager", () => {
    useVcsInitAction(fullScope).resetError();
    expect(h.resetErrorCalls).toHaveLength(1);
    expect(h.resetErrorCalls[0]!.operation).toBe("init");
  });
});

describe("useSourceControlActionRunning", () => {
  it("is true only when a matching operation is running", () => {
    h.actionState = { operation: "pull", error: null, isRunning: true };
    expect(useSourceControlActionRunning(fullScope, ["pull"])).toBe(true);
    expect(useSourceControlActionRunning(fullScope, ["init"])).toBe(false);
  });

  it("is false when nothing is running or no operation is set", () => {
    h.actionState = { operation: "pull", error: null, isRunning: false };
    expect(useSourceControlActionRunning(fullScope, ["pull"])).toBe(false);

    h.actionState = { operation: null, error: null, isRunning: true };
    expect(useSourceControlActionRunning(fullScope, ["pull"])).toBe(false);
  });
});

describe("readCachedPullRequestResolution", () => {
  it("returns null when the target is incomplete", () => {
    expect(
      readCachedPullRequestResolution({ environmentId: null, cwd: null, reference: null }),
    ).toBeNull();
    expect(h.registryGetCalls).toHaveLength(0);
  });

  it("returns the cached value from the atom registry when present", () => {
    const cached = { pullRequest: { number: 7 } };
    h.registryValue = AsyncResult.success(cached);
    const result = readCachedPullRequestResolution({
      environmentId,
      cwd: "/repo",
      reference: "main",
    });
    expect(result).toBe(cached);
    expect(h.registryGetCalls).toHaveLength(1);
  });

  it("returns null when the cached atom has no settled value", () => {
    h.registryValue = AsyncResult.initial();
    expect(
      readCachedPullRequestResolution({ environmentId, cwd: "/repo", reference: "main" }),
    ).toBeNull();
  });
});

describe("usePullRequestResolutionState", () => {
  it("prefers live query data and suppresses pending once cached data exists", () => {
    h.queryData = { pullRequest: { number: 1 } };
    h.queryPending = true;
    h.registryValue = AsyncResult.success({ pullRequest: { number: 0 } });
    const state = usePullRequestResolutionState({
      environmentId,
      cwd: "/repo",
      reference: "main",
    });
    expect(state.data).toBe(h.queryData);
    expect(state.isFetching).toBe(true);
    // Pending is suppressed because a cached resolution is available.
    expect(state.isPending).toBe(false);
  });

  it("falls back to the cached resolution and stays pending while empty", () => {
    h.queryData = null;
    h.queryPending = true;
    h.registryValue = AsyncResult.initial();
    const state = usePullRequestResolutionState({
      environmentId,
      cwd: "/repo",
      reference: "main",
    });
    expect(state.data).toBeNull();
    expect(state.isPending).toBe(true);
  });
});
