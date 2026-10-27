import { RegistryContext } from "@effect/atom-react";
import { projectKey } from "@coddy/client-runtime/state/entities";
import { isQueryTransportCutoff } from "@coddy/client-runtime/state/runtime";
import type {
  GitManagerInProgressOperation,
  GitManagerConflictState,
  GitManagerOperationEvent,
  GitManagerOperationRequest,
  GitManagerRefEntry,
  GitManagerRefsSnapshot,
  GitManagerStashEntry,
  ScopedProjectRef,
  VcsWorktreeDescriptor,
} from "@coddy/contracts";
import * as Cause from "effect/Cause";
import {
  ArchiveIcon,
  GitBranchIcon,
  GitCompareArrowsIcon,
  GitMergeIcon,
  GitPullRequestIcon,
} from "lucide-react";
import {
  memo,
  useCallback,
  useContext,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type RefObject,
} from "react";

import {
  DEFAULT_GIT_MANAGER_VIEW_STATE,
  type GitManagerTab,
  useGitManagerStore,
} from "../../gitManagerStore";
import { useProject, useServerConfigs } from "../../state/entities";
import { useEnvironment, useEnvironmentConnectionState } from "../../state/environments";
import {
  gitManagerEnvironment,
  runGitManagerOperation,
  type GitManagerOperationHandle,
} from "../../state/gitManager";
import { useEnvironmentQuery } from "../../state/query";
import { vcsEnvironment } from "../../state/vcs";
import { worktreeEnvironment } from "../../state/worktrees";
import { Button } from "../ui/button";
import { Tabs, TabsList, TabsPanel, TabsTab } from "../ui/tabs";
import {
  resolveGitManagerTabTransition,
  resolveGitManagerTabTransitionBaseline,
  resolveGitManagerWorkingTree,
  type GitManagerTabTransitionInputs,
} from "./gitManagerTabTransition";
import { GitManagerInProgressStrip } from "./GitManagerInProgressStrip";
import {
  GitManagerBranchDialogs,
  type GitManagerBranchDialog,
  type GitManagerBranchDialogSubmission,
} from "./dialogs/GitManagerBranchDialogs";
import { GitManagerMergeDialog } from "./merge/GitManagerMergeDialog";
import { hasMergeSource } from "./merge/GitManagerMergeDialog.logic";
import { GitManagerStashDiff } from "./stash/GitManagerStashDiff";
import { GitManagerStashList } from "./stash/GitManagerStashList";
import { resolveStashIndex } from "./stash/GitManagerStashList.logic";
import {
  resolveGitManagerAvailability,
  resolveGitManagerCapabilityDisabledReasons,
  type GitManagerAvailability,
} from "./gitManagerAvailability";
import { GitManagerChangesView } from "./changes/GitManagerChangesView";
import { GitManagerImageDiffModeProvider } from "./diff/GitManagerImageDiffModeContext";
import {
  GitManagerHistoryView,
  type GitManagerHistoryAction,
} from "./history/GitManagerHistoryView";
import { GitManagerPullRequestPanel } from "./provider/GitManagerPullRequestPanel";
import { resolveStatusChangeRequestPresentation } from "./provider/GitManagerPullRequestPanel.logic";
import { GitManagerToolbar } from "./GitManagerToolbar";
import { GitManagerMultiCommitOperationDialog } from "./rewrite/GitManagerMultiCommitOperationDialog";
import { GitManagerResetDialog, type GitManagerResetMode } from "./rewrite/GitManagerResetDialog";
import {
  advanceMultiCommitOperation,
  type GitManagerMultiCommitEvent,
  type GitManagerMultiCommitKind,
  type GitManagerMultiCommitState,
} from "./rewrite/gitManagerMultiCommitOperation.logic";
import { GitManagerTagDialog } from "./tags/GitManagerTagDialog";
import { GitManagerTagsView, type GitManagerTagRowAction } from "./tags/GitManagerTagsView";
import { GitManagerOperationBanner } from "./toolbar/GitManagerOperationBanner";
import { useCheckoutStatusRereads } from "./useCheckoutStatusRereads";
import { GitManagerRepositoryUnavailable } from "./GitManagerRepositoryUnavailable";
import {
  gitManagerRepositoryUnavailableCopy,
  resolveGitManagerRepositoryUnavailable,
  type RepositoryUnavailable,
} from "./gitManagerRepositoryAvailability";

const EMPTY_WORKTREES: ReadonlyArray<VcsWorktreeDescriptor> = Object.freeze([]);
const EMPTY_REFS: ReadonlyArray<GitManagerRefEntry> = Object.freeze([]);
const EMPTY_STASHES: ReadonlyArray<GitManagerStashEntry> = Object.freeze([]);
const EMPTY_CONFLICT_PATHS: ReadonlyArray<string> = Object.freeze([]);
const EMPTY_TAG_NAMES: ReadonlyArray<string> = Object.freeze([]);
const EMPTY_TAGS: ReadonlyArray<GitManagerRefEntry> = Object.freeze([]);
const EMPTY_REMOTES: ReadonlyArray<string> = Object.freeze([]);
const STASH_MUTATION_OPERATIONS = new Set(["stash-apply", "stash-pop", "stash-drop"]);

type ResumableInProgressOperation = Omit<GitManagerInProgressOperation, "kind"> & {
  readonly kind: "merge" | "rebase" | "cherry-pick" | "revert";
};

function asResumableOperation(
  operation: GitManagerInProgressOperation | null,
): ResumableInProgressOperation | null {
  if (operation === null || operation.kind === "squash") return null;
  return { ...operation, kind: operation.kind };
}

function asMultiCommitKind(
  operation: GitManagerInProgressOperation | null,
): GitManagerMultiCommitKind | null {
  if (operation === null || operation.kind === "revert") return null;
  return operation.kind;
}

function createMultiCommitState(
  kind: GitManagerMultiCommitKind,
  step: GitManagerMultiCommitState["step"],
  selectedShas: ReadonlyArray<string>,
): GitManagerMultiCommitState {
  return {
    step,
    kind,
    selectedShas,
    selectedBranch: null,
    conflicts: [],
    continueBlocked: null,
    originalBranchTip: null,
    operationEvent: null,
    operationStartedExternally: false,
    abortRequested: false,
  };
}

export interface GitManagerPanelProps {
  readonly projectRef: ScopedProjectRef;
}

interface GitManagerUnavailableStateProps {
  readonly reason: string;
}

const GitManagerUnavailableState = memo(function GitManagerUnavailableState({
  reason,
}: GitManagerUnavailableStateProps) {
  return (
    <section
      aria-live="polite"
      className="flex min-h-0 flex-1 items-center justify-center p-6"
      data-testid="git-manager-unavailable"
    >
      <div className="max-w-md rounded-xl border border-border/70 bg-card/30 px-5 py-4 text-center">
        <GitBranchIcon aria-hidden="true" className="mx-auto mb-3 size-5 text-muted-foreground" />
        <h1 className="text-balance text-sm font-medium text-foreground">
          Git Manager Unavailable
        </h1>
        <p className="mt-1 text-sm text-muted-foreground">{reason}</p>
      </div>
    </section>
  );
});

