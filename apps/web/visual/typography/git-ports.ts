import type { AtomCommand } from "@bibcode/client-runtime/state/runtime";
import * as Cause from "effect/Cause";
import { AsyncResult, Atom } from "effect/unstable/reactivity";
import { useCallback } from "react";
import type * as GitPorts from "~/state/gitManager";
import type * as PullRequestPorts from "~/state/pullRequests";
import type * as VcsPorts from "~/state/vcs";
import {
  commit,
  commitPage,
  createDefaults,
  diffFor,
  entries,
  mergePreviewFor,
  refsSnapshot,
  remoteTagsFor,
  vocabularyFor,
  vcsRefs,
  vcsStatus,
} from "./git-data.ts";
export { gitServerConfigs, makeGitFixtureServerConfig } from "./git-server-config.ts";

type Git = typeof GitPorts.gitManagerEnvironment;
type PullRequests = typeof PullRequestPorts.pullRequestsEnvironment;
type Vcs = typeof VcsPorts.vcsEnvironment;
type Manager = typeof VcsPorts.vcsActionManager;

function refused<W, A, E>(label: string): AtomCommand<W, A, E> {
  return {
    label,
    run: async () =>
      AsyncResult.failure(Cause.die(new Error(`Component fixture forbids ${label}.`))),
  };
}

// Every read computes a new payload. Actual PR query hooks require new identity after refresh.
export const gitManagerEnvironment: Pick<
  Git,
  | "getDiff"
  | "getRemoteTags"
  | "previewMerge"
  | "getRefs"
  | "getCommits"
  | "getStashes"
  | "stagePartial"
  | "unstagePartial"
  | "discardPartial"
  | "commit"
  | "undoCommit"
  | "discard"
  | "runOperation"
> = {
  getDiff: Atom.family((target: Parameters<Git["getDiff"]>[0]) =>
    Atom.make(() => AsyncResult.success(diffFor(target.input))),
  ),
  getRemoteTags: Atom.family((target: Parameters<Git["getRemoteTags"]>[0]) =>
    Atom.make(() => AsyncResult.success(remoteTagsFor(target.input))),
  ),
  previewMerge: Atom.family((target: Parameters<Git["previewMerge"]>[0]) =>
    Atom.make(() => AsyncResult.success(mergePreviewFor(target.input))),
  ),
  getRefs: Atom.family((_target: Parameters<Git["getRefs"]>[0]) =>
    Atom.make(() => AsyncResult.success(structuredClone(refsSnapshot))),
  ),
  getCommits: Atom.family((target: Parameters<Git["getCommits"]>[0]) =>
    Atom.make(() => {
      const tip = target.input.pinnedTips?.[0] ?? commitPage.pinnedTips[0] ?? commit.sha;
      return AsyncResult.success({
        ...structuredClone(commitPage),
        pinnedTips: [tip],
        commits: [{ ...structuredClone(commit), sha: tip, shortSha: tip.slice(0, 7) }],
      });
    }),
  ),
  getStashes: Atom.family((_target: Parameters<Git["getStashes"]>[0]) =>
    Atom.make(() => AsyncResult.success(structuredClone(entries))),
  ),
  stagePartial: refused("partial staging"),
  unstagePartial: refused("partial unstaging"),
  discardPartial: refused("partial discard"),
  commit: refused("commits"),
  undoCommit: refused("commit undo"),
  discard: refused("file discard"),
  runOperation: refused("Git operations"),
};

export const pullRequestsEnvironment: Pick<
  PullRequests,
  "getVocabulary" | "getCreateDefaults" | "runAction"
> = {
  getVocabulary: Atom.family((target: Parameters<PullRequests["getVocabulary"]>[0]) =>
    Atom.make(() => AsyncResult.success(vocabularyFor(target.input))),
  ),
  getCreateDefaults: Atom.family((_target: Parameters<PullRequests["getCreateDefaults"]>[0]) =>
    Atom.make(() => AsyncResult.success(structuredClone(createDefaults))),
  ),
  runAction: refused("pull request mutations"),
};

export const vcsEnvironment: Pick<
  Vcs,
  | "status"
  | "summary"
  | "listRefs"
  | "listCommits"
  | "pull"
  | "refreshStatus"
  | "clone"
  | "cancelClone"
  | "createRef"
  | "switchRef"
  | "init"
  | "generateCommitMessage"
  | "stageFiles"
  | "unstageFiles"
  | "discardFiles"
> = {
  status: Atom.family((_target: Parameters<Vcs["status"]>[0]) =>
    Atom.make(() => AsyncResult.success(structuredClone(vcsStatus))),
  ),
  summary: Atom.family((_target: Parameters<Vcs["summary"]>[0]) =>
    Atom.make(() => AsyncResult.success(structuredClone(vcsStatus))),
  ),
  listRefs: Atom.family((_target: Parameters<Vcs["listRefs"]>[0]) =>
    Atom.make(() => AsyncResult.success(structuredClone(vcsRefs))),
  ),
  listCommits: Atom.family((_target: Parameters<Vcs["listCommits"]>[0]) =>
    Atom.make(() => AsyncResult.success({ commits: [structuredClone(commit)], nextCursor: null })),
  ),
  pull: refused("pull"),
  refreshStatus: refused("status command refresh"),
  clone: refused("clone"),
  cancelClone: refused("clone cancellation"),
  createRef: refused("ref creation"),
  switchRef: refused("ref switching"),
  init: refused("repository initialization"),
  generateCommitMessage: refused("commit message generation"),
  stageFiles: refused("file staging"),
  unstageFiles: refused("file unstaging"),
  discardFiles: refused("file discard"),
};

const idleState: Atom.Type<ReturnType<Manager["stateAtom"]>> = {
  isRunning: false,
  operation: null,
  actionId: null,
  action: null,
  currentLabel: null,
  currentPhaseLabel: null,
  hookName: null,
  lastOutputLine: null,
  phaseStartedAtMs: null,
  hookStartedAtMs: null,
  error: null,
};
export const vcsActionManager: Pick<
  Manager,
  "stateAtom" | "runStackedAction" | "track" | "resetError"
> = {
  stateAtom: Atom.family((_target: Parameters<Manager["stateAtom"]>[0]) =>
    Atom.make(structuredClone(idleState)),
  ),
  runStackedAction: () => refused("stacked Git actions"),
  track: async () =>
    AsyncResult.failure(Cause.die(new Error("Component fixture forbids tracked Git actions."))),
  resetError: () => {
    throw new Error("Component fixture forbids resetting application action state.");
  },
};

export const runGitManagerOperation: typeof GitPorts.runGitManagerOperation = () => ({
  result: Promise.resolve(
    AsyncResult.failure(Cause.die(new Error("Component fixture forbids Git operation streams."))),
  ),
  cancel: () => undefined,
});
export const useRunGitManagerOperation: typeof GitPorts.useRunGitManagerOperation = () =>
  useCallback((target, onEvent) => runGitManagerOperationForHook(target, onEvent), []);
function runGitManagerOperationForHook(
  _target: Parameters<ReturnType<typeof GitPorts.useRunGitManagerOperation>>[0],
  _onEvent: Parameters<ReturnType<typeof GitPorts.useRunGitManagerOperation>>[1],
): GitPorts.GitManagerOperationHandle {
  return {
    result: Promise.resolve(
      AsyncResult.failure(Cause.die(new Error("Component fixture forbids Git operation streams."))),
    ),
    cancel: () => undefined,
  };
}
