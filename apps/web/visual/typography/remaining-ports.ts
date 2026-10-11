import { useAtomValue } from "@effect/atom-react";
import type { AtomCommand } from "@bibcode/client-runtime/state/runtime";
import {
  createEnvironmentThreadDetailAtoms,
  createEnvironmentThreadShellAtoms,
  EMPTY_ENVIRONMENT_THREAD_STATE,
} from "@bibcode/client-runtime/state/threads";
import type { OrchestrationShellSnapshot } from "@bibcode/contracts";
import * as Cause from "effect/Cause";
import * as Option from "effect/Option";
import { AsyncResult, Atom } from "effect/unstable/reactivity";
import type * as ReviewPorts from "../../src/state/review";
import type * as SourceControlPorts from "../../src/state/sourceControl";
import type * as ThreadPorts from "../../src/state/threads";
import type * as OrchestrationPorts from "../../src/state/orchestration";
import type * as VcsPorts from "../../src/state/vcs";
import {
  remainingDiff,
  remainingPatch,
  remainingCwd,
  publishCwd,
  remainingThreadRef,
  publishDiscovery,
  publishStatus,
} from "./remaining-data";
import { useDiffPanelStore } from "../../src/diffPanelStore";
import { TurnId } from "@bibcode/contracts";

type RemainingDiffState =
  | "branch"
  | "raw"
  | "missing"
  | "error"
  | "empty-turn"
  | "net-zero"
  | "not-repo";
let selectedDiffState: RemainingDiffState = "branch";
export function prepareRemainingContextFixture(id: string): () => void {
  selectedDiffState =
    id === "diff-panel-raw"
      ? "raw"
      : id === "diff-panel-missing"
        ? "missing"
        : id === "diff-panel-error"
          ? "error"
          : id === "diff-panel-turn-empty"
            ? "empty-turn"
            : id === "diff-panel-net-zero"
              ? "net-zero"
              : id === "diff-panel-not-repo"
                ? "not-repo"
                : "branch";
  useDiffPanelStore.getState().removeThread(remainingThreadRef);
  if (selectedDiffState === "empty-turn")
    useDiffPanelStore.getState().selectTurn(remainingThreadRef, TurnId.make("fixture-empty-turn"));
  return () => {
    useDiffPanelStore.getState().removeThread(remainingThreadRef);
    selectedDiffState = "branch";
  };
}
function selectedDiffResult(): ReturnType<
  ReturnType<typeof ReviewPorts.reviewEnvironment.diffPreview>["read"]
> {
  if (selectedDiffState === "error")
    return AsyncResult.failure(Cause.die(new Error("Owned fixture diff preview failed.")));
  const sources =
    selectedDiffState === "missing"
      ? []
      : remainingDiff.sources.map((source) => ({
          ...source,
          truncated: selectedDiffState === "branch" && source.truncated,
          diff:
            selectedDiffState === "raw"
              ? "Owned fixture raw patch text"
              : selectedDiffState === "net-zero"
                ? ""
                : source.diff,
        }));
  return AsyncResult.success({ ...remainingDiff, sources });
}

function refused<W, A, E>(label: string): AtomCommand<W, A, E> {
  return {
    label,
    run: async () =>
      AsyncResult.failure(Cause.die(new Error(`Component fixture forbids ${label}.`))),
  };
}
export const reviewEnvironment: Pick<typeof ReviewPorts.reviewEnvironment, "diffPreview"> = {
  diffPreview: Atom.family(
    (_target: Parameters<typeof ReviewPorts.reviewEnvironment.diffPreview>[0]) =>
      Atom.make(selectedDiffResult),
  ),
};
export const sourceControlEnvironment: Pick<
  typeof SourceControlPorts.sourceControlEnvironment,
  "discovery" | "publishRepository"
> = {
  discovery: Atom.family(
    (_target: Parameters<typeof SourceControlPorts.sourceControlEnvironment.discovery>[0]) =>
      Atom.make(() => AsyncResult.success(structuredClone(publishDiscovery))),
  ),
  publishRepository: refused("publishing a repository"),
};
// Root merges this status read only for publishCwd. Existing owned VCS data stays
// authoritative for the previously drafted Git scenes.
export const remainingVcsStatus: typeof VcsPorts.vcsEnvironment.status = Atom.family(
  (target: Parameters<typeof VcsPorts.vcsEnvironment.status>[0]) =>
    Atom.make(() =>
      AsyncResult.success({
        ...publishStatus,
        hasPrimaryRemote: target.input.cwd !== publishCwd,
        hasUpstream: target.input.cwd !== publishCwd,
        isRepo: !(target.input.cwd === remainingCwd && selectedDiffState === "not-repo"),
      }),
    ),
);
export const remainingRefreshVcsStatus: typeof VcsPorts.vcsEnvironment.refreshStatus = {
  label: "owned fixture status refresh",
  run: async () => AsyncResult.success(structuredClone(publishStatus)),
};

const emptyThread = Atom.make(AsyncResult.success(EMPTY_ENVIRONMENT_THREAD_STATE));
export const environmentThreads: Pick<typeof ThreadPorts.environmentThreads, "stateAtom"> = {
  stateAtom: () => emptyThread,
};
export const environmentThreadDetails = createEnvironmentThreadDetailAtoms(
  environmentThreads.stateAtom,
);
const shellSnapshot = Atom.make<OrchestrationShellSnapshot | null>(null);
export const environmentThreadShells = createEnvironmentThreadShellAtoms({
  catalogValueAtom: Atom.make({ isReady: true, entries: new Map() }),
  snapshotAtom: () => shellSnapshot,
});
export const threadEnvironment: Pick<typeof ThreadPorts.threadEnvironment, "updateMetadata"> = {
  updateMetadata: refused("thread metadata writes"),
};
export const useEnvironmentThread: typeof ThreadPorts.useEnvironmentThread = () =>
  Option.getOrElse(
    AsyncResult.value(useAtomValue(emptyThread)),
    () => EMPTY_ENVIRONMENT_THREAD_STATE,
  );

export const orchestrationEnvironment: Pick<
  typeof OrchestrationPorts.orchestrationEnvironment,
  "fullThreadDiff" | "turnDiff"
> = {
  fullThreadDiff: Atom.family(
    (target: Parameters<typeof OrchestrationPorts.orchestrationEnvironment.fullThreadDiff>[0]) =>
      Atom.make(() =>
        AsyncResult.success({
          threadId: target.input.threadId,
          fromTurnCount: 0,
          toTurnCount: target.input.toTurnCount,
          diff: remainingPatch,
        }),
      ),
  ),
  turnDiff: Atom.family(
    (target: Parameters<typeof OrchestrationPorts.orchestrationEnvironment.turnDiff>[0]) =>
      Atom.make(() =>
        AsyncResult.success({
          threadId: target.input.threadId,
          fromTurnCount: target.input.fromTurnCount,
          toTurnCount: target.input.toTurnCount,
          diff: remainingPatch,
        }),
      ),
  ),
};