function unavailableReason(
  availability: Exclude<GitManagerAvailability, { readonly kind: "ready" }>,
): string {
  return availability.kind === "unsupported"
    ? `This environment does not support the Git Manager. Missing capability: ${availability.missingCapability}.`
    : availability.reason;
}

function selectedCheckoutCwd(
  storedCwd: string | null,
  mainCheckoutCwd: string,
  worktrees: ReadonlyArray<VcsWorktreeDescriptor>,
): string {
  if (storedCwd === null || storedCwd === mainCheckoutCwd) return mainCheckoutCwd;
  return worktrees.some((worktree) => worktree.path === storedCwd) ? storedCwd : mainCheckoutCwd;
}

interface GitManagerTabTransitionBaseline {
  readonly checkout: string;
  readonly inputs: GitManagerTabTransitionInputs;
}

interface GitManagerRepositorySurfacesProps {
  readonly repositoryUnavailable: RepositoryUnavailable;
  readonly scope: {
    readonly environmentId: ScopedProjectRef["environmentId"];
    readonly cwd: string;
  };
  readonly projectRef: ScopedProjectRef;
  readonly signalGeneration: number | null;
  /** True while the live signal is expected but has not delivered its first generation. */
  readonly signalPending: boolean;
  readonly branchSyncDisabledReason: string | null;
  readonly stashMergeDisabledReason: string | null;
  /** The environment can merge into a branch that is not checked out. */
  readonly mergeIntoAvailable: boolean;
  readonly rewriteDisabledReason: string | null;
  readonly tagDisabledReason: string | null;
  readonly pullRequestsDisabledReason: string | null;
  readonly liveSignalDisabledReason: string | null;
  readonly activeTab: GitManagerTab;
  readonly tabTransitionBaselineRef: RefObject<GitManagerTabTransitionBaseline | null>;
  readonly onTabChange: (value: string | number | null) => void;
}

const GitManagerRepositorySurfaces = memo(function GitManagerRepositorySurfaces({
  scope,
  projectRef,
  repositoryUnavailable,
  signalGeneration,
  signalPending,
  branchSyncDisabledReason,
  stashMergeDisabledReason,
  mergeIntoAvailable,
  rewriteDisabledReason,
  tagDisabledReason,
  pullRequestsDisabledReason,
  liveSignalDisabledReason,
  activeTab,
  tabTransitionBaselineRef,
  onTabChange,
}: GitManagerRepositorySurfacesProps) {
  const registry = useContext(RegistryContext);
  const { environmentId, cwd } = scope;
  const { retrying, onRetry } = useCheckoutStatusRereads(scope);
  const { projectId } = projectRef;
  const storeKey = projectKey(projectRef);
  const selectSelectedStashSha = useCallback(
    (state: ReturnType<typeof useGitManagerStore.getState>) =>
      state.byProjectKey[storeKey]?.selectedStashSha ??
      DEFAULT_GIT_MANAGER_VIEW_STATE.selectedStashSha,
    [storeKey],
  );
  const selectStashPaneOpen = useCallback(
    (state: ReturnType<typeof useGitManagerStore.getState>) =>
      state.byProjectKey[storeKey]?.stashPaneOpen ?? DEFAULT_GIT_MANAGER_VIEW_STATE.stashPaneOpen,
    [storeKey],
  );
  const selectCollapsedTagSections = useCallback(
    (state: ReturnType<typeof useGitManagerStore.getState>) =>
      state.byProjectKey[storeKey]?.collapsedTagSections ??
      DEFAULT_GIT_MANAGER_VIEW_STATE.collapsedTagSections,
    [storeKey],
  );
  const selectSelectedFilePath = useCallback(
    (state: ReturnType<typeof useGitManagerStore.getState>) =>
      state.byProjectKey[storeKey]?.selectedFilePath ??
      DEFAULT_GIT_MANAGER_VIEW_STATE.selectedFilePath,
    [storeKey],
  );
  const selectProviderPaneOpen = useCallback(
    (state: ReturnType<typeof useGitManagerStore.getState>) =>
      state.byProjectKey[storeKey]?.providerPaneOpen ??
      DEFAULT_GIT_MANAGER_VIEW_STATE.providerPaneOpen,
    [storeKey],
  );
  const selectRecentRef = useCallback(
    (state: ReturnType<typeof useGitManagerStore.getState>) =>
      state.byProjectKey[storeKey]?.selectedRef ?? DEFAULT_GIT_MANAGER_VIEW_STATE.selectedRef,
    [storeKey],
  );
  const selectedStashSha = useGitManagerStore(selectSelectedStashSha);
  const stashPaneOpen = useGitManagerStore(selectStashPaneOpen);
  const collapsedTagSections = useGitManagerStore(selectCollapsedTagSections);
  const setTagSectionCollapsed = useGitManagerStore((state) => state.setTagSectionCollapsed);
  const handleTagSectionCollapsedChange = useCallback(
    (section: string, collapsed: boolean) => setTagSectionCollapsed(projectRef, section, collapsed),
    [projectRef, setTagSectionCollapsed],
  );
  const selectedFilePath = useGitManagerStore(selectSelectedFilePath);
  const providerPaneOpen = useGitManagerStore(selectProviderPaneOpen);
  const recentRef = useGitManagerStore(selectRecentRef);
  const setSelectedStash = useGitManagerStore((state) => state.setSelectedStash);
  const setStashPaneOpen = useGitManagerStore((state) => state.setStashPaneOpen);
  const setSelectedFile = useGitManagerStore((state) => state.setSelectedFile);
  const setProviderPaneOpen = useGitManagerStore((state) => state.setProviderPaneOpen);

  const refsAtom = useMemo(
    () =>
      gitManagerEnvironment.getRefs?.({
        environmentId,
        input: { cwd },
      }) ?? null,
    [cwd, environmentId],
  );
  const statusAtom = useMemo(
    () => vcsEnvironment.status({ environmentId, input: { cwd } }),
    [cwd, environmentId],
  );
  // The stash list costs one reflog read plus one `git stash show` per entry,
  // so it is read only while the Stashes pane is open; opening the pane and
  // each signal change while it is open read the current list.
  const stashesAtom = useMemo(
    () =>
      !stashPaneOpen || stashMergeDisabledReason !== null
        ? null
        : (gitManagerEnvironment.getStashes?.({
            environmentId,
            input: { cwd },
          }) ?? null),
    [cwd, environmentId, stashMergeDisabledReason, stashPaneOpen],
  );
  const refsQuery = useEnvironmentQuery(refsAtom);
  const statusQuery = useEnvironmentQuery(statusAtom);
  // The toggle and the provider pane name requests from this one answer: neutral
  // until status names the host (a failed read without an earlier answer included).
  const statusLoaded = statusQuery.data !== null;
  const sourceControlProvider = statusQuery.data?.sourceControlProvider ?? null;
  const changeRequest = resolveStatusChangeRequestPresentation(sourceControlProvider, statusLoaded);
  const stashesQuery = useEnvironmentQuery(stashesAtom);
  // Manual Refresh/Retry use refresh; automatic reads (signals, finished operations) revalidate.
  const refreshRefs = refsQuery.refresh;
  const revalidateRefs = refsQuery.revalidate;
  const refsPending = refsQuery.isPending;
  const revalidateStashes = stashesQuery.revalidate;
  const checkout = `${environmentId}\u0000${cwd}`;
  const repositoryReadable = statusQuery.error === null && statusQuery.data?.isRepo === true;
  const refsFailure = refsQuery.emission?._tag === "Failure" ? refsQuery.emission.cause : null;
  const [unavailableRefsFailure, setUnavailableRefsFailure] = useState<{
    readonly checkout: string;
    readonly cause: Cause.Cause<unknown>;
  } | null>(null);
  if (
    repositoryUnavailable !== null &&
    refsFailure !== null &&
    (unavailableRefsFailure?.checkout !== checkout || unavailableRefsFailure.cause !== refsFailure)
  ) {
    setUnavailableRefsFailure({ checkout, cause: refsFailure });
  }
  // Keep the failed observation above the tabs so entering Changes mid-recovery cannot
  // present it as current. Transport cut-offs still need their explicit Retry.
  const refsRecovering =
    repositoryReadable &&
    refsFailure !== null &&
    !refsQuery.requiresRetry &&
    !Cause.hasInterruptsOnly(refsFailure) &&
    !isQueryTransportCutoff(Cause.squash(refsFailure)) &&
    unavailableRefsFailure?.checkout === checkout &&
    unavailableRefsFailure.cause === refsFailure;
  // This is the sole owner of signal/recovery-driven refs revalidation. The first generation
  // per checkout/subscription re-reads only idle refs; an in-flight read covers it.
  // Every later step, including 0 -> 1, re-reads refs so History sees changed commits.
  const refsSignalGenerationRef = useRef<{
    readonly environmentId: typeof environmentId;
    readonly cwd: string;
    readonly generation: number | null;
    readonly unavailable: boolean;
  } | null>(null);
  useEffect(() => {
    const baseline = refsSignalGenerationRef.current;
    const previous =
      baseline?.cwd === cwd && baseline.environmentId === environmentId ? baseline : null;
    const recovered = previous?.unavailable === true && repositoryReadable;
    refsSignalGenerationRef.current = {
      environmentId,
      cwd,
      generation: signalGeneration,
      unavailable:
        repositoryUnavailable !== null || (previous?.unavailable === true && !repositoryReadable),
    };
    if (repositoryUnavailable !== null) return;
    const signalChanged =
      signalGeneration !== null &&
      (previous?.generation == null ? !refsPending : signalGeneration !== previous.generation);
    // A repair and its signal may arrive together; one read covers both.
    if (recovered || signalChanged) revalidateRefs();
  }, [
    cwd,
    environmentId,
    refsPending,
    repositoryReadable,
    repositoryUnavailable,
    revalidateRefs,
    signalGeneration,
  ]);

  // Mounting (including recovery) reads the list. Only later signal steps while
  // this checkout's query is mounted need an additional read.
  const stashesEnabled = stashPaneOpen && stashMergeDisabledReason === null;
  const stashSignalGenerationRef = useRef<{
    readonly checkout: string;
    readonly generation: number | null;
  } | null>(null);
  useEffect(() => {
    const baseline = stashSignalGenerationRef.current;
    const previous = baseline?.checkout === checkout ? baseline.generation : null;
    stashSignalGenerationRef.current = {
      checkout,
      generation: stashesEnabled ? signalGeneration : null,
    };
    if (!stashesEnabled || previous === null || signalGeneration === null) return;
    if (signalGeneration !== previous) revalidateStashes();
  }, [checkout, revalidateStashes, signalGeneration, stashesEnabled]);

  const snapshot: GitManagerRefsSnapshot | null = refsQuery.data ?? null;
  const localBranches = snapshot?.localBranches ?? EMPTY_REFS;
  const currentBranch = localBranches.find((branch) => branch.current) ?? null;
  const currentBranchName = currentBranch?.name ?? null;
  const stashes = stashesQuery.data ?? EMPTY_STASHES;
  const repositoryBlockedReasons = useMemo(
    () => localBranches.flatMap((branch) => branch.blocked),
    [localBranches],
  );
  const stashBlockedReasons = useMemo(
    () =>
      repositoryBlockedReasons.filter((reason) => STASH_MUTATION_OPERATIONS.has(reason.operation)),
    [repositoryBlockedReasons],
  );
  const inProgressBlocked =
    repositoryBlockedReasons.find((reason) => reason.code === "merge-in-progress") ?? null;
  const continueBlocked =
    repositoryBlockedReasons.find((reason) => reason.operation === "continue") ?? null;
  const inProgressOperation = snapshot?.inProgressOperation ?? null;
  const mergePending = snapshot === null ? null : inProgressOperation?.kind === "merge";
  const workingTree = resolveGitManagerWorkingTree(statusQuery.data);
  // The tab moves only on a transition of these inputs (see resolveGitManagerTabTransition):
  // a checkout that becomes clean moves to History, and a merge that appears moves to
  // Changes. Another project or checkout starts a new baseline, like opening the panel. The
  // selected tab is read from the store when a transition happens instead of being a
  // dependency, so a manual pick never triggers a move; a Git failure and its recovery never
  // move it either. (useEffectEvent would do, but react-dom 19.2 never refreshes an Effect
  // Event declared in a memo component, so it would read a stale tab.)
  useEffect(() => {
    const checkout = `${storeKey}\u0000${cwd}`;
    const baseline = tabTransitionBaselineRef.current;
    const previous = baseline?.checkout === checkout ? baseline.inputs : null;
    const next = resolveGitManagerTabTransitionBaseline(previous, { mergePending, workingTree });
    tabTransitionBaselineRef.current = { checkout, inputs: next };
    const currentTab = useGitManagerStore.getState().selectViewState(projectRef).activeTab;
    const tab = resolveGitManagerTabTransition(previous, next, currentTab);
    if (tab !== null && tab !== currentTab) onTabChange(tab);
  }, [cwd, mergePending, onTabChange, projectRef, storeKey, tabTransitionBaselineRef, workingTree]);
  const resumableOperation = asResumableOperation(inProgressOperation);
  const resumableOperationDisabledReason =
    resumableOperation?.kind === "merge" ? stashMergeDisabledReason : rewriteDisabledReason;
  const externalMultiCommitKind = asMultiCommitKind(inProgressOperation);
  const conflictedPaths = snapshot?.conflictedPaths ?? EMPTY_CONFLICT_PATHS;
  const conflicts: ReadonlyArray<GitManagerConflictState> = conflictedPaths.map((path) => ({
    path,
    kind: "binary",
    markerCount: 1,
    resolution: null,
  }));
  const recentNames = useMemo(() => (recentRef === null ? [] : [recentRef]), [recentRef]);

  const [mergeDialogOpen, setMergeDialogOpen] = useState(false);
  const [multiCommitState, setMultiCommitState] = useState<GitManagerMultiCommitState | null>(null);
  const [historyBranchDialog, setHistoryBranchDialog] = useState<GitManagerBranchDialog | null>(
    null,
  );
  const [tagDialog, setTagDialog] = useState<{
    readonly action: "create" | GitManagerTagRowAction;
    readonly tag: string | null;
    readonly targetSha: string | null;
  } | null>(null);
  const [resetTargetSha, setResetTargetSha] = useState<string | null>(null);
  const [historyActionMessage, setHistoryActionMessage] = useState<string | null>(null);
  const [pendingRewriteRequest, setPendingRewriteRequest] =
    useState<GitManagerOperationRequest | null>(null);
  const [operationEvent, setOperationEvent] = useState<GitManagerOperationEvent | null>(null);
  const [operationRunning, setOperationRunning] = useState(false);
  const activeOperationRef = useRef<GitManagerOperationHandle | null>(null);
  useEffect(
    () => () => {
      activeOperationRef.current?.cancel();
    },
    [],
  );
  const revalidateRepositoryReads = useCallback(() => {
    revalidateRefs();
    if (stashPaneOpen) revalidateStashes();
  }, [revalidateRefs, revalidateStashes, stashPaneOpen]);
  const executeOperation = useCallback(
    async (
      input: GitManagerOperationRequest,
      onEvent?: (event: GitManagerOperationEvent) => void,
    ): Promise<boolean> => {
      if (activeOperationRef.current !== null) return false;
      setOperationRunning(true);
      const startedEvent = { _tag: "started", operation: input._tag } as const;
      setOperationEvent(startedEvent);
      onEvent?.(startedEvent);
      const handle = runGitManagerOperation(registry, { environmentId, input }, (event) => {
        setOperationEvent(event);
        onEvent?.(event);
        if (event._tag === "failed" || event._tag === "finished") {
          setOperationRunning(false);
        }
      });
      activeOperationRef.current = handle;
      const result = await handle.result;
      if (activeOperationRef.current === handle) activeOperationRef.current = null;
      setOperationRunning(false);
      revalidateRepositoryReads();
      if (result._tag === "Failure") {
        if (Cause.hasInterruptsOnly(result.cause)) return false;
        const error = Cause.squash(result.cause);
        const message = error instanceof Error ? error.message : "The Git operation failed.";
        setOperationEvent({
          _tag: "failed",
          operation: input._tag,
          code: "transport-error",
          message,
          blocked: null,
        });
        onEvent?.({
          _tag: "failed",
          operation: input._tag,
          code: "transport-error",
          message,
          blocked: null,
        });
        return false;
      }
      return result.value._tag === "finished";
    },
    [environmentId, revalidateRepositoryReads, registry],
  );
  const cancelOperation = useCallback(() => {
    activeOperationRef.current?.cancel();
    activeOperationRef.current = null;
    setOperationRunning(false);
  }, []);
  const hasConflicts = conflicts.length > 0;
  // Memoized: built inline, this object stops the React Compiler preserving the callbacks below.
  const externalMultiCommitState = useMemo(
    () =>
      externalMultiCommitKind === null || !hasConflicts
        ? null
        : {
            ...createMultiCommitState(externalMultiCommitKind, "show-conflicts", []),
            operationStartedExternally: true,
          },
    [externalMultiCommitKind, hasConflicts],
  );
  const baseMultiCommitState = multiCommitState ?? externalMultiCommitState;
  const presentedMultiCommitState =
    baseMultiCommitState === null
      ? null
      : {
          ...baseMultiCommitState,
          conflicts,
          continueBlocked,
          inProgressOperation,
          refs: localBranches,
          recentNames,
        };
  const presentedMultiCommitStateRef = useRef(presentedMultiCommitState);
  const pendingRewriteRequestRef = useRef(pendingRewriteRequest);
  useLayoutEffect(() => {
    presentedMultiCommitStateRef.current = presentedMultiCommitState;
    pendingRewriteRequestRef.current = pendingRewriteRequest;
  });
  const observeMultiCommitEvent = useCallback((event: GitManagerOperationEvent) => {
    setMultiCommitState((current) => {
      if (current === null) return null;
      const next = advanceMultiCommitOperation(current, event);
      return next.step === null ? null : next;
    });
  }, []);
  const runMultiCommitOperation = useCallback(
    (state: GitManagerMultiCommitState, input: GitManagerOperationRequest) => {
      setHistoryActionMessage(null);
      setPendingRewriteRequest(null);
      setMultiCommitState(state);
      void executeOperation(input, observeMultiCommitEvent);
    },
    [executeOperation, observeMultiCommitEvent],
  );
  const rewriteNeedsForceWarning =
    currentBranch?.upstream !== null && currentBranch?.upstream !== undefined;
  const startHistoryRewrite = useCallback(
    (
      kind: "squash" | "reorder",
      shas: ReadonlyArray<string>,
      request: GitManagerOperationRequest,
    ) => {
      const state = {
        ...createMultiCommitState(
          kind,
          rewriteNeedsForceWarning ? "warn-force-push" : "show-progress",
          shas,
        ),
        commitsArePushed: rewriteNeedsForceWarning,
      };
      if (rewriteNeedsForceWarning) {
        setPendingRewriteRequest(request);
        setMultiCommitState(state);
        return;
      }
      runMultiCommitOperation(state, request);
    },
    [rewriteNeedsForceWarning, runMultiCommitOperation],
  );
  const handleHistoryAction = useCallback(
    (action: GitManagerHistoryAction) => {
      const operationBase = { cwd, projectId };
      switch (action._tag) {
        case "reset":
          if (rewriteDisabledReason !== null) {
            setHistoryActionMessage(rewriteDisabledReason);
            return;
          }
          setResetTargetSha(action.sha);
          return;
        case "revert":
          if (rewriteDisabledReason !== null) {
            setHistoryActionMessage(rewriteDisabledReason);
            return;
          }
          setHistoryActionMessage(null);
          void executeOperation({ _tag: "revert", ...operationBase, sha: action.sha });
          return;
        case "cherry-pick":
          if (rewriteDisabledReason !== null) {
            setHistoryActionMessage(rewriteDisabledReason);
            return;
          }
          runMultiCommitOperation(
            createMultiCommitState("cherry-pick", "show-progress", action.shas),
            { _tag: "cherry-pick", ...operationBase, shas: action.shas as [string, ...string[]] },
          );
          return;
        case "squash":
          if (rewriteDisabledReason !== null) {
            setHistoryActionMessage(rewriteDisabledReason);
            return;
          }
          if (action.shas.length < 2) {
            setHistoryActionMessage("Select at least two commits to squash.");
            return;
          }
          startHistoryRewrite("squash", action.shas, {
            _tag: "squash",
            ...operationBase,
            shas: action.shas as [string, ...string[]],
            message: action.message,
          });
          return;
        case "reorder":
          if (rewriteDisabledReason !== null) {
            setHistoryActionMessage(rewriteDisabledReason);
            return;
          }
          startHistoryRewrite("reorder", action.shas, {
            _tag: "reorder",
            ...operationBase,
            shas: action.shas as [string, ...string[]],
            insertBeforeSha: action.insertBeforeSha,
          });
          return;
        case "prepare-reorder":
          if (rewriteDisabledReason !== null) {
            setHistoryActionMessage(rewriteDisabledReason);
            return;
          }
          setHistoryActionMessage(
            "Drag the selected commit to an insertion line, then drop it to reorder history.",
          );
          return;
        case "create-branch":
          if (branchSyncDisabledReason !== null) {
            setHistoryActionMessage(branchSyncDisabledReason);
            return;
          }
          setHistoryBranchDialog({ kind: "create", baseBranch: null, baseCommit: action.sha });
          return;
        case "create-tag":
          if (tagDisabledReason !== null) {
            setHistoryActionMessage(tagDisabledReason);
            return;
          }
          setTagDialog({ action: "create", tag: null, targetSha: action.sha });
      }
    },
    [
      branchSyncDisabledReason,
      cwd,
      executeOperation,
      projectId,
      rewriteDisabledReason,
      runMultiCommitOperation,
      startHistoryRewrite,
      tagDisabledReason,
    ],
  );
  const openRebaseDialog = useCallback(() => {
    setHistoryActionMessage(null);
    if (rewriteDisabledReason !== null) {
      setHistoryActionMessage(rewriteDisabledReason);
      return;
    }
    setPendingRewriteRequest(null);
    setMultiCommitState({
      ...createMultiCommitState("rebase", "choose-branch", []),
      commitsArePushed: currentBranch?.upstream !== null && currentBranch?.upstream !== undefined,
    });
  }, [currentBranch?.upstream, rewriteDisabledReason]);
  const cancelMultiCommitOperation = useCallback(() => {
    cancelOperation();
    setPendingRewriteRequest(null);
    setMultiCommitState((current) =>
      current === null ? null : advanceMultiCommitOperation(current, { _tag: "cancelled" }),
    );
  }, [cancelOperation]);
  const runRebase = useCallback(
    (state: GitManagerMultiCommitState) => {
      if (rewriteDisabledReason !== null) {
        setHistoryActionMessage(rewriteDisabledReason);
        return;
      }
      if (state.selectedBranch === null || currentBranchName === null) return;
      runMultiCommitOperation(state, {
        _tag: "rebase",
        cwd,
        projectId,
        base: state.selectedBranch,
        target: currentBranchName,
      });
    },
    [currentBranchName, cwd, projectId, rewriteDisabledReason, runMultiCommitOperation],
  );
  const advanceMultiCommit = useCallback(
    (event: GitManagerMultiCommitEvent) => {
      const current = presentedMultiCommitStateRef.current;
      if (current === null) return;
      switch (event._tag) {
        case "branch-chosen": {
          const next = advanceMultiCommitOperation(current, event);
          setMultiCommitState(next);
          if (next.step === "show-progress") runRebase(next);
          return;
        }
        case "force-push-confirmed": {
          const next = advanceMultiCommitOperation(current, event);
          setMultiCommitState(next);
          const pendingRequest = pendingRewriteRequestRef.current;
          if (pendingRequest === null) {
            runRebase(next);
          } else {
            runMultiCommitOperation(next, pendingRequest);
          }
          return;
        }
        case "continue-requested": {
          const next = advanceMultiCommitOperation(current, event);
          if (next === current) return;
          setMultiCommitState(next);
          const operation =
            current.kind === "merge" || current.kind === "rebase" || current.kind === "cherry-pick"
              ? current.kind
              : null;
          if (operation !== null) {
            runMultiCommitOperation(next, {
              _tag: "continue",
              cwd,
              projectId,
              operation,
            });
          }
          return;
        }
        case "resolve-conflict-requested":
          void executeOperation({
            _tag: "resolve-conflict",
            cwd,
            projectId,
            path: event.path,
            side: event.side,
          });
          return;
        case "undo-conflict-resolution-requested":
          setHistoryActionMessage(
            `Edit ${event.path} again to change its resolution before continuing.`,
          );
          return;
        default:
          setMultiCommitState(advanceMultiCommitOperation(current, event));
      }
    },
    [cwd, executeOperation, projectId, runMultiCommitOperation, runRebase],
  );
  const confirmMultiCommitAbort = useCallback(() => {
    const current = presentedMultiCommitStateRef.current;
    if (current === null) return;
    const next = advanceMultiCommitOperation(current, { _tag: "abort-confirmed" });
    setMultiCommitState(next);
    const operation =
      current.kind === "merge" || current.kind === "rebase" || current.kind === "cherry-pick"
        ? current.kind
        : null;
    if (operation !== null) {
      runMultiCommitOperation(next, { _tag: "abort", cwd, projectId, operation });
    }
  }, [cwd, projectId, runMultiCommitOperation]);
  const runStashMutation = useCallback(
    async (kind: "stash-apply" | "stash-pop" | "stash-drop", sha: string) => {
      if (stashMergeDisabledReason !== null) {
        setHistoryActionMessage(stashMergeDisabledReason);
        return;
      }
      const index = resolveStashIndex(stashes, sha);
      if (index === null) {
        revalidateStashes();
        return;
      }
      await executeOperation({ _tag: kind, cwd, projectId, index });
    },
    [cwd, executeOperation, projectId, revalidateStashes, stashes, stashMergeDisabledReason],
  );
  const runStashApply = useCallback(
    (sha: string) => runStashMutation("stash-apply", sha),
    [runStashMutation],
  );
  const runStashPop = useCallback(
    (sha: string) => runStashMutation("stash-pop", sha),
    [runStashMutation],
  );
  const runStashDrop = useCallback(
    (sha: string) => runStashMutation("stash-drop", sha),
    [runStashMutation],
  );
  const selectStash = useCallback(
    (sha: string) => setSelectedStash(projectRef, sha),
    [projectRef, setSelectedStash],
  );
  const selectStashFile = useCallback(
    (path: string) => setSelectedFile(projectRef, path),
    [projectRef, setSelectedFile],
  );
  const toggleStashPane = useCallback(
    () => setStashPaneOpen(projectRef, !stashPaneOpen),
    [projectRef, setStashPaneOpen, stashPaneOpen],
  );
  const toggleProviderPane = useCallback(
    () => setProviderPaneOpen(projectRef, !providerPaneOpen),
    [projectRef, providerPaneOpen, setProviderPaneOpen],
  );
  const openMergeDialog = useCallback(() => {
    if (stashMergeDisabledReason !== null) {
      setHistoryActionMessage(stashMergeDisabledReason);
      return;
    }
    setMergeDialogOpen(true);
  }, [stashMergeDisabledReason]);
  const handleMergeFinished = useCallback(() => revalidateRefs(), [revalidateRefs]);
  const closeHistoryBranchDialog = useCallback(() => {
    if (!operationRunning) setHistoryBranchDialog(null);
  }, [operationRunning]);
  const submitHistoryBranchDialog = useCallback(
    async (submission: GitManagerBranchDialogSubmission) => {
      if (branchSyncDisabledReason !== null) {
        setHistoryActionMessage(branchSyncDisabledReason);
        return;
      }
      if (submission.kind !== "create") return;
      if (
        await executeOperation({
          _tag: "branch-create",
          cwd,
          projectId,
          name: submission.name,
          startPoint: submission.startPoint,
          checkout: submission.checkout,
        })
      ) {
        setHistoryBranchDialog(null);
      }
    },
    [branchSyncDisabledReason, cwd, executeOperation, projectId],
  );
  const handleTagRowAction = useCallback(
    (action: GitManagerTagRowAction, tag: string) => {
      if (tagDisabledReason !== null) {
        setHistoryActionMessage(tagDisabledReason);
        return;
      }
      setTagDialog({ action, tag, targetSha: null });
    },
    [tagDisabledReason],
  );
  const closeHistoryTagDialog = useCallback((open: boolean) => {
    if (!open) setTagDialog(null);
  }, []);
  const closeResetDialog = useCallback(() => setResetTargetSha(null), []);
  const confirmReset = useCallback(
    (mode: GitManagerResetMode) => {
      if (rewriteDisabledReason !== null) {
        setHistoryActionMessage(rewriteDisabledReason);
        return;
      }
      const sha = resetTargetSha;
      if (sha === null) return;
      setResetTargetSha(null);
      void executeOperation({ _tag: "reset", cwd, projectId, sha, mode });
    },
    [cwd, executeOperation, projectId, resetTargetSha, rewriteDisabledReason],
  );
  const continueInProgress = useCallback(() => {
    if (resumableOperation === null) return;
    if (resumableOperationDisabledReason !== null) {
      setHistoryActionMessage(resumableOperationDisabledReason);
      return;
    }
    void executeOperation({
      _tag: "continue",
      cwd,
      projectId,
      operation: resumableOperation.kind,
    });
  }, [cwd, executeOperation, projectId, resumableOperation, resumableOperationDisabledReason]);
  const abortInProgress = useCallback(() => {
    if (resumableOperation === null) return;
    if (resumableOperationDisabledReason !== null) {
      setHistoryActionMessage(resumableOperationDisabledReason);
      return;
    }
    void executeOperation({
      _tag: "abort",
      cwd,
      projectId,
      operation: resumableOperation.kind,
    });
  }, [cwd, executeOperation, projectId, resumableOperation, resumableOperationDisabledReason]);
  const mergeDisabledReason =
    stashMergeDisabledReason ??
    (refsQuery.isPending || snapshot === null
      ? "Loading branches."
      : !hasMergeSource(localBranches, snapshot?.remoteBranches ?? EMPTY_REFS)
        ? "No source branch is available."
        : null);
  const rebaseBlockedReason =
    currentBranch?.blocked.find((reason) => reason.operation === "rebase") ?? null;
  const rebaseDisabledReason =
    rewriteDisabledReason ??
    (refsQuery.isPending || snapshot === null
      ? "Loading branches."
      : currentBranch === null
        ? "Check out a branch before rebasing."
        : localBranches.every((branch) => branch.current)
          ? "No base branch is available."
          : (rebaseBlockedReason?.message ?? null));
  const historyOperationError =
    operationEvent?._tag === "failed"
      ? (operationEvent.blocked?.message ?? operationEvent.message)
      : null;
  const historyTagScope = useMemo(() => ({ environmentId, cwd }), [cwd, environmentId]);
  const historyTagNames = snapshot?.tags.map((tag) => tag.name) ?? EMPTY_TAG_NAMES;
  const historyTagRemote = snapshot?.remotes[0] ?? null;

  return (
    <>
      {liveSignalDisabledReason === null ? null : (
        <p className="border-b border-border px-3 py-2 text-xs text-muted-foreground" role="status">
          {liveSignalDisabledReason}
        </p>
      )}
      {presentedMultiCommitState === null ? (
        <GitManagerOperationBanner operation={operationEvent} onCancel={cancelOperation} />
      ) : null}
      {resumableOperation === null ? null : (
        <GitManagerInProgressStrip
          blocked={inProgressBlocked}
          disabledReason={resumableOperationDisabledReason}
          operation={resumableOperation}
          onAbort={abortInProgress}
          onContinue={continueInProgress}
        />
      )}
      <div className="flex items-center justify-end gap-2 border-b border-panel-separator px-4 py-1.5">
        <Button
          aria-describedby={
            pullRequestsDisabledReason === null
              ? undefined
              : "git-manager-pull-requests-disabled-reason"
          }
          aria-expanded={providerPaneOpen}
          aria-label={`${providerPaneOpen ? "Hide" : "Show"} ${changeRequest.pluralLongName} and checks`}
          disabled={pullRequestsDisabledReason !== null}
          size="xs"
          title={pullRequestsDisabledReason ?? undefined}
          variant="ghost"
          onClick={toggleProviderPane}
        >
          <GitPullRequestIcon aria-hidden="true" />
          {`${providerPaneOpen ? "Hide" : "Show"} ${changeRequest.pluralLongName}`}
        </Button>
        {pullRequestsDisabledReason === null ? null : (
          <span className="sr-only" id="git-manager-pull-requests-disabled-reason">
            {pullRequestsDisabledReason}
          </span>
        )}
        <Button
          aria-describedby={
            stashMergeDisabledReason === null ? undefined : "git-manager-stash-disabled-reason"
          }
          aria-expanded={stashPaneOpen}
          aria-label="Toggle repository stashes"
          disabled={stashMergeDisabledReason !== null}
          size="xs"
          title={stashMergeDisabledReason ?? undefined}
          variant="ghost"
          onClick={toggleStashPane}
        >
          <ArchiveIcon aria-hidden="true" />
          Stashes
          {repositoryUnavailable !== null || stashesQuery.data === null
            ? ""
            : ` (${stashes.length})`}
        </Button>
        {stashMergeDisabledReason === null ? null : (
          <span className="sr-only" id="git-manager-stash-disabled-reason">
            {stashMergeDisabledReason}
          </span>
        )}
        <Button
          aria-describedby={
            mergeDisabledReason === null ? undefined : "git-manager-merge-trigger-reason"
          }
          disabled={mergeDisabledReason !== null}
          size="xs"
          title={mergeDisabledReason ?? "Merge one branch into another"}
          variant="ghost"
          onClick={openMergeDialog}
        >
          <GitMergeIcon aria-hidden="true" />
          Merge…
        </Button>
        {mergeDisabledReason === null ? null : (
          <span className="sr-only" id="git-manager-merge-trigger-reason">
            {mergeDisabledReason}
          </span>
        )}
        <Button
          aria-describedby={
            rebaseDisabledReason === null ? undefined : "git-manager-rebase-trigger-reason"
          }
          disabled={rebaseDisabledReason !== null}
          size="xs"
          title={rebaseDisabledReason ?? "Rebase the current branch onto another branch"}
          variant="ghost"
          onClick={openRebaseDialog}
        >
          <GitCompareArrowsIcon aria-hidden="true" />
          Rebase…
        </Button>
        {rebaseDisabledReason === null ? null : (
          <span className="sr-only" id="git-manager-rebase-trigger-reason">
            {rebaseDisabledReason}
          </span>
        )}
      </div>
      {historyActionMessage === null ? null : (
        <p
          aria-live="polite"
          className="border-b border-border px-3 py-2 text-xs text-muted-foreground"
        >
          {historyActionMessage}
        </p>
      )}
      {providerPaneOpen ? (
        <div className="h-80 min-h-0 overflow-auto border-b border-panel-separator">
          <GitManagerPullRequestPanel
            projectRef={projectRef}
            provider={sourceControlProvider}
            statusLoaded={statusLoaded}
            disabledReason={pullRequestsDisabledReason}
            scope={scope}
            onRefresh={refreshRefs}
          />
        </div>
      ) : null}
      {stashPaneOpen ? (
        <section
          aria-label="Repository stash browser"
          className="grid h-80 min-h-0 grid-cols-[minmax(14rem,32%)_minmax(0,1fr)] border-b border-panel-separator"
        >
          {stashMergeDisabledReason !== null ? (
            <p className="col-span-2 p-3 text-xs text-muted-foreground">
              {stashMergeDisabledReason}
            </p>
          ) : (
            <>
              {/* A flex column gives the virtualized list a bounded height to scroll in. */}
              <div className="flex min-h-0 flex-col border-r border-panel-separator">
                {stashesQuery.error !== null && stashes.length === 0 ? (
                  <p className="p-3 text-xs text-destructive">{stashesQuery.error}</p>
                ) : (
                  <GitManagerStashList
                    blockedReasons={stashBlockedReasons}
                    disabledReason={stashMergeDisabledReason}
                    entries={stashes}
                    operationInFlight={operationRunning}
                    projectRef={projectRef}
                    scope={scope}
                    selectedSha={selectedStashSha}
                    onApply={runStashApply}
                    onDrop={runStashDrop}
                    onPop={runStashPop}
                    onSelectStash={selectStash}
                  />
                )}
              </div>
              <GitManagerStashDiff
                entries={stashes}
                projectRef={projectRef}
                scope={scope}
                selectedPath={selectedFilePath}
                selectedStashSha={selectedStashSha}
                stashesPending={stashesQuery.isPending}
                onRefreshStashes={revalidateStashes}
                onSelectPath={selectStashFile}
              />
            </>
          )}
        </section>
      ) : null}
      <Tabs className="min-h-0 flex-1 gap-0" value={activeTab} onValueChange={onTabChange}>
        <div className="border-b border-panel-separator px-4 pt-2">
          <TabsList className="w-fit rounded-none border-0 bg-transparent p-0">
            <TabsTab
              className="rounded-none border-b-2 border-transparent px-3 py-2 data-active:border-foreground data-active:bg-transparent data-active:shadow-none"
              value="changes"
            >
              Changes
            </TabsTab>
            <TabsTab
              className="rounded-none border-b-2 border-transparent px-3 py-2 data-active:border-foreground data-active:bg-transparent data-active:shadow-none"
              value="history"
            >
              History
            </TabsTab>
            <TabsTab
              className="rounded-none border-b-2 border-transparent px-3 py-2 data-active:border-foreground data-active:bg-transparent data-active:shadow-none"
              value="tags"
            >
              Tags
            </TabsTab>
          </TabsList>
        </div>
        <TabsPanel className="min-h-0 flex-1 gap-0 p-4" value="changes">
          <GitManagerChangesView
            scope={scope}
            projectRef={projectRef}
            repositoryUnavailable={repositoryUnavailable}
            refsRecovering={refsRecovering}
            retrying={retrying}
            onRetry={onRetry}
          />
        </TabsPanel>
        <TabsPanel className="min-h-0 flex-1 gap-0 p-4" value="history">
          {repositoryUnavailable !== null ? (
            <GitManagerRepositoryUnavailable
              title="Could not load history"
              reason={repositoryUnavailable}
              cwd={cwd}
              retrying={retrying}
              onRetry={onRetry}
            />
          ) : activeTab === "history" ? (
            <GitManagerHistoryView
              blockedReasons={repositoryBlockedReasons}
              branchSyncDisabledReason={branchSyncDisabledReason}
              repositoryGeneration={snapshot?.generation ?? null}
              signalGeneration={signalGeneration}
              signalPending={signalPending}
              projectRef={projectRef}
              rewriteDisabledReason={rewriteDisabledReason}
              scope={scope}
              tagDisabledReason={tagDisabledReason}
              onAction={handleHistoryAction}
            />
          ) : null}
        </TabsPanel>
        <TabsPanel className="min-h-0 flex-1 gap-0 p-4" value="tags">
          {repositoryUnavailable !== null ? (
            <GitManagerRepositoryUnavailable
              title="Could not load tags"
              reason={repositoryUnavailable}
              cwd={cwd}
              retrying={retrying}
              onRetry={onRetry}
            />
          ) : activeTab === "tags" ? (
            <GitManagerTagsView
              collapsedSections={collapsedTagSections}
              remotes={snapshot?.remotes ?? EMPTY_REMOTES}
              scope={historyTagScope}
              tagDisabledReason={tagDisabledReason}
              tags={snapshot?.tags ?? EMPTY_TAGS}
              onSectionCollapsedChange={handleTagSectionCollapsedChange}
              onTagAction={handleTagRowAction}
            />
          ) : null}
        </TabsPanel>
      </Tabs>
      {presentedMultiCommitState === null ? null : (
        <GitManagerMultiCommitOperationDialog
          disabledReason={rewriteDisabledReason}
          state={presentedMultiCommitState}
          onAdvance={advanceMultiCommit}
          onCancel={cancelMultiCommitOperation}
          onConfirmAbort={confirmMultiCommitAbort}
        />
      )}
      <GitManagerBranchDialogs
        busy={operationRunning}
        dialog={historyBranchDialog}
        disabledReason={branchSyncDisabledReason}
        errorMessage={historyOperationError}
        refs={localBranches}
        remoteRefs={snapshot?.remoteBranches ?? EMPTY_REFS}
        onClose={closeHistoryBranchDialog}
        onSubmit={submitHistoryBranchDialog}
      />
      <GitManagerTagDialog
        action={tagDialog?.action ?? "create"}
        disabledReason={tagDisabledReason}
        existingTags={historyTagNames}
        open={tagDialog !== null}
        projectRef={projectRef}
        remote={historyTagRemote}
        scope={historyTagScope}
        tag={tagDialog?.tag ?? null}
        targetSha={tagDialog?.targetSha ?? null}
        onFinished={revalidateRefs}
        onOpenChange={closeHistoryTagDialog}
      />
      <GitManagerResetDialog
        disabledReason={rewriteDisabledReason}
        sha={resetTargetSha}
        onClose={closeResetDialog}
        onConfirm={confirmReset}
      />
      <GitManagerMergeDialog
        disabledReason={stashMergeDisabledReason}
        mergeIntoAvailable={mergeIntoAvailable}
        open={mergeDialogOpen}
        projectRef={projectRef}
        recentNames={recentNames}
        refs={localBranches}
        remoteRefs={snapshot?.remoteBranches ?? EMPTY_REFS}
        scope={scope}
        onFinished={handleMergeFinished}
        onOpenChange={setMergeDialogOpen}
      />
    </>
  );
});

export const GitManagerPanel = memo(function GitManagerPanel({ projectRef }: GitManagerPanelProps) {
  // Reconnecting unmounts the surfaces, not the route: keep their transition memory here
  // so only a real panel opening or checkout change starts a fresh baseline.
  const tabTransitionBaselineRef = useRef<GitManagerTabTransitionBaseline | null>(null);
  const { environmentId, projectId } = projectRef;
  const stableProjectRef = useMemo(
    () => ({ environmentId, projectId }) as ScopedProjectRef,
    [environmentId, projectId],
  );
  const project = useProject(stableProjectRef);
  const connection = useEnvironmentConnectionState(environmentId);
  const environmentLabel = useEnvironment(environmentId)?.label ?? "this environment";
  const serverConfig = useServerConfigs().get(environmentId) ?? null;
  const availability = resolveGitManagerAvailability(
    connection.data,
    serverConfig,
    environmentLabel,
  );
  const capabilityDisabledReasons = resolveGitManagerCapabilityDisabledReasons(serverConfig);
  const catalogProjectId = project?.id ?? null;
  const storeKey = projectKey(stableProjectRef);
  const [selectionOwnerKey, setSelectionOwnerKey] = useState<string | null>(null);
  const selectViewState = useCallback(
    (state: ReturnType<typeof useGitManagerStore.getState>) =>
      state.byProjectKey[storeKey] ?? DEFAULT_GIT_MANAGER_VIEW_STATE,
    [storeKey],
  );
  const viewState = useGitManagerStore(selectViewState);
  const touchProject = useGitManagerStore((state) => state.touchProject);
  const setActiveTab = useGitManagerStore((state) => state.setActiveTab);
  const setSelectedWorktree = useGitManagerStore((state) => state.setSelectedWorktree);

  useEffect(() => {
    touchProject({ environmentId, projectId } as ScopedProjectRef);
  }, [environmentId, projectId, touchProject]);

  const catalogAtom = useMemo(
    () =>
      availability.kind === "ready" && catalogProjectId !== null
        ? worktreeEnvironment.catalog({
            environmentId,
            input: { projectId: catalogProjectId },
          })
        : null,
    [availability.kind, catalogProjectId, environmentId],
  );
  const catalog = useEnvironmentQuery(catalogAtom);
  const worktrees = catalog.data?.worktrees ?? EMPTY_WORKTREES;
  const mainCheckoutCwd = project?.workspaceRoot ?? null;
  const sessionSelectedWorktreeCwd =
    selectionOwnerKey === storeKey ? viewState.selectedWorktreeCwd : null;
  const activeCwd =
    mainCheckoutCwd === null
      ? null
      : selectedCheckoutCwd(sessionSelectedWorktreeCwd, mainCheckoutCwd, worktrees);
  const activeScope = useMemo(
    () => ({ environmentId, cwd: activeCwd ?? "" }),
    [activeCwd, environmentId],
  );
  const environmentReady = availability.kind === "ready";
  const statusAtom = useMemo(
    () =>
      environmentReady && activeCwd !== null
        ? vcsEnvironment.status({ environmentId, input: { cwd: activeCwd } })
        : null,
    [activeCwd, environmentId, environmentReady],
  );
  const statusQuery = useEnvironmentQuery(statusAtom);
  const repositoryUnavailable = resolveGitManagerRepositoryUnavailable(
    statusQuery.error === null ? statusQuery.data : null,
  );
  const repositoryDisabledReason =
    repositoryUnavailable === null
      ? null
      : gitManagerRepositoryUnavailableCopy(repositoryUnavailable, activeScope.cwd).message;
  const effectiveDisabledReasons = {
    branchSync: repositoryDisabledReason ?? capabilityDisabledReasons.branchSync,
    stashMerge: repositoryDisabledReason ?? capabilityDisabledReasons.stashMerge,
    rewrite: repositoryDisabledReason ?? capabilityDisabledReasons.rewrite,
    tag: repositoryDisabledReason ?? capabilityDisabledReasons.tag,
  };
  const signalAtom = useMemo(
    () =>
      availability.kind === "ready" &&
      activeCwd !== null &&
      capabilityDisabledReasons.liveSignal === null
        ? gitManagerEnvironment.signalWithDegradedFocusRefresh({
            environmentId,
            input: { cwd: activeCwd },
          })
        : null,
    [activeCwd, availability.kind, capabilityDisabledReasons.liveSignal, environmentId],
  );
  const signalQuery = useEnvironmentQuery(signalAtom);
  const signalGeneration = signalQuery.data?.generation ?? null;
  // A failed subscription is not pending: History then reads without a signal.
  const signalPending =
    signalAtom !== null && signalQuery.data === null && signalQuery.error === null;

  const handleTabChange = useCallback(
    (value: string | number | null) => {
      if (value === "changes" || value === "history" || value === "tags") {
        setActiveTab(stableProjectRef, value as GitManagerTab);
      }
    },
    [setActiveTab, stableProjectRef],
  );
  const handleWorktreeChange = useCallback(
    (cwd: string) => {
      setSelectionOwnerKey(storeKey);
      setSelectedWorktree(stableProjectRef, cwd);
    },
    [setSelectedWorktree, stableProjectRef, storeKey],
  );

  if (availability.kind !== "ready") {
    return <GitManagerUnavailableState reason={unavailableReason(availability)} />;
  }
  if (project === null || mainCheckoutCwd === null || activeCwd === null) {
    return <GitManagerUnavailableState reason="Waiting for project data." />;
  }

  return (
    <GitManagerImageDiffModeProvider projectRef={stableProjectRef}>
      <div className="flex h-full min-h-0 min-w-0 flex-1 flex-col bg-background">
        <GitManagerToolbar
          repositoryUnavailable={repositoryUnavailable}
          branchSyncDisabledReason={effectiveDisabledReasons.branchSync}
          projectRef={stableProjectRef}
          mainCheckoutCwd={mainCheckoutCwd}
          selectedWorktreeCwd={activeCwd}
          worktrees={worktrees}
          catalogPending={catalog.isPending && catalog.data === null}
          catalogError={
            catalog.error !== null && repositoryUnavailable !== null
              ? repositoryDisabledReason
              : catalog.error
          }
          stashMergeDisabledReason={effectiveDisabledReasons.stashMerge}
          tagDisabledReason={effectiveDisabledReasons.tag}
          onSelectedWorktreeChange={handleWorktreeChange}
        />
        <GitManagerRepositorySurfaces
          repositoryUnavailable={repositoryUnavailable}
          activeTab={viewState.activeTab}
          branchSyncDisabledReason={effectiveDisabledReasons.branchSync}
          liveSignalDisabledReason={capabilityDisabledReasons.liveSignal}
          projectRef={stableProjectRef}
          pullRequestsDisabledReason={capabilityDisabledReasons.pullRequests}
          rewriteDisabledReason={effectiveDisabledReasons.rewrite}
          scope={activeScope}
          signalGeneration={signalGeneration}
          signalPending={signalPending}
          stashMergeDisabledReason={effectiveDisabledReasons.stashMerge}
          mergeIntoAvailable={capabilityDisabledReasons.mergeInto === null}
          tagDisabledReason={effectiveDisabledReasons.tag}
          tabTransitionBaselineRef={tabTransitionBaselineRef}
          onTabChange={handleTabChange}
        />
      </div>
    </GitManagerImageDiffModeProvider>
  );
});
